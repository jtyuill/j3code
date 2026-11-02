//! The git workflow (`vcs.refreshStatus`, `vcs.pull`, `git.runStackedAction`)
//! against real temporary repositories with a bare remote, and a fake omp
//! for generated commit messages.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use futures_util::StreamExt;
use j3_core::{git, omp::OmpLauncher, rpc::StreamItem, store::random_hex};
use j3_protocol::t3::git::{
    GitActionProgress, GitActionProgressEvent, GitCommitStepStatus, GitManagerServiceError,
    GitPrStepStatus, GitPushStepStatus, GitRunStackedActionInput, GitRunStackedActionResult,
    GitStackedAction, GitToastCta, VcsChangeRequestState, VcsPullStatus, VcsWorkingTreeFile,
};

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

fn input(cwd: &Path, action: GitStackedAction, message: Option<&str>) -> GitRunStackedActionInput {
    GitRunStackedActionInput {
        action_id: "action-1".into(),
        cwd: cwd.to_str().unwrap().to_owned(),
        action,
        commit_message: message.map(str::to_owned),
        feature_branch: None,
        file_paths: None,
    }
}

/// The stream's events and, when it failed, its error.
async fn run(
    omp: OmpLauncher,
    input: GitRunStackedActionInput,
) -> (Vec<GitActionProgressEvent>, Option<GitManagerServiceError>) {
    let items: Vec<StreamItem> = git::run_stacked_action(omp, input).collect().await;
    let mut events = Vec::new();
    let mut failure = None;
    for item in items {
        assert!(failure.is_none(), "nothing follows a failure");
        match item {
            StreamItem::Value(value) => events.push(serde_json::from_value(value).unwrap()),
            StreamItem::Fail(error) => failure = Some(serde_json::from_value(error).unwrap()),
        }
    }
    (events, failure)
}

fn labels(events: &[GitActionProgressEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| match &event.progress {
            GitActionProgress::ActionStarted { .. } => "started".to_owned(),
            GitActionProgress::PhaseStarted { label, .. } => label.clone(),
            GitActionProgress::ActionFinished { .. } => "finished".to_owned(),
            GitActionProgress::ActionFailed { message, .. } => format!("failed: {message}"),
            other => format!("{other:?}"),
        })
        .collect()
}

fn finished(events: &[GitActionProgressEvent]) -> GitRunStackedActionResult {
    match &events.last().unwrap().progress {
        GitActionProgress::ActionFinished { result } => (**result).clone(),
        other => panic!("expected action_finished, got {other:?}"),
    }
}

/// An omp that must not be called.
fn no_omp() -> OmpLauncher {
    OmpLauncher::new("/nonexistent/omp")
}

/// An omp that answers every one-shot prompt with `reply`.
fn fake_omp(root: &TempDir, reply: &str) -> OmpLauncher {
    let script = root.0.join("fake-omp");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ncat > /dev/null\ncat <<'EOF'\n{reply}\nEOF\n"),
    )
    .unwrap();
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
    assert!(status.has_primary_remote);
    assert!(status.is_default_ref);
    assert_eq!(status.ref_name.as_deref(), Some("main"));
    assert!(!status.has_upstream);
    assert_eq!((status.ahead_count, status.behind_count), (1, 0));
    assert!(status.has_working_tree_changes);
    assert_eq!(
        status.working_tree.files,
        [
            VcsWorkingTreeFile {
                path: "a.txt".into(),
                insertions: 2,
                deletions: 1
            },
            VcsWorkingTreeFile {
                path: "new.txt".into(),
                insertions: 3,
                deletions: 0
            },
        ]
    );
    assert_eq!(
        (
            status.working_tree.insertions,
            status.working_tree.deletions
        ),
        (5, 1)
    );
    assert_eq!(status.pr, None);

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
    assert_eq!(status.ref_name.as_deref(), Some("main"));
    assert_eq!(status.ahead_count, 0);
    assert_eq!(status.working_tree.files.len(), 1);
    assert_eq!(status.working_tree.insertions, 1);
}

