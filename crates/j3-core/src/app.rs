//! The j3 server application: every WS RPC method of the iOS thread surface
//! (ios-surface §2.1, §3, §4) on top of the store, hub, thread actors and the
//! omp catalog.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use futures_util::{
    StreamExt,
    future::BoxFuture,
    stream::{self, BoxStream},
};
use j3_protocol::j3::{self as j3, UsageError, UsageSnapshot};
use j3_protocol::t3::{
    self,
    commands::{
        DispatchCommandResult, OrchestrationV2Command, OrchestrationV2DispatchCommandError,
        OrchestrationV2GetShellSnapshotError, OrchestrationV2GetThreadProjectionError,
        OrchestrationV2ThreadLaunchError, ThreadDeleteCommand, ThreadLaunchInput,
        ThreadLaunchResult,
    },
    environment::ExecutionEnvironmentDescriptor,
    filesystem::{
        FilesystemBrowseEntry, FilesystemBrowseError, FilesystemBrowseFailure,
        FilesystemBrowseInput, FilesystemBrowseResult, FilesystemGetMetadataInput,
    },
    git::{GitRunStackedActionInput, VcsCwdInput},
    methods::{
        ArchivedShellSnapshot, ForceStopOutcome, ForceStopThreadInput, ForceStopThreadResult,
        GetThreadProjectionInput, GetTurnItemInput, GetTurnItemResult, SubscribeThreadInput,
    },
    orchestration::{
        Actor as Creator, CreationSource, ShellSnapshot, ThreadProjection, ThreadShell,
        WorkspaceStrategy,
    },
    project::{Project, ProjectMutation, ProjectMutationError, ProjectShell},
    server::{
        ServerConfig, ServerProvider, ServerRefreshProvidersInput, ServerSettingsError,
        ServerSettingsOperation,
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast::error::RecvError, oneshot};

use crate::{
    attachments::AttachmentStore,
    catalog::Catalog,
    engine::{
        actor::{self, ActorConfig, Request, ThreadHandle},
        model::{
            NewThread, Reject, ThreadModel, UserInput,
            triage::{self, DAY_MS},
        },
        projection::Projection,
        session::{OmpFactory, SessionFactory},
        streams::{self, SnapshotFn},
    },
    git, http,
    hub::{Commit, Hub, ShellChange},
    omp::OmpLauncher,
    rpc::{Response, RpcHandler, StreamItem},
    store::{self, Store, random_hex},
};

/// Close a thread's omp after it has been settled this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Coalescing window for streaming text.
const COALESCE: Duration = Duration::from_millis(50);
/// How long the config handshake waits for the model list before sending
/// a provider that is still loading.
const CONFIG_WAIT: Duration = Duration::from_secs(8);
/// Threads inactive this long settle automatically (t3's default
/// `sidebarAutoSettleAfterDays`, `contracts/src/settings.ts:85`).
const AUTO_SETTLE_AFTER_MS: i64 = 3 * DAY_MS;
/// How often the auto-settle sweep runs (also once at startup).
const AUTO_SETTLE_EVERY: Duration = Duration::from_secs(5 * 60);

pub struct AppConfig {
    /// `j3.sqlite`, `attachments/`, `logs/`.
    pub data_dir: PathBuf,
    /// Descriptor label (hostname).
    pub label: String,
    pub omp: OmpLauncher,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("creating {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] store::Error),
    #[error("recovering thread {thread}: {message}")]
    Recovery { thread: String, message: String },
}

/// The server application. Cheap to share; implements [`RpcHandler`].
pub struct App {
    inner: Arc<Inner>,
}

struct Inner {
    hub: Arc<Hub>,
    attachments: Arc<AttachmentStore>,
    catalog: Arc<Catalog>,
    /// For `omp usage` (`j3.getUsage`) and commit messages
    /// (`git.runStackedAction`).
    omp: OmpLauncher,
    actor_config: ActorConfig,
    actors: Mutex<HashMap<String, ThreadHandle>>,
    descriptor: Value,
    home: String,
    data_dir: PathBuf,
}

fn create_dir(path: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(path).map_err(|source| Error::Io {
        path: path.to_owned(),
        source,
    })
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("wire types always encode")
}

fn home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())
}

/// Expands a leading `~` to the home directory.
fn expand_home(path: &str, home: &str) -> PathBuf {
    if path == "~" {
        PathBuf::from(home)
    } else if let Some(rest) = path.strip_prefix("~/") {
        Path::new(home).join(rest)
    } else {
        PathBuf::from(path)
    }
}

