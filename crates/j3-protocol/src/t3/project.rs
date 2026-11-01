//! Projects (`C/project.ts`, `C/orchestrationProject.ts`): `projects.mutate`
//! (ios-surface M12), `projects.readFile` (M19), `projects.searchEntries`
//! (M20) and the shell snapshot's project rows.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::double_option;
use super::environment::{RepositoryIdentity, ThreadEnvMode};
use super::ids::{CommandId, IsoDateTime, ProjectId};
use super::model::ModelSelection;

/// `ProjectScriptIcon` (`C/project.ts:24-31`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectScriptIcon {
    #[serde(rename = "play")]
    Play,
    #[serde(rename = "test")]
    Test,
    #[serde(rename = "lint")]
    Lint,
    #[serde(rename = "configure")]
    Configure,
    #[serde(rename = "build")]
    Build,
    #[serde(rename = "debug")]
    Debug,
}

/// `ProjectScript` (`C/project.ts:34-44`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectScript {
    pub id: String,
    pub name: String,
    pub command: String,
    pub icon: ProjectScriptIcon,
    pub run_on_worktree_create: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_on_settle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#async: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_open_preview: Option<bool>,
}

/// `ProjectIconColor` (`C/project.ts:47-66`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectIconColor {
    Gray,
    Red,
    Orange,
    Amber,
    Yellow,
    Lime,
    Green,
    Emerald,
    Teal,
    Cyan,
    Sky,
    Blue,
    Indigo,
    Violet,
    Purple,
    Fuchsia,
    Pink,
    Rose,
}

/// `ProjectIconOverride` (`C/project.ts:82-107`), tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProjectIconOverride {
    #[serde(rename = "lucide")]
    Lucide {
        name: String,
        color: ProjectIconColor,
    },
    #[serde(rename = "emoji")]
    Emoji { emoji: String },
    #[serde(rename = "monogram")]
    Monogram {
        text: String,
        color: ProjectIconColor,
    },
}

/// `Project` (`C/project.ts:152-169`); success of `projects.mutate`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub repository_identity: Option<Option<RepositoryIdentity>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub favicon_path: Option<Option<String>>,
    /// `ReceivedProjectIcon` (forward-compatible, legacy monogram aware); opaque here.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub project_icon: Option<Option<Value>>,
    pub default_model_selection: Option<ModelSelection>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull: Option<bool>,
    pub scripts: Vec<ProjectScript>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub pinned_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub archived_at: Option<Option<IsoDateTime>>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
    pub deleted_at: Option<IsoDateTime>,
}

/// `OrchestrationProjectShell` (`C/orchestrationProject.ts:8-25`): a project
/// row of the shell snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectShell {
    pub id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub repository_identity: Option<Option<RepositoryIdentity>>,
    pub default_model_selection: Option<ModelSelection>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub favicon_path: Option<Option<String>>,
    /// `ReceivedProjectIcon`; opaque here.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub project_icon: Option<Option<Value>>,
    pub scripts: Vec<ProjectScript>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub pinned_at: Option<Option<IsoDateTime>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub archived_at: Option<Option<IsoDateTime>>,
    pub created_at: IsoDateTime,
    pub updated_at: IsoDateTime,
}

/// `project.create` member of `ProjectMutation` (`C/project.ts:178-184,206-211`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCreateMutation {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    pub title: String,
    pub workspace_root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_workspace_root_if_missing: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub default_model_selection: Option<Option<ModelSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<Vec<ProjectScript>>,
}

/// `project.update` member of `ProjectMutation` (`C/project.ts:187-202,212-217`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectUpdateMutation {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub default_model_selection: Option<Option<ModelSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_pull: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub project_icon: Option<Option<ProjectIconOverride>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub favicon_path: Option<Option<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scripts: Option<Vec<ProjectScript>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    /// Threads and files are kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

/// `project.delete` member of `ProjectMutation` (`C/project.ts:218-223`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDeleteMutation {
    pub command_id: CommandId,
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

/// `ProjectMutation` (`C/project.ts:205-224`), tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProjectMutation {
    #[serde(rename = "project.create")]
    Create(ProjectCreateMutation),
    #[serde(rename = "project.update")]
    Update(ProjectUpdateMutation),
    #[serde(rename = "project.delete")]
    Delete(ProjectDeleteMutation),
}

json_literal! {
    /// `_tag` of [`ProjectMutationError`].
    pub struct ProjectMutationErrorTag = str("ProjectMutationError");
}

/// `ProjectMutationError` (`C/project.ts:227-234`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMutationError {
    #[serde(rename = "_tag")]
    pub tag: ProjectMutationErrorTag,
    pub command_id: CommandId,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

