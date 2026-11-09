//! The file preview under the tree: a breadcrumb with copy and close, then the file read with
//! `projects.readFile`, syntax-coloured by its extension with line numbers, or a notice
//! (truncated, binary, failed).

use std::{borrow::Cow, ops::Range};

use j3_client::{Error, envelope::ErrorCode, methods::ReadFileResult};
use rune::{El, GrowthSpec, ScrollSpec, TextStyle, Tx, col, el, icon, row, text};
use rune_kit::{
    IconButton, IconButtonVariant, Spinner,
    syntax::{Highlight, SyntaxTheme},
};

use super::FilePane;
use crate::{
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
    theme::Status,
};

/// The open file.
pub(super) struct Preview {
    pub path: String,
    pub body: Body,
    /// The read whose answer counts.
    request: u64,
}

impl Preview {
    pub fn loading_for(path: String, request: u64) -> Self {
        Self {
            path,
            body: Body::Loading,
            request,
        }
    }

    pub fn loading(&self) -> bool {
        matches!(self.body, Body::Loading)
    }

    /// The truncation notice of a text preview.
    #[cfg(test)]
    pub fn truncated(&self) -> Option<&str> {
        match &self.body {
            Body::Text(code) => code.truncated.as_deref(),
            _ => None,
        }
    }
}

pub(super) enum Body {
    Loading,
    Text(Code),
    /// The server would not send a binary file.
    Binary,
    Failed(String),
}

/// A file's text split into lines, each with its highlights.
pub(super) struct Code {
    text: String,
    /// Each line's bytes in `text`, without its `\n` (or `\r\n`).
    lines: Vec<Range<usize>>,
    /// Each line's highlights, relative to the line.
    spans: Vec<Vec<Highlight>>,
    /// The widest line in columns (tabs to the next multiple of four).
    columns: usize,
    /// "Showing the first X of Y." over a truncated file.
    truncated: Option<String>,
}

impl Body {
    /// The preview of `path` from its read; highlighting runs here, off the UI thread.
    pub fn read(path: &str, result: Result<ReadFileResult, Error>) -> Body {
        match result {
            Ok(file) => {
                let truncated = file.truncated.then(|| {
                    format!(
                        "Showing the first {} of {}.",
                        format_size(file.text.len() as u64),
                        format_size(file.size)
                    )
                });
                let language = crate::syntax::language(path);
                let highlights =
                    rune_kit::syntax::builtin((language != "text").then_some(language), &file.text);
                let (lines, spans) = split_lines(&file.text, &highlights);
                let columns = lines
                    .iter()
                    .map(|line| columns(&file.text[line.clone()]))
                    .max()
                    .unwrap_or(0);
                Body::Text(Code {
                    text: file.text,
                    lines,
                    spans,
                    columns,
                    truncated,
                })
            }
            Err(Error::Server(error))
                if error.code == ErrorCode::Rejected && error.message.contains("binary") =>
            {
                Body::Binary
            }
            Err(error) => Body::Failed(error.to_string()),
        }
    }
}

impl FilePane {
    /// The answer to reading `path` as `request`.
    pub(super) fn file_read(&mut self, path: &str, request: u64, body: Body) {
        if let Some(preview) = self
            .preview
            .as_mut()
            .filter(|preview| preview.path == path && preview.request == request)
        {
            preview.body = body;
        }
    }
}

/// Splits sorted, disjoint `highlights` of `text` by line (`\n`, with a `\r` before it left
/// out): each line's byte range and its highlights relative to the line.
pub(super) fn split_lines(
    text: &str,
    highlights: &[Highlight],
) -> (Vec<Range<usize>>, Vec<Vec<Highlight>>) {
    let mut highlights = highlights.iter().peekable();
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        // `end` is the newline's offset; a highlight reaching past it continues.
        let end = start + line.len();
        let content_end = start + line.trim_end_matches('\r').len();
        let mut line_spans = Vec::new();
        while let Some((range, paint)) = highlights.peek() {
            if range.start > end {
                break;
            }
            let (from, to) = (range.start.max(start), range.end.min(content_end));
            if from < to {
                line_spans.push((from - start..to - start, *paint));
            }
            if range.end > end + 1 {
                break;
            }
            highlights.next();
        }
        lines.push(start..content_end);
        spans.push(line_spans);
        start = end + 1;
    }
    (lines, spans)
}

/// `line`'s width in columns, with tabs advancing to the next multiple of four.
fn columns(line: &str) -> usize {
    line.chars().fold(0, |col, c| {
        if c == '\t' {
            (col / 4 + 1) * 4
        } else {
            col + 1
        }
    })
}

/// `bytes` for people: `512 B`, `3.4 KB`, `1.0 MB`.
pub(super) fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < KB * KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{:.1} MB", value / (KB * KB))
    }
}