/// A random UUIDv4-shaped id.
fn new_id() -> String {
    let hex = random_hex(32);
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

/// `ExecutionEnvironmentDescriptor` JSON (ios-surface H1).
fn descriptor(environment_id: &str, label: &str) -> Value {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        _ => "unknown",
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => "other",
    };
    let label = if label.trim().is_empty() {
        "j3"
    } else {
        label.trim()
    };
    let descriptor: ExecutionEnvironmentDescriptor = serde_json::from_value(json!({
        "environmentId": environment_id,
        "label": label,
        "platform": {"os": os, "arch": arch},
        "serverVersion": env!("CARGO_PKG_VERSION"),
        "orchestrationProtocolVersion": 2,
        "capabilities": {
            "repositoryIdentity": false,
            "attachmentUploads": true,
            "threadStop": true,
            "providerWorkspaceSnapshotsOnDemand": true,
            "threadSettlement": true,
            "threadAutoSettlement": true,
            "threadSnooze": true,
            "threadPinning": true,
            "threadPinReorder": true,
            "threadActiveReorder": true,
            "threadAutoSettleOptOut": true,
            "threadVisitedTracking": true,
        },
    }))
    .expect("descriptor matches ExecutionEnvironmentDescriptor");
    to_value(&descriptor)
}

fn dispatch_error(command_id: &str, command_type: &str, message: &str) -> Response {
    let command_id = if command_id.trim().is_empty() {
        "unknown"
    } else {
        command_id
    };
    let command_type = if command_type.trim().is_empty() {
        "unknown"
    } else {
        command_type
    };
    Response::fail(to_value(&OrchestrationV2DispatchCommandError::new(
        command_id,
        command_type,
        message,
    )))
}

fn projection_error(thread_id: &str, message: &str) -> Value {
    to_value(&OrchestrationV2GetThreadProjectionError::new(
        if thread_id.trim().is_empty() {
            "unknown"
        } else {
            thread_id
        },
        message,
    ))
}

/// Typed decode of a request payload; a bad payload is a per-request defect.
fn decode<T: serde::de::DeserializeOwned>(tag: &str, payload: Value) -> Result<T, Response> {
    serde_json::from_value(payload)
        .map_err(|error| Response::die(format!("invalid {tag} payload: {error}")))
}

/// A unary git method's answer: the value, or its `GitManagerServiceError`.
fn git_response<T: Serialize>(result: git::GitResult<T>) -> Response {
    match result {
        Ok(value) => Response::ok(to_value(&value)),
        Err(error) => Response::fail(to_value(&error)),
    }
}

/// The thread projection as stored.
fn stored_projection(
    store: &Store,
    thread_id: &str,
) -> Result<Option<ThreadProjection>, store::Error> {
    match store.thread(thread_id)? {
        Some(row) if !row.deleted => {}
        _ => return Ok(None),
    }
    Ok(Projection::from_rows(store.entities(thread_id)?)?.map(|projection| projection.to_wire()))
}

/// Shell rows of stored threads, split into (active, archived).
fn stored_shells(store: &Store) -> Result<(Vec<ThreadShell>, Vec<ThreadShell>), store::Error> {
    let mut active = Vec::new();
    let mut archived = Vec::new();
    for row in store.threads()? {
        let shell: ThreadShell = serde_json::from_value(row.shell)?;
        if row.archived {
            archived.push(shell);
        } else {
            active.push(shell);
        }
    }
    Ok((active, archived))
}

fn stored_project_shells(store: &Store) -> Result<Vec<ProjectShell>, store::Error> {
    store
        .projects()?
        .into_iter()
        .map(|json| Ok(serde_json::from_value(json)?))
        .collect()
}

impl App {
    /// Opens the data directory, recovers interrupted threads, starts
    /// loading the omp model catalog in the background and runs the
    /// auto-settle sweep now and every [`AUTO_SETTLE_EVERY`].
    pub async fn open(config: AppConfig) -> Result<Arc<App>, Error> {
        let factory = Arc::new(OmpFactory(config.omp.clone()));
        let app = Self::open_with(config, factory, IDLE_TIMEOUT).await?;
        let catalog = Arc::clone(&app.inner.catalog);
        tokio::spawn(async move {
            catalog.provider().await;
        });
        let inner = Arc::downgrade(&app.inner);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(AUTO_SETTLE_EVERY);
            every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                every.tick().await;
                let Some(inner) = inner.upgrade() else {
                    break;
                };
                inner
                    .auto_settle(triage::now_ms(), AUTO_SETTLE_AFTER_MS)
                    .await;
            }
        });
        Ok(app)
    }

    /// [`App::open`] with a custom session factory and idle timeout (tests,
    /// embedding). Does not preload the catalog or run the auto-settle sweep
    /// (see [`App::auto_settle`]).
    pub async fn open_with(
        config: AppConfig,
        factory: Arc<dyn SessionFactory>,
        idle_timeout: Duration,
    ) -> Result<Arc<App>, Error> {
        create_dir(&config.data_dir)?;
        create_dir(&config.data_dir.join("logs"))?;
        let store = Store::open(&config.data_dir.join("j3.sqlite"))?;
        let attachments_dir = config.data_dir.join("attachments");
        let attachments = Arc::new(AttachmentStore::open(attachments_dir.clone()).map_err(
            |source| Error::Io {
                path: attachments_dir,
                source,
            },
        )?);
        let descriptor = descriptor(store.environment_id(), &config.label);
        let hub = Arc::new(Hub::new(store));
        let home = home_dir();
        let catalog = Arc::new(Catalog::new(config.omp.clone(), PathBuf::from(&home)));
        let sink_catalog = Arc::clone(&catalog);
        let actor_config = ActorConfig {
            hub: Arc::clone(&hub),
            factory,
            attachments: Arc::clone(&attachments),
            idle_timeout,
            coalesce: COALESCE,
            commands: Some(Arc::new(move |cwd, commands| {
                sink_catalog.put_commands(cwd, commands)
            })),
        };
        let inner = Arc::new(Inner {
            hub,
            attachments,
            catalog,
            actor_config,
            actors: Mutex::new(HashMap::new()),
            descriptor,
            home,
            data_dir: config.data_dir,
            omp: config.omp,
        });
        inner.recover()?;
        Ok(Arc::new(App { inner }))
    }

    /// `ExecutionEnvironmentDescriptor` JSON (static for the process).
    pub fn descriptor(&self) -> Value {
        self.inner.descriptor.clone()
    }

    /// One auto-settle sweep as of `now_ms` (epoch ms): settles every thread
    /// t3 would settle automatically after `inactive_for` without activity.
    /// Returns how many it settled.
    pub async fn auto_settle(&self, now_ms: i64, inactive_for: Duration) -> usize {
        let after_ms = i64::try_from(inactive_for.as_millis()).unwrap_or(i64::MAX);
        self.inner.auto_settle(now_ms, after_ms).await
    }

    pub fn attachments(&self) -> Arc<AttachmentStore> {
        Arc::clone(&self.inner.attachments)
    }

    /// Stops every thread actor and its omp process.
    pub async fn shutdown(&self) {
        let handles: Vec<ThreadHandle> = self
            .inner
            .actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .map(|(_, handle)| handle)
            .collect();
        for handle in handles {
            let (reply, done) = oneshot::channel();
            if handle.send(Request::Shutdown { reply }).is_ok() {
                let _ = done.await;
            }
        }
    }
}

