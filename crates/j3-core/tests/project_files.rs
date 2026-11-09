//! `projects.listEntries`, `projects.searchEntries` and `projects.readFile`
//! (wire.md §5.3) over the v2 WebSocket against the real `App`, on temporary
//! project directories.

mod engine_support;
mod v2_support;

use std::{path::Path, process::Command, sync::Arc};

use engine_support::{Script, open};
use j3_core::app::App;
use serde_json::{Value, json};
use v2_support::{Client, TempDir, serve};

/// Most entries `projects.listEntries` returns.
const MAX_ENTRIES: usize = 2_000;
/// Most bytes `projects.readFile` returns.
const MAX_READ_BYTES: usize = 1 << 20;

struct Env {
    ws: Client,
    app: Arc<App>,
    _data: TempDir,
}

impl Env {
    async fn start() -> Self {
        let data = TempDir::new("files-data");
        let app = open(&data.0, &Arc::new(Script::default())).await;
        let addr = serve(&app).await;
        Self {
            ws: Client::connect(addr).await,
            app,
            _data: data,
        }
    }

    async fn finish(self) {
        self.ws.close().await;
        self.app.shutdown().await;
    }

    /// `projects.create` on `root`; the project id.
    async fn project(&mut self, root: &Path) -> String {
        let title = root.file_name().unwrap().to_string_lossy().into_owned();
        let created = self
            .ws
            .ok("projects.create", json!({"title": title, "path": root}))
            .await;
        created["project"]["id"].as_str().unwrap().to_owned()
    }

    async fn list(&mut self, project: &str, dir: Option<&str>) -> Result<Value, Value> {
        let mut params = json!({"projectId": project});
        if let Some(dir) = dir {
            params["dir"] = json!(dir);
        }
        self.ws.call("projects.listEntries", params).await
    }

    async fn read(&mut self, project: &str, path: &str) -> Result<Value, Value> {
        self.ws
            .call(
                "projects.readFile",
                json!({"projectId": project, "path": path}),
            )
            .await
    }

    /// The paths `query` finds, best first.
    async fn search(&mut self, project: &str, query: &str) -> Vec<String> {
        let found = self
            .ws
            .ok(
                "projects.searchEntries",
                json!({"projectId": project, "query": query, "limit": 50}),
            )
            .await;
        rows(&found).into_iter().map(|(path, ..)| path).collect()
    }
}

fn write(root: &Path, path: &str, contents: impl AsRef<[u8]>) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn git_init(root: &Path) {
    let git = Command::new("git")
        .args(["init", "-q"])
        .current_dir(root)
        .status()
        .unwrap();
    assert!(git.success());
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
    git_init(root);
    write(root, ".gitignore", "target/\n*.log\n");
    write(root, "b.rs", "");
    write(root, "A.md", "");
    write(root, "debug.log", "");
    write(root, "src/main.rs", "fn main() {}\n");
    write(root, "src/lib/mod.rs", "");
    write(root, "target/debug/j3", "");
    write(root, "Zeta/x", "");
    let id = env.project(root).await;

    let listed = env.list(&id, None).await.unwrap();
    assert_eq!(listed["truncated"], false);
    // Directories first, then case-insensitive names; `.git` is left out.
    assert_eq!(
        rows(&listed),
        [
            row("src", "dir", false),
            row("target", "dir", true),
            row("Zeta", "dir", false),
            row(".gitignore", "file", false),
            row("A.md", "file", false),
            row("b.rs", "file", false),
            row("debug.log", "file", true),
        ]
    );
    // Absent `ignored` is omitted on the wire.
    assert!(listed["entries"][0].get("ignored").is_none());

    // One level below, paths stay relative to the project.
    assert_eq!(
        rows(&env.list(&id, Some("src")).await.unwrap()),
        [
            row("src/lib", "dir", false),
            row("src/main.rs", "file", false)
        ]
    );
    assert_eq!(
        rows(&env.list(&id, Some("./src/lib/")).await.unwrap()),
        [row("src/lib/mod.rs", "file", false)]
    );
    // Inside an ignored directory everything is ignored.
    assert_eq!(
        rows(&env.list(&id, Some("target")).await.unwrap()),
        [row("target/debug", "dir", true)]
    );
    // Empty is the root.
    assert_eq!(rows(&env.list(&id, Some("")).await.unwrap()).len(), 7);
    env.finish().await;
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
    let id = env.project(root).await;
    let listed = env.list(&id, None).await.unwrap();
    assert_eq!(
        rows(&listed),
        [row("many", "dir", false), row("target", "dir", false)]
    );

    let many = env.list(&id, Some("many")).await.unwrap();
    assert_eq!(many["truncated"], true);
    let rows = rows(&many);
    assert_eq!(rows.len(), MAX_ENTRIES);
    assert_eq!(rows[0], row("many/f00000.txt", "file", false));
    env.finish().await;
}

