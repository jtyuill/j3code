//! A tool row's body (spec §6.4): a tinted monospace block, borderless, under the header.
//! Commands show a `$` gutter and the command coloured as shell, then the tail of the output
//! (tinted on failure); diffs a line-number gutter, the sign and a tinted row; code its line
//! numbers and colours; lists one result a line; sections their labels, with JSON coloured.
//! Collapsed lines are cut with an ellipsis, expanded ones wrap. A footer (the "more lines"
//! toggle) closes the block.

use rune::{El, TextStyle, col, rich, row, text};
use rune_kit::Kit;

use super::lines;
use crate::{
    feed::{DiffKind, ToolBody, diff_line},
    theme::Status,
};

/// Command lines shown while collapsed; the rest are counted.
const COMMAND_PREVIEW: usize = 3;

/// The monospace style of tool bodies: the code font at 1.45 line height.
pub fn mono(k: &Kit) -> TextStyle {
    TextStyle {
        line_height: (k.ty.mono.size * 1.45).round(),
        ..k.ty.mono
    }
}

/// `body` showing at most `limit` lines, then `footer`; `expanded` lines wrap, collapsed ones
/// are cut. `failed` tints a command's output.
pub fn view(
    k: &Kit,
    body: &ToolBody,
    limit: usize,
    expanded: bool,
    failed: bool,
    footer: Option<El>,
) -> El {
    let block = content(k, body, limit, expanded, failed);
    match footer {
        Some(footer) => block.child(footer),
        None => block,
    }
}

