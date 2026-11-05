//! Model and thinking-level choices derived from a server's config, the way
//! t3's `buildModelOptions` does (ios-surface §2.2 M1, "Model picker").

use std::{collections::HashMap, hash::Hash};

use gpui_kit::SharedString;
use j3_protocol::t3::{
    model::{
        ModelSelection, ProviderOptionDescriptor, ProviderOptionSelection,
        ProviderOptionSelectionValue, SelectProviderOptionDescriptor,
    },
    server::{ServerConfig, ServerProvider, ServerProviderAuthStatus, ServerProviderModel},
};

use crate::{logos, mention};

/// The option id omp's thinking level travels under.
const THINKING: &str = "thinking";

/// One pickable model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// `(provider instance id, model slug)`: the model's identity.
    pub key: (String, String),
    pub name: String,
    /// The upstream provider serving it (omp's `anthropic`, `openai-codex`,
    /// …): the model's `subProvider`, else the slug's `<provider>/` prefix.
    pub provider: String,
    pub is_default: bool,
    /// How the UI names it; [`model_choices`] sets it for the whole catalog.
    pub label: ModelLabel,
}

impl ModelChoice {
    pub fn instance_id(&self) -> &str {
        &self.key.0
    }

    pub fn slug(&self) -> &str {
        &self.key.1
    }

    /// The slug after its first `/` (the provider's own model id), or the
    /// whole slug.
    pub fn id(&self) -> &str {
        let slug = self.slug();
        slug.split_once('/').map_or(slug, |(_, id)| id)
    }
}

/// How a model is shown: its pretty name and, when another listed model
/// has the same name, what tells them apart. Never the raw slug unless
/// nothing else does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelLabel {
    name: SharedString,
    qualifier: Option<SharedString>,
    /// `qualifier` without the provider's name.
    detail: Option<SharedString>,
    text: SharedString,
}

impl ModelLabel {
    fn new(name: String, provider: Option<&str>, detail: Option<String>) -> Self {
        let name = SharedString::from(name);
        let qualifier = qualifier(provider, detail.as_deref()).map(SharedString::from);
        let detail = detail.map(SharedString::from);
        let text = match &qualifier {
            Some(qualifier) => format!("{name} ({qualifier})").into(),
            None => name.clone(),
        };
        Self {
            name,
            qualifier,
            detail,
            text,
        }
    }

    /// The pretty name ([`mention::display_name`]).
    pub fn name(&self) -> &SharedString {
        &self.name
    }

    /// What sets the model apart from others with its name; shown muted
    /// after the name.
    pub fn qualifier(&self) -> Option<&SharedString> {
        self.qualifier.as_ref()
    }

    /// [`Self::qualifier`] without the provider's name, for a row under its
    /// provider's header (which already names it).
    pub fn section_qualifier(&self) -> Option<&SharedString> {
        self.detail.as_ref()
    }

