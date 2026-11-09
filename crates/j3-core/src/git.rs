//! The git workflow of wire.md §5.10 (`git.status`, `git.pull`, `git.run`)
//! over the host's `git` and `gh` CLIs: status, fast-forward pull, and the
//! commit / push / pull request actions. Commits without a message from the
//! client go through `omp commit`, so omp writes them the way its CLI does;
//! pull request text comes from omp's `commit` model role. Credentials,
//! identity and `gh` login are the host user's own (`~/.gitconfig`,
//! `~/.config/gh`), nothing is configured here. Pull requests need a GitHub
//! remote.
//!
//! Errors: j3 refusing an action is `rejected`, with a message for the user;
//! git, `gh` or omp failing is `unavailable`, with the tool's own message.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{LazyLock, Mutex, PoisonError},
    time::{Duration, Instant},
};

use futures_util::stream::{self, BoxStream, StreamExt};
use j3_protocol::v2::{
    envelope::{Error, ErrorCode},
    methods::{
        GitAction, GitFile, GitNext, GitPhase, GitRunItem, GitRunResult, GitStatus, PullRequest,
        PullRequestState, PullResult, PullStatus,
    },
};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::omp::OmpLauncher;

/// Default timeout of one git command.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
/// `git commit` (hooks), `git push` and `git pull` (network).
const LONG_GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// The tree of a repository without commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// Untracked files larger than this count no lines.
const MAX_COUNTED_FILE_BYTES: u64 = 1024 * 1024;
/// Length of the commit hashes in result titles.
const SHORT_SHA_LENGTH: usize = 7;
/// Longest result description; longer ones end in `...`.
const DESCRIPTION_MAX: usize = 72;
/// The system prompt of pull request text generation.
const PR_SYSTEM_PROMPT: &str = "You write GitHub pull request titles and descriptions.";
/// The models pull request text is asked of, in order: omp's `commit`, `smol`
/// and `default` roles, the order `omp commit` resolves its own model in. An
/// unset role makes omp fail at once, which moves on to the next.
const PR_MODELS: [&str; 3] = [
    crate::omp::COMMIT_MODEL,
    crate::omp::SMOL_MODEL,
    crate::omp::DEFAULT_MODEL,
];
/// How long a branch's pull request is remembered between status polls.
const PR_CACHE_TTL: Duration = Duration::from_secs(60);
/// `gh` talks to the network; `gh pr create` also runs `git push` hooks.
const GH_TIMEOUT: Duration = Duration::from_secs(60);

fn unavailable(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::Unavailable, message)
}

// ---------------------------------------------------------------------------
// Running git
// ---------------------------------------------------------------------------

/// `git` (or `gh`) in one directory. Both run with the server's environment,
/// so they use the host user's git config, ssh keys and `gh` login; only
/// prompts are switched off, because nobody is there to answer them.
struct Git<'a> {
    program: &'static str,
    dir: &'a Path,
}

impl<'a> Git<'a> {
    fn new(dir: &'a Path) -> Self {
        Self {
            program: "git",
            dir,
        }
    }

    /// The GitHub CLI, run in `dir` like [`Git::new`].
    fn gh(dir: &'a Path) -> Self {
        Self { program: "gh", dir }
    }

