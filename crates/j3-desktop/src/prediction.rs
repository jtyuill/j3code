//! Composer ghost text from omp's native word predictor (`predict_word`):
//! the rest of the word being typed, painted dim after the caret. Tab takes
//! it with a trailing space, → without one; typing through it keeps the
//! rest, typing past it rejects it. These are omp's own editor rules
//! (`packages/tui/src/components/editor.ts`), kept free of GPUI so they can
//! be tested as plain text edits.

use std::ops::Range;

/// Typed characters that replace the provisional space a Tab accept adds,
/// so they attach to the accepted word: closing punctuation, and `-` for
/// compounds. A newline drops the space as well. Quotes are left out: they
/// may open a quote. (omp's `PROVISIONAL_SPACE_ABSORBERS`.)
const SPACE_ABSORBERS: &[char] = &['-', '.', ',', ';', ':', '!', '?', ')', ']', '}', '\n'];

/// A suggestion shown after the caret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ghost {
    /// The draft it follows.
    pub text: String,
    /// The caret it follows, a byte offset into `text`.
    pub cursor: usize,
    /// The untyped rest of the suggested word.
    pub suffix: String,
}

impl Ghost {
    /// Whether this ghost still belongs to the draft `text` with the caret
    /// at `cursor`.
    pub fn matches(&self, text: &str, cursor: usize) -> bool {
        self.cursor == cursor && self.text == text
    }

    /// What the draft became, relative to this ghost.
    pub fn follow(&self, text: &str, cursor: usize) -> Follow {
        if self.matches(text, cursor) {
            return Follow::Unchanged;
        }
        let Some(typed) = typed_at(&self.text, self.cursor, text, cursor) else {
            return Follow::Unrelated;
        };
        let mut suffix = self.suffix.chars();
        for typed in typed.chars() {
            match suffix.next() {
                Some(next) if same_letter(next, typed) => {}
                _ => return Follow::Diverged,
            }
        }
        let rest = suffix.as_str();
        Follow::TypedThrough((!rest.is_empty()).then(|| Ghost {
            text: text.to_owned(),
            cursor,
            suffix: rest.to_owned(),
        }))
    }
}

/// How the draft moved on from a [`Ghost`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Follow {
    /// Same draft, same caret.
    Unchanged,
    /// The user typed the start of the suggestion; the rest, if any, is
    /// still the ghost.
    TypedThrough(Option<Ghost>),
    /// The user typed something else at the caret: the suggestion was
    /// rejected.
    Diverged,
    /// Any other edit or caret move; it says nothing about the suggestion.
    Unrelated,
}

/// The draft right after a Tab accept, whose trailing space stays
/// provisional until the next keystroke: a typed space is swallowed, and
/// closing punctuation, a hyphen or a newline replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provisional {
    pub text: String,
    /// The caret, right after the space.
    pub cursor: usize,
}

impl Provisional {
    /// The edit that settles the provisional space now that the draft is
    /// `text` with the caret at `cursor`: the byte range of `text` to
    /// replace and its replacement. `None` when the space stays.
    pub fn settle(&self, text: &str, cursor: usize) -> Option<(Range<usize>, String)> {
        let typed = typed_at(&self.text, self.cursor, text, cursor)?;
        let mut chars = typed.chars();
        let (Some(typed), None) = (chars.next(), chars.next()) else {
            return None;
        };
        let at = self.cursor;
        if typed == ' ' {
            // The typed space goes; the provisional one stays.
            return Some((at..at + 1, String::new()));
        }
        SPACE_ABSORBERS
            .contains(&typed)
            .then(|| (at - 1..at + typed.len_utf8(), typed.to_string()))
    }
}

/// `text` with the caret at `cursor` is `before` with something inserted at
/// `at` and the caret after it: the inserted text.
fn typed_at<'a>(before: &str, at: usize, text: &'a str, cursor: usize) -> Option<&'a str> {
    if cursor <= at || text.len() != before.len() + (cursor - at) {
        return None;
    }
    (text.get(..at)? == before.get(..at)? && text.get(cursor..)? == before.get(at..)?)
        .then(|| text.get(at..cursor))
        .flatten()
}