#[tokio::test]
async fn commit_push_sets_the_upstream_then_skips_when_up_to_date() {
    let root = TempDir::new();
    let (work, remote) = repo(&root);
    write(&work, "a.txt", "hello\n");

    let (events, failure) = run(
        no_omp(),
        input(
            &work,
            GitStackedAction::CommitPush,
            Some("Add a\n\nbody line"),
        ),
    )
    .await;
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(
        labels(&events),
        ["started", "Committing...", "Pushing...", "finished"]
    );
    let result = finished(&events);
    assert_eq!(result.commit.status, GitCommitStepStatus::Created);
    assert_eq!(result.commit.subject.as_deref(), Some("Add a"));
    let sha = result.commit.commit_sha.clone().unwrap();
    assert_eq!(
        git(&work, &["log", "-1", "--format=%B"]),
        "Add a\n\nbody line"
    );
    assert_eq!(result.push.status, GitPushStepStatus::Pushed);
    assert_eq!(result.push.set_upstream, Some(true));
    assert_eq!(result.push.upstream_branch.as_deref(), Some("origin/main"));
    assert_eq!(git(&remote, &["rev-parse", "main"]), sha);
    assert_eq!(
        result.toast.title,
        format!("Pushed {} to origin/main", &sha[..7])
    );
    assert_eq!(result.toast.description.as_deref(), Some("Add a"));
    assert_eq!(result.toast.cta, GitToastCta::None);

    let status = git::status(work.to_str().unwrap()).await.unwrap();
    assert!(status.has_upstream);
    assert_eq!((status.ahead_count, status.behind_count), (0, 0));

    let (events, failure) = run(no_omp(), input(&work, GitStackedAction::Push, None)).await;
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(labels(&events), ["started", "Pushing...", "finished"]);
    let result = finished(&events);
    assert_eq!(result.push.status, GitPushStepStatus::SkippedUpToDate);
    assert_eq!(result.toast.title, "Done");

    // An existing upstream: plain push of a new local commit.
    write(&work, "b.txt", "b\n");
    commit_all(&work, "Add b");
    let (events, failure) = run(no_omp(), input(&work, GitStackedAction::Push, None)).await;
    assert!(failure.is_none(), "{failure:?}");
    let result = finished(&events);
    assert_eq!(result.push.status, GitPushStepStatus::Pushed);
    assert_eq!(result.push.set_upstream, Some(false));
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

    let (events, failure) = run(
        no_omp(),
        input(&work, GitStackedAction::Commit, Some("Nothing")),
    )
    .await;
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(labels(&events), ["started", "finished"]);
    let result = finished(&events);
    assert_eq!(result.commit.status, GitCommitStepStatus::SkippedNoChanges);
    assert_eq!(result.toast.title, "Done");
    assert_eq!(result.toast.cta, GitToastCta::None);
}

#[tokio::test]
async fn push_from_a_detached_head_fails() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    write(&work, "a.txt", "a\n");
    commit_all(&work, "Initial");
    git(&work, &["checkout", "--detach"]);

    let (events, failure) = run(no_omp(), input(&work, GitStackedAction::Push, None)).await;
    let message = "Git manager failed in runStackedAction: Cannot push from detached HEAD.";
    assert_eq!(
        labels(&events),
        ["started".to_owned(), format!("failed: {message}")]
    );
    let failure = failure.expect("the stream fails");
    assert!(matches!(
        failure,
        GitManagerServiceError::GitManagerError { .. }
    ));
    assert_eq!(failure.message(), message);
}

#[tokio::test]
async fn feature_branches_are_refused() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    let mut feature = input(&work, GitStackedAction::Commit, None);
    feature.feature_branch = Some(true);
    let (_, failure) = run(no_omp(), feature).await;
    assert_eq!(
        failure.unwrap().message(),
        "Git manager failed in runStackedAction: Feature branches are not supported by this server."
    );
}