    /// Runs `<program> <args>` and returns its output whatever the exit
    /// status.
    async fn output(&self, args: &[&str], timeout: Duration) -> Result<Output, Error> {
        let mut command = tokio::process::Command::new(self.program);
        command
            .args(args)
            .current_dir(self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            // Status reads would otherwise refresh the index under `index.lock`; a read
            // killed mid-way (timeout, or a dropped request: `kill_on_drop` is SIGKILL)
            // leaves that lock behind, and every later commit in the repo fails.
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_PAGER", "cat")
            .env("NO_COLOR", "1")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(timeout, command.output()).await {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => Err(unavailable(
                format!("`{}` is not installed on this host.", self.program),
            )),
            Ok(Err(error)) => Err(unavailable(format!(
                "`{}`: {error}",
                command_line(self.program, args)
            ))),
            Err(_) => Err(unavailable(format!(
                "`{}` timed out after {}s.",
                command_line(self.program, args),
                timeout.as_secs()
            ))),
        }
    }

    /// Runs `<program> <args>`; a non-zero exit is an error with its stderr
    /// (else stdout).
    async fn run_with(&self, args: &[&str], timeout: Duration) -> Result<String, Error> {
        let output = self.output(args, timeout).await?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = [stderr.trim(), stdout.trim()]
            .into_iter()
            .find(|text| !text.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("`{}` failed.", command_line(self.program, args)));
        Err(unavailable(detail))
    }

    async fn run(&self, args: &[&str]) -> Result<String, Error> {
        self.run_with(args, GIT_TIMEOUT).await
    }

    /// Trimmed stdout when `<program> <args>` succeeds, `None` otherwise.
    async fn try_line(&self, args: &[&str]) -> Result<Option<String>, Error> {
        let output = self.output(args, GIT_TIMEOUT).await?;
        Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned()))
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

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

/// What the actions need beyond [`GitStatus`].
struct Details {
    status: GitStatus,
    /// `branch.upstream` of `git status`, e.g. `origin/main`.
    upstream: Option<String>,
    /// `origin` when present, else the first remote.
    primary_remote: Option<String>,
}

/// `git.status`: the working tree and branch state of `cwd`, and on a GitHub
/// remote the branch's pull request.
pub async fn status(cwd: &str) -> Result<GitStatus, Error> {
    let mut status = details(cwd).await?.status;
    // A pushed feature branch may have a PR; the default branch and an
    // unpublished branch cannot, so `gh` is not asked.
    if status.github
        && status.has_upstream
        && !status.on_default_branch
        && let Some(branch) = &status.branch
    {
        status.pull_request = cached_pull_request(cwd, branch).await;
    }
    Ok(status)
}

async fn details(cwd: &str) -> Result<Details, Error> {
    let not_a_repo = || Details {
        status: GitStatus {
            is_repo: false,
            branch: None,
            on_default_branch: false,
            has_remote: false,
            github: false,
            has_upstream: false,
            ahead: 0,
            behind: 0,
            dirty: false,
            files: Vec::new(),
            additions: 0,
            deletions: 0,
            pull_request: None,
        },
        upstream: None,
        primary_remote: None,
    };
    let dir = Path::new(cwd);
    if !dir.is_dir() {
        return Ok(not_a_repo());
    }
    let Some(top) = Git::new(dir)
        .try_line(&["rev-parse", "--show-toplevel"])
        .await?
        .filter(|top| !top.is_empty())
    else {
        return Ok(not_a_repo());
    };
    // Paths of `status` and `diff` are relative to the top level.
    let top = PathBuf::from(top);
    let git = Git::new(&top);

    let branch = git
        .try_line(&["symbolic-ref", "--quiet", "--short", "HEAD"])
        .await?;
    let has_commits = git
        .try_line(&["rev-parse", "--verify", "--quiet", "HEAD"])
        .await?
        .is_some();

    let porcelain = git
        .run(&[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=all",
            "--no-renames",
            "-z",
        ])
        .await?;
    let parsed = parse_porcelain(&porcelain);

    let base = if has_commits { "HEAD" } else { EMPTY_TREE };
    let numstat = git
        .run(&[
            "-c",
            "core.quotePath=false",
            "diff",
            base,
            "--numstat",
            "--no-renames",
            "-z",
        ])
        .await?;
    let counts = parse_numstat(&numstat);

    let (mut files, mut additions, mut deletions) = (Vec::new(), 0, 0);
    for entry in parsed.entries {
        let (added, deleted) = if entry.untracked {
            (count_lines(&top.join(&entry.path)).await, 0)
        } else {
            counts.get(entry.path.as_str()).copied().unwrap_or((0, 0))
        };
        additions += added;
        deletions += deleted;
        files.push(GitFile {
            path: entry.path,
            additions: added,
            deletions: deleted,
        });
    }

    let remotes = git.run(&["remote"]).await?;
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

    let github = match &primary_remote {
        // The URL as configured, before `url.*.insteadOf` rewrites it.
        Some(remote) => git
            .try_line(&["config", "--get", &format!("remote.{remote}.url")])
            .await?
            .is_some_and(|url| is_github_remote(&url)),
        None => false,
    };

    let on_default_branch = match &branch {
        None => false,
        Some(branch) => {
            let remote_head = match &primary_remote {
                Some(remote) => remote_head(&git, remote).await?,
                None => None,
            };
            match remote_head {
                Some(head) => *branch == head,
                None => branch == "main" || branch == "master",
            }
        }
    };

    let has_upstream = parsed.upstream.is_some();
    let (ahead, behind) = if has_upstream {
        parsed.ahead_behind
    } else if primary_remote.is_some() && has_commits {
        let count = git
            .run(&["rev-list", "--count", "HEAD", "--not", "--remotes"])
            .await?;
        (count.trim().parse().unwrap_or(0), 0)
    } else {
        (0, 0)
    };

    Ok(Details {
        status: GitStatus {
            is_repo: true,
            branch,
            on_default_branch,
            has_remote: primary_remote.is_some(),
            github,
            has_upstream,
            ahead,
            behind,
            dirty: !files.is_empty(),
            files,
            additions,
            deletions,
            pull_request: None,
        },
        upstream: parsed.upstream,
        primary_remote,
    })
}

/// The branch `refs/remotes/<remote>/HEAD` points at.
async fn remote_head(git: &Git<'_>, remote: &str) -> Result<Option<String>, Error> {
    Ok(git
        .try_line(&[
            "symbolic-ref",
            "--quiet",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ])
        .await?
        .and_then(|head| head.strip_prefix(&format!("{remote}/")).map(str::to_owned)))
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

/// Parses `git diff --numstat -z --no-renames`: path -> (additions,
/// deletions); binary files (`-`) count 0.
fn parse_numstat(output: &str) -> HashMap<&str, (u64, u64)> {
    output
        .split('\0')
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').splitn(3, '\t');
            let additions = fields.next()?.parse().unwrap_or(0);
            let deletions = fields.next()?.parse().unwrap_or(0);
            Some((fields.next()?, (additions, deletions)))
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

/// `git.pull`: `git pull --ff-only` on a branch with an upstream.
pub async fn pull(cwd: &str) -> Result<PullResult, Error> {
    let details = details(cwd).await?;
    if !details.status.is_repo {
        return Err(Error::rejected("Not a git repository."));
    }
    let Some(branch) = details.status.branch else {
        return Err(Error::rejected("Cannot pull from detached HEAD."));
    };
    if details.upstream.is_none() {
        return Err(Error::rejected(
            "Current branch has no upstream configured. Push with upstream first.",
        ));
    }
    let git = Git::new(Path::new(cwd));
    let before = git.run(&["rev-parse", "HEAD"]).await?;
    git.run_with(&["pull", "--ff-only"], LONG_GIT_TIMEOUT)
        .await?;
    let after = git.run(&["rev-parse", "HEAD"]).await?;
    Ok(PullResult {
        status: if before.trim() == after.trim() {
            PullStatus::UpToDate
        } else {
            PullStatus::Pulled
        },
        branch,
        upstream: details.upstream,
    })
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

/// `git.run` (wire.md §5.10): `Phase` items as each phase starts, then one
/// `Finished`. A failure is the stream's last element, `Err(error)` with
/// `data: {"phase": <phase>}` when a phase was running. The action runs on
/// its own task, so a client that goes away does not interrupt a commit or
/// push half way.
pub fn run(
    omp: OmpLauncher,
    cwd: String,
    action: GitAction,
    message: Option<String>,
) -> BoxStream<'static, Result<GitRunItem, Error>> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let progress = Progress {
            tx,
            phases: Mutex::default(),
        };
        let item = match run_action(&omp, &cwd, action, message.as_deref(), &progress).await {
            Ok(result) => Ok(GitRunItem::Finished { result }),
            Err(error) => Err(match progress.phases().current {
                Some(phase) => error.with_data(serde_json::json!({ "phase": phase })),
                None => error,
            }),
        };
        // The client may be gone; the action still completed.
        let _ = progress.tx.send(item);
    });
    stream::poll_fn(move |cx| rx.poll_recv(cx)).boxed()
}

