//! t3's git workflow over the host's `git` and `gh` CLIs (`vcs.refreshStatus`,
//! `vcs.pull`, `git.runStackedAction`): status, fast-forward pull, and the
//! commit / push / pull request stacked actions with omp-generated commit
//! messages and PR text. Credentials, identity and `gh` login are the host
//! user's own (`~/.gitconfig`, `~/.config/gh`), nothing is configured here.
//! Pull requests need a GitHub remote; feature branches are not served.
//!
//! Mirrors t3's `GitManager` (`apps/server/src/git/GitManager.ts`) and
//! `GitVcsDriverCore` (`apps/server/src/vcs/GitVcsDriverCore.ts`).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{LazyLock, Mutex, PoisonError},
    time::{Duration, Instant},
};

use futures_util::stream::{self, BoxStream, StreamExt};
use j3_protocol::t3::git::{
    GitActionProgress, GitActionProgressEvent, GitActionProgressPhase, GitBranchStep,
    GitBranchStepStatus, GitCommitStep, GitCommitStepStatus, GitManagerServiceError, GitPrStep,
    GitPrStepStatus, GitPushStep, GitPushStepStatus, GitRunStackedActionInput,
    GitRunStackedActionResult, GitStackedAction, GitToast, GitToastCta, GitToastRunAction,
    SourceControlProviderInfo, VcsChangeRequest, VcsChangeRequestState, VcsPullResult,
    VcsPullStatus, VcsStatusResult, VcsWorkingTree, VcsWorkingTreeFile,
};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{omp::OmpLauncher, rpc::StreamItem};

/// Boxed: the error is large and only built on failure.
pub type GitResult<T> = Result<T, Box<GitManagerServiceError>>;

/// Default timeout of one git command.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// `git commit` (hooks), `git push` and `git pull` (network).
const LONG_GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// The tree of a repository without commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// Untracked files larger than this count no lines.
const MAX_COUNTED_FILE_BYTES: u64 = 1024 * 1024;
/// t3's `SHORT_SHA_LENGTH`.
const SHORT_SHA_LENGTH: usize = 7;
/// t3's `TOAST_DESCRIPTION_MAX`.
const TOAST_DESCRIPTION_MAX: usize = 72;
/// The system prompt of commit message generation.
const COMMIT_SYSTEM_PROMPT: &str = "You write concise git commit messages.";
/// The system prompt of pull request text generation.
const PR_SYSTEM_PROMPT: &str = "You write GitHub pull request titles and descriptions.";
/// How long a branch's pull request is remembered between status polls.
const PR_CACHE_TTL: Duration = Duration::from_secs(60);
/// `gh` talks to the network; `gh pr create` also runs `git push` hooks.
const GH_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Running git
// ---------------------------------------------------------------------------

/// `git` (or `gh`) in one directory. `cwd` is the client's string, used in
/// errors. Both run with the server's environment, so they use the host
/// user's git config, ssh keys and `gh` login; only prompts are switched off,
/// because nobody is there to answer them.
struct Git<'a> {
    program: &'static str,
    dir: &'a Path,
    cwd: &'a str,
}

impl<'a> Git<'a> {
    fn new(dir: &'a Path, cwd: &'a str) -> Self {
        Self {
            program: "git",
            dir,
            cwd,
        }
    }

    /// The GitHub CLI, run in `dir` like [`Git::new`].
    fn gh(dir: &'a Path, cwd: &'a str) -> Self {
        Self {
            program: "gh",
            dir,
            cwd,
        }
    }

