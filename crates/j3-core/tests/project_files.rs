//! `projects.listEntries`, `projects.searchEntries` and `projects.readFile`
//! over the WebSocket against the real `App`, on temporary project
//! directories.

mod transport_support;

use std::{net::SocketAddr, path::Path, process::Command, sync::Arc, time::Duration};

use futures_util::future::BoxFuture;
use j3_core::{
    app::{App, AppConfig},
    engine::session::{Events, Result as SessionResult, SessionFactory},
    files::{MAX_ENTRIES, MAX_READ_BYTES},
    omp::{self, OmpLauncher},
    serve::Server,
};
use serde_json::{Value, json};
use transport_support::{TempDir, Ws};

/// No thread is ever started here.
struct NoSessions;

impl SessionFactory for NoSessions {
    fn start(
        &self,
        _cwd: std::path::PathBuf,
        _resume: Option<std::path::PathBuf>,
    ) -> BoxFuture<'static, SessionResult<(Box<dyn j3_core::engine::session::AgentSession>, Events)>>
    {
        Box::pin(async { Err(omp::Error::NoSessionFile) })
    }
}

struct Env {
    ws: Ws,
    next_id: u64,
    _server: Server,
    _data: TempDir,
}

impl Env {
    async fn start() -> Self {
        let data = TempDir::new("files-data");
        let app = App::open_with(
            AppConfig {
                data_dir: data.0.clone(),
                label: "test-host".into(),
                omp: OmpLauncher::default(),
            },
            Arc::new(NoSessions),
            Duration::from_secs(600),
        )
        .await
        .unwrap();
        let any_port: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = Server::bind(app, &[any_port]).await.unwrap();
        let addr = server.addrs()[0];
        Self {
            ws: Ws::connect(addr).await,
            next_id: 0,
            _server: server,
            _data: data,
        }
    }

    /// The success value, or the typed failure.
    async fn call(&mut self, tag: &str, payload: Value) -> Result<Value, Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.ws.request(json!(id), tag, payload).await;
        let frame = self.ws.recv().await;
        assert_eq!(frame["requestId"], json!(id), "{frame}");
        let exit = &frame["exit"];
        match exit["_tag"].as_str() {
            Some("Success") => Ok(exit["value"].clone()),
            _ => {
                let cause = &exit["cause"][0];
                assert_eq!(cause["_tag"], "Fail", "{frame}");
                Err(cause["error"].clone())
            }
        }
    }

    async fn list(&mut self, cwd: &Path, dir: Option<&str>) -> Result<Value, Value> {
        let mut payload = json!({"cwd": cwd});
        if let Some(dir) = dir {
            payload["directoryPath"] = json!(dir);
        }
        self.call("projects.listEntries", payload).await
    }

    async fn read(&mut self, cwd: &Path, path: &str) -> Result<Value, Value> {
        self.call(
            "projects.readFile",
            json!({"cwd": cwd, "relativePath": path}),
        )
        .await
    }

    /// The paths `query` finds, best first.
    async fn search(&mut self, cwd: &Path, query: &str) -> Vec<String> {
        let found = self
            .call(
                "projects.searchEntries",
                json!({"cwd": cwd, "query": query, "limit": 50}),
            )
            .await
            .unwrap();
        rows(&found).into_iter().map(|(path, ..)| path).collect()
    }
}

fn write(root: &Path, path: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// `(path, kind, ignored)` of each listed entry.
fn rows(result: &Value) -> Vec<(String, String, bool)> {
    result["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["path"].as_str().unwrap().to_owned(),
                entry["kind"].as_str().unwrap().to_owned(),
                entry["ignored"] == json!(true),
            )
        })
        .collect()
}

fn row(path: &str, kind: &str, ignored: bool) -> (String, String, bool) {
    (path.to_owned(), kind.to_owned(), ignored)
}

