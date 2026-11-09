//! Model and thinking-level choices derived from a server's catalog (ios-surface §2.2 M1,
//! "Model picker"). A model is named by its selector `provider/id`, which is also what a
//! `^model` mention types.

use std::{collections::HashMap, hash::Hash};

use j3_client::{
    entities::{ModelRef, ThinkingLevel},
    methods::Catalog,
};

use crate::{logos, mention};

/// One pickable model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// The provider serving it (omp's `anthropic`, `openai-codex`, …).
    pub provider: String,
    id: String,
    /// `provider/id`: the model's identity.
    slug: String,
    pub name: String,
    /// The thinking levels it offers; empty without reasoning.
    pub thinking: Vec<ThinkingLevel>,
    pub is_default: bool,
    /// How the UI names it; [`model_choices`] sets it for the whole catalog.
    pub label: ModelLabel,
}

impl ModelChoice {
    /// Model `id` of `provider`, unlabelled, without thinking levels.
    pub fn new(provider: &str, id: &str, name: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            id: id.to_owned(),
            slug: format!("{provider}/{id}"),
            name: name.to_owned(),
            thinking: Vec::new(),
            is_default: false,
            label: ModelLabel::default(),
        }
    }

    /// `provider/id`.
    pub fn slug(&self) -> &str {
        &self.slug
    }

    /// The provider's own model id.
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// How a model is shown: its pretty name and, when another listed model
/// has the same name, what tells them apart. Never the raw slug unless
/// nothing else does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelLabel {
    name: String,
    qualifier: Option<String>,
    /// `qualifier` without the provider's name.
    detail: Option<String>,
    text: String,
    section_text: String,
}

impl ModelLabel {
    fn new(name: String, provider: Option<&str>, detail: Option<String>) -> Self {
        let qualifier = qualifier(provider, detail.as_deref());
        let text = match &qualifier {
            Some(qualifier) => format!("{name} ({qualifier})"),
            None => name.clone(),
        };
        let section_text = match &detail {
            Some(detail) => format!("{name} ({detail})"),
            None => name.clone(),
        };
        Self {
            name,
            qualifier,
            detail,
            text,
            section_text,
        }
    }

    /// The pretty name ([`mention::display_name`]).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What sets the model apart from others with its name; shown muted
    /// after the name.
    pub fn qualifier(&self) -> Option<&str> {
        self.qualifier.as_deref()
    }

