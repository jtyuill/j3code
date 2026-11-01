//! Presentation snapshots of turn items for the thread feed: which items
//! show, and the one-line summary and expandable detail of tool rows.

use j3_protocol::t3::orchestration::{
    TurnItem, TurnItemKind, TurnItemStatus, UserMessageInputIntent,
};
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Longest single-line summary before it is cut with an ellipsis.
const SUMMARY_CHARS: usize = 160;

/// Whether the feed skips this item. Queued messages show in the queue
/// panel until they start; stop requests show as their result row; kinds
/// j3 never emits are dropped.
pub fn is_hidden(item: &TurnItem) -> bool {
    match &item.kind {
        TurnItemKind::UserMessage(message) => {
            message.input_intent == UserMessageInputIntent::QueuedTurn
                && item.status == TurnItemStatus::Pending
        }
        TurnItemKind::RunInterruptRequest(_) | TurnItemKind::Unknown(_) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolIcon {
    Terminal,
    File,
    Search,
    Web,
    Tool,
}

/// The collapsed row and expanded body of a tool item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSummary {
    pub icon: ToolIcon,
    /// What ran, e.g. `bash` or the tool name.
    pub title: String,
    /// One line describing the call.
    pub subtitle: String,
    /// Monospace body shown when expanded.
    pub detail: Option<String>,
    pub failed: bool,
    pub running: bool,
}

/// First line of `text`, cut to [`SUMMARY_CHARS`] characters.
pub fn one_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or_default().trim();
    let multiline = text.trim().lines().nth(1).is_some();
    let mut out: String = line.chars().take(SUMMARY_CHARS).collect();
    if multiline || line.chars().count() > SUMMARY_CHARS {
        out.push('…');
    }
    out
}

