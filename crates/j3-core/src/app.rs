//! The j3 server application behind protocol v2 (`docs/protocol/wire.md`):
//! the store, hub, thread actors, the omp catalog, terminals and project
//! files. [`crate::v2::router`] decodes calls and answers them here.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use j3_protocol::v2::{
    PROTOCOL,
    entities::{Attachment, ModelRef, Project},
    envelope::{Error, ErrorCode},
    methods::{
        BrowseResult, BtwAskParams, Catalog, CreateProjectParams, CreateThreadParams,
        DeleteProjectParams, EntriesResult, GitAction, GitRunItem, GitStatus, ImageUrlResult,
        PullResult, ReadFileResult, SearchEntriesParams, TranscriptResult, UpdateProjectParams,
        Usage,
    },
    streams::ThreadSnapshot,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast::error::RecvError, mpsc, oneshot};

use crate::{
    attachments::{self, AttachmentKind, AttachmentStore},
    catalog::Catalog as OmpCatalog,
    clock::{iso, now_iso, now_ms},
    engine::{
        actor::{self, ActorConfig, Command, Request, ThreadHandle},
        model::{NewThread, ThreadModel, UserInput},
        session::{OmpFactory, SessionFactory},
        streams,
        triage::{self, AUTO_SETTLE_AFTER_MS, Facts},
    },
    entry_search::EntryIndex,
    files, git,
    hub::{Commit, Hub, ProjectWrite, ResultFn},
    omp::OmpLauncher,
    store::{self, Store, ThreadIndex, ThreadRecords, random_hex},
    terminal::Terminals,
    transcript,
    v2::{driver::StreamEvent, replay},
};

/// Close a thread's omp after it has been idle this long.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Coalescing window for growing text (wire.md §6.4).
const COALESCE: Duration = Duration::from_millis(50);
/// How long `server.watch` waits for the model list before sending a
/// catalog that is still loading.
const CATALOG_WAIT: Duration = Duration::from_secs(8);
/// How often the auto-settle sweep runs (also once at startup).
const AUTO_SETTLE_EVERY: Duration = Duration::from_secs(5 * 60);

