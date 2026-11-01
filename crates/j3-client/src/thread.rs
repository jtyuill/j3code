//! Live thread state: the t3 thread projection kept current by
//! `orchestration.subscribeThread` (like t3's client-runtime
//! `state/threads.ts`), plus the helpers t3 derives from it
//! (`threadWorkflows.ts`, `threadRequests.ts`).

use j3_protocol::t3::{
    chat_attachment::ChatAttachment,
    orchestration::{
        ConversationMessage, DomainEventBody, MessageRole, PlanArtifactKind, PlanArtifactStatus,
        PlanStep, ProjectedTurnItem, ProviderGoal, ProviderSession, ProviderSessionStatus,
        ProviderTurnStatus, ResponseCapability, Run, RunStatus, RuntimeRequest, RuntimeRequestKind,
        RuntimeRequestStatus, Subagent, ThreadProjection, ThreadStreamItem, TurnItem, TurnItemKind,
        UserInputQuestion, UserInputResponseMode,
    },
};

use crate::projection::apply_event;

/// One thread as the client sees it.
#[derive(Debug, Clone)]
pub struct ThreadState {
    pub thread_id: String,
    pub sync: ThreadSync,
    /// Sequence of the last snapshot or event applied.
    pub sequence: u64,
    /// `None` until the first snapshot arrives, and after the thread is deleted.
    pub projection: Option<ThreadProjection>,
}

/// Subscription state of a [`ThreadState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadSync {
    /// Nothing received yet.
    Loading,
    /// A (re)subscription is catching up; the projection may be stale.
    Synchronizing,
    /// Up to date and receiving live events.
    Live,
    /// The thread was deleted (or never existed).
    Deleted,
    /// The subscription failed; it is retried with backoff.
    Failed(String),
}

/// A user-queued run with its message text (`deriveThreadQueueWorkflowState`).
#[derive(Debug, Clone, Copy)]
pub struct QueuedRun<'a> {
    pub run: &'a Run,
    pub text: &'a str,
    pub attachments: &'a [ChatAttachment],
    pub message: Option<&'a ConversationMessage>,
}

/// A pending `user_input` request joined with its `user_input_request` item
/// (`derivePendingThreadRequests`).
#[derive(Debug, Clone, Copy)]
pub struct PendingUserInput<'a> {
    pub request: &'a RuntimeRequest,
    pub item: &'a TurnItem,
    pub questions: &'a [UserInputQuestion],
    /// The request can be dismissed (`thread.user-input.dismiss`).
    pub dismissible: bool,
}

/// The latest todo list of the thread.
#[derive(Debug, Clone, Copy)]
pub struct TodoPlan<'a> {
    pub plan_id: &'a str,
    pub status: PlanArtifactStatus,
    pub steps: &'a [PlanStep],
    pub explanation: Option<&'a str>,
}

const EMPTY_ATTACHMENTS: &[ChatAttachment] = &[];

impl ThreadState {
    pub(crate) fn new(thread_id: String) -> Self {
        Self {
            thread_id,
            sync: ThreadSync::Loading,
            sequence: 0,
            projection: None,
        }
    }

    /// The feed: visible turn items in order.
    pub fn feed(&self) -> &[ProjectedTurnItem] {
        self.projection
            .as_ref()
            .map_or(&[], |projection| &projection.visible_turn_items)
    }

    /// The last run whose status is `preparing|starting|running|waiting`
    /// (`resolveActiveThreadRun`).
    pub fn active_run(&self) -> Option<&Run> {
        self.projection.as_ref().and_then(active_run)
    }