/// A compact rendering of a dynamic tool's JSON input: the string value of
/// a lone field, else `key: value` pairs, else the JSON itself.
fn input_summary(input: &Value) -> String {
    match input {
        Value::Object(map) if map.len() == 1 => match map.values().next() {
            Some(Value::String(s)) => one_line(s),
            Some(other) => one_line(&other.to_string()),
            None => String::new(),
        },
        Value::Object(map) => one_line(
            &map.iter()
                .map(|(key, value)| match value {
                    Value::String(s) => format!("{key}: {}", one_line(s)),
                    other => format!("{key}: {other}"),
                })
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Value::String(s) => one_line(s),
        Value::Null => String::new(),
        other => one_line(&other.to_string()),
    }
}

fn pretty(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn join_nonempty(parts: impl IntoIterator<Item = String>) -> Option<String> {
    let joined = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!joined.is_empty()).then_some(joined)
}

/// The tool row for `item`, or `None` if it is not a tool item.
pub fn tool_summary(item: &TurnItem) -> Option<ToolSummary> {
    let running = matches!(
        item.status,
        TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting
    );
    let status_failed = item.status == TurnItemStatus::Failed;
    let title = |fallback: &str| {
        item.title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| fallback.to_owned())
    };
    let summary = match &item.kind {
        TurnItemKind::CommandExecution(command) => ToolSummary {
            icon: ToolIcon::Terminal,
            title: title("bash"),
            subtitle: one_line(&command.input),
            detail: join_nonempty([
                format!("$ {}", command.input.trim_end()),
                command.output.clone().unwrap_or_default(),
                command
                    .exit_code
                    .filter(|code| *code != 0)
                    .map(|code| format!("exit code {code}"))
                    .unwrap_or_default(),
            ]),
            failed: status_failed
                || command.exit_code.is_some_and(|code| code != 0)
                || command.output_indicates_failure == Some(true),
            running,
        },
        TurnItemKind::FileChange(change) => {
            let counts = match (change.additions, change.deletions) {
                (None, None) => String::new(),
                (additions, deletions) => {
                    format!("  +{} −{}", additions.unwrap_or(0), deletions.unwrap_or(0))
                }
            };
            ToolSummary {
                icon: ToolIcon::File,
                title: title("edit"),
                subtitle: format!("{}{counts}", change.file_name),
                detail: change
                    .diff_str
                    .clone()
                    .or_else(|| change.new_str.clone())
                    .filter(|d| !d.trim().is_empty()),
                failed: status_failed,
                running,
            }
        }
        TurnItemKind::FileSearch(search) => ToolSummary {
            icon: ToolIcon::Search,
            title: title("search"),
            subtitle: search.pattern.as_deref().map(one_line).unwrap_or_default(),
            detail: search.results.as_ref().and_then(|results| {
                join_nonempty([results
                    .iter()
                    .map(|result| {
                        let mut line = result.file_name.clone();
                        if let Some(n) = result.line {
                            line.push_str(&format!(":{n}"));
                        }
                        if let Some(preview) = &result.preview {
                            line.push_str(&format!("  {}", preview.trim()));
                        }
                        line
                    })
                    .collect::<Vec<_>>()
                    .join("\n")])
            }),
            failed: status_failed,
            running,
        },
        TurnItemKind::WebSearch(search) => ToolSummary {
            icon: ToolIcon::Web,
            title: title("web search"),
            subtitle: search
                .patterns
                .as_ref()
                .map(|p| one_line(&p.join(", ")))
                .unwrap_or_default(),
            detail: search.results.as_ref().and_then(|results| {
                join_nonempty([results
                    .iter()
                    .map(|result| {
                        [
                            result.title.clone(),
                            result.url.clone(),
                            result.snippet.clone(),
                        ]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join(" — ")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")])
            }),
            failed: status_failed,
            running,
        },
        TurnItemKind::DynamicTool(tool) => ToolSummary {
            icon: ToolIcon::Tool,
            title: tool
                .tool_name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| title("tool")),
            subtitle: input_summary(&tool.input),
            detail: join_nonempty([
                pretty(&tool.input),
                tool.output
                    .as_ref()
                    .and_then(|o| o.as_ref())
                    .map(pretty)
                    .unwrap_or_default(),
            ]),
            failed: status_failed,
            running,
        },
        _ => return None,
    };
    Some(summary)
}

/// A compact age like `now`, `5m`, `3h`, `2d`, `6w` between an ISO-8601
/// timestamp and `now`; `None` if the timestamp does not parse.
pub fn relative_age(iso: &str, now: OffsetDateTime) -> Option<String> {
    let at = OffsetDateTime::parse(iso, &Rfc3339).ok()?;
    let seconds = (now - at).whole_seconds().max(0);
    Some(match seconds {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        86_400..604_800 => format!("{}d", seconds / 86_400),
        _ => format!("{}w", seconds / 604_800),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(status: &str, body: Value) -> TurnItem {
        let mut value = json!({
            "id":"ti","threadId":"T","runId":"r","nodeId":"n","providerThreadId":null,"providerTurnId":null,
            "nativeItemRef":null,"parentItemId":null,"ordinal":1,"status":status,"title":null,
            "startedAt":null,"completedAt":null,"updatedAt":"2026-11-01T12:00:00.000Z"
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn hides_pending_queued_messages_and_stop_requests() {
        let user = |intent: &str, status: &str| {
            item(
                status,
                json!({"type":"user_message","createdBy":"user","creationSource":"mobile","messageId":"m",
                       "inputIntent":intent,"text":"hi","attachments":[]}),
            )
        };
        assert!(is_hidden(&user("queued_turn", "pending")));
        assert!(!is_hidden(&user("queued_turn", "completed")));
        assert!(!is_hidden(&user("turn_start", "completed")));
        assert!(is_hidden(&item(
            "completed",
            json!({"type":"run_interrupt_request","message":"stop"})
        )));
        assert!(!is_hidden(&item(
            "completed",
            json!({"type":"run_interrupt_result","message":"stopped"})
        )));
        assert!(is_hidden(&item(
            "completed",
            json!({"type":"proposed_plan","markdown":"x"})
        )));
    }

    #[test]
    fn summarizes_commands_with_output_and_failure() {
        let summary = tool_summary(&item(
            "completed",
            json!({"type":"command_execution","input":"sleep 30\necho done","output":"done\n","exitCode":1}),
        ))
        .unwrap();
        assert_eq!(summary.icon, ToolIcon::Terminal);
        assert_eq!(summary.title, "bash");
        assert_eq!(summary.subtitle, "sleep 30…");
        assert_eq!(
            summary.detail.as_deref(),
            Some("$ sleep 30\necho done\n\ndone\n\n\nexit code 1")
        );
        assert!(summary.failed);
        assert!(!summary.running);

        let running = tool_summary(&item(
            "running",
            json!({"type":"command_execution","input":"ls"}),
        ))
        .unwrap();
        assert!(running.running);
        assert!(!running.failed);
        assert_eq!(running.detail.as_deref(), Some("$ ls"));
    }

    #[test]
    fn summarizes_files_searches_and_dynamic_tools() {
        let edit = tool_summary(&item(
            "completed",
            json!({"type":"file_change","fileName":"src/a.rs","additions":3,"deletions":1,"diffStr":"@@ -1 +1 @@"}),
        ))
        .unwrap();
        assert_eq!(edit.subtitle, "src/a.rs  +3 −1");
        assert_eq!(edit.detail.as_deref(), Some("@@ -1 +1 @@"));

        let search = tool_summary(&item(
            "completed",
            json!({"type":"file_search","pattern":"fn main","results":[{"fileName":"a.rs","line":3,"preview":"  fn main() {"}]}),
        ))
        .unwrap();
        assert_eq!(search.subtitle, "fn main");
        assert_eq!(search.detail.as_deref(), Some("a.rs:3  fn main() {"));

        let web = tool_summary(&item(
            "completed",
            json!({"type":"web_search","patterns":["gpui","zed"],"results":[{"title":"Zed","url":"https://zed.dev"}]}),
        ))
        .unwrap();
        assert_eq!(web.subtitle, "gpui, zed");
        assert_eq!(web.detail.as_deref(), Some("Zed — https://zed.dev"));

        let ask = tool_summary(&item(
            "completed",
            json!({"type":"dynamic_tool","toolName":"read","input":{"path":"/tmp/x"},"output":"contents"}),
        ))
        .unwrap();
        assert_eq!(ask.title, "read");
        assert_eq!(ask.subtitle, "/tmp/x");
        assert_eq!(
            ask.detail.as_deref(),
            Some("{\n  \"path\": \"/tmp/x\"\n}\n\ncontents")
        );

        let pairs = tool_summary(&item(
            "completed",
            json!({"type":"dynamic_tool","toolName":null,"title":"grep","input":{"a":1,"b":"two"}}),
        ))
        .unwrap();
        assert_eq!(pairs.title, "grep");
        assert_eq!(pairs.subtitle, "a: 1, b: two");

        assert!(
            tool_summary(&item(
                "completed",
                json!({"type":"reasoning","text":"x","streaming":false})
            ))
            .is_none()
        );
    }

    #[test]
    fn ages_are_compact() {
        let now = OffsetDateTime::parse("2026-11-01T12:00:00.000Z", &Rfc3339).unwrap();
        let age = |iso: &str| relative_age(iso, now);
        assert_eq!(age("2026-11-01T11:59:30.000Z").as_deref(), Some("now"));
        assert_eq!(age("2026-11-01T11:55:00.000Z").as_deref(), Some("5m"));
        assert_eq!(age("2026-11-01T09:00:00.000Z").as_deref(), Some("3h"));
        assert_eq!(age("2026-10-30T12:00:00.000Z").as_deref(), Some("2d"));
        assert_eq!(age("2026-09-01T12:00:00.000Z").as_deref(), Some("8w"));
        assert_eq!(age("2026-11-02T12:00:00.000Z").as_deref(), Some("now"));
        assert_eq!(age("garbage"), None);
    }
}
