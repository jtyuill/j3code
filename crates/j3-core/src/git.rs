//! t3's git workflow over the `git` CLI (`vcs.refreshStatus`, `vcs.pull`,
//! `git.runStackedAction`): status, fast-forward pull, and the commit / push
//! stacked actions with omp-generated commit messages. Pull requests and
//! feature branches are not served.
//!
//! Mirrors t3's `GitManager` (`apps/server/src/git/GitManager.ts`) and
//! `GitVcsDriverCore` (`apps/server/src/vcs/GitVcsDriverCore.ts`).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{Mutex, PoisonError},
    time::Duration,
};

use futures_util::stream::{self, BoxStream, StreamExt};
use j3_protocol::t3::git::{
    GitActionProgress, GitActionProgressEvent, GitActionProgressPhase, GitBranchStep,
    GitBranchStepStatus, GitCommitStep, GitCommitStepStatus, GitManagerServiceError, GitPrStep,
    GitPrStepStatus, GitPushStep, GitPushStepStatus, GitRunStackedActionInput,
    GitRunStackedActionResult, GitStackedAction, GitToast, GitToastCta, GitToastRunAction,
    VcsPullResult, VcsPullStatus, VcsStatusResult, VcsWorkingTree, VcsWorkingTreeFile,
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

// ---------------------------------------------------------------------------
// Running git
// ---------------------------------------------------------------------------

/// `git` in one directory. `cwd` is the client's string, used in errors.
struct Git<'a> {
    dir: &'a Path,
    cwd: &'a str,
}

impl<'a> Git<'a> {
    fn new(dir: &'a Path, cwd: &'a str) -> Self {
        Self { dir, cwd }
    }

    /// Runs `git <args>` and returns its output whatever the exit status.
    async fn output(&self, operation: &str, args: &[&str], timeout: Duration) -> GitResult<Output> {
        let mut command = tokio::process::Command::new("git");
        command
            .args(args)
            .current_dir(self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(timeout, command.output()).await {
            Ok(Ok(output)) => Ok(output),
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
            .unwrap_or_else(|| format!("{} failed", command_line(args)));
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
            command_line(args),
            self.cwd,
            exit_code,
            detail,
        ))
    }
}

fn command_line(args: &[&str]) -> String {
    let mut line = String::from("git");
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

/// `vcs.refreshStatus`: the working tree and branch state of `cwd`.
pub async fn status(cwd: &str) -> GitResult<VcsStatusResult> {
    Ok(details(cwd).await?.status)
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
            source_control_provider: None,
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
    if matches!(
        input.action,
        GitStackedAction::CreatePr | GitStackedAction::CommitPushPr
    ) {
        return Err(manager(
            OPERATION,
            cwd,
            "Pull requests are not supported by this server.",
        ));
    }
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
    let wants_push = input.action.pushes();
    let phases = [
        wants_commit.then_some(GitActionProgressPhase::Commit),
        wants_push.then_some(GitActionProgressPhase::Push),
    ]
    .into_iter()
    .flatten()
    .collect();
    progress.emit(GitActionProgress::ActionStarted { phases });

    let branch = initial.status.ref_name.clone();
    if wants_push && branch.is_none() {
        return Err(manager(OPERATION, cwd, "Cannot push from detached HEAD."));
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

    let toast = toast(input.action, &commit, &push);
    let result = GitRunStackedActionResult {
        action: input.action,
        branch: GitBranchStep {
            status: GitBranchStepStatus::SkippedNotRequested,
            name: None,
        },
        commit,
        push,
        pr: GitPrStep {
            status: GitPrStepStatus::SkippedNotRequested,
            url: None,
            number: None,
            base_branch: None,
            head_branch: None,
            title: None,
        },
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
            generate_commit_message(omp, &prompt, Path::new(cwd)).await?
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
) -> GitResult<CommitMessage> {
    const OPERATION: &str = "generateCommitMessage";
    let reply = omp
        .generate_text(prompt, COMMIT_SYSTEM_PROMPT, cwd)
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

/// The JSON object in a model reply, tolerating code fences and prose
/// around it.
fn parse_generated(reply: &str) -> Option<GeneratedCommitMessage> {
    let start = reply.find('{')?;
    let end = reply.rfind('}')?;
    serde_json::from_str(reply.get(start..=end)?).ok()
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

/// t3's `summarizeGitActionResult` plus the CTA of `buildCompletionToast`
/// (no pull requests: the only CTA is "Push" after a plain commit).
fn toast(action: GitStackedAction, commit: &GitCommitStep, push: &GitPushStep) -> GitToast {
    let short_sha = commit
        .commit_sha
        .as_deref()
        .filter(|sha| !sha.is_empty())
        .map(|sha| take_chars(sha, SHORT_SHA_LENGTH));
    let description = truncate_text(commit.subject.as_deref());
    let (title, description) = if push.status == GitPushStepStatus::Pushed {
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
    let cta = if action == GitStackedAction::Commit && commit.status == GitCommitStepStatus::Created
    {
        GitToastCta::RunAction {
            label: "Push".to_owned(),
            action: GitToastRunAction {
                kind: GitStackedAction::Push,
            },
        }
    } else {
        GitToastCta::None
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
}
