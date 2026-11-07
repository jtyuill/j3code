//! The `\(…\)` and `\[…\]` math delimiters models write, rewritten to `$…$`
//! and `$$…$$`, and the line scanner that tells code from prose for it and
//! for [`super::prepare`].

use std::borrow::Cow;

/// Rewrites the `\(…\)` and `\[…\]` math delimiters models write, which
/// markdown reads as escaped brackets, to `$…$` and `$$…$$`.
///
/// Code spans, fenced and indented code blocks are left alone, and so is an
/// escaped backslash (`\\(` is a backslash and a parenthesis). Space just
/// inside `\(…\)` is dropped, so the formula passes the dollar rule. A `\[`
/// that starts a line (after indentation or `>` markers) becomes a `$$`
/// block, its fences on lines of their own. Text without `\(` or `\[` is
/// returned as is.
pub(super) fn normalize_delimiters(text: &str) -> Cow<'_, str> {
    if !text.contains("\\(") && !text.contains("\\[") {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut blocks = CodeBlocks::new();
    let mut display = None;
    let mut prose: Option<usize> = None;
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        if blocks.is_verbatim(line) {
            if let Some(start) = prose.take() {
                convert(&text[start..pos], &mut out, &mut display);
            }
            out.push_str(line);
        } else if prose.is_none() {
            prose = Some(pos);
        }
        pos += line.len();
    }
    if let Some(start) = prose {
        convert(&text[start..], &mut out, &mut display);
    }
    if out == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(out)
    }
}

/// An open `\[`: inside a line, or a block whose lines start with the
/// opening line's prefix (indentation and `>` markers).
#[derive(Clone, Copy)]
enum Bracket<'a> {
    Inline,
    Block(&'a str),
}

/// Rewrites the delimiters in `chunk`, consecutive non-blank prose lines
/// (code spans don't cross a blank line), onto `out`. `display` carries an
/// open `\[` block from one chunk to the next.
fn convert<'a>(chunk: &'a str, out: &mut String, display: &mut Option<Bracket<'a>>) {
    let bytes = chunk.as_bytes();
    // An open `\(`: where its `$` is in `out`, and where it is in `chunk`.
    let mut inline: Option<(usize, usize)> = None;
    let (mut i, mut copied, mut line_start) = (0, 0, 0);
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                i += 1;
                line_start = i;
            }
            b'`' => {
                let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                let end = closing_run(&chunk[i + run..], run).map_or(i + run, |end| i + run + end);
                if let Some(newline) = chunk[i..end].rfind('\n') {
                    line_start = i + newline + 1;
                }
                i = end;
            }
            b'\\' if i + 1 < bytes.len() => {
                let next = bytes[i + 1];
                if !matches!(next, b'(' | b')' | b'[' | b']') {
                    // An escaped character, `\\` included, is never a delimiter.
                    i += if next.is_ascii_punctuation() { 2 } else { 1 };
                    continue;
                }
                out.push_str(&chunk[copied..i]);
                let start = i;
                i += 2;
                match next {
                    b'(' => {
                        inline = Some((out.len(), start));
                        out.push('$');
                        i = skip_blanks(bytes, i);
                    }
                    b')' => match inline.take() {
                        Some((open, source)) => {
                            trim_blanks(out, open + 1);
                            if out.len() == open + 1 {
                                // `\(\)`: `$$` would open display math.
                                out.truncate(open);
                                out.push_str(&chunk[source..i]);
                            } else {
                                out.push('$');
                            }
                        }
                        None => out.push_str("\\)"),
                    },
                    b'[' => {
                        let prefix = &chunk[line_start..start];
                        if is_line_prefix(prefix) {
                            *display = Some(Bracket::Block(prefix));
                            out.push_str("$$");
                            i = skip_blanks(bytes, i);
                            if !at_line_end(bytes, i) {
                                out.push('\n');
                                out.push_str(prefix);
                            }
                        } else {
                            *display = Some(Bracket::Inline);
                            out.push_str("$$");
                        }
                    }
                    _ => match display.take() {
                        Some(Bracket::Block(prefix)) => {
                            if !is_line_prefix(&chunk[line_start..start]) {
                                trim_blanks(out, 0);
                                out.push('\n');
                                out.push_str(prefix);
                            }
                            out.push_str("$$");
                            i = skip_blanks(bytes, i);
                            if !at_line_end(bytes, i) {
                                out.push('\n');
                                out.push_str(prefix);
                            }
                        }
                        Some(Bracket::Inline) => out.push_str("$$"),
                        None => out.push_str("\\]"),
                    },
                }
                copied = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&chunk[copied..]);
}

