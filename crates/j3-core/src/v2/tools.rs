//! omp tool calls and results → v2 items (`docs/protocol/wire.md` §4.4,
//! §7). Shared by the live engine and the session replay, so a thread
//! looks the same whether its items were streamed or rebuilt.

use j3_protocol::v2::entities::{
    BackgroundJob, JobState, SearchMatch, SubagentItem, TodoPhase, TodoStatus, TodoTask,
    ToolDetail, ToolItem, ToolOutput, WebResult,
};
use serde_json::Value;

/// The phases of a `todo` tool result's `details.phases`
/// (`[{name, tasks: [{content, status}]}]`).
pub fn todo_phases(phases: &Value) -> Vec<TodoPhase> {
    phases
        .as_array()
        .into_iter()
        .flatten()
        .map(|phase| TodoPhase {
            name: phase
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            tasks: phase
                .get("tasks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|task| TodoTask {
                    text: task
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    status: task
                        .get("status")
                        .and_then(|status| serde_json::from_value(status.clone()).ok())
                        .unwrap_or(TodoStatus::Pending),
                })
                .collect(),
        })
        .collect()
}

/// The subagents a `task` tool result spawned (`details.progress`), still
/// running.
pub fn spawned_subagents(result: &Value) -> Vec<SubagentItem> {
    result
        .pointer("/details/progress")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|progress| {
            Some(SubagentItem {
                subagent_id: progress.get("id")?.as_str()?.to_owned(),
                agent: progress
                    .get("agent")
                    .and_then(Value::as_str)
                    .unwrap_or("task")
                    .to_owned(),
                description: progress
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
                prompt: progress
                    .get("assignment")
                    .or_else(|| progress.get("task"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                model: None,
                progress: None,
                result: None,
            })
        })
        .collect()
}

/// Most search matches kept on one item.
pub const MAX_SEARCH_MATCHES: usize = 200;
/// Most characters kept of a search match line or web result field.
const MAX_FIELD: usize = 300;

/// A tool call as omp sends it.
pub fn tool_item(call_id: &str, name: &str, args: &Value) -> ToolItem {
    ToolItem {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        intent: arg_str(args, "i").map(str::to_owned),
        input: args.clone(),
        output: None,
        detail: detail_of(name, args),
    }
}

/// The structured view of a tool call, before its result.
pub fn detail_of(name: &str, args: &Value) -> Option<ToolDetail> {
    Some(match name {
        "bash" => ToolDetail::Command {
            command: arg_str(args, "command").unwrap_or_default().to_owned(),
        },
        "edit" | "write" => ToolDetail::FileChange {
            path: changed_file(name, args)?.to_owned(),
            diff: None,
            additions: None,
            deletions: None,
            content: (name == "write")
                .then(|| args.get("content").and_then(Value::as_str))
                .flatten()
                .map(str::to_owned),
        },
        // `glob` and `find` take their pattern as `path`.
        "grep" | "find" | "glob" => ToolDetail::Search {
            pattern: arg_str(args, "pattern")
                .or_else(|| arg_str(args, "path"))
                .unwrap_or_default()
                .to_owned(),
            matches: None,
        },
        "web_search" => ToolDetail::WebSearch {
            query: arg_str(args, "query").unwrap_or_default().to_owned(),
            results: None,
        },
        _ => return None,
    })
}

/// Applies omp's tool result (`{content, details}`) to `item`: output text,
/// exit code, background job, and the detail fields the result carries.
/// Images are not read here; see [`result_images`].
pub fn apply_result(item: &mut ToolItem, result: &Value) {
    let text = result_text(result).unwrap_or_default();
    let details = result.get("details");
    let field = |key: &str| details.and_then(|details| details.get(key));
    match &mut item.detail {
        Some(ToolDetail::FileChange {
            diff,
            additions,
            deletions,
            ..
        }) => {
            if let Some(text) = field("diff")
                .and_then(Value::as_str)
                .filter(|d| !d.is_empty())
            {
                let (added, removed) = diff_counts(text);
                *diff = Some(text.to_owned());
                *additions = Some(added);
                *deletions = Some(removed);
            }
        }
        Some(ToolDetail::Search { matches, .. }) => {
            let mut found = grep_matches(&text);
            if found.is_empty()
                && let Some(files) = field("files").and_then(Value::as_array)
            {
                found = files
                    .iter()
                    .filter_map(Value::as_str)
                    .take(MAX_SEARCH_MATCHES)
                    .map(|path| SearchMatch {
                        path: path.to_owned(),
                        line: None,
                        text: None,
                    })
                    .collect();
            }
            *matches = Some(found);
        }
        Some(ToolDetail::WebSearch { results, .. }) => {
            if let Some(sources) = details
                .and_then(|details| details.pointer("/response/sources"))
                .and_then(Value::as_array)
            {
                let get = |source: &Value, key: &str| {
                    source
                        .get(key)
                        .and_then(Value::as_str)
                        .map(|text| truncate(text.trim(), MAX_FIELD))
                };
                *results = Some(
                    sources
                        .iter()
                        .map(|source| WebResult {
                            title: get(source, "title").unwrap_or_default(),
                            url: get(source, "url").unwrap_or_default(),
                            snippet: get(source, "snippet"),
                        })
                        .collect(),
                );
            }
        }
        _ => {}
    }
    let background = field("async").and_then(|job| {
        Some(BackgroundJob {
            job_id: job.get("jobId")?.as_str()?.to_owned(),
            state: job_state(job.get("state")?.as_str()?)?,
        })
    });
    let output = item.output.get_or_insert_with(ToolOutput::default);
    output.text = text;
    output.exit_code = field("exitCode").and_then(Value::as_i64);
    output.background = background;
}

/// A background job of `item` ended: its final output replaces the
/// "backgrounded" text.
pub fn finish_job(item: &mut ToolItem, state: JobState, text: String) {
    let output = item.output.get_or_insert_with(ToolOutput::default);
    output.text = text;
    if let Some(job) = &mut output.background {
        job.state = state;
    }
}

/// omp's background job `status`/`state` strings.
pub fn job_state(state: &str) -> Option<JobState> {
    Some(match state {
        "running" | "pending" => JobState::Running,
        "completed" => JobState::Completed,
        "failed" => JobState::Failed,
        "cancelled" | "aborted" => JobState::Cancelled,
        _ => return None,
    })
}

/// An image block of a tool result, to keep as an attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultImage {
    pub mime_type: String,
    /// Base64, as omp sends it.
    pub data: String,
}

