//! Model capabilities and selections (`C/model.ts`, `C/modelSelection.ts`).

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use super::ids::ProviderInstanceId;

/// `ProviderOptionChoice` (`C/model.ts:10-15`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOptionChoice {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
}

/// `role` of a select descriptor: only `"reasoning"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderOptionRole {
    #[serde(rename = "reasoning")]
    Reasoning,
}

/// `SelectProviderOptionDescriptor` (`C/model.ts:24-33`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectProviderOptionDescriptor {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub options: Vec<ProviderOptionChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_injected_values: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ProviderOptionRole>,
}

/// `BooleanProviderOptionDescriptor` (`C/model.ts:36-40`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BooleanProviderOptionDescriptor {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_value: Option<bool>,
}

/// `ProviderOptionDescriptor`, discriminated by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProviderOptionDescriptor {
    #[serde(rename = "select")]
    Select(SelectProviderOptionDescriptor),
    #[serde(rename = "boolean")]
    Boolean(BooleanProviderOptionDescriptor),
}

/// `ProviderOptionSelectionValue`: a non-blank string or a boolean.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderOptionSelectionValue {
    String(String),
    Bool(bool),
}

/// `ProviderOptionSelection` (`C/model.ts:52-56`), e.g. `{"id":"thinking","value":"high"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOptionSelection {
    pub id: String,
    pub value: ProviderOptionSelectionValue,
}

/// `ModelCapabilities` (`C/model.ts:102-105`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_descriptors: Option<Vec<ProviderOptionDescriptor>>,
}

/// `ModelSelection` (`C/modelSelection.ts`).
///
/// Decodes the legacy `provider` key in place of `instanceId`, and `options`
/// as either `[{id, value}]` or the legacy `{id: value}` object. Always
/// encodes `instanceId` and the array form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub instance_id: ProviderInstanceId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ProviderOptionSelection>>,
}

impl ModelSelection {
    /// The value of option `id`, if selected.
    pub fn option(&self, id: &str) -> Option<&ProviderOptionSelectionValue> {
        self.options
            .as_deref()?
            .iter()
            .find(|selection| selection.id == id)
            .map(|selection| &selection.value)
    }
}

impl<'de> Deserialize<'de> for ModelSelection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut raw = Map::<String, Value>::deserialize(deserializer)?;
        // `raw.instanceId !== undefined ? raw.instanceId : typeof raw.provider === "string" ? raw.provider : undefined`
        let instance_id = match raw.remove("instanceId") {
            Some(Value::String(id)) => id,
            Some(other) => {
                return Err(D::Error::custom(format_args!(
                    "ModelSelection.instanceId must be a string, got {other}"
                )));
            }
            None => match raw.remove("provider") {
                Some(Value::String(id)) => id,
                _ => return Err(D::Error::missing_field("instanceId")),
            },
        };
        let model = match raw.remove("model") {
            Some(Value::String(model)) => model,
            Some(other) => {
                return Err(D::Error::custom(format_args!(
                    "ModelSelection.model must be a string, got {other}"
                )));
            }
            None => return Err(D::Error::missing_field("model")),
        };
        let options = match raw.remove("options") {
            None => None,
            Some(Value::Array(items)) => Some(
                items
                    .into_iter()
                    .map(serde_json::from_value)
                    .collect::<Result<Vec<ProviderOptionSelection>, _>>()
                    .map_err(D::Error::custom)?,
            ),
            Some(Value::Object(legacy)) => Some(legacy_options_to_array(legacy)),
            Some(other) => {
                return Err(D::Error::custom(format_args!(
                    "ModelSelection.options must be an array or object, got {other}"
                )));
            }
        };
        Ok(Self {
            instance_id,
            model,
            options,
        })
    }
}

/// `coerceLegacyOptionsObjectToArray` (`C/model.ts:73-88`).
fn legacy_options_to_array(record: Map<String, Value>) -> Vec<ProviderOptionSelection> {
    record
        .into_iter()
        .filter_map(|(key, value)| {
            let id = key.trim();
            if id.is_empty() {
                return None;
            }
            let value = match value {
                Value::String(text) => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        return None;
                    }
                    ProviderOptionSelectionValue::String(trimmed.to_owned())
                }
                Value::Bool(flag) => ProviderOptionSelectionValue::Bool(flag),
                _ => return None,
            };
            Some(ProviderOptionSelection {
                id: id.to_owned(),
                value,
            })
        })
        .collect()
}