    /// [`Self::qualifier`] without the provider's name, for a row under its
    /// provider's header (which already names it).
    pub fn section_qualifier(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// The label as plain text: `Name (qualifier)`, or just the name.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// [`Self::text`] without the provider's name, for where the provider's
    /// logo already shows it: `Name (detail)`, or just the name.
    pub fn section_text(&self) -> &str {
        &self.section_text
    }
}

/// Labels for a whole catalog, in `choices` order. Models whose names
/// collide (trimmed, case-insensitively) are told apart by their provider's
/// name when the providers differ, and within one provider by the id tokens
/// (split on `-`, `_`, `.`, `:`, `/`) not every colliding id has. Labels that
/// still collide fall back to the model id. Unique names stay bare.
pub fn model_labels(choices: &[ModelChoice]) -> Vec<ModelLabel> {
    let names: Vec<String> = choices.iter().map(mention::display_name).collect();
    let keys: Vec<String> = names
        .iter()
        .map(|name| name.trim().to_lowercase())
        .collect();
    let mut providers: Vec<Option<&str>> = vec![None; choices.len()];
    let mut details: Vec<Option<String>> = vec![None; choices.len()];
    for group in collisions(0..choices.len(), |ix| keys[ix].as_str()) {
        let first = &choices[group[0]].provider;
        if group.iter().any(|&ix| choices[ix].provider != *first) {
            for &ix in &group {
                providers[ix] = Some(logos::provider_name(&choices[ix].provider));
            }
        }
        for same in collisions(group.iter().copied(), |ix| choices[ix].provider.as_str()) {
            let ids: Vec<&str> = same.iter().map(|&ix| choices[ix].id()).collect();
            for (&ix, detail) in same.iter().zip(distinct_tokens(&ids)) {
                details[ix] = detail;
            }
        }
    }
    let still = collisions(0..choices.len(), |ix| {
        (
            keys[ix].as_str(),
            qualifier(providers[ix], details[ix].as_deref()).map(|q| q.to_lowercase()),
        )
    });
    for ix in still.into_iter().flatten() {
        details[ix] = Some(choices[ix].id().to_owned());
    }
    names
        .into_iter()
        .zip(providers)
        .zip(details)
        .map(|((name, provider), detail)| ModelLabel::new(name, provider, detail))
        .collect()
}

/// Groups of two or more `indices` with equal `key`s.
fn collisions<K: Eq + Hash>(
    indices: impl IntoIterator<Item = usize>,
    key: impl Fn(usize) -> K,
) -> Vec<Vec<usize>> {
    let mut groups: HashMap<K, Vec<usize>> = HashMap::new();
    for ix in indices {
        groups.entry(key(ix)).or_default().push(ix);
    }
    groups
        .into_values()
        .filter(|group| group.len() > 1)
        .collect()
}

/// For each of `ids`, its tokens that not all `ids` have, space-separated;
/// `None` when it has none.
fn distinct_tokens(ids: &[&str]) -> Vec<Option<String>> {
    fn tokens(id: &str) -> impl Iterator<Item = &str> {
        id.split(['-', '_', '.', ':', '/'])
            .filter(|token| !token.is_empty())
    }
    let shared = |token: &str| ids.iter().all(|id| tokens(id).any(|t| t == token));
    ids.iter()
        .map(|id| {
            let own: Vec<&str> = tokens(id).filter(|&token| !shared(token)).collect();
            (!own.is_empty()).then(|| own.join(" "))
        })
        .collect()
}

/// `Provider · detail`, either part alone, or `None`.
fn qualifier(provider: Option<&str>, detail: Option<&str>) -> Option<String> {
    match (provider, detail) {
        (Some(provider), Some(detail)) => Some(format!("{provider} · {detail}")),
        (Some(provider), None) => Some(provider.to_owned()),
        (None, detail) => detail.map(str::to_owned),
    }
}

/// Models of the catalog, in catalog order, labelled.
pub fn model_choices(catalog: &Catalog) -> Vec<ModelChoice> {
    let mut choices: Vec<ModelChoice> = catalog
        .models
        .iter()
        .map(|model| {
            let mut choice = ModelChoice::new(&model.provider, &model.id, &model.name);
            choice.thinking = model.thinking.clone().unwrap_or_default();
            choice.is_default = model.default;
            choice
        })
        .collect();
    let labels = model_labels(&choices);
    for (choice, label) in choices.iter_mut().zip(labels) {
        choice.label = label;
    }
    choices
}

/// `choices` grouped by provider, groups in order of first appearance:
/// `(provider, indices into choices)`.
pub fn by_provider(choices: &[ModelChoice]) -> Vec<(&str, Vec<usize>)> {
    let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
    for (ix, choice) in choices.iter().enumerate() {
        match groups.iter_mut().find(|(p, _)| *p == choice.provider) {
            Some((_, rows)) => rows.push(ix),
            None => groups.push((&choice.provider, vec![ix])),
        }
    }
    groups
}

/// The choice to preselect: `preferred` if the server still offers it,
/// else the server's default model, else the first one.
pub fn initial_choice(choices: &[ModelChoice], preferred: Option<&ModelRef>) -> Option<usize> {
    preferred
        .and_then(|model| {
            choices
                .iter()
                .position(|choice| choice.provider == model.provider && choice.id == model.id)
        })
        .or_else(|| choices.iter().position(|choice| choice.is_default))
        .or(if choices.is_empty() { None } else { Some(0) })
}

/// The level to preselect: `preferred` if the model offers it; otherwise none, which
/// leaves the level to omp.
pub fn initial_level(
    choice: &ModelChoice,
    preferred: Option<ThinkingLevel>,
) -> Option<ThinkingLevel> {
    preferred.filter(|level| choice.thinking.contains(level))
}

/// A thinking level's wire name (`off` … `xhigh`), also its element id.
pub fn level_id(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "off",
        ThinkingLevel::Minimal => "minimal",
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh => "xhigh",
    }
}

/// How a thinking level is shown.
pub fn level_label(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Off => "Off",
        ThinkingLevel::Minimal => "Minimal",
        ThinkingLevel::Low => "Low",
        ThinkingLevel::Medium => "Medium",
        ThinkingLevel::High => "High",
        ThinkingLevel::Xhigh => "Extra high",
    }
}