/// `ProjectEntryKind` (`C/project.ts:236`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectEntryKind {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "directory")]
    Directory,
}

/// `ProjectSearchEntriesInput` (`C/project.ts:239-245`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchEntriesInput {
    pub cwd: String,
    /// Trimmed, at most 256 chars.
    pub query: String,
    /// `PositiveInt` ≤ 200.
    pub limit: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProjectEntryKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_only: Option<bool>,
}

/// `ProjectEntry` (`C/project.ts:248-252`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntry {
    pub path: String,
    pub kind: ProjectEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored: Option<bool>,
}

/// `ProjectSearchEntriesResult` (`C/project.ts:255-258`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchEntriesResult {
    pub entries: Vec<ProjectEntry>,
    pub truncated: bool,
}

/// `ProjectEntriesFailure` (`C/project.ts:324-333`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectEntriesFailure {
    #[serde(rename = "workspace_root_not_found")]
    WorkspaceRootNotFound,
    #[serde(rename = "workspace_root_create_failed")]
    WorkspaceRootCreateFailed,
    #[serde(rename = "workspace_root_stat_failed")]
    WorkspaceRootStatFailed,
    #[serde(rename = "workspace_root_not_directory")]
    WorkspaceRootNotDirectory,
    #[serde(rename = "search_index_create_failed")]
    SearchIndexCreateFailed,
    #[serde(rename = "search_index_scan_timed_out")]
    SearchIndexScanTimedOut,
    #[serde(rename = "search_index_search_failed")]
    SearchIndexSearchFailed,
    #[serde(rename = "directory_list_failed")]
    DirectoryListFailed,
}

json_literal! {
    /// `_tag` of [`ProjectSearchEntriesError`].
    pub struct ProjectSearchEntriesErrorTag = str("ProjectSearchEntriesError");
}

/// `ProjectSearchEntriesError` (`C/project.ts:349-378`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSearchEntriesError {
    #[serde(rename = "_tag")]
    pub tag: ProjectSearchEntriesErrorTag,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ProjectEntriesFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

/// `ProjectReadFileInput` (`C/project.ts:433-439`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReadFileInput {
    pub cwd: String,
    /// Workspace-relative or absolute host path; at most 512 chars.
    pub relative_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_missing: Option<bool>,
}

/// `ProjectReadFileResult` (`C/project.ts:442-449`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReadFileResult {
    pub relative_path: String,
    pub contents: String,
    pub byte_length: u64,
    pub truncated: bool,
    /// `false` only for an absent optional file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
}

/// `ProjectFileFailure` (`C/project.ts:452-458`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectFileFailure {
    #[serde(rename = "workspace_path_outside_root")]
    WorkspacePathOutsideRoot,
    #[serde(rename = "resolved_path_outside_root")]
    ResolvedPathOutsideRoot,
    #[serde(rename = "path_not_file")]
    PathNotFile,
    #[serde(rename = "binary_file")]
    BinaryFile,
    #[serde(rename = "operation_failed")]
    OperationFailed,
}

/// `ProjectFileOperation` (`C/project.ts:461-470`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectFileOperation {
    #[serde(rename = "realpath-workspace-root")]
    RealpathWorkspaceRoot,
    #[serde(rename = "realpath-target")]
    RealpathTarget,
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "stat")]
    Stat,
    #[serde(rename = "read")]
    Read,
    #[serde(rename = "close")]
    Close,
    #[serde(rename = "make-directory")]
    MakeDirectory,
    #[serde(rename = "write-file")]
    WriteFile,
}

json_literal! {
    /// `_tag` of [`ProjectReadFileError`].
    pub struct ProjectReadFileErrorTag = str("ProjectReadFileError");
}

/// `ProjectReadFileError` (`C/project.ts:484-507`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectReadFileError {
    #[serde(rename = "_tag")]
    pub tag: ProjectReadFileErrorTag,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<ProjectFileFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_workspace_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<ProjectFileOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_path: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

impl ProjectMutationError {
    pub fn new(command_id: CommandId, message: impl Into<String>) -> Self {
        Self {
            tag: ProjectMutationErrorTag,
            command_id,
            message: message.into(),
            cause: None,
        }
    }
}

impl ProjectSearchEntriesError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            tag: ProjectSearchEntriesErrorTag,
            cwd: None,
            query_length: None,
            limit: None,
            failure: None,
            normalized_cwd: None,
            timeout: None,
            detail: None,
            message: message.into(),
            cause: None,
        }
    }
}

impl ProjectReadFileError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            tag: ProjectReadFileErrorTag,
            cwd: None,
            relative_path: None,
            failure: None,
            resolved_path: None,
            resolved_workspace_root: None,
            operation: None,
            operation_path: None,
            message: message.into(),
            cause: None,
        }
    }
}