    /// The label as plain text: `Name (qualifier)`, or just the name.
    pub fn text(&self) -> &SharedString {
        &self.text
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

fn usable(provider: &ServerProvider) -> bool {
    provider.enabled
        && provider.installed
        && provider.auth.status != ServerProviderAuthStatus::Unauthenticated
}

/// Models of every usable provider, in config order, labelled.
pub fn model_choices(config: &ServerConfig) -> Vec<ModelChoice> {
    let mut choices: Vec<ModelChoice> = config
        .providers
        .iter()
        .filter(|provider| usable(provider))
        .flat_map(|provider| {
            provider.models.iter().map(|model| ModelChoice {
                key: (provider.instance_id.clone(), model.slug.clone()),
                name: model.name.clone(),
                provider: model
                    .sub_provider
                    .clone()
                    .or_else(|| Some(model.slug.split_once('/')?.0.to_owned()))
                    .unwrap_or_else(|| provider.instance_id.clone()),
                is_default: model.is_default == Some(true),
                label: ModelLabel::default(),
            })
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
pub fn initial_choice(
    choices: &[ModelChoice],
    preferred: Option<&ModelSelection>,
) -> Option<usize> {
    preferred
        .and_then(|selection| {
            choices.iter().position(|choice| {
                choice.instance_id() == selection.instance_id && choice.slug() == selection.model
            })
        })
        .or_else(|| choices.iter().position(|choice| choice.is_default))
        .or(if choices.is_empty() { None } else { Some(0) })
}

fn find_model<'a>(
    config: &'a ServerConfig,
    choice: &ModelChoice,
) -> Option<&'a ServerProviderModel> {
    config
        .providers
        .iter()
        .find(|provider| provider.instance_id == choice.instance_id())?
        .models
        .iter()
        .find(|model| model.slug == choice.slug())
}

/// The model's thinking-level select descriptor: the one with id `thinking`,
/// else the first `select` with the `reasoning` role.
pub fn thinking_descriptor<'a>(
    config: &'a ServerConfig,
    choice: &ModelChoice,
) -> Option<&'a SelectProviderOptionDescriptor> {
    let descriptors = find_model(config, choice)?
        .capabilities
        .as_ref()?
        .option_descriptors
        .as_deref()?;
    let selects = descriptors
        .iter()
        .filter_map(|descriptor| match descriptor {
            ProviderOptionDescriptor::Select(select) => Some(select),
            ProviderOptionDescriptor::Boolean(_) => None,
        });
    selects
        .clone()
        .find(|select| select.id == THINKING)
        .or_else(|| selects.clone().find(|select| select.role.is_some()))
}

/// The level to preselect: `preferred` if offered, else the descriptor's
/// current value, else its default option, else the first option.
pub fn initial_level(
    descriptor: &SelectProviderOptionDescriptor,
    preferred: Option<&str>,
) -> Option<String> {
    let offered = |id: &str| descriptor.options.iter().any(|option| option.id == id);
    preferred
        .filter(|id| offered(id))
        .or(descriptor.current_value.as_deref().filter(|id| offered(id)))
        .or_else(|| {
            descriptor
                .options
                .iter()
                .find(|option| option.is_default == Some(true))
                .map(|option| option.id.as_str())
        })
        .or_else(|| descriptor.options.first().map(|option| option.id.as_str()))
        .map(str::to_owned)
}

/// The thinking level carried by a selection, if any.
pub fn selection_level<'a>(selection: &'a ModelSelection, descriptor_id: &str) -> Option<&'a str> {
    match selection.option(descriptor_id)? {
        ProviderOptionSelectionValue::String(value) => Some(value),
        ProviderOptionSelectionValue::Bool(_) => None,
    }
}

