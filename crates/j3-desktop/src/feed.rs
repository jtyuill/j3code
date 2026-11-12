//! Presentation snapshots of turn items for the thread feed: which items
//! show, and how a tool call reads, modelled on omp's own TUI renderers: an
//! icon, a title (`Bash`, `Read`, `Edit`, …), what it acted on, dim meta
//! (`+3 −1`, `12 matches · 3 files`, `2.1s`) and a body (the command and its
//! output, the diff, the matches). Only a running command's output and a
//! diff show unasked, a few lines of them; every other body waits for a click.

use j3_client::entities::{
    Answer, CompactionItem, Goal, GoalState, Item, ItemKind, ItemStatus, SearchMatch, StopItem,
    TodoItem, TodoStatus, ToolDetail, ToolItem, ToolOutput, WebResult,
};
use serde_json::Value;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Longest single-line summary before it is cut with an ellipsis.
const SUMMARY_CHARS: usize = 160;

/// Lines of a running command's output and of a diff shown before the row is expanded.
const COMMAND_PREVIEW: usize = 6;
const DIFF_PREVIEW: usize = 12;

/// Whether the feed skips this item: kinds this build doesn't know. (Queued
/// messages are not items; they show in the queue panel until they start.)
pub fn is_hidden(item: &Item) -> bool {
    matches!(item.kind, ItemKind::Unknown)
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
    /// Body lines shown while collapsed; 0 shows only the header. Set by [`tool_view`]:
    /// only a running command and a diff show theirs unasked.
    pub preview: usize,
    pub status: ToolStatus,
    /// The attachment ids of the images in the tool's output.
    pub images: Vec<String>,
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
pub fn duration(item: &Item) -> Option<String> {
    let parse = |iso: &str| OffsetDateTime::parse(iso, &Rfc3339).ok();
    let elapsed = parse(item.ended_at.as_deref()?)? - parse(&item.started_at)?;
    let ms = elapsed.whole_milliseconds().max(0);
    Some(match ms {
        0..1_000 => format!("{ms}ms"),
        1_000..60_000 => format!("{:.1}s", ms as f64 / 1_000.),
        _ => format!("{}m {:02}s", ms / 60_000, ms % 60_000 / 1_000),
    })
}

/// The tool row for `item`, or `None` if it is not a tool item.
pub fn tool_view(item: &Item) -> Option<ToolView> {
    let ItemKind::Tool(tool) = &item.kind else {
        return None;
    };
    let status = match item.status {
        ItemStatus::Running => ToolStatus::Running,
        ItemStatus::Failed => ToolStatus::Failed,
        ItemStatus::Completed | ItemStatus::Cancelled => ToolStatus::Done,
    };
    let output = tool.output.as_ref();
    let images: Vec<String> = output
        .into_iter()
        .flat_map(|output| &output.images)
        .map(|image| image.attachment_id.clone())
        .collect();
    let mut view = match &tool.detail {
        Some(ToolDetail::Command { command }) => command_view(command, output),
        Some(ToolDetail::FileChange {
            path,
            diff,
            additions,
            deletions,
            content,
        }) => file_change_view(
            path,
            diff.as_deref(),
            additions.zip(*deletions),
            content.as_deref(),
        ),
        Some(ToolDetail::Search { pattern, matches }) => {
            search_view(&tool.name, pattern, matches.as_deref())
        }
        Some(ToolDetail::WebSearch { query, results }) => {
            web_search_view(query, results.as_deref())
        }
        Some(ToolDetail::Unknown) | None => plain_view(
            &tool.name,
            &tool.input,
            output_text(tool),
            !images.is_empty(),
        ),
    };
    let command = matches!(tool.detail, Some(ToolDetail::Command { .. }));
    view.status = match status {
        ToolStatus::Done
            if command
                && output
                    .and_then(|o| o.exit_code)
                    .is_some_and(|code| code != 0) =>
        {
            ToolStatus::Failed
        }
        status => status,
    };
    // The header names the call's intent, unless it only repeats the tool.
    view.intent = tool.intent.clone().filter(|intent| {
        !intent.trim().is_empty()
            && !intent.eq_ignore_ascii_case(&view.title)
            && *intent != tool.name
    });
    if view.status != ToolStatus::Running
        && let Some(elapsed) = duration(item)
    {
        view.meta.push(muted(elapsed));
    }
    // A command shows itself in its box; the header says what it is for.
    if command && let Some(intent) = view.intent.take() {
        view.description = intent;
        view.prose = true;
    }
    view.images = images;
    view.preview = match (&view.body, view.status) {
        (Some(ToolBody::Command { .. }), ToolStatus::Running) => COMMAND_PREVIEW,
        (Some(ToolBody::Diff { .. }), _) => DIFF_PREVIEW,
        _ => 0,
    };
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
        images: Vec::new(),
    }
}

