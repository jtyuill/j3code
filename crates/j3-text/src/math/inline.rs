//! Formulas inside a line of text (`$…$`, and `$$…$$` set in display style).
//!
//! A markdown view that draws a paragraph as one text cannot hold an image in it, but a
//! paragraph drawn apart can set each formula in its line. [`split`] cuts a message into the
//! top-level paragraphs that hold formulas and the markdown around them; in that markdown a
//! formula reads as its Unicode approximation ([`super::unicode`]).

use std::ops::Range;

use super::{
    Formula,
    delimiters::{CodeBlocks, after_quote_markers, closing_run, columns, is_list_item},
    inline_formula, unicode,
};

/// A formula in a line of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Its bytes in the text, delimiters included.
    pub range: Range<usize>,
    pub formula: Formula,
}

/// The formulas in `text`, prose with no code blocks in it, in order. Code spans and
/// escaped dollars are skipped; a dollar that opens no formula RaTeX accepts stays text.
pub fn find(text: &str) -> Vec<Found> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => {
                i += if bytes[i + 1].is_ascii_punctuation() {
                    2
                } else {
                    1
                };
            }
            b'`' => {
                let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                i = closing_run(&text[i + run..], run).map_or(i + run, |end| i + run + end);
            }
            b'$' => match formula_at(text, i) {
                Some(formula) => {
                    i = formula.range.end;
                    found.push(formula);
                }
                None => i += 1,
            },
            _ => i += 1,
        }
    }
    found
}

/// The formula opening at byte `start`, a `$`.
fn formula_at(text: &str, start: usize) -> Option<Found> {
    let display = text[start..].starts_with("$$");
    let open = if display { 2 } else { 1 };
    let close = closing_dollars(&text[start + open..], open)? + start + open;
    let end = close + open;
    let tex = &text[start + open..close];
    if tex.contains("\n\n") {
        return None;
    }
    let formula = inline_formula(&text[start..end], tex.trim(), &text[end..])?;
    Some(Found {
        range: start..end,
        formula,
    })
}

/// Where the first unescaped run of `len` dollars in `text` starts.
fn closing_dollars(text: &str, len: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'$' if len == 1 => return (i > 0).then_some(i),
            b'$' if bytes.get(i + 1) == Some(&b'$') => return (i > 0).then_some(i),
            _ => i += 1,
        }
    }
    None
}

/// A part of a message from [`split`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Markdown, its formulas written as their Unicode approximation.
    Markdown(String),
    /// A top-level paragraph holding formulas: its markdown with each formula replaced by
    /// [`placeholder`] of its index in `formulas`.
    Paragraph {
        source: String,
        formulas: Vec<Formula>,
    },
}

/// The character standing for formula `index` in a [`Part::Paragraph`]'s source: a private
/// use character, which markdown keeps as text. It is three bytes long, as the word joiner
/// the view draws in its place is.
pub fn placeholder(index: usize) -> Option<char> {
    u32::try_from(index)
        .ok()
        .and_then(|index| char::from_u32(0xE000 + index))
        .filter(|c| *c <= '\u{F8FF}')
}

/// The formula index a [`placeholder`] character stands for.
pub fn placeholder_index(c: char) -> Option<usize> {
    ('\u{E000}'..='\u{F8FF}')
        .contains(&c)
        .then(|| c as usize - 0xE000)
}

