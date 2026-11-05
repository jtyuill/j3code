//! omp "magic keywords": `ultrathink`, `orchestrate`, `workflowz`, `jevify`.
//!
//! A keyword counts only as exact-case, standalone prose, mirroring omp's
//! `magicKeywordRegex` and `maskNonProse`:
//!
//! - LEFT boundary: not preceded by a letter, digit, `_`, `.`, `/`, `\` or `-`,
//!   and not preceded by `::`.
//! - RIGHT boundary: not followed by a letter, digit, `_`, `/`, `\` or `-`, not
//!   followed by `.` plus a letter, digit, `_` or `-`, and not followed by `(`.
//! - Matching runs over a masked copy of the text in which fenced code blocks
//!   (backtick or tilde fences), inline code spans (matching backtick-run
//!   lengths) and HTML/XML comments, tags and the elements they enclose are
//!   blanked to spaces. Every byte of a masked char is replaced, so the masked
//!   string is valid UTF-8 and byte offsets map 1:1 onto the original text.
//!
//! The gradient palette follows omp's `gradient-highlight.ts`: 14 stops at
//! HSL saturation 0.90 and lightness 0.62, swept across the word by a phase
//! that the editor advances every [`SHIMMER_FRAME`].

use std::ops::Range;
use std::time::Duration;

use gpui_kit::{Hsla, hsla};

/// One magic keyword and the hue span of its gradient, in degrees.
pub struct Keyword {
    pub word: &'static str,
    pub hue_from: f32,
    pub hue_to: f32,
}

pub static KEYWORDS: [Keyword; 4] = [
    Keyword {
        word: "ultrathink",
        hue_from: 0.0,
        hue_to: 330.0,
    },
    Keyword {
        word: "orchestrate",
        hue_from: 150.0,
        hue_to: 280.0,
    },
    Keyword {
        word: "workflowz",
        hue_from: 30.0,
        hue_to: 150.0,
    },
    Keyword {
        word: "jevify",
        hue_from: 300.0,
        hue_to: 420.0,
    },
];

/// A keyword occurrence; `range` is a byte range into the searched text.
pub struct Found {
    pub range: Range<usize>,
    pub keyword: &'static Keyword,
}

/// Interval between shimmer repaints while the editor is focused.
pub const SHIMMER_FRAME: Duration = Duration::from_millis(70);

/// Duration of one full gradient sweep.
const SHIMMER_PERIOD_MS: u128 = 1800;

/// Number of palette stops per keyword.
const STOPS: usize = 14;

/// Shimmer phase in `[0, 1)` after `elapsed` of animation.
pub fn shimmer_phase(elapsed: Duration) -> f32 {
    (elapsed.as_millis() % SHIMMER_PERIOD_MS) as f32 / SHIMMER_PERIOD_MS as f32
}

/// One colour per `char` of `keyword.word`, with the gradient rotated by
/// `phase` (any finite value; it wraps into `[0, 1)`).
pub fn colors(keyword: &Keyword, phase: f32) -> Vec<Hsla> {
    let palette: Vec<Hsla> = (0..STOPS)
        .map(|i| {
            let hue = (keyword.hue_from
                + (i as f32 / STOPS as f32) * (keyword.hue_to - keyword.hue_from))
                % 360.0;
            hsla(hue / 360.0, 0.90, 0.62, 1.0)
        })
        .collect();
    let wrapped = ((phase % 1.0) + 1.0) % 1.0;
    let n = keyword.word.chars().count();
    (0..n)
        .map(|i| {
            let t = (i as f32 / n as f32 + wrapped) % 1.0;
            palette[(t * STOPS as f32) as usize % STOPS]
        })
        .collect()
}

/// Standalone-prose keywords in `text`, sorted by `range.start`.
pub fn find(text: &str) -> Vec<Found> {
    if !KEYWORDS.iter().any(|k| text.contains(k.word)) {
        return Vec::new();
    }
    let masked = mask_non_prose(text);
    let mut found = Vec::new();
    for keyword in &KEYWORDS {
        for (start, word) in masked.match_indices(keyword.word) {
            let end = start + word.len();
            if left_ok(&masked, start) && right_ok(&masked, end) {
                found.push(Found {
                    range: start..end,
                    keyword,
                });
            }
        }
    }
    found.sort_by_key(|f| f.range.start);
    found
}

fn left_ok(text: &str, start: usize) -> bool {
    let before = &text[..start];
    if before.ends_with("::") {
        return false;
    }
    match before.chars().next_back() {
        Some(c) => !(c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '\\' | '-')),
        None => true,
    }
}

fn right_ok(text: &str, end: usize) -> bool {
    let mut after = text[end..].chars();
    match after.next() {
        None => true,
        Some(c) if c.is_alphanumeric() || matches!(c, '_' | '/' | '\\' | '-') => false,
        Some('(') => false,
        Some('.') => !matches!(
            after.next(),
            Some(d) if d.is_alphanumeric() || matches!(d, '_' | '-')
        ),
        Some(_) => true,
    }
}