fn command_view(command: &str, output: Option<&ToolOutput>) -> ToolView {
    let mut view = base(ToolIcon::Terminal, "Bash", one_line(command));
    if let Some(code) = output.and_then(|o| o.exit_code).filter(|code| *code != 0) {
        view.meta.push((format!("exit {code}"), Tone::Failed));
    }
    view.body = Some(ToolBody::Command {
        command: command.trim_end().to_owned(),
        output: strip_notices(output.map_or("", |o| o.text.as_str())).to_owned(),
    });
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

fn file_change_view(
    path: &str,
    diff: Option<&str>,
    counts: Option<(u64, u64)>,
    content: Option<&str>,
) -> ToolView {
    // A write to `agent://` and the like is not a file.
    if path.contains("://") {
        return write_view(path, content, None);
    }
    let diff = diff.filter(|d| !d.trim().is_empty());
    match (diff, content) {
        (None, Some(content)) => {
            let mut view = base(ToolIcon::Write, "Write", path.to_owned());
            view.meta
                .push(muted(count(content.lines().count(), "line", "lines")));
            view.body = Some(ToolBody::Code {
                text: content.trim_end().to_owned(),
                first_line: 1,
                language: crate::syntax::language(path),
            });
            view
        }
        _ => {
            let mut view = base(ToolIcon::Edit, "Edit", path.to_owned());
            if let Some((added, removed)) = counts {
                view.meta.push((format!("+{added}"), Tone::Added));
                view.meta.push((format!("−{removed}"), Tone::Removed));
            }
            view.body = diff.map(|diff| ToolBody::Diff {
                text: diff.trim_end().to_owned(),
                language: crate::syntax::language(path),
            });
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
            }
            view
        }
    }
}

/// `grep` hits, or the files `glob`/`find` matched.
fn search_view(name: &str, pattern: &str, matches: Option<&[SearchMatch]>) -> ToolView {
    let pattern = one_line(pattern);
    let results = matches.unwrap_or_default();
    let grep = match name {
        "grep" => true,
        "glob" | "find" => false,
        _ => results.iter().any(|r| r.line.is_some()),
    };
    let mut view = if grep {
        base(ToolIcon::Grep, "Grep", pattern)
    } else {
        base(ToolIcon::Glob, "Glob", pattern)
    };
    if matches.is_some() {
        let mut files: Vec<&str> = results.iter().map(|r| r.path.as_str()).collect();
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
                    let mut line = result.path.clone();
                    if let Some(n) = result.line {
                        line.push_str(&format!(":{n}"));
                    }
                    if let Some(preview) = result.text.as_deref().filter(|p| !p.is_empty()) {
                        line.push_str(&format!("  {preview}"));
                    }
                    line
                })
                .collect(),
        ));
    }
    view
}

fn web_search_view(query: &str, results: Option<&[WebResult]>) -> ToolView {
    let mut view = base(ToolIcon::Web, "Web Search", one_line(query));
    if let Some(results) = results {
        view.meta
            .push(muted(count(results.len(), "source", "sources")));
        view.body = Some(ToolBody::List(
            results
                .iter()
                .map(|result| {
                    let url = result.url.as_str();
                    let host = url
                        .split("://")
                        .nth(1)
                        .unwrap_or(url)
                        .split('/')
                        .next()
                        .unwrap_or(url);
                    match (result.title.trim(), host) {
                        ("", host) => host.to_owned(),
                        (title, "") => title.to_owned(),
                        (title, host) => format!("{title} ({host})"),
                    }
                })
                .collect(),
        ));
    }
    view
}

/// A tool's output text, if it has any.
fn output_text(tool: &ToolItem) -> Option<String> {
    tool.output
        .as_ref()
        .map(|output| output.text.clone())
        .filter(|text| !text.trim().is_empty())
}

/// Drops the `[PATH#TAG]` header omp's `read` puts above a file.
fn strip_hashline_header(text: &str) -> &str {
    match text.split_once('\n') {
        Some((first, rest)) if hashline_path(first).is_some() => rest,
        _ => text,
    }
}

