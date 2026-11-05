//! Syntax colouring shared by the file pane and the feed's tool output: the
//! extension → highlighter language map, and per-line tree-sitter highlights
//! of whole texts, cached so an unchanged text is parsed once.

use std::{
    cell::RefCell,
    collections::{HashMap, hash_map::Entry},
    hash::{DefaultHasher, Hash as _, Hasher as _},
    ops::Range,
    rc::Rc,
    sync::Arc,
};

use gpui_kit::HighlightStyle;
use gpui_kit::component::Rope;
use gpui_kit::component::highlighter::{HighlightTheme, LanguageRegistry, SyntaxHighlighter};

/// Highlighter languages by the file extensions that select them.
const LANGUAGES: &[(&[&str], &str)] = &[
    (&["rs"], "rust"),
    (&["ts", "mts", "cts"], "typescript"),
    (&["tsx"], "tsx"),
    (&["js", "mjs", "cjs", "jsx"], "javascript"),
    (&["json", "jsonc"], "json"),
    (&["toml"], "toml"),
    (&["md", "markdown", "mdx"], "markdown"),
    (&["py", "pyi"], "python"),
    (&["sh", "bash", "zsh"], "bash"),
    (&["c", "h"], "c"),
    (&["cpp", "cc", "cxx", "hpp", "hh"], "cpp"),
    (&["css"], "css"),
    (&["go"], "go"),
    (&["html", "htm"], "html"),
    (&["java"], "java"),
    (&["sql"], "sql"),
    (&["yaml", "yml"], "yaml"),
];