    /// Runs `<program> <args>` and returns its output whatever the exit
    /// status.
    async fn output(&self, operation: &str, args: &[&str], timeout: Duration) -> GitResult<Output> {
        let mut command = tokio::process::Command::new(self.program);
        command
            .args(args)
            .current_dir(self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_PAGER", "cat")
            .env("NO_COLOR", "1")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(timeout, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Err(self.error(
                operation,
                args,
                None,
                format!("`{}` is not installed on this host.", self.program),
            )),
            Ok(Err(error)) => Err(self.error(operation, args, None, format!("{error}"))),
            Err(_) => Err(self.error(
                operation,
                args,
                None,
                format!("Timed out after {}s.", timeout.as_secs()),
            )),
        }
    }

    /// Runs `git <args>`; a non-zero exit is a `GitCommandError`.
    async fn run_with(
        &self,
        operation: &str,
        args: &[&str],
        timeout: Duration,
    ) -> GitResult<String> {
        let output = self.output(operation, args, timeout).await?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = [stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|text| !text.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{} failed", command_line(self.program, args)));
        Err(self.error(operation, args, output.status.code().map(i64::from), detail))
    }

    async fn run(&self, operation: &str, args: &[&str]) -> GitResult<String> {
        self.run_with(operation, args, GIT_TIMEOUT).await
    }

    /// Trimmed stdout when `git <args>` succeeds, `None` otherwise.
    async fn try_line(&self, args: &[&str]) -> GitResult<Option<String>> {
        let output = self.output("probe", args, GIT_TIMEOUT).await?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
    }

    fn error(
        &self,
        operation: &str,
        args: &[&str],
        exit_code: Option<i64>,
        detail: String,
    ) -> Box<GitManagerServiceError> {
        Box::new(GitManagerServiceError::command(
            operation,
            command_line(self.program, args),
            self.cwd,
            exit_code,
            detail,
        ))
    }
}

fn command_line(program: &str, args: &[&str]) -> String {
    let mut line = String::from(program);
    for arg in args {
        line.push(' ');
        line.push_str(arg);
    }
    line
}

fn manager(operation: &str, cwd: &str, detail: impl Into<String>) -> Box<GitManagerServiceError> {
    Box::new(GitManagerServiceError::manager(operation, cwd, detail))
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// What the stacked actions need beyond [`VcsStatusResult`].
struct Details {
    status: VcsStatusResult,
    /// `branch.upstream` of `git status`, e.g. `origin/main`.
    upstream: Option<String>,
    /// `origin` when present, else the first remote.
    primary_remote: Option<String>,
}

/// `vcs.refreshStatus`: the working tree and branch state of `cwd`, and on a
/// GitHub remote the branch's pull request.
pub async fn status(cwd: &str) -> GitResult<VcsStatusResult> {
    let mut status = details(cwd).await?.status;
    // A pushed feature branch may have a PR; the default branch and an
    // unpublished branch cannot, so `gh` is not asked.
    if status.source_control_provider.is_some()
        && status.has_upstream
        && !status.is_default_ref
        && let Some(branch) = &status.ref_name
    {
        status.pr = cached_pull_request(cwd, branch).await;
    }
    Ok(status)
}

async fn details(cwd: &str) -> GitResult<Details> {
    let not_a_repo = || Details {
        status: VcsStatusResult::not_a_repo(),
        upstream: None,
        primary_remote: None,
    };
    let dir = Path::new(cwd);
    if !dir.is_dir() {
        return Ok(not_a_repo());
    }
    let git = Git::new(dir, cwd);
    let Some(top) = git
        .try_line(&["rev-parse", "--show-toplevel"])
        .await?
        .filter(|top| !top.is_empty())
    else {
        return Ok(not_a_repo());
    };
    // Paths of `status` and `diff` are relative to the top level.
    let top = PathBuf::from(top);
    let git = Git::new(&top, cwd);

    let ref_name = git
        .try_line(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await?;
    let has_commits = git
        .try_line(&["rev-parse", "--verify", "--quiet", "HEAD"])
        .await?
        .is_some();

    let porcelain = git
        .run(
            "GitVcsDriver.statusDetails.status",
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "--untracked-files=all",
                "--no-renames",
                "-z",
            ],
        )
        .await?;
    let parsed = parse_porcelain(&porcelain);

    let base = if has_commits { "HEAD" } else { EMPTY_TREE };
    let numstat = git
        .run(
            "GitVcsDriver.statusDetails.numstat",
            &[
                "-c",
                "core.quotePath=false",
                "diff",
                base,
                "--numstat",
                "--no-renames",
                "-z",
            ],
        )
        .await?;
    let counts = parse_numstat(&numstat);

    let mut working_tree = VcsWorkingTree::default();
    for entry in parsed.entries {
        let (insertions, deletions) = if entry.untracked {
            (count_lines(&top.join(&entry.path)).await, 0)
        } else {
            counts.get(entry.path.as_str()).copied().unwrap_or((0, 0))
        };
        working_tree.insertions += insertions;
        working_tree.deletions += deletions;
        working_tree.files.push(VcsWorkingTreeFile {
            path: entry.path,
            insertions,
            deletions,
        });
    }

    let remotes = git
        .run("GitVcsDriver.statusDetails.remotes", &["remote"])
        .await?;
    let remotes: Vec<&str> = remotes
        .lines()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect();
    let primary_remote = remotes
        .iter()
        .find(|remote| **remote == "origin")
        .or(remotes.first())
        .map(|remote| (*remote).to_owned());

    let source_control_provider = match &primary_remote {
        // The URL as configured, before `url.*.insteadOf` rewrites it.
        Some(remote) => git
            .try_line(&["config", "--get", &format!("remote.{remote}.url")])
            .await?
            .filter(|url| is_github_remote(url))
            .map(|_| SourceControlProviderInfo::github()),
        None => None,
    };

    let is_default_ref = match &ref_name {
        None => false,
        Some(branch) => {
            let remote_head = match &primary_remote {
                Some(remote) => git
                    .try_line(&[
                        "symbolic-ref",
                        "--quiet",
                        "--short",
                        &format!("refs/remotes/{remote}/HEAD"),
                    ])
                    .await?
                    .and_then(|head| head.strip_prefix(&format!("{remote}/")).map(str::to_owned)),
                None => None,
            };
            match remote_head {
                Some(head) => *branch == head,
                None => branch == "main" || branch == "master",
            }
        }
    };

    let has_upstream = parsed.upstream.is_some();
    let (ahead_count, behind_count) = if has_upstream {
        parsed.ahead_behind
    } else if primary_remote.is_some() && has_commits {
        let count = git
            .run(
                "GitVcsDriver.statusDetails.aheadCount",
                &["rev-list", "--count", "HEAD", "--not", "--remotes"],
            )
            .await?;
        (count.trim().parse().unwrap_or(0), 0)
    } else {
        (0, 0)
    };

    Ok(Details {
        status: VcsStatusResult {
            is_repo: true,
            source_control_provider,
            has_primary_remote: primary_remote.is_some(),
            is_default_ref,
            ref_name,
            has_working_tree_changes: !working_tree.files.is_empty(),
            working_tree,
            has_upstream,
            ahead_count,
            behind_count,
            ahead_of_default_count: None,
            pr: None,
        },
        upstream: parsed.upstream,
        primary_remote,
    })
}

struct StatusEntry {
    path: String,
    untracked: bool,
}

#[derive(Default)]
struct Porcelain {
    upstream: Option<String>,
    ahead_behind: (u64, u64),
    entries: Vec<StatusEntry>,
}

/// Parses `git status --porcelain=v2 --branch -z --no-renames`.
fn parse_porcelain(output: &str) -> Porcelain {
    let mut parsed = Porcelain::default();
    for record in output.split('\0').filter(|record| !record.is_empty()) {
        if let Some(header) = record.strip_prefix("# ") {
            if let Some(upstream) = header.strip_prefix("branch.upstream ") {
                parsed.upstream = Some(upstream.to_owned());
            } else if let Some(ab) = header.strip_prefix("branch.ab ") {
                let mut counts = ab
                    .split(' ')
                    .map(|count| count.trim_start_matches(['+', '-']).parse().unwrap_or(0));
                parsed.ahead_behind = (counts.next().unwrap_or(0), counts.next().unwrap_or(0));
            }
            continue;
        }
        // Fields before the path: `1` has 8, `2` (renames, off here) 9, `u` 10.
        let (fields, untracked) = match record.as_bytes()[0] {
            b'1' => (8, false),
            b'2' => (9, false),
            b'u' => (10, false),
            b'?' => (1, true),
            _ => continue,
        };
        if let Some(path) = record.splitn(fields + 1, ' ').nth(fields) {
            parsed.entries.push(StatusEntry {
                path: path.to_owned(),
                untracked,
            });
        }
    }
    parsed
}

/// Parses `git diff --numstat -z --no-renames`: path -> (insertions,
/// deletions); binary files (`-`) count 0.
fn parse_numstat(output: &str) -> HashMap<&str, (u64, u64)> {
    output
        .split('\0')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').splitn(3, '\t');
            let insertions = fields.next()?.parse().unwrap_or(0);
            let deletions = fields.next()?.parse().unwrap_or(0);
            Some((fields.next()?, (insertions, deletions)))
        })
        .collect()
}

/// Lines of an untracked regular file; 0 for large or binary files.
async fn count_lines(path: &Path) -> u64 {
    let Ok(metadata) = tokio::fs::symlink_metadata(path).await else {
        return 0;
    };
    if !metadata.is_file() || metadata.len() > MAX_COUNTED_FILE_BYTES {
        return 0;
    }
    let Ok(bytes) = tokio::fs::read(path).await else {
        return 0;
    };
    if bytes.contains(&0) {
        return 0;
    }
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
    match bytes.last() {
        Some(b'\n') | None => newlines,
        Some(_) => newlines + 1,
    }
}

// ---------------------------------------------------------------------------
// Pull
// ---------------------------------------------------------------------------

/// `vcs.pull`: `git pull --ff-only` on a branch with an upstream.
pub async fn pull(cwd: &str) -> GitResult<VcsPullResult> {
    const OPERATION: &str = "GitVcsDriver.pullCurrentBranch";
    let details = details(cwd).await?;
    if !details.status.is_repo {
        return Err(manager(OPERATION, cwd, "Not a git repository."));
    }
    let Some(ref_name) = details.status.ref_name else {
        return Err(manager(OPERATION, cwd, "Cannot pull from detached HEAD."));
    };
    if details.upstream.is_none() {
        return Err(manager(
            OPERATION,
            cwd,
            "Current branch has no upstream configured. Push with upstream first.",
        ));
    }
    let git = Git::new(Path::new(cwd), cwd);
    let before = git
        .run(
            "GitVcsDriver.pullCurrentBranch.beforeSha",
            &["rev-parse", "HEAD"],
        )
        .await?;
    git.run_with(
        "GitVcsDriver.pullCurrentBranch.pull",
        &["pull", "--ff-only"],
        LONG_GIT_TIMEOUT,
    )
    .await?;
    let after = git
        .run(
            "GitVcsDriver.pullCurrentBranch.afterSha",
            &["rev-parse", "HEAD"],
        )
        .await?;
    Ok(VcsPullResult {
        status: if before.trim() == after.trim() {
            VcsPullStatus::SkippedUpToDate
        } else {
            VcsPullStatus::Pulled
        },
        ref_name,
        upstream_ref: details.upstream,
    })
}

// ---------------------------------------------------------------------------
// Stacked actions
// ---------------------------------------------------------------------------

/// `git.runStackedAction`: a stream of [`GitActionProgressEvent`]s that ends
/// after `action_finished`, or fails with the [`GitManagerServiceError`]
/// right after `action_failed`. The action runs on its own task, so a client
/// that goes away does not interrupt a commit or push half way.
pub fn run_stacked_action(
    omp: OmpLauncher,
    input: GitRunStackedActionInput,
) -> BoxStream<'static, StreamItem> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let progress = Progress {
            tx,
            input: &input,
            phase: Mutex::new(None),
        };
        if let Err(error) = run_action(&omp, &input, &progress).await {
            progress.emit(GitActionProgress::ActionFailed {
                phase: progress.phase(),
                message: error.message().to_owned(),
            });
            let _ = progress.tx.send(StreamItem::Fail(
                serde_json::to_value(&error).expect("wire types always encode"),
            ));
        }
    });
    stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed()
}