    /// User-queued runs in queue order, with their text.
    pub fn queued_runs(&self) -> Vec<QueuedRun<'_>> {
        let Some(projection) = &self.projection else {
            return Vec::new();
        };
        let mut runs: Vec<&Run> = projection
            .runs
            .iter()
            .filter(|run| {
                run.status == RunStatus::Queued
                    && !projection.messages.iter().any(|message| {
                        message.id == run.user_message_id
                            && (message.delegated_completion.is_some()
                                || message.notification.is_some())
                    })
            })
            .collect();
        runs.sort_by_key(|run| {
            (
                run.queue_position.flatten().unwrap_or(run.ordinal),
                run.ordinal,
            )
        });
        runs.into_iter()
            .map(|run| {
                let message = projection
                    .messages
                    .iter()
                    .find(|message| message.id == run.user_message_id);
                let guest_text = message
                    .and_then(|message| message.context.as_ref())
                    .and_then(|context| context.guest.as_ref())
                    .and_then(|guest| guest.get("text"))
                    .and_then(|text| text.as_str());
                QueuedRun {
                    run,
                    text: guest_text
                        .or(message.map(|message| message.text.as_str()))
                        .unwrap_or("Queued message"),
                    attachments: message.map_or(EMPTY_ATTACHMENTS, |message| &message.attachments),
                    message,
                }
            })
            .collect()
    }

    /// Some queued run is held (after a stop with `holdQueue`); offer Resume.
    pub fn queue_held(&self) -> bool {
        self.projection.as_ref().is_some_and(|projection| {
            projection
                .runs
                .iter()
                .any(|run| run.status == RunStatus::Queued && run.queue_held == Some(true))
        })
    }

    /// No run is active and the thread is neither archived nor deleted.
    pub fn can_send_now(&self) -> bool {
        self.projection.as_ref().is_some_and(|projection| {
            active_run(projection).is_none()
                && projection.thread.archived_at.is_none()
                && projection.thread.deleted_at.is_none()
        })
    }

    /// A message can steer the active run (`canPromoteToSteer`, ios-surface
    /// §4.3): the active run is `running` with an active attempt whose provider
    /// turn is running, and the provider session supports steering; or the
    /// active run is a `/compact`, which promoting stops instead.
    pub fn can_steer(&self) -> bool {
        let Some(projection) = &self.projection else {
            return false;
        };
        let Some(run) = active_run(projection) else {
            return false;
        };
        let compaction = projection
            .messages
            .iter()
            .find(|message| message.id == run.user_message_id)
            .is_some_and(is_compaction_command);
        if compaction {
            return true;
        }
        let steerable_turn = run.status == RunStatus::Running
            && run.active_attempt_id.as_ref().is_some_and(|attempt| {
                projection.provider_turns.iter().any(|turn| {
                    turn.run_attempt_id.as_ref() == Some(attempt)
                        && turn.status == ProviderTurnStatus::Running
                })
            });
        steerable_turn
            && provider_session(projection).is_some_and(|session| {
                session.capabilities.turns.supports_active_steering
                    || session
                        .capabilities
                        .turns
                        .supports_steering_by_interrupt_restart
            })
    }

    /// Pending questions in request order; each has its `user_input_request`
    /// item (requests without one are skipped, as in t3).
    pub fn pending_user_inputs(&self) -> Vec<PendingUserInput<'_>> {
        let Some(projection) = &self.projection else {
            return Vec::new();
        };
        projection
            .runtime_requests
            .iter()
            .filter(|request| {
                request.status == RuntimeRequestStatus::Pending
                    && request.kind == RuntimeRequestKind::UserInput
            })
            .filter_map(|request| {
                let item = projection.turn_items.iter().rev().find(|item| {
                    matches!(&item.kind, TurnItemKind::UserInputRequest(body) if body.request_id == request.id)
                })?;
                let TurnItemKind::UserInputRequest(body) = &item.kind else {
                    return None;
                };
                Some(PendingUserInput {
                    request,
                    item,
                    questions: &body.questions,
                    dismissible: body.response_mode == Some(UserInputResponseMode::Message)
                        || request.response_capability == ResponseCapability::Message,
                })
            })
            .collect()
    }

    /// The latest todo list plan.
    pub fn latest_todo(&self) -> Option<TodoPlan<'_>> {
        let projection = self.projection.as_ref()?;
        projection
            .plans
            .iter()
            .rev()
            .find_map(|plan| match &plan.kind {
                PlanArtifactKind::TodoList { steps, explanation } => Some(TodoPlan {
                    plan_id: &plan.id,
                    status: plan.status,
                    steps,
                    explanation: explanation.as_deref(),
                }),
                PlanArtifactKind::ProposedPlan { .. } => None,
            })
    }

    /// The thread's omp goal: on the active provider thread (else any
    /// provider thread that has one). `None` without a goal; a completed goal
    /// stays until it is dropped or replaced.
    pub fn goal(&self) -> Option<&ProviderGoal> {
        let projection = self.projection.as_ref()?;
        let active = projection.thread.active_provider_thread_id.as_ref();
        projection
            .provider_threads
            .iter()
            .find(|thread| Some(&thread.id) == active)
            .and_then(|thread| thread.goal.as_ref())
            .or_else(|| {
                projection
                    .provider_threads
                    .iter()
                    .find_map(|thread| thread.goal.as_ref())
            })
    }

    /// Subagents the agent started, oldest first, with status, latest
    /// progress line and final result. Each also has a `subagent` turn item in
    /// the feed (`TurnItemKind::Subagent`, keyed by `subagent_id`).
    pub fn subagents(&self) -> &[Subagent] {
        self.projection
            .as_ref()
            .map_or(&[], |projection| &projection.subagents)
    }

    /// One subagent by id (the `subagentId` of its turn item).
    pub fn subagent(&self, subagent_id: &str) -> Option<&Subagent> {
        self.subagents()
            .iter()
            .find(|subagent| subagent.id == subagent_id)
    }

    /// Applies one `orchestration.subscribeThread` item (`applyItems` in t3's
    /// `threads.ts`). `awaiting` is true until the `synchronized` marker of the
    /// current subscription. Returns whether the state changed.
    pub(crate) fn apply(&mut self, item: ThreadStreamItem, awaiting: &mut bool) -> bool {
        match item {
            ThreadStreamItem::Synchronized => {
                *awaiting = false;
                if self.projection.is_some()
                    && matches!(self.sync, ThreadSync::Synchronizing | ThreadSync::Loading)
                {
                    self.sync = ThreadSync::Live;
                    return true;
                }
                false
            }
            ThreadStreamItem::Snapshot(snapshot) => {
                // Snapshots replace everything.
                self.sequence = snapshot.snapshot_sequence;
                self.projection = Some(snapshot.projection);
                self.sync = if *awaiting {
                    ThreadSync::Synchronizing
                } else {
                    ThreadSync::Live
                };
                true
            }
            ThreadStreamItem::Event(item) => {
                if item.sequence <= self.sequence || self.sync == ThreadSync::Deleted {
                    return false;
                }
                self.sequence = item.sequence;
                if matches!(item.event.body, DomainEventBody::ThreadDeleted(_)) {
                    self.projection = None;
                    self.sync = ThreadSync::Deleted;
                    return true;
                }
                let Some(projection) = &mut self.projection else {
                    return true;
                };
                apply_event(projection, item.event);
                self.sync = if *awaiting {
                    ThreadSync::Synchronizing
                } else {
                    ThreadSync::Live
                };
                true
            }
            ThreadStreamItem::Unknown(_) => false,
        }
    }
}

