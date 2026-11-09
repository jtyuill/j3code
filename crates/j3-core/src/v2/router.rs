//! v2 method router (`docs/protocol/wire.md` §5): decodes each call's params
//! and answers it from the [`App`].

use std::sync::Arc;

use futures_util::{StreamExt, future::BoxFuture};
use j3_protocol::v2::{
    envelope::{Error, ErrorCode, INITIAL_WINDOW},
    methods::{self as m},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use super::driver::{Handler, Reply, StreamEvent};
use crate::{
    app::App,
    engine::{actor::Command, model::UserInput},
};

/// Stream window of `terminal.attach` (wire.md §5.11).
const TERMINAL_WINDOW: u32 = 64;

/// The v2 [`Handler`] of an [`App`].
pub struct Router {
    app: Arc<App>,
}

impl Router {
    pub fn new(app: Arc<App>) -> Self {
        Self { app }
    }
}

impl Handler for Router {
    fn call(&self, method: String, params: Value) -> BoxFuture<'static, Reply> {
        let app = Arc::clone(&self.app);
        Box::pin(async move { call(&app, &method, params).await })
    }
}

fn decode<T: DeserializeOwned>(params: Value) -> Result<T, Error> {
    serde_json::from_value(params).map_err(|error| Error::bad_request(error.to_string()))
}

fn encode<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("v2 types always encode")
}

fn stream(window: u32, items: futures_util::stream::BoxStream<'static, StreamEvent>) -> Reply {
    Reply::Stream { window, items }
}

async fn call(app: &App, method: &str, params: Value) -> Reply {
    let inner = app.inner();
    match method {
        m::SERVER_WATCH => stream(INITIAL_WINDOW, inner.server_watch()),
        m::SHELL_WATCH => match decode::<m::WatchParams>(params) {
            Ok(_) => stream(INITIAL_WINDOW, inner.shell_watch()),
            Err(error) => Reply::Err(error),
        },
        m::THREADS_WATCH => match decode::<m::WatchThreadParams>(params) {
            Ok(params) => stream(INITIAL_WINDOW, inner.thread_watch(&params.thread_id)),
            Err(error) => Reply::Err(error),
        },
        m::GIT_RUN => {
            let run = decode::<m::GitRunParams>(params).and_then(|params| {
                inner.git_run(&params.project_id, params.action, params.message)
            });
            match run {
                Ok(items) => stream(
                    INITIAL_WINDOW,
                    items
                        .map(|item| match item {
                            Ok(item) => StreamEvent::Item(encode(&item)),
                            Err(error) => StreamEvent::Fail(error),
                        })
                        .boxed(),
                ),
                Err(error) => Reply::Err(error),
            }
        }
        m::TERMINAL_ATTACH => {
            match decode::<m::AttachParams>(params)
                .and_then(|params| inner.terminal_attach(&params))
            {
                Ok(items) => stream(TERMINAL_WINDOW, items),
                Err(error) => Reply::Err(error),
            }
        }
        method if m::METHODS.contains(&method) => match unary(app, method, params).await {
            Ok(value) => Reply::Ok(value),
            Err(error) => Reply::Err(error),
        },
        method => Reply::Err(Error::new(
            ErrorCode::UnknownMethod,
            format!("{method} is not a j3 method."),
        )),
    }
}

/// A thread command with only a thread id.
async fn simple(app: &App, params: Value, command: Command) -> Result<Value, Error> {
    let params: m::ThreadParams = decode(params)?;
    app.inner()
        .thread_command(&params.thread_id, params.command_id, command)
        .await
}