/// Emits progress events of one action and tracks its current phase.
struct Progress<'a> {
    tx: mpsc::UnboundedSender<StreamItem>,
    input: &'a GitRunStackedActionInput,
    phase: Mutex<Option<GitActionProgressPhase>>,
}

impl Progress<'_> {
    fn emit(&self, progress: GitActionProgress) {
        let event = GitActionProgressEvent {
            action_id: self.input.action_id.clone(),
            cwd: self.input.cwd.clone(),
            action: self.input.action,
            progress,
        };
        // The client may be gone; the action still completes.
        let _ = self.tx.send(StreamItem::Value(
            serde_json::to_value(&event).expect("wire types always encode"),
        ));
    }

    fn phase_started(&self, phase: GitActionProgressPhase, label: &str) {
        self.set_phase(phase);
        self.emit(GitActionProgress::PhaseStarted {
            phase,
            label: label.to_owned(),
        });
    }

    fn set_phase(&self, phase: GitActionProgressPhase) {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner) = Some(phase);
    }

    fn phase(&self) -> Option<GitActionProgressPhase> {
        *self.phase.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn run_action(
    omp: &OmpLauncher,
    input: &GitRunStackedActionInput,
    progress: &Progress<'_>,
) -> GitResult<GitRunStackedActionResult> {
    const OPERATION: &str = "runStackedAction";
    let cwd = input.cwd.as_str();
    if input.feature_branch == Some(true) {
        return Err(manager(
            OPERATION,
            cwd,
            "Feature branches are not supported by this server.",
        ));
    }
    let initial = details(cwd).await?;
    if !initial.status.is_repo {
        return Err(manager(OPERATION, cwd, "Not a git repository."));
    }
    let wants_commit = input.action.commits();
    let wants_pr = matches!(
        input.action,
        GitStackedAction::CreatePr | GitStackedAction::CommitPushPr
    );
    // A pull request needs its branch on the remote; `push_step` does
    // nothing for a branch that is there already.
    let wants_push = input.action.pushes() || wants_pr;
    let phases = [
        wants_commit.then_some(GitActionProgressPhase::Commit),
        wants_push.then_some(GitActionProgressPhase::Push),
        wants_pr.then_some(GitActionProgressPhase::Pr),
    ]
    .into_iter()
    .flatten()
    .collect();
    progress.emit(GitActionProgress::ActionStarted { phases });

    let branch = initial.status.ref_name.clone();
    if (wants_push || wants_pr) && branch.is_none() {
        return Err(manager(OPERATION, cwd, "Cannot push from detached HEAD."));
    }
    if wants_pr && initial.status.source_control_provider.is_none() {
        return Err(manager(
            OPERATION,
            cwd,
            "Pull requests need a GitHub remote; this repository has none.",
        ));
    }

    let commit = if wants_commit {
        progress.set_phase(GitActionProgressPhase::Commit);
        commit_step(omp, input, branch.as_deref(), progress).await?
    } else {
        GitCommitStep {
            status: GitCommitStepStatus::SkippedNotRequested,
            commit_sha: None,
            subject: None,
        }
    };

    let push = match (&branch, wants_push) {
        (Some(branch), true) => {
            progress.phase_started(GitActionProgressPhase::Push, "Pushing...");
            push_step(cwd, branch).await?
        }
        _ => GitPushStep {
            status: GitPushStepStatus::SkippedNotRequested,
            branch: None,
            upstream_branch: None,
            set_upstream: None,
        },
    };

    let pr = match (&branch, wants_pr) {
        (Some(branch), true) => {
            progress.set_phase(GitActionProgressPhase::Pr);
            // The push moved the branch; read the state it left.
            let current = details(cwd).await?;
            pr_step(omp, cwd, branch, &current, input.model.as_deref(), progress).await?
        }
        _ => GitPrStep {
            status: GitPrStepStatus::SkippedNotRequested,
            url: None,
            number: None,
            base_branch: None,
            head_branch: None,
            title: None,
        },
    };

    // A pushed feature branch without a pull request gets one offered.
    let offers_pr = pr.status == GitPrStepStatus::SkippedNotRequested
        && push.status == GitPushStepStatus::Pushed
        && initial.status.source_control_provider.is_some()
        && !initial.status.is_default_ref;
    let toast = toast(input.action, &commit, &push, &pr, offers_pr);
    let result = GitRunStackedActionResult {
        action: input.action,
        branch: GitBranchStep {
            status: GitBranchStepStatus::SkippedNotRequested,
            name: None,
        },
        commit,
        push,
        pr,
        toast,
    };
    progress.emit(GitActionProgress::ActionFinished {
        result: Box::new(result.clone()),
    });
    Ok(result)
}

/// t3's `runCommitStep`: stage, resolve the message, commit.
async fn commit_step(
    omp: &OmpLauncher,
    input: &GitRunStackedActionInput,
    branch: Option<&str>,
    progress: &Progress<'_>,
) -> GitResult<GitCommitStep> {
    let cwd = input.cwd.as_str();
    let git = Git::new(Path::new(cwd), cwd);
    let custom = input
        .commit_message
        .as_deref()
        .and_then(parse_custom_commit_message);
    if custom.is_none() {
        progress.phase_started(
            GitActionProgressPhase::Commit,
            "Generating commit message...",
        );
    }

    // t3's `prepareCommitContext`.
    match input.file_paths.as_deref() {
        Some(paths) if !paths.is_empty() => {
            let _ = git
                .run("GitVcsDriver.prepareCommitContext.reset", &["reset"])
                .await;
            let mut args = vec!["--literal-pathspecs", "add", "-A", "--"];
            args.extend(paths.iter().map(String::as_str));
            git.run("GitVcsDriver.prepareCommitContext.addSelected", &args)
                .await?;
        }
        _ => {
            git.run("GitVcsDriver.prepareCommitContext.addAll", &["add", "-A"])
                .await?;
        }
    }
    let staged_summary = git
        .run(
            "GitVcsDriver.prepareCommitContext.stagedSummary",
            &["diff", "--cached", "--name-status"],
        )
        .await?;
    let staged_summary = staged_summary.trim();
    if staged_summary.is_empty() {
        return Ok(GitCommitStep {
            status: GitCommitStepStatus::SkippedNoChanges,
            commit_sha: None,
            subject: None,
        });
    }

    let message = match custom {
        Some(message) => message,
        None => {
            let staged_patch = git
                .run(
                    "GitVcsDriver.prepareCommitContext.stagedPatch",
                    &["diff", "--no-ext-diff", "--cached", "--patch", "--minimal"],
                )
                .await?;
            let prompt = commit_message_prompt(branch, staged_summary, &staged_patch);
            generate_commit_message(omp, &prompt, Path::new(cwd), input.model.as_deref()).await?
        }
    };

    progress.phase_started(GitActionProgressPhase::Commit, "Committing...");
    let mut args = vec!["commit", "-m", message.subject.as_str()];
    if !message.body.is_empty() {
        args.extend(["-m", message.body.as_str()]);
    }
    git.run_with("GitVcsDriver.commit.commit", &args, LONG_GIT_TIMEOUT)
        .await?;
    let sha = git
        .run("GitVcsDriver.commit.revParseHead", &["rev-parse", "HEAD"])
        .await?;
    Ok(GitCommitStep {
        status: GitCommitStepStatus::Created,
        commit_sha: Some(sha.trim().to_owned()),
        subject: Some(message.subject),
    })
}

/// t3's `pushCurrentBranch` without a requested remote.
async fn push_step(cwd: &str, branch: &str) -> GitResult<GitPushStep> {
    const OPERATION: &str = "GitVcsDriver.pushCurrentBranch";
    let details = details(cwd).await?;
    let git = Git::new(Path::new(cwd), cwd);
    if let Some(upstream) = &details.upstream {
        if details.status.ahead_count == 0 {
            return Ok(GitPushStep {
                status: GitPushStepStatus::SkippedUpToDate,
                branch: Some(branch.to_owned()),
                upstream_branch: Some(upstream.clone()),
                set_upstream: None,
            });
        }
        let remote = git
            .try_line(&["config", &format!("branch.{branch}.remote")])
            .await?;
        let merge = git
            .try_line(&["config", &format!("branch.{branch}.merge")])
            .await?;
        if let (Some(remote), Some(merge)) = (remote, merge) {
            let target = merge.strip_prefix("refs/heads/").unwrap_or(&merge);
            git.run_with(
                "GitVcsDriver.pushCurrentBranch.pushUpstream",
                &["push", &remote, &format!("HEAD:refs/heads/{target}")],
                LONG_GIT_TIMEOUT,
            )
            .await?;
        } else {
            git.run_with(
                "GitVcsDriver.pushCurrentBranch.push",
                &["push"],
                LONG_GIT_TIMEOUT,
            )
            .await?;
        }
        return Ok(GitPushStep {
            status: GitPushStepStatus::Pushed,
            branch: Some(branch.to_owned()),
            upstream_branch: Some(upstream.clone()),
            set_upstream: Some(false),
        });
    }
    let Some(remote) = details.primary_remote else {
        return Err(manager(
            OPERATION,
            cwd,
            "Cannot push because no git remote is configured for this repository.",
        ));
    };
    git.run_with(
        "GitVcsDriver.pushCurrentBranch.pushWithUpstream",
        &["push", "-u", &remote, &format!("HEAD:refs/heads/{branch}")],
        LONG_GIT_TIMEOUT,
    )
    .await?;
    Ok(GitPushStep {
        status: GitPushStepStatus::Pushed,
        branch: Some(branch.to_owned()),
        upstream_branch: Some(format!("{remote}/{branch}")),
        set_upstream: Some(true),
    })
}

// ---------------------------------------------------------------------------
// Pull requests (GitHub, through `gh`)
// ---------------------------------------------------------------------------

/// Whether a remote URL (`git@github.com:o/r.git`, `ssh://git@github.com/o/r`,
/// `https://github.com/o/r`) points at github.com.
fn is_github_remote(url: &str) -> bool {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    // `user:token@host:port` (or scp-style `user@host:path`) before the path.
    let authority = rest.split('/').next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    host.split(':')
        .next()
        .is_some_and(|host| host.eq_ignore_ascii_case("github.com"))
}

/// `gh pr list --json` entry.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPullRequest {
    number: u64,
    title: String,
    url: String,
    base_ref_name: String,
    head_ref_name: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    updated_at: Option<String>,
}

