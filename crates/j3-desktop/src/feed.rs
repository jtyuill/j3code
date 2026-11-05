//! Presentation snapshots of turn items for the thread feed: which items
//! show, and how a tool call reads, modelled on omp's own TUI renderers: an
//! icon, a title (`Bash`, `Read`, `Edit`, …), what it acted on, dim meta
//! (`+3 −1`, `12 matches · 3 files`, `2.1s`) and a body (the command and its
//! output, the diff, the matches) of which a few lines show until expanded.

use j3_core::engine::model::tool_output_images;
use j3_protocol::t3::orchestration::{
    CommandExecutionItem, DynamicToolItem, FileChangeItem, FileSearchItem, TurnItem, TurnItemKind,
    TurnItemStatus, UserMessageInputIntent, WebSearchItem,
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

/// What a tool did, which picks its icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolIcon {
    Terminal,
    Read,
    Edit,
    Write,
    Delete,
    Grep,
    Glob,
    Web,
    Eval,
    Task,
    Todo,
    Wait,
    Message,
    Device,
    Process,
    Lsp,
    Ast,
    Debug,
    Browser,
    Github,
    Memory,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Done,
    Failed,
}

/// How a meta part is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Added,
    Removed,
    Failed,
}

/// The expandable body under a tool's header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolBody {
    /// `$ command`, then its output (bash).
    Command { command: String, output: String },
    /// A diff: omp's `+12|text` / `-12|text` / ` 12|text` lines, or unified;
    /// its code coloured as `language`.
    Diff {
        text: String,
        language: &'static str,
    },
    /// File text, numbered from `first_line`, coloured as `language`.
    Code {
        text: String,
        first_line: usize,
        language: &'static str,
    },
    /// Result lines drawn as a tree (search hits, files, sources).
    List(Vec<String>),
    /// Labelled monospace sections, such as a script and its output.
    Sections(Vec<(Option<String>, String)>),
}

impl ToolBody {
    /// The body's line count, as the expander counts it.
    pub fn lines(&self) -> usize {
        match self {
            Self::Command { output, .. } => output.lines().count(),
            Self::Diff { text, .. } | Self::Code { text, .. } => text.lines().count(),
            Self::List(lines) => lines.len(),
            Self::Sections(sections) => sections.iter().map(|(_, text)| text.lines().count()).sum(),
        }
    }
}

/// omp's numbered file text (`12:line` rows, one contiguous range) as code
/// in `language` starting at its first number; `None` for anything else.
fn numbered(text: &str, language: &'static str) -> Option<ToolBody> {
    let mut first_line = None;
    let mut lines = Vec::new();
    for (ix, row) in text.lines().enumerate() {
        let (number, line) = row.split_once(':')?;
        let number: usize = number.trim().parse().ok()?;
        let first = *first_line.get_or_insert(number);
        if number != first + ix {
            return None;
        }
        lines.push(line);
    }
    Some(ToolBody::Code {
        text: lines.join("\n"),
        first_line: first_line?,
        language,
    })
}

/// The highlighter language of a read path, which may end in a selector
/// (`src/main.rs:10-20`, `notes.md:raw`).
fn read_language(path: &str) -> &'static str {
    let name = path.rfind('/').map_or(0, |ix| ix + 1);
    let end = path[name..].find(':').map_or(path.len(), |ix| name + ix);
    crate::syntax::language(&path[..end])
}

/// One tool row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolView {
    pub icon: ToolIcon,
    /// omp's name for the tool, e.g. `Bash`, `Read`, `Edit`.
    pub title: String,
    /// What it acted on, one line: the command, path, pattern or query.
    pub description: String,
    /// The agent's intent for the call (`Reading the plan`), when given.
    pub intent: Option<String>,
    /// The description is prose (an intent), not code: no monospace.
    pub prose: bool,
    pub meta: Vec<(String, Tone)>,
    pub body: Option<ToolBody>,
    /// Body lines shown while collapsed; 0 shows only the header.
    pub preview: usize,
    pub status: ToolStatus,
    /// Images in the tool's output (`tool-output-image` indices `0..images`).
    pub images: usize,
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

/// `n` with its noun, singular for one: `1 match`, `3 matches`.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn muted(text: impl Into<String>) -> (String, Tone) {
    (text.into(), Tone::Muted)
}

