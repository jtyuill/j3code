//! The omp provider catalog: the single `ServerProvider` (`instanceId:"omp"`,
//! `driver:"pi"`) with its models, thinking descriptor, slash commands and
//! skills (ios-surface M1, M3, §5).
//!
//! Models come from a utility `omp --mode rpc --no-session` process; slash
//! commands and skills are per cwd, cached, and also fed by thread actors
//! whenever their omp reports its catalog.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use j3_protocol::t3::{
    self,
    model::{
        ModelCapabilities, ProviderOptionChoice, ProviderOptionDescriptor,
        SelectProviderOptionDescriptor,
    },
    server::{
        ServerProvider, ServerProviderModel, ServerProviderSkill, ServerProviderSlashCommand,
        ServerProviderSlashCommandInput, ServerProviderWorkspaceSnapshot,
    },
};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::{
    engine::{
        model::THINKING_OPTION,
        projection::{DRIVER, PROVIDER_INSTANCE},
    },
    omp::{
        OmpLauncher,
        omp_rpc::{
            AvailableSlashCommand, GetAvailableCommandsCommand, GetAvailableModelsCommand,
            GetStateCommand, ModelInfo, SlashCommandSource, ThinkingLevel,
        },
    },
};

/// Deadline for one utility omp round trip (spawn + queries).
const UTILITY_TIMEOUT: Duration = Duration::from_secs(45);

