//! Server config and providers (`C/server.ts`): `subscribeServerConfig`
//! (ios-surface M1), `server.probe` (M2), `server.refreshProviders` (M3),
//! `server.updateSettings` (M22) and `server.getConfig` (G10).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::One;
use super::auth::ServerAuthDescriptor;
use super::double_option;
use super::environment::{
    ExecutionEnvironmentDescriptor, forward_compatible_vec, forward_compatible_vec_option,
};
use super::ids::{IsoDateTime, ProviderDriverKind, ProviderInstanceId};
use super::model::{ModelCapabilities, ProviderOptionDescriptor};

/// `ServerConfigIssue` (`C/server.ts:33-47`), tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ServerConfigIssue {
    #[serde(rename = "keybindings.malformed-config")]
    KeybindingsMalformedConfig { message: String },
    #[serde(rename = "keybindings.invalid-entry")]
    KeybindingsInvalidEntry {
        message: String,
        /// `Schema.Number`.
        index: f64,
    },
}

/// `ServerProviderState` (`C/server.ts:52`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerProviderState {
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "warning")]
    Warning,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "disabled")]
    Disabled,
}

/// `ServerProviderAuthStatus` (`C/server.ts:55-59`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerProviderAuthStatus {
    #[serde(rename = "authenticated")]
    Authenticated,
    #[serde(rename = "unauthenticated")]
    Unauthenticated,
    #[serde(rename = "unknown")]
    Unknown,
}

/// `AcpRegistryUrlAuthAction` (`C/acpRegistry.ts:122-128`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpRegistryUrlAuthAction {
    pub elicitation_id: String,
    pub url: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// `ServerProviderAuth` (`C/server.ts:62-72`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderAuth {
    pub status: ServerProviderAuthStatus,
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<AcpRegistryUrlAuthAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_logout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_sharing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

json_literal! {
    /// `Schema.Literal("new")` (model badge).
    pub struct NewBadge = str("new");
}

/// `ServerProviderModel` (`C/server.ts:75-86`). `capabilities` is a required
/// key (`null` allowed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderModel {
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<NewBadge>,
    pub is_custom: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_legacy: Option<bool>,
    pub capabilities: Option<ModelCapabilities>,
}

/// `ServerProviderUpdateRequiredModel` (`C/server.ts:89-94`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUpdateRequiredModel {
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<NewBadge>,
    pub min_version: String,
}

/// `ServerProviderSlashCommandInput` (`C/server.ts:97-99`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSlashCommandInput {
    pub hint: String,
}

/// `ServerProviderSlashCommand` (`C/server.ts:102-106`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSlashCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<ServerProviderSlashCommandInput>,
}

/// `ServerProviderSkill` (`C/server.ts:109-121`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSkill {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_invocation_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_invocable: Option<bool>,
}

/// `ServerProviderWorkspaceSnapshot` (`C/server.ts:124-131`): per-cwd slash
/// commands and skills.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderWorkspaceSnapshot {
    pub cwd: String,
    pub checked_at: IsoDateTime,
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slash_commands_pending: Option<bool>,
    pub skills: Vec<ServerProviderSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_descriptors: Option<Vec<ProviderOptionDescriptor>>,
}

/// `ServerProviderAvailability` (`C/server.ts:144`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerProviderAvailability {
    #[serde(rename = "available")]
    Available,
    #[serde(rename = "unavailable")]
    Unavailable,
}

/// `ServerProviderContinuation` (`C/server.ts:147-149`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderContinuation {
    pub group_key: String,
}

/// `ServerProviderCompatibilityStatus` (`C/server.ts:152-158`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerProviderCompatibilityStatus {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "supported")]
    Supported,
    #[serde(rename = "graceful")]
    Graceful,
    #[serde(rename = "unsupported")]
    Unsupported,
    #[serde(rename = "broken")]
    Broken,
}

/// `ServerProviderCompatibilityAdvisory` (`C/server.ts:159-165`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderCompatibilityAdvisory {
    pub status: ServerProviderCompatibilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version_status: Option<ServerProviderCompatibilityStatus>,
    pub message: Option<String>,
    pub recommended_version: Option<String>,
    pub recommended_range: Option<String>,
}

/// `ServerProviderVersionAdvisoryStatus` (`C/server.ts:168-172`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerProviderVersionAdvisoryStatus {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "current")]
    Current,
    #[serde(rename = "behind_latest")]
    BehindLatest,
}