/// The wire selection for `choice` at thinking `level` (under `descriptor_id`).
pub fn model_selection(choice: &ModelChoice, level: Option<(&str, &str)>) -> ModelSelection {
    ModelSelection {
        instance_id: choice.instance_id().to_owned(),
        model: choice.slug().to_owned(),
        options: level.map(|(id, value)| {
            vec![ProviderOptionSelection {
                id: id.to_owned(),
                value: ProviderOptionSelectionValue::String(value.to_owned()),
            }]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ServerConfig {
        serde_json::from_value(serde_json::json!({
            "environment": {"environmentId":"env","label":"mac","platform":{"os":"darwin","arch":"arm64"},
                "serverVersion":"0.1.0","orchestrationProtocolVersion":2,"capabilities":{}},
            "auth": {"policy":"unsafe-no-auth","bootstrapMethods":[],"sessionMethods":[],"sessionCookieName":"j3"},
            "cwd":"/", "keybindingsConfigPath":"/k.json", "keybindings":[], "issues":[], "availableEditors":[],
            "observability":{"logsDirectoryPath":"/l","localTracingEnabled":false,"otlpTracesEnabled":false,"otlpMetricsEnabled":false},
            "settings":{},
            "providers":[
                {"instanceId":"off","driver":"pi","enabled":false,"installed":true,"version":null,"status":"ready",
                 "auth":{"status":"authenticated"},"checkedAt":"t","models":[
                    {"slug":"x/hidden","name":"Hidden","isCustom":false,"capabilities":null}]},
                {"instanceId":"omp","driver":"pi","enabled":true,"installed":true,"version":"1","status":"ready",
                 "auth":{"status":"authenticated"},"checkedAt":"t","models":[
                    {"slug":"a/one","name":"One","isCustom":false,"capabilities":null},
                    {"slug":"b/two","name":"Two","subProvider":"bee","isCustom":false,"isDefault":true,"capabilities":{"optionDescriptors":[
                        {"type":"boolean","id":"fast","label":"Fast"},
                        {"type":"select","id":"thinking","label":"Thinking","options":[
                            {"id":"off","label":"Off"},{"id":"medium","label":"Medium","isDefault":true},{"id":"high","label":"High"}]}]}},
                    {"slug":"a/three","name":"Three","isCustom":false,"capabilities":null}]}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn lists_models_of_usable_providers_and_picks_defaults() {
        let config = config();
        let choices = model_choices(&config);
        let slugs: Vec<&str> = choices.iter().map(|c| c.slug()).collect();
        assert_eq!(slugs, ["a/one", "b/two", "a/three"]);
        // The subProvider wins over the slug prefix; groups keep first-seen order.
        let groups: Vec<(&str, Vec<usize>)> = by_provider(&choices);
        assert_eq!(groups, [("a", vec![0, 2]), ("bee", vec![1])]);
        assert_eq!(initial_choice(&choices, None), Some(1));
        let preferred = ModelSelection {
            instance_id: "omp".into(),
            model: "a/one".into(),
            options: None,
        };
        assert_eq!(initial_choice(&choices, Some(&preferred)), Some(0));
        let gone = ModelSelection {
            instance_id: "omp".into(),
            model: "z/gone".into(),
            options: None,
        };
        assert_eq!(initial_choice(&choices, Some(&gone)), Some(1));
        assert_eq!(initial_choice(&[], None), None);
    }

    #[test]
    fn thinking_levels_come_from_the_model_descriptor() {
        let config = config();
        let choices = model_choices(&config);
        assert!(thinking_descriptor(&config, &choices[0]).is_none());
        let descriptor = thinking_descriptor(&config, &choices[1]).unwrap();
        assert_eq!(descriptor.id, "thinking");
        assert_eq!(initial_level(descriptor, None).as_deref(), Some("medium"));
        assert_eq!(
            initial_level(descriptor, Some("high")).as_deref(),
            Some("high")
        );
        assert_eq!(
            initial_level(descriptor, Some("bogus")).as_deref(),
            Some("medium")
        );

        let selection = model_selection(&choices[1], Some(("thinking", "high")));
        assert_eq!(
            serde_json::to_value(&selection).unwrap(),
            serde_json::json!({"instanceId":"omp","model":"b/two","options":[{"id":"thinking","value":"high"}]})
        );
        assert_eq!(selection_level(&selection, "thinking"), Some("high"));
        assert_eq!(
            serde_json::to_value(model_selection(&choices[0], None)).unwrap(),
            serde_json::json!({"instanceId":"omp","model":"a/one"})
        );
    }

    fn named(provider: &str, slug: &str, name: &str) -> ModelChoice {
        ModelChoice {
            key: ("omp".into(), slug.into()),
            name: name.into(),
            provider: provider.into(),
            is_default: false,
            label: ModelLabel::default(),
        }
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
        let sections: Vec<Option<&str>> = labels
            .iter()
            .map(|label| label.section_qualifier().map(SharedString::as_str))
            .collect();
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
        assert_eq!(labels[1].name().as_str(), "Claude Opus 4.5");
        assert_eq!(
            labels[1].qualifier().map(SharedString::as_str),
            Some("20251101")
        );
        assert_eq!(labels[1].text().as_str(), "Claude Opus 4.5 (20251101)");
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