/// Model list state from the last utility run.
#[derive(Debug, Clone)]
struct Models {
    models: Vec<ServerProviderModel>,
    checked_at: String,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct Commands {
    checked_at: String,
    commands: Vec<AvailableSlashCommand>,
}

pub struct Catalog {
    launcher: OmpLauncher,
    default_cwd: PathBuf,
    models: Mutex<Option<Models>>,
    commands: Mutex<HashMap<String, Commands>>,
    /// Serializes model loads (single flight).
    loading: tokio::sync::Mutex<()>,
    updates: broadcast::Sender<ServerProvider>,
}

fn label_of(id: &str) -> String {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn level_name(level: impl serde::Serialize) -> Option<String> {
    match serde_json::to_value(level).ok()? {
        Value::String(name) => Some(name),
        _ => None,
    }
}

/// `ServerProviderModel`s from omp's model list (ios-surface §5 "Model list",
/// "Thinking picker").
pub fn provider_models(
    models: &[ModelInfo],
    default: Option<&str>,
    thinking: Option<ThinkingLevel>,
) -> Vec<ServerProviderModel> {
    let current = thinking.and_then(level_name);
    models
        .iter()
        .map(|model| {
            let slug = format!("{}/{}", model.provider, model.id);
            let descriptors = if model.reasoning {
                let efforts: Vec<String> = match &model.thinking {
                    Some(config) => config
                        .efforts
                        .iter()
                        .filter_map(|e| level_name(*e))
                        .collect(),
                    None => vec!["low".into(), "medium".into(), "high".into()],
                };
                let options: Vec<ProviderOptionChoice> = std::iter::once("off".to_owned())
                    .chain(efforts)
                    .map(|id| ProviderOptionChoice {
                        label: label_of(&id),
                        is_default: (current.as_deref() == Some(id.as_str())).then_some(true),
                        description: None,
                        id,
                    })
                    .collect();
                vec![ProviderOptionDescriptor::Select(
                    SelectProviderOptionDescriptor {
                        id: THINKING_OPTION.to_owned(),
                        label: "Thinking".into(),
                        description: None,
                        options,
                        current_value: None,
                        prompt_injected_values: None,
                        role: None,
                    },
                )]
            } else {
                Vec::new()
            };
            ServerProviderModel {
                is_default: (default == Some(slug.as_str())).then_some(true),
                slug,
                name: if model.name.trim().is_empty() {
                    model.id.clone()
                } else {
                    model.name.clone()
                },
                short_name: None,
                // Clients show it beside the name; the same model is often
                // offered by several omp providers.
                sub_provider: Some(model.provider.clone()),
                aliases: None,
                badge: None,
                is_custom: false,
                is_legacy: None,
                capabilities: Some(ModelCapabilities {
                    option_descriptors: Some(descriptors),
                }),
            }
        })
        .collect()
}

/// Splits omp's slash-command catalog into t3 slash commands and skills.
pub fn split_commands(
    commands: &[AvailableSlashCommand],
) -> (Vec<ServerProviderSlashCommand>, Vec<ServerProviderSkill>) {
    let mut slash = Vec::new();
    let mut skills = Vec::new();
    for command in commands {
        if command.name.trim().is_empty() {
            continue;
        }
        if command.source == SlashCommandSource::Skill {
            let name = command
                .name
                .strip_prefix("skill:")
                .unwrap_or(&command.name)
                .to_owned();
            skills.push(ServerProviderSkill {
                // omp's catalog carries no file path; its skill URL names it.
                path: format!("skill://{name}"),
                name,
                description: command.description.clone(),
                scope: None,
                enabled: true,
                display_name: None,
                short_description: None,
                user_invocation_only: None,
                user_invocable: Some(true),
            });
        } else {
            slash.push(ServerProviderSlashCommand {
                name: command.name.clone(),
                description: command.description.clone(),
                input: command
                    .input
                    .as_ref()
                    .and_then(|input| input.hint.clone())
                    .filter(|hint| !hint.trim().is_empty())
                    .map(|hint| ServerProviderSlashCommandInput { hint }),
            });
        }
    }
    (slash, skills)
}

impl Catalog {
    pub fn new(launcher: OmpLauncher, default_cwd: PathBuf) -> Self {
        Self {
            launcher,
            default_cwd,
            models: Mutex::new(None),
            commands: Mutex::new(HashMap::new()),
            loading: tokio::sync::Mutex::new(()),
            updates: broadcast::channel(16).0,
        }
    }

    /// Provider updates (pushed as `providerStatuses`).
    pub fn subscribe(&self) -> broadcast::Receiver<ServerProvider> {
        self.updates.subscribe()
    }

    /// Records the slash-command catalog reported for `cwd`.
    pub fn put_commands(&self, cwd: &str, commands: &[AvailableSlashCommand]) {
        self.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                cwd.to_owned(),
                Commands {
                    checked_at: t3::now_iso(),
                    commands: commands.to_vec(),
                },
            );
    }

    fn cached_models(&self) -> Option<Models> {
        self.models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn cached_commands(&self, cwd: &str) -> Option<Commands> {
        self.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(cwd)
            .cloned()
    }

    /// The provider; loads the model list on first use.
    pub async fn provider(&self) -> ServerProvider {
        if self.cached_models().is_none() {
            self.load_models(false).await;
        }
        self.build(None)
    }

    /// The provider if the model list is already known (no waiting).
    pub fn provider_now(&self) -> Option<ServerProvider> {
        self.cached_models().map(|_| self.build(None))
    }

    /// The provider as currently known (`warning` while models load).
    pub fn current(&self) -> ServerProvider {
        self.build(None)
    }

    /// `server.refreshProviders`: optionally reloads models, and includes the
    /// workspace snapshot for `cwd`.
    pub async fn refresh(
        &self,
        cwd: Option<&str>,
        refresh_models: bool,
        fresh: bool,
    ) -> ServerProvider {
        if refresh_models || self.cached_models().is_none() {
            self.load_models(refresh_models).await;
        }
        if let Some(cwd) = cwd
            && (fresh || self.cached_commands(cwd).is_none())
        {
            self.load_commands(cwd).await;
        }
        self.build(cwd)
    }

    async fn load_models(&self, force: bool) {
        let _guard = self.loading.lock().await;
        if !force && self.cached_models().is_some() {
            return;
        }
        let checked_at = t3::now_iso();
        let models = match self.query_models().await {
            Ok((models, commands)) => {
                tracing::info!(models = models.len(), "loaded the omp model catalog");
                let cwd = self.default_cwd.to_string_lossy().into_owned();
                self.put_commands(&cwd, &commands);
                Models {
                    models,
                    checked_at,
                    error: None,
                }
            }
            Err(error) => {
                tracing::warn!(%error, "loading omp models");
                Models {
                    models: Vec::new(),
                    checked_at,
                    error: Some(error),
                }
            }
        };
        *self.models.lock().unwrap_or_else(PoisonError::into_inner) = Some(models);
        let _ = self.updates.send(self.build(None));
    }

    async fn query_models(
        &self,
    ) -> Result<(Vec<ServerProviderModel>, Vec<AvailableSlashCommand>), String> {
        let launcher = self.launcher.clone();
        let cwd = self.default_cwd.clone();
        let work = async move {
            let (session, _events) = launcher
                .start_utility(&cwd)
                .await
                .map_err(|error| error.to_string())?;
            let result = async {
                let models = session
                    .call(GetAvailableModelsCommand {})
                    .await
                    .map_err(|error| error.to_string())?;
                let state = session
                    .call(GetStateCommand {})
                    .await
                    .map_err(|error| error.to_string())?;
                let commands = session
                    .call(GetAvailableCommandsCommand {})
                    .await
                    .unwrap_or_default();
                let default = state
                    .model
                    .as_ref()
                    .map(|model| format!("{}/{}", model.provider, model.id));
                Ok((
                    provider_models(&models, default.as_deref(), state.thinking_level),
                    commands,
                ))
            }
            .await;
            let _ = session.close().await;
            result
        };
        tokio::time::timeout(UTILITY_TIMEOUT, work)
            .await
            .map_err(|_| "omp did not answer in time".to_owned())?
    }

    async fn load_commands(&self, cwd: &str) {
        let launcher = self.launcher.clone();
        let path = PathBuf::from(cwd);
        let work = async move {
            if !Path::new(&path).is_dir() {
                return Err(format!("{} is not a directory", path.display()));
            }
            let (session, _events) = launcher
                .start_utility(&path)
                .await
                .map_err(|error| error.to_string())?;
            let commands = session
                .call(GetAvailableCommandsCommand {})
                .await
                .map_err(|error| error.to_string());
            let _ = session.close().await;
            commands
        };
        match tokio::time::timeout(UTILITY_TIMEOUT, work).await {
            Ok(Ok(commands)) => self.put_commands(cwd, &commands),
            Ok(Err(error)) => tracing::warn!(%error, cwd, "loading omp commands"),
            Err(_) => tracing::warn!(cwd, "loading omp commands timed out"),
        }
    }

    /// The `ServerProvider`, with a workspace snapshot for `cwd` if known.
    fn build(&self, cwd: Option<&str>) -> ServerProvider {
        let models = self.cached_models();
        let default_cwd = self.default_cwd.to_string_lossy().into_owned();
        let (slash, skills) = self
            .cached_commands(&default_cwd)
            .map(|commands| split_commands(&commands.commands))
            .unwrap_or_default();
        let workspace = cwd.map(|cwd| {
            let cached = self.cached_commands(cwd);
            let (slash, skills) = cached
                .as_ref()
                .map(|commands| split_commands(&commands.commands))
                .unwrap_or_default();
            ServerProviderWorkspaceSnapshot {
                cwd: cwd.to_owned(),
                checked_at: cached.map_or_else(t3::now_iso, |commands| commands.checked_at),
                slash_commands: slash,
                slash_commands_pending: None,
                skills,
                option_descriptors: None,
            }
        });
        let (status, message, checked_at, models) = match models {
            None => (
                "warning",
                Some("Loading models…".to_owned()),
                t3::now_iso(),
                Vec::new(),
            ),
            Some(Models {
                error: Some(error),
                checked_at,
                ..
            }) => (
                "error",
                Some(format!("omp could not list models: {error}")),
                checked_at,
                Vec::new(),
            ),
            Some(Models {
                models, checked_at, ..
            }) => ("ready", None, checked_at, models),
        };
        let mut provider = json!({
            "instanceId": PROVIDER_INSTANCE,
            "driver": DRIVER,
            "displayName": "omp",
            "enabled": true,
            "installed": true,
            "version": null,
            "status": status,
            "auth": {"status": "authenticated"},
            "checkedAt": checked_at,
            "models": models,
            "slashCommands": slash,
            "skills": skills,
        });
        if let Some(message) = message {
            provider["message"] = Value::String(message);
        }
        if let Some(workspace) = workspace {
            provider["workspaceSnapshots"] = json!([workspace]);
        }
        serde_json::from_value(provider).expect("catalog provider matches ServerProvider")
    }
}

/// Shared handle used by the app and thread actors.
pub type SharedCatalog = Arc<Catalog>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::omp::omp_rpc::RpcNotification;

    fn fixture_commands() -> Vec<AvailableSlashCommand> {
        let text = include_str!("../tests/fixtures/omp/text.jsonl");
        text.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find_map(|value| match RpcNotification::from_value(value).ok()? {
                RpcNotification::AvailableCommandsUpdate(update) => Some(update.commands),
                _ => None,
            })
            .expect("fixture has available_commands_update")
    }

    #[test]
    fn engine_catalog_commands_split_into_slash_commands_and_skills() {
        let (slash, skills) = split_commands(&fixture_commands());
        assert!(!slash.is_empty());
        assert!(!skills.is_empty());
        assert!(skills.iter().all(|skill| !skill.name.starts_with("skill:")));
        assert!(
            slash
                .iter()
                .all(|command| command.input.as_ref().is_none_or(|i| !i.hint.is_empty()))
        );
    }

    #[test]
    fn engine_catalog_models_get_thinking_descriptor_and_default() {
        let text = include_str!("../tests/fixtures/omp/text.jsonl");
        let state: crate::omp::omp_rpc::SessionState = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| value["command"] == "get_state")
            .map(|value| serde_json::from_value(value["data"].clone()).unwrap())
            .unwrap();
        let model = state.model.clone().unwrap();
        let models = provider_models(
            &[model],
            Some("opencode-go/glm-5.3-flash"),
            state.thinking_level,
        );
        assert_eq!(models[0].slug, "opencode-go/glm-5.3-flash");
        assert_eq!(models[0].is_default, Some(true));
        let descriptors = models[0]
            .capabilities
            .as_ref()
            .and_then(|c| c.option_descriptors.clone())
            .unwrap();
        let ProviderOptionDescriptor::Select(select) = &descriptors[0] else {
            panic!("thinking descriptor is a select");
        };
        assert_eq!(select.id, "thinking");
        assert_eq!(select.options[0].id, "off");
        // Encodes and decodes as a ServerProviderModel.
        let json = serde_json::to_value(&models[0]).unwrap();
        let _: ServerProviderModel = serde_json::from_value(json).unwrap();
    }
}