/// `ServerProviderVersionAdvisory` (`C/server.ts:175-184`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderVersionAdvisory {
    pub status: ServerProviderVersionAdvisoryStatus,
    pub current_version: Option<String>,
    pub latest_version: Option<String>,
    pub update_command: Option<String>,
    #[serde(default)]
    pub can_update: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_install_version: Option<bool>,
    pub checked_at: Option<IsoDateTime>,
    pub message: Option<String>,
}

/// `ServerProviderUpdateStatus` (`C/server.ts:187-194`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerProviderUpdateStatus {
    #[serde(rename = "idle")]
    Idle,
    #[serde(rename = "queued")]
    Queued,
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "succeeded")]
    Succeeded,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "unchanged")]
    Unchanged,
}

/// `ServerProviderUpdateState` (`C/server.ts:197-203`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUpdateState {
    pub status: ServerProviderUpdateStatus,
    pub started_at: Option<IsoDateTime>,
    pub finished_at: Option<IsoDateTime>,
    pub message: Option<String>,
    /// At most 10 000 chars.
    pub output: Option<String>,
}

/// `RuntimeMode` (`C/providerPolicy.ts:25-30`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeMode {
    #[serde(rename = "approval-required")]
    ApprovalRequired,
    #[serde(rename = "auto-accept-edits")]
    AutoAcceptEdits,
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "full-access")]
    FullAccess,
}

/// `ServerProvider.setup` (`C/server.ts:221-227`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSetup {
    pub can_authenticate: bool,
    pub can_install: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
}

/// `ServerProvider.nativeSessions` (`C/server.ts:228-235`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderNativeSessions {
    pub can_list: bool,
    pub can_load: bool,
    pub can_resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_delete: Option<bool>,
}

/// `ServerProvider.runtimePaths` (`C/server.ts:237-242`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderRuntimePaths {
    pub home_path: String,
    pub shadow_home_path: Option<String>,
}

/// `ServerProvider` (`C/server.ts:206-267`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProvider {
    pub instance_id: ProviderInstanceId,
    pub driver: ProviderDriverKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ServerProviderContinuation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_interaction_mode_toggle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reports_context_window: Option<bool>,
    /// `ForwardCompatibleArray`: unknown modes are dropped on decode.
    #[serde(
        default,
        deserialize_with = "forward_compatible_vec_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub supported_runtime_modes: Option<Vec<RuntimeMode>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_conversation_rollback: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_text_generation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<ServerProviderSetup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_sessions: Option<ServerProviderNativeSessions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configurable_providers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_paths: Option<ServerProviderRuntimePaths>,
    pub enabled: bool,
    pub installed: bool,
    pub version: Option<String>,
    /// `optionalKey(NullOr)`: `null` means unknown, absent means legacy.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub installed_version: Option<Option<String>>,
    pub status: ServerProviderState,
    pub auth: ServerProviderAuth,
    pub checked_at: IsoDateTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<ServerProviderAvailability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub models: Vec<ServerProviderModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_required_models: Option<Vec<ServerProviderUpdateRequiredModel>>,
    #[serde(default)]
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    #[serde(default)]
    pub skills: Vec<ServerProviderSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshots: Option<Vec<ServerProviderWorkspaceSnapshot>>,
    /// `ServerProviderUsageLimits` (`C/providerUsageLimits.ts`); opaque here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limits: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_advisory: Option<ServerProviderVersionAdvisory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility_advisory: Option<ServerProviderCompatibilityAdvisory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_state: Option<ServerProviderUpdateState>,
}

/// `OtlpEndpointSource` (`C/server.ts:280`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OtlpEndpointSource {
    #[serde(rename = "environment")]
    Environment,
    #[serde(rename = "bootstrap")]
    Bootstrap,
    #[serde(rename = "settings")]
    Settings,
}

/// `ServerObservability` (`C/server.ts:283-298`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerObservability {
    pub logs_directory_path: String,
    pub local_tracing_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_traces_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_traces_source: Option<OtlpEndpointSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_traces_variable: Option<String>,
    pub otlp_traces_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_metrics_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_metrics_source: Option<OtlpEndpointSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_metrics_variable: Option<String>,
    pub otlp_metrics_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_source: Option<OtlpEndpointSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_variable: Option<String>,
    #[serde(default)]
    pub otlp_logs_enabled: bool,
}