/// Most images kept from one tool result.
pub const MAX_RESULT_IMAGES: usize = 8;

/// The raster image blocks (`{type:"image", data, mimeType}`) of a tool
/// result, in order: at most [`MAX_RESULT_IMAGES`], none larger than the
/// image upload limit.
pub fn result_images(result: &Value) -> Vec<ResultImage> {
    const MAX_BASE64: usize = (crate::attachments::MAX_IMAGE_BYTES as usize).div_ceil(3) * 4;
    let Some(content) = result.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("image"))
        .filter_map(|block| {
            let mime_type = block.get("mimeType")?.as_str()?.to_ascii_lowercase();
            let data = block.get("data")?.as_str()?;
            (crate::attachments::SUPPORTED_IMAGE_MIME_TYPES.contains(&mime_type.as_str())
                && !data.is_empty()
                && data.len() <= MAX_BASE64)
                .then(|| ResultImage {
                    mime_type,
                    data: data.to_owned(),
                })
        })
        .take(MAX_RESULT_IMAGES)
        .collect()
}

/// Text of a tool result `{content:[{type:"text",text}]}`.
pub fn result_text(result: &Value) -> Option<String> {
    let content = result.get("content")?.as_array()?;
    let text: Vec<&str> = content
        .iter()
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then(|| text.concat())
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

/// The file an `edit` or `write` call changes: its `path`, or for a
/// hashline `edit` the first `[PATH#TAG]` header. None for internal URLs
/// (`agent://`, `xd://`, …) and web pages.
fn changed_file<'a>(name: &str, args: &'a Value) -> Option<&'a str> {
    let path = arg_str(args, "path")
        .or_else(|| (name == "edit").then(|| arg_str(args, "input").and_then(hashline_path))?)?;
    (!path.contains("://")).then(|| path.trim())
}

/// The file a hashline `edit` input names first, from its `[PATH#TAG]`
/// header.
fn hashline_path(input: &str) -> Option<&str> {
    input.lines().find_map(|line| {
        let inner = line.trim().strip_prefix('[')?.strip_suffix(']')?;
        let (path, tag) = inner.rsplit_once('#')?;
        let path = path.trim();
        (tag.len() == 4 && tag.chars().all(|c| c.is_ascii_hexdigit()) && !path.is_empty())
            .then_some(path)
    })
}