fn active_run(projection: &ThreadProjection) -> Option<&Run> {
    projection
        .runs
        .iter()
        .rev()
        .find(|run| run.status.is_active())
}

/// `isCompactionCommandMessage`.
fn is_compaction_command(message: &ConversationMessage) -> bool {
    message.role == MessageRole::User
        && message.text.trim().to_lowercase() == "/compact"
        && message.attachments.is_empty()
}

/// `resolveThreadProviderSession`.
fn provider_session(projection: &ThreadProjection) -> Option<&ProviderSession> {
    let provider_thread_id = active_run(projection)
        .and_then(|run| run.provider_thread_id.as_ref())
        .or(projection.thread.active_provider_thread_id.as_ref());
    let active_provider_thread = provider_thread_id.and_then(|id| {
        projection
            .provider_threads
            .iter()
            .find(|thread| &thread.id == id)
    });
    let attached = active_provider_thread.or_else(|| {
        projection.provider_threads.iter().find(|thread| {
            thread.app_thread_id.as_deref() == Some(projection.thread.id.as_str())
                && thread.provider_session_id.is_some()
        })
    });
    if let Some(session_id) = attached.and_then(|thread| thread.provider_session_id.as_ref()) {
        return projection
            .provider_sessions
            .iter()
            .find(|session| &session.id == session_id);
    }
    projection.provider_sessions.iter().rev().find(|session| {
        session.status != ProviderSessionStatus::Stopped
            && session.status != ProviderSessionStatus::Error
    })
}