/// `EditorId` (`C/editor.ts:17-151`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditorId {
    #[serde(rename = "cursor")]
    Cursor,
    #[serde(rename = "trae")]
    Trae,
    #[serde(rename = "kiro")]
    Kiro,
    #[serde(rename = "vscode")]
    Vscode,
    #[serde(rename = "vscode-insiders")]
    VscodeInsiders,
    #[serde(rename = "vscodium")]
    Vscodium,
    #[serde(rename = "zed")]
    Zed,
    #[serde(rename = "antigravity")]
    Antigravity,
    #[serde(rename = "idea")]
    Idea,
    #[serde(rename = "aqua")]
    Aqua,
    #[serde(rename = "clion")]
    Clion,
    #[serde(rename = "datagrip")]
    Datagrip,
    #[serde(rename = "dataspell")]
    Dataspell,
    #[serde(rename = "goland")]
    Goland,
    #[serde(rename = "phpstorm")]
    Phpstorm,
    #[serde(rename = "pycharm")]
    Pycharm,
    #[serde(rename = "rider")]
    Rider,
    #[serde(rename = "rubymine")]
    Rubymine,
    #[serde(rename = "rustrover")]
    Rustrover,
    #[serde(rename = "webstorm")]
    Webstorm,
    #[serde(rename = "file-manager")]
    FileManager,
}

/// `FileManagerRevealKind` (`C/editor.ts:154`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileManagerRevealKind {
    #[serde(rename = "finder")]
    Finder,
    #[serde(rename = "file-explorer")]
    FileExplorer,
    #[serde(rename = "files")]
    Files,
}

/// `RemoteOpenTargetKind` (`C/editor.ts:222`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteOpenTargetKind {
    #[serde(rename = "tailscale")]
    Tailscale,
    #[serde(rename = "mdns")]
    Mdns,
}

/// `RemoteOpenTarget` (`C/editor.ts:225-229`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOpenTarget {
    pub kind: RemoteOpenTargetKind,
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// `ServerDirectEndpointKind` (`C/server.ts:587`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerDirectEndpointKind {
    #[serde(rename = "lan")]
    Lan,
    #[serde(rename = "tailnet")]
    Tailnet,
}

/// `ServerDirectEndpoint` (`C/server.ts:590-593`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerDirectEndpoint {
    pub kind: ServerDirectEndpointKind,
    pub http_base_url: String,
}

/// `ServerEventLoopStall` (`C/server.ts:600-606`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerEventLoopStall {
    pub detected_at: IsoDateTime,
    pub duration_ms: u64,
    /// `Schema.Number`; absent on Windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_average: Option<f64>,
    pub cpu_count: u64,
}

/// `ServerConfig` (`C/server.ts:609-661`); `server.getConfig` success and the
/// `snapshot` stream item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub environment: ExecutionEnvironmentDescriptor,
    pub auth: ServerAuthDescriptor,
    pub cwd: String,
    pub keybindings_config_path: String,
    /// `ResolvedKeybindingsConfig`: rules are opaque here.
    pub keybindings: Vec<Value>,
    /// `ForwardCompatibleArray`.
    #[serde(deserialize_with = "forward_compatible_vec")]
    pub issues: Vec<ServerConfigIssue>,
    /// `ForwardCompatibleArray`.
    #[serde(deserialize_with = "forward_compatible_vec")]
    pub providers: Vec<ServerProvider>,
    /// `ForwardCompatibleArray`.
    #[serde(deserialize_with = "forward_compatible_vec")]
    pub available_editors: Vec<EditorId>,
    #[serde(
        default,
        deserialize_with = "forward_compatible_vec_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub remote_open_targets: Option<Vec<RemoteOpenTarget>>,
    #[serde(
        default,
        deserialize_with = "forward_compatible_vec_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub direct_endpoints: Option<Vec<ServerDirectEndpoint>>,
    pub observability: ServerObservability,
    /// `ServerSettings` (`C/settings.ts`); opaque. `{}` decodes in t3.
    pub settings: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_resume_completion_marker: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_thread_families: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_older_settled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_reveal_in_file_manager: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_reveal_in_file_manager_kind: Option<FileManagerRevealKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_resume_completion_marker: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_snapshot_pagination: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_find: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_find_progressive: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_provider_settings: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_references: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scratch_workspace_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_projects_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_loop_stall: Option<ServerEventLoopStall>,
    /// `EnvironmentTheme[]`; never sent in a snapshot. Opaque here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_themes: Option<Vec<Value>>,
    /// `UsageLimitSourceSnapshots`; stream-only. Opaque here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_sources: Option<Value>,
}

