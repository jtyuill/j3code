//! Models for omp's `^model` mentions ([`j3_text::mention`]): the name a
//! mention shows and ranking models for the picker.

use crate::models::ModelChoice;

/// omp's `modelMentionDisplayName`: the trimmed name, else the model id; `"`
/// removed. The base of [`crate::models::ModelLabel`],
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
        let (provider, id) = slug.split_once('/').unwrap_or(("", slug));
        ModelChoice::new(provider, id, name)
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
