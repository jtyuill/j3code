//! j3 extensions to the t3 wire protocol (`docs/protocol/j3-extensions.md`).
//!
//! Everything j3 sends to clients stays inside t3's contract shapes, so the
//! iOS client decodes it unchanged. The extensions here are commands only:
//! extra `orchestration.dispatchCommand` variants (typed in
//! [`OrchestrationV2Command`](crate::t3::commands::OrchestrationV2Command))
//! that j3 clients send and t3 clients never do. Their `type`s carry the
//! `j3.` prefix so they can never collide with a t3 command.

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