/// `ServerConfigStreamSnapshotEvent` (`C/server.ts:723-727`) without `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigSnapshotEvent {
    pub version: One,
    pub config: ServerConfig,
}

/// `ServerConfigKeybindingsUpdatedPayload` (`C/server.ts:706-709`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigKeybindingsUpdatedPayload {
    /// `ResolvedKeybindingsConfig`; opaque rules.
    pub keybindings: Vec<Value>,
    #[serde(deserialize_with = "forward_compatible_vec")]
    pub issues: Vec<ServerConfigIssue>,
}

/// `ServerConfigStreamKeybindingsUpdatedEvent` (`C/server.ts:730-734`) without `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigKeybindingsUpdatedEvent {
    pub version: One,
    pub payload: ServerConfigKeybindingsUpdatedPayload,
}

/// `ServerConfigProviderStatusesPayload` (`C/server.ts:713-715`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigProviderStatusesPayload {
    #[serde(deserialize_with = "forward_compatible_vec")]
    pub providers: Vec<ServerProvider>,
}

/// `ServerConfigStreamProviderStatusesEvent` (`C/server.ts:738-742`) without `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigProviderStatusesEvent {
    pub version: One,
    pub payload: ServerConfigProviderStatusesPayload,
}

/// `ServerConfigSettingsUpdatedPayload` (`C/server.ts:718-720`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigSettingsUpdatedPayload {
    /// `ServerSettings`; opaque.
    pub settings: Value,
}

/// `ServerConfigStreamSettingsUpdatedEvent` (`C/server.ts:746-750`) without `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigSettingsUpdatedEvent {
    pub version: One,
    pub payload: ServerConfigSettingsUpdatedPayload,
}

/// `ServerConfigEnvironmentThemesUpdatedPayload` (`C/server.ts:754-756`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigEnvironmentThemesUpdatedPayload {
    /// `EnvironmentTheme[]`; opaque themes.
    pub themes: Vec<Value>,
}

/// `ServerConfigStreamEnvironmentThemesUpdatedEvent` (`C/server.ts:760-764`) without `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigEnvironmentThemesUpdatedEvent {
    pub version: One,
    pub payload: ServerConfigEnvironmentThemesUpdatedPayload,
}

/// `ServerConfigUsageLimitSourcesUpdatedPayload` (`C/server.ts:768-770`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigUsageLimitSourcesUpdatedPayload {
    /// `UsageLimitSourceSnapshots`; opaque.
    pub sources: Value,
}

/// `ServerConfigStreamUsageLimitSourcesUpdatedEvent` (`C/server.ts:774-778`) without `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigUsageLimitSourcesUpdatedEvent {
    pub version: One,
    pub payload: ServerConfigUsageLimitSourcesUpdatedPayload,
}

open_union! {
    /// `ServerConfigStreamEvent` (`C/server.ts:782-789`), tagged by `type`;
    /// item of `subscribeServerConfig`. Unknown types are kept verbatim.
    pub enum ServerConfigStreamEvent {
        tag = "type";
        newtype {
            "snapshot" => Snapshot(ServerConfigSnapshotEvent),
            "keybindingsUpdated" => KeybindingsUpdated(ServerConfigKeybindingsUpdatedEvent),
            "providerStatuses" => ProviderStatuses(ServerConfigProviderStatusesEvent),
            "settingsUpdated" => SettingsUpdated(ServerConfigSettingsUpdatedEvent),
            "environmentThemesUpdated" => EnvironmentThemesUpdated(ServerConfigEnvironmentThemesUpdatedEvent),
            "usageLimitSourcesUpdated" => UsageLimitSourcesUpdated(ServerConfigUsageLimitSourcesUpdatedEvent),
        }
        unit {}
    }
}

/// `subscribeServerConfig` payload (`C/rpc.ts:1862-1872`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeServerConfigInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_themes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_sources: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limits_command: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshots_on_demand: Option<bool>,
}