fn content(k: &Kit, body: &ToolBody, limit: usize, expanded: bool, failed: bool) -> El {
    let theme = crate::theme::syntax(k);
    let status = Status::of(k);
    match body {
        ToolBody::Command { command, output } => {
            let colours = lines::code(command, "bash", &theme);
            let command_lines: Vec<&str> = command.lines().collect();
            let shown = if expanded {
                command_lines.len()
            } else {
                command_lines.len().min(COMMAND_PREVIEW)
            };
            let mut prompt = col().px(8.0).pt(6.0).pb(4.0);
            for (ix, line) in command_lines[..shown].iter().enumerate() {
                prompt = prompt.child(
                    row()
                        .w_full()
                        .gap(6.0)
                        .child(
                            text(if ix == 0 { "$" } else { " " })
                                .font(mono(k))
                                .color(k.c.text_faint),
                        )
                        .child(
                            line_text(k, line, lines::spans(colours.as_deref(), ix), expanded)
                                .color(k.c.text_base),
                        ),
                );
            }
            if shown < command_lines.len() {
                prompt = prompt.child(
                    text(format!("  … {} more", command_lines.len() - shown))
                        .font(mono(k))
                        .color(k.c.text_faint),
                );
            }
            let output_lines: Vec<&str> = output.lines().collect();
            // The end of the output is what matters: show the tail.
            let tail = &output_lines[output_lines.len().saturating_sub(limit)..];
            let mut frame = frame(k).child(prompt);
            if !tail.is_empty() {
                let ink = if failed {
                    k.alpha(status.danger, 0.9)
                } else {
                    k.c.text_muted
                };
                let mut out = col().px(8.0).pb(6.0);
                for line in tail {
                    out = out.child(line_text(k, line, Vec::new(), expanded).color(ink));
                }
                frame = frame.child(out);
            }
            frame
        }
        ToolBody::Diff {
            text: diff,
            language,
        } => {
            let gutter = diff
                .lines()
                .filter_map(|line| diff_line(line).number)
                .map(str::len)
                .max()
                .unwrap_or(0);
            let colours = lines::diff(diff, language, &theme, |row| {
                let line = diff_line(row);
                (line.kind != DiffKind::Hunk).then_some(line.text)
            });
            let mut frame = frame(k).py(6.0);
            for (ix, line) in diff.lines().take(limit).enumerate() {
                let line = diff_line(line);
                let (sign, sign_color, tint) = match line.kind {
                    DiffKind::Added => ("+", status.success, Some(k.alpha(status.success, 0.12))),
                    DiffKind::Removed => ("-", status.danger, Some(k.alpha(status.danger, 0.12))),
                    DiffKind::Hunk => (" ", status.info, None),
                    DiffKind::Context => (" ", k.c.text_muted, None),
                };
                let ink = match line.kind {
                    DiffKind::Context => k.alpha(k.c.text_base, 0.75),
                    DiffKind::Hunk => status.info,
                    _ => k.c.text_base,
                };
                let mut r = row().w_full().px(8.0).gap(6.0);
                if let Some(tint) = tint {
                    r = r.fill(tint);
                }
                if gutter > 0 {
                    r = r.child(
                        text(format!("{:>gutter$}", line.number.unwrap_or("")))
                            .font(mono(k))
                            .color(k.alpha(k.c.text_muted, 0.7)),
                    );
                }
                let spans = lines::spans(colours.as_deref(), ix);
                frame = frame.child(
                    r.child(text(sign).font(mono(k)).color(sign_color))
                        .child(line_text(k, line.text, spans, expanded).color(ink)),
                );
            }
            frame
        }
        ToolBody::Code {
            text: code,
            first_line,
            language,
        } => {
            let colours = lines::code(code, language, &theme);
            let lines: Vec<&str> = code.lines().collect();
            let gutter = (first_line + lines.len()).to_string().len();
            let mut frame = frame(k).px(8.0).py(6.0);
            for (ix, line) in lines.iter().take(limit).enumerate() {
                frame = frame.child(
                    row()
                        .w_full()
                        .gap(8.0)
                        .child(
                            text(format!("{:>gutter$}", first_line + ix))
                                .font(mono(k))
                                .color(k.alpha(k.c.text_muted, 0.7)),
                        )
                        .child(
                            line_text(k, line, lines::spans(colours.as_deref(), ix), expanded)
                                .color(k.c.text_base),
                        ),
                );
            }
            frame
        }
        ToolBody::List(items) => {
            let mut list = frame(k).px(8.0).py(6.0);
            for item in items.iter().take(limit) {
                list = list.child(line_text(k, item, Vec::new(), expanded).color(k.c.text_muted));
            }
            list
        }
        ToolBody::Sections(sections) => {
            let mut budget = limit;
            let mut frame = frame(k).py(2.0);
            for (label, body) in sections {
                let lines: Vec<&str> = body.lines().collect();
                let colours = lines::json(body, &theme);
                let shown = lines.len().min(budget);
                budget -= shown;
                let mut section = col().w_full().px(8.0).py(4.0);
                if let Some(label) = label {
                    section = section.child(
                        text(label.clone())
                            .font(k.ty.text_xsmall)
                            .color(k.c.text_faint),
                    );
                }
                for (line_ix, line) in lines[..shown].iter().enumerate() {
                    section = section.child(
                        line_text(k, line, lines::spans(colours.as_deref(), line_ix), expanded)
                            .color(k.alpha(k.c.text_base, 0.85)),
                    );
                }
                frame = frame.child(section);
            }
            frame
        }
    }
}

/// The block a body sits in: a faint fill, rounded, no border.
fn frame(k: &Kit) -> El {
    col()
        .w_full()
        .radius(k.radius.md)
        .clip_rounded(k.radius.md)
        .fill(k.alpha(k.c.layer_03, 0.35))
}

/// One monospace line coloured by `spans`: cut with an ellipsis while collapsed, wrapped once
/// the row is expanded.
fn line_text(k: &Kit, line: &str, spans: Vec<rune::Span>, expanded: bool) -> El {
    let line = if line.is_empty() { " " } else { line };
    let e = rich(line.to_owned(), spans).font(mono(k));
    if expanded {
        e.wrap_anywhere().grow(1.0).shrink()
    } else {
        e.truncate().grow(1.0)
    }
}
