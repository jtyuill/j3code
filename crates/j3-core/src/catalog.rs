//! What omp offers (wire.md §5.1.1): models, skills and slash commands.
//!
//! Models come from a utility `omp --mode rpc --no-session` process; slash
//! commands and skills are per cwd, cached, and also fed by thread actors
//! whenever their omp reports its catalog.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
    time::Duration,
};

use j3_protocol::v2::{
    entities::ThinkingLevel,
    methods::{Catalog as CatalogData, CatalogCommand, CatalogModel, CatalogSkill, CatalogStatus},
};
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::omp::{
    OmpLauncher,
    omp_rpc::{
        AvailableSlashCommand, GetAvailableCommandsCommand, GetAvailableModelsCommand,
        GetStateCommand, ModelInfo, SlashCommandSource,
    },
};

/// Deadline for one utility omp round trip (spawn + queries).
const UTILITY_TIMEOUT: Duration = Duration::from_secs(45);

/// Model list from the last utility run, or why it failed.
type Models = Result<Vec<CatalogModel>, String>;

pub struct Catalog {
    launcher: OmpLauncher,
    default_cwd: PathBuf,
    models: Mutex<Option<Models>>,
    commands: Mutex<HashMap<String, Vec<AvailableSlashCommand>>>,
    /// Serializes model loads (single flight).
    loading: tokio::sync::Mutex<()>,
    updates: broadcast::Sender<CatalogData>,
}

/// The v2 thinking level of an omp level (same lowercase names).
fn thinking_level(level: impl serde::Serialize) -> Option<ThinkingLevel> {
    ThinkingLevel::deserialize(serde_json::to_value(level).ok()?).ok()
}

/// Catalog models from omp's model list: thinking is `off` plus the model's
/// efforts (`low`/`medium`/`high` when omp lists none), absent without
/// reasoning; `default` marks omp's current model `provider/id`.
fn models(models: &[ModelInfo], default: Option<&str>) -> Vec<CatalogModel> {
    models
        .iter()
        .map(|model| {
            let thinking = model.reasoning.then(|| {
                let efforts: Vec<ThinkingLevel> = match &model.thinking {
                    Some(config) => config
                        .efforts
                        .iter()
                        .filter_map(|e| thinking_level(*e))
                        .collect(),
                    None => vec![
                        ThinkingLevel::Low,
                        ThinkingLevel::Medium,
                        ThinkingLevel::High,
                    ],
                };
                std::iter::once(ThinkingLevel::Off).chain(efforts).collect()
            });
            CatalogModel {
                default: default == Some(format!("{}/{}", model.provider, model.id).as_str()),
                provider: model.provider.clone(),
                id: model.id.clone(),
                name: if model.name.trim().is_empty() {
                    model.id.clone()
                } else {
                    model.name.clone()
                },
                thinking,
            }
        })
        .collect()
}

/// `/btw`: omp offers it in its own interface only, and j3 clients answer it
/// through `btw.ask` (wire.md §5.13).
fn btw_command() -> CatalogCommand {
    CatalogCommand {
        name: "btw".to_owned(),
        description: "Ask a side question without interrupting the thread".to_owned(),
        hint: Some("<question>".to_owned()),
    }
}