impl RpcHandler for App {
    fn call(&self, tag: String, payload: Value) -> BoxFuture<'static, Response> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { inner.call(&tag, payload).await })
    }
}

/// Why a thread actor could not be reached.
enum ThreadError {
    NotFound,
    Store(store::Error),
}

impl Inner {
    /// Runs left non-terminal by a previous process are interrupted and
    /// pending requests expire, through normal commits.
    fn recover(&self) -> Result<(), Error> {
        let rows = self.hub.read(|store| store.threads())?;
        for row in rows {
            let entities = self.hub.read(|store| store.entities(&row.id))?;
            let Some(projection) = Projection::from_rows(entities).map_err(store::Error::from)?
            else {
                continue;
            };
            let mut model = ThreadModel::load(projection);
            if !model.needs_recovery() {
                continue;
            }
            model.recover();
            let mut last_shell = Some(row.shell.clone());
            actor::commit_thread(
                &self.hub,
                &mut model,
                row.session_file.clone(),
                false,
                &mut last_shell,
                None,
            )
            .map_err(|Reject(message)| Error::Recovery {
                thread: row.id.clone(),
                message,
            })?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Thread actors
    // -----------------------------------------------------------------------

    fn actor(&self, thread_id: &str) -> Result<ThreadHandle, ThreadError> {
        let mut actors = self.actors.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = actors.get(thread_id).filter(|handle| !handle.is_closed()) {
            return Ok(handle.clone());
        }
        let (row, entities) = self
            .hub
            .read(|store| -> Result<_, store::Error> {
                Ok((store.thread(thread_id)?, store.entities(thread_id)?))
            })
            .map_err(ThreadError::Store)?;
        let Some(row) = row.filter(|row| !row.deleted) else {
            return Err(ThreadError::NotFound);
        };
        let projection = Projection::from_rows(entities)
            .map_err(|error| ThreadError::Store(error.into()))?
            .ok_or(ThreadError::NotFound)?;
        let handle = actor::spawn(
            self.actor_config.clone(),
            ThreadModel::load(projection),
            row.session_file,
            Some(row.shell),
        );
        actors.insert(thread_id.to_owned(), handle.clone());
        Ok(handle)
    }

    /// Sends a request built by `make` to the thread's actor and awaits the
    /// reply, restarting the actor once if it ended meanwhile.
    async fn ask<T>(
        &self,
        thread_id: &str,
        make: impl Fn(oneshot::Sender<T>) -> Request,
    ) -> Result<T, ThreadError> {
        for _ in 0..2 {
            let handle = self.actor(thread_id)?;
            let (reply, response) = oneshot::channel();
            if handle.send(make(reply)).is_ok()
                && let Ok(value) = response.await
            {
                return Ok(value);
            }
            self.actors
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(thread_id);
        }
        Err(ThreadError::NotFound)
    }

    /// The auto-settle sweep (t3 `ThreadSettlementService`, without pull
    /// requests). Stored shells pick the threads that look due; each one's
    /// actor decides again on its live state (and the snooze wake time the
    /// shell lacks) and settles it with a `server:auto-settle:<id>:<snapshot>`
    /// receipt, so user changes after the snapshot win.
    async fn auto_settle(&self, now_ms: i64, after_ms: i64) -> usize {
        let rows = match self.hub.read(|store| store.threads()) {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, "reading threads to auto-settle");
                return 0;
            }
        };
        let mut settled = 0;
        for row in rows {
            if row.archived || row.deleted {
                continue;
            }
            let Ok(shell) = serde_json::from_value::<ThreadShell>(row.shell) else {
                continue;
            };
            // The snooze wake time only delays settlement, so a thread that
            // isn't due without it isn't due at all.
            if triage::auto_settlement_at(&shell, None, now_ms, after_ms).is_none() {
                continue;
            }
            let snapshot_at = shell.updated_at;
            let result = self
                .ask(&row.id, |reply| Request::AutoSettle {
                    now_ms,
                    after_ms,
                    snapshot_at: snapshot_at.clone(),
                    reply,
                })
                .await;
            match result {
                Ok(Ok(Some(_))) => settled += 1,
                Ok(Ok(None)) | Err(ThreadError::NotFound) => {}
                Ok(Err(Reject(message))) => {
                    tracing::debug!(thread = %row.id, message, "automatic settlement skipped");
                }
                Err(ThreadError::Store(error)) => {
                    tracing::warn!(%error, thread = %row.id, "loading thread to auto-settle");
                }
            }
        }
        settled
    }