#[derive(Clone, Copy, Default)]
struct Phases {
    /// The phase running now; a failure names it.
    current: Option<GitPhase>,
    /// The last phase reported to the client.
    reported: Option<GitPhase>,
}

/// Reports the phases of one action and tracks the current one.
struct Progress {
    tx: mpsc::UnboundedSender<Result<GitRunItem, Error>>,
    phases: Mutex<Phases>,
}

impl Progress {
    fn phases(&self) -> std::sync::MutexGuard<'_, Phases> {
        self.phases.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `phase` runs now, without telling the client yet.
    fn set_phase(&self, phase: GitPhase) {
        self.phases().current = Some(phase);
    }

    /// `phase` runs now; the client hears of it once.
    fn start(&self, phase: GitPhase) {
        let mut phases = self.phases();
        phases.current = Some(phase);
        if phases.reported != Some(phase) {
            phases.reported = Some(phase);
            // The client may be gone; the action still completes.
            let _ = self.tx.send(Ok(GitRunItem::Phase { phase }));
        }
    }
}

/// A commit the action made.
struct Commit {
    sha: String,
    subject: String,
}

/// A push the action did.
enum Push {
    UpToDate,
    Pushed { upstream: String },
}

/// The pull request the action opened or found.
struct Pr {
    created: bool,
    url: Option<String>,
    number: Option<u64>,
    title: String,
}