impl GhPullRequest {
    fn into_change_request(self) -> Option<VcsChangeRequest> {
        let state = match self.state.to_ascii_uppercase().as_str() {
            "OPEN" => VcsChangeRequestState::Open,
            "MERGED" => VcsChangeRequestState::Merged,
            "CLOSED" => VcsChangeRequestState::Closed,
            _ => return None,
        };
        Some(VcsChangeRequest {
            number: self.number,
            title: self.title,
            url: self.url,
            base_ref: self.base_ref_name,
            head_ref: self.head_ref_name,
            state,
            is_draft: Some(self.is_draft),
            updated_at: self.updated_at,
        })
    }
}

/// The pull request `gh pr list --json` output names for the branch: the
/// open one, else the most recently updated.
fn pick_pull_request(json: &str) -> Option<VcsChangeRequest> {
    let listed: Vec<GhPullRequest> = serde_json::from_str(json).ok()?;
    listed
        .into_iter()
        .filter_map(GhPullRequest::into_change_request)
        .max_by(|a, b| {
            let open = |pr: &VcsChangeRequest| pr.state == VcsChangeRequestState::Open;
            open(a)
                .cmp(&open(b))
                .then_with(|| a.updated_at.cmp(&b.updated_at))
        })
}

const PR_FIELDS: &str = "number,title,url,baseRefName,headRefName,state,isDraft,updatedAt";