    // -----------------------------------------------------------------------
    // Dispatch
    // -----------------------------------------------------------------------

    async fn call(&self, tag: &str, payload: Value) -> Response {
        if let Some(response) = self.attachments.handle(tag, payload.clone()).await {
            return response;
        }
        match tag {
            "subscribeServerConfig" => self.subscribe_server_config(),
            "server.getConfig" => {
                let provider = self.catalog.provider().await;
                Response::ok((self.config_builder())(provider))
            }
            "server.probe" => Response::ok(json!({})),
            "server.refreshProviders" => self.refresh_providers(payload).await,
            "server.reportClientActivity" => Response::ok(Value::Null),
            "server.updateSettings" => Response::fail(to_value(&ServerSettingsError::new(
                self.data_dir.join("settings.json").to_string_lossy(),
                ServerSettingsOperation::Normalize,
            ))),
            "orchestration.subscribeShell" => Response::Stream(self.shell_stream()),
            "orchestration.subscribeThread" => match decode::<SubscribeThreadInput>(tag, payload) {
                Ok(input) => Response::Stream(self.thread_stream(&input.thread_id)),
                Err(response) => response,
            },
            "orchestration.dispatchCommand" => self.dispatch(payload).await,
            "orchestration.launchThread" => match decode::<ThreadLaunchInput>(tag, payload) {
                Ok(input) => self.launch(input).await,
                Err(response) => response,
            },
            "orchestration.getThreadProjection" => {
                match decode::<GetThreadProjectionInput>(tag, payload) {
                    Ok(input) => match self.projection(&input.thread_id).await {
                        Ok(projection) => Response::ok(to_value(&projection)),
                        Err(error) => Response::fail(error),
                    },
                    Err(response) => response,
                }
            }
            "orchestration.getArchivedShellSnapshot" => self.archived_snapshot(),
            "orchestration.getTurnItem" => match decode::<GetTurnItemInput>(tag, payload) {
                Ok(input) => {
                    let item =
                        self.projection(&input.thread_id)
                            .await
                            .ok()
                            .and_then(|projection| {
                                projection
                                    .turn_items
                                    .into_iter()
                                    .find(|item| item.id == input.item_id)
                            });
                    Response::ok(to_value(&GetTurnItemResult { item }))
                }
                Err(response) => response,
            },
            "orchestration.searchThreads" => self.search_threads(payload),
            "orchestration.forceStopThread" => match decode::<ForceStopThreadInput>(tag, payload) {
                Ok(input) => {
                    let _ = self
                        .ask(&input.thread_id, |reply| Request::ForceStop { reply })
                        .await;
                    Response::ok(to_value(&ForceStopThreadResult {
                        outcome: ForceStopOutcome::Killed,
                        shared_with_thread_ids: Vec::new(),
                    }))
                }
                Err(response) => response,
            },
            "projects.mutate" => self.mutate_project(payload).await,
            "filesystem.browse" => match decode::<FilesystemBrowseInput>(tag, payload) {
                Ok(input) => self.browse(input),
                Err(response) => response,
            },
            "projects.readFile" => {
                let relative_path = payload
                    .get("relativePath")
                    .and_then(Value::as_str)
                    .unwrap_or("t3.json")
                    .to_owned();
                Response::ok(json!({
                    "relativePath": relative_path, "contents": "", "byteLength": 0,
                    "truncated": false, "exists": false,
                }))
            }
            "projects.searchEntries" => Response::ok(json!({"entries": [], "truncated": false})),
            "filesystem.getMetadata" => match decode::<FilesystemGetMetadataInput>(tag, payload) {
                Ok(input) => Response::ok(json!({"entries": vec![Value::Null; input.paths.len()]})),
                Err(response) => response,
            },
            j3::GET_USAGE => self.usage().await,
            t3::git::VCS_REFRESH_STATUS => match decode::<VcsCwdInput>(tag, payload) {
                Ok(input) => git_response(git::status(&input.cwd).await),
                Err(response) => response,
            },
            t3::git::VCS_PULL => match decode::<VcsCwdInput>(tag, payload) {
                Ok(input) => git_response(git::pull(&input.cwd).await),
                Err(response) => response,
            },
            t3::git::GIT_RUN_STACKED_ACTION => {
                match decode::<GitRunStackedActionInput>(tag, payload) {
                    Ok(input) => Response::Stream(git::run_stacked_action(self.omp.clone(), input)),
                    Err(response) => response,
                }
            }
            other => Response::die(format!("{other} is not supported by this server")),
        }
    }

