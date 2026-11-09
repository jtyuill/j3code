//! The git workflow (`git.status`, `git.pull`, `git.run`)
//! against real temporary repositories with a bare remote, and a fake omp
//! for `omp commit` and pull request text.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use futures_util::StreamExt;
use j3_core::{git, omp::OmpLauncher, store::random_hex};
use j3_protocol::v2::{
    envelope::{Error, ErrorCode},
    methods::{
        GitAction, GitFile, GitNext, GitPhase, GitRunItem, GitRunResult, PullRequestState,
        PullStatus,
    },
};
use serde_json::json;

/// A temporary directory removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("j3-git-{}", random_hex(12)));
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Identity and settings so tests do not depend on the global git config.
fn configure(dir: &Path) {
    for (key, value) in [
        ("user.name", "j3 test"),
        ("user.email", "j3@example.com"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
        ("pull.rebase", "false"),
    ] {
        git(dir, &["config", key, value]);
    }
}

/// `root/work` (branch `main`, remote `origin` = `root/remote.git`, nothing
/// pushed) and the bare remote's path.
fn repo(root: &TempDir) -> (PathBuf, PathBuf) {
    let remote = root.0.join("remote.git");
    let work = root.0.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&root.0, &["init", "--bare", "-b", "main", "remote.git"]);
    git(&work, &["init", "-b", "main"]);
    configure(&work);
    git(
        &work,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    (work, remote)
}

fn write(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).unwrap();
}

fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", message]);
}

/// What `git.run` streamed: the phases, then the result or the error.
struct Run {
    phases: Vec<GitPhase>,
    outcome: Result<GitRunResult, Error>,
}

impl Run {
    fn finished(&self) -> &GitRunResult {
        self.outcome.as_ref().expect("the action finishes")
    }

    fn error(&self) -> &Error {
        self.outcome.as_ref().expect_err("the action fails")
    }
}

async fn run(omp: OmpLauncher, cwd: &Path, action: GitAction, message: Option<&str>) -> Run {
    let items: Vec<_> = git::run(
        omp,
        cwd.to_str().unwrap().to_owned(),
        action,
        message.map(str::to_owned),
    )
    .collect()
    .await;
    let mut phases = Vec::new();
    let mut outcome = None;
    for item in items {
        assert!(outcome.is_none(), "nothing follows the result or the error");
        match item {
            Ok(GitRunItem::Phase { phase }) => phases.push(phase),
            Ok(GitRunItem::Finished { result }) => outcome = Some(Ok(result)),
            Err(error) => outcome = Some(Err(error)),
        }
    }
    Run {
        phases,
        outcome: outcome.expect("the stream ends with a result or an error"),
    }
}

/// An omp that must not be called.
fn no_omp() -> OmpLauncher {
    OmpLauncher::new("/nonexistent/omp")
}

/// An omp that answers every one-shot prompt with `reply`.
fn fake_omp(root: &TempDir, reply: &str) -> OmpLauncher {
    omp_script(
        root,
        &format!("cat > /dev/null\ncat <<'EOF'\n{reply}\nEOF\n"),
    )
}

/// An omp that runs the shell script `body`.
fn omp_script(root: &TempDir, body: &str) -> OmpLauncher {
    let script = root.0.join("fake-omp");
    std::fs::write(&script, format!("#!/bin/sh\n{body}")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    OmpLauncher::new(script)
}

#[tokio::test]
async fn status_of_a_dirty_repo() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    write(&work, "a.txt", "one\ntwo\n");
    commit_all(&work, "Initial");
    write(&work, "a.txt", "one\nTWO\nthree\n");
    write(&work, "new.txt", "x\ny\nz");

    let status = git::status(work.to_str().unwrap()).await.unwrap();
    assert!(status.is_repo);
    assert!(status.has_remote);
    assert!(status.on_default_branch);
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert!(!status.has_upstream);
    assert_eq!((status.ahead, status.behind), (1, 0));
    assert!(status.dirty);
    assert_eq!(
        status.files,
        [
            GitFile {
                path: "a.txt".into(),
                additions: 2,
                deletions: 1
            },
            GitFile {
                path: "new.txt".into(),
                additions: 3,
                deletions: 0
            },
        ]
    );
    assert_eq!((status.additions, status.deletions), (5, 1));
    assert_eq!(status.pull_request, None);

    let plain = TempDir::new();
    let status = git::status(plain.0.to_str().unwrap()).await.unwrap();
    assert!(!status.is_repo);
    let status = git::status("/nonexistent/j3/dir").await.unwrap();
    assert!(!status.is_repo);
}