/// `markdown` cut into top-level paragraphs that hold formulas and the markdown around them.
/// A paragraph is top level when it starts a line after a blank line (or a heading, or a
/// code fence it closes), outside lists, quotes and code; a table, a setext heading or an
/// HTML block is not one. Markdown parts keep their text, with their formulas as Unicode.
pub fn split(markdown: &str) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut prose = String::new();
    let mut blocks = CodeBlocks::new();
    // A top-level paragraph may start on the next line.
    let mut may_start = true;
    let mut lines = markdown.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        let verbatim = blocks.is_verbatim(line);
        if verbatim || !may_start || !opens_paragraph(line) {
            may_start = line.trim().is_empty() || (!verbatim && is_heading(line));
            if verbatim && !line.trim().is_empty() {
                // A fence that just closed lets a paragraph follow it.
                may_start = is_fence(line);
            }
            prose.push_str(line);
            continue;
        }
        let mut paragraph = String::from(line);
        while let Some(next) = lines.peek() {
            if next.trim().is_empty() || interrupts(next) {
                break;
            }
            paragraph.push_str(next);
            blocks.is_verbatim(next);
            lines.next();
        }
        may_start = false;
        let second = paragraph.split_inclusive('\n').nth(1).unwrap_or("");
        let found = find(&paragraph);
        if found.is_empty() || is_table_delimiter(second) || is_setext_underline(second) {
            prose.push_str(&paragraph);
            continue;
        }
        if !prose.is_empty() {
            parts.push(Part::Markdown(with_unicode(&std::mem::take(&mut prose))));
        }
        let mut source = String::with_capacity(paragraph.len());
        let mut formulas = Vec::with_capacity(found.len());
        let mut copied = 0;
        for f in found {
            let Some(mark) = placeholder(formulas.len()) else {
                break;
            };
            source.push_str(&paragraph[copied..f.range.start]);
            source.push(mark);
            copied = f.range.end;
            formulas.push(f.formula);
        }
        source.push_str(&paragraph[copied..]);
        parts.push(Part::Paragraph { source, formulas });
    }
    if !prose.is_empty() {
        parts.push(Part::Markdown(with_unicode(&prose)));
    }
    parts
}

/// Whether `line` can start a top-level paragraph: unindented prose, not a block's opening.
fn opens_paragraph(line: &str) -> bool {
    let body = line.trim_start_matches([' ', '\t']);
    columns(&line[..line.len() - body.len()]) < 2
        && !line.trim().is_empty()
        && !interrupts(line)
        && !body.starts_with(['|', '<', '!'])
}

/// Whether `line` ends the paragraph before it and starts a block of its own.
fn interrupts(line: &str) -> bool {
    if after_quote_markers(line).len() != line.len() {
        return true;
    }
    let body = line.trim_start_matches([' ', '\t']);
    is_heading(line)
        || is_list_item(body)
        || is_fence(line)
        || is_rule(body)
        || body.starts_with("$$")
        || body.starts_with("![")
}

fn is_heading(line: &str) -> bool {
    let body = line.trim_start_matches([' ', '\t']);
    let hashes = body.bytes().take_while(|&b| b == b'#').count();
    (1..=6).contains(&hashes)
        && (body.len() == hashes || body[hashes..].starts_with([' ', '\t', '\n', '\r']))
}

fn is_fence(line: &str) -> bool {
    let body = line.trim_start_matches([' ', '\t']);
    body.starts_with("```") || body.starts_with("~~~")
}

fn is_rule(body: &str) -> bool {
    let body = body.trim_end();
    let Some(c) = body.chars().next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    body.chars().filter(|&x| x == c).count() >= 3 && body.chars().all(|x| x == c || x == ' ')
}

fn is_table_delimiter(line: &str) -> bool {
    let body = line.trim();
    body.contains('-')
        && body.contains('|')
        && body
            .chars()
            .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
}

/// A [`Part::Paragraph`]'s source as markdown, each placeholder written as its formula's
/// Unicode approximation: for when the paragraph cannot be drawn apart after all.
pub fn unicode_source(source: &str, formulas: &[Formula]) -> String {
    let mut out = String::with_capacity(source.len());
    for c in source.chars() {
        match placeholder_index(c).and_then(|i| formulas.get(i)) {
            Some(formula) => unicode::escaped(&unicode::approximate(&formula.tex), &mut out),
            None => out.push(c),
        }
    }
    out
}

fn is_setext_underline(line: &str) -> bool {
    let body = line.trim();
    !body.is_empty() && (body.chars().all(|c| c == '=') || body.chars().all(|c| c == '-'))
}