/// The branch's pull request as `gh` reports it now.
async fn find_pull_request(cwd: &str, branch: &str) -> GitResult<Option<VcsChangeRequest>> {
    let gh = Git::gh(Path::new(cwd), cwd);
    let json = gh
        .run_with(
            "GitHubCli.listPullRequests",
            &[
                "pr", "list", "--head", branch, "--state", "all", "--limit", "20", "--json",
                PR_FIELDS,
            ],
            GH_TIMEOUT,
        )
        .await?;
    Ok(pick_pull_request(&json))
}

type PrCache = HashMap<(String, String), (Instant, Option<VcsChangeRequest>)>;

/// Status polls every 30 s; `gh` goes to the network, so its answer is kept
/// for [`PR_CACHE_TTL`]. A missing or logged-out `gh` is no PR, not an error.
static PR_CACHE: LazyLock<Mutex<PrCache>> = LazyLock::new(Mutex::default);

async fn cached_pull_request(cwd: &str, branch: &str) -> Option<VcsChangeRequest> {
    let key = (cwd.to_owned(), branch.to_owned());
    if let Some((at, pr)) = PR_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&key)
        && at.elapsed() < PR_CACHE_TTL
    {
        return pr.clone();
    }
    let pr = find_pull_request(cwd, branch)
        .await
        .inspect_err(|error| {
            tracing::debug!("pull request of {branch} in {cwd}: {}", error.message())
        })
        .ok()
        .flatten();
    PR_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(key, (Instant::now(), pr.clone()));
    pr
}

fn forget_pull_request(cwd: &str, branch: &str) {
    PR_CACHE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&(cwd.to_owned(), branch.to_owned()));
}