#[tokio::test]
async fn status_before_the_first_commit() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    write(&work, "a.txt", "one\n");
    git(&work, &["add", "a.txt"]);
    let status = git::status(work.to_str().unwrap()).await.unwrap();
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.ahead, 0);
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.additions, 1);
}

#[tokio::test]
async fn commit_push_sets_the_upstream_then_skips_when_up_to_date() {
    let root = TempDir::new();
    let (work, remote) = repo(&root);
    write(&work, "a.txt", "hello\n");

    let run1 = run(
        no_omp(),
        &work,
        GitAction::CommitPush,
        Some("Add a\n\nbody line"),
    )
    .await;
    assert_eq!(run1.phases, [GitPhase::Commit, GitPhase::Push]);
    let result = run1.finished();
    let sha = git(&work, &["rev-parse", "HEAD"]);
    assert_eq!(
        git(&work, &["log", "-1", "--format=%B"]),
        "Add a\n\nbody line"
    );
    assert_eq!(git(&remote, &["rev-parse", "main"]), sha);
    assert_eq!(result.title, format!("Pushed {} to origin/main", &sha[..7]));
    assert_eq!(result.description.as_deref(), Some("Add a"));
    assert_eq!(result.next, None);

    let status = git::status(work.to_str().unwrap()).await.unwrap();
    assert!(status.has_upstream);
    assert_eq!((status.ahead, status.behind), (0, 0));

    let up_to_date = run(no_omp(), &work, GitAction::Push, None).await;
    assert_eq!(up_to_date.phases, [GitPhase::Push]);
    assert_eq!(up_to_date.finished().title, "Done");

    // An existing upstream: plain push of a new local commit.
    write(&work, "b.txt", "b\n");
    commit_all(&work, "Add b");
    let pushed = run(no_omp(), &work, GitAction::Push, None).await;
    assert_eq!(pushed.finished().title, "Pushed to origin/main");
    assert_eq!(
        git(&remote, &["rev-parse", "main"]),
        git(&work, &["rev-parse", "HEAD"])
    );
}

#[tokio::test]
async fn commit_without_changes_is_skipped() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    write(&work, "a.txt", "a\n");
    commit_all(&work, "Initial");

    let skipped = run(no_omp(), &work, GitAction::Commit, Some("Nothing")).await;
    assert!(skipped.phases.is_empty());
    let result = skipped.finished();
    assert_eq!(result.title, "Done");
    assert_eq!(result.description, None);
    assert_eq!(result.next, None);
}

#[tokio::test]
async fn push_from_a_detached_head_fails() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    write(&work, "a.txt", "a\n");
    commit_all(&work, "Initial");
    git(&work, &["checkout", "--detach"]);

    let failed = run(no_omp(), &work, GitAction::Push, None).await;
    assert!(failed.phases.is_empty());
    let error = failed.error();
    assert_eq!(error.code, ErrorCode::Rejected);
    assert_eq!(error.message, "Cannot push from detached HEAD.");
    assert_eq!(error.data, None, "no phase was running");
}

#[tokio::test]
async fn pull_from_a_detached_head_is_refused() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    write(&work, "a.txt", "a\n");
    commit_all(&work, "Initial");
    git(&work, &["checkout", "--detach"]);
    let error = git::pull(work.to_str().unwrap()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Rejected);
    assert_eq!(error.message, "Cannot pull from detached HEAD.");
}

