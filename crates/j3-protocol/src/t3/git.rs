//! The git workflow methods (`C/git.ts` = `G`, tags in `C/rpc.ts:208-221`):
//! `vcs.refreshStatus`, `vcs.pull` and the streaming `git.runStackedAction`.
//! j3 serves commit, push and, through the host's `gh`, GitHub pull requests
//! (`create_pr`, `commit_push_pr`, and the open PR in the status). Feature
//! branches are cut.

use serde::{Deserialize, Serialize};

/// `vcs.refreshStatus` (`WsVcsRefreshStatusRpc`, `C/rpc.ts:647-651`).
pub const VCS_REFRESH_STATUS: &str = "vcs.refreshStatus";
/// `vcs.pull` (`C/rpc.ts:209`).
pub const VCS_PULL: &str = "vcs.pull";
/// `git.runStackedAction`, a stream of [`GitActionProgressEvent`]
/// (`WsGitRunStackedActionRpc`, `C/rpc.ts:653-658`).
pub const GIT_RUN_STACKED_ACTION: &str = "git.runStackedAction";

/// `GitStackedAction` (`G:10-17`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitStackedAction {
    Commit,
    Push,
    CreatePr,
    CommitPush,
    CommitPushPr,
}

impl GitStackedAction {
    /// Whether the action commits first.
    pub fn commits(self) -> bool {
        matches!(self, Self::Commit | Self::CommitPush | Self::CommitPushPr)
    }

    /// Whether the action pushes (`create_pr` pushes only when it must).
    pub fn pushes(self) -> bool {
        matches!(self, Self::Push | Self::CommitPush | Self::CommitPushPr)
    }
}

/// `GitActionProgressPhase` (`G:19`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitActionProgressPhase {
    Branch,
    Commit,
    Push,
    Pr,
}

/// `VcsStatusInput` / `VcsPullInput` (`G:96-104`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsCwdInput {
    pub cwd: String,
}

/// `GitRunStackedActionInput` (`G:106-118`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRunStackedActionInput {
    pub action_id: String,
    pub cwd: String,
    pub action: GitStackedAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_branch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_paths: Option<Vec<String>>,
}

/// One changed file of [`VcsWorkingTree`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsWorkingTreeFile {
    pub path: String,
    pub insertions: u64,
    pub deletions: u64,
}

/// `VcsStatusLocalShape.workingTree` (`G:209-219`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsWorkingTree {
    pub files: Vec<VcsWorkingTreeFile>,
    pub insertions: u64,
    pub deletions: u64,
}

/// `SourceControlProviderInfo` (`C/sourceControl.ts:12-16`): the host a
/// repository's remote lives on. j3 knows GitHub only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProviderInfo {
    pub kind: String,
    pub name: String,
    pub base_url: String,
}

impl SourceControlProviderInfo {
    pub fn github() -> Self {
        Self {
            kind: "github".to_owned(),
            name: "GitHub".to_owned(),
            base_url: "https://github.com".to_owned(),
        }
    }
}

/// `VcsStatusChangeRequestState` (`G:52`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VcsChangeRequestState {
    Open,
    Closed,
    Merged,
}

/// `VcsStatusChangeRequest` (`G:203-214`): the pull request of the branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsChangeRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub base_ref: String,
    pub head_ref: String,
    pub state: VcsChangeRequestState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// `VcsStatusResult` (`G:202-240`): the local and remote halves together.
/// `sourceControlProvider` is set for GitHub remotes and `pr` is the
/// branch's latest pull request there, read through `gh`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusResult {
    pub is_repo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_control_provider: Option<SourceControlProviderInfo>,
    pub has_primary_remote: bool,
    pub is_default_ref: bool,
    /// `None` on a detached HEAD.
    pub ref_name: Option<String>,
    pub has_working_tree_changes: bool,
    pub working_tree: VcsWorkingTree,
    pub has_upstream: bool,
    pub ahead_count: u64,
    pub behind_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead_of_default_count: Option<u64>,
    pub pr: Option<VcsChangeRequest>,
}

impl VcsStatusResult {
    /// A directory that is not inside a git work tree.
    pub fn not_a_repo() -> Self {
        Self {
            is_repo: false,
            source_control_provider: None,
            has_primary_remote: false,
            is_default_ref: false,
            ref_name: None,
            has_working_tree_changes: false,
            working_tree: VcsWorkingTree::default(),
            has_upstream: false,
            ahead_count: 0,
            behind_count: 0,
            ahead_of_default_count: None,
            pr: None,
        }
    }
}

/// `VcsPullResult` status (`G:324-329`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VcsPullStatus {
    Pulled,
    SkippedUpToDate,
}

/// `VcsPullResult` (`G:324-329`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsPullResult {
    pub status: VcsPullStatus,
    pub ref_name: String,
    pub upstream_ref: Option<String>,
}

/// `GitBranchStepStatus` (`G:42`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitBranchStepStatus {
    Created,
    SkippedNotRequested,
}

/// `GitCommitStepStatus` (`G:32-36`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCommitStepStatus {
    Created,
    SkippedNoChanges,
    SkippedNotRequested,
}

/// `GitPushStepStatus` (`G:37-41`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPushStepStatus {
    Pushed,
    SkippedNotRequested,
    SkippedUpToDate,
}

/// `GitPrStepStatus` (`G:43`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPrStepStatus {
    Created,
    OpenedExisting,
    SkippedNotRequested,
}

/// `GitRunStackedActionResult.branch` (`G:301-304`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchStep {
    pub status: GitBranchStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `GitRunStackedActionResult.commit` (`G:305-309`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitStep {
    pub status: GitCommitStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// `GitRunStackedActionResult.push` (`G:310-315`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPushStep {
    pub status: GitPushStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_upstream: Option<bool>,
}

/// `GitRunStackedActionResult.pr` (`G:316-323`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPrStep {
    pub status: GitPrStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// `GitRunStackedActionToastRunAction` (`G:48-50`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitToastRunAction {
    pub kind: GitStackedAction,
}

