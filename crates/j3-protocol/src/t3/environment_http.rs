//! HTTP error bodies of the environment API (`C/environmentHttp.ts:84-251`)
//! and the `/ws` protocol-mismatch body (`apps/server/src/ws.ts:3348-3357`).

use serde::{Deserialize, Serialize};

use super::auth::AuthEnvironmentScope;

/// `EnvironmentRequestInvalidReason` (`C/environmentHttp.ts:84-90`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvironmentRequestInvalidReason {
    #[serde(rename = "invalid_scope")]
    InvalidScope,
    #[serde(rename = "scope_not_granted")]
    ScopeNotGranted,
    #[serde(rename = "invalid_command")]
    InvalidCommand,
    #[serde(rename = "invalid_history_cursor")]
    InvalidHistoryCursor,
    #[serde(rename = "thread_transcript_too_large")]
    ThreadTranscriptTooLarge,
}

/// `EnvironmentAuthInvalidReason` (`C/environmentHttp.ts:93-96`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvironmentAuthInvalidReason {
    #[serde(rename = "missing_credential")]
    MissingCredential,
    #[serde(rename = "invalid_credential")]
    InvalidCredential,
}

/// `DpopFailureReason` (`C/baseSchemas.ts:35-42`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DpopFailureReason {
    #[serde(rename = "time_window")]
    TimeWindow,
    #[serde(rename = "key_mismatch")]
    KeyMismatch,
    #[serde(rename = "request_mismatch")]
    RequestMismatch,
    #[serde(rename = "token_mismatch")]
    TokenMismatch,
    #[serde(rename = "replay")]
    Replay,
    #[serde(rename = "invalid_proof")]
    InvalidProof,
}

/// `EnvironmentOperationForbiddenReason` (`C/environmentHttp.ts:99-101`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvironmentOperationForbiddenReason {
    #[serde(rename = "current_session_revoke_not_allowed")]
    CurrentSessionRevokeNotAllowed,
}

/// `EnvironmentInternalErrorReason` (`C/environmentHttp.ts:104-128`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvironmentInternalErrorReason {
    #[serde(rename = "bootstrap_validation_failed")]
    BootstrapValidationFailed,
    #[serde(rename = "browser_session_issuance_failed")]
    BrowserSessionIssuanceFailed,
    #[serde(rename = "browser_session_cookie_failed")]
    BrowserSessionCookieFailed,
    #[serde(rename = "access_token_issuance_failed")]
    AccessTokenIssuanceFailed,
    #[serde(rename = "websocket_ticket_issuance_failed")]
    WebsocketTicketIssuanceFailed,
    #[serde(rename = "pairing_credential_issuance_failed")]
    PairingCredentialIssuanceFailed,
    #[serde(rename = "pairing_links_load_failed")]
    PairingLinksLoadFailed,
    #[serde(rename = "pairing_link_revoke_failed")]
    PairingLinkRevokeFailed,
    #[serde(rename = "client_sessions_load_failed")]
    ClientSessionsLoadFailed,
    #[serde(rename = "client_session_revoke_failed")]
    ClientSessionRevokeFailed,
    #[serde(rename = "project_snapshot_failed")]
    ProjectSnapshotFailed,
    #[serde(rename = "project_mutation_failed")]
    ProjectMutationFailed,
    #[serde(rename = "orchestration_snapshot_failed")]
    OrchestrationSnapshotFailed,
    #[serde(rename = "orchestration_thread_snapshot_failed")]
    OrchestrationThreadSnapshotFailed,
    #[serde(rename = "orchestration_thread_bounded_snapshot_failed")]
    OrchestrationThreadBoundedSnapshotFailed,
    #[serde(rename = "orchestration_thread_history_failed")]
    OrchestrationThreadHistoryFailed,
    #[serde(rename = "internal_error")]
    InternalError,
}

/// `EnvironmentResourceNotFoundReason` (`C/environmentHttp.ts:220`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnvironmentResourceNotFoundReason {
    #[serde(rename = "thread_not_found")]
    ThreadNotFound,
}

json_literal! {
    /// `_tag` of [`EnvironmentRequestInvalidError`].
    pub struct EnvironmentRequestInvalidErrorTag = str("EnvironmentRequestInvalidError");
}
json_literal! {
    /// `Schema.Literal("invalid_request")`.
    pub struct InvalidRequestCode = str("invalid_request");
}

/// `EnvironmentRequestInvalidError` (`C/environmentHttp.ts:131-147`), HTTP 400.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRequestInvalidError {
    #[serde(rename = "_tag")]
    pub tag: EnvironmentRequestInvalidErrorTag,
    pub code: InvalidRequestCode,
    pub reason: EnvironmentRequestInvalidReason,
    pub trace_id: String,
}

json_literal! {
    /// `_tag` of [`EnvironmentAuthInvalidError`].
    pub struct EnvironmentAuthInvalidErrorTag = str("EnvironmentAuthInvalidError");
}
json_literal! {
    /// `Schema.Literal("auth_invalid")`.
    pub struct AuthInvalidCode = str("auth_invalid");
}

/// `EnvironmentAuthInvalidError` (`C/environmentHttp.ts:149-166`), HTTP 401.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAuthInvalidError {
    #[serde(rename = "_tag")]
    pub tag: EnvironmentAuthInvalidErrorTag,
    pub code: AuthInvalidCode,
    pub reason: EnvironmentAuthInvalidReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpop_failure_reason: Option<DpopFailureReason>,
    pub trace_id: String,
}