#[tokio::test]
async fn pull_requests_need_a_github_remote() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    write(&work, "a.txt", "a\n");
    commit_all(&work, "Initial");
    let status = git::status(work.to_str().unwrap()).await.unwrap();
    assert!(!status.github);

    let failed = run(no_omp(), &work, GitAction::Pr, None).await;
    assert!(failed.phases.is_empty());
    let error = failed.error();
    assert_eq!(error.code, ErrorCode::Rejected);
    assert_eq!(
        error.message,
        "Pull requests need a GitHub remote; this repository has none."
    );
}

// ---------------------------------------------------------------------------
// GitHub: pull requests through a fake `gh`
// ---------------------------------------------------------------------------

/// A `gh` on `PATH` that answers from the files in `<git dir>/fake-gh` of the
/// repository it runs in and logs its arguments to `calls`, so tests do not
/// share state.
fn install_fake_gh() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let dir = std::env::temp_dir().join(format!("j3-fake-gh-{}", random_hex(8)));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("gh");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             state=\"$(git rev-parse --absolute-git-dir)/fake-gh\"\n\
             mkdir -p \"$state\"\n\
             echo \"$@\" >> \"$state/calls\"\n\
             if [ -f \"$state/fail\" ]; then cat \"$state/fail\" >&2; exit 4; fi\n\
             case \"$1 $2\" in\n\
               \"pr list\") cat \"$state/list.json\" 2>/dev/null || echo '[]' ;;\n\
               \"pr create\") echo 'Creating pull request' >&2; cat \"$state/create.url\" ;;\n\
               \"repo view\") echo main ;;\n\
               *) echo \"unexpected: $*\" >&2; exit 1 ;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![dir];
        paths.extend(std::env::split_paths(&path));
        // SAFETY: runs once, before any test starts a process that reads PATH
        // from a C library; std's own environment access is locked.
        unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
    });
}