    /// `j3.getUsage`: this host's `omp usage --json`, as a [`UsageSnapshot`].
    async fn usage(&self) -> Response {
        let snapshot = match self.omp.usage().await {
            Ok(stdout) => serde_json::from_slice::<UsageSnapshot>(&stdout)
                .map_err(|error| format!("unexpected omp usage output: {error}")),
            Err(error) => Err(error.to_string()),
        };
        match snapshot {
            Ok(snapshot) => Response::ok(to_value(&snapshot)),
            Err(message) => Response::fail(to_value(&UsageError { message })),
        }
    }

    // -----------------------------------------------------------------------
    // Server config and providers
    // -----------------------------------------------------------------------

    /// M1: the snapshot handshake, then provider updates; never ends.
    fn subscribe_server_config(&self) -> Response {
        let catalog = Arc::clone(&self.catalog);
        let updates = catalog.subscribe();
        let config = self.config_builder();
        let first = async move {
            let provider = match catalog.provider_now() {
                Some(provider) => provider,
                None => tokio::time::timeout(CONFIG_WAIT, catalog.provider())
                    .await
                    .unwrap_or_else(|_| catalog.current()),
            };
            StreamItem::Value(json!({"version": 1, "type": "snapshot", "config": config(provider)}))
        };
        let live = stream::unfold(updates, |mut updates| async move {
            loop {
                match updates.recv().await {
                    Ok(provider) => {
                        let item = json!({
                            "version": 1,
                            "type": "providerStatuses",
                            "payload": {"providers": [to_value(&provider)]},
                        });
                        return Some((StreamItem::Value(item), updates));
                    }
                    Err(RecvError::Lagged(_)) => continue,
                    // The catalog lives as long as the app; never end the
                    // stream (the client drops the session if it ends).
                    Err(RecvError::Closed) => std::future::pending::<()>().await,
                }
            }
        });
        Response::Stream(stream::once(first).chain(live).boxed())
    }

