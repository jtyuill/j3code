//! Client activity reports (`C/background.ts`), payload of
//! `server.reportClientActivity` (ios-surface M14; void success).

use serde::{Deserialize, Serialize};

use super::ids::{EnvironmentId, IsoDateTime, ProviderInstanceId, ThreadId};

/// `BackgroundBooleanState` (`C/background.ts:13`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackgroundBooleanState {
    #[serde(rename = "true")]
    True,
    #[serde(rename = "false")]
    False,
    #[serde(rename = "unknown")]
    Unknown,
}

/// `BackgroundScope` (`C/background.ts:48-57`), tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BackgroundScope {
    #[serde(rename = "server-config")]
    ServerConfig,
    #[serde(rename = "provider-status", rename_all = "camelCase")]
    ProviderStatus {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instance_id: Option<ProviderInstanceId>,
    },
    #[serde(rename = "vcs-status")]
    VcsStatus { cwd: String },
    #[serde(rename = "git-refs")]
    GitRefs { cwd: String },
    #[serde(rename = "diagnostics")]
    Diagnostics,
    #[serde(rename = "thread", rename_all = "camelCase")]
    Thread { thread_id: ThreadId },
}

/// `ClientKind` (`C/background.ts:60`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientKind {
    #[serde(rename = "web")]
    Web,
    #[serde(rename = "desktop-renderer")]
    DesktopRenderer,
    #[serde(rename = "mobile")]
    Mobile,
    #[serde(rename = "unknown")]
    Unknown,
}

/// `ClientActivityReportInput.appState` literals (`C/background.ts:74`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientAppState {
    #[serde(rename = "active")]
    Active,
    #[serde(rename = "inactive")]
    Inactive,
    #[serde(rename = "background")]
    Background,
    #[serde(rename = "unknown")]
    Unknown,
}

/// `ClientActivityReportInput` (`C/background.ts:67-80`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientActivityReportInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_id: Option<EnvironmentId>,
    /// At most 128 chars.
    pub client_id: String,
    pub client_kind: ClientKind,
    pub visible: bool,
    pub focused: bool,
    pub recently_interacted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_state: Option<ClientAppState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low_power_mode: Option<BackgroundBooleanState>,
    pub scopes: Vec<BackgroundScope>,
    /// `Schema.Number`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<serde_json::Number>,
    pub observed_at: IsoDateTime,
}