/// `server.probe` payload (`C/rpc.ts:627-631`): `{}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerProbeInput {}

/// `server.probe` success (`C/rpc.ts:627-631`): `{}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerProbeResult {}

/// `server.getConfig` payload (`C/rpc.ts:633-637`): `{}`. Success is [`ServerConfig`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerGetConfigInput {}

/// `server.refreshProviders` payload (`C/rpc.ts:639-656`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerRefreshProvidersInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<ProviderInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_models: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reload_secrets: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshots_on_demand: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limits_command: Option<bool>,
}

/// `ServerProviderUpdatedPayload` (`C/server.ts:859-861`); success of
/// `server.refreshProviders`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUpdatedPayload {
    #[serde(deserialize_with = "forward_compatible_vec")]
    pub providers: Vec<ServerProvider>,
}

/// `server.updateSettings` payload (`C/rpc.ts:821-829`). Success is the full
/// `ServerSettings` (opaque `Value`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerUpdateSettingsInput {
    /// `ServerSettingsPatch` (`C/settings.ts:1521-1681`); opaque.
    pub patch: Value,
    /// `ProviderInstanceMutation` (`C/providerInstance.ts:85-100`); opaque.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_mutation: Option<Value>,
}

/// `ServerSettingsOperation` (`C/settings.ts:1472-1486`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerSettingsOperation {
    #[serde(rename = "normalize")]
    Normalize,
    #[serde(rename = "check-exists")]
    CheckExists,
    #[serde(rename = "create-provider-instance")]
    CreateProviderInstance,
    #[serde(rename = "read-file")]
    ReadFile,
    #[serde(rename = "read-provider-history")]
    ReadProviderHistory,
    #[serde(rename = "read-project-settings")]
    ReadProjectSettings,
    #[serde(rename = "read-secret")]
    ReadSecret,
    #[serde(rename = "remove-secret")]
    RemoveSecret,
    #[serde(rename = "remove-stale-secret")]
    RemoveStaleSecret,
    #[serde(rename = "write-secret")]
    WriteSecret,
    #[serde(rename = "write-secret-provider")]
    WriteSecretProvider,
    #[serde(rename = "write-file")]
    WriteFile,
    #[serde(rename = "prepare-directory")]
    PrepareDirectory,
}

json_literal! {
    /// `_tag` of [`ServerSettingsError`].
    pub struct ServerSettingsErrorTag = str("ServerSettingsError");
}

/// `ServerSettingsError` (`C/settings.ts:1488-1508`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettingsError {
    #[serde(rename = "_tag")]
    pub tag: ServerSettingsErrorTag,
    pub settings_path: String,
    pub operation: ServerSettingsOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_variable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

json_literal! {
    /// `_tag` of [`KeybindingsConfigError`]; differs from the class name.
    pub struct KeybindingsConfigErrorTag = str("KeybindingsConfigParseError");
}

/// `KeybindingsConfigError` (`C/keybindings.ts:212-224`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingsConfigError {
    #[serde(rename = "_tag")]
    pub tag: KeybindingsConfigErrorTag,
    pub config_path: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

json_literal! {
    /// `_tag` of [`ProviderSetupError`].
    pub struct ProviderSetupErrorTag = str("ProviderSetupError");
}

/// `ProviderSetupError` (`C/providerSetup.ts:171-184`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSetupError {
    #[serde(rename = "_tag")]
    pub tag: ProviderSetupErrorTag,
    pub instance_id: ProviderInstanceId,
    pub operation: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

impl ServerSettingsError {
    pub fn new(settings_path: impl Into<String>, operation: ServerSettingsOperation) -> Self {
        Self {
            tag: ServerSettingsErrorTag,
            settings_path: settings_path.into(),
            operation,
            provider_instance_id: None,
            environment_variable: None,
            cause: None,
        }
    }
}

impl KeybindingsConfigError {
    pub fn new(config_path: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            tag: KeybindingsConfigErrorTag,
            config_path: config_path.into(),
            detail: detail.into(),
            cause: None,
        }
    }
}

impl ProviderSetupError {
    pub fn new(
        instance_id: ProviderInstanceId,
        operation: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            tag: ProviderSetupErrorTag,
            instance_id,
            operation: operation.into(),
            detail: detail.into(),
            cause: None,
        }
    }
}