/// Case-insensitive, like omp's typed-past check.
fn same_letter(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// Whether `cursor` ends its line: the only place ghost text renders, and
/// the only place omp predicts.
pub fn at_line_end(text: &str, cursor: usize) -> bool {
    text.get(cursor..)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('\n'))
}

/// `byte` (a byte offset into `text`) as the UTF-16 offset omp's RPC
/// takes.
pub fn utf16_offset(text: &str, byte: usize) -> usize {
    text.get(..byte)
        .map_or(0, |head| head.encode_utf16().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ghost(text: &str, suffix: &str) -> Ghost {
        Ghost {
            text: text.into(),
            cursor: text.len(),
            suffix: suffix.into(),
        }
    }

    #[test]
    fn typing_through_keeps_the_rest_of_the_word() {
        let shown = ghost("please refac", "tor");
        assert_eq!(
            shown.follow("please refact", 13),
            Follow::TypedThrough(Some(ghost("please refact", "or")))
        );
        // Case does not matter, as in omp; the shown suffix keeps its case.
        assert_eq!(
            ghost("the ", "Quick").follow("the qu", 6),
            Follow::TypedThrough(Some(ghost("the qu", "ick")))
        );
        // Typing the whole word leaves nothing to show.
        assert_eq!(
            shown.follow("please refactor", 15),
            Follow::TypedThrough(None)
        );
    }

    #[test]
    fn typing_past_rejects_and_other_edits_say_nothing() {
        let shown = ghost("please refac", "tor");
        assert_eq!(shown.follow("please refacs", 13), Follow::Diverged);
        assert_eq!(shown.follow("please refac", 12), Follow::Unchanged);
        // Deleting, a caret move, an edit elsewhere.
        assert_eq!(shown.follow("please refa", 11), Follow::Unrelated);
        assert_eq!(shown.follow("please refac", 3), Follow::Unrelated);
        assert_eq!(shown.follow("xplease refac", 13), Follow::Unrelated);
    }

    #[test]
    fn typing_through_multibyte_text() {
        let shown = ghost("café ", "crème");
        assert_eq!(
            shown.follow("café cr", "café cr".len()),
            Follow::TypedThrough(Some(ghost("café cr", "ème")))
        );
        assert_eq!(
            shown.follow("café crè", "café crè".len()),
            Follow::TypedThrough(Some(ghost("café crè", "me")))
        );
    }

    #[test]
    fn provisional_space_swallows_a_space_and_yields_to_punctuation() {
        let space = Provisional {
            text: "fix the bug ".into(),
            cursor: 12,
        };
        assert_eq!(
            space.settle("fix the bug  ", 13),
            Some((12..13, String::new()))
        );
        assert_eq!(
            space.settle("fix the bug ,", 13),
            Some((11..13, ",".into()))
        );
        assert_eq!(
            space.settle("fix the bug -", 13),
            Some((11..13, "-".into()))
        );
        assert_eq!(
            space.settle("fix the bug \n", 13),
            Some((11..13, "\n".into()))
        );
        // Anything else keeps it, including a quote.
        assert_eq!(space.settle("fix the bug a", 13), None);
        assert_eq!(space.settle("fix the bug \"", 13), None);
        // Only the very next keystroke at the caret counts.
        assert_eq!(space.settle("fix the bug ab", 14), None);
        assert_eq!(space.settle("fix the bug", 11), None);
    }

    #[test]
    fn line_ends_and_utf16_offsets() {
        assert!(at_line_end("ab", 2));
        assert!(at_line_end("ab\ncd", 2));
        assert!(!at_line_end("ab\ncd", 1));
        assert!(!at_line_end("é", 1));
        // `é` is two bytes and one UTF-16 unit; `🙂` is four bytes, two units.
        assert_eq!(utf16_offset("é🙂x", "é🙂".len()), 3);
        assert_eq!(utf16_offset("abc", 99), 0);
    }
}