/// The end of the first run of exactly `len` backticks in `text`.
fn closing_run(text: &str, len: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let start = i;
            while i < bytes.len() && bytes[i] == b'`' {
                i += 1;
            }
            if i - start == len {
                return Some(i);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Whether a line's text before some point is only indentation and `>`
/// markers.
fn is_line_prefix(text: &str) -> bool {
    text.bytes().all(|b| matches!(b, b' ' | b'\t' | b'>'))
}

fn skip_blanks(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

fn at_line_end(bytes: &[u8], i: usize) -> bool {
    matches!(bytes.get(i), None | Some(b'\n' | b'\r'))
}

/// Drops spaces and tabs from the end of `out`, not before `floor`.
fn trim_blanks(out: &mut String, floor: usize) {
    let keep = floor + out[floor..].trim_end_matches([' ', '\t']).len();
    out.truncate(keep);
}

/// Tells code lines from prose, one line after another.
pub(super) struct CodeBlocks {
    /// An open fence: its character and length.
    fence: Option<(u8, usize)>,
    /// The previous line was indented code.
    indented: bool,
    /// The previous line was blank (or there was none).
    after_blank: bool,
    /// Inside a list, where indented lines continue an item.
    in_list: bool,
}

impl CodeBlocks {
    pub(super) fn new() -> Self {
        Self {
            fence: None,
            indented: false,
            after_blank: true,
            in_list: false,
        }
    }

    /// Whether `line` is copied unchanged: blank, a fence or inside one, or
    /// indented code.
    pub(super) fn is_verbatim(&mut self, line: &str) -> bool {
        let rest = after_quote_markers(line);
        let body = rest.trim_start_matches([' ', '\t']);
        let indent = columns(&rest[..rest.len() - body.len()]);
        let may_fence = indent < 4 || self.in_list;
        if let Some((fence, len)) = self.fence {
            let run = body.bytes().take_while(|&b| b == fence).count();
            if may_fence && run >= len && body[run..].trim().is_empty() {
                self.fence = None;
            }
            return true;
        }
        if body.trim().is_empty() {
            self.after_blank = true;
            return true;
        }
        if may_fence && let Some(fence) = opening_fence(body) {
            self.fence = Some(fence);
            self.indented = false;
            self.after_blank = false;
            return true;
        }
        let code = indent >= 4 && !self.in_list && (self.after_blank || self.indented);
        if !code {
            if indent < 4 && is_list_item(body) {
                self.in_list = true;
            } else if indent == 0 && self.after_blank {
                self.in_list = false;
            }
        }
        self.indented = code;
        self.after_blank = false;
        code
    }
}

/// `line` after its blockquote markers.
pub(super) fn after_quote_markers(mut line: &str) -> &str {
    while let Some(rest) = line.trim_start_matches([' ', '\t']).strip_prefix('>') {
        line = rest.strip_prefix(' ').unwrap_or(rest);
    }
    line
}

/// The width of leading whitespace, tabs stopping every four columns.
pub(super) fn columns(indent: &str) -> usize {
    indent.bytes().fold(0, |column, b| match b {
        b'\t' => column + 4 - column % 4,
        _ => column + 1,
    })
}

/// The fence `body` opens: three or more backticks (with no backtick after)
/// or tildes.
fn opening_fence(body: &str) -> Option<(u8, usize)> {
    let fence = *body.as_bytes().first()?;
    if !matches!(fence, b'`' | b'~') {
        return None;
    }
    let len = body.bytes().take_while(|&b| b == fence).count();
    (len >= 3 && (fence == b'~' || !body[len..].contains('`'))).then_some((fence, len))
}

/// Whether `body` starts with a bullet or ordered list marker.
pub(super) fn is_list_item(body: &str) -> bool {
    list_marker_len(body).is_some()
}

/// The length of the bullet or ordered list marker `body` starts with, not
/// counting the space after it.
pub(super) fn list_marker_len(body: &str) -> Option<usize> {
    let rest = match body.strip_prefix(['-', '*', '+']) {
        Some(rest) => rest,
        None => {
            let digits = body.bytes().take_while(u8::is_ascii_digit).count();
            if !(1..=9).contains(&digits) {
                return None;
            }
            body[digits..].strip_prefix(['.', ')'])?
        }
    };
    (rest.is_empty() || rest.starts_with([' ', '\t', '\n', '\r'])).then_some(body.len() - rest.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn normalized(text: &str) -> String {
        normalize_delimiters(text).into_owned()
    }

    #[test]
    fn inline_delimiters_become_dollars() {
        assert_eq!(normalized(r"so \(x^2\) and \( y \)."), "so $x^2$ and $y$.");
        // Mid-line `\[` is display math inside the line.
        assert_eq!(normalized(r"then \[x\] here"), "then $$x$$ here");
        // Empty: `$$` would open display math.
        assert_eq!(normalized(r"a \(\) b"), r"a \(\) b");
    }

    #[test]
    fn a_line_starting_with_bracket_becomes_a_block() {
        assert_eq!(
            normalized("Sum:\n\\[ \\sum_k k \\]\nafter"),
            "Sum:\n$$\n\\sum_k k\n$$\nafter"
        );
        assert_eq!(normalized("\\[\nx\n\\]\n"), "$$\nx\n$$\n");
        assert_eq!(normalized("> \\[ x \\]"), "> $$\n> x\n> $$");
    }

    #[test]
    fn code_is_left_alone() {
        assert_eq!(normalized(r"`\(x\)` and \(y\)"), r"`\(x\)` and $y$");
        assert_eq!(
            normalized("```\n\\(x\\)\n```\n\\(y\\)"),
            "```\n\\(x\\)\n```\n$y$"
        );
        assert!(matches!(
            normalize_delimiters("text\n\n    \\(x\\)\n"),
            Cow::Borrowed(_)
        ));
        // Indented lines in a list continue its item.
        assert_eq!(normalized("- item\n\n    \\(x\\)"), "- item\n\n    $x$");
    }

    #[test]
    fn an_escaped_backslash_is_not_a_delimiter() {
        assert!(matches!(
            normalize_delimiters(r"a \\(x\\) b"),
            Cow::Borrowed(_)
        ));
        // TeX's `\\` line break inside a formula stays.
        assert_eq!(normalized(r"\(a \\ b\)"), r"$a \\ b$");
    }

    #[test]
    fn text_without_delimiters_is_borrowed() {
        assert!(matches!(
            normalize_delimiters("plain $x$ and (y)"),
            Cow::Borrowed(_)
        ));
    }
}