/// t3's `runPrStep`: the branch's open pull request, or a new one with
/// omp-written text (`thread_model` as in [`generate_text`]). `branch` is
/// pushed already.
async fn pr_step(
    omp: &OmpLauncher,
    cwd: &str,
    branch: &str,
    details: &Details,
    thread_model: Option<&str>,
    progress: &Progress<'_>,
) -> GitResult<GitPrStep> {
    const OPERATION: &str = "runPrStep";
    if details.status.source_control_provider.is_none() {
        return Err(manager(
            OPERATION,
            cwd,
            "Pull requests need a GitHub remote; this repository has none.",
        ));
    }
    if details.status.is_default_ref {
        return Err(manager(
            OPERATION,
            cwd,
            format!(
                "`{branch}` is the default branch. Switch to a feature branch to open a pull request."
            ),
        ));
    }
    progress.phase_started(GitActionProgressPhase::Pr, "Looking for a pull request...");
    if let Some(existing) = find_pull_request(cwd, branch)
        .await?
        .filter(|pr| pr.state == VcsChangeRequestState::Open)
    {
        forget_pull_request(cwd, branch);
        return Ok(GitPrStep {
            status: GitPrStepStatus::OpenedExisting,
            url: Some(existing.url),
            number: Some(existing.number),
            base_branch: Some(existing.base_ref),
            head_branch: Some(existing.head_ref),
            title: Some(existing.title),
        });
    }

    let git = Git::new(Path::new(cwd), cwd);
    let remote = details.primary_remote.as_deref().unwrap_or("origin");
    let base = base_branch(&git, remote).await?.ok_or_else(|| {
        manager(
            OPERATION,
            cwd,
            "Could not tell which branch the pull request should target.",
        )
    })?;
    let remote_base = format!("{remote}/{base}");
    let base_ref = if git
        .try_line(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/{remote_base}"),
        ])
        .await?
        .is_some()
    {
        remote_base
    } else {
        base.clone()
    };

    let commits = git
        .run(
            "GitHubCli.prContext.commits",
            &[
                "log",
                "--reverse",
                "-n",
                "40",
                "--format=- %s",
                &format!("{base_ref}..HEAD"),
            ],
        )
        .await?;
    if commits.trim().is_empty() {
        return Err(manager(
            OPERATION,
            cwd,
            format!(
                "`{branch}` has no commits that `{base}` lacks, so there is nothing to open a pull request for."
            ),
        ));
    }
    let range = format!("{base_ref}...HEAD");
    let stat = git
        .run("GitHubCli.prContext.stat", &["diff", "--stat", &range])
        .await?;
    let patch = git
        .run(
            "GitHubCli.prContext.patch",
            &["diff", "--no-ext-diff", "--patch", "--minimal", &range],
        )
        .await?;

    progress.phase_started(GitActionProgressPhase::Pr, "Generating PR content...");
    let prompt = pr_prompt(&base, branch, commits.trim(), stat.trim(), &patch);
    let generated = generate_pull_request(omp, &prompt, Path::new(cwd), thread_model).await?;
    let title = generated.title;
    let body = generated.body;

    progress.phase_started(GitActionProgressPhase::Pr, "Creating pull request...");
    let gh = Git::gh(Path::new(cwd), cwd);
    let created = gh
        .run_with(
            "GitHubCli.createPullRequest",
            &[
                "pr", "create", "--base", &base, "--head", branch, "--title", &title, "--body",
                &body,
            ],
            GH_TIMEOUT,
        )
        .await?;
    forget_pull_request(cwd, branch);
    let url = created
        .lines()
        .map(str::trim)
        .rfind(|line| line.starts_with("http"))
        .map(str::to_owned);
    let number = url.as_deref().and_then(pull_request_number);
    Ok(GitPrStep {
        status: GitPrStepStatus::Created,
        url,
        number,
        base_branch: Some(base),
        head_branch: Some(branch.to_owned()),
        title: Some(title),
    })
}

/// `…/pull/123` -> 123.
fn pull_request_number(url: &str) -> Option<u64> {
    let (_, tail) = url.rsplit_once("/pull/")?;
    tail.split(['/', '?', '#']).next()?.parse().ok()
}

/// The branch pull requests target: the remote's HEAD, else `main` or
/// `master` when the remote has it, else what `gh` says is the default.
async fn base_branch(git: &Git<'_>, remote: &str) -> GitResult<Option<String>> {
    let head = git
        .try_line(&[
            "symbolic-ref",
            "--quiet",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ])
        .await?
        .and_then(|head| head.strip_prefix(&format!("{remote}/")).map(str::to_owned));
    if head.is_some() {
        return Ok(head);
    }
    for name in ["main", "master"] {
        let exists = git
            .try_line(&[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/{remote}/{name}"),
            ])
            .await?
            .is_some();
        if exists {
            return Ok(Some(name.to_owned()));
        }
    }
    let gh = Git::gh(git.dir, git.cwd);
    let name = gh
        .try_line(&[
            "repo",
            "view",
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ])
        .await?;
    Ok(name.filter(|name| !name.is_empty()))
}

fn pr_prompt(base: &str, head: &str, commits: &str, stat: &str, patch: &str) -> String {
    [
        PR_SYSTEM_PROMPT,
        "Return a JSON object with keys: title, body.",
        "Rules:",
        "- title must be imperative, <= 72 chars, and no trailing period",
        "- body is markdown: a short summary, then bullet points of the notable changes",
        "- describe only what the commits and diff show",
        "",
        &format!("Base branch: {base}"),
        &format!("Head branch: {head}"),
        "",
        "Commits:",
        &limit_section(commits, 6_000),
        "",
        "Diff stat:",
        &limit_section(stat, 6_000),
        "",
        "Diff:",
        &limit_section(patch, 40_000),
    ]
    .join("\n")
}

#[derive(Debug, PartialEq, Eq)]
struct PullRequestText {
    title: String,
    body: String,
}

#[derive(Deserialize)]
struct GeneratedPullRequest {
    title: String,
    #[serde(default)]
    body: String,
}

async fn generate_pull_request(
    omp: &OmpLauncher,
    prompt: &str,
    cwd: &Path,
    thread_model: Option<&str>,
) -> GitResult<PullRequestText> {
    const OPERATION: &str = "generatePullRequestContent";
    let reply = generate_text(omp, prompt, PR_SYSTEM_PROMPT, cwd, thread_model)
        .await
        .map_err(|error| {
            Box::new(GitManagerServiceError::text_generation(
                OPERATION,
                error.to_string(),
            ))
        })?;
    let generated: GeneratedPullRequest = parse_json(&reply).ok_or_else(|| {
        Box::new(GitManagerServiceError::text_generation(
            OPERATION,
            format!(
                "omp did not return a pull request JSON object: {}",
                reply.trim()
            ),
        ))
    })?;
    Ok(sanitize_pull_request(generated))
}