/// A tool without a structured detail: its view from its name, input and output text, as
/// omp's renderers draw it.
fn plain_view(name: &str, input: &Value, output: Option<String>, has_images: bool) -> ToolView {
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
            if let Some(output) = output.filter(|_| !has_images) {
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

/// Rows of agent activity (tool calls, thinking, plan updates, notices, stops) that
/// read as one list when they follow each other: they sit close together, on
/// one rail.
pub fn is_activity(item: &Item) -> bool {
    matches!(
        item.kind,
        ItemKind::Tool(_)
            | ItemKind::Thinking(_)
            | ItemKind::Todo(_)
            | ItemKind::Notice(_)
            | ItemKind::Stop(_)
    )
}

/// A subagent's status (its item's status) as the feed and the agents hub name it.
pub fn subagent_status_label(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::Running => "Running",
        ItemStatus::Completed => "Done",
        ItemStatus::Failed => "Failed",
        ItemStatus::Cancelled => "Stopped",
    }
}

/// A stop row's text: the stop while it is under way, then its result.
pub fn stop_label(stop: &StopItem) -> &'static str {
    match stop.stopped_at {
        Some(_) => "Stopped",
        None => "Stopping…",
    }
}

/// A goal row's text: what became of the goal, then its objective.
pub fn goal_label(goal: &Goal) -> String {
    let state = match goal.state {
        GoalState::Active => "Goal set",
        GoalState::Paused => "Goal paused",
        GoalState::BudgetLimited => "Goal reached its budget",
        GoalState::Complete => "Goal complete",
        GoalState::Dropped => "Goal dropped",
    };
    format!("{state}: {}", one_line(&goal.objective))
}

/// A compaction rule's label: `Context compacted`, with ` · {before} tokens
/// before` when the count is known.
pub fn compaction_label(compaction: &CompactionItem) -> String {
    match compaction.tokens_before {
        Some(before) => format!("Context compacted · {before} tokens before"),
        None => "Context compacted".to_owned(),
    }
}

/// A todo row's notice: `Updated the plan · {done}/{total} done`.
pub fn todo_notice(todo: &TodoItem) -> String {
    let tasks = todo.phases.iter().flat_map(|phase| &phase.tasks);
    let total = tasks.clone().count();
    let done = tasks
        .filter(|task| task.status == TodoStatus::Completed)
        .count();
    format!("Updated the plan · {done}/{total} done")
}