/// The highlighter language of `path`, from its extension; `"text"` when
/// there is none for it.
pub fn language(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "text";
    };
    LANGUAGES
        .iter()
        .find(|(extensions, _)| {
            extensions
                .iter()
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
        .map_or("text", |(_, language)| *language)
}

/// The styled spans of one line, offsets relative to the line.
pub type Spans = [(Range<usize>, HighlightStyle)];

/// A text's highlights split by line.
#[derive(Debug, PartialEq)]
pub struct Lines(Vec<Vec<(Range<usize>, HighlightStyle)>>);

impl Lines {
    /// The spans of line `ix` (as `str::lines` counts lines); none past the
    /// end.
    pub fn line(&self, ix: usize) -> &Spans {
        self.0.get(ix).map_or(&[], Vec::as_slice)
    }
}

/// `text` highlighted as `language`; `None` without a grammar for it.
pub fn code(text: &str, language: &'static str, theme: &Arc<HighlightTheme>) -> Option<Rc<Lines>> {
    if language == "text" {
        return None;
    }
    cached(text, Kind::Code(language), theme, |highlighters| {
        highlight(highlighters, text, language, theme)
    })
}

/// `text` highlighted as JSON when it is a JSON object or array; `None`
/// otherwise.
pub fn json(text: &str, theme: &Arc<HighlightTheme>) -> Option<Rc<Lines>> {
    if !text.trim_start().starts_with(['{', '[']) {
        return None;
    }
    cached(text, Kind::Json, theme, |highlighters| {
        serde_json::from_str::<serde_json::Value>(text).ok()?;
        highlight(highlighters, text, "json", theme)
    })
}

/// The code in `diff` highlighted as `language`, one line per diff row
/// (`str::lines`). `code` gives a row's code text, `None` for rows that are
/// not code (hunk headers), which stay unstyled. The rows' code reads as one
/// document in order, so constructs spanning rows colour as a whole.
pub fn diff<'a>(
    diff: &'a str,
    language: &'static str,
    theme: &Arc<HighlightTheme>,
    code: impl Fn(&'a str) -> Option<&'a str>,
) -> Option<Rc<Lines>> {
    if language == "text" {
        return None;
    }
    cached(diff, Kind::Diff(language), theme, |highlighters| {
        let mut document = String::with_capacity(diff.len());
        for (ix, row) in diff.lines().enumerate() {
            if ix > 0 {
                document.push('\n');
            }
            document.push_str(code(row).unwrap_or_default());
        }
        highlight(highlighters, &document, language, theme)
    })
}

/// `line` with each tab drawn as four spaces, and `spans` moved to match.
pub fn expand_tabs(line: &str, spans: &Spans) -> (String, Vec<(Range<usize>, HighlightStyle)>) {
    let shift = |offset: usize| {
        offset
            + 3 * line.as_bytes()[..offset]
                .iter()
                .filter(|&&b| b == b'\t')
                .count()
    };
    let spans = spans
        .iter()
        .map(|(range, style)| (shift(range.start)..shift(range.end), *style))
        .collect();
    (line.replace('\t', "    "), spans)
}

/// What a cached text was highlighted as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Code(&'static str),
    Diff(&'static str),
    Json,
}

/// Texts kept per cache generation. Lookups move entries of the previous
/// generation into the current one, which retires the previous once full,
/// so the cache holds at most twice this many and keeps what is drawn.
const GENERATION: usize = 256;

/// Highlights of recently drawn texts, keyed by the text's hash and kind.
#[derive(Default)]
struct Cache {
    /// The theme the entries are styled with. Held so its address stays
    /// unique while compared.
    theme: Option<Arc<HighlightTheme>>,
    current: HashMap<(u64, Kind), Option<Rc<Lines>>>,
    previous: HashMap<(u64, Kind), Option<Rc<Lines>>>,
    /// One highlighter per language, reused: building one compiles its
    /// queries.
    highlighters: HashMap<&'static str, SyntaxHighlighter>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::default();
}

/// The cached highlights of `text` as `kind`, from `compute` on a miss.
fn cached(
    text: &str,
    kind: Kind,
    theme: &Arc<HighlightTheme>,
    compute: impl FnOnce(&mut HashMap<&'static str, SyntaxHighlighter>) -> Option<Lines>,
) -> Option<Rc<Lines>> {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let key = (hasher.finish(), kind);
    CACHE.with_borrow_mut(|cache| {
        if !cache
            .theme
            .as_ref()
            .is_some_and(|cached| Arc::ptr_eq(cached, theme))
        {
            cache.current.clear();
            cache.previous.clear();
            cache.theme = Some(theme.clone());
        }
        if let Some(lines) = cache.current.get(&key) {
            return lines.clone();
        }
        let lines = match cache.previous.remove(&key) {
            Some(lines) => lines,
            None => compute(&mut cache.highlighters).map(Rc::new),
        };
        if cache.current.len() >= GENERATION {
            cache.previous = std::mem::take(&mut cache.current);
        }
        cache.current.insert(key, lines.clone());
        lines
    })
}

/// Parses `text` as `language` and splits its highlights by line; `None`
/// when the language is not registered.
fn highlight(
    highlighters: &mut HashMap<&'static str, SyntaxHighlighter>,
    text: &str,
    language: &'static str,
    theme: &HighlightTheme,
) -> Option<Lines> {
    let highlighter = match highlighters.entry(language) {
        Entry::Occupied(entry) => entry.into_mut(),
        Entry::Vacant(entry) => {
            LanguageRegistry::singleton().language(language)?;
            entry.insert(SyntaxHighlighter::new(language))
        }
    };
    // An unknown change: the whole text is parsed afresh.
    highlighter.update(None, &Rope::from_str(text), None);
    let styles = highlighter.styles(&(0..text.len()), theme);
    Some(Lines(split_lines(text, &styles)))
}

/// Splits sorted, disjoint `styles` of `text` by line (`\n`, with a `\r`
/// before it left out), offsets relative to each line. Unstyled spans are
/// dropped.
fn split_lines(
    text: &str,
    styles: &[(Range<usize>, HighlightStyle)],
) -> Vec<Vec<(Range<usize>, HighlightStyle)>> {
    let mut styles = styles
        .iter()
        .filter(|(_, style)| *style != HighlightStyle::default())
        .peekable();
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
    use gpui_kit::{FontWeight, hsla};

    use super::*;

    #[test]
    fn languages_come_from_extensions() {
        assert_eq!(language("src/main.rs"), "rust");
        assert_eq!(language("web/App.TSX"), "tsx");
        assert_eq!(language("include/a.HPP"), "cpp");
        assert_eq!(language("lib/x.h"), "c");
        assert_eq!(language(".github/ci.yml"), "yaml");
        assert_eq!(language("db/schema.sql"), "sql");
        assert_eq!(language("index.htm"), "html");
        assert_eq!(language("settings.jsonc"), "json");
        assert_eq!(language("Makefile"), "text");
        assert_eq!(language("v1.2/README"), "text");
        assert_eq!(language("notes.txt"), "text");
    }

    #[test]
    fn highlights_split_by_line_relative_to_each_line() {
        let a = HighlightStyle {
            color: Some(hsla(0., 1., 0.5, 1.)),
            ..Default::default()
        };
        let b = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..Default::default()
        };
        // "ab\r\n" "/* c" "d */ e" "" with a comment across lines 1-2.
        let text = "ab\r\n/* c\nd */ e\n";
        let styles = [
            (0..1, a),
            (1..2, HighlightStyle::default()),
            (2..3, a),
            (4..13, b),
            (14..15, a),
        ];
        let lines = Lines(split_lines(text, &styles));
        assert_eq!(lines.line(0), [(0..1, a)], "\\r and unstyled spans drop");
        assert_eq!(lines.line(1), [(0..4, b)]);
        assert_eq!(lines.line(2), [(0..4, b), (5..6, a)]);
        assert!(lines.line(3).is_empty());
        assert!(lines.line(9).is_empty(), "past the end");
    }

    #[test]
    fn a_style_ending_on_a_newline_stays_on_its_line() {
        let a = HighlightStyle {
            color: Some(hsla(0., 1., 0.5, 1.)),
            ..Default::default()
        };
        let lines = split_lines("x\ny", &[(0..2, a), (2..3, a)]);
        assert_eq!(lines, [vec![(0..1, a)], vec![(0..1, a)]]);
    }

    #[test]
    fn tabs_expand_with_their_spans() {
        let a = HighlightStyle {
            color: Some(hsla(0., 1., 0.5, 1.)),
            ..Default::default()
        };
        let (line, spans) = expand_tabs("\tx\ty", &[(1..2, a), (3..4, a)]);
        assert_eq!(line, "    x    y");
        assert_eq!(spans, [(4..5, a), (9..10, a)]);
    }
}