#[tokio::test]
async fn pull_requests_need_a_github_remote() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    write(&work, "a.txt", "a\n");
    commit_all(&work, "Initial");
    let status = git::status(work.to_str().unwrap()).await.unwrap();
    assert_eq!(status.source_control_provider, None);

    let (events, failure) = run(no_omp(), input(&work, GitStackedAction::CreatePr, None)).await;
    assert_eq!(events.len(), 2, "started, failed");
    assert_eq!(
        failure.unwrap().message(),
        "Git manager failed in runStackedAction: Pull requests need a GitHub remote; this repository has none."
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
    let provider = status.source_control_provider.expect("a GitHub remote");
    assert_eq!(
        (provider.kind.as_str(), provider.name.as_str()),
        ("github", "GitHub")
    );
    assert_eq!(status.pr, None, "the default branch has no PR");
    assert!(gh_calls(&work).is_empty(), "and gh is not asked");

    git(&work, &["checkout", "-b", "feature"]);
    write(&work, "f.txt", "f\n");
    commit_all(&work, "Add f");
    git(&work, &["push", "-u", "origin", "feature"]);
    std::fs::write(gh_state(&work, "list.json"), PR_JSON).unwrap();

    let status = git::status(cwd).await.unwrap();
    let pr = status
        .pr
        .expect("the open PR wins over the older merged one");
    assert_eq!(pr.number, 9);
    assert_eq!(pr.state, VcsChangeRequestState::Open);
    assert_eq!(pr.is_draft, Some(true));
    assert_eq!(
        (pr.base_ref.as_str(), pr.head_ref.as_str()),
        ("main", "feature")
    );
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

    let (events, failure) = run(omp, input(&work, GitStackedAction::CreatePr, None)).await;
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(
        labels(&events),
        [
            "started",
            "Pushing...",
            "Looking for a pull request...",
            "Generating PR content...",
            "Creating pull request...",
            "finished"
        ]
    );
    let result = finished(&events);
    assert_eq!(result.push.status, GitPushStepStatus::Pushed);
    assert_eq!(
        git(&remote, &["rev-parse", "feature"]),
        git(&work, &["rev-parse", "HEAD"])
    );
    assert_eq!(result.pr.status, GitPrStepStatus::Created);
    assert_eq!(result.pr.number, Some(12));
    assert_eq!(
        result.pr.url.as_deref(),
        Some("https://github.com/acme/widgets/pull/12")
    );
    assert_eq!(result.pr.base_branch.as_deref(), Some("main"));
    assert_eq!(result.pr.head_branch.as_deref(), Some("feature"));
    assert_eq!(result.pr.title.as_deref(), Some("Add the f file"));
    assert_eq!(result.toast.title, "Created PR #12");
    assert_eq!(result.toast.description.as_deref(), Some("Add the f file"));
    assert_eq!(
        result.toast.cta,
        GitToastCta::OpenPr {
            label: "View PR".into(),
            url: "https://github.com/acme/widgets/pull/12".into()
        }
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
    let (work, _) = github_repo(&root);
    git(&work, &["checkout", "-b", "feature"]);
    write(&work, "f.txt", "f\n");
    std::fs::write(gh_state(&work, "list.json"), PR_JSON).unwrap();

    let (events, failure) = run(
        no_omp(),
        input(&work, GitStackedAction::CommitPushPr, Some("Add f")),
    )
    .await;
    assert!(failure.is_none(), "{failure:?}");
    let result = finished(&events);
    assert_eq!(result.commit.status, GitCommitStepStatus::Created);
    assert_eq!(result.push.status, GitPushStepStatus::Pushed);
    assert_eq!(result.pr.status, GitPrStepStatus::OpenedExisting);
    assert_eq!(result.pr.number, Some(9));
    assert_eq!(result.toast.title, "Opened PR #9");
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
    let (events, failure) = run(
        no_omp(),
        input(&work, GitStackedAction::CommitPush, Some("Add f")),
    )
    .await;
    assert!(failure.is_none(), "{failure:?}");
    let result = finished(&events);
    assert_eq!(result.pr.status, GitPrStepStatus::SkippedNotRequested);
    match result.toast.cta {
        GitToastCta::RunAction { label, action } => {
            assert_eq!(label, "Create PR");
            assert_eq!(action.kind, GitStackedAction::CreatePr);
        }
        other => panic!("expected a Create PR button, got {other:?}"),
    }

    // On the default branch there is nothing to open a PR from.
    git(&work, &["checkout", "main"]);
    write(&work, "m.txt", "m\n");
    let (events, _) = run(
        no_omp(),
        input(&work, GitStackedAction::CommitPush, Some("Add m")),
    )
    .await;
    assert_eq!(finished(&events).toast.cta, GitToastCta::None);
    let (_, failure) = run(no_omp(), input(&work, GitStackedAction::CreatePr, None)).await;
    assert!(failure.unwrap().message().contains("default branch"));
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
    let (events, failure) = run(omp, input(&work, GitStackedAction::CreatePr, None)).await;
    let message = failure.expect("the action fails").message().to_owned();
    assert!(message.contains("gh auth login"), "{message}");
    assert!(
        matches!(
            events.last().unwrap().progress,
            GitActionProgress::ActionFailed { .. }
        ),
        "the stream reports the failure"
    );
    // Status never fails because of gh: it just has no PR.
    git(&work, &["push", "-u", "origin", "feature"]);
    let status = git::status(work.to_str().unwrap()).await.unwrap();
    assert_eq!(status.pr, None);
}

#[tokio::test]
async fn generated_commit_messages_come_from_omp() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    let prompt = root.0.join("prompt.txt");
    let args = root.0.join("args.txt");
    let script = root.0.join("omp");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat > '{}'\n\
             printf 'Here you go:\\n```json\\n{{\"subject\": \"Add greeting.\", \"body\": \"- say hi\"}}\\n```\\n'\n",
            args.display(),
            prompt.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    write(&work, "greet.txt", "hi\n");

    let (events, failure) = run(
        OmpLauncher::new(&script),
        input(&work, GitStackedAction::Commit, None),
    )
    .await;
    assert!(failure.is_none(), "{failure:?}");
    assert_eq!(
        labels(&events),
        [
            "started",
            "Generating commit message...",
            "Committing...",
            "finished"
        ]
    );
    let result = finished(&events);
    assert_eq!(result.commit.subject.as_deref(), Some("Add greeting"));
    assert_eq!(
        git(&work, &["log", "-1", "--format=%B"]),
        "Add greeting\n\n- say hi"
    );
    let sha = result.commit.commit_sha.unwrap();
    assert_eq!(result.toast.title, format!("Committed {}", &sha[..7]));
    assert!(matches!(
        result.toast.cta,
        GitToastCta::RunAction { ref label, action } if label == "Push" && action.kind == GitStackedAction::Push
    ));

    let prompt = std::fs::read_to_string(prompt).unwrap();
    assert!(prompt.starts_with("You write concise git commit messages.\n"));
    assert!(prompt.contains("Branch: main\n"));
    assert!(prompt.contains("Staged files:\nA\tgreet.txt\n"));
    assert!(prompt.contains("+hi"));
    let args = std::fs::read_to_string(args).unwrap();
    assert!(args.starts_with("-p\n--no-session\n"));
    assert!(args.contains("--model\npi/smol\n"));
}

