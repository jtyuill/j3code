//! `j3.getSubagentTranscript`: a subagent's omp session file, read
//! incrementally and cut down to [`TranscriptEntry`]s.
//!
//! omp writes subagent `<id>` of session `<stem>.jsonl` to
//! `<stem>/<id>.jsonl`. Reading the file directly works whether or not the
//! thread's omp process is running, and after server restarts.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use j3_protocol::j3::{SubagentTranscript, TranscriptEntry};
use serde_json::Value;

use crate::omp::omp_rpc::{AgentMessage, AssistantContent, MessageContent, UserContent};

/// Most bytes one call reads; the client asks again for the rest.
const MAX_READ: u64 = 4 << 20;

/// Session file of subagent `subagent_id` of the thread session
/// `session_file`; `None` for ids that are not a plain file name.
pub fn subagent_file(session_file: &Path, subagent_id: &str) -> Option<PathBuf> {
    let valid = !subagent_id.is_empty()
        && subagent_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !subagent_id.starts_with('.');
    valid.then(|| {
        session_file
            .with_extension("")
            .join(format!("{subagent_id}.jsonl"))
    })
}

/// Whole lines of `path` from `from_byte` (restarting at zero when the file
/// is shorter), up to [`MAX_READ`] bytes.
pub fn read(path: &Path, from_byte: u64) -> std::io::Result<SubagentTranscript> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let reset = from_byte > len;
    let start = if reset { 0 } else { from_byte };
    let want = (len - start).min(MAX_READ);
    let mut bytes = Vec::with_capacity(want as usize);
    file.seek(SeekFrom::Start(start))?;
    file.take(want).read_to_end(&mut bytes)?;
    let whole = bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |ix| ix + 1);
    let mut entries = Vec::new();
    for line in bytes[..whole].split(|&b| b == b'\n') {
        if let Ok(value) = serde_json::from_slice::<Value>(line) {
            push_line(value, &mut entries);
        }
    }
    let next_byte = start + whole as u64;
    Ok(SubagentTranscript {
        next_byte,
        reset,
        more: want < len - start && whole > 0,
        entries,
    })
}

/// The entries of one session line; only `message` lines show.
fn push_line(mut line: Value, entries: &mut Vec<TranscriptEntry>) {
    if line.get("type").and_then(Value::as_str) != Some("message") {
        return;
    }
    let Some(message) = line.get_mut("message").map(Value::take) else {
        return;
    };
    let Ok(message) = AgentMessage::from_value(message) else {
        return;
    };
    match message {
        AgentMessage::User(message) => push_text(entries, content_text(message.content), |text| {
            TranscriptEntry::User { text }
        }),
        AgentMessage::Assistant(message) => {
            for block in message.content.into_iter().flatten() {
                match block {
                    AssistantContent::Text(block) => {
                        push_text(entries, block.text, |text| TranscriptEntry::Assistant {
                            text,
                        });
                    }
                    AssistantContent::Thinking(block) => {
                        push_text(entries, block.thinking, |text| TranscriptEntry::Thinking {
                            text,
                        });
                    }
                    AssistantContent::ToolCall(call) => entries.push(TranscriptEntry::ToolCall {
                        id: call.id.unwrap_or_default(),
                        name: call.name.unwrap_or_default(),
                        intent: call.intent.filter(|intent| !intent.trim().is_empty()),
                        arguments: call
                            .arguments
                            .map(|arguments| Value::Object(arguments).to_string())
                            .unwrap_or_default(),
                    }),
                    _ => {}
                }
            }
            push_text(entries, message.error_message, |text| {
                TranscriptEntry::Error { text }
            });
        }
        AgentMessage::ToolResult(result) => entries.push(TranscriptEntry::ToolResult {
            tool_call_id: result.tool_call_id.unwrap_or_default(),
            name: result.tool_name.unwrap_or_default(),
            text: blocks_text(result.content.unwrap_or_default()),
            is_error: result.is_error.unwrap_or(false),
        }),
        _ => {}
    }
}

fn push_text(
    entries: &mut Vec<TranscriptEntry>,
    text: Option<String>,
    make: impl FnOnce(String) -> TranscriptEntry,
) {
    if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
        entries.push(make(text));
    }
}

fn content_text(content: Option<MessageContent>) -> Option<String> {
    match content? {
        MessageContent::String(text) => Some(text),
        MessageContent::UserContentList(blocks) => Some(blocks_text(blocks)),
    }
}

/// The text blocks, one per paragraph; images are left out.
fn blocks_text(blocks: Vec<UserContent>) -> String {
    blocks
        .into_iter()
        .filter_map(|block| match block {
            UserContent::Text(block) => block.text,
            UserContent::Image(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn subagent_file_sits_next_to_the_session_and_refuses_paths() {
        let session = Path::new("/s/2026-11-01T23_abc.jsonl");
        assert_eq!(
            subagent_file(session, "CivilRat"),
            Some(PathBuf::from("/s/2026-11-01T23_abc/CivilRat.jsonl"))
        );
        assert_eq!(subagent_file(session, "../x"), None);
        assert_eq!(subagent_file(session, ".."), None);
        assert_eq!(subagent_file(session, ""), None);
    }

    #[test]
    fn reads_whole_lines_incrementally_and_resets_when_rewritten() {
        let dir = std::env::temp_dir().join(format!("j3-transcript-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Sub.jsonl");
        let user = r#"{"type":"session","id":"x"}
{"type":"message","message":{"role":"user","content":"Reply PONG."}}
"#;
        let call = r#"{"type":"message","message":{"role":"assistant","content":[{"type":"thinking","thinking":"easy"},{"type":"toolCall","id":"c1","name":"bash","intent":"Echoing","arguments":{"command":"echo PONG"}}]}}
{"type":"message","message":{"role":"toolResult","toolCallId":"c1","toolName":"bash","isError":false,"content":[{"type":"text","text":"PONG"}]}}
{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"PONG"}]"#;
        std::fs::write(&path, format!("{user}{call}")).unwrap();

        let first = read(&path, 0).unwrap();
        assert!(!first.reset && !first.more);
        assert_eq!(
            first.entries,
            vec![
                TranscriptEntry::User {
                    text: "Reply PONG.".into()
                },
                TranscriptEntry::Thinking {
                    text: "easy".into()
                },
                TranscriptEntry::ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    intent: Some("Echoing".into()),
                    arguments: r#"{"command":"echo PONG"}"#.into(),
                },
                TranscriptEntry::ToolResult {
                    tool_call_id: "c1".into(),
                    name: "bash".into(),
                    text: "PONG".into(),
                    is_error: false,
                },
            ]
        );

        // The half-written last line arrives once it is complete.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"}}\n")
            .unwrap();
        let second = read(&path, first.next_byte).unwrap();
        assert_eq!(
            second.entries,
            vec![TranscriptEntry::Assistant {
                text: "PONG".into()
            }]
        );
        assert_eq!(read(&path, second.next_byte).unwrap().entries, vec![]);

        // A rewritten (shorter) file starts over.
        std::fs::write(&path, user).unwrap();
        let reset = read(&path, second.next_byte).unwrap();
        assert!(reset.reset);
        assert_eq!(reset.entries.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
