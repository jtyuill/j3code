//! omp's `^model` mention syntax: finding mentions in composer text and the
//! `^query` being typed at the cursor.
//!
//! A mention is `^selector` where the selector is a model slug
//! (`provider/model-id`). omp expands whitespace-delimited tokens itself on
//! submit, so this module only mirrors its matching rules for presentation.
//! All ranges are UTF-8 byte offsets into the given text.

use std::ops::Range;

/// A `^selector` token inside a draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention<'a> {
    /// Bytes of `^selector`; the leading delimiter is not included.
    pub range: Range<usize>,
    pub selector: &'a str,
}

/// Whether mentions are interpreted in `text`. omp never expands them in
/// local-execution drafts (those starting with `!` or `$`).
pub fn allowed(text: &str) -> bool {
    !text.trim_start().starts_with(['!', '$'])
}

/// omp's `(^|\s)\^([^\s^]+)(?=\s|$)` over `text`; empty when mentions are not
/// [`allowed`].
pub fn mentions(text: &str) -> Vec<Mention<'_>> {
    let mut found = Vec::new();
    if !allowed(text) {
        return found;
    }
    let mut prev_boundary = true;
    let mut skip_until = 0;
    for (start, ch) in text.char_indices() {
        let boundary = prev_boundary;
        prev_boundary = ch.is_whitespace();
        if start < skip_until || ch != '^' || !boundary {
            continue;
        }
        let rest = &text[start + 1..];
        let len = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let selector = &rest[..len];
        if selector.is_empty() || selector.contains('^') {
            continue;
        }
        let end = start + 1 + len;
        found.push(Mention {
            range: start..end,
            selector,
        });
        skip_until = end;
    }
    found
}

/// omp's `/(?:^|\s)(\^[^\s]*)$/` applied to `text[..cursor]`: the `^query`
/// token ending at `cursor` and the query after the `^`. `None` when mentions
/// are not allowed, `cursor` is not a char boundary, or no such token ends
/// there.
pub fn query_at(text: &str, cursor: usize) -> Option<(Range<usize>, &str)> {
    if !allowed(text) || !text.is_char_boundary(cursor) {
        return None;
    }
    let prefix = &text[..cursor];
    let start = prefix
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let token = &prefix[start..];
    let query = token.strip_prefix('^')?;
    Some((start..cursor, query))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sels(text: &str) -> Vec<&str> {
        mentions(text).iter().map(|m| m.selector).collect()
    }

    #[test]
    fn mentions_at_start_after_whitespace_and_at_end() {
        let text = "^a/b hello ^c/d";
        let found = mentions(text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].range, 0..4);
        assert_eq!(&text[found[1].range.clone()], "^c/d");
        assert_eq!(found[1].selector, "c/d");
        assert_eq!(sels("see ^x/y now"), ["x/y"]);
    }

    #[test]
    fn mentions_reject_malformed_tokens() {
        assert!(mentions("a^b").is_empty());
        assert!(mentions("^^x").is_empty());
        assert!(mentions("^").is_empty());
        assert!(mentions("x ^ y").is_empty());
        assert!(mentions("^a^b").is_empty());
    }

    #[test]
    fn mentions_handle_whitespace_kinds_and_multibyte() {
        assert_eq!(sels("a\t^x/y\n^z/w"), ["x/y", "z/w"]);
        let text = "é ^pröv/モデル ok";
        let found = mentions(text);
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].range.clone()], "^pröv/モデル");
        assert_eq!(found[0].selector, "pröv/モデル");
    }

    #[test]
    fn mentions_disabled_for_local_execution_drafts() {
        assert!(mentions("!ls ^a/b").is_empty());
        assert!(mentions("  $x ^a/b").is_empty());
        assert!(!allowed("\n!x"));
        assert!(allowed("hi !x"));
    }

    #[test]
    fn query_at_token_end() {
        let text = "Have ^";
        let (range, q) = query_at(text, text.len()).unwrap();
        assert_eq!((range, q), (5..6, ""));
        let text = "Have ^anthr";
        let (range, q) = query_at(text, text.len()).unwrap();
        assert_eq!((range, q), (5..11, "anthr"));
    }

    #[test]
    fn query_at_mid_token_uses_prefix() {
        let text = "Have ^anthr";
        let (range, q) = query_at(text, 8).unwrap();
        assert_eq!((range, q), (5..8, "an"));
        let (range, q) = query_at("^ab", 2).unwrap();
        assert_eq!((range, q), (0..2, "a"));
    }

    #[test]
    fn query_at_rejects_non_tokens() {
        assert_eq!(query_at("a^b", 3), None);
        let text = "^a/b done";
        assert_eq!(query_at(text, text.len()), None);
        assert_eq!(query_at("!x ^a", 5), None);
        assert_eq!(query_at("é^", 1), None);
        assert_eq!(query_at("ab", 9), None);
    }
}
