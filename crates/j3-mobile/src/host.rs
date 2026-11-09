//! `Host`: one j3 server, its live state pushed to Swift listeners, and the
//! commands of the iOS app.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use j3_client::{
    Connection,
    entities::{Answer, Attachment, ModelRef, Project, ThreadMode},
    methods::{
        AnswerParams, BrowseParams, BrowseResult, CreateGoalParams, CreateProjectParams,
        CreateThreadParams, DeleteThreadParams, DismissParams, EntriesResult, EntryKind,
        QueueEditParams, QueueEntryParams, RefreshParams, RenameThreadParams, SearchEntriesParams,
        SendParams, SetModeParams, StopParams, ThreadParams, TranscriptParams,
    },
};
use tokio::{sync::watch, task::AbortHandle};

use crate::{
    ClientError, RUNTIME,
    types::{
        Catalog, HostSnapshot, SendDelivery, SendOutcome, ThreadRow, ThreadSnapshot, Transcript,
    },
};

/// Receives a `Host`'s state. Called on a library runtime thread, never the
/// main thread: hop to the main actor before touching UI state, and return
/// quickly, since a slow listener holds up one of the runtime's two workers.
#[uniffi::export(with_foreign)]
pub trait HostListener: Send + Sync {
    /// The whole state, at once and after every change. Changes that
    /// land while a call is in progress coalesce into the next one.
    fn host_changed(&self, snapshot: HostSnapshot);
}

/// Receives one thread's state; called like `HostListener`.
#[uniffi::export(with_foreign)]
pub trait ThreadListener: Send + Sync {
    /// The whole thread, at once and after every change. Changes that
    /// land while a call is in progress coalesce into the next one.
    fn thread_changed(&self, snapshot: ThreadSnapshot);
}

/// A running `Host::watch` or `Host::watch_thread`. Calls stop on
/// `cancel()` or when the object is released; a call already under way
/// finishes. A thread subscription also ends the server-side
/// `threads.watch` once no other subscription of that thread is alive.
#[derive(uniffi::Object)]
pub struct Subscription {
    cancelled: Arc<AtomicBool>,
    task: AbortHandle,
}