/// Copy of `text` with non-prose regions blanked to spaces (newlines kept).
/// Every delimiter that opens or closes a region is ASCII, so region edges
/// always fall on char boundaries and the result is valid UTF-8.
fn mask_non_prose(text: &str) -> String {
    if !text.contains('`') && !text.contains('<') && !text.contains("~~~") {
        return text.to_owned();
    }
    let b = text.as_bytes();
    let n = b.len();
    let mut masked = vec![false; n];

    mask_fences(text, &mut masked);

    let mut i = 0;
    while i < n {
        if masked[i] {
            i += 1;
            continue;
        }
        match b[i] {
            b'`' => {
                let run_end = backtick_run_end(b, i);
                match find_backtick_close(b, run_end, run_end - i, &masked) {
                    Some(close) => {
                        masked[i..close].fill(true);
                        i = close;
                    }
                    None => i = run_end,
                }
            }
            b'<' => {
                let end = mask_tag_at(b, i, &mut masked);
                i = if end > i { end } else { i + 1 };
            }
            _ => i += 1,
        }
    }

    let out: Vec<u8> = b
        .iter()
        .zip(&masked)
        .map(|(&c, &m)| if m && c != b'\n' { b' ' } else { c })
        .collect();
    String::from_utf8(out).expect("masked regions are delimited by ASCII bytes")
}

/// Phase 1: fenced code blocks, line by line.
fn mask_fences(text: &str, masked: &mut [bool]) {
    let mut fence: Option<(u8, usize)> = None;
    let mut line_start = 0;
    loop {
        let nl = text[line_start..]
            .find('\n')
            .map_or(text.len(), |p| line_start + p);
        let line = &text[line_start..nl];
        let open = fence_marker(line);
        match fence {
            Some((ch, len)) => {
                masked[line_start..nl].fill(true);
                // A closing fence: same char, at least as long, nothing else on the line.
                if let Some((indent, marker)) = open
                    && line.as_bytes()[indent] == ch
                    && marker >= len
                    && line[indent + marker..].trim().is_empty()
                {
                    fence = None;
                }
            }
            None => {
                if let Some((indent, marker)) = open {
                    let ch = line.as_bytes()[indent];
                    // A backtick fence's info string may not contain a backtick.
                    if !(ch == b'`' && line[indent + marker..].contains('`')) {
                        fence = Some((ch, marker));
                        masked[line_start..nl].fill(true);
                    }
                }
            }
        }
        if nl == text.len() {
            break;
        }
        line_start = nl + 1;
    }
}

/// `FENCE_RE` (`^( {0,3})([`~]{3,})`): returns (indent width, marker length).
fn fence_marker(line: &str) -> Option<(usize, usize)> {
    let b = line.as_bytes();
    let indent = b.iter().take(3).take_while(|&&c| c == b' ').count();
    let marker = b[indent..]
        .iter()
        .take_while(|&&c| c == b'`' || c == b'~')
        .count();
    (marker >= 3).then_some((indent, marker))
}

fn backtick_run_end(b: &[u8], i: usize) -> usize {
    i + b[i..].iter().take_while(|&&c| c == b'`').count()
}

/// End of the closing backtick run of exactly `run_len` backticks at or after
/// `from`, skipping already-masked bytes.
fn find_backtick_close(b: &[u8], from: usize, run_len: usize, masked: &[bool]) -> Option<usize> {
    let mut k = from;
    while k < b.len() {
        if masked[k] {
            k += 1;
        } else if b[k] == b'`' {
            let e = backtick_run_end(b, k);
            if e - k == run_len {
                return Some(e);
            }
            k = e;
        } else {
            k += 1;
        }
    }
    None
}

/// End (exclusive) of the tag name starting at `at`, if one starts there.
fn tag_name_end(b: &[u8], at: usize) -> Option<usize> {
    if !b.get(at)?.is_ascii_alphabetic() {
        return None;
    }
    Some(
        at + 1
            + b[at + 1..]
                .iter()
                .take_while(|&&c| c.is_ascii_alphanumeric() || c == b'-')
                .count(),
    )
}

/// Index of the `>` closing a tag whose attributes begin at `j`, honoring
/// quoted values; `None` for a malformed tag.
fn find_tag_end(b: &[u8], j: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (k, &ch) in b.iter().enumerate().skip(j) {
        if quote != 0 {
            if ch == quote {
                quote = 0;
            }
            continue;
        }
        match ch {
            b'"' | b'\'' => quote = ch,
            b'>' => return Some(k),
            b'<' => return None,
            _ => {}
        }
    }
    None
}