#[tokio::test]
async fn list_entries_orders_one_level_and_flags_git_ignored_entries() {
    let mut env = Env::start().await;
    let project = TempDir::new("files-project");
    let root = &project.0;
    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(git.success());
    write(root, ".gitignore", "target/\n*.log\n");
    write(root, "b.rs", "");
    write(root, "A.md", "");
    write(root, "debug.log", "");
    write(root, "src/main.rs", "fn main() {}\n");
    write(root, "src/lib/mod.rs", "");
    write(root, "target/debug/j3", "");
    write(root, "Zeta/x", "");

    let listed = env.list(root, None).await.unwrap();
    assert_eq!(listed["truncated"], false);
    // Directories first, then case-insensitive names; `.git` is left out.
    assert_eq!(
        rows(&listed),
        [
            row("src", "directory", false),
            row("target", "directory", true),
            row("Zeta", "directory", false),
            row(".gitignore", "file", false),
            row("A.md", "file", false),
            row("b.rs", "file", false),
            row("debug.log", "file", true),
        ]
    );
    // Absent `ignored` is omitted on the wire.
    assert!(listed["entries"][0].get("ignored").is_none());

    // One level below, paths stay relative to `cwd`.
    assert_eq!(
        rows(&env.list(root, Some("src")).await.unwrap()),
        [
            row("src/lib", "directory", false),
            row("src/main.rs", "file", false)
        ]
    );
    assert_eq!(
        rows(&env.list(root, Some("./src/lib/")).await.unwrap()),
        [row("src/lib/mod.rs", "file", false)]
    );
    // Inside an ignored directory everything is ignored.
    assert_eq!(
        rows(&env.list(root, Some("target")).await.unwrap()),
        [row("target/debug", "directory", true)]
    );
    // Empty is the root.
    assert_eq!(rows(&env.list(root, Some("")).await.unwrap()).len(), 7);
}

#[tokio::test]
async fn list_entries_without_git_has_no_flags_and_truncates() {
    let mut env = Env::start().await;
    let project = TempDir::new("files-plain");
    let root = &project.0;
    write(root, "target/x", "");
    for n in 0..MAX_ENTRIES + 5 {
        write(root, &format!("many/f{n:05}.txt"), "");
    }
    let listed = env.list(root, None).await.unwrap();
    assert_eq!(
        rows(&listed),
        [
            row("many", "directory", false),
            row("target", "directory", false)
        ]
    );

    let many = env.list(root, Some("many")).await.unwrap();
    assert_eq!(many["truncated"], true);
    let rows = rows(&many);
    assert_eq!(rows.len(), MAX_ENTRIES);
    assert_eq!(rows[0], row("many/f00000.txt", "file", false));
}

#[tokio::test]
async fn search_entries_matches_like_omps_at_completion() {
    let mut env = Env::start().await;
    let project = TempDir::new("files-search");
    let root = &project.0;
    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(git.success());
    write(root, ".gitignore", "target/\n");
    write(root, "src/main.rs", "");
    write(root, "src/domain/mailbox.rs", "");
    write(root, "docs/main-loop.md", "");
    write(root, "target/main.rs", "");
    write(root, ".github/ci.yml", "");

    // Fuzzy over the walk: names starting with the query, names containing
    // it, paths containing it, then tight subsequences; ignored files and
    // `.git` never show, hidden ones do.
    assert_eq!(
        env.search(root, "main").await,
        [
            "src/main.rs",
            "docs/main-loop.md",
            "src/domain",
            "src/domain/mailbox.rs"
        ]
    );
    assert_eq!(
        env.search(root, "smrs").await,
        ["src/main.rs", "src/domain/mailbox.rs"]
    );
    assert_eq!(env.search(root, "ci.y").await, [".github/ci.yml"]);
    assert!(env.search(root, "HEAD").await.is_empty());

    // An empty query or `dir/` lists one level, directories first; `dir/rest`
    // searches under `dir`.
    assert_eq!(
        env.search(root, "").await,
        [".github", "docs", "src", "target", ".gitignore"]
    );
    assert_eq!(
        env.search(root, "src/").await,
        ["src/domain", "src/main.rs"]
    );
    assert_eq!(
        env.search(root, "src/ma").await,
        ["src/main.rs", "src/domain/mailbox.rs", "src/domain"]
    );

    // Outside the workspace: the named directory, by name prefix, as typed.
    let parent = root.parent().unwrap().to_str().unwrap();
    let name = root.file_name().unwrap().to_str().unwrap();
    assert_eq!(
        env.search(root, &format!("../{name}")).await,
        [format!("../{name}")]
    );
    assert_eq!(
        env.search(root, &format!("{parent}/{name}/d")).await,
        [format!("{parent}/{name}/docs")]
    );

    // A missing workspace is a typed failure.
    let missing = env
        .call(
            "projects.searchEntries",
            json!({"cwd": root.join("gone"), "query": "x", "limit": 5}),
        )
        .await
        .unwrap_err();
    assert_eq!(missing["_tag"], "ProjectSearchEntriesError");
    assert_eq!(missing["failure"], "workspace_root_not_found");
}

