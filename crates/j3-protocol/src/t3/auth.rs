//! Auth descriptor and session state (`C/auth.ts`), served by
//! `GET /api/auth/session` and embedded in `ServerConfig`.

use serde::{Deserialize, Serialize};

use super::environment::forward_compatible_vec_option;
use super::ids::IsoDateTime;

/// `ServerAuthPolicy` (`C/auth.ts:12-17`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerAuthPolicy {
    #[serde(rename = "desktop-managed-local")]
    DesktopManagedLocal,
    #[serde(rename = "loopback-browser")]
    LoopbackBrowser,
    #[serde(rename = "remote-reachable")]
    RemoteReachable,
    #[serde(rename = "unsafe-no-auth")]
    UnsafeNoAuth,
}

/// `ServerAuthBootstrapMethod` (`C/auth.ts:20`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerAuthBootstrapMethod {
    #[serde(rename = "desktop-bootstrap")]
    DesktopBootstrap,
    #[serde(rename = "one-time-token")]
    OneTimeToken,
}

/// `ServerAuthSessionMethod` (`C/auth.ts:23-27`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerAuthSessionMethod {
    #[serde(rename = "browser-session-cookie")]
    BrowserSessionCookie,
    #[serde(rename = "bearer-access-token")]
    BearerAccessToken,
    #[serde(rename = "dpop-access-token")]
    DpopAccessToken,
}

/// `AuthEnvironmentScope` (`C/auth.ts:30-66`). One literal union serves both
/// `scopes` and `permissions`; `review:write` is decode-only legacy
/// (`AuthGrantScope` excludes it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthEnvironmentScope {
    #[serde(rename = "orchestration:read")]
    OrchestrationRead,
    #[serde(rename = "orchestration:operate")]
    OrchestrationOperate,
    #[serde(rename = "settings:write")]
    SettingsWrite,
    #[serde(rename = "providers:manage")]
    ProvidersManage,
    #[serde(rename = "environment:maintain")]
    EnvironmentMaintain,
    #[serde(rename = "preview:operate")]
    PreviewOperate,
    #[serde(rename = "diagnostics:read")]
    DiagnosticsRead,
    #[serde(rename = "terminal:read")]
    TerminalRead,
    #[serde(rename = "terminal:operate")]
    TerminalOperate,
    #[serde(rename = "filesystem:read")]
    FilesystemRead,
    #[serde(rename = "filesystem:write")]
    FilesystemWrite,
    #[serde(rename = "review:write")]
    ReviewWrite,
    #[serde(rename = "source-control:write")]
    SourceControlWrite,
    #[serde(rename = "access:read")]
    AccessRead,
    #[serde(rename = "access:write")]
    AccessWrite,
    #[serde(rename = "relay:read")]
    RelayRead,
    #[serde(rename = "relay:write")]
    RelayWrite,
}

json_literal! {
    /// `Schema.Literal("environment:maintain")` (`C/auth.ts:180`).
    pub struct EnvironmentMaintainScope = str("environment:maintain");
}

/// `ServerAuthDescriptor` (`C/auth.ts:174-181`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerAuthDescriptor {
    pub policy: ServerAuthPolicy,
    pub bootstrap_methods: Vec<ServerAuthBootstrapMethod>,
    pub session_methods: Vec<ServerAuthSessionMethod>,
    pub session_cookie_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_update_scope: Option<EnvironmentMaintainScope>,
}

/// `AuthSessionState` (`C/auth.ts:392-399`); body of `GET /api/auth/session`
/// (ios-surface H3). When `permissions` is present it is authoritative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionState {
    pub authenticated: bool,
    pub auth: ServerAuthDescriptor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<AuthEnvironmentScope>>,
    /// `ForwardCompatibleArray`: unknown scopes are dropped on decode.
    #[serde(
        default,
        deserialize_with = "forward_compatible_vec_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub permissions: Option<Vec<AuthEnvironmentScope>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_method: Option<ServerAuthSessionMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<IsoDateTime>,
}

json_literal! {
    /// `_tag` of [`EnvironmentAuthorizationError`].
    pub struct EnvironmentAuthorizationErrorTag = str("EnvironmentAuthorizationError");
}

/// `EnvironmentAuthorizationError` (`C/auth.ts:340-347`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAuthorizationError {
    #[serde(rename = "_tag")]
    pub tag: EnvironmentAuthorizationErrorTag,
    pub message: String,
    pub required_scope: AuthEnvironmentScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_permission: Option<String>,
}

impl EnvironmentAuthorizationError {
    pub fn new(message: impl Into<String>, required_scope: AuthEnvironmentScope) -> Self {
        Self {
            tag: EnvironmentAuthorizationErrorTag,
            message: message.into(),
            required_scope,
            required_permission: None,
        }
    }
}