json_literal! {
    /// `_tag` of [`EnvironmentScopeRequiredError`].
    pub struct EnvironmentScopeRequiredErrorTag = str("EnvironmentScopeRequiredError");
}
json_literal! {
    /// `Schema.Literal("insufficient_scope")`.
    pub struct InsufficientScopeCode = str("insufficient_scope");
}

/// `EnvironmentScopeRequiredError` (`C/environmentHttp.ts:168-185`), HTTP 403.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentScopeRequiredError {
    #[serde(rename = "_tag")]
    pub tag: EnvironmentScopeRequiredErrorTag,
    pub code: InsufficientScopeCode,
    pub required_scope: AuthEnvironmentScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_permission: Option<String>,
    pub trace_id: String,
}

json_literal! {
    /// `_tag` of [`EnvironmentOperationForbiddenError`].
    pub struct EnvironmentOperationForbiddenErrorTag = str("EnvironmentOperationForbiddenError");
}
json_literal! {
    /// `Schema.Literal("operation_forbidden")`.
    pub struct OperationForbiddenCode = str("operation_forbidden");
}

/// `EnvironmentOperationForbiddenError` (`C/environmentHttp.ts:187-202`), HTTP 403.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentOperationForbiddenError {
    #[serde(rename = "_tag")]
    pub tag: EnvironmentOperationForbiddenErrorTag,
    pub code: OperationForbiddenCode,
    pub reason: EnvironmentOperationForbiddenReason,
    pub trace_id: String,
}

json_literal! {
    /// `_tag` of [`EnvironmentInternalError`].
    pub struct EnvironmentInternalErrorTag = str("EnvironmentInternalError");
}
json_literal! {
    /// `Schema.Literal("internal_error")`.
    pub struct InternalErrorCode = str("internal_error");
}

/// `EnvironmentInternalError` (`C/environmentHttp.ts:204-218`), HTTP 500.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentInternalError {
    #[serde(rename = "_tag")]
    pub tag: EnvironmentInternalErrorTag,
    pub code: InternalErrorCode,
    pub reason: EnvironmentInternalErrorReason,
    pub trace_id: String,
}

json_literal! {
    /// `_tag` of [`EnvironmentResourceNotFoundError`].
    pub struct EnvironmentResourceNotFoundErrorTag = str("EnvironmentResourceNotFoundError");
}
json_literal! {
    /// `Schema.Literal("not_found")`.
    pub struct NotFoundCode = str("not_found");
}

/// `EnvironmentResourceNotFoundError` (`C/environmentHttp.ts:223-239`), HTTP 404.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentResourceNotFoundError {
    #[serde(rename = "_tag")]
    pub tag: EnvironmentResourceNotFoundErrorTag,
    pub code: NotFoundCode,
    pub reason: EnvironmentResourceNotFoundReason,
    pub trace_id: String,
}

json_literal! {
    /// `"orchestration_protocol_incompatible"` (`apps/server/src/ws.ts:3351`).
    pub struct OrchestrationProtocolIncompatibleCode = str("orchestration_protocol_incompatible");
}

/// HTTP 426 body for `/ws` upgrades without `orchestrationProtocol=2`
/// (`apps/server/src/ws.ts:3348-3357`); a plain object, not a tagged error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationProtocolIncompatibleBody {
    pub code: OrchestrationProtocolIncompatibleCode,
    pub message: String,
    pub orchestration_protocol_version: i64,
}

impl EnvironmentRequestInvalidError {
    pub fn new(reason: EnvironmentRequestInvalidReason, trace_id: impl Into<String>) -> Self {
        Self {
            tag: EnvironmentRequestInvalidErrorTag,
            code: InvalidRequestCode,
            reason,
            trace_id: trace_id.into(),
        }
    }
}

impl EnvironmentAuthInvalidError {
    pub fn new(reason: EnvironmentAuthInvalidReason, trace_id: impl Into<String>) -> Self {
        Self {
            tag: EnvironmentAuthInvalidErrorTag,
            code: AuthInvalidCode,
            reason,
            dpop_failure_reason: None,
            trace_id: trace_id.into(),
        }
    }
}

impl EnvironmentScopeRequiredError {
    pub fn new(required_scope: AuthEnvironmentScope, trace_id: impl Into<String>) -> Self {
        Self {
            tag: EnvironmentScopeRequiredErrorTag,
            code: InsufficientScopeCode,
            required_scope,
            required_permission: None,
            trace_id: trace_id.into(),
        }
    }
}

impl EnvironmentOperationForbiddenError {
    pub fn new(reason: EnvironmentOperationForbiddenReason, trace_id: impl Into<String>) -> Self {
        Self {
            tag: EnvironmentOperationForbiddenErrorTag,
            code: OperationForbiddenCode,
            reason,
            trace_id: trace_id.into(),
        }
    }
}

impl EnvironmentInternalError {
    pub fn new(reason: EnvironmentInternalErrorReason, trace_id: impl Into<String>) -> Self {
        Self {
            tag: EnvironmentInternalErrorTag,
            code: InternalErrorCode,
            reason,
            trace_id: trace_id.into(),
        }
    }
}

impl EnvironmentResourceNotFoundError {
    pub fn new(reason: EnvironmentResourceNotFoundReason, trace_id: impl Into<String>) -> Self {
        Self {
            tag: EnvironmentResourceNotFoundErrorTag,
            code: NotFoundCode,
            reason,
            trace_id: trace_id.into(),
        }
    }
}