#[tokio::test]
async fn search_entries_matches_like_omps_at_completion() {
    let mut env = Env::start().await;
    let project = TempDir::new("files-search");
    let root = &project.0;
    git_init(root);
    write(root, ".gitignore", "target/\n");
    write(root, "src/main.rs", "");
    write(root, "src/domain/mailbox.rs", "");
    write(root, "docs/main-loop.md", "");
    write(root, "target/main.rs", "");
    write(root, ".github/ci.yml", "");
    let id = env.project(root).await;

    // Fuzzy over the walk: names starting with the query, names containing
    // it, paths containing it, then tight subsequences; ignored files and
    // `.git` never show, hidden ones do.
    assert_eq!(
        env.search(&id, "main").await,
        [
            "src/main.rs",
            "docs/main-loop.md",
            "src/domain",
            "src/domain/mailbox.rs"
        ]
    );
    assert_eq!(
        env.search(&id, "smrs").await,
        ["src/main.rs", "src/domain/mailbox.rs"]
    );
    assert_eq!(env.search(&id, "ci.y").await, [".github/ci.yml"]);
    assert!(env.search(&id, "HEAD").await.is_empty());

    // An empty query or `dir/` lists one level, directories first; `dir/rest`
    // searches under `dir`.
    assert_eq!(
        env.search(&id, "").await,
        [".github", "docs", "src", "target", ".gitignore"]
    );
    assert_eq!(env.search(&id, "src/").await, ["src/domain", "src/main.rs"]);
    assert_eq!(
        env.search(&id, "src/ma").await,
        ["src/main.rs", "src/domain/mailbox.rs", "src/domain"]
    );

    // An unknown project names itself.
    let unknown = env
        .ws
        .err(
            "projects.searchEntries",
            json!({"projectId": "nope", "query": "x", "limit": 5}),
        )
        .await;
    assert_eq!(unknown["code"], "not_found", "{unknown}");
    env.finish().await;
}

#[tokio::test]
async fn list_entries_refuses_paths_outside_the_project() {
    let mut env = Env::start().await;
    let outside = TempDir::new("files-outside");
    write(&outside.0, "secret.txt", "s");
    let project = TempDir::new("files-escape");
    let root = &project.0;
    write(root, "inside/a.txt", "");
    std::os::unix::fs::symlink(&outside.0, root.join("out")).unwrap();
    std::os::unix::fs::symlink(root.join("inside"), root.join("in")).unwrap();
    let id = env.project(root).await;

    // Symlinks list as what they point at.
    assert_eq!(
        rows(&env.list(&id, None).await.unwrap()),
        [
            row("in", "dir", false),
            row("inside", "dir", false),
            row("out", "dir", false),
        ]
    );
    // A link staying inside lists under its own path.
    assert_eq!(
        rows(&env.list(&id, Some("in")).await.unwrap()),
        [row("in/a.txt", "file", false)]
    );
    for dir in ["..", "inside/../..", "/etc", "out"] {
        let error = env.list(&id, Some(dir)).await.unwrap_err();
        assert_eq!(error["code"], "rejected", "{dir}: {error}");
        assert!(
            error["message"].as_str().unwrap().contains("outside"),
            "{error}"
        );
    }
    let error = env.list(&id, Some("missing")).await.unwrap_err();
    assert_eq!(error["code"], "rejected", "{error}");
    let error = env.list("nope", None).await.unwrap_err();
    assert_eq!(error["code"], "not_found", "{error}");
    // A project whose folder is gone refuses to list.
    std::fs::remove_dir_all(root).unwrap();
    let error = env.list(&id, None).await.unwrap_err();
    assert_eq!(error["code"], "rejected", "{error}");
    env.finish().await;
}

#[tokio::test]
async fn read_file_text_truncation_and_absolute_paths() {
    let mut env = Env::start().await;
    let project = TempDir::new("files-read");
    let root = &project.0;
    write(root, "src/main.rs", "fn main() {}\n");
    let id = env.project(root).await;

    let read = env.read(&id, "src/main.rs").await.unwrap();
    assert_eq!(
        read,
        json!({"path": "src/main.rs", "text": "fn main() {}\n", "size": 13,
               "truncated": false, "exists": true})
    );
    // Normalized, and absolute paths inside the project are read as well.
    assert_eq!(
        env.read(&id, "./src/../src/main.rs").await.unwrap()["path"],
        "src/main.rs"
    );
    let absolute = root.join("src/main.rs");
    let read = env.read(&id, absolute.to_str().unwrap()).await.unwrap();
    assert_eq!(read["text"], "fn main() {}\n");

    // Large files: the first MiB, with the whole size.
    let size = MAX_READ_BYTES + 10;
    write(root, "big.txt", "x".repeat(size));
    let read = env.read(&id, "big.txt").await.unwrap();
    assert_eq!(read["truncated"], true);
    assert_eq!(read["size"], size);
    assert_eq!(read["text"].as_str().unwrap().len(), MAX_READ_BYTES);
    env.finish().await;
}

#[tokio::test]
async fn read_file_binary_missing_and_escapes_are_refused() {
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
    let id = env.project(root).await;

    let error = env.read(&id, "logo.png").await.unwrap_err();
    assert_eq!(error["code"], "rejected", "{error}");
    assert!(env.read(&id, "late.txt").await.is_ok());

    // A missing file without `allowMissing` is not found.
    let error = env.read(&id, "gone.txt").await.unwrap_err();
    assert_eq!(error["code"], "not_found", "{error}");
    assert_eq!(
        error["data"],
        json!({"kind": "file", "id": "gone.txt"}),
        "{error}"
    );
    // A config file the client may or may not find.
    let missing = env
        .ws
        .ok(
            "projects.readFile",
            json!({"projectId": id, "path": "j3.json", "allowMissing": true}),
        )
        .await;
    assert_eq!(
        missing,
        json!({"path": "j3.json", "text": "", "size": 0, "truncated": false, "exists": false})
    );

    for path in ["dir", "../x", "leak.txt"] {
        let error = env.read(&id, path).await.unwrap_err();
        assert_eq!(error["code"], "rejected", "{path}: {error}");
    }
    let error = env.read("nope", "a.txt").await.unwrap_err();
    assert_eq!(error["code"], "not_found", "{error}");
    env.finish().await;
}