pub struct AppConfig {
    /// `j3.sqlite`, `attachments/`, `logs/`.
    pub data_dir: PathBuf,
    /// Server name shown in clients (the host name).
    pub label: String,
    pub omp: OmpLauncher,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
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

/// The server application. Cheap to share.
pub struct App {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    hub: Arc<Hub>,
    attachments: Arc<AttachmentStore>,
    catalog: Arc<OmpCatalog>,
    /// For `omp usage` and the git actions' commit messages.
    omp: OmpLauncher,
    actor_config: ActorConfig,
    actors: Mutex<HashMap<String, ThreadHandle>>,
    /// Numbers `btw.ask` calls.
    next_btw: AtomicU64,
    info: Value,
    home: String,
    /// Every thread's terminals.
    terminals: Terminals,
    /// Recent project walks answering `projects.searchEntries`.
    entries: Arc<EntryIndex>,
}

fn create_dir(path: &Path) -> Result<(), OpenError> {
    std::fs::create_dir_all(path).map_err(|source| OpenError::Io {
        path: path.to_owned(),
        source,
    })
}

fn encode<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("v2 types always encode")
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
pub(crate) fn new_id() -> String {
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

fn storage(error: store::Error) -> Error {
    tracing::error!(%error, "store");
    Error::internal("The server could not read its state.")
}

/// Runs blocking file work off the runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| Error::internal(error.to_string()))?
}

/// `GET /api/v1/info` (wire.md §2).
fn info(server_id: &str, label: &str, home: &str) -> Value {
    let name = if label.trim().is_empty() {
        "j3"
    } else {
        label.trim()
    };
    json!({
        "serverId": server_id,
        "name": name,
        "protocol": PROTOCOL,
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": ["terminal", "git", "files", "usage", "goals", "btw"],
        "platform": std::env::consts::OS,
        "home": home,
    })
}

impl App {
    /// Opens the data directory (migrating and rebuilding threads from their
    /// omp sessions when needed), recovers interrupted threads, starts
    /// loading omp's catalog in the background and runs the auto-settle
    /// sweep now and every [`AUTO_SETTLE_EVERY`].
    pub async fn open(config: AppConfig) -> Result<Arc<App>, OpenError> {
        let factory = Arc::new(OmpFactory(config.omp.clone()));
        let app = Self::open_with(config, factory, IDLE_TIMEOUT).await?;
        let catalog = Arc::clone(&app.inner.catalog);
        tokio::spawn(async move {
            catalog.catalog().await;
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
                inner.auto_settle(now_ms(), AUTO_SETTLE_AFTER_MS).await;
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
    ) -> Result<Arc<App>, OpenError> {
        create_dir(&config.data_dir)?;
        create_dir(&config.data_dir.join("logs"))?;
        let store = Store::open(&config.data_dir.join("j3.sqlite"))?;
        let attachments_dir = config.data_dir.join("attachments");
        let attachments = Arc::new(AttachmentStore::open(attachments_dir.clone()).map_err(
            |source| OpenError::Io {
                path: attachments_dir,
                source,
            },
        )?);
        let home = home_dir();
        let info = info(store.server_id(), &config.label, &home);
        let hub = Arc::new(Hub::new(store));
        let catalog = Arc::new(OmpCatalog::new(config.omp.clone(), PathBuf::from(&home)));
        let sink = Arc::clone(&catalog);
        let actor_config = ActorConfig {
            hub: Arc::clone(&hub),
            factory,
            attachments: Arc::clone(&attachments),
            idle_timeout,
            coalesce: COALESCE,
            commands: Some(Arc::new(move |cwd, commands| {
                sink.put_commands(cwd, commands)
            })),
        };
        let inner = Arc::new(Inner {
            hub,
            attachments,
            catalog,
            omp: config.omp,
            actor_config,
            actors: Mutex::new(HashMap::new()),
            next_btw: AtomicU64::new(0),
            info,
            home,
            terminals: Terminals::new(),
            entries: Arc::default(),
        });
        inner.replay_pending().await?;
        inner.recover()?;
        Ok(Arc::new(App { inner }))
    }

    /// The `GET /api/v1/info` body.
    pub fn info(&self) -> Value {
        self.inner.info.clone()
    }

    /// Stable for the life of the database.
    pub fn server_id(&self) -> String {
        self.inner.info["serverId"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    pub fn attachments(&self) -> Arc<AttachmentStore> {
        Arc::clone(&self.inner.attachments)
    }

    /// One auto-settle sweep as of `now_ms` (epoch ms): settles every thread
    /// quiet for longer than `inactive_for` (`docs/triage.md` §8). Returns
    /// how many it settled.
    pub async fn auto_settle(&self, now_ms: i64, inactive_for: Duration) -> usize {
        let after_ms = i64::try_from(inactive_for.as_millis()).unwrap_or(i64::MAX);
        self.inner.auto_settle(now_ms, after_ms).await
    }

    /// Stops every terminal, then every thread actor and its omp process.
    pub async fn shutdown(&self) {
        self.inner.terminals.shutdown();
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

    pub(crate) fn inner(&self) -> &Inner {
        &self.inner
    }
}

/// Why a thread actor could not be reached.
enum ThreadError {
    NotFound,
    Store(store::Error),
}

impl ThreadError {
    fn into_error(self, thread_id: &str) -> Error {
        match self {
            Self::NotFound => Error::not_found("thread", thread_id),
            Self::Store(error) => storage(error),
        }
    }
}

impl Inner {
    // -----------------------------------------------------------------------
    // Startup
    // -----------------------------------------------------------------------

    /// Rebuilds threads whose records are missing (after migration 2) from
    /// their omp session files, keeping replayed images as attachments.
    async fn replay_pending(&self) -> Result<(), OpenError> {
        let pending = self.hub.read(|store| store.pending_replays())?;
        for index in pending {
            let lines = match &index.session_file {
                Some(path) => match tokio::fs::read_to_string(path).await {
                    Ok(text) => Some(text),
                    Err(error) => {
                        tracing::warn!(%error, path, thread = %index.id, "reading omp session file");
                        None
                    }
                },
                None => None,
            };
            let session_file = index.session_file.as_deref().map(Path::new);
            let image_bytes = |data: &str| match attachments::image_bytes(data, session_file) {
                Ok(bytes) => Some(bytes),
                Err(error) => {
                    tracing::warn!(%error, thread = %index.id, "reading a replayed image");
                    None
                }
            };
            let replayed = lines
                .as_deref()
                .map(|text| replay::replay(&index.id, text.lines(), &image_bytes))
                .unwrap_or_default();
            for (attachment, bytes) in &replayed.images {
                if let Err(error) = self
                    .attachments
                    .store_image(
                        &attachment.id,
                        &attachment.name,
                        &attachment.mime_type,
                        bytes,
                    )
                    .await
                {
                    tracing::warn!(%error, thread = %index.id, "storing a replayed image");
                }
            }
            let mut model = ThreadModel::load(
                &index.row,
                ThreadRecords {
                    turns: replayed.turns,
                    items: replayed.items,
                    ..ThreadRecords::default()
                },
            );
            if let Some(model_ref) = &replayed.model {
                model.set_model(model_ref);
            }
            model.rewrite_all();
            tracing::info!(
                thread = %index.id,
                turns = model.turns.len(),
                items = model.items.len(),
                "rebuilt from the omp session"
            );
            actor::commit_thread(
                &self.hub,
                &mut model,
                index.session_file.as_deref(),
                false,
                None,
            )
            .map_err(|error| OpenError::Recovery {
                thread: index.id.clone(),
                message: error.message,
            })?;
        }
        Ok(())
    }

    /// Turns left active by a previous process stop and open requests
    /// expire, through normal commits.
    fn recover(&self) -> Result<(), OpenError> {
        for index in self.hub.read(|store| store.threads())? {
            let records = self.hub.read(|store| store.records(&index.id))?;
            let mut model = ThreadModel::load(&index.row, records);
            if !model.needs_recovery() {
                continue;
            }
            model.recover();
            actor::commit_thread(
                &self.hub,
                &mut model,
                index.session_file.as_deref(),
                false,
                None,
            )
            .map_err(|error| OpenError::Recovery {
                thread: index.id.clone(),
                message: error.message,
            })?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Thread actors
    // -----------------------------------------------------------------------

    fn index(&self, thread_id: &str) -> Result<ThreadIndex, ThreadError> {
        self.hub
            .read(|store| store.thread(thread_id))
            .map_err(ThreadError::Store)?
            .filter(|index| !index.deleted)
            .ok_or(ThreadError::NotFound)
    }

    fn actor(&self, thread_id: &str) -> Result<ThreadHandle, ThreadError> {
        let mut actors = self.actors.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(handle) = actors.get(thread_id).filter(|handle| !handle.is_closed()) {
            return Ok(handle.clone());
        }
        let index = self.index(thread_id)?;
        let (records, project) = self
            .hub
            .read(|store| -> store::Result<_> {
                Ok((store.records(thread_id)?, store.project(&index.project_id)?))
            })
            .map_err(ThreadError::Store)?;
        let cwd = project.map_or_else(|| self.home.clone(), |project| project.path);
        let handle = actor::spawn(
            self.actor_config.clone(),
            ThreadModel::load(&index.row, records),
            cwd,
            index.session_file,
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
            self.forget(thread_id);
        }
        Err(ThreadError::NotFound)
    }

    fn forget(&self, thread_id: &str) {
        self.actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(thread_id);
    }

    fn running(&self, thread_id: &str) -> bool {
        self.actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(thread_id)
            .is_some_and(|handle| !handle.is_closed())
    }

    /// The auto-settle sweep (`docs/triage.md` §8): stored rows pick the
    /// threads that look due; each one's actor decides again on its live
    /// state and settles it only if it has not changed since the row the
    /// sweep saw, so user changes win.
    async fn auto_settle(&self, now: i64, after_ms: i64) -> usize {
        let threads = match self.hub.read(|store| store.threads()) {
            Ok(threads) => threads,
            Err(error) => {
                tracing::error!(%error, "reading threads to auto-settle");
                return 0;
            }
        };
        let mut settled = 0;
        for index in threads {
            let row = &index.row;
            if triage::auto_settle_at(&row.triage, &Facts::of(row), now, after_ms).is_none() {
                continue;
            }
            let seen = row.updated_at.clone();
            match self
                .ask(&index.id, |reply| Request::AutoSettle {
                    now_ms: now,
                    after_ms,
                    seen_updated_at: seen.clone(),
                    reply,
                })
                .await
            {
                Ok(true) => settled += 1,
                Ok(false) => tracing::debug!(thread = %index.id, "automatic settlement skipped"),
                Err(ThreadError::NotFound) => {}
                Err(ThreadError::Store(error)) => {
                    tracing::warn!(%error, thread = %index.id, "loading thread to auto-settle");
                }
            }
        }
        settled
    }

    // -----------------------------------------------------------------------
    // server
    // -----------------------------------------------------------------------

    /// `server.watch`: the catalog, then one per change; never ends.
    pub(crate) fn server_watch(&self) -> BoxStream<'static, StreamEvent> {
        let catalog = Arc::clone(&self.catalog);
        let updates = catalog.subscribe();
        let first = async move {
            let current = match catalog.catalog_now() {
                Some(current) => current,
                None => tokio::time::timeout(CATALOG_WAIT, catalog.catalog())
                    .await
                    .unwrap_or_else(|_| catalog.current()),
            };
            StreamEvent::Item(encode(&current))
        };
        let live = stream::unfold(updates, |mut updates| async move {
            loop {
                match updates.recv().await {
                    Ok(catalog) => return Some((StreamEvent::Item(encode(&catalog)), updates)),
                    Err(RecvError::Lagged(_)) => continue,
                    // The catalog lives as long as the app.
                    Err(RecvError::Closed) => std::future::pending::<()>().await,
                }
            }
        });
        stream::once(first).chain(live).boxed()
    }

    pub(crate) async fn server_refresh(
        &self,
        project_id: Option<&str>,
        fresh: bool,
    ) -> Result<Catalog, Error> {
        let cwd = match project_id {
            Some(project_id) => Some(self.project(project_id)?.path),
            None => None,
        };
        let (mut catalog, commands) = self.catalog.refresh(cwd.as_deref(), fresh).await;
        if let Some(project_id) = project_id {
            catalog.project_commands = BTreeMap::from([(project_id.to_owned(), commands)]);
        }
        Ok(catalog)
    }

    pub(crate) async fn usage(&self) -> Result<Usage, Error> {
        crate::usage::usage(&self.omp).await
    }

    // -----------------------------------------------------------------------
    // shell and threads
    // -----------------------------------------------------------------------

    pub(crate) fn shell_watch(&self) -> BoxStream<'static, StreamEvent> {
        streams::shell_stream(Arc::clone(&self.hub))
    }

    pub(crate) fn thread_watch(&self, thread_id: &str) -> BoxStream<'static, StreamEvent> {
        streams::thread_stream(Arc::clone(&self.hub), thread_id)
    }

    /// `threads.get`: what is committed once the actor has flushed.
    pub(crate) async fn thread_get(&self, thread_id: &str) -> Result<ThreadSnapshot, Error> {
        if self.running(thread_id) {
            let _ = self.ask(thread_id, |reply| Request::Flush { reply }).await;
        }
        self.hub
            .read(|store| store.snapshot(thread_id))
            .map_err(storage)?
            .ok_or_else(|| Error::not_found("thread", thread_id))
    }

    /// Uploaded attachments by id.
    pub(crate) async fn attachments(&self, ids: &[String]) -> Result<Vec<Attachment>, Error> {
        let mut attachments = Vec::with_capacity(ids.len());
        for id in ids {
            let attachment = self
                .attachments
                .attachment(id)
                .await
                .map_err(|error| Error::internal(error.to_string()))?
                .ok_or_else(|| Error::not_found("attachment", id))?;
            attachments.push(attachment);
        }
        Ok(attachments)
    }

    pub(crate) async fn create_thread(&self, params: CreateThreadParams) -> Result<Value, Error> {
        if let Some(command_id) = &params.command_id
            && let Some(thread_id) = self
                .hub
                .read(|store| store.receipt(command_id))
                .map_err(storage)?
                .and_then(|result| result.get("threadId")?.as_str().map(str::to_owned))
        {
            let row = self
                .index(&thread_id)
                .map_err(|error| error.into_error(&thread_id))?;
            return Ok(json!({"thread": row.row, "sequence": self.hub.high_water()}));
        }
        let project = self.project(&params.project_id)?;
        let attachments = self.attachments(&params.attachments).await?;
        if params.text.trim().is_empty() && attachments.is_empty() {
            return Err(Error::rejected("The message is empty."));
        }
        let thread_id = new_id();
        let model = ThreadModel::create(NewThread {
            id: thread_id.clone(),
            project_id: project.id,
            title: params.title.clone().unwrap_or_default(),
            model: params.model.clone(),
            mode: params.mode.unwrap_or_default(),
        });
        let handle = actor::spawn(self.actor_config.clone(), model, project.path, None);
        self.actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(thread_id.clone(), handle.clone());
        let (reply, response) = oneshot::channel();
        handle
            .send(Request::Launch {
                input: UserInput {
                    text: params.text,
                    attachments,
                    model: params.model,
                },
                generate_title: params.title.is_none(),
                command_id: params.command_id,
                reply,
            })
            .map_err(|_| Error::internal("The thread could not be started."))?;
        let (sequence, row) = response
            .await
            .map_err(|_| Error::internal("The thread could not be started."))??;
        Ok(json!({"thread": row, "sequence": sequence}))
    }

    /// A thread command (wire.md §5.4–§5.9); its result.
    pub(crate) async fn thread_command(
        &self,
        thread_id: &str,
        command_id: Option<String>,
        command: Command,
    ) -> Result<Value, Error> {
        if let Some(command_id) = &command_id
            && let Some(result) = self
                .hub
                .read(|store| store.receipt(command_id))
                .map_err(storage)?
        {
            return Ok(result);
        }
        let delete = matches!(command, Command::Delete { .. });
        let result = self
            .ask(thread_id, |reply| Request::Command {
                command: command.clone(),
                command_id: command_id.clone(),
                reply,
            })
            .await;
        match result {
            Ok(Ok(result)) => {
                if delete {
                    self.forget(thread_id);
                    self.terminals.close_thread(thread_id);
                }
                Ok(result)
            }
            Ok(Err(error)) => Err(error),
            // Deleting a missing thread succeeds.
            Err(ThreadError::NotFound) if delete => Ok(json!({"sequence": self.hub.high_water()})),
            Err(error) => Err(error.into_error(thread_id)),
        }
    }

    /// `turns.send`'s model: the client's, else the thread's.
    pub(crate) fn thread_model(&self, thread_id: &str) -> Result<ModelRef, Error> {
        self.index(thread_id)
            .map(|index| index.row.model)
            .map_err(|error| error.into_error(thread_id))
    }

    pub(crate) async fn transcript(
        &self,
        thread_id: &str,
        subagent_id: &str,
        from_byte: u64,
    ) -> Result<TranscriptResult, Error> {
        let index = self
            .index(thread_id)
            .map_err(|error| error.into_error(thread_id))?;
        let Some(session_file) = index.session_file else {
            return Err(Error::new(
                ErrorCode::Unavailable,
                "The thread has no omp session yet.",
            ));
        };
        let path = transcript::subagent_file(Path::new(&session_file), subagent_id)
            .ok_or_else(|| Error::not_found("subagent", subagent_id))?;
        blocking(move || {
            transcript::read(&path, from_byte).map_err(|error| {
                Error::new(
                    ErrorCode::Unavailable,
                    format!("The subagent transcript can't be read: {error}"),
                )
            })
        })
        .await
    }

    /// `btw.ask`: the topic as it starts, the answer's text as it grows,
    /// then the finished topic. Dropping the stream cancels the question.
    pub(crate) fn btw_ask(
        &self,
        params: BtwAskParams,
    ) -> Result<BoxStream<'static, StreamEvent>, Error> {
        /// Cancels the ask when its stream goes.
        struct Cancel {
            actor: ThreadHandle,
            ask_id: u64,
        }
        impl Drop for Cancel {
            fn drop(&mut self) {
                let _ = self.actor.send(Request::BtwCancel {
                    ask_id: self.ask_id,
                });
            }
        }
        let ask_id = self.next_btw.fetch_add(1, Ordering::Relaxed);
        for _ in 0..2 {
            let actor = self
                .actor(&params.thread_id)
                .map_err(|error| error.into_error(&params.thread_id))?;
            let (items, answers) = mpsc::unbounded_channel();
            let request = Request::Btw {
                ask_id,
                question: params.question.clone(),
                record_id: params.record_id.clone(),
                items,
            };
            if actor.send(request).is_err() {
                self.forget(&params.thread_id);
                continue;
            }
            let cancel = Cancel { actor, ask_id };
            return Ok(
                stream::unfold((answers, cancel), |(mut answers, cancel)| async move {
                    let event = match answers.recv().await? {
                        Ok(item) => StreamEvent::Item(encode(&item)),
                        Err(error) => StreamEvent::Fail(error),
                    };
                    Some((event, (answers, cancel)))
                })
                .boxed(),
            );
        }
        Err(Error::not_found("thread", &params.thread_id))
    }

    // -----------------------------------------------------------------------
    // projects
    // -----------------------------------------------------------------------

    pub(crate) fn project(&self, project_id: &str) -> Result<Project, Error> {
        self.hub
            .read(|store| store.project(project_id))
            .map_err(storage)?
            .ok_or_else(|| Error::not_found("project", project_id))
    }

    /// The project directory of a thread.
    fn thread_root(&self, thread_id: &str) -> Result<String, Error> {
        let index = self
            .index(thread_id)
            .map_err(|error| error.into_error(thread_id))?;
        Ok(self.project(&index.project_id)?.path)
    }

    /// The stored result of command `command_id`, if it ran before.
    fn receipt(&self, command_id: Option<&str>) -> Result<Option<Value>, Error> {
        match command_id {
            Some(command_id) => self
                .hub
                .read(|store| store.receipt(command_id))
                .map_err(storage),
            None => Ok(None),
        }
    }

    /// Commits project changes; the command's result, kept for repeats of
    /// `command_id`.
    fn commit_projects(
        &self,
        projects: Vec<ProjectWrite>,
        command_id: Option<String>,
        result: impl Fn(u64) -> Value + Clone + Send + 'static,
    ) -> Result<Value, Error> {
        let stored = command_id.map(|id| {
            let result = result.clone();
            let stored: ResultFn = Box::new(result);
            (id, stored)
        });
        let sequence = self
            .hub
            .commit(Commit {
                thread: None,
                projects,
                result: stored,
            })
            .map_err(storage)?;
        Ok(result(sequence))
    }

    pub(crate) fn create_project(&self, params: CreateProjectParams) -> Result<Value, Error> {
        if let Some(result) = self.receipt(params.command_id.as_deref())? {
            return Ok(result);
        }
        let root = expand_home(params.path.trim(), &self.home);
        if !root.is_dir() {
            return Err(Error::rejected(
                "That folder doesn't exist on this machine.",
            ));
        }
        let path = root.to_string_lossy().into_owned();
        let projects = self.hub.read(|store| store.projects()).map_err(storage)?;
        if projects.iter().any(|project| project.path == path) {
            return Err(Error::rejected("That folder is already a project."));
        }
        let title = match params.title.trim() {
            "" => root.file_name().map_or_else(
                || "Project".to_owned(),
                |name| name.to_string_lossy().into_owned(),
            ),
            title => title.to_owned(),
        };
        let now = now_iso();
        let project = Project {
            id: new_id(),
            title,
            path,
            default_model: params.default_model,
            created_at: now.clone(),
            updated_at: now,
        };
        let value = encode(&project);
        self.commit_projects(
            vec![ProjectWrite::Upsert(project)],
            params.command_id,
            move |sequence| json!({"project": value, "sequence": sequence}),
        )
    }

    pub(crate) fn update_project(&self, params: UpdateProjectParams) -> Result<Value, Error> {
        if let Some(result) = self.receipt(params.command_id.as_deref())? {
            return Ok(result);
        }
        let mut project = self.project(&params.project_id)?;
        if let Some(title) = &params.title {
            let title = title.trim();
            if title.is_empty() {
                return Err(Error::rejected("The title can't be empty."));
            }
            title.clone_into(&mut project.title);
        }
        if let Some(model) = params.default_model {
            project.default_model = model;
        }
        project.updated_at = now_iso();
        let value = encode(&project);
        self.commit_projects(
            vec![ProjectWrite::Upsert(project)],
            params.command_id,
            move |sequence| json!({"project": value, "sequence": sequence}),
        )
    }

    pub(crate) async fn delete_project(&self, params: DeleteProjectParams) -> Result<Value, Error> {
        if let Some(result) = self.receipt(params.command_id.as_deref())? {
            return Ok(result);
        }
        self.project(&params.project_id)?;
        let threads: Vec<String> = self
            .hub
            .read(|store| store.threads())
            .map_err(storage)?
            .into_iter()
            .filter(|index| index.project_id == params.project_id)
            .map(|index| index.id)
            .collect();
        if !threads.is_empty() && params.force != Some(true) {
            return Err(Error::rejected("Delete this project's threads first."));
        }
        for thread_id in threads {
            self.thread_command(
                &thread_id,
                None,
                Command::Delete {
                    only_if_unused: false,
                },
            )
            .await?;
        }
        self.commit_projects(
            vec![ProjectWrite::Remove(params.project_id)],
            params.command_id,
            |sequence| json!({"sequence": sequence}),
        )
    }

    pub(crate) async fn browse(&self, partial_path: String) -> Result<BrowseResult, Error> {
        let home = self.home.clone();
        blocking(move || files::browse(&home, &partial_path)).await
    }

    pub(crate) async fn list_entries(
        &self,
        project_id: &str,
        dir: Option<String>,
    ) -> Result<EntriesResult, Error> {
        let root = self.project(project_id)?.path;
        blocking(move || files::list_entries(&root, dir.as_deref())).await
    }

    pub(crate) async fn search_entries(
        &self,
        params: SearchEntriesParams,
    ) -> Result<EntriesResult, Error> {
        let root = self.project(&params.project_id)?.path;
        let entries = Arc::clone(&self.entries);
        blocking(move || entries.search(&root, &params)).await
    }

    pub(crate) async fn read_file(
        &self,
        project_id: &str,
        path: String,
        allow_missing: bool,
    ) -> Result<ReadFileResult, Error> {
        let root = self.project(project_id)?.path;
        blocking(move || files::read_file(&root, &path, allow_missing)).await
    }

    pub(crate) async fn image_url(
        &self,
        project_id: &str,
        path: String,
    ) -> Result<ImageUrlResult, Error> {
        let root = self.project(project_id)?.path;
        let requested = path.clone();
        let found = blocking(move || {
            files::image_file(&root, &requested).map_err(|error| match error {
                files::ImageFileError::OutsideRoot => {
                    Error::rejected("The path is outside the project.")
                }
                files::ImageFileError::NotImage => Error::new(
                    ErrorCode::TooLarge,
                    "Only GIF, JPEG, PNG, WebP and SVG images up to 10 MiB can be shown.",
                )
                .with_data(json!({"limit": crate::attachments::MAX_IMAGE_BYTES})),
                files::ImageFileError::NotFound(_) => Error::not_found("file", &requested),
            })
        })
        .await?;
        let (file, mime_type) = found;
        let (token, expires_at) = self.attachments.grant_path(file, mime_type);
        Ok(ImageUrlResult {
            url: format!("/api/v1/assets/{token}"),
            expires_at: iso(expires_at as i64),
        })
    }

    // -----------------------------------------------------------------------
    // git
    // -----------------------------------------------------------------------

    pub(crate) async fn git_status(&self, project_id: &str) -> Result<GitStatus, Error> {
        git::status(&self.project(project_id)?.path).await
    }

    pub(crate) async fn git_pull(&self, project_id: &str) -> Result<PullResult, Error> {
        git::pull(&self.project(project_id)?.path).await
    }

    pub(crate) fn git_run(
        &self,
        project_id: &str,
        action: GitAction,
        message: Option<String>,
    ) -> Result<BoxStream<'static, Result<GitRunItem, Error>>, Error> {
        let cwd = self.project(project_id)?.path;
        Ok(git::run(self.omp.clone(), cwd, action, message))
    }

    // -----------------------------------------------------------------------
    // terminals and attachments
    // -----------------------------------------------------------------------

    pub(crate) fn terminal_attach(
        &self,
        params: &j3_protocol::v2::methods::AttachParams,
    ) -> Result<BoxStream<'static, StreamEvent>, Error> {
        let root = self.thread_root(&params.thread_id)?;
        let items = self.terminals.attach(params, &root)?;
        Ok(items.map(|item| StreamEvent::Item(encode(&item))).boxed())
    }

    pub(crate) fn terminals(&self) -> &Terminals {
        &self.terminals
    }

    pub(crate) async fn delete_attachment(&self, attachment_id: &str) -> Result<(), Error> {
        self.attachments
            .delete(attachment_id)
            .await
            .map_err(|error| Error::internal(error.to_string()))
    }

    /// Declares an upload of `size` bytes; its new id.
    pub(crate) async fn create_attachment(
        &self,
        image: bool,
        name: &str,
        mime_type: &str,
        size: u64,
    ) -> Result<String, Error> {
        let kind = if image {
            AttachmentKind::Image
        } else {
            AttachmentKind::File
        };
        self.attachments
            .create(kind, name, mime_type, size)
            .await
            .map_err(|error| Error::internal(error.to_string()))
    }
}