/// The preview: breadcrumb bar, then the file, a spinner or a notice.
pub(super) fn view(s: &J3, preview: &Preview, ui: &Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let status = Status::of(k);
    let notice = |label: String, color| {
        el().key("file-preview-notice")
            .label(label.clone())
            .w_full()
            .px(12.0)
            .py(4.0)
            .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
            .child(
                text(label)
                    .font(k.ty.text_xsmall)
                    .color(color)
                    .wrap()
                    .a11y_hidden(),
            )
    };
    let body = match &preview.body {
        Body::Loading => el()
            .key("file-preview-loading")
            .w_full()
            .grow(1.0)
            .center()
            .child(Spinner::new().build(k)),
        Body::Text(code) => col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .children(
                code.truncated
                    .clone()
                    .map(|truncated| notice(truncated, status.muted)),
            )
            .child(code_view(s, code, &preview.path, ui, key)),
        Body::Binary => notice("Binary file; there is no preview.".to_owned(), status.muted),
        Body::Failed(error) => notice(error.clone(), status.danger),
    };
    col()
        .key("file-preview")
        .w_full()
        .min_h(0.0)
        .child(breadcrumb(&preview.path, ui, key))
        .child(body)
}

/// The path's segments with chevrons between (the last one never shrinks), copy and close.
fn breadcrumb(path: &str, ui: &Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let segments: Vec<&str> = path.split('/').collect();
    let last = segments.len() - 1;
    let mut crumbs = row()
        .key("file-preview-path")
        .grow(1.0)
        .min_w(0.0)
        .gap(2.0)
        .center_y();
    for (ix, segment) in segments.into_iter().enumerate() {
        if ix > 0 {
            crumbs = crumbs.child(
                icon(&icons::CHEVRON_RIGHT, 12.0)
                    .color(k.c.icon_muted)
                    .a11y_hidden(),
            );
        }
        let crumb = text(segment).font(k.ty.text_xsmall);
        crumbs = crumbs.child(if ix == last {
            crumb.color(k.c.text_base)
        } else {
            crumb.color(k.c.text_muted).shrink().min_w(0.0).truncate()
        });
    }
    let copied = path.to_owned();
    let close_key = key.clone();
    row()
        .key("file-preview-header")
        .w_full()
        .h(32.0)
        .px(12.0)
        .gap(4.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(crumbs)
        .child(
            IconButton::new(&icons::COPY)
                .variant(IconButtonVariant::GhostMuted)
                .control_size(rune_kit::ControlSize::Small)
                .label("Copy path")
                .on_press_tx(move |_: &mut J3, tx: &mut Tx| tx.copy(copied.clone()))
                .build(k)
                .key("copy-file-path"),
        )
        .child(
            IconButton::new(&icons::X)
                .variant(IconButtonVariant::GhostMuted)
                .control_size(rune_kit::ControlSize::Small)
                .label("Close preview")
                .on_press(move |s: &mut J3| {
                    if let Some(pane) = s.files.get_mut(&close_key) {
                        pane.preview = None;
                    }
                })
                .build(k)
                .key("close-file-preview"),
        )
}

/// The file's lines, virtualised, with a line-number gutter; long lines scroll sideways.
fn code_view(s: &J3, code: &Code, path: &str, ui: &Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let font = TextStyle {
        line_height: (k.ty.mono.size * 1.5).round(),
        ..k.ty.mono
    };
    let line_h = font.line_height;
    let advance = 0.6 * font.size;
    let digits = code.lines.len().to_string().len();
    let gutter = (digits as f32 * advance + 20.0).ceil();
    let width = gutter + code.columns as f32 * advance + 24.0;
    let theme = SyntaxTheme::from_kit(&crate::theme::kit(k.dark, s.prefs.ui()));
    let (text_c, number_c) = (k.c.text_base, k.c.text_faint);
    let rows_key = key.clone();
    let lines = col()
        .key("file-preview-lines")
        .min_w(width)
        .w_full()
        .py(4.0)
        .selection_group(true)
        .growth(GrowthSpec::default())
        .virtual_rows(
            code.lines.len(),
            line_h,
            |_: &J3, i| Cow::Owned(format!("L{i}")),
            move |s: &J3, i| {
                let Some(Body::Text(code)) = s
                    .files
                    .get(&rows_key)
                    .and_then(|pane| pane.preview.as_ref())
                    .map(|preview| &preview.body)
                else {
                    return el();
                };
                let line = &code.text[code.lines[i].clone()];
                row()
                    .h(line_h)
                    .center_y()
                    .child(
                        text((i + 1).to_string())
                            .font(font)
                            .color(number_c)
                            .w(gutter)
                            .pl(8.0)
                            .pr(12.0)
                            .a11y_hidden(),
                    )
                    .child(
                        text(line)
                            .spans(theme.spans(&code.spans[i], 0))
                            .font(font)
                            .color(text_c)
                            .selectable(true),
                    )
            },
        )
        .virtual_rows_version((path, code.lines.len()));
    col()
        .key("file-preview-scroll")
        .label(format!("Contents of {path}"))
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .scroll_xy()
        .child(lines)
}