async fn unary(app: &App, method: &str, params: Value) -> Result<Value, Error> {
    let inner = app.inner();
    match method {
        m::SERVER_REFRESH => {
            let params: m::RefreshParams = decode(params)?;
            let catalog = inner
                .server_refresh(params.project_id.as_deref(), params.fresh == Some(true))
                .await?;
            Ok(encode(&catalog))
        }
        m::SERVER_USAGE => Ok(encode(&inner.usage().await?)),
        m::PROJECTS_CREATE => inner.create_project(decode(params)?),
        m::PROJECTS_UPDATE => inner.update_project(decode(params)?),
        m::PROJECTS_DELETE => inner.delete_project(decode(params)?).await,
        m::PROJECTS_BROWSE => {
            let params: m::BrowseParams = decode(params)?;
            Ok(encode(&inner.browse(params.partial_path).await?))
        }
        m::PROJECTS_LIST_ENTRIES => {
            let params: m::ListEntriesParams = decode(params)?;
            Ok(encode(
                &inner.list_entries(&params.project_id, params.dir).await?,
            ))
        }
        m::PROJECTS_SEARCH_ENTRIES => Ok(encode(&inner.search_entries(decode(params)?).await?)),
        m::PROJECTS_READ_FILE => {
            let params: m::ReadFileParams = decode(params)?;
            let file = inner
                .read_file(
                    &params.project_id,
                    params.path,
                    params.allow_missing == Some(true),
                )
                .await?;
            Ok(encode(&file))
        }
        m::PROJECTS_IMAGE_URL => {
            let params: m::ImageUrlParams = decode(params)?;
            Ok(encode(
                &inner.image_url(&params.project_id, params.path).await?,
            ))
        }
        m::THREADS_CREATE => inner.create_thread(decode(params)?).await,
        m::THREADS_GET => {
            let params: m::ThreadParams = decode(params)?;
            Ok(encode(&inner.thread_get(&params.thread_id).await?))
        }
        m::THREADS_RENAME => {
            let params: m::RenameThreadParams = decode(params)?;
            inner
                .thread_command(
                    &params.thread_id,
                    params.command_id,
                    Command::Rename(params.title),
                )
                .await
        }
        m::THREADS_ARCHIVE => simple(app, params, Command::Archive).await,
        m::THREADS_UNARCHIVE => simple(app, params, Command::Unarchive).await,
        m::THREADS_DELETE => {
            let params: m::DeleteThreadParams = decode(params)?;
            let command = Command::Delete {
                only_if_unused: params.only_if_unused == Some(true),
            };
            inner
                .thread_command(&params.thread_id, params.command_id, command)
                .await
        }
        m::THREADS_SET_MODE => {
            let params: m::SetModeParams = decode(params)?;
            inner
                .thread_command(
                    &params.thread_id,
                    params.command_id,
                    Command::SetMode(params.mode),
                )
                .await
        }
        m::TURNS_SEND => {
            let params: m::SendParams = decode(params)?;
            let model = match params.model {
                Some(model) => model,
                None => inner.thread_model(&params.thread_id)?,
            };
            let input = UserInput {
                text: params.text,
                attachments: inner.attachments(&params.attachments).await?,
                model,
            };
            let command = Command::Send {
                input,
                delivery: params.delivery,
            };
            inner
                .thread_command(&params.thread_id, params.command_id, command)
                .await
        }
        m::TURNS_STOP => {
            let params: m::StopParams = decode(params)?;
            let command = Command::Stop {
                hold_queue: params.hold_queue == Some(true),
                force: params.force == Some(true),
            };
            inner
                .thread_command(&params.thread_id, params.command_id, command)
                .await
        }
        m::REQUESTS_ANSWER => {
            let params: m::AnswerParams = decode(params)?;
            let command = Command::Answer {
                request_id: params.request_id,
                answers: params.answers,
            };
            inner
                .thread_command(&params.thread_id, params.command_id, command)
                .await
        }
        m::REQUESTS_DISMISS => {
            let params: m::DismissParams = decode(params)?;
            let command = Command::Dismiss {
                request_id: params.request_id,
            };
            inner
                .thread_command(&params.thread_id, params.command_id, command)
                .await
        }
        m::QUEUE_EDIT => {
            let params: m::QueueEditParams = decode(params)?;
            let attachments = match &params.attachments {
                Some(ids) => Some(inner.attachments(ids).await?),
                None => None,
            };
            let command = Command::QueueEdit {
                entry_id: params.entry_id,
                text: params.text,
                attachments,
            };
            inner
                .thread_command(&params.thread_id, params.command_id, command)
                .await
        }
        m::QUEUE_CANCEL | m::QUEUE_STEER | m::QUEUE_START => {
            let params: m::QueueEntryParams = decode(params)?;
            let command = match method {
                m::QUEUE_CANCEL => Command::QueueCancel(params.entry_id),
                m::QUEUE_STEER => Command::QueueSteer(params.entry_id),
                _ => Command::QueueStart(params.entry_id),
            };
            inner
                .thread_command(&params.thread_id, params.command_id, command)
                .await
        }
        m::QUEUE_RESUME => simple(app, params, Command::QueueResume).await,
        m::TRIAGE_VISIT => {
            let params: m::VisitParams = decode(params)?;
            inner
                .thread_command(
                    &params.thread_id,
                    params.command_id,
                    Command::Visit(params.visited_at),
                )
                .await
        }
        m::TRIAGE_MARK_UNREAD => simple(app, params, Command::MarkUnread).await,
        m::TRIAGE_SETTLE => {
            let params: m::SettleParams = decode(params)?;
            inner
                .thread_command(
                    &params.thread_id,
                    params.command_id,
                    Command::Settle(params.settled_at),
                )
                .await
        }
        m::TRIAGE_UNSETTLE => simple(app, params, Command::Unsettle).await,
        m::TRIAGE_SNOOZE => {
            let params: m::SnoozeParams = decode(params)?;
            inner
                .thread_command(
                    &params.thread_id,
                    params.command_id,
                    Command::Snooze(params.until),
                )
                .await
        }
        m::TRIAGE_WAKE => simple(app, params, Command::Wake).await,
        m::TRIAGE_PIN => {
            let params: m::PinParams = decode(params)?;
            inner
                .thread_command(
                    &params.thread_id,
                    params.command_id,
                    Command::Pin(params.order),
                )
                .await
        }
        m::TRIAGE_UNPIN => simple(app, params, Command::Unpin).await,
        m::TRIAGE_ORDER_PINNED | m::TRIAGE_ORDER_ACTIVE => {
            let params: m::OrderParams = decode(params)?;
            let command = if method == m::TRIAGE_ORDER_PINNED {
                Command::OrderPinned(params.order)
            } else {
                Command::OrderActive(params.order)
            };
            inner
                .thread_command(&params.thread_id, params.command_id, command)
                .await
        }
        m::TRIAGE_SET_AUTO_SETTLE => {
            let params: m::AutoSettleParams = decode(params)?;
            inner
                .thread_command(
                    &params.thread_id,
                    params.command_id,
                    Command::SetAutoSettle(params.enabled),
                )
                .await
        }
        m::GOALS_CREATE => {
            let params: m::CreateGoalParams = decode(params)?;
            let command = Command::GoalCreate {
                objective: params.objective,
                budget: params.budget,
            };
            inner
                .thread_command(&params.thread_id, params.command_id, command)
                .await
        }
        m::GOALS_PAUSE => simple(app, params, Command::GoalPause).await,
        m::GOALS_RESUME => simple(app, params, Command::GoalResume).await,
        m::GOALS_DROP => simple(app, params, Command::GoalDrop).await,
        m::SUBAGENTS_STOP => {
            let params: m::StopSubagentParams = decode(params)?;
            inner
                .thread_command(
                    &params.thread_id,
                    params.command_id,
                    Command::SubagentStop(params.subagent_id),
                )
                .await
        }
        m::SUBAGENTS_TRANSCRIPT => {
            let params: m::TranscriptParams = decode(params)?;
            let transcript = inner
                .transcript(&params.thread_id, &params.subagent_id, params.from_byte)
                .await?;
            Ok(encode(&transcript))
        }
        m::GIT_STATUS => {
            let params: m::ProjectParams = decode(params)?;
            Ok(encode(&inner.git_status(&params.project_id).await?))
        }
        m::GIT_PULL => {
            let params: m::ProjectParams = decode(params)?;
            Ok(encode(&inner.git_pull(&params.project_id).await?))
        }
        m::TERMINAL_WRITE => {
            inner.terminals().write(decode(params)?).await?;
            Ok(json!({}))
        }
        m::TERMINAL_RESIZE => {
            inner.terminals().resize(&decode(params)?)?;
            Ok(json!({}))
        }
        m::TERMINAL_CLOSE => {
            inner.terminals().close(&decode(params)?);
            Ok(json!({}))
        }
        m::ATTACHMENTS_DELETE => {
            let params: m::DeleteAttachmentParams = decode(params)?;
            inner.delete_attachment(&params.attachment_id).await?;
            Ok(json!({}))
        }
        method => Err(Error::internal(format!("{method} has no route"))),
    }
}