async fn run_action(
    omp: &OmpLauncher,
    cwd: &str,
    action: GitAction,
    message: Option<&str>,
    progress: &Progress,
) -> Result<GitRunResult, Error> {
    let initial = details(cwd).await?;
    if !initial.status.is_repo {
        return Err(Error::rejected("Not a git repository."));
    }
    let wants_commit = matches!(
        action,
        GitAction::Commit | GitAction::CommitPush | GitAction::CommitPushPr
    );
    let wants_pr = matches!(action, GitAction::Pr | GitAction::CommitPushPr);
    // A pull request needs its branch on the remote; `push_step` does
    // nothing for a branch that is there already.
    let wants_push = wants_pr || matches!(action, GitAction::Push | GitAction::CommitPush);

    let branch = initial.status.branch.clone();
    if wants_push && branch.is_none() {
        return Err(Error::rejected("Cannot push from detached HEAD."));
    }
    if wants_pr && !initial.status.github {
        return Err(Error::rejected(
            "Pull requests need a GitHub remote; this repository has none.",
        ));
    }

    let commit = if wants_commit {
        progress.set_phase(GitPhase::Commit);
        commit_step(omp, cwd, message, progress).await?
    } else {
        None
    };

    let push = match (&branch, wants_push) {
        (Some(branch), true) => {
            progress.start(GitPhase::Push);
            Some(push_step(cwd, branch).await?)
        }
        _ => None,
    };

    let pr = match (&branch, wants_pr) {
        (Some(branch), true) => {
            progress.set_phase(GitPhase::Pr);
            // The push moved the branch; read the state it left.
            let current = details(cwd).await?;
            Some(pr_step(omp, cwd, branch, &current, progress).await?)
        }
        _ => None,
    };

    // A pushed feature branch without a pull request gets one offered.
    let pushed = matches!(push, Some(Push::Pushed { .. }));
    let offers_pr =
        pr.is_none() && pushed && initial.status.github && !initial.status.on_default_branch;
    Ok(result(
        action,
        commit.as_ref(),
        push.as_ref(),
        pr.as_ref(),
        offers_pr,
    ))
}