#[uniffi::export]
impl Subscription {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.task.abort();
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Calls `deliver` with the current state of `rx` and after each change,
/// on the library runtime, until cancelled or the sender is gone. `keep`
/// lives as long as the task.
fn subscribe<S: Send + Sync + 'static>(
    mut rx: watch::Receiver<Arc<S>>,
    keep: impl Send + 'static,
    deliver: impl Fn(&S) + Send + 'static,
) -> Arc<Subscription> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancelled);
    let task = RUNTIME.spawn(async move {
        let _keep = keep;
        loop {
            let state = Arc::clone(&rx.borrow_and_update());
            if flag.load(Ordering::Acquire) {
                return;
            }
            deliver(&state);
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
    Arc::new(Subscription {
        cancelled,
        task: task.abort_handle(),
    })
}

/// One j3 server. Connects at once and reconnects with backoff until
/// `close()` or until Swift releases it (and every thread subscription).
#[derive(uniffi::Object)]
pub struct Host {
    connection: Connection,
}

#[uniffi::export]
impl Host {
    /// `base_url` is the server's HTTP base, e.g. `http://shieldy:3780`.
    #[uniffi::constructor]
    pub fn new(base_url: String) -> Arc<Self> {
        Arc::new(Self {
            connection: Connection::open(base_url, RUNTIME.handle().clone()),
        })
    }

    /// Stops reconnecting and closes the socket. Later commands fail with
    /// `ClientError.Disconnected`; listeners get a last `Closed` snapshot.
    pub async fn close(&self) {
        self.connection.close().await;
    }

    /// Pushes the host's state to `listener` now and after every change.
    pub fn watch(&self, listener: Arc<dyn HostListener>) -> Arc<Subscription> {
        subscribe(self.connection.state(), (), move |state| {
            listener.host_changed(state.into())
        })
    }

    /// Subscribes to `thread_id` and pushes its state to `listener` now and
    /// after every change. The subscription resumes after reconnects.
    pub fn watch_thread(
        &self,
        thread_id: String,
        listener: Arc<dyn ThreadListener>,
    ) -> Arc<Subscription> {
        let watch = self.connection.thread(&thread_id);
        subscribe(watch.state(), watch, move |view| {
            listener.thread_changed(view.into())
        })
    }

    /// The absolute URL serving an attachment's bytes (user images, tool
    /// images).
    pub fn attachment_url(&self, attachment_id: String) -> String {
        self.connection.attachment_url(&attachment_id)
    }

    /// An absolute server path (e.g. a `/api/v1/assets/…` URL) as an
    /// absolute URL.
    pub fn url(&self, path: String) -> String {
        self.connection.url(&path)
    }

    /// The catalog again, with `project_id`'s slash commands in
    /// `project_commands`; `fresh` asks omp instead of the server's cache.
    pub async fn refresh_catalog(
        &self,
        project_id: Option<String>,
        fresh: bool,
    ) -> Result<Catalog, ClientError> {
        let catalog = self
            .connection
            .server_refresh(RefreshParams {
                project_id,
                fresh: fresh.then_some(true),
            })
            .await?;
        Ok((&catalog).into())
    }

    // ---- projects ----

    /// Adds the server directory `path` as a project.
    pub async fn create_project(
        &self,
        title: String,
        path: String,
        default_model: Option<ModelRef>,
    ) -> Result<Project, ClientError> {
        let created = self
            .connection
            .projects_create(CreateProjectParams {
                title,
                path,
                default_model,
                command_id: None,
            })
            .await?;
        Ok(created.project)
    }

    /// Directories under the server user's home matching `partial_path`,
    /// for the add-project flow.
    pub async fn browse_directories(
        &self,
        partial_path: String,
    ) -> Result<BrowseResult, ClientError> {
        Ok(self
            .connection
            .projects_browse(BrowseParams { partial_path })
            .await?)
    }

    /// Files and directories of a project matching `query`, for `@path`
    /// completion. `limit` is at most 200.
    pub async fn search_entries(
        &self,
        project_id: String,
        query: String,
        limit: Option<u32>,
        kind: Option<EntryKind>,
        images_only: bool,
    ) -> Result<EntriesResult, ClientError> {
        Ok(self
            .connection
            .projects_search_entries(SearchEntriesParams {
                project_id,
                query,
                limit,
                kind,
                images_only: images_only.then_some(true),
            })
            .await?)
    }

    // ---- threads ----

    /// Creates a thread whose first turn starts at once with `text`.
    /// `attachment_ids` come from `upload_attachment`.
    pub async fn create_thread(
        &self,
        project_id: String,
        model: ModelRef,
        mode: Option<ThreadMode>,
        title: Option<String>,
        text: String,
        attachment_ids: Vec<String>,
    ) -> Result<ThreadRow, ClientError> {
        let created = self
            .connection
            .threads_create(CreateThreadParams {
                project_id,
                model,
                mode,
                title,
                text,
                attachments: attachment_ids,
                command_id: None,
            })
            .await?;
        Ok((&*created.thread).into())
    }

    pub async fn rename_thread(
        &self,
        thread_id: String,
        title: String,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .threads_rename(RenameThreadParams {
                thread_id,
                title,
                command_id: None,
            })
            .await?;
        Ok(done.sequence)
    }

    pub async fn archive_thread(&self, thread_id: String) -> Result<u64, ClientError> {
        let done = self.connection.threads_archive(thread(thread_id)).await?;
        Ok(done.sequence)
    }

    pub async fn unarchive_thread(&self, thread_id: String) -> Result<u64, ClientError> {
        let done = self.connection.threads_unarchive(thread(thread_id)).await?;
        Ok(done.sequence)
    }

    /// `only_if_unused` deletes only a thread nothing was sent to.
    pub async fn delete_thread(
        &self,
        thread_id: String,
        only_if_unused: bool,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .threads_delete(DeleteThreadParams {
                thread_id,
                only_if_unused: only_if_unused.then_some(true),
                command_id: None,
            })
            .await?;
        Ok(done.sequence)
    }

    pub async fn set_mode(&self, thread_id: String, mode: ThreadMode) -> Result<u64, ClientError> {
        let done = self
            .connection
            .threads_set_mode(SetModeParams {
                thread_id,
                mode,
                command_id: None,
            })
            .await?;
        Ok(done.sequence)
    }

    // ---- turns ----

    /// Sends a prompt; `delivery` says what happens when a turn is active.
    /// `model` switches the thread's model first.
    pub async fn send(
        &self,
        thread_id: String,
        text: String,
        attachment_ids: Vec<String>,
        model: Option<ModelRef>,
        delivery: SendDelivery,
    ) -> Result<SendOutcome, ClientError> {
        let sent = self
            .connection
            .turns_send(SendParams {
                thread_id,
                text,
                attachments: attachment_ids,
                model,
                delivery: delivery.into(),
                command_id: None,
            })
            .await?;
        Ok(sent.into())
    }

    /// Stops the active turn. `hold_queue` keeps queued prompts from
    /// starting; `force` stops a turn that ignores the first stop.
    pub async fn stop(
        &self,
        thread_id: String,
        hold_queue: bool,
        force: bool,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .turns_stop(StopParams {
                thread_id,
                hold_queue: hold_queue.then_some(true),
                force: force.then_some(true),
                command_id: None,
            })
            .await?;
        Ok(done.sequence)
    }

    // ---- requests ----

    pub async fn answer_request(
        &self,
        thread_id: String,
        request_id: String,
        answers: Vec<Answer>,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .requests_answer(AnswerParams {
                thread_id,
                request_id,
                answers,
                command_id: None,
            })
            .await?;
        Ok(done.sequence)
    }

    pub async fn dismiss_request(
        &self,
        thread_id: String,
        request_id: String,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .requests_dismiss(DismissParams {
                thread_id,
                request_id,
                command_id: None,
            })
            .await?;
        Ok(done.sequence)
    }

    // ---- queue ----

    /// Replaces a queued prompt's text; `attachment_ids` `None` keeps its
    /// attachments.
    pub async fn edit_queued(
        &self,
        thread_id: String,
        entry_id: String,
        text: String,
        attachment_ids: Option<Vec<String>>,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .queue_edit(QueueEditParams {
                thread_id,
                entry_id,
                text,
                attachments: attachment_ids,
                command_id: None,
            })
            .await?;
        Ok(done.sequence)
    }

    pub async fn cancel_queued(
        &self,
        thread_id: String,
        entry_id: String,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .queue_cancel(entry(thread_id, entry_id))
            .await?;
        Ok(done.sequence)
    }

    /// Sends a queued prompt into the active turn now.
    pub async fn steer_queued(
        &self,
        thread_id: String,
        entry_id: String,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .queue_steer(entry(thread_id, entry_id))
            .await?;
        Ok(done.sequence)
    }

    /// Stops the active turn and starts a queued prompt in its place.
    pub async fn start_queued(
        &self,
        thread_id: String,
        entry_id: String,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .queue_start(entry(thread_id, entry_id))
            .await?;
        Ok(done.sequence)
    }

    /// Releases a queue held by `stop(hold_queue: true)`.
    pub async fn resume_queue(&self, thread_id: String) -> Result<u64, ClientError> {
        let done = self.connection.queue_resume(thread(thread_id)).await?;
        Ok(done.sequence)
    }

    // ---- goals ----

    /// Starts omp goal mode; `budget` is in tokens.
    pub async fn create_goal(
        &self,
        thread_id: String,
        objective: String,
        budget: Option<u64>,
    ) -> Result<u64, ClientError> {
        let done = self
            .connection
            .goals_create(CreateGoalParams {
                thread_id,
                objective,
                budget,
                command_id: None,
            })
            .await?;
        Ok(done.sequence)
    }

    pub async fn pause_goal(&self, thread_id: String) -> Result<u64, ClientError> {
        let done = self.connection.goals_pause(thread(thread_id)).await?;
        Ok(done.sequence)
    }

    pub async fn resume_goal(&self, thread_id: String) -> Result<u64, ClientError> {
        let done = self.connection.goals_resume(thread(thread_id)).await?;
        Ok(done.sequence)
    }

    pub async fn drop_goal(&self, thread_id: String) -> Result<u64, ClientError> {
        let done = self.connection.goals_drop(thread(thread_id)).await?;
        Ok(done.sequence)
    }

    // ---- subagents and attachments ----

    /// A subagent's transcript from byte `from_byte`; pass the result's
    /// `next_byte` to continue.
    pub async fn subagent_transcript(
        &self,
        thread_id: String,
        subagent_id: String,
        from_byte: u64,
    ) -> Result<Transcript, ClientError> {
        let page = self
            .connection
            .subagents_transcript(TranscriptParams {
                thread_id,
                subagent_id,
                from_byte,
            })
            .await?;
        Ok(page.into())
    }

    /// Uploads a file (an image from the photo picker or the pasteboard)
    /// for `create_thread`, `send` and `edit_queued`.
    pub async fn upload_attachment(
        &self,
        name: String,
        mime_type: String,
        bytes: Vec<u8>,
    ) -> Result<Attachment, ClientError> {
        Ok(self.connection.upload(&name, &mime_type, bytes).await?)
    }
}

fn thread(thread_id: String) -> ThreadParams {
    ThreadParams {
        thread_id,
        command_id: None,
    }
}

fn entry(thread_id: String, entry_id: String) -> QueueEntryParams {
    QueueEntryParams {
        thread_id,
        entry_id,
        command_id: None,
    }
}
