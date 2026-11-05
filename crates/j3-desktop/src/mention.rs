//! omp's `^model` mention syntax: finding mentions in composer text, the
//! `^query` being typed at the cursor, and ranking models for the picker.
//!
//! A mention is `^selector` where the selector is a model slug
//! (`provider/model-id`). omp expands whitespace-delimited tokens itself on
//! submit, so this module only mirrors its matching rules for presentation.
//! All ranges are UTF-8 byte offsets into the given text.

use std::ops::Range;

use crate::models::ModelChoice;

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

/// omp's `modelMentionDisplayName`: the trimmed name, else the id after the
/// first `/` of the slug; `"` removed. The base of [`crate::models::ModelLabel`],
/// which is what the UI shows.
pub fn display_name(choice: &ModelChoice) -> String {
    let name = choice.name.trim();
    let base = if name.is_empty() { choice.id() } else { name };
    base.replace('"', "")
}

/// The model whose slug is exactly `selector`.
pub fn find_choice<'a>(choices: &'a [ModelChoice], selector: &str) -> Option<&'a ModelChoice> {
    choices.iter().find(|c| c.slug() == selector)
}

/// Indices into `choices` for the `^` picker, best first, at most `limit`.
///
/// An empty query lists favourites then the rest. Otherwise each
/// space-separated token must match, tiered as prefix (slug, id or display
/// name), contains (slug or name) or subsequence (id or name, never across
/// the provider); a choice scores the sum of its tokens' tiers, so more
/// prefix hits rank higher. Equal scores list favourites first, then
/// `choices` order.
pub fn rank(
    choices: &[ModelChoice],
    favorites: &[String],
    query: &str,
    limit: usize,
) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let tokens: Vec<&str> = query.split_whitespace().collect();
    let mut scored: Vec<(u32, bool, usize)> = choices
        .iter()
        .enumerate()
        .filter_map(|(i, choice)| {
            let favorite = favorites.iter().any(|f| f == choice.slug());
            let score = if tokens.is_empty() {
                0
            } else {
                let fields = Fields::new(choice);
                tokens
                    .iter()
                    .map(|token| fields.tier(token))
                    .try_fold(0u32, |sum, tier| tier.map(|tier| sum + u32::from(tier)))?
            };
            Some((score, !favorite, i))
        })
        .collect();
    scored.sort_unstable();
    scored.into_iter().take(limit).map(|(_, _, i)| i).collect()
}

/// Lowercased searchable fields of one model.
struct Fields {
    slug: String,
    id: String,
    name: String,
}

impl Fields {
    fn new(choice: &ModelChoice) -> Self {
        Self {
            slug: choice.slug().to_lowercase(),
            id: choice.id().to_lowercase(),
            name: display_name(choice).to_lowercase(),
        }
    }

    /// 1 prefix, 2 contains, 3 subsequence; `None` when nothing matches.
    fn tier(&self, token: &str) -> Option<u8> {
        if [&self.slug, &self.id, &self.name]
            .iter()
            .any(|f| f.starts_with(token))
        {
            Some(1)
        } else if [&self.slug, &self.name].iter().any(|f| f.contains(token)) {
            Some(2)
        } else if [&self.id, &self.name]
            .iter()
            .any(|f| is_subsequence(token, f))
        {
            Some(3)
        } else {
            None
        }
    }
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut hay = haystack.chars();
    needle.chars().all(|n| hay.any(|h| h == n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(slug: &str, name: &str) -> ModelChoice {
        ModelChoice {
            key: ("omp".into(), slug.into()),
            name: name.into(),
            provider: slug.split('/').next().unwrap_or_default().into(),
            is_default: false,
            label: Default::default(),
        }
    }

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

    #[test]
    fn display_name_fallbacks_and_quotes() {
        assert_eq!(display_name(&choice("a/b-1", "  Big \"One\" ")), "Big One");
        assert_eq!(display_name(&choice("anthropic/claude-x", " ")), "claude-x");
        assert_eq!(display_name(&choice("solo", "")), "solo");
        assert_eq!(display_name(&choice("a/b/c", "")), "b/c");
    }

    #[test]
    fn find_choice_is_exact() {
        let cs = [choice("a/b", "B"), choice("a/bc", "BC")];
        assert_eq!(find_choice(&cs, "a/bc"), Some(&cs[1]));
        assert_eq!(find_choice(&cs, "a/"), None);
    }

    fn catalog() -> Vec<ModelChoice> {
        vec![
            choice("openai/gpt-5", "GPT 5"),
            choice("anthropic/claude-opus", "Claude Opus"),
            choice("google/gemini-pro", "Gemini Pro"),
            choice("anthropic/claude-sonnet", "Claude Sonnet"),
        ]
    }

    #[test]
    fn rank_empty_query_puts_favorites_first() {
        let cs = catalog();
        let favs = vec![
            "google/gemini-pro".to_string(),
            "anthropic/claude-sonnet".to_string(),
        ];
        assert_eq!(rank(&cs, &favs, "  ", 10), [2, 3, 0, 1]);
        assert_eq!(rank(&cs, &[], "", 10), [0, 1, 2, 3]);
    }

    #[test]
    fn rank_tiers_prefix_contains_subsequence() {
        let cs = vec![
            choice("x/a-opus", "Zed"),
            choice("x/opus-1", "Alpha"),
            choice("x/o-p-u-s", "Beta"),
        ];
        assert_eq!(rank(&cs, &[], "opus", 10), [1, 0, 2]);
        assert_eq!(rank(&cs, &[], "OPUS", 10), [1, 0, 2]);
        assert!(rank(&cs, &[], "zzz", 10).is_empty());
    }

    #[test]
    fn rank_favorites_win_within_a_tier() {
        let cs = catalog();
        let favs = vec!["anthropic/claude-sonnet".to_string()];
        assert_eq!(rank(&cs, &favs, "claude", 10), [3, 1]);
    }

    #[test]
    fn rank_multi_token_requires_all() {
        let cs = catalog();
        // "opus" only matches the opus model: it is not a subsequence of the
        // sonnet id, and subsequences never reach across the provider.
        assert_eq!(rank(&cs, &[], "claude opus", 10), [1]);
        assert_eq!(rank(&cs, &[], "claude gpt", 10), Vec::<usize>::new());
        // Scores add up: "claude" is a prefix of the second id, only contained
        // in the first, and "son" is contained in both.
        let mixed = vec![choice("p/xclaude-son", "A"), choice("p/claude-son", "B")];
        assert_eq!(rank(&mixed, &[], "claude son", 10), [1, 0]);
    }

    #[test]
    fn rank_respects_limit() {
        let cs = catalog();
        assert_eq!(rank(&cs, &[], "", 2), [0, 1]);
        assert_eq!(rank(&cs, &[], "a", 1).len(), 1);
        assert!(rank(&cs, &[], "", 0).is_empty());
    }
}
