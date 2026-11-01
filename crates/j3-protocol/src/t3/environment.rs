//! Execution environment descriptor (`C/environment.ts`), served by
//! `GET /.well-known/t3/environment` and embedded in `ServerConfig`.
//!
//! Also hosts the serde helpers for t3's forward-compatible schemas
//! (`C/baseSchemas.ts:48-132`), shared by the other t3 modules.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::ids::EnvironmentId;

/// `ForwardCompatibleOptional(X)` (`C/baseSchemas.ts:48-62`): an absent key or
/// a value that does not decode as `X` both decode to `None`.
pub(crate) fn forward_compatible_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// `ForwardCompatibleArray(X)` (`C/baseSchemas.ts:98-132`): elements that do
/// not decode as `X` are dropped; the array itself must be an array.
pub(crate) fn forward_compatible_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let values = Vec::<Value>::deserialize(deserializer)?;
    Ok(values
        .into_iter()
        .filter_map(|value| serde_json::from_value(value).ok())
        .collect())
}

/// `optionalKey(ForwardCompatibleArray(X))`: [`forward_compatible_vec`] for a
/// key that may be absent (pair with `#[serde(default)]`).
pub(crate) fn forward_compatible_vec_option<'de, D, T>(
    deserializer: D,
) -> Result<Option<Vec<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    forward_compatible_vec(deserializer).map(Some)
}

/// `ORCHESTRATION_PROTOCOL_VERSION` (`C/environment.ts:13`).
pub const ORCHESTRATION_PROTOCOL_VERSION: i64 = 2;

/// `ExecutionEnvironmentPlatformOs` (`C/environment.ts:18-23`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionEnvironmentPlatformOs {
    #[serde(rename = "darwin")]
    Darwin,
    #[serde(rename = "linux")]
    Linux,
    #[serde(rename = "windows")]
    Windows,
    #[serde(rename = "unknown")]
    Unknown,
}

/// `ExecutionEnvironmentPlatformArch` (`C/environment.ts:26`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionEnvironmentPlatformArch {
    #[serde(rename = "arm64")]
    Arm64,
    #[serde(rename = "x64")]
    X64,
    #[serde(rename = "other")]
    Other,
}

/// `EnvironmentMachineKind` (`C/environment.ts:29-39`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvironmentMachineKind {
    #[serde(rename = "server")]
    Server,
    #[serde(rename = "cloud")]
    Cloud,
    #[serde(rename = "linux")]
    Linux,
    #[serde(rename = "desktop")]
    Desktop,
    #[serde(rename = "laptop")]
    Laptop,
    #[serde(rename = "mac-mini")]
    MacMini,
    #[serde(rename = "mac-studio")]
    MacStudio,
}

/// `ExecutionEnvironmentPlatform` (`C/environment.ts:42-46`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentPlatform {
    pub os: ExecutionEnvironmentPlatformOs,
    pub arch: ExecutionEnvironmentPlatformArch,
    /// `ForwardCompatibleOptional`: unknown values decode as absent.
    #[serde(
        default,
        deserialize_with = "forward_compatible_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub machine: Option<EnvironmentMachineKind>,
}

/// `ThreadEnvMode` (`C/environment.ts:49`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreadEnvMode {
    #[serde(rename = "local")]
    Local,
    #[serde(rename = "worktree")]
    Worktree,
}

/// `ServerSelfUpdateMethod` (`C/environment.ts:55`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerSelfUpdateMethod {
    #[serde(rename = "boot-service")]
    BootService,
    #[serde(rename = "respawn")]
    Respawn,
    #[serde(rename = "desktop-app")]
    DesktopApp,
}

/// `ServerInstallation` `kind` of the package-runner member (`C/environment.ts:59`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerInstallationRunner {
    #[serde(rename = "npx")]
    Npx,
    #[serde(rename = "pnpm-dlx")]
    PnpmDlx,
    #[serde(rename = "bunx")]
    Bunx,
}

json_literal! {
    /// `Schema.Literal("npm-global")`.
    pub struct NpmGlobalKind = str("npm-global");
}

/// `ServerInstallation` (`C/environment.ts:58-61`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ServerInstallation {
    Runner { kind: ServerInstallationRunner },
    NpmGlobal { kind: NpmGlobalKind, prefix: String },
}

/// `ServerSelfUpdateCapability` (`C/environment.ts:64-68`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerSelfUpdateCapability {
    #[serde(rename = "boot-service")]
    BootService,
    #[serde(rename = "respawn")]
    Respawn,
    #[serde(rename = "desktop-managed")]
    DesktopManaged,
}

/// `ExecutionEnvironmentCapabilities.fileAttachments` (`C/environment.ts:76-80`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachmentsCapability {
    /// `Int` ≥ 1.
    pub max_upload_bytes: i64,
}

/// `ExecutionEnvironmentCapabilities` (`C/environment.ts:72-166`). `{}`
/// decodes: `repositoryIdentity` defaults to `false`, every other key is
/// `optionalKey`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentCapabilities {
    #[serde(default)]
    pub repository_identity: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_probe: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_uploads: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_attachments: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_attachments: Option<FileAttachmentsCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_checks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_chats: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_message_context: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_worktree_bootstrap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_settlement: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_auto_settlement: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_cleanup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_cleanup_run: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_storage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_endpoint_check: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_worktree_cleanup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_worktree_cleanup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees_directory: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_auto_settlement_scope: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_restart_continuation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_settings_overrides: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_provider_instance_overrides: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_snooze: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_force_stop: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_themes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_sources: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_workspace_snapshots_on_demand: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_price_overrides: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_model_aliases: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pinning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pin_reorder: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_active_reorder: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_auto_settle_opt_out: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_project_from_scratch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_title_regeneration: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_visited_tracking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pull_request_linking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_resolved_command_context: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_stop: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_server_stop: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pull_requests: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pull_request_watch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_stack_actions: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_self_update: Option<ServerSelfUpdateCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm_release_lookup: Option<bool>,
    /// `ForwardCompatibleOptional`: unknown shapes decode as absent.
    #[serde(
        default,
        deserialize_with = "forward_compatible_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub server_installation: Option<ServerInstallation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_self_update_progress: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_update_thread_continuation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_activity_publishing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_clone_tracking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_organization: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored_file_listing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_icon: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_app_update: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_app_channel_switch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_output_streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_browser: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_links: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_mode_limit_header: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_fence: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_time_schedules: Option<bool>,
}

/// `ExecutionEnvironmentDescriptor` (`C/environment.ts:169-178`); body of
/// `GET /.well-known/t3/environment` (ios-surface H1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentDescriptor {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub platform: ExecutionEnvironmentPlatform,
    pub server_version: String,
    /// Absent means protocol 1; j3 sends [`ORCHESTRATION_PROTOCOL_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_protocol_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_migration: Option<i64>,
    pub capabilities: ExecutionEnvironmentCapabilities,
}

json_literal! {
    /// `Schema.Literal("git-remote")`.
    pub struct GitRemoteSource = str("git-remote");
}

/// `RepositoryIdentityLocator` (`C/environment.ts:180-184`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryIdentityLocator {
    pub source: GitRemoteSource,
    pub remote_name: String,
    pub remote_url: String,
}

/// `RepositoryOrigin` (`C/environment.ts:187-191`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryOrigin {
    pub canonical_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// `RepositoryIdentity` (`C/environment.ts:194-206`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryIdentity {
    pub canonical_key: String,
    pub locator: RepositoryIdentityLocator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// `SourceControlProviderKind` (`C/sourceControl.ts`); opaque here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<RepositoryOrigin>,
}