/// The wire model for `choice` at thinking `level`.
pub fn model_ref(choice: &ModelChoice, level: Option<ThinkingLevel>) -> ModelRef {
    ModelRef {
        provider: choice.provider.clone(),
        id: choice.id.clone(),
        thinking: level,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        serde_json::from_value(serde_json::json!({
            "models": [
                {"provider": "a", "id": "one", "name": "One"},
                {"provider": "bee", "id": "two", "name": "Two", "default": true,
                 "thinking": ["off", "medium", "high"]},
                {"provider": "a", "id": "three", "name": "Three"}
            ],
            "skills": [], "commands": [], "status": "ready"
        }))
        .unwrap()
    }

    fn model(provider: &str, id: &str, thinking: Option<ThinkingLevel>) -> ModelRef {
        ModelRef {
            provider: provider.into(),
            id: id.into(),
            thinking,
        }
    }

    #[test]
    fn lists_the_catalog_and_picks_defaults() {
        let choices = model_choices(&catalog());
        let slugs: Vec<&str> = choices.iter().map(|c| c.slug()).collect();
        assert_eq!(slugs, ["a/one", "bee/two", "a/three"]);
        // Groups keep first-seen order.
        let groups: Vec<(&str, Vec<usize>)> = by_provider(&choices);
        assert_eq!(groups, [("a", vec![0, 2]), ("bee", vec![1])]);
        assert_eq!(initial_choice(&choices, None), Some(1));
        assert_eq!(
            initial_choice(&choices, Some(&model("a", "one", None))),
            Some(0)
        );
        assert_eq!(
            initial_choice(&choices, Some(&model("z", "gone", None))),
            Some(1)
        );
        assert_eq!(initial_choice(&[], None), None);
    }

    #[test]
    fn thinking_levels_come_from_the_catalog() {
        let choices = model_choices(&catalog());
        assert!(choices[0].thinking.is_empty());
        assert_eq!(
            choices[1].thinking,
            [
                ThinkingLevel::Off,
                ThinkingLevel::Medium,
                ThinkingLevel::High
            ]
        );
        assert_eq!(
            initial_level(&choices[1], Some(ThinkingLevel::High)),
            Some(ThinkingLevel::High)
        );
        assert_eq!(initial_level(&choices[1], Some(ThinkingLevel::Low)), None);
        assert_eq!(initial_level(&choices[0], Some(ThinkingLevel::High)), None);

        assert_eq!(
            serde_json::to_value(model_ref(&choices[1], Some(ThinkingLevel::High))).unwrap(),
            serde_json::json!({"provider": "bee", "id": "two", "thinking": "high"})
        );
        assert_eq!(
            serde_json::to_value(model_ref(&choices[0], None)).unwrap(),
            serde_json::json!({"provider": "a", "id": "one"})
        );
        assert_eq!(level_id(ThinkingLevel::Xhigh), "xhigh");
    }

    fn named(provider: &str, slug: &str, name: &str) -> ModelChoice {
        let id = slug.strip_prefix(&format!("{provider}/")).unwrap_or(slug);
        ModelChoice::new(provider, id, name)
    }

    fn texts(choices: &[ModelChoice]) -> Vec<String> {
        model_labels(choices)
            .iter()
            .map(|label| label.text().to_string())
            .collect()
    }

    #[test]
    fn unique_names_get_no_qualifier() {
        let choices = [
            named("anthropic", "anthropic/claude-opus-4-5", "Claude Opus 4.5"),
            named("openai", "openai/gpt-5", "GPT-5"),
            named("x", "x/raw-id", " "),
        ];
        let labels = model_labels(&choices);
        assert!(labels.iter().all(|label| label.qualifier().is_none()));
        assert_eq!(texts(&choices), ["Claude Opus 4.5", "GPT-5", "raw-id"]);
    }

    #[test]
    fn cross_provider_collisions_name_the_provider() {
        let choices = [
            named("anthropic", "anthropic/claude-opus-4-5", "Claude Opus 4.5"),
            named(
                "openrouter",
                "openrouter/anthropic/claude-opus-4.5",
                " claude opus 4.5",
            ),
            named(
                "anthropic",
                "anthropic/claude-opus-4-5-20251101",
                "Claude Opus 4.5",
            ),
        ];
        assert_eq!(
            texts(&choices),
            [
                "Claude Opus 4.5 (Anthropic)",
                "claude opus 4.5 (OpenRouter)",
                "Claude Opus 4.5 (Anthropic · 20251101)",
            ]
        );
        // Under its provider's header a row drops the provider's name.
        let labels = model_labels(&choices);
        let sections: Vec<Option<&str>> =
            labels.iter().map(ModelLabel::section_qualifier).collect();
        assert_eq!(sections, [None, None, Some("20251101")]);
    }

    #[test]
    fn same_provider_collisions_add_the_differing_id_tokens() {
        let choices = [
            named("anthropic", "anthropic/claude-opus-4-5", "Claude Opus 4.5"),
            named(
                "anthropic",
                "anthropic/claude-opus-4-5-20251101",
                "Claude Opus 4.5",
            ),
        ];
        let labels = model_labels(&choices);
        assert_eq!(labels[0].qualifier(), None);
        assert_eq!(labels[1].name(), "Claude Opus 4.5");
        assert_eq!(labels[1].qualifier(), Some("20251101"));
        assert_eq!(labels[1].text(), "Claude Opus 4.5 (20251101)");
        assert_eq!(labels[1].section_qualifier(), labels[1].qualifier());
    }

    #[test]
    fn three_way_collisions_drop_the_tokens_all_ids_share() {
        let choices = [
            named("x", "x/gpt-5-codex", "GPT Codex"),
            named("x", "x/gpt-5-codex-mini", "GPT Codex"),
            named("x", "x/gpt-5.1-codex-max", "GPT Codex"),
        ];
        assert_eq!(
            texts(&choices),
            ["GPT Codex", "GPT Codex (mini)", "GPT Codex (1 max)"]
        );
    }

    #[test]
    fn labels_that_still_collide_fall_back_to_the_id() {
        let choices = [
            named("google", "google/gemini-2.5-pro-05-06", "Gemini 2.5 Pro"),
            named("google", "google/gemini-2.5-pro-06-05", "Gemini 2.5 Pro"),
            named("google", "google/gemini-flash", "Gemini Flash"),
        ];
        assert_eq!(
            texts(&choices),
            [
                "Gemini 2.5 Pro (gemini-2.5-pro-05-06)",
                "Gemini 2.5 Pro (gemini-2.5-pro-06-05)",
                "Gemini Flash",
            ]
        );
    }
}