#[tokio::test]
async fn failed_generation_is_a_text_generation_error() {
    let root = TempDir::new();
    let (work, _) = repo(&root);
    let script = root.0.join("omp");
    std::fs::write(
        &script,
        "#!/bin/sh\ncat >/dev/null\necho nope >&2\nexit 3\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    write(&work, "a.txt", "a\n");

    let (events, failure) = run(
        OmpLauncher::new(&script),
        input(&work, GitStackedAction::Commit, None),
    )
    .await;
    let failure = failure.unwrap();
    assert!(
        matches!(failure, GitManagerServiceError::TextGenerationError { ref operation, .. } if operation == "generateCommitMessage"),
        "{failure:?}"
    );
    assert!(matches!(
        events.last().unwrap().progress,
        GitActionProgress::ActionFailed { phase: Some(_), .. }
    ));
}

#[tokio::test]
async fn pull_fast_forwards() {
    let root = TempDir::new();
    let (work, remote) = repo(&root);
    write(&work, "a.txt", "a\n");
    commit_all(&work, "Initial");
    let cwd = work.to_str().unwrap();

    let error = git::pull(cwd).await.unwrap_err();
    assert!(matches!(
        *error,
        GitManagerServiceError::GitManagerError { .. }
    ));

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
    assert_eq!((status.ahead_count, status.behind_count), (0, 1));

    let pulled = git::pull(cwd).await.unwrap();
    assert_eq!(pulled.status, VcsPullStatus::Pulled);
    assert_eq!(pulled.ref_name, "main");
    assert_eq!(pulled.upstream_ref.as_deref(), Some("origin/main"));
    assert!(work.join("b.txt").exists());

    let again = git::pull(cwd).await.unwrap();
    assert_eq!(again.status, VcsPullStatus::SkippedUpToDate);
}
