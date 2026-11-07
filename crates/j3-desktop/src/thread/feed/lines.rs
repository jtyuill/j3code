//! Syntax colour for a tool body's text, split by line: the bodies draw one element per line
//! (gutters, signs, tinted diff rows), each coloured by its own share of the highlights.

use std::ops::Range;

use rune::{Color, Span};
use rune_kit::syntax::{Highlight, SyntaxTheme, highlight};

/// One line's colours: byte ranges of the line and their colour.
pub type LineColors = Vec<(Range<usize>, Color)>;

/// `text` highlighted as fence language `language`, by line; `None` for a language the
/// highlighter doesn't know.
pub fn code(text: &str, language: &str, theme: &SyntaxTheme) -> Option<Vec<LineColors>> {
    if language == "text" || !rune_kit::syntax::known(language) {
        return None;
    }
    Some(split_lines(text, &highlight(Some(language), text), theme))
}

/// `text` highlighted as JSON when it is a JSON object or array.
pub fn json(text: &str, theme: &SyntaxTheme) -> Option<Vec<LineColors>> {
    if !text.trim_start().starts_with(['{', '[']) {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(text).ok()?;
    code(text, "json", theme)
}

/// The code in `diff` highlighted as `language`, one entry per diff row (`str::lines`).
/// `code_of` gives a row's code, `None` for rows that are not code (hunk headers), which
/// stay plain. The rows' code reads as one document in order, so constructs spanning rows
/// colour as a whole.
pub fn diff<'a>(
    diff: &'a str,
    language: &str,
    theme: &SyntaxTheme,
    code_of: impl Fn(&'a str) -> Option<&'a str>,
) -> Option<Vec<LineColors>> {
    let mut document = String::with_capacity(diff.len());
    for (ix, row) in diff.lines().enumerate() {
        if ix > 0 {
            document.push('\n');
        }
        document.push_str(code_of(row).unwrap_or_default());
    }
    code(&document, language, theme)
}

/// Line `ix`'s colours as spans, none past the end or without highlights.
pub fn spans(lines: Option<&[LineColors]>, ix: usize) -> Vec<Span> {
    lines
        .and_then(|lines| lines.get(ix))
        .map(|line| {
            line.iter()
                .map(|(range, color)| {
                    let mut span = Span::new(range.start, range.end);
                    span.color = Some(*color);
                    span
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Splits sorted, disjoint `highlights` of `text` by line (`\n`, with a `\r` before it left
/// out), offsets relative to each line.
fn split_lines(text: &str, highlights: &[Highlight], theme: &SyntaxTheme) -> Vec<LineColors> {
    split(
        text,
        highlights
            .iter()
            .map(|(range, paint)| (range.clone(), theme.paint(*paint))),
    )
}

fn split<T: Copy>(
    text: &str,
    styles: impl Iterator<Item = (Range<usize>, T)>,
) -> Vec<Vec<(Range<usize>, T)>> {
    let mut styles = styles.peekable();
    let mut lines = Vec::new();
    let mut start = 0;
    for line in text.split('\n') {
        // `end` is the newline's offset; a style reaching past it continues.
        let end = start + line.len();
        let content_end = start + line.trim_end_matches('\r').len();
        let mut spans = Vec::new();
        while let Some((range, style)) = styles.peek() {
            if range.start > end {
                break;
            }
            let (from, to) = (range.start.max(start), range.end.min(content_end));
            if from < to {
                spans.push((from - start..to - start, *style));
            }
            if range.end > end + 1 {
                break;
            }
            styles.next();
        }
        lines.push(spans);
        start = end + 1;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_split_by_line_relative_to_each_line() {
        // "ab\r\n" "/* c" "d */ e" "" with a comment across lines 1-2.
        let text = "ab\r\n/* c\nd */ e\n";
        let styles = [(0..1, 'a'), (2..3, 'a'), (4..13, 'b'), (14..15, 'a')];
        let lines = split(text, styles.into_iter());
        assert_eq!(lines[0], [(0..1, 'a')], "\\r drops");
        assert_eq!(lines[1], [(0..4, 'b')]);
        assert_eq!(lines[2], [(0..4, 'b'), (5..6, 'a')]);
        assert!(lines[3].is_empty());
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn a_style_ending_on_a_newline_stays_on_its_line() {
        let lines = split("x\ny", [(0..2, 'a'), (2..3, 'a')].into_iter());
        assert_eq!(lines, [vec![(0..1, 'a')], vec![(0..1, 'a')]]);
    }

    #[test]
    fn json_and_known_languages_highlight_and_others_do_not() {
        let theme = SyntaxTheme::neutral(true);
        let lines = json("{\n  \"a\": 1\n}", &theme).unwrap();
        assert_eq!(lines.len(), 3);
        assert!(!lines[1].is_empty());
        assert!(json("not json", &theme).is_none());
        assert!(json("{ broken", &theme).is_none());
        assert!(code("x = 1", "text", &theme).is_none());
        assert!(code("let x = 1;", "rust", &theme).is_some());
    }
}
