//! Thread search (`C/threadSearch.ts`), base of `orchestration.searchThreads`
//! (ios-surface M17). The V2 payload extension lives in `orchestration`.

use serde::{Deserialize, Serialize};

use super::ids::{IsoDateTime, MessageId, ProjectId, ThreadId};

/// `OrchestrationThreadSearchSource` (`C/threadSearch.ts:12`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrchestrationThreadSearchSource {
    #[serde(rename = "user")]
    User,
    #[serde(rename = "assistant")]
    Assistant,
}

/// `OrchestrationSearchThreadsInput` (`C/threadSearch.ts:20-26`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationSearchThreadsInput {
    /// Trimmed, 2..=200 chars.
    pub query: String,
    /// `Int` in 1..=50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
}

/// `OrchestrationThreadSearchMatch` (`C/threadSearch.ts:29-47`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationThreadSearchMatch {
    pub thread_id: ThreadId,
    pub project_id: ProjectId,
    pub source: OrchestrationThreadSearchSource,
    /// At most 240 chars.
    pub snippet: String,
    pub message_created_at: Option<IsoDateTime>,
    /// `Schema.Finite`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hits: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
}

/// `OrchestrationSearchThreadsResult` (`C/threadSearch.ts:50-52`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationSearchThreadsResult {
    pub matches: Vec<OrchestrationThreadSearchMatch>,
}

json_literal! {
    /// `_tag` of [`OrchestrationSearchThreadsError`].
    pub struct OrchestrationSearchThreadsErrorTag = str("OrchestrationSearchThreadsError");
}

/// `OrchestrationSearchThreadsError` (`C/threadSearch.ts:55-60`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrchestrationSearchThreadsError {
    #[serde(rename = "_tag")]
    pub tag: OrchestrationSearchThreadsErrorTag,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<serde_json::Value>,
}

impl OrchestrationSearchThreadsError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            tag: OrchestrationSearchThreadsErrorTag,
            message: message.into(),
            cause: None,
        }
    }
}