/// Where the fake `gh` keeps `name` for the repository at `work`.
fn gh_state(work: &Path, name: &str) -> PathBuf {
    let dir = PathBuf::from(git(work, &["rev-parse", "--absolute-git-dir"])).join("fake-gh");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn gh_calls(work: &Path) -> Vec<String> {
    std::fs::read_to_string(gh_state(work, "calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Like [`repo`], with a GitHub URL for `origin` that pushes still reach the
/// bare remote through `insteadOf`.
fn github_repo(root: &TempDir) -> (PathBuf, PathBuf) {
    install_fake_gh();
    let (work, remote) = repo(root);
    let github = "git@github.com:acme/widgets.git";
    git(&work, &["remote", "set-url", "origin", github]);
    git(
        &work,
        &[
            "config",
            &format!("url.{}.insteadOf", remote.to_str().unwrap()),
            github,
        ],
    );
    // The bare remote's default branch, as a clone would know it.
    write(&work, "base.txt", "base\n");
    commit_all(&work, "Base");
    git(&work, &["push", "-u", "origin", "main"]);
    git(&work, &["remote", "set-head", "origin", "main"]);
    (work, remote)
}

const PR_JSON: &str = r#"[
  {"number": 7, "title": "Old", "url": "https://github.com/acme/widgets/pull/7",
   "baseRefName": "main", "headRefName": "feature", "state": "MERGED",
   "isDraft": false, "updatedAt": "2026-10-01T00:00:00Z"},
  {"number": 9, "title": "Add widgets", "url": "https://github.com/acme/widgets/pull/9",
   "baseRefName": "main", "headRefName": "feature", "state": "OPEN",
   "isDraft": true, "updatedAt": "2026-09-01T00:00:00Z"}
]"#;

#[tokio::test]
async fn a_github_remote_adds_the_provider_and_the_branch_pull_request() {
    let root = TempDir::new();
    let (work, _) = github_repo(&root);
    let cwd = work.to_str().unwrap();

    let status = git::status(cwd).await.unwrap();
    assert!(status.github, "a GitHub remote");
    assert_eq!(status.pull_request, None, "the default branch has no PR");
    assert!(gh_calls(&work).is_empty(), "and gh is not asked");

    git(&work, &["checkout", "-b", "feature"]);
    write(&work, "f.txt", "f\n");
    commit_all(&work, "Add f");
    git(&work, &["push", "-u", "origin", "feature"]);
    std::fs::write(gh_state(&work, "list.json"), PR_JSON).unwrap();

    let status = git::status(cwd).await.unwrap();
    let pr = status
        .pull_request
        .expect("the open PR wins over the older merged one");
    assert_eq!(pr.url, "https://github.com/acme/widgets/pull/9");
    assert_eq!(pr.state, PullRequestState::Open);
    let calls = gh_calls(&work);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(
        calls[0].starts_with("pr list --head feature --state all"),
        "{calls:?}"
    );

    // Polled again: answered from the cache.
    git::status(cwd).await.unwrap();
    assert_eq!(gh_calls(&work).len(), 1);
}

#[tokio::test]
async fn create_pr_pushes_then_opens_a_pull_request_with_omp_text() {
    let root = TempDir::new();
    let (work, remote) = github_repo(&root);
    git(&work, &["checkout", "-b", "feature"]);
    write(&work, "f.txt", "f\n");
    commit_all(&work, "Add f");
    std::fs::write(
        gh_state(&work, "create.url"),
        "https://github.com/acme/widgets/pull/12\n",
    )
    .unwrap();
    let omp = fake_omp(
        &root,
        r#"{"title": "Add the f file.", "body": "- adds f.txt"}"#,
    );

    let created = run(omp, &work, GitAction::Pr, None).await;
    assert_eq!(created.phases, [GitPhase::Push, GitPhase::Pr]);
    assert_eq!(
        git(&remote, &["rev-parse", "feature"]),
        git(&work, &["rev-parse", "HEAD"])
    );
    let result = created.finished();
    assert_eq!(result.title, "Created PR #12");
    assert_eq!(result.description.as_deref(), Some("Add the f file"));
    assert_eq!(
        result.next,
        Some(GitNext::OpenPr {
            label: "View PR".into(),
            url: "https://github.com/acme/widgets/pull/12".into()
        })
    );
    let calls = gh_calls(&work);
    let create = calls.iter().find(|c| c.starts_with("pr create")).unwrap();
    assert_eq!(
        create,
        "pr create --base main --head feature --title Add the f file --body - adds f.txt"
    );
}

#[tokio::test]
async fn commit_push_pr_commits_pushes_and_reuses_an_open_pull_request() {
    let root = TempDir::new();
    let (work, remote) = github_repo(&root);
    git(&work, &["checkout", "-b", "feature"]);
    write(&work, "f.txt", "f\n");
    std::fs::write(gh_state(&work, "list.json"), PR_JSON).unwrap();

    let reused = run(no_omp(), &work, GitAction::CommitPushPr, Some("Add f")).await;
    assert_eq!(
        reused.phases,
        [GitPhase::Commit, GitPhase::Push, GitPhase::Pr]
    );
    assert_eq!(git(&work, &["log", "-1", "--format=%s"]), "Add f");
    assert_eq!(
        git(&remote, &["rev-parse", "feature"]),
        git(&work, &["rev-parse", "HEAD"])
    );
    let result = reused.finished();
    assert_eq!(result.title, "Opened PR #9");
    assert_eq!(result.description.as_deref(), Some("Add widgets"));
    assert_eq!(
        result.next,
        Some(GitNext::OpenPr {
            label: "View PR".into(),
            url: "https://github.com/acme/widgets/pull/9".into()
        })
    );
    assert!(
        !gh_calls(&work).iter().any(|c| c.starts_with("pr create")),
        "no second PR"
    );
}

#[tokio::test]
async fn a_pushed_feature_branch_offers_to_create_the_pull_request() {
    let root = TempDir::new();
    let (work, _) = github_repo(&root);
    git(&work, &["checkout", "-b", "feature"]);
    write(&work, "f.txt", "f\n");
    let pushed = run(no_omp(), &work, GitAction::CommitPush, Some("Add f")).await;
    assert_eq!(pushed.phases, [GitPhase::Commit, GitPhase::Push]);
    assert_eq!(
        pushed.finished().next,
        Some(GitNext::Run {
            label: "Create PR".into(),
            action: GitAction::Pr
        })
    );

    // On the default branch there is nothing to open a PR from.
    git(&work, &["checkout", "main"]);
    write(&work, "m.txt", "m\n");
    let pushed = run(no_omp(), &work, GitAction::CommitPush, Some("Add m")).await;
    assert_eq!(pushed.finished().next, None);
    let refused = run(no_omp(), &work, GitAction::Pr, None).await;
    let error = refused.error();
    assert_eq!(error.code, ErrorCode::Rejected);
    assert!(error.message.contains("default branch"), "{error:?}");
    assert_eq!(error.data, Some(json!({"phase": "pr"})));
}

#[tokio::test]
async fn gh_failures_surface_with_gh_s_own_message() {
    let root = TempDir::new();
    let (work, _) = github_repo(&root);
    git(&work, &["checkout", "-b", "feature"]);
    write(&work, "f.txt", "f\n");
    commit_all(&work, "Add f");
    // What a logged-out gh looks like.
    std::fs::write(
        gh_state(&work, "fail"),
        "To get started with GitHub CLI, please run:  gh auth login\n",
    )
    .unwrap();
    let omp = fake_omp(&root, r#"{"title": "t", "body": ""}"#);
    let failed = run(omp, &work, GitAction::Pr, None).await;
    assert_eq!(failed.phases, [GitPhase::Push, GitPhase::Pr]);
    let error = failed.error();
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert!(error.message.contains("gh auth login"), "{error:?}");
    assert_eq!(error.data, Some(json!({"phase": "pr"})));
    // Status never fails because of gh: it just has no PR.
    git(&work, &["push", "-u", "origin", "feature"]);
    let status = git::status(work.to_str().unwrap()).await.unwrap();
    assert_eq!(status.pull_request, None);
}

#[tokio::test]
async fn generated_commits_are_made_by_omp_commit() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    let args = root.0.join("args.txt");
    // What `omp commit` prints and does: step lines, the agent's closing
    // remark, then a commit of what is staged.
    let omp = omp_script(
        &root,
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\n\
             echo '● Resolving model...'\n\
             echo '  └─ Fake Haiku'\n\
             echo '● Starting commit agent...'\n\
             echo '● I proposed a single feat(greet) commit.'\n\
             echo '● Creating commit...'\n\
             git commit -q -m 'feat(greet): add greeting' -m '- say hi'\n\
             echo 'Commit created.'\n",
            args.display()
        ),
    );
    write(&work, "greet.txt", "hi\n");
    write(&work, "other.txt", "also staged\n");

    let committed = run(omp, &work, GitAction::Commit, None).await;
    assert_eq!(committed.phases, [GitPhase::Commit]);
    assert_eq!(std::fs::read_to_string(args).unwrap(), "commit\n");
    let result = committed.finished();
    let sha = git(&work, &["rev-parse", "HEAD"]);
    assert_eq!(result.title, format!("Committed {}", &sha[..7]));
    assert_eq!(
        result.description.as_deref(),
        Some("feat(greet): add greeting")
    );
    assert_eq!(
        result.next,
        Some(GitNext::Run {
            label: "Push".into(),
            action: GitAction::Push
        })
    );
    // omp committed what j3 staged: everything.
    assert_eq!(
        git(&work, &["show", "--name-only", "--format=", "HEAD"]),
        "greet.txt\nother.txt"
    );
    assert_eq!(git(&work, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn omp_commit_failures_are_the_action_s_error() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    write(&work, "a.txt", "a\n");
    commit_all(&work, "Initial");

    // A refusing hook: omp reports it on stderr and exits 1.
    write(&work, "b.txt", "b\n");
    let omp = omp_script(
        &root,
        "echo '● Creating commit...'\n\
         printf '✗ Commit failed:\\n    pre-commit: lint failed\\n' >&2\nexit 1\n",
    );
    let failed = run(omp, &work, GitAction::Commit, None).await;
    assert_eq!(failed.phases, [GitPhase::Commit]);
    let error = failed.error();
    assert_eq!(error.code, ErrorCode::Unavailable);
    assert!(
        error.message.contains("pre-commit: lint failed"),
        "{error:?}"
    );
    assert_eq!(error.data, Some(json!({"phase": "commit"})));

    // omp exiting cleanly without a commit is a failure too.
    let omp = omp_script(&root, "echo 'No changes to commit.' >&2\n");
    let failed = run(omp, &work, GitAction::Commit, None).await;
    let error = failed.error();
    assert!(
        error.message.contains("omp commit made no commit"),
        "{error:?}"
    );
    assert_eq!(error.data, Some(json!({"phase": "commit"})));
}

#[tokio::test]
async fn pull_request_text_falls_back_from_commit_to_smol_to_default() {
    let root = TempDir::new();
    let (work, _) = github_repo(&root);
    git(&work, &["checkout", "-b", "feature"]);
    write(&work, "f.txt", "f\n");
    commit_all(&work, "Add f");
    std::fs::write(
        gh_state(&work, "create.url"),
        "https://github.com/acme/widgets/pull/12\n",
    )
    .unwrap();
    let tried = root.0.join("tried.txt");
    // Answers only as the default role; records every model asked.
    let omp = omp_script(
        &root,
        &format!(
            "cat >/dev/null\nwhile [ \"$1\" != --model ]; do shift; done\n\
             echo \"$2\" >> '{}'\n\
             [ \"$2\" = pi/default ] || {{ echo \"Model \\\"$2\\\" not found\" >&2; exit 1; }}\n\
             printf '{{\"title\": \"Add f\", \"body\": \"\"}}'\n",
            tried.display()
        ),
    );

    let created = run(omp, &work, GitAction::Pr, None).await;
    assert_eq!(created.finished().description.as_deref(), Some("Add f"));
    assert_eq!(
        std::fs::read_to_string(&tried).unwrap(),
        "pi/commit\npi/smol\npi/default\n"
    );
}

#[tokio::test]
async fn pull_fast_forwards() {
    let root = TempDir::new();
    let (work, remote) = repo(&root);
    write(&work, "a.txt", "a\n");
    commit_all(&work, "Initial");
    let cwd = work.to_str().unwrap();

    let error = git::pull(cwd).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Rejected);
    assert_eq!(
        error.message,
        "Current branch has no upstream configured. Push with upstream first."
    );

    git(&work, &["push", "-u", "origin", "main"]);
    let other = root.0.join("other");
    git(
        &root.0,
        &["clone", remote.to_str().unwrap(), other.to_str().unwrap()],
    );
    configure(&other);
    write(&other, "b.txt", "b\n");
    commit_all(&other, "Add b");
    git(&other, &["push"]);
    git(&work, &["fetch"]);

    let status = git::status(cwd).await.unwrap();
    assert_eq!((status.ahead, status.behind), (0, 1));

    let pulled = git::pull(cwd).await.unwrap();
    assert_eq!(pulled.status, PullStatus::Pulled);
    assert_eq!(pulled.branch, "main");
    assert_eq!(pulled.upstream.as_deref(), Some("origin/main"));
    assert!(work.join("b.txt").exists());

    let again = git::pull(cwd).await.unwrap();
    assert_eq!(again.status, PullStatus::UpToDate);
}
