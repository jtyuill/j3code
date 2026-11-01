//! Branded ids (`C/baseSchemas.ts:246-335`). Each is a
//! `TrimmedNonEmptyString` with a brand; on the wire a plain non-blank string.
//! These aliases only document intent.

pub type ThreadId = String;
pub type ProjectId = String;
pub type EnvironmentId = String;
pub type CommandId = String;
pub type EventId = String;
pub type MessageId = String;
pub type RunId = String;
pub type RunAttemptId = String;
pub type NodeId = String;
pub type ProviderSessionId = String;
pub type ProviderThreadId = String;
pub type ProviderTurnId = String;
pub type TurnItemId = String;
pub type RuntimeRequestId = String;
pub type ScheduledTaskId = String;
pub type CheckpointId = String;
pub type CheckpointScopeId = String;
pub type ContextHandoffId = String;
pub type ContextTransferId = String;
pub type RawEventId = String;
pub type PlanId = String;
pub type GuestMessageId = String;
/// `^[a-zA-Z][a-zA-Z0-9_-]*$`, at most 64 chars (`C/providerInstance.ts`).
pub type ProviderInstanceId = String;
/// Same constraint as [`ProviderInstanceId`]; known values in `C/model.ts`.
pub type ProviderDriverKind = String;
/// `/^[a-z0-9_-]+$/i`, at most 128 chars (`C/chatAttachment.ts:34-37`).
pub type ChatAttachmentId = String;
/// `IsoDateTime` (unvalidated string) or `Schema.DateTimeUtc` (ISO-8601 UTC).
pub type IsoDateTime = String;
