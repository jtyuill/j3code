//! j3 extensions to the t3 wire protocol (`docs/protocol/j3-extensions.md`).
//!
//! Everything j3 sends to clients on t3's own methods stays inside t3's
//! contract shapes, so the iOS client decodes it unchanged. The extensions
//! here are:
//!
//! - extra `orchestration.dispatchCommand` variants (typed in
//!   [`OrchestrationV2Command`](crate::t3::commands::OrchestrationV2Command))
//!   that j3 clients send and t3 clients never do. Their `type`s carry the
//!   `j3.` prefix so they can never collide with a t3 command.
//! - the `j3.getUsage` method ([`GET_USAGE`]) and the
//!   `j3.getSubagentTranscript` method ([`GET_SUBAGENT_TRANSCRIPT`]), which
//!   t3 clients never call.

use serde::{Deserialize, Serialize};

use crate::t3::ids::{CommandId, ThreadId};

/// `j3.goal.create`: start omp goal mode with an objective (omp `goal`
/// `op:"create"`). Refused while the thread already has an active or paused
/// goal, in plan mode, or when omp's `goal.enabled` setting is off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalCreateCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
    /// Non-blank.
    pub objective: String,
    /// Positive token budget for the goal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
}

/// `j3.goal.pause`, `j3.goal.resume`, `j3.goal.drop`: change the thread's
/// current goal (omp `goal` `op:"pause" | "resume" | "drop"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalCommand {
    pub command_id: CommandId,
    pub thread_id: ThreadId,
}

/// Method tag of the server host's provider usage limits. Payload `{}`;
/// succeeds with a [`UsageSnapshot`], fails with a [`UsageError`].
pub const GET_USAGE: &str = "j3.getUsage";

/// `omp usage --json` on the server host: the usage limits of every account
/// omp is logged into there. Only the fields j3 clients show are kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSnapshot {
    /// When omp produced the snapshot (Unix ms).
    pub generated_at: i64,
    #[serde(default)]
    pub reports: Vec<UsageReport>,
}

/// One account's limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    /// omp provider id, e.g. `anthropic`.
    pub provider: String,
    /// When omp last fetched this report from the provider (Unix ms); omp
    /// caches reports, so this can be older than the snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    #[serde(default)]
    pub limits: Vec<UsageLimit>,
    #[serde(default)]
    pub metadata: UsageAccount,
}

/// The account a report belongs to (omp's report `metadata`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAccount {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
}

/// One limit of an account, e.g. Anthropic's 5-hour window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimit {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<UsageWindow>,
    #[serde(default)]
    pub amount: UsageAmount,
    /// omp's status, e.g. `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// When the window resets (Unix ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
}

/// How much of a limit is used. `unit` is `percent` for every provider omp
/// knows today, so `used`/`limit` are percentages there.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageAmount {
    /// 0..=1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_fraction: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// `j3.getUsage` failure: omp could not be run, failed, or printed
/// something that is not a usage snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename = "J3UsageError")]
pub struct UsageError {
    pub message: String,
}

/// Method tag of one subagent's transcript, read incrementally from its omp
/// session file. Payload [`SubagentTranscriptInput`]; succeeds with a
/// [`SubagentTranscript`], fails with a [`SubagentTranscriptError`].
pub const GET_SUBAGENT_TRANSCRIPT: &str = "j3.getSubagentTranscript";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTranscriptInput {
    pub thread_id: ThreadId,
    /// The subagent's id (`Subagent.id`).
    pub subagent_id: String,
    /// Byte offset to continue from: the previous answer's `nextByte`.
    /// Omitted or `0` reads from the start.
    #[serde(default)]
    pub from_byte: u64,
}

/// Transcript entries appended since `fromByte`. Only whole lines are read,
/// so a line omp is still writing comes with the next call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTranscript {
    /// Pass as the next `fromByte`.
    pub next_byte: u64,
    /// The file is shorter than `fromByte` (rewritten): reading restarted at
    /// zero and the client must drop what it has.
    pub reset: bool,
    /// More whole lines follow `nextByte` already (the read was capped).
    pub more: bool,
    pub entries: Vec<TranscriptEntry>,
}

/// One displayable part of a subagent message, in transcript order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TranscriptEntry {
    /// The assignment and any steering messages.
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    Thinking {
        text: String,
    },
    /// `arguments` is the call's JSON arguments, compact.
    #[serde(rename_all = "camelCase")]
    ToolCall {
        id: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intent: Option<String>,
        arguments: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        tool_call_id: String,
        name: String,
        text: String,
        is_error: bool,
    },
    /// A failed model request (the assistant message's `errorMessage`).
    Error {
        text: String,
    },
}

/// `j3.getSubagentTranscript` failure: unknown thread, no omp session yet,
/// or the transcript file cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename = "J3SubagentTranscriptError")]
pub struct SubagentTranscriptError {
    pub message: String,
}