/// The title is one line without a closing period, at most 72 characters;
/// without one the PR is named after its work.
fn sanitize_pull_request(generated: GeneratedPullRequest) -> PullRequestText {
    let first = generated.title.trim().lines().next().unwrap_or("").trim();
    let title = first.trim_end_matches('.').trim();
    PullRequestText {
        title: if title.is_empty() {
            "Update project files".to_owned()
        } else {
            take_chars(title, 72).trim_end().to_owned()
        },
        body: generated.body.trim().to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Commit messages
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
struct CommitMessage {
    subject: String,
    body: String,
}

/// t3's `parseCustomCommitMessage`: first line is the subject, the rest the
/// body; `None` without a subject.
fn parse_custom_commit_message(raw: &str) -> Option<CommitMessage> {
    let normalized = raw.replace("\r\n", "\n");
    let normalized = normalized.trim();
    let (first, rest) = normalized.split_once('\n').unwrap_or((normalized, ""));
    let subject = first.trim();
    (!subject.is_empty()).then(|| CommitMessage {
        subject: subject.to_owned(),
        body: rest.trim().to_owned(),
    })
}

/// t3's `limitSection`.
fn limit_section(value: &str, max_chars: usize) -> String {
    match value.char_indices().nth(max_chars) {
        None => value.to_owned(),
        Some((end, _)) => format!("{}\n\n[truncated]", &value[..end]),
    }
}

/// t3's `buildCommitMessagePrompt` without a branch name request or style
/// policy.
fn commit_message_prompt(branch: Option<&str>, staged_summary: &str, staged_patch: &str) -> String {
    [
        COMMIT_SYSTEM_PROMPT,
        "Return a JSON object with keys: subject, body.",
        "Rules:",
        "- subject must be imperative, <= 72 chars, and no trailing period",
        "- body can be empty string or short bullet points",
        "- capture the primary user-visible or developer-visible change",
        "",
        &format!("Branch: {}", branch.unwrap_or("(detached)")),
        "",
        "Staged files:",
        &limit_section(staged_summary, 6_000),
        "",
        "Staged patch:",
        &limit_section(staged_patch, 40_000),
    ]
    .join("\n")
}

#[derive(Deserialize)]
struct GeneratedCommitMessage {
    subject: String,
    #[serde(default)]
    body: String,
}

async fn generate_commit_message(
    omp: &OmpLauncher,
    prompt: &str,
    cwd: &Path,
    thread_model: Option<&str>,
) -> GitResult<CommitMessage> {
    const OPERATION: &str = "generateCommitMessage";
    let reply = generate_text(omp, prompt, COMMIT_SYSTEM_PROMPT, cwd, thread_model)
        .await
        .map_err(|error| {
            Box::new(GitManagerServiceError::text_generation(
                OPERATION,
                error.to_string(),
            ))
        })?;
    let generated = parse_generated(&reply).ok_or_else(|| {
        Box::new(GitManagerServiceError::text_generation(
            OPERATION,
            format!(
                "omp did not return a commit message JSON object: {}",
                reply.trim()
            ),
        ))
    })?;
    Ok(sanitize_commit_message(generated))
}

/// Asks omp's `pi/smol`, then `pi/tiny`, then the thread's own model, and
/// answers with the first reply. A model that cannot be reached (omp fails
/// or times out with it) moves on to the next; the last failure is the
/// error.
async fn generate_text(
    omp: &OmpLauncher,
    prompt: &str,
    system: &str,
    cwd: &Path,
    thread_model: Option<&str>,
) -> crate::omp::Result<String> {
    let models = [
        Some(crate::omp::SMOL_MODEL),
        Some(crate::omp::TINY_MODEL),
        thread_model,
    ];
    let mut last = None;
    for model in models.into_iter().flatten() {
        match omp.generate_text(prompt, system, cwd, model).await {
            Ok(reply) => return Ok(reply),
            Err(error) => {
                tracing::warn!(%error, model, "omp could not generate git text");
                last = Some(error);
            }
        }
    }
    Err(last.expect("at least one model is tried"))
}

/// The JSON object in a model reply, tolerating code fences and prose
/// around it.
fn parse_json<T: serde::de::DeserializeOwned>(reply: &str) -> Option<T> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    serde_json::from_str(reply.get(start..=end)?).ok()
}

fn parse_generated(reply: &str) -> Option<GeneratedCommitMessage> {
    parse_json(reply)
}

/// t3's `sanitizeCommitMessage`.
fn sanitize_commit_message(generated: GeneratedCommitMessage) -> CommitMessage {
    let first = generated.subject.trim().lines().next().unwrap_or("").trim();
    let subject = first.trim_end_matches('.').trim();
    let subject = if subject.is_empty() {
        "Update project files".to_owned()
    } else {
        take_chars(subject, 72).trim_end().to_owned()
    };
    CommitMessage {
        subject,
        body: generated.body.trim().to_owned(),
    }
}

fn take_chars(value: &str, max_chars: usize) -> &str {
    match value.char_indices().nth(max_chars) {
        Some((end, _)) => &value[..end],
        None => value,
    }
}

// ---------------------------------------------------------------------------
// Toast
// ---------------------------------------------------------------------------

/// t3's `truncateText`.
fn truncate_text(value: Option<&str>) -> Option<String> {
    let value = value.filter(|value| !value.is_empty())?;
    if value.chars().count() <= TOAST_DESCRIPTION_MAX {
        return Some(value.to_owned());
    }
    Some(format!(
        "{}...",
        take_chars(value, TOAST_DESCRIPTION_MAX - 3).trim_end()
    ))
}

/// t3's `summarizeGitActionResult` plus the CTA of `buildCompletionToast`:
/// "View PR" after a pull request, "Push" after a plain commit, and "Create
/// PR" after a push to a feature branch on GitHub (`offers_pr`).
fn toast(
    action: GitStackedAction,
    commit: &GitCommitStep,
    push: &GitPushStep,
    pr: &GitPrStep,
    offers_pr: bool,
) -> GitToast {
    let short_sha = commit
        .commit_sha
        .as_deref()
        .filter(|sha| !sha.is_empty())
        .map(|sha| take_chars(sha, SHORT_SHA_LENGTH));
    let description = truncate_text(commit.subject.as_deref());
    let pr_number = pr
        .number
        .map_or_else(String::new, |number| format!(" #{number}"));
    let (title, description) = if pr.status == GitPrStepStatus::Created {
        (
            format!("Created PR{pr_number}"),
            truncate_text(pr.title.as_deref()),
        )
    } else if pr.status == GitPrStepStatus::OpenedExisting {
        (
            format!("Opened PR{pr_number}"),
            truncate_text(pr.title.as_deref()),
        )
    } else if push.status == GitPushStepStatus::Pushed {
        let mut title = String::from("Pushed");
        if let Some(sha) = short_sha {
            title.push(' ');
            title.push_str(sha);
        }
        if let Some(branch) = push.upstream_branch.as_ref().or(push.branch.as_ref()) {
            title.push_str(" to ");
            title.push_str(branch);
        }
        (title, description)
    } else if commit.status == GitCommitStepStatus::Created {
        let title = match short_sha {
            Some(sha) => format!("Committed {sha}"),
            None => "Committed changes".to_owned(),
        };
        (title, description)
    } else {
        ("Done".to_owned(), None)
    };
    let run = |label: &str, kind: GitStackedAction| GitToastCta::RunAction {
        label: label.to_owned(),
        action: GitToastRunAction { kind },
    };
    let cta = match (&pr.url, action) {
        (Some(url), _) => GitToastCta::OpenPr {
            label: "View PR".to_owned(),
            url: url.clone(),
        },
        (None, GitStackedAction::Commit) if commit.status == GitCommitStepStatus::Created => {
            run("Push", GitStackedAction::Push)
        }
        (None, _) if offers_pr => run("Create PR", GitStackedAction::CreatePr),
        _ => GitToastCta::None,
    };
    GitToast {
        title,
        description,
        cta,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_headers_and_entries() {
        let output = "# branch.oid abc\0# branch.head main\0# branch.upstream origin/main\0\
                      # branch.ab +2 -1\0\
                      1 .M N... 100644 100644 100644 aaa bbb src/a b.rs\0\
                      u UU N... 100644 100644 100644 100644 a b c conflict.txt\0\
                      ? new.txt\0! ignored\0";
        let parsed = parse_porcelain(output);
        assert_eq!(parsed.upstream.as_deref(), Some("origin/main"));
        assert_eq!(parsed.ahead_behind, (2, 1));
        let paths: Vec<_> = parsed
            .entries
            .iter()
            .map(|e| (e.path.as_str(), e.untracked))
            .collect();
        assert_eq!(
            paths,
            [
                ("src/a b.rs", false),
                ("conflict.txt", false),
                ("new.txt", true)
            ]
        );
    }

    #[test]
    fn numstat_counts_binary_as_zero() {
        let counts = parse_numstat("3\t1\ta.rs\0-\t-\timg.png\0");
        assert_eq!(counts["a.rs"], (3, 1));
        assert_eq!(counts["img.png"], (0, 0));
    }

    #[test]
    fn custom_messages_split_subject_and_body() {
        assert_eq!(
            parse_custom_commit_message("  Fix it\r\n\r\n- detail\n"),
            Some(CommitMessage {
                subject: "Fix it".into(),
                body: "- detail".into()
            })
        );
        assert_eq!(parse_custom_commit_message(" \n "), None);
    }

    #[test]
    fn generated_messages_are_sanitized() {
        let generated = parse_generated(
            "```json\n{\"subject\": \"Add things...\\nmore\", \"body\": \" b \"}\n```",
        )
        .unwrap();
        assert_eq!(
            sanitize_commit_message(generated),
            CommitMessage {
                subject: "Add things".into(),
                body: "b".into()
            }
        );
        let empty = sanitize_commit_message(GeneratedCommitMessage {
            subject: "...".into(),
            body: String::new(),
        });
        assert_eq!(empty.subject, "Update project files");
        assert!(parse_generated("no json").is_none());
    }

    #[test]
    fn sections_truncate_with_a_marker() {
        assert_eq!(limit_section("abc", 3), "abc");
        assert_eq!(limit_section("abcd", 3), "abc\n\n[truncated]");
    }

    #[test]
    fn toast_descriptions_truncate() {
        let long = "x".repeat(80);
        assert_eq!(
            truncate_text(Some(&long)).unwrap(),
            format!("{}...", "x".repeat(69))
        );
        assert_eq!(truncate_text(Some("")), None);
    }

    #[test]
    fn github_remotes_are_recognised_by_host() {
        for url in [
            "git@github.com:acme/widgets.git",
            "ssh://git@github.com/acme/widgets",
            "https://github.com/acme/widgets.git",
            "https://user:token@GitHub.com/acme/widgets",
            "github.com:acme/widgets.git",
        ] {
            assert!(is_github_remote(url), "{url}");
        }
        for url in [
            "git@gitlab.com:acme/widgets.git",
            "https://example.com/github.com/acme/widgets",
            "https://github.com.evil.test/acme/widgets",
            "/srv/git/widgets.git",
            "",
        ] {
            assert!(!is_github_remote(url), "{url}");
        }
    }

    #[test]
    fn pull_request_numbers_come_from_urls() {
        assert_eq!(
            pull_request_number("https://github.com/acme/widgets/pull/12"),
            Some(12)
        );
        assert_eq!(
            pull_request_number("https://github.com/acme/widgets/pull/12/files?w=1"),
            Some(12)
        );
        assert_eq!(pull_request_number("https://github.com/acme/widgets"), None);
    }

    #[test]
    fn the_open_pull_request_beats_newer_closed_ones() {
        let listed = r#"[
          {"number": 1, "title": "a", "url": "u1", "baseRefName": "main", "headRefName": "f",
           "state": "MERGED", "isDraft": false, "updatedAt": "2026-10-02T00:00:00Z"},
          {"number": 2, "title": "b", "url": "u2", "baseRefName": "main", "headRefName": "f",
           "state": "OPEN", "isDraft": false, "updatedAt": "2026-10-01T00:00:00Z"},
          {"number": 3, "title": "c", "url": "u3", "baseRefName": "main", "headRefName": "f",
           "state": "CLOSED", "isDraft": false, "updatedAt": "2026-09-01T00:00:00Z"}
        ]"#;
        assert_eq!(pick_pull_request(listed).unwrap().number, 2);
        let closed_only = listed.replace("OPEN", "CLOSED");
        let picked = pick_pull_request(&closed_only).unwrap();
        assert_eq!(
            (picked.number, picked.state),
            (1, VcsChangeRequestState::Merged)
        );
        assert_eq!(pick_pull_request("[]"), None);
        assert_eq!(pick_pull_request("not json"), None);
    }

    #[test]
    fn generated_pull_request_text_is_sanitized() {
        let generated: GeneratedPullRequest =
            parse_json("```json\n{\"title\": \"Add widgets.\\nmore\", \"body\": \" - a \"}\n```")
                .unwrap();
        assert_eq!(
            sanitize_pull_request(generated),
            PullRequestText {
                title: "Add widgets".into(),
                body: "- a".into()
            }
        );
        let empty = sanitize_pull_request(GeneratedPullRequest {
            title: "".into(),
            body: String::new(),
        });
        assert_eq!(empty.title, "Update project files");
    }
}