/// Stages everything, then commits with the client's message, or through
/// `omp commit`, which writes the message and may split the staged changes
/// into several commits and update changelogs on the way. The step reports
/// the last commit made; `None` when there was nothing to commit.
async fn commit_step(
    omp: &OmpLauncher,
    cwd: &str,
    message: Option<&str>,
    progress: &Progress,
) -> Result<Option<Commit>, Error> {
    let git = Git::new(Path::new(cwd));
    let custom = message.and_then(parse_custom_commit_message);
    if custom.is_none() {
        progress.start(GitPhase::Commit);
    }

    git.run(&["add", "-A"]).await?;
    let staged = git.run(&["diff", "--cached", "--name-only"]).await?;
    if staged.trim().is_empty() {
        return Ok(None);
    }

    let before = git
        .try_line(&["rev-parse", "--verify", "--quiet", "HEAD"])
        .await?;
    match custom {
        Some(message) => {
            progress.start(GitPhase::Commit);
            let mut args = vec!["commit", "-m", message.subject.as_str()];
            if !message.body.is_empty() {
                args.extend(["-m", message.body.as_str()]);
            }
            git.run_with(&args, LONG_GIT_TIMEOUT).await?;
        }
        None => omp
            .commit(Path::new(cwd), |_| {})
            .await
            .map_err(|error| unavailable(error.to_string()))?,
    }
    let head = git.run(&["log", "-1", "--format=%H%x00%s"]).await?;
    let (sha, subject) = head
        .trim_end()
        .split_once('\0')
        .unwrap_or((head.trim(), ""));
    if before.as_deref() == Some(sha) {
        return Err(unavailable("omp commit made no commit."));
    }
    Ok(Some(Commit {
        sha: sha.to_owned(),
        subject: subject.to_owned(),
    }))
}

/// Pushes the current branch to its upstream, or to the primary remote
/// setting the upstream.
async fn push_step(cwd: &str, branch: &str) -> Result<Push, Error> {
    let details = details(cwd).await?;
    let git = Git::new(Path::new(cwd));
    if let Some(upstream) = details.upstream {
        if details.status.ahead == 0 {
            return Ok(Push::UpToDate);
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
                &["push", &remote, &format!("HEAD:refs/heads/{target}")],
                LONG_GIT_TIMEOUT,
            )
            .await?;
        } else {
            git.run_with(&["push"], LONG_GIT_TIMEOUT).await?;
        }
        return Ok(Push::Pushed { upstream });
    }
    let Some(remote) = details.primary_remote else {
        return Err(Error::rejected(
            "Cannot push because no git remote is configured for this repository.",
        ));
    };
    git.run_with(
        &["push", "-u", &remote, &format!("HEAD:refs/heads/{branch}")],
        LONG_GIT_TIMEOUT,
    )
    .await?;
    Ok(Push::Pushed {
        upstream: format!("{remote}/{branch}"),
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
    state: String,
    #[serde(default)]
    updated_at: Option<String>,
}

/// A listed pull request with a known state.
#[derive(Debug, PartialEq, Eq)]
struct ListedPullRequest {
    number: u64,
    title: String,
    url: String,
    state: PullRequestState,
    updated_at: Option<String>,
}

impl GhPullRequest {
    fn into_listed(self) -> Option<ListedPullRequest> {
        let state = match self.state.to_ascii_uppercase().as_str() {
            "OPEN" => PullRequestState::Open,
            "MERGED" => PullRequestState::Merged,
            "CLOSED" => PullRequestState::Closed,
            _ => return None,
        };
        Some(ListedPullRequest {
            number: self.number,
            title: self.title,
            url: self.url,
            state,
            updated_at: self.updated_at,
        })
    }
}

/// The pull request `gh pr list --json` output names for the branch: the
/// open one, else the most recently updated.
fn pick_pull_request(json: &str) -> Option<ListedPullRequest> {
    let listed: Vec<GhPullRequest> = serde_json::from_str(json).ok()?;
    listed
        .into_iter()
        .filter_map(GhPullRequest::into_listed)
        .max_by(|a, b| {
            let open = |pr: &ListedPullRequest| pr.state == PullRequestState::Open;
            open(a)
                .cmp(&open(b))
                .then_with(|| a.updated_at.cmp(&b.updated_at))
        })
}

const PR_FIELDS: &str = "number,title,url,state,updatedAt";

/// The branch's pull request as `gh` reports it now.
async fn find_pull_request(cwd: &str, branch: &str) -> Result<Option<ListedPullRequest>, Error> {
    let json = Git::gh(Path::new(cwd))
        .run_with(
            &[
                "pr", "list", "--head", branch, "--state", "all", "--limit", "20", "--json",
                PR_FIELDS,
            ],
            GH_TIMEOUT,
        )
        .await?;
    Ok(pick_pull_request(&json))
}

type PrCache = HashMap<(String, String), (Instant, Option<PullRequest>)>;

/// Status polls every 30 s; `gh` goes to the network, so its answer is kept
/// for [`PR_CACHE_TTL`]. A missing or logged-out `gh` is no PR, not an error.
static PR_CACHE: LazyLock<Mutex<PrCache>> = LazyLock::new(Mutex::default);

async fn cached_pull_request(cwd: &str, branch: &str) -> Option<PullRequest> {
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
            tracing::debug!("pull request of {branch} in {cwd}: {}", error.message)
        })
        .ok()
        .flatten()
        .map(|pr| PullRequest {
            url: pr.url,
            state: pr.state,
        });
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