/// Index just past the `</name>` balancing an opening tag whose content starts
/// at `start`, counting nested same-name tags.
fn find_matching_close(b: &[u8], start: usize, name: &[u8], masked: &[bool]) -> Option<usize> {
    let mut depth = 1usize;
    let mut k = start;
    while k < b.len() {
        if masked[k] || b[k] != b'<' {
            k += 1;
            continue;
        }
        let mut m = k + 1;
        let is_close = b.get(m) == Some(&b'/');
        if is_close {
            m += 1;
        }
        let Some(name_end) = tag_name_end(b, m) else {
            k += 1;
            continue;
        };
        let Some(gt) = find_tag_end(b, name_end) else {
            k += 1;
            continue;
        };
        if b[m..name_end].eq_ignore_ascii_case(name) {
            if is_close {
                depth -= 1;
                if depth == 0 {
                    return Some(gt + 1);
                }
            } else if b[gt - 1] != b'/' {
                depth += 1;
            }
        }
        k = gt + 1;
    }
    None
}

/// Mask the HTML/XML construct at `<` (index `i`); returns the index past the
/// masked region, or `i` when the `<` does not begin a tag.
fn mask_tag_at(b: &[u8], i: usize, masked: &mut [bool]) -> usize {
    if b[i..].starts_with(b"<!--") {
        let stop = b[i + 4..]
            .windows(3)
            .position(|w| w == b"-->")
            .map_or(b.len(), |p| i + 4 + p + 3);
        masked[i..stop].fill(true);
        return stop;
    }
    let mut j = i + 1;
    let closing = b.get(j) == Some(&b'/');
    if closing {
        j += 1;
    }
    let Some(name_end) = tag_name_end(b, j) else {
        return i;
    };
    let Some(gt) = find_tag_end(b, name_end) else {
        return i;
    };
    let tag_end = gt + 1;
    let self_closing = b[gt - 1] == b'/';
    masked[i..tag_end].fill(true);
    if closing || self_closing {
        return tag_end;
    }
    match find_matching_close(b, tag_end, &b[j..name_end], masked) {
        Some(close) => {
            masked[tag_end..close].fill(true);
            close
        }
        None => tag_end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<&'static str> {
        find(text).iter().map(|f| f.keyword.word).collect()
    }

    fn hue(c: Hsla) -> f32 {
        c.h * 360.0
    }

    #[test]
    fn matches_at_prose_boundaries() {
        assert_eq!(words("please orchestrate, then go"), ["orchestrate"]);
        assert_eq!(words("now orchestrate."), ["orchestrate"]);
        assert_eq!(words("use \"workflowz\" here"), ["workflowz"]);
        let text = "x orchestrate.";
        assert_eq!(find(text)[0].range, 2..13);
    }

    #[test]
    fn rejects_bound_or_cased_words() {
        for text in [
            "orchestrated",
            "orchestrate.ts",
            "foo::orchestrate",
            "orchestrate()",
            "Orchestrate",
            "my-jevify",
            "a/ultrathink",
            "_jevify",
        ] {
            assert!(find(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn ignores_code_and_markup() {
        for text in [
            "```\nultrathink\n```\nultrathink",
            "~~~\nultrathink\n~~~\nultrathink",
            "`ultrathink` ultrathink",
            "<tag>ultrathink</tag> ultrathink",
            "<!-- ultrathink --> ultrathink",
        ] {
            let found = find(text);
            assert_eq!(found.len(), 1, "{text}");
            assert_eq!(found[0].range.end, text.len(), "{text}");
        }
    }

    #[test]
    fn returns_all_keywords_in_order() {
        let found = find("jevify then ultrathink");
        let words: Vec<_> = found.iter().map(|f| f.keyword.word).collect();
        assert_eq!(words, ["jevify", "ultrathink"]);
        assert!(found[0].range.start < found[1].range.start);
    }

    #[test]
    fn ranges_use_original_byte_offsets() {
        let text = "héllo ultrathink";
        let found = find(text);
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].range.clone()], "ultrathink");

        let text = "`é` ünï ultrathink";
        let found = find(text);
        assert_eq!(&text[found[0].range.clone()], "ultrathink");
    }

    #[test]
    fn colors_cover_each_char_and_wrap_phase() {
        let k = &KEYWORDS[1];
        let cs = colors(k, 0.0);
        assert_eq!(cs.len(), k.word.chars().count());
        assert!((hue(cs[0]) - k.hue_from).abs() < 0.01);
        assert_eq!(colors(k, -0.25), colors(k, 0.75));
        assert_eq!(colors(k, 1.25), colors(k, 0.25));
    }

    #[test]
    fn shimmer_phase_wraps_each_period() {
        assert_eq!(shimmer_phase(Duration::from_millis(0)), 0.0);
        assert_eq!(shimmer_phase(Duration::from_millis(900)), 0.5);
        assert_eq!(shimmer_phase(Duration::from_millis(1800)), 0.0);
    }
}