/// An answer to a question as the feed shows it: the choices joined with
/// commas, then the typed answer.
pub fn answer_text(answer: &Answer) -> String {
    answer
        .values
        .iter()
        .map(String::as_str)
        .chain(
            answer
                .other
                .as_deref()
                .filter(|other| !other.trim().is_empty()),
        )
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use j3_client::entities::{
        CompactionItem, NoticeItem, NoticeLevel, TextItem, TodoPhase, TodoTask, ToolImage,
    };
    use serde_json::json;

    fn item(status: ItemStatus, kind: ItemKind) -> Item {
        Item {
            id: "it".into(),
            thread_id: "T".into(),
            turn_id: "tu".into(),
            ordinal: 1,
            status,
            started_at: "2026-11-01T12:00:00.000Z".into(),
            ended_at: None,
            kind,
        }
    }

    fn output(text: &str) -> ToolOutput {
        ToolOutput {
            text: text.into(),
            ..ToolOutput::default()
        }
    }

    fn tool(
        status: ItemStatus,
        name: &str,
        input: Value,
        output: Option<ToolOutput>,
        detail: Option<ToolDetail>,
    ) -> Item {
        item(
            status,
            ItemKind::Tool(ToolItem {
                call_id: "c".into(),
                name: name.into(),
                intent: None,
                input,
                output,
                detail,
            }),
        )
    }

    fn view(item: &Item) -> ToolView {
        tool_view(item).unwrap()
    }

    fn command(status: ItemStatus, command: &str, out: Option<ToolOutput>) -> Item {
        tool(
            status,
            "bash",
            json!({"command": command}),
            out,
            Some(ToolDetail::Command {
                command: command.into(),
            }),
        )
    }

    fn change(
        path: &str,
        diff: Option<&str>,
        counts: Option<(u64, u64)>,
        content: Option<&str>,
    ) -> Item {
        tool(
            ItemStatus::Completed,
            "edit",
            json!({}),
            None,
            Some(ToolDetail::FileChange {
                path: path.into(),
                diff: diff.map(Into::into),
                additions: counts.map(|c| c.0),
                deletions: counts.map(|c| c.1),
                content: content.map(Into::into),
            }),
        )
    }

    #[test]
    fn hides_only_unknown_kinds() {
        assert!(is_hidden(&item(ItemStatus::Completed, ItemKind::Unknown)));
        assert!(!is_hidden(&item(
            ItemStatus::Completed,
            ItemKind::Text(TextItem { text: "hi".into() })
        )));
        assert!(!is_hidden(&item(
            ItemStatus::Completed,
            ItemKind::Stop(StopItem {
                requested_at: "2026-11-01T12:00:00.000Z".into(),
                stopped_at: None
            })
        )));
    }

    #[test]
    fn commands_fail_on_a_nonzero_exit_and_time_their_run() {
        let mut failed = command(
            ItemStatus::Completed,
            "sleep 30\necho done",
            Some(ToolOutput {
                exit_code: Some(1),
                ..output("done\n\nWall time: 2.10 seconds\n")
            }),
        );
        if let ItemKind::Tool(tool) = &mut failed.kind {
            tool.intent = Some("Waiting for the build".into());
        }
        failed.ended_at = Some("2026-11-01T12:00:02.140Z".into());
        let view = view(&failed);
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

        let mut running = command(ItemStatus::Running, "ls", None);
        if let ItemKind::Tool(tool) = &mut running.kind {
            tool.intent = Some("bash".into());
        }
        let view = tool_view(&running).unwrap();
        assert_eq!(view.status, ToolStatus::Running);
        assert_eq!(
            (view.description.as_str(), view.prose),
            ("ls", false),
            "the bare tool name is not an intent"
        );
    }

    #[test]
    fn only_a_running_command_and_a_diff_show_their_body_unasked() {
        let preview = |status| {
            view(&command(
                status,
                "cargo test",
                Some(ToolOutput {
                    exit_code: Some(0),
                    ..output("ok\n")
                }),
            ))
            .preview
        };
        assert!(
            preview(ItemStatus::Running) > 0,
            "a running command shows its output"
        );
        assert_eq!(
            preview(ItemStatus::Completed),
            0,
            "a finished one folds away"
        );
        let diff = view(&change("src/a.rs", Some("+1|x"), None, None));
        assert!(diff.preview > 0, "a diff shows unasked");
        let read = view(&tool(
            ItemStatus::Completed,
            "read",
            json!({"path": "a.rs"}),
            Some(output("1:fn main() {}")),
            None,
        ));
        assert!(read.body.is_some());
        assert_eq!(read.preview, 0, "a read waits for a click");
    }

    #[test]
    fn edits_writes_and_searches_read_like_omp() {
        let edit = view(&change("src/a.rs", Some("+1|x"), Some((3, 1)), None));
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

        let write = view(&change("b.txt", None, None, Some("one\ntwo\n")));
        assert_eq!(
            (write.title.as_str(), write.icon),
            ("Write", ToolIcon::Write)
        );
        assert_eq!(write.meta, [("2 lines".to_owned(), Tone::Muted)]);

        let hit = |line: u64, text: &str| SearchMatch {
            path: "a.rs".into(),
            line: Some(line),
            text: Some(text.into()),
        };
        let grep = view(&tool(
            ItemStatus::Completed,
            "grep",
            json!({"pattern": "fn main"}),
            None,
            Some(ToolDetail::Search {
                pattern: "fn main".into(),
                matches: Some(vec![hit(3, "fn main() {"), hit(9, "fn main2()")]),
            }),
        ));
        assert_eq!((grep.title.as_str(), grep.icon), ("Grep", ToolIcon::Grep));
        assert_eq!(grep.meta, [("2 matches · 1 file".to_owned(), Tone::Muted)]);
        assert_eq!(
            grep.body,
            Some(ToolBody::List(vec![
                "a.rs:3  fn main() {".into(),
                "a.rs:9  fn main2()".into()
            ]))
        );

        let glob = view(&tool(
            ItemStatus::Running,
            "glob",
            json!({"pattern": "src/**/*.rs"}),
            None,
            Some(ToolDetail::Search {
                pattern: "src/**/*.rs".into(),
                matches: None,
            }),
        ));
        assert_eq!(glob.title, "Glob");
    }

    #[test]
    fn plain_tools_get_their_omp_titles() {
        let plain = |name: &str, input: Value, out: Option<&str>| {
            view(&tool(
                ItemStatus::Completed,
                name,
                input,
                out.map(output),
                None,
            ))
        };
        let read = plain(
            "read",
            json!({"path": "PLAN.md:7-8"}),
            Some("[PLAN.md#C34C]\n7:# plan\n8:"),
        );
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
        let listing = plain("read", json!({"path": "src/"}), Some("main.rs\nlib.rs"));
        assert_eq!(
            listing.body,
            Some(ToolBody::Sections(vec![(None, "main.rs\nlib.rs".into())]))
        );

        // An edit without a file-change detail names its file from the script.
        let edit = plain(
            "edit",
            json!({"input": "[README.md#AF62]\nPUT 1:\n+x"}),
            None,
        );
        assert_eq!(
            (edit.title.as_str(), edit.description.as_str()),
            ("Edit", "README.md")
        );

        let message = plain(
            "write",
            json!({"path": "agent://Hexmap", "content": "hi"}),
            None,
        );
        assert_eq!(
            (
                message.icon,
                message.title.as_str(),
                message.description.as_str()
            ),
            (ToolIcon::Message, "Message", "→ Hexmap")
        );

        let eval = plain(
            "eval",
            json!({"code": "1+1", "language": "js", "title": "Adding"}),
            Some("2"),
        );
        assert_eq!(
            (eval.title.as_str(), eval.description.as_str()),
            ("Eval", "Adding")
        );
        assert_eq!(eval.meta, [("js".to_owned(), Tone::Muted)]);

        let other = plain(
            "search_tool_bm25",
            json!({"i": "Finding", "query": "x", "limit": 2}),
            None,
        );
        assert_eq!(other.title, "Search tool bm25");
        assert_eq!(other.description, "query=x, limit=2");
    }

    #[test]
    fn tool_images_are_their_attachments() {
        let shot = view(&tool(
            ItemStatus::Completed,
            "browser",
            json!({"action": "screenshot"}),
            Some(ToolOutput {
                images: vec![ToolImage {
                    attachment_id: "att_1".into(),
                    mime_type: "image/png".into(),
                }],
                ..output("")
            }),
            None,
        ));
        assert_eq!(shot.images, ["att_1".to_owned()]);
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

    #[test]
    fn notices_count_steps_tokens_and_answers() {
        let compaction = |before| CompactionItem {
            summary: None,
            tokens_before: before,
        };
        assert_eq!(
            compaction_label(&compaction(Some(182_340))),
            "Context compacted · 182340 tokens before"
        );
        assert_eq!(compaction_label(&compaction(None)), "Context compacted");

        let task = |text: &str, status| TodoTask {
            text: text.into(),
            status,
        };
        let todo = TodoItem {
            phases: vec![
                TodoPhase {
                    name: "One".into(),
                    tasks: vec![
                        task("A", TodoStatus::Completed),
                        task("B", TodoStatus::InProgress),
                    ],
                },
                TodoPhase {
                    name: "Two".into(),
                    tasks: vec![task("C", TodoStatus::Pending)],
                },
            ],
        };
        assert_eq!(todo_notice(&todo), "Updated the plan · 1/3 done");

        let answer = |values: &[&str], other: Option<&str>| Answer {
            question_id: "q".into(),
            values: values.iter().map(|v| (*v).to_owned()).collect(),
            other: other.map(Into::into),
        };
        assert_eq!(
            answer_text(&answer(&["60 Hz", "120 Hz"], None)),
            "60 Hz, 120 Hz"
        );
        assert_eq!(answer_text(&answer(&["Bottom"], None)), "Bottom");
        assert_eq!(answer_text(&answer(&[], Some("Left"))), "Left");
        assert_eq!(answer_text(&answer(&[], None)), "");
    }

    #[test]
    fn activity_rows_are_tools_thinking_plans_and_notices() {
        let thinking = item(
            ItemStatus::Completed,
            ItemKind::Thinking(TextItem {
                text: String::new(),
            }),
        );
        let notice = item(
            ItemStatus::Completed,
            ItemKind::Notice(NoticeItem {
                level: NoticeLevel::Info,
                text: "m".into(),
            }),
        );
        let reply = item(
            ItemStatus::Completed,
            ItemKind::Text(TextItem { text: "hi".into() }),
        );
        assert!(is_activity(&thinking));
        assert!(is_activity(&notice), "notices sit on the rail");
        assert!(!is_activity(&reply));
        assert_eq!(subagent_status_label(ItemStatus::Cancelled), "Stopped");
        assert_eq!(subagent_status_label(ItemStatus::Completed), "Done");
    }
}