/// `GitRunStackedActionToastCta` (`G:52-67`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitToastCta {
    None,
    OpenPr {
        label: String,
        url: String,
    },
    RunAction {
        label: String,
        action: GitToastRunAction,
    },
}

/// `GitRunStackedActionToast` (`G:69-74`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitToast {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub cta: GitToastCta,
}

/// `GitRunStackedActionResult` (`G:299-322`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRunStackedActionResult {
    pub action: GitStackedAction,
    pub branch: GitBranchStep,
    pub commit: GitCommitStep,
    pub push: GitPushStep,
    pub pr: GitPrStep,
    pub toast: GitToast,
}

/// `GitActionProgressStream` (`G:30`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitActionProgressStream {
    Stdout,
    Stderr,
}

/// What a [`GitActionProgressEvent`] reports (`G:417-461`), tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitActionProgress {
    ActionStarted {
        phases: Vec<GitActionProgressPhase>,
    },
    PhaseStarted {
        phase: GitActionProgressPhase,
        label: String,
    },
    #[serde(rename_all = "camelCase")]
    HookStarted {
        hook_name: String,
    },
    #[serde(rename_all = "camelCase")]
    HookOutput {
        hook_name: Option<String>,
        stream: GitActionProgressStream,
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    HookFinished {
        hook_name: String,
        exit_code: Option<i64>,
        duration_ms: Option<u64>,
    },
    ActionFinished {
        result: Box<GitRunStackedActionResult>,
    },
    ActionFailed {
        phase: Option<GitActionProgressPhase>,
        message: String,
    },
}

/// `GitActionProgressEvent` (`G:411-472`): the fields every event shares,
/// then the [`GitActionProgress`] it reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionProgressEvent {
    pub action_id: String,
    pub cwd: String,
    pub action: GitStackedAction,
    #[serde(flatten)]
    pub progress: GitActionProgress,
}

/// The `GitManagerServiceError` members j3 sends (`G:342-400`). t3 builds
/// each one's `message` from its fields; j3 also sends it, so a client sees
/// the same sentence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum GitManagerServiceError {
    /// `GitManagerError`: message `Git manager failed in {operation}: {detail}`.
    GitManagerError {
        operation: String,
        cwd: String,
        detail: String,
        message: String,
    },
    /// `GitCommandError`: message `Git command failed in {operation} ({cwd}): {detail}`.
    #[serde(rename_all = "camelCase")]
    GitCommandError {
        operation: String,
        command: String,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i64>,
        detail: String,
        message: String,
    },
    /// `TextGenerationError`: message `Text generation failed in {operation}: {detail}`.
    TextGenerationError {
        operation: String,
        detail: String,
        message: String,
    },
}

impl GitManagerServiceError {
    pub fn manager(
        operation: impl Into<String>,
        cwd: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        let (operation, detail) = (operation.into(), detail.into());
        Self::GitManagerError {
            message: format!("Git manager failed in {operation}: {detail}"),
            operation,
            cwd: cwd.into(),
            detail,
        }
    }

    pub fn command(
        operation: impl Into<String>,
        command: impl Into<String>,
        cwd: impl Into<String>,
        exit_code: Option<i64>,
        detail: impl Into<String>,
    ) -> Self {
        let (operation, cwd, detail) = (operation.into(), cwd.into(), detail.into());
        Self::GitCommandError {
            message: format!("Git command failed in {operation} ({cwd}): {detail}"),
            operation,
            command: command.into(),
            cwd,
            exit_code,
            detail,
        }
    }

    pub fn text_generation(operation: impl Into<String>, detail: impl Into<String>) -> Self {
        let (operation, detail) = (operation.into(), detail.into());
        Self::TextGenerationError {
            message: format!("Text generation failed in {operation}: {detail}"),
            operation,
            detail,
        }
    }

    /// The human-readable sentence.
    pub fn message(&self) -> &str {
        match self {
            Self::GitManagerError { message, .. }
            | Self::GitCommandError { message, .. }
            | Self::TextGenerationError { message, .. } => message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn progress_events_carry_the_shared_fields_and_their_kind() {
        let event = GitActionProgressEvent {
            action_id: "a1".into(),
            cwd: "/repo".into(),
            action: GitStackedAction::CommitPush,
            progress: GitActionProgress::PhaseStarted {
                phase: GitActionProgressPhase::Commit,
                label: "Committing...".into(),
            },
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({"actionId": "a1", "cwd": "/repo", "action": "commit_push",
                   "kind": "phase_started", "phase": "commit", "label": "Committing..."})
        );
        let failed: GitActionProgressEvent = serde_json::from_value(json!({
            "actionId": "a1", "cwd": "/repo", "action": "push",
            "kind": "action_failed", "phase": null, "message": "Cannot push from detached HEAD."
        }))
        .unwrap();
        assert_eq!(
            failed.progress,
            GitActionProgress::ActionFailed {
                phase: None,
                message: "Cannot push from detached HEAD.".into()
            }
        );
    }

    #[test]
    fn toast_ctas_are_tagged_by_kind() {
        let cta = GitToastCta::RunAction {
            label: "Push".into(),
            action: GitToastRunAction {
                kind: GitStackedAction::Push,
            },
        };
        assert_eq!(
            serde_json::to_value(&cta).unwrap(),
            json!({"kind": "run_action", "label": "Push", "action": {"kind": "push"}})
        );
        assert_eq!(
            serde_json::to_value(GitToastCta::None).unwrap(),
            json!({"kind": "none"})
        );
    }
}