fn pretty(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

fn str_arg<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// A compact rendering of a tool's JSON input: the string value of a lone
/// field, else `key=value` pairs, else the JSON itself. The intent (`i`)
/// is left out; it shows on its own.
fn input_summary(input: &Value) -> String {
    match input {
        Value::Object(map) => {
            let fields: Vec<(&String, &Value)> =
                map.iter().filter(|(key, _)| *key != "i").collect();
            match fields.as_slice() {
                [] => String::new(),
                [(_, Value::String(s))] => one_line(s),
                fields => one_line(
                    &fields
                        .iter()
                        .map(|(key, value)| match value {
                            Value::String(s) => format!("{key}={}", one_line(s)),
                            other => format!("{key}={other}"),
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            }
        }
        Value::String(s) => one_line(s),
        Value::Null => String::new(),
        other => one_line(&other.to_string()),
    }
}

/// `snake_case` tool name as a title: `ast_grep` → `Ast grep`.
fn humanize(name: &str) -> String {
    let words = name.replace(['_', '-'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Tool".to_owned(),
    }
}

/// The file a hashline `edit` input names first (`[PATH#TAG]`).
fn hashline_path(input: &str) -> Option<&str> {
    input.lines().find_map(|line| {
        let inner = line.trim().strip_prefix('[')?.strip_suffix(']')?;
        let (path, tag) = inner.rsplit_once('#')?;
        (tag.len() == 4 && tag.chars().all(|c| c.is_ascii_hexdigit())).then_some(path.trim())
    })
}

/// A run time like `340ms`, `2.1s`, `1m 05s`, from the item's start and end.
fn duration(item: &TurnItem) -> Option<String> {
    let parse = |iso: &Option<String>| OffsetDateTime::parse(iso.as_deref()?, &Rfc3339).ok();
    let elapsed = parse(&item.completed_at)? - parse(&item.started_at)?;
    let ms = elapsed.whole_milliseconds().max(0);
    Some(match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..60_000 => format!("{:.1}s", ms as f64 / 1_000.),
        _ => format!("{}m {:02}s", ms / 60_000, ms % 60_000 / 1_000),
    })
}

/// The tool row for `item`, or `None` if it is not a tool item.
pub fn tool_view(item: &TurnItem) -> Option<ToolView> {
    let status = match item.status {
        TurnItemStatus::Pending | TurnItemStatus::Running | TurnItemStatus::Waiting => {
            ToolStatus::Running
        }
        TurnItemStatus::Failed => ToolStatus::Failed,
        _ => ToolStatus::Done,
    };
    let mut view = match &item.kind {
        TurnItemKind::CommandExecution(command) => command_view(command),
        TurnItemKind::FileChange(change) => file_change_view(change),
        TurnItemKind::FileSearch(search) => file_search_view(search),
        TurnItemKind::WebSearch(search) => web_search_view(search),
        TurnItemKind::DynamicTool(tool) => dynamic_view(tool),
        _ => return None,
    };
    view.status = match (status, &item.kind) {
        (ToolStatus::Done, TurnItemKind::CommandExecution(command))
            if command.exit_code.is_some_and(|code| code != 0)
                || command.output_indicates_failure == Some(true) =>
        {
            ToolStatus::Failed
        }
        _ => status,
    };
    // The server titles a row with the call's intent, else the tool name.
    view.intent = item
        .title
        .clone()
        .filter(|title| !title.trim().is_empty() && !title.eq_ignore_ascii_case(&view.title))
        .filter(|title| match &item.kind {
            TurnItemKind::DynamicTool(tool) => tool.tool_name.as_deref() != Some(title.as_str()),
            _ => !matches!(
                title.as_str(),
                "bash" | "edit" | "write" | "grep" | "glob" | "find"
            ),
        });
    if view.status != ToolStatus::Running
        && let Some(elapsed) = duration(item)
    {
        view.meta.push(muted(elapsed));
    }
    // A command shows itself in its box; the header says what it is for.
    if matches!(item.kind, TurnItemKind::CommandExecution(_))
        && let Some(intent) = view.intent.take()
    {
        view.description = intent;
        view.prose = true;
    }
    if let TurnItemKind::DynamicTool(tool) = &item.kind {
        view.images = output_images(tool);
    }
    Some(view)
}

fn base(icon: ToolIcon, title: &str, description: String) -> ToolView {
    ToolView {
        icon,
        title: title.to_owned(),
        description,
        intent: None,
        prose: false,
        meta: Vec::new(),
        body: None,
        preview: 0,
        status: ToolStatus::Done,
        images: 0,
    }
}

fn command_view(command: &CommandExecutionItem) -> ToolView {
    let mut view = base(ToolIcon::Terminal, "Bash", one_line(&command.input));
    if let Some(code) = command.exit_code.filter(|code| *code != 0) {
        view.meta.push((format!("exit {code}"), Tone::Failed));
    }
    view.body = Some(ToolBody::Command {
        command: command.input.trim_end().to_owned(),
        output: strip_notices(command.output.as_deref().unwrap_or_default()).to_owned(),
    });
    view.preview = 6;
    view
}

/// The output without the notices omp appends for the model (`Wall time:
/// 5.26 seconds`, `Command exited with code 1`); the row shows the run time
/// and exit code itself.
fn strip_notices(output: &str) -> &str {
    let mut output = output.trim_end();
    loop {
        let (rest, last) = output.rsplit_once('\n').unwrap_or(("", output));
        if !(last.starts_with("Wall time: ") || last.starts_with("Command exited with code ")) {
            return output;
        }
        output = rest.trim_end();
    }
}

fn file_change_view(change: &FileChangeItem) -> ToolView {
    // Servers before the scheme check sent writes to `agent://` and the
    // like as file changes.
    if change.file_name.contains("://") {
        return write_view(&change.file_name, change.new_str.as_deref(), None);
    }
    let diff = change.diff_str.as_deref().filter(|d| !d.trim().is_empty());
    match (diff, change.new_str.as_deref()) {
        (None, Some(content)) => {
            let mut view = base(ToolIcon::Write, "Write", change.file_name.clone());
            view.meta
                .push(muted(count(content.lines().count(), "line", "lines")));
            view.body = Some(ToolBody::Code {
                text: content.trim_end().to_owned(),
                first_line: 1,
                language: crate::syntax::language(&change.file_name),
            });
            view.preview = 8;
            view
        }
        _ => {
            let mut view = base(ToolIcon::Edit, "Edit", change.file_name.clone());
            if let (Some(added), Some(removed)) = (change.additions, change.deletions) {
                view.meta.push((format!("+{added}"), Tone::Added));
                view.meta.push((format!("−{removed}"), Tone::Removed));
            }
            view.body = diff.map(|diff| ToolBody::Diff {
                text: diff.trim_end().to_owned(),
                language: crate::syntax::language(&change.file_name),
            });
            view.preview = 12;
            view
        }
    }
}

/// omp's `write`: a file, or an internal URL — a message to a peer agent
/// (`agent://`), a tool device call (`xd://`), a job command (`proc://`).
fn write_view(path: &str, content: Option<&str>, output: Option<String>) -> ToolView {
    match path.split_once("://") {
        Some(("agent", peer)) => {
            let mut view = base(ToolIcon::Message, "Message", format!("→ {peer}"));
            view.body = content.map(|content| ToolBody::Sections(vec![(None, content.to_owned())]));
            view.preview = 3;
            view
        }
        Some(("xd", device)) => {
            let device = device.strip_prefix("mcp__").unwrap_or(device);
            let mut view = base(ToolIcon::Device, "Tool", device.replace("__", " › "));
            let sections: Vec<(Option<String>, String)> = [
                content.map(|c| (Some("Input".to_owned()), c.to_owned())),
                output.map(|o| (Some("Output".to_owned()), o)),
            ]
            .into_iter()
            .flatten()
            .collect();
            view.body = (!sections.is_empty()).then_some(ToolBody::Sections(sections));
            view
        }
        Some(("proc", target)) => base(ToolIcon::Process, "Proc", target.to_owned()),
        _ => {
            let mut view = base(ToolIcon::Write, "Write", path.to_owned());
            if let Some(content) = content {
                view.meta
                    .push(muted(count(content.lines().count(), "line", "lines")));
                view.body = Some(ToolBody::Code {
                    text: content.to_owned(),
                    first_line: 1,
                    language: crate::syntax::language(path),
                });
                view.preview = 8;
            }
            view
        }
    }
}

fn file_search_view(search: &FileSearchItem) -> ToolView {
    let pattern = search.pattern.as_deref().map(one_line).unwrap_or_default();
    let results = search.results.as_deref().unwrap_or_default();
    let grep = results.iter().any(|r| r.line.is_some())
        || (results.is_empty() && !pattern.contains(['*', '?', '{']));
    let mut view = if grep {
        base(ToolIcon::Grep, "Grep", pattern)
    } else {
        base(ToolIcon::Glob, "Glob", pattern)
    };
    if search.results.is_some() {
        let mut files: Vec<&str> = results.iter().map(|r| r.file_name.as_str()).collect();
        files.dedup();
        view.meta.push(muted(if grep {
            format!(
                "{} · {}",
                count(results.len(), "match", "matches"),
                count(files.len(), "file", "files")
            )
        } else {
            count(results.len(), "file", "files")
        }));
    }
    if !results.is_empty() {
        view.body = Some(ToolBody::List(
            results
                .iter()
                .map(|result| {
                    let mut line = result.file_name.clone();
                    if let Some(n) = result.line {
                        line.push_str(&format!(":{n}"));
                    }
                    if let Some(preview) = result.preview.as_deref().filter(|p| !p.is_empty()) {
                        line.push_str(&format!("  {preview}"));
                    }
                    line
                })
                .collect(),
        ));
        view.preview = 4;
    }
    view
}

fn web_search_view(search: &WebSearchItem) -> ToolView {
    let query = search
        .patterns
        .as_ref()
        .map(|p| one_line(&p.join(", ")))
        .unwrap_or_default();
    let mut view = base(ToolIcon::Web, "Web Search", query);
    if let Some(results) = &search.results {
        view.meta
            .push(muted(count(results.len(), "source", "sources")));
        view.body = Some(ToolBody::List(
            results
                .iter()
                .map(|result| {
                    let host = result.url.as_deref().map(|url| {
                        url.split("://")
                            .nth(1)
                            .unwrap_or(url)
                            .split('/')
                            .next()
                            .unwrap_or(url)
                            .to_owned()
                    });
                    match (&result.title, host) {
                        (Some(title), Some(host)) => format!("{title} ({host})"),
                        (Some(title), None) => title.clone(),
                        (None, Some(host)) => host,
                        (None, None) => String::new(),
                    }
                })
                .collect(),
        ));
        view.preview = 4;
    }
    view
}

/// The output text of a dynamic tool, if any. Output with images is t3's
/// block list; its text blocks are the text.
fn output_text(tool: &DynamicToolItem) -> Option<String> {
    let output = tool.output.as_ref()?.as_ref()?;
    let text = match output {
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        other => pretty(other),
    };
    (!text.trim().is_empty()).then_some(text)
}

/// How many images a dynamic tool's output shows.
fn output_images(tool: &DynamicToolItem) -> usize {
    tool.output
        .as_ref()
        .and_then(Option::as_ref)
        .map_or(0, |output| tool_output_images(output).count())
}

/// Drops the `[PATH#TAG]` header omp's `read` puts above a file.
fn strip_hashline_header(text: &str) -> &str {
    match text.split_once('\n') {
        Some((first, rest)) if hashline_path(first).is_some() => rest,
        _ => text,
    }
}

fn dynamic_view(tool: &DynamicToolItem) -> ToolView {
    let name = tool.tool_name.as_deref().unwrap_or("tool");
    let input = &tool.input;
    let output = output_text(tool);
    let path = str_arg(input, "path");
    match name {
        "read" => {
            let path = path.unwrap_or_default();
            let icon = match path.split_once("://").map(|(scheme, _)| scheme) {
                Some("http" | "https") => ToolIcon::Web,
                Some("agent" | "history") => ToolIcon::Task,
                Some("proc") => ToolIcon::Process,
                Some("xd" | "mcp") => ToolIcon::Device,
                _ => ToolIcon::Read,
            };
            let mut view = base(icon, "Read", path.to_owned());
            if let Some(output) = output.filter(|_| output_images(tool) == 0) {
                // Only a file's text is code; pages and agent output are not.
                let language = if matches!(icon, ToolIcon::Read) {
                    read_language(path)
                } else {
                    "text"
                };
                let text = strip_hashline_header(&output).trim_end();
                view.meta
                    .push(muted(count(text.lines().count(), "line", "lines")));
                view.body = Some(
                    numbered(text, language)
                        .unwrap_or_else(|| ToolBody::Sections(vec![(None, text.to_owned())])),
                );
            }
            view
        }
        "write" => write_view(path.unwrap_or_default(), str_arg(input, "content"), output),
        "edit" => {
            let script = str_arg(input, "input").unwrap_or_default();
            let target = path.or_else(|| hashline_path(script)).unwrap_or_default();
            let mut view = base(ToolIcon::Edit, "Edit", target.to_owned());
            view.body = Some(ToolBody::Sections(
                [
                    Some((None, script.to_owned())),
                    output.map(|o| (Some("Result".to_owned()), o)),
                ]
                .into_iter()
                .flatten()
                .collect(),
            ));
            view
        }
        "eval" => {
            let code = str_arg(input, "code")
                .or_else(|| str_arg(input, "input"))
                .unwrap_or_default();
            let description = str_arg(input, "title")
                .map(one_line)
                .unwrap_or_else(|| one_line(code));
            let mut view = base(ToolIcon::Eval, "Eval", description);
            if let Some(language) = str_arg(input, "language") {
                view.meta.push(muted(language));
            }
            view.body = Some(ToolBody::Sections(
                [
                    (!code.is_empty()).then(|| (None, code.to_owned())),
                    output.map(|o| (Some("Output".to_owned()), o)),
                ]
                .into_iter()
                .flatten()
                .collect(),
            ));
            view.preview = 6;
            view
        }
        "task" => {
            let tasks = input.get("tasks").and_then(Value::as_array);
            let names: Vec<&str> = tasks
                .into_iter()
                .flatten()
                .filter_map(|task| str_arg(task, "name").or_else(|| str_arg(task, "id")))
                .collect();
            let description = match (names.is_empty(), tasks.map_or(0, Vec::len)) {
                (false, _) => names.join(", "),
                (true, 0) => input_summary(input),
                (true, n) => count(n, "agent", "agents"),
            };
            let mut view = base(ToolIcon::Task, "Task", description);
            if let Some(agent) = str_arg(input, "agent") {
                view.meta.push(muted(agent));
            }
            view.body = output.map(|o| ToolBody::Sections(vec![(None, o)]));
            view
        }
        "todo" | "todo_write" => {
            let mut view = base(
                ToolIcon::Todo,
                "Todo",
                str_arg(input, "op").unwrap_or("update").to_owned(),
            );
            view.body = output.map(|o| ToolBody::Sections(vec![(None, o)]));
            view
        }
        _ => {
            let (icon, title) = match name {
                "wait" => (ToolIcon::Wait, "Wait".to_owned()),
                "fetch" | "web_fetch" => (ToolIcon::Web, "Fetch".to_owned()),
                "lsp" => (ToolIcon::Lsp, "LSP".to_owned()),
                "ast_grep" => (ToolIcon::Ast, "AST Grep".to_owned()),
                "ast_edit" => (ToolIcon::Ast, "AST Edit".to_owned()),
                "debug" => (ToolIcon::Debug, "Debug".to_owned()),
                "browser" | "puppeteer" => (ToolIcon::Browser, "Browser".to_owned()),
                "computer" => (ToolIcon::Browser, "Computer".to_owned()),
                "recall" | "retain" | "reflect" => (ToolIcon::Memory, humanize(name)),
                "delete" | "rm" => (ToolIcon::Delete, "Delete".to_owned()),
                name if name.starts_with("gh") || name == "github" => {
                    (ToolIcon::Github, "GitHub".to_owned())
                }
                name if name.starts_with("mcp__") => (
                    ToolIcon::Device,
                    name.trim_start_matches("mcp__").replace("__", " › "),
                ),
                name => (ToolIcon::Tool, humanize(name)),
            };
            let mut view = base(icon, &title, input_summary(input));
            let input_text = match input {
                Value::Object(map) if map.keys().all(|key| key == "i") => None,
                Value::Null => None,
                other => Some(pretty(other)),
            };
            let sections: Vec<(Option<String>, String)> = [
                input_text.map(|i| (Some("Input".to_owned()), i)),
                output.map(|o| (Some("Output".to_owned()), o)),
            ]
            .into_iter()
            .flatten()
            .collect();
            view.body = (!sections.is_empty()).then_some(ToolBody::Sections(sections));
            view
        }
    }
}

/// One line of a diff, split for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffLine<'a> {
    pub kind: DiffKind,
    /// The line number omp prints (`+12|`), if any.
    pub number: Option<&'a str>,
    pub text: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Added,
    Removed,
    Context,
    Hunk,
}

/// Splits one line of an omp (`+12|text`) or unified (`+text`, `@@ …`) diff.
pub fn diff_line(line: &str) -> DiffLine<'_> {
    let (kind, rest) = match line.chars().next() {
        Some('+') => (DiffKind::Added, &line[1..]),
        Some('-') => (DiffKind::Removed, &line[1..]),
        Some('@') if line.starts_with("@@") => {
            return DiffLine {
                kind: DiffKind::Hunk,
                number: None,
                text: line,
            };
        }
        Some(' ') => (DiffKind::Context, &line[1..]),
        _ => (DiffKind::Context, line),
    };
    match rest.split_once('|') {
        Some((number, text))
            if !number.is_empty() && number.trim().chars().all(|c| c.is_ascii_digit()) =>
        {
            DiffLine {
                kind,
                number: Some(number.trim()),
                text,
            }
        }
        _ => DiffLine {
            kind,
            number: None,
            text: rest,
        },
    }
}

/// A compact age like `now`, `5m`, `3h`, `2d`, `6w` between an ISO-8601
/// timestamp and `now`; `None` if the timestamp does not parse.
pub fn relative_age(iso: &str, now: OffsetDateTime) -> Option<String> {
    let at = OffsetDateTime::parse(iso, &Rfc3339).ok()?;
    Some(age_since(at, now))
}

/// [`relative_age`] of a parsed time.
pub fn age_since(at: OffsetDateTime, now: OffsetDateTime) -> String {
    let seconds = (now - at).whole_seconds().max(0);
    match seconds {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        86_400..604_800 => format!("{}d", seconds / 86_400),
        _ => format!("{}w", seconds / 604_800),
    }
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
    fn commands_fail_on_a_nonzero_exit_and_time_their_run() {
        let mut failed = item(
            "completed",
            json!({"type":"command_execution","input":"sleep 30\necho done",
                   "output":"done\n\nWall time: 2.10 seconds\n","exitCode":1}),
        );
        failed.title = Some("Waiting for the build".into());
        failed.started_at = Some("2026-11-01T12:00:00.000Z".into());
        failed.completed_at = Some("2026-11-01T12:00:02.140Z".into());
        let view = tool_view(&failed).unwrap();
        assert_eq!(
            (view.icon, view.title.as_str()),
            (ToolIcon::Terminal, "Bash")
        );
        // The box shows the command; the header says what it is for.
        assert_eq!(
            (view.description.as_str(), view.prose),
            ("Waiting for the build", true)
        );
        assert_eq!(view.status, ToolStatus::Failed);
        assert_eq!(
            view.meta,
            [
                ("exit 1".to_owned(), Tone::Failed),
                ("2.1s".to_owned(), Tone::Muted)
            ]
        );
        assert_eq!(
            view.body,
            Some(ToolBody::Command {
                command: "sleep 30\necho done".into(),
                output: "done".into()
            })
        );

        let mut running = item("running", json!({"type":"command_execution","input":"ls"}));
        running.title = Some("bash".into());
        let view = tool_view(&running).unwrap();
        assert_eq!(view.status, ToolStatus::Running);
        assert_eq!(
            (view.description.as_str(), view.prose),
            ("ls", false),
            "the bare tool name is not an intent"
        );
    }

    #[test]
    fn edits_writes_and_searches_read_like_omp() {
        let edit = tool_view(&item(
            "completed",
            json!({"type":"file_change","fileName":"src/a.rs","additions":3,"deletions":1,"diffStr":"+1|x"}),
        ))
        .unwrap();
        assert_eq!(
            (edit.title.as_str(), edit.description.as_str()),
            ("Edit", "src/a.rs")
        );
        assert_eq!(
            edit.meta,
            [
                ("+3".to_owned(), Tone::Added),
                ("−1".to_owned(), Tone::Removed)
            ]
        );
        assert_eq!(
            edit.body,
            Some(ToolBody::Diff {
                text: "+1|x".into(),
                language: "rust"
            })
        );

        let write = tool_view(&item(
            "completed",
            json!({"type":"file_change","fileName":"b.txt","newStr":"one\ntwo\n"}),
        ))
        .unwrap();
        assert_eq!(
            (write.title.as_str(), write.icon),
            ("Write", ToolIcon::Write)
        );
        assert_eq!(write.meta, [("2 lines".to_owned(), Tone::Muted)]);

        let grep = tool_view(&item(
            "completed",
            json!({"type":"file_search","pattern":"fn main","results":[
                {"fileName":"a.rs","line":3,"preview":"fn main() {"},
                {"fileName":"a.rs","line":9,"preview":"fn main2()"}]}),
        ))
        .unwrap();
        assert_eq!((grep.title.as_str(), grep.icon), ("Grep", ToolIcon::Grep));
        assert_eq!(grep.meta, [("2 matches · 1 file".to_owned(), Tone::Muted)]);
        assert_eq!(
            grep.body,
            Some(ToolBody::List(vec![
                "a.rs:3  fn main() {".into(),
                "a.rs:9  fn main2()".into()
            ]))
        );

        let glob = tool_view(&item(
            "running",
            json!({"type":"file_search","pattern":"src/**/*.rs"}),
        ))
        .unwrap();
        assert_eq!(glob.title, "Glob");
    }

    #[test]
    fn dynamic_tools_get_their_omp_titles() {
        let read = tool_view(&item(
            "completed",
            json!({"type":"dynamic_tool","toolName":"read","input":{"path":"PLAN.md:7-8"},
                   "output":"[PLAN.md#C34C]\n7:# plan\n8:"}),
        ))
        .unwrap();
        assert_eq!(
            (read.title.as_str(), read.description.as_str()),
            ("Read", "PLAN.md:7-8")
        );
        assert_eq!(read.meta, [("2 lines".to_owned(), Tone::Muted)]);
        assert_eq!(
            read.body,
            Some(ToolBody::Code {
                text: "# plan\n".into(),
                first_line: 7,
                language: "markdown"
            })
        );
        // Not one numbered range: shown as it is.
        let listing = tool_view(&item(
            "completed",
            json!({"type":"dynamic_tool","toolName":"read","input":{"path":"src/"},
                   "output":"main.rs\nlib.rs"}),
        ))
        .unwrap();
        assert_eq!(
            listing.body,
            Some(ToolBody::Sections(vec![(None, "main.rs\nlib.rs".into())]))
        );

        // An edit from a server that predates hashline mapping.
        let edit = tool_view(&item(
            "completed",
            json!({"type":"dynamic_tool","toolName":"edit","input":{"input":"[README.md#AF62]\nPUT 1:\n+x"}}),
        ))
        .unwrap();
        assert_eq!(
            (edit.title.as_str(), edit.description.as_str()),
            ("Edit", "README.md")
        );

        let message = tool_view(&item(
            "completed",
            json!({"type":"dynamic_tool","toolName":"write","input":{"path":"agent://Hexmap","content":"hi"}}),
        ))
        .unwrap();
        assert_eq!(
            (
                message.icon,
                message.title.as_str(),
                message.description.as_str()
            ),
            (ToolIcon::Message, "Message", "→ Hexmap")
        );

        let eval = tool_view(&item(
            "completed",
            json!({"type":"dynamic_tool","toolName":"eval","input":{"code":"1+1","language":"js","title":"Adding"},
                   "output":"2"}),
        ))
        .unwrap();
        assert_eq!(
            (eval.title.as_str(), eval.description.as_str()),
            ("Eval", "Adding")
        );
        assert_eq!(eval.meta, [("js".to_owned(), Tone::Muted)]);

        let other = tool_view(&item(
            "completed",
            json!({"type":"dynamic_tool","toolName":"search_tool_bm25","input":{"i":"Finding","query":"x","limit":2}}),
        ))
        .unwrap();
        assert_eq!(other.title, "Search tool bm25");
        assert_eq!(other.description, "query=x, limit=2");
    }

    #[test]
    fn diff_lines_split_omp_and_unified_forms() {
        assert_eq!(
            diff_line("+13|    },"),
            DiffLine {
                kind: DiffKind::Added,
                number: Some("13"),
                text: "    },"
            }
        );
        assert_eq!(diff_line(" 3|  x").kind, DiffKind::Context);
        assert_eq!(diff_line(" 3|  x").number, Some("3"));
        assert_eq!(
            diff_line("-old"),
            DiffLine {
                kind: DiffKind::Removed,
                number: None,
                text: "old"
            }
        );
        assert_eq!(diff_line("@@ -1 +1 @@").kind, DiffKind::Hunk);
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