#[tokio::test]
async fn list_entries_refuses_paths_outside_cwd() {
    let mut env = Env::start().await;
    let outside = TempDir::new("files-outside");
    write(&outside.0, "secret.txt", "s");
    let project = TempDir::new("files-escape");
    let root = &project.0;
    write(root, "inside/a.txt", "");
    std::os::unix::fs::symlink(&outside.0, root.join("out")).unwrap();
    std::os::unix::fs::symlink(root.join("inside"), root.join("in")).unwrap();

    // Symlinks list as what they point at.
    assert_eq!(
        rows(&env.list(root, None).await.unwrap()),
        [
            row("in", "directory", false),
            row("inside", "directory", false),
            row("out", "directory", false),
        ]
    );
    // A link staying inside lists under its own path.
    assert_eq!(
        rows(&env.list(root, Some("in")).await.unwrap()),
        [row("in/a.txt", "file", false)]
    );
    for dir in ["..", "inside/../..", "/etc", "out"] {
        let error = env.list(root, Some(dir)).await.unwrap_err();
        assert_eq!(error["_tag"], "ProjectListEntriesError", "{dir}");
        assert_eq!(error["failure"], "directory_list_failed", "{dir}");
        assert!(
            error["message"].as_str().unwrap().contains("outside"),
            "{error}"
        );
    }
    let error = env.list(root, Some("missing")).await.unwrap_err();
    assert_eq!(error["failure"], "directory_list_failed");
    let error = env.list(&root.join("nope"), None).await.unwrap_err();
    assert_eq!(error["failure"], "workspace_root_not_found");
    let error = env
        .list(&root.join("inside/a.txt"), None)
        .await
        .unwrap_err();
    assert_eq!(error["failure"], "workspace_root_not_directory");
}

#[tokio::test]
async fn read_file_text_truncation_and_absolute_paths() {
    let mut env = Env::start().await;
    let project = TempDir::new("files-read");
    let root = &project.0;
    write(root, "src/main.rs", "fn main() {}\n");

    let read = env.read(root, "src/main.rs").await.unwrap();
    assert_eq!(
        read,
        json!({"relativePath": "src/main.rs", "contents": "fn main() {}\n", "byteLength": 13,
               "truncated": false, "exists": true})
    );
    // Normalized, and absolute paths are read as they are.
    assert_eq!(
        env.read(root, "./src/../src/main.rs").await.unwrap()["relativePath"],
        "src/main.rs"
    );
    let absolute = root.join("src/main.rs");
    let read = env.read(root, absolute.to_str().unwrap()).await.unwrap();
    assert_eq!(read["contents"], "fn main() {}\n");
    assert_eq!(read["relativePath"], absolute.to_str().unwrap());

    // Large files: the first MiB, with the whole size.
    let size = MAX_READ_BYTES as usize + 10;
    write(root, "big.txt", "x".repeat(size));
    let read = env.read(root, "big.txt").await.unwrap();
    assert_eq!(read["truncated"], true);
    assert_eq!(read["byteLength"], size);
    assert_eq!(
        read["contents"].as_str().unwrap().len(),
        MAX_READ_BYTES as usize
    );
}

#[tokio::test]
async fn read_file_binary_missing_and_escapes_fail_typed() {
    let mut env = Env::start().await;
    let outside = TempDir::new("files-read-outside");
    write(&outside.0, "secret.txt", "s");
    let project = TempDir::new("files-read-errors");
    let root = &project.0;
    write(root, "logo.png", b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR");
    write(root, "dir/a.txt", "a");
    // A NUL past the sniffed prefix keeps the file text.
    let mut late = vec![b'a'; 9000];
    late.push(0);
    write(root, "late.txt", &late);
    std::os::unix::fs::symlink(outside.0.join("secret.txt"), root.join("leak.txt")).unwrap();

    let error = env.read(root, "logo.png").await.unwrap_err();
    assert_eq!(error["_tag"], "ProjectReadFileError");
    assert_eq!(error["failure"], "binary_file");
    assert_eq!(error["relativePath"], "logo.png");
    assert!(env.read(root, "late.txt").await.is_ok());

    let error = env.read(root, "gone.txt").await.unwrap_err();
    assert_eq!(error["failure"], "operation_failed");
    assert_eq!(error["operation"], "realpath-target");
    // The iOS app's optional `t3.json`.
    let missing = env
        .call(
            "projects.readFile",
            json!({"cwd": root, "relativePath": "t3.json", "allowMissing": true}),
        )
        .await
        .unwrap();
    assert_eq!(
        missing,
        json!({"relativePath": "t3.json", "contents": "", "byteLength": 0, "truncated": false,
               "exists": false})
    );

    assert_eq!(
        env.read(root, "dir").await.unwrap_err()["failure"],
        "path_not_file"
    );
    assert_eq!(
        env.read(root, "../x").await.unwrap_err()["failure"],
        "workspace_path_outside_root"
    );
    let error = env.read(root, "leak.txt").await.unwrap_err();
    assert_eq!(error["failure"], "resolved_path_outside_root");
}