/// The branch's open pull request, or a new one with omp-written text (see
/// [`PR_MODELS`]). `branch` is pushed already.
async fn pr_step(
    omp: &OmpLauncher,
    cwd: &str,
    branch: &str,
    details: &Details,
    progress: &Progress,
) -> Result<Pr, Error> {
    if !details.status.github {
        return Err(Error::rejected(
            "Pull requests need a GitHub remote; this repository has none.",
        ));
    }
    if details.status.on_default_branch {
        return Err(Error::rejected(format!(
            "`{branch}` is the default branch. Switch to a feature branch to open a pull request."
        )));
    }
    progress.start(GitPhase::Pr);
    if let Some(existing) = find_pull_request(cwd, branch)
        .await?
        .filter(|pr| pr.state == PullRequestState::Open)
    {
        forget_pull_request(cwd, branch);
        return Ok(Pr {
            created: false,
            url: Some(existing.url),
            number: Some(existing.number),
            title: existing.title,
        });
    }

    let git = Git::new(Path::new(cwd));
    let remote = details.primary_remote.as_deref().unwrap_or("origin");
    let base = base_branch(&git, remote).await?.ok_or_else(|| {
        Error::rejected("Could not tell which branch the pull request should target.")
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
        .run(&[
            "log",
            "--reverse",
            "-n",
            "40",
            "--format=- %s",
            &format!("{base_ref}..HEAD"),
        ])
        .await?;
    if commits.trim().is_empty() {
        return Err(Error::rejected(format!(
            "`{branch}` has no commits that `{base}` lacks, so there is nothing to open a pull request for."
        )));
    }
    let range = format!("{base_ref}...HEAD");
    let stat = git.run(&["diff", "--stat", &range]).await?;
    let patch = git
        .run(&["diff", "--no-ext-diff", "--patch", "--minimal", &range])
        .await?;

    let prompt = pr_prompt(&base, branch, commits.trim(), stat.trim(), &patch);
    let generated = generate_pull_request(omp, &prompt, Path::new(cwd)).await?;

    let created = Git::gh(Path::new(cwd))
        .run_with(
            &[
                "pr",
                "create",
                "--base",
                &base,
                "--head",
                branch,
                "--title",
                &generated.title,
                "--body",
                &generated.body,
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
    Ok(Pr {
        created: true,
        url,
        number,
        title: generated.title,
    })
}

/// `…/pull/123` -> 123.
fn pull_request_number(url: &str) -> Option<u64> {
    let (_, tail) = url.rsplit_once("/pull/")?;
    tail.split(['/', '?', '#']).next()?.parse().ok()
}

/// The branch pull requests target: the remote's HEAD, else `main` or
/// `master` when the remote has it, else what `gh` says is the default.
async fn base_branch(git: &Git<'_>, remote: &str) -> Result<Option<String>, Error> {
    let head = remote_head(git, remote).await?;
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
    let name = Git::gh(git.dir)
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
) -> Result<PullRequestText, Error> {
    let reply = generate_text(omp, prompt, PR_SYSTEM_PROMPT, cwd)
        .await
        .map_err(|error| unavailable(error.to_string()))?;
    let generated: GeneratedPullRequest = parse_json(&reply).ok_or_else(|| {
        unavailable(format!(
            "omp did not return a pull request JSON object: {}",
            reply.trim()
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

/// A commit message the client typed.
#[derive(Debug, PartialEq, Eq)]
struct CommitMessage {
    subject: String,
    body: String,
}

/// First line is the subject, the rest the body; `None` without a subject.
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

/// `value` cut to `max_chars`, marked when cut.
fn limit_section(value: &str, max_chars: usize) -> String {
    match value.char_indices().nth(max_chars) {
        None => value.to_owned(),
        Some((end, _)) => format!("{}\n\n[truncated]", &value[..end]),
    }
}

/// Asks each of [`PR_MODELS`] in turn and answers with the first reply. A
/// model that cannot be reached (omp fails or times out with it) moves on to
/// the next; the last failure is the error.
async fn generate_text(
    omp: &OmpLauncher,
    prompt: &str,
    system: &str,
    cwd: &Path,
) -> crate::omp::Result<String> {
    let mut last = None;
    for model in PR_MODELS {
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

fn take_chars(value: &str, max_chars: usize) -> &str {
    match value.char_indices().nth(max_chars) {
        Some((end, _)) => &value[..end],
        None => value,
    }
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

/// A description of at most [`DESCRIPTION_MAX`] characters; none for an
/// empty text.
fn truncate_text(value: Option<&str>) -> Option<String> {
    let value = value.filter(|value| !value.is_empty())?;
    if value.chars().count() <= DESCRIPTION_MAX {
        return Some(value.to_owned());
    }
    Some(format!(
        "{}...",
        take_chars(value, DESCRIPTION_MAX - 3).trim_end()
    ))
}

/// The success toast: what the action did, and as its button "View PR" after
/// a pull request, "Push" after a plain commit, and "Create PR" after a push
/// to a feature branch on GitHub (`offers_pr`).
fn result(
    action: GitAction,
    commit: Option<&Commit>,
    push: Option<&Push>,
    pr: Option<&Pr>,
    offers_pr: bool,
) -> GitRunResult {
    let short_sha = commit
        .map(|commit| commit.sha.as_str())
        .filter(|sha| !sha.is_empty())
        .map(|sha| take_chars(sha, SHORT_SHA_LENGTH));
    let description = truncate_text(commit.map(|commit| commit.subject.as_str()));
    let (title, description) = match (pr, push, commit) {
        (Some(pr), _, _) => {
            let verb = if pr.created { "Created" } else { "Opened" };
            let number = pr
                .number
                .map_or_else(String::new, |number| format!(" #{number}"));
            (format!("{verb} PR{number}"), truncate_text(Some(&pr.title)))
        }
        (None, Some(Push::Pushed { upstream }), _) => {
            let mut title = String::from("Pushed");
            if let Some(sha) = short_sha {
                title.push(' ');
                title.push_str(sha);
            }
            title.push_str(" to ");
            title.push_str(upstream);
            (title, description)
        }
        (None, _, Some(_)) => {
            let title = match short_sha {
                Some(sha) => format!("Committed {sha}"),
                None => "Committed changes".to_owned(),
            };
            (title, description)
        }
        _ => ("Done".to_owned(), None),
    };
    let run = |label: &str, action: GitAction| GitNext::Run {
        label: label.to_owned(),
        action,
    };
    let next = match (pr.and_then(|pr| pr.url.as_ref()), action) {
        (Some(url), _) => Some(GitNext::OpenPr {
            label: "View PR".to_owned(),
            url: url.clone(),
        }),
        (None, GitAction::Commit) if commit.is_some() => Some(run("Push", GitAction::Push)),
        (None, _) if offers_pr => Some(run("Create PR", GitAction::Pr)),
        _ => None,
    };
    GitRunResult {
        title,
        description,
        next,
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
    fn sections_truncate_with_a_marker() {
        assert_eq!(limit_section("abc", 3), "abc");
        assert_eq!(limit_section("abcd", 3), "abc\n\n[truncated]");
    }

    #[test]
    fn descriptions_truncate() {
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
          {"number": 1, "title": "a", "url": "u1", "state": "MERGED",
           "updatedAt": "2026-10-02T00:00:00Z"},
          {"number": 2, "title": "b", "url": "u2", "state": "OPEN",
           "updatedAt": "2026-10-01T00:00:00Z"},
          {"number": 3, "title": "c", "url": "u3", "state": "CLOSED",
           "updatedAt": "2026-09-01T00:00:00Z"}
        ]"#;
        assert_eq!(pick_pull_request(listed).unwrap().number, 2);
        let closed_only = listed.replace("OPEN", "CLOSED");
        let picked = pick_pull_request(&closed_only).unwrap();
        assert_eq!((picked.number, picked.state), (1, PullRequestState::Merged));
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
