//! Model and thinking-level choices derived from a server's config, the way
//! t3's `buildModelOptions` does (ios-surface §2.2 M1, "Model picker").

use j3_protocol::t3::{
    model::{
        ModelSelection, ProviderOptionDescriptor, ProviderOptionSelection,
        ProviderOptionSelectionValue, SelectProviderOptionDescriptor,
    },
    server::{ServerConfig, ServerProvider, ServerProviderAuthStatus, ServerProviderModel},
};

/// The option id omp's thinking level travels under.
const THINKING: &str = "thinking";

/// One pickable model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// `(provider instance id, model slug)`: the model's identity.
    pub key: (String, String),
    pub name: String,
    pub is_default: bool,
}

impl ModelChoice {
    pub fn instance_id(&self) -> &str {
        &self.key.0
    }

    pub fn slug(&self) -> &str {
        &self.key.1
    }
}

fn usable(provider: &ServerProvider) -> bool {
    provider.enabled
        && provider.installed
        && provider.auth.status != ServerProviderAuthStatus::Unauthenticated
}

/// Models of every usable provider, in config order.
pub fn model_choices(config: &ServerConfig) -> Vec<ModelChoice> {
    config
        .providers
        .iter()
        .filter(|provider| usable(provider))
        .flat_map(|provider| {
            provider.models.iter().map(|model| ModelChoice {
                key: (provider.instance_id.clone(), model.slug.clone()),
                name: model.name.clone(),
                is_default: model.is_default == Some(true),
            })
        })
        .collect()
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
                    {"slug":"b/two","name":"Two","isCustom":false,"isDefault":true,"capabilities":{"optionDescriptors":[
                        {"type":"boolean","id":"fast","label":"Fast"},
                        {"type":"select","id":"thinking","label":"Thinking","options":[
                            {"id":"off","label":"Off"},{"id":"medium","label":"Medium","isDefault":true},{"id":"high","label":"High"}]}]}}]}
            ]
        }))
        .unwrap()
    }

    #[test]
    fn lists_models_of_usable_providers_and_picks_defaults() {
        let config = config();
        let choices = model_choices(&config);
        let slugs: Vec<&str> = choices.iter().map(|c| c.slug()).collect();
        assert_eq!(slugs, ["a/one", "b/two"]);
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
}