/// Splits omp's slash-command catalog into slash commands and skills
/// (source `skill`, `skill:` prefix stripped).
fn split_commands(commands: &[AvailableSlashCommand]) -> (Vec<CatalogCommand>, Vec<CatalogSkill>) {
    let mut slash = Vec::new();
    let mut skills = Vec::new();
    for command in commands {
        if command.name.trim().is_empty() {
            continue;
        }
        let description = command.description.clone().unwrap_or_default();
        if command.source == SlashCommandSource::Skill {
            skills.push(CatalogSkill {
                name: command
                    .name
                    .strip_prefix("skill:")
                    .unwrap_or(&command.name)
                    .to_owned(),
                description,
            });
        } else {
            slash.push(CatalogCommand {
                name: command.name.clone(),
                description,
                hint: command
                    .input
                    .as_ref()
                    .and_then(|input| input.hint.clone())
                    .filter(|hint| !hint.trim().is_empty()),
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

    /// Every change of the catalog (model list loaded or refreshed).
    pub fn subscribe(&self) -> broadcast::Receiver<CatalogData> {
        self.updates.subscribe()
    }

    /// Records the slash-command catalog reported for `cwd`.
    pub fn put_commands(&self, cwd: &str, commands: &[AvailableSlashCommand]) {
        self.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(cwd.to_owned(), commands.to_vec());
    }

    fn cached_models(&self) -> Option<Models> {
        self.models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn cached_commands(&self, cwd: &str) -> Option<Vec<AvailableSlashCommand>> {
        self.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(cwd)
            .cloned()
    }

    /// Loads the model list on first use.
    pub async fn catalog(&self) -> CatalogData {
        if self.cached_models().is_none() {
            self.load_models(false).await;
        }
        self.current()
    }

    /// None until the model list has loaded once.
    pub fn catalog_now(&self) -> Option<CatalogData> {
        self.cached_models().map(|_| self.current())
    }

    /// As currently known (`warning` while models load).
    pub fn current(&self) -> CatalogData {
        let default_cwd = self.default_cwd.to_string_lossy();
        let (mut commands, skills) = self
            .cached_commands(&default_cwd)
            .map(|commands| split_commands(&commands))
            .unwrap_or_default();
        if !commands.iter().any(|command| command.name == "btw") {
            commands.push(btw_command());
        }
        let (status, message, models) = match self.cached_models() {
            None => (
                CatalogStatus::Warning,
                Some("Loading models…".to_owned()),
                Vec::new(),
            ),
            Some(Err(error)) => (
                CatalogStatus::Error,
                Some(format!("omp could not list models: {error}")),
                Vec::new(),
            ),
            Some(Ok(models)) => (CatalogStatus::Ready, None, models),
        };
        CatalogData {
            models,
            skills,
            commands,
            project_commands: BTreeMap::new(),
            status,
            message,
        }
    }

    /// `server.refresh`: `fresh` reloads omp's model list and the cwd's
    /// commands/skills; returns the catalog and `cwd`'s own slash commands
    /// (empty without a cwd). `project_commands` of the returned catalog is
    /// empty; the caller files the cwd's commands under the project id.
    pub async fn refresh(
        &self,
        cwd: Option<&str>,
        fresh: bool,
    ) -> (CatalogData, Vec<CatalogCommand>) {
        if fresh || self.cached_models().is_none() {
            self.load_models(fresh).await;
        }
        let Some(cwd) = cwd else {
            return (self.current(), Vec::new());
        };
        if fresh || self.cached_commands(cwd).is_none() {
            self.load_commands(cwd).await;
        }
        let commands = self
            .cached_commands(cwd)
            .map(|commands| split_commands(&commands).0)
            .unwrap_or_default();
        (self.current(), commands)
    }

    async fn load_models(&self, force: bool) {
        let _guard = self.loading.lock().await;
        if !force && self.cached_models().is_some() {
            return;
        }
        let models = match self.query_models().await {
            Ok((models, commands)) => {
                tracing::info!(models = models.len(), "loaded the omp model catalog");
                self.put_commands(&self.default_cwd.to_string_lossy(), &commands);
                Ok(models)
            }
            Err(error) => {
                tracing::warn!(%error, "loading omp models");
                Err(error)
            }
        };
        *self.models.lock().unwrap_or_else(PoisonError::into_inner) = Some(models);
        let _ = self.updates.send(self.current());
    }

    async fn query_models(
        &self,
    ) -> Result<(Vec<CatalogModel>, Vec<AvailableSlashCommand>), String> {
        let launcher = self.launcher.clone();
        let cwd = self.default_cwd.clone();
        let work = async move {
            let (session, _events) = launcher
                .start_utility(&cwd)
                .await
                .map_err(|error| error.to_string())?;
            let result = async {
                let list = session
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
                Ok((models(&list, default.as_deref()), commands))
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
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::omp::omp_rpc::RpcNotification;

    fn fixture_lines() -> impl Iterator<Item = Value> {
        include_str!("../tests/fixtures/omp/text.jsonl")
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    }

    fn fixture_commands() -> Vec<AvailableSlashCommand> {
        fixture_lines()
            .find_map(|value| match RpcNotification::from_value(value).ok()? {
                RpcNotification::AvailableCommandsUpdate(update) => Some(update.commands),
                _ => None,
            })
            .expect("fixture has available_commands_update")
    }

    fn model_info(value: Value) -> ModelInfo {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn commands_split_into_slash_commands_and_skills() {
        let (slash, skills) = split_commands(&fixture_commands());
        assert!(!slash.is_empty());
        assert!(!skills.is_empty());
        assert!(skills.iter().all(|skill| !skill.name.starts_with("skill:")));
        assert!(slash.iter().all(|command| {
            command
                .hint
                .as_ref()
                .is_none_or(|hint| !hint.trim().is_empty())
        }));
    }

    #[test]
    fn fixture_model_gets_thinking_and_default() {
        let state: crate::omp::omp_rpc::SessionState = fixture_lines()
            .find(|value| value["command"] == "get_state")
            .map(|value| serde_json::from_value(value["data"].clone()).unwrap())
            .unwrap();
        let model = state.model.unwrap();
        let list = models(&[model], Some("opencode-go/glm-5.3-flash"));
        assert_eq!(
            (list[0].provider.as_str(), list[0].id.as_str()),
            ("opencode-go", "glm-5.3-flash")
        );
        assert!(list[0].default);
        assert_eq!(list[0].thinking.as_ref().unwrap()[0], ThinkingLevel::Off);
    }

    #[test]
    fn thinking_lists_efforts_falls_back_and_is_absent_without_reasoning() {
        let base = serde_json::json!({"provider": "anthropic", "id": "opus", "name": "",
            "api": "anthropic-messages", "baseUrl": "", "reasoning": true, "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1, "maxTokens": 1});
        let mut efforts = base.clone();
        efforts["thinking"] = serde_json::json!({"mode": "effort", "minLevel": "low",
            "maxLevel": "high", "efforts": ["low", "high"]});
        let mut plain = base.clone();
        plain["reasoning"] = Value::Bool(false);
        plain["id"] = "gpt".into();
        let list = models(
            &[model_info(efforts), model_info(base), model_info(plain)],
            Some("openai/gpt"),
        );
        assert_eq!(
            list[0].thinking,
            Some(vec![
                ThinkingLevel::Off,
                ThinkingLevel::Low,
                ThinkingLevel::High
            ])
        );
        assert_eq!(list[0].name, "opus");
        assert_eq!(
            list[1].thinking,
            Some(vec![
                ThinkingLevel::Off,
                ThinkingLevel::Low,
                ThinkingLevel::Medium,
                ThinkingLevel::High
            ])
        );
        assert_eq!(list[2].thinking, None);
        assert!(list.iter().all(|model| !model.default));
    }

    #[test]
    fn current_reports_loading_then_ready_or_error_with_default_cwd_commands() {
        let catalog = Catalog::new(OmpLauncher::new("omp"), PathBuf::from("/w"));
        let loading = catalog.current();
        assert_eq!(loading.status, CatalogStatus::Warning);
        assert!(loading.message.is_some());
        assert!(catalog.catalog_now().is_none());

        catalog.put_commands("/w", &fixture_commands());
        *catalog.models.lock().unwrap() = Some(Ok(Vec::new()));
        let ready = catalog.catalog_now().unwrap();
        assert_eq!((ready.status, ready.message), (CatalogStatus::Ready, None));
        assert!(!ready.commands.is_empty() && !ready.skills.is_empty());
        assert!(ready.project_commands.is_empty());

        *catalog.models.lock().unwrap() = Some(Err("boom".into()));
        let failed = catalog.current();
        assert_eq!(failed.status, CatalogStatus::Error);
        assert_eq!(
            failed.message.as_deref(),
            Some("omp could not list models: boom")
        );
    }
}