/// `markdown` with each formula in its prose written as its Unicode approximation, escaped
/// so markdown shows it as written.
fn with_unicode(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut blocks = CodeBlocks::new();
    let mut prose_start = None;
    let mut pos = 0;
    let flush = |out: &mut String, chunk: &str| {
        let mut copied = 0;
        for f in find(chunk) {
            out.push_str(&chunk[copied..f.range.start]);
            unicode::escaped(&unicode::approximate(&f.formula.tex), out);
            copied = f.range.end;
        }
        out.push_str(&chunk[copied..]);
    };
    for line in markdown.split_inclusive('\n') {
        if blocks.is_verbatim(line) {
            if let Some(start) = prose_start.take() {
                flush(&mut out, &markdown[start..pos]);
            }
            out.push_str(line);
        } else if prose_start.is_none() {
            prose_start = Some(pos);
        }
        pos += line.len();
    }
    if let Some(start) = prose_start {
        flush(&mut out, &markdown[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texs(text: &str) -> Vec<(String, bool)> {
        find(text)
            .into_iter()
            .map(|f| (f.formula.tex, f.formula.display))
            .collect()
    }

    #[test]
    fn formulas_in_a_line_follow_the_dollar_rule() {
        assert_eq!(texs("with $h_i$ rows"), [("h_i".to_owned(), false)]);
        assert_eq!(texs("costs $5 and $10"), []);
        assert_eq!(texs("a $$x^2$$ b"), [("x^2".to_owned(), true)]);
        assert_eq!(
            texs("$a$, $b$."),
            [("a".into(), false), ("b".into(), false)]
        );
        assert_eq!(texs(r"not \$x$ here"), []);
        assert_eq!(texs("code `$x$` and $y$"), [("y".to_owned(), false)]);
        assert_eq!(texs(r"rejected $\frac{a$ text"), []);
        assert_eq!(texs("open $x"), []);
    }

    #[test]
    fn a_paragraph_with_formulas_is_drawn_apart() {
        let parts = split("# Title\n\nWith $h_i$ above\nthe anchor.\n\nPlain $5 text.\n");
        assert_eq!(
            parts,
            [
                Part::Markdown("# Title\n\n".into()),
                Part::Paragraph {
                    source: "With \u{E000} above\nthe anchor.\n".into(),
                    formulas: vec![Formula {
                        tex: "h_i".into(),
                        display: false
                    }],
                },
                Part::Markdown("\nPlain $5 text.\n".into()),
            ]
        );
    }

    #[test]
    fn lists_quotes_tables_and_code_keep_their_formulas_as_text() {
        let parts = split("- with $\\alpha$\n\n> $x^2$\n\n```\n$y$\n```\n\n| $a$ |\n|---|\n");
        assert_eq!(
            parts,
            [Part::Markdown(
                "- with α\n\n> x²\n\n```\n$y$\n```\n\n| a |\n|---|\n".into()
            )]
        );
        // A line right after a list item continues it.
        assert_eq!(
            split("- item\ncontinued $x$\n"),
            [Part::Markdown("- item\ncontinued x\n".into())]
        );
    }

    #[test]
    fn a_paragraph_may_follow_a_heading_or_a_fence() {
        let parts = split("## H\nSo $x$.\n");
        assert!(matches!(&parts[1], Part::Paragraph { source, .. } if source == "So \u{E000}.\n"));
        let parts = split("```\ncode\n```\nThen $y$.\n");
        assert!(
            matches!(&parts[1], Part::Paragraph { source, .. } if source == "Then \u{E000}.\n")
        );
    }

    #[test]
    fn placeholders_round_trip_and_match_the_joiner_length() {
        let mark = placeholder(5).unwrap();
        assert_eq!(placeholder_index(mark), Some(5));
        assert_eq!(mark.len_utf8(), '\u{2060}'.len_utf8());
        assert_eq!(placeholder_index('a'), None);
        assert!(placeholder(10_000).is_none());
    }
}