    /// A `'static` builder of `ServerConfig` JSON.
    fn config_builder(&self) -> impl Fn(ServerProvider) -> Value + Send + 'static {
        let descriptor = self.descriptor.clone();
        let home = self.home.clone();
        let data_dir = self.data_dir.clone();
        move |provider| {
            let logs = data_dir.join("logs");
            let config: ServerConfig = serde_json::from_value(json!({
                "environment": descriptor,
                "auth": http::auth_descriptor(),
                "cwd": home,
                "keybindingsConfigPath": data_dir.join("keybindings.json").to_string_lossy(),
                "keybindings": [],
                "issues": [],
                "providers": [to_value(&provider)],
                "availableEditors": [],
                "observability": {
                    "logsDirectoryPath": logs.to_string_lossy(),
                    "localTracingEnabled": false,
                    "otlpTracesEnabled": false,
                    "otlpMetricsEnabled": false,
                    "otlpLogsEnabled": false,
                },
                // The contracts treat an instance missing from `providerInstances` as
                // enabled only for codex/claudeAgent; `omp` (driver pi) must be listed
                // or clients drop its models (`resolveProjectProviderInstanceEnabled`).
                "settings": {
                    "providerInstances": {
                        "omp": {"driver": "pi", "displayName": "omp", "enabled": true},
                    },
                },
            }))
            .expect("server config matches ServerConfig");
            to_value(&config)
        }
    }

    async fn refresh_providers(&self, payload: Value) -> Response {
        let input: ServerRefreshProvidersInput =
            serde_json::from_value(payload).unwrap_or_default();
        let provider = self
            .catalog
            .refresh(
                input.cwd.as_deref(),
                input.refresh_models == Some(true),
                input.fresh == Some(true),
            )
            .await;
        Response::ok(json!({"providers": [to_value(&provider)]}))
    }

    // -----------------------------------------------------------------------
    // Streams and snapshots
    // -----------------------------------------------------------------------

    fn shell_stream(&self) -> BoxStream<'static, StreamItem> {
        let snapshot: SnapshotFn = Arc::new(|store: &Store| {
            let fail = |error: store::Error| {
                tracing::error!(%error, "reading shell snapshot");
                to_value(&OrchestrationV2GetShellSnapshotError::new(
                    "The thread list could not be read.",
                ))
            };
            let (threads, _archived) = stored_shells(store).map_err(fail)?;
            let projects = stored_project_shells(store).map_err(fail)?;
            Ok(to_value(&ShellSnapshot {
                schema_version: 1,
                snapshot_sequence: store.high_water(),
                threads,
                archived_threads: Vec::new(),
                projects,
                older_settled: None,
            }))
        });
        streams::shell_stream(Arc::clone(&self.hub), snapshot)
    }

    fn thread_stream(&self, thread_id: &str) -> BoxStream<'static, StreamItem> {
        let id = thread_id.to_owned();
        let snapshot: SnapshotFn =
            Arc::new(move |store: &Store| match stored_projection(store, &id) {
                Ok(Some(projection)) => Ok(to_value(&projection)),
                Ok(None) => Err(projection_error(&id, "This thread doesn't exist.")),
                Err(error) => {
                    tracing::error!(%error, thread = %id, "reading thread snapshot");
                    Err(projection_error(&id, "The thread could not be read."))
                }
            });
        streams::thread_stream(Arc::clone(&self.hub), thread_id, snapshot)
    }

    async fn projection(&self, thread_id: &str) -> Result<ThreadProjection, Value> {
        let running = self
            .actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(thread_id)
            .is_some_and(|handle| !handle.is_closed());
        if running
            && let Ok(projection) = self
                .ask(thread_id, |reply| Request::Projection { reply })
                .await
        {
            return Ok(projection);
        }
        match self.hub.read(|store| stored_projection(store, thread_id)) {
            Ok(Some(projection)) => Ok(projection),
            Ok(None) => Err(projection_error(thread_id, "This thread doesn't exist.")),
            Err(error) => {
                tracing::error!(%error, "reading projection");
                Err(projection_error(thread_id, "The thread could not be read."))
            }
        }
    }

    fn archived_snapshot(&self) -> Response {
        let result = self.hub.read(|store| -> Result<_, store::Error> {
            let (_, archived) = stored_shells(store)?;
            Ok(ArchivedShellSnapshot {
                schema_version: 1,
                snapshot_sequence: store.high_water(),
                projects: stored_project_shells(store)?,
                threads: archived,
            })
        });
        match result {
            Ok(snapshot) => Response::ok(to_value(&snapshot)),
            Err(error) => {
                tracing::error!(%error, "reading archived threads");
                Response::fail(to_value(&OrchestrationV2GetShellSnapshotError::new(
                    "The archived threads could not be read.",
                )))
            }
        }
    }

    fn search_threads(&self, payload: Value) -> Response {
        let query = payload
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_lowercase();
        let limit = payload.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
        let shells = self.hub.read(stored_shells);
        let matches: Vec<Value> = match shells {
            Ok((active, archived)) => active
                .into_iter()
                .chain(archived)
                .filter(|shell| !query.is_empty() && shell.title.to_lowercase().contains(&query))
                .take(limit)
                .map(|shell| {
                    json!({
                        "threadId": shell.id,
                        "projectId": shell.project_id,
                        "source": "user",
                        "snippet": shell.title,
                        "messageCreatedAt": shell.updated_at,
                    })
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        Response::ok(json!({"matches": matches}))
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    async fn dispatch(&self, payload: Value) -> Response {
        let command_id = payload
            .get("commandId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let command_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let command: OrchestrationV2Command = match serde_json::from_value(payload) {
            Ok(command) => command,
            Err(error) => {
                tracing::debug!(%error, command_type, "undecodable command");
                return dispatch_error(
                    &command_id,
                    &command_type,
                    "This request could not be read.",
                );
            }
        };
        if let OrchestrationV2Command::Unknown(_) = command {
            return dispatch_error(
                &command_id,
                &command_type,
                "This action isn't supported by this server.",
            );
        }
        if let Ok(Some(receipt)) = self.hub.read(|store| store.receipt(&command_id)) {
            let sequence = receipt.get("sequence").and_then(Value::as_u64).unwrap_or(0);
            return Response::ok(to_value(&DispatchCommandResult { sequence }));
        }
        let thread_id = command.thread_id().unwrap_or_default().to_owned();
        let is_delete = matches!(command, OrchestrationV2Command::ThreadDelete(_));
        let result = self
            .ask(&thread_id, |reply| Request::Dispatch {
                command: Box::new(command.clone()),
                reply,
            })
            .await;
        match result {
            Ok(Ok(sequence)) => {
                if is_delete {
                    self.actors
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&thread_id);
                }
                Response::ok(to_value(&DispatchCommandResult { sequence }))
            }
            Ok(Err(Reject(message))) => dispatch_error(&command_id, &command_type, &message),
            Err(ThreadError::NotFound) if is_delete => {
                Response::ok(to_value(&DispatchCommandResult {
                    sequence: self.hub.high_water(),
                }))
            }
            Err(ThreadError::NotFound) => {
                dispatch_error(&command_id, &command_type, "This thread no longer exists.")
            }
            Err(ThreadError::Store(error)) => {
                tracing::error!(%error, "loading thread");
                dispatch_error(
                    &command_id,
                    &command_type,
                    "The thread could not be loaded.",
                )
            }
        }
    }

    async fn launch(&self, input: ThreadLaunchInput) -> Response {
        let fail = |message: &str| {
            Response::fail(to_value(&OrchestrationV2ThreadLaunchError::new(
                input.command_id.clone(),
                input.project_id.clone(),
                message,
            )))
        };
        let replay = self
            .hub
            .read(|store| store.receipt(&input.command_id))
            .ok()
            .flatten()
            .and_then(|receipt| {
                Some((
                    receipt.get("threadId")?.as_str()?.to_owned(),
                    receipt.get("sequence").and_then(Value::as_u64),
                ))
            });
        let existing = input.thread_id.as_ref().filter(|thread_id| {
            self.hub
                .read(|store| store.thread(thread_id))
                .ok()
                .flatten()
                .is_some_and(|row| !row.deleted)
        });
        if let Some((thread_id, sequence)) =
            replay.or_else(|| existing.map(|id| (id.clone(), None)))
        {
            return match self.projection(&thread_id).await {
                Ok(projection) => Response::ok(to_value(&ThreadLaunchResult {
                    thread_id,
                    projection,
                    resumed: true,
                    sequence,
                })),
                Err(_) => fail("This thread no longer exists."),
            };
        }
        if !matches!(input.workspace_strategy, WorkspaceStrategy::Root { .. }) {
            return fail(
                "Worktrees aren't supported here; start the thread in the project folder.",
            );
        }
        let project: Option<Project> = self
            .hub
            .read(|store| store.project(&input.project_id))
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_value(json).ok());
        let Some(project) = project else {
            return fail("This project no longer exists.");
        };
        let thread_id = input.thread_id.clone().unwrap_or_else(new_id);
        let creation_source = input.creation_source.unwrap_or(CreationSource::Mobile);
        let model = ThreadModel::create(NewThread {
            id: thread_id.clone(),
            project_id: input.project_id.clone(),
            title: input.title.clone(),
            model_selection: input.model_selection.clone(),
            runtime_mode: input.runtime_mode,
            interaction_mode: input.interaction_mode,
            created_by: Creator::User,
            creation_source,
            cwd: project.workspace_root.clone(),
        });
        let initial = input.initial_message.as_ref().map(|message| UserInput {
            message_id: message.message_id.clone().unwrap_or_else(new_id),
            text: message.text.clone(),
            attachments: message.attachments.clone(),
            context: message.context.clone(),
            model_selection: input.model_selection.clone(),
            created_by: Creator::User,
            creation_source,
        });
        let handle = actor::spawn(self.actor_config.clone(), model, None, None);
        self.actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(thread_id.clone(), handle.clone());
        let (reply, response) = oneshot::channel();
        if handle
            .send(Request::Launch {
                command_id: input.command_id.clone(),
                input: initial.map(Box::new),
                reply,
            })
            .is_err()
        {
            return fail("The thread could not be started.");
        }
        match response.await {
            Ok(Ok((sequence, projection))) => Response::ok(to_value(&ThreadLaunchResult {
                thread_id,
                projection,
                resumed: false,
                sequence: Some(sequence),
            })),
            Ok(Err(Reject(message))) => fail(&message),
            Err(_) => fail("The thread could not be started."),
        }
    }

    // -----------------------------------------------------------------------
    // Projects
    // -----------------------------------------------------------------------

    async fn mutate_project(&self, payload: Value) -> Response {
        let command_id = payload
            .get("commandId")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        let fail = |message: &str| {
            Response::fail(to_value(&ProjectMutationError::new(
                command_id.clone(),
                message,
            )))
        };
        let mutation: ProjectMutation = match serde_json::from_value(payload) {
            Ok(mutation) => mutation,
            Err(_) => return fail("This project change could not be read."),
        };
        if let Ok(Some(project)) = self.hub.read(|store| store.receipt(&command_id)) {
            return Response::ok(project);
        }
        let now = t3::now_iso();
        let (project_id, project, removed) = match mutation {
            ProjectMutation::Create(create) => {
                if let Ok(Some(existing)) = self.hub.read(|store| store.project(&create.project_id))
                {
                    return Response::ok(existing);
                }
                let root = expand_home(create.workspace_root.trim(), &self.home);
                if !root.is_dir() {
                    if create.create_workspace_root_if_missing == Some(true) {
                        if std::fs::create_dir_all(&root).is_err() {
                            return fail("The folder could not be created.");
                        }
                    } else {
                        return fail("That folder doesn't exist on this machine.");
                    }
                }
                let title = if create.title.trim().is_empty() {
                    root.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "Project".into())
                } else {
                    create.title.trim().to_owned()
                };
                let project = json!({
                    "id": create.project_id,
                    "title": title,
                    "workspaceRoot": root.to_string_lossy(),
                    "defaultModelSelection": create.default_model_selection.flatten(),
                    "scripts": create.scripts.unwrap_or_default(),
                    "createdAt": now,
                    "updatedAt": now,
                    "deletedAt": null,
                });
                (create.project_id, project, false)
            }
            ProjectMutation::Update(update) => {
                let Ok(Some(mut project)) =
                    self.hub.read(|store| store.project(&update.project_id))
                else {
                    return fail("This project no longer exists.");
                };
                if let Some(title) = update.title.filter(|title| !title.trim().is_empty()) {
                    project["title"] = json!(title.trim());
                }
                if let Some(root) = update.workspace_root.filter(|root| !root.trim().is_empty()) {
                    let root = expand_home(root.trim(), &self.home);
                    if !root.is_dir() {
                        return fail("That folder doesn't exist on this machine.");
                    }
                    project["workspaceRoot"] = json!(root.to_string_lossy());
                }
                if let Some(selection) = update.default_model_selection {
                    project["defaultModelSelection"] = to_value(&selection);
                }
                if let Some(scripts) = update.scripts {
                    project["scripts"] = to_value(&scripts);
                }
                if let Some(pinned) = update.pinned {
                    project["pinnedAt"] = if pinned { json!(now) } else { Value::Null };
                }
                if let Some(archived) = update.archived {
                    project["archivedAt"] = if archived { json!(now) } else { Value::Null };
                }
                project["updatedAt"] = json!(now);
                (update.project_id, project, false)
            }
            ProjectMutation::Delete(delete) => {
                let Ok(Some(project)) = self.hub.read(|store| store.project(&delete.project_id))
                else {
                    return Response::ok(json!({
                        "id": delete.project_id, "title": "Deleted project", "workspaceRoot": "/",
                        "defaultModelSelection": null, "scripts": [], "createdAt": now,
                        "updatedAt": now, "deletedAt": now,
                    }));
                };
                let threads: Vec<String> = self
                    .hub
                    .read(|store| store.threads())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|row| row.project_id == delete.project_id)
                    .map(|row| row.id)
                    .collect();
                if !threads.is_empty() && delete.force != Some(true) {
                    return fail("Delete this project's threads first.");
                }
                for thread_id in threads {
                    let command = OrchestrationV2Command::ThreadDelete(ThreadDeleteCommand {
                        command_id: format!("{command_id}:{thread_id}"),
                        thread_id: thread_id.clone(),
                        only_if_unused: None,
                    });
                    let _ = self
                        .ask(&thread_id, |reply| Request::Dispatch {
                            command: Box::new(command.clone()),
                            reply,
                        })
                        .await;
                    self.actors
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&thread_id);
                }
                let mut project = project;
                project["deletedAt"] = json!(now);
                project["updatedAt"] = json!(now);
                (delete.project_id, project, true)
            }
        };
        self.commit_project(&command_id, project_id, project, removed)
    }

    /// Saves (or removes) a project with its shell item and receipt; the
    /// response is the `Project`.
    fn commit_project(
        &self,
        command_id: &str,
        project_id: String,
        project: Value,
        removed: bool,
    ) -> Response {
        let Ok(typed) = serde_json::from_value::<Project>(project) else {
            return Response::fail(to_value(&ProjectMutationError::new(
                command_id.to_owned(),
                "The project is invalid.",
            )));
        };
        let json = to_value(&typed);
        let change = if removed {
            ShellChange::ProjectRemoved(project_id.clone())
        } else {
            let shell: ProjectShell =
                serde_json::from_value(json.clone()).expect("a Project is a ProjectShell");
            ShellChange::ProjectUpdated(to_value(&shell))
        };
        let result_json = json.clone();
        let commit = Commit {
            thread: None,
            projects: vec![(project_id, (!removed).then(|| json.clone()))],
            shell: vec![change],
            receipt: Some((command_id.to_owned(), Box::new(move |_| result_json))),
        };
        match self.hub.commit(commit) {
            Ok(_) => Response::ok(json),
            Err(error) => {
                tracing::error!(%error, "saving project");
                Response::fail(to_value(&ProjectMutationError::new(
                    command_id.to_owned(),
                    "The project could not be saved.",
                )))
            }
        }
    }

    // -----------------------------------------------------------------------
    // Filesystem
    // -----------------------------------------------------------------------

    fn browse(&self, input: FilesystemBrowseInput) -> Response {
        let partial = input.partial_path.trim();
        let base = input
            .cwd
            .as_deref()
            .map(|cwd| expand_home(cwd, &self.home))
            .unwrap_or_else(|| PathBuf::from(&self.home));
        let mut dir = if partial.is_empty() {
            base
        } else {
            let path = expand_home(partial, &self.home);
            if path.is_absolute() {
                path
            } else {
                base.join(path)
            }
        };
        if !partial.ends_with('/')
            && !dir.is_dir()
            && let Some(parent) = dir.parent()
        {
            dir = parent.to_owned();
        }
        let fail = |message: String| {
            let mut error = FilesystemBrowseError::new(message);
            error.partial_path = Some(input.partial_path.clone());
            error.failure = Some(FilesystemBrowseFailure::ReadDirectoryFailed);
            Response::fail(to_value(&error))
        };
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) => return fail(format!("{} can't be listed: {error}", dir.display())),
        };
        let mut listed: Vec<FilesystemBrowseEntry> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .map(|entry| FilesystemBrowseEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                full_path: entry.path().to_string_lossy().into_owned(),
            })
            .collect();
        listed.sort_by_cached_key(|entry| entry.name.to_lowercase());
        Response::ok(to_value(&FilesystemBrowseResult {
            parent_path: dir.to_string_lossy().into_owned(),
            entries: listed,
        }))
    }
}