/// Added and removed lines of an omp edit diff (`+12|text`, `-12|text`,
/// context ` 12|text`).
fn diff_counts(diff: &str) -> (u64, u64) {
    diff.lines().fold((0, 0), |(added, removed), line| {
        if line.starts_with('+') && !line.starts_with("+++") {
            (added + 1, removed)
        } else if line.starts_with('-') && !line.starts_with("---") {
            (added, removed + 1)
        } else {
            (added, removed)
        }
    })
}

/// The matching lines of omp's `grep` output: `# dir/` and `## file#TAG`
/// headers (`# file#TAG` alone when one file matched), then numbered lines
/// where `*` marks a match (`*15:text`) and a space a context line
/// (` 14:text`).
fn grep_matches(text: &str) -> Vec<SearchMatch> {
    let mut dir = "";
    let mut file = None::<String>;
    let mut matches = Vec::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            let name = name.rsplit_once('#').map_or(name, |(name, _)| name);
            file = Some(format!("{dir}{}", name.trim()));
        } else if let Some(path) = line.strip_prefix("# ") {
            match path.trim().rsplit_once('#') {
                Some((name, _)) => {
                    dir = "";
                    file = Some(name.to_owned());
                }
                None => {
                    dir = path.trim();
                    file = None;
                }
            }
        } else if let Some(rest) = line.strip_prefix('*')
            && let Some((number, text)) = rest.split_once(':')
            && let Ok(number) = number.trim().parse::<u64>()
            && let Some(file) = &file
        {
            matches.push(SearchMatch {
                path: file.clone(),
                line: Some(number),
                text: Some(truncate(text.trim(), MAX_FIELD)),
            });
            if matches.len() == MAX_SEARCH_MATCHES {
                break;
            }
        }
    }
    matches
}

fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hashline_edit_is_a_file_change_and_internal_urls_are_not() {
        let edit = tool_item(
            "c1",
            "edit",
            &json!({"i": "Fixing", "input": "[src/a.rs#1A2B]\nPUT 3:\n+x\n[src/b.rs#FFFF]\nCUT 1"}),
        );
        assert_eq!(edit.intent.as_deref(), Some("Fixing"));
        assert!(matches!(
            edit.detail,
            Some(ToolDetail::FileChange { ref path, .. }) if path == "src/a.rs"
        ));
        let internal = tool_item(
            "c2",
            "write",
            &json!({"path": "local://notes.md", "content": "x"}),
        );
        assert_eq!(internal.detail, None);
    }

    #[test]
    fn results_fill_diff_counts_matches_exit_code_and_background_job() {
        let mut edit = tool_item("c1", "edit", &json!({"path": "a.rs"}));
        apply_result(
            &mut edit,
            &json!({"content": [{"type": "text", "text": "ok"}],
                    "details": {"diff": "+1|a\n+2|b\n-3|c\n 4|d"}}),
        );
        assert!(matches!(
            edit.detail,
            Some(ToolDetail::FileChange {
                additions: Some(2),
                deletions: Some(1),
                ..
            })
        ));

        let mut grep = tool_item("c2", "grep", &json!({"pattern": "x"}));
        apply_result(
            &mut grep,
            &json!({"content": [{"type": "text", "text": "# src/\n## a.rs#1A2B\n 3:ctx\n*4:  hit\n"}]}),
        );
        let Some(ToolDetail::Search {
            matches: Some(matches),
            ..
        }) = &grep.detail
        else {
            panic!("no matches");
        };
        assert_eq!(matches[0].path, "src/a.rs");
        assert_eq!(matches[0].line, Some(4));
        assert_eq!(matches[0].text.as_deref(), Some("hit"));

        let mut bash = tool_item("c3", "bash", &json!({"command": "sleep 99"}));
        apply_result(
            &mut bash,
            &json!({"content": [{"type": "text", "text": "Backgrounded as job bg_1"}],
                    "details": {"async": {"state": "running", "jobId": "bg_1", "type": "bash"}}}),
        );
        let output = bash.output.as_ref().unwrap();
        assert_eq!(
            output.background,
            Some(BackgroundJob {
                job_id: "bg_1".into(),
                state: JobState::Running
            })
        );
        finish_job(&mut bash, JobState::Completed, "done".into());
        let output = bash.output.unwrap();
        assert_eq!(output.text, "done");
        assert_eq!(output.background.unwrap().state, JobState::Completed);
    }
}
