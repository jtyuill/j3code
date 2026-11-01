//! The typed working projection of one thread, its change tracking, and the
//! derived wire shapes (`OrchestrationV2ThreadProjection`, thread shell).
//!
//! Every mutation marks the touched entity pending together with the domain
//! event type it produces. [`Projection::drain`] turns the pending set into
//! store rows plus one domain event per entity, in the order they were first
//! touched; streaming-only changes are flagged so the actor can coalesce them.

use j3_protocol::t3::orchestration::{
    ActivityRunStatus, AppThread, ConversationMessage, LatestVisibleMessageSummary, MessageRole,
    PendingRuntimeRequestSummary, PlanArtifact, ProjectedTurnItem, ProviderGoal, ProviderSession,
    ProviderThread, ProviderTurn, Run, RunAttempt, RunStatus, RuntimeRequest, RuntimeRequestStatus,
    ShellThreadStatus, Subagent, ThreadProjection, ThreadShell, TurnItem, TurnItemKind,
    TurnItemVisibility, UserMessageInputIntent,
};
use serde::Serialize;
use serde_json::Value;

use crate::{hub::PendingEvent, store::EntityRow};

/// Provider instance every j3 thread uses.
pub const PROVIDER_INSTANCE: &str = "omp";
/// Client-side driver kind for omp (Pi is omp's upstream).
pub const DRIVER: &str = "pi";

/// One entity collection of a projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Thread,
    Run,
    Attempt,
    Session,
    ProviderThread,
    ProviderTurn,
    Request,
    Message,
    Plan,
    Item,
    Subagent,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thread => "thread",
            Self::Run => "run",
            Self::Attempt => "attempt",
            Self::Session => "providerSession",
            Self::ProviderThread => "providerThread",
            Self::ProviderTurn => "providerTurn",
            Self::Request => "runtimeRequest",
            Self::Message => "message",
            Self::Plan => "plan",
            Self::Item => "turnItem",
            Self::Subagent => "subagent",
        }
    }

    fn parse(kind: &str) -> Option<Self> {
        Some(match kind {
            "thread" => Self::Thread,
            "run" => Self::Run,
            "attempt" => Self::Attempt,
            "providerSession" => Self::Session,
            "providerThread" => Self::ProviderThread,
            "providerTurn" => Self::ProviderTurn,
            "runtimeRequest" => Self::Request,
            "message" => Self::Message,
            "plan" => Self::Plan,
            "turnItem" => Self::Item,
            "subagent" => Self::Subagent,
            _ => return None,
        })
    }

    /// Event type for an update of an existing entity.
    pub fn updated_event(self) -> &'static str {
        match self {
            Self::Thread => "thread.metadata-updated",
            Self::Run => "run.updated",
            Self::Attempt => "run-attempt.updated",
            Self::Session => "provider-session.updated",
            Self::ProviderThread => "provider-thread.updated",
            Self::ProviderTurn => "provider-turn.updated",
            Self::Request => "runtime-request.updated",
            Self::Message => "message.updated",
            Self::Plan => "plan.updated",
            Self::Item => "turn-item.updated",
            Self::Subagent => "subagent.updated",
        }
    }

    /// Event type for a newly created entity.
    pub fn created_event(self) -> &'static str {
        match self {
            Self::Thread => "thread.created",
            Self::Run => "run.created",
            Self::Attempt => "run-attempt.created",
            Self::Session => "provider-session.attached",
            other => other.updated_event(),
        }
    }
}

#[derive(Debug, Clone)]
struct Pending {
    kind: Kind,
    id: String,
    event: &'static str,
    /// Only streaming text changed; may wait for the coalescing window.
    streaming: bool,
}

/// Rows and events produced by one [`Projection::drain`].
#[derive(Debug, Default)]
pub struct Drained {
    pub rows: Vec<EntityRow>,
    pub events: Vec<PendingEvent>,
}

#[derive(Debug, Clone)]
pub struct Projection {
    pub thread: AppThread,
    pub runs: Vec<Run>,
    pub attempts: Vec<RunAttempt>,
    pub sessions: Vec<ProviderSession>,
    pub provider_threads: Vec<ProviderThread>,
    pub provider_turns: Vec<ProviderTurn>,
    pub requests: Vec<RuntimeRequest>,
    pub messages: Vec<ConversationMessage>,
    pub plans: Vec<PlanArtifact>,
    pub items: Vec<TurnItem>,
    pub subagents: Vec<Subagent>,
    pending: Vec<Pending>,
}

impl Projection {
    pub fn new(thread: AppThread) -> Self {
        Self {
            thread,
            runs: Vec::new(),
            attempts: Vec::new(),
            sessions: Vec::new(),
            provider_threads: Vec::new(),
            provider_turns: Vec::new(),
            requests: Vec::new(),
            messages: Vec::new(),
            plans: Vec::new(),
            items: Vec::new(),
            subagents: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Rebuilds a projection from stored rows; `None` without a thread row.
    pub fn from_rows(rows: Vec<EntityRow>) -> Result<Option<Self>, serde_json::Error> {
        let mut thread = None;
        let mut projection_rows = Vec::with_capacity(rows.len());
        for row in rows {
            match Kind::parse(&row.kind) {
                Some(Kind::Thread) => thread = Some(serde_json::from_value(row.json)?),
                Some(kind) => projection_rows.push((kind, row.json)),
                None => {}
            }
        }
        let Some(thread) = thread else {
            return Ok(None);
        };
        let mut projection = Self::new(thread);
        // Rows arrive ordered by kind, ordinal, id.
        for (kind, json) in projection_rows {
            match kind {
                Kind::Thread => {}
                Kind::Run => projection.runs.push(serde_json::from_value(json)?),
                Kind::Attempt => projection.attempts.push(serde_json::from_value(json)?),
                Kind::Session => projection.sessions.push(serde_json::from_value(json)?),
                Kind::ProviderThread => projection
                    .provider_threads
                    .push(serde_json::from_value(json)?),
                Kind::ProviderTurn => projection
                    .provider_turns
                    .push(serde_json::from_value(json)?),
                Kind::Request => projection.requests.push(serde_json::from_value(json)?),
                Kind::Message => projection.messages.push(serde_json::from_value(json)?),
                Kind::Plan => projection.plans.push(serde_json::from_value(json)?),
                Kind::Item => projection.items.push(serde_json::from_value(json)?),
                Kind::Subagent => projection.subagents.push(serde_json::from_value(json)?),
            }
        }
        Ok(Some(projection))
    }

    /// Marks an entity changed; `event` is its domain event type.
    pub fn touch(&mut self, kind: Kind, id: &str, event: &'static str) {
        self.mark(kind, id, event, false);
    }

    /// Marks an entity created.
    pub fn touch_created(&mut self, kind: Kind, id: &str) {
        self.mark(kind, id, kind.created_event(), false);
    }

    /// Marks an entity updated.
    pub fn touch_updated(&mut self, kind: Kind, id: &str) {
        self.mark(kind, id, kind.updated_event(), false);
    }

    /// Marks a streaming-only change (text appended to a streaming item).
    pub fn touch_streaming(&mut self, kind: Kind, id: &str) {
        self.mark(kind, id, kind.updated_event(), true);
    }

    fn mark(&mut self, kind: Kind, id: &str, event: &'static str, streaming: bool) {
        if let Some(pending) = self
            .pending
            .iter_mut()
            .find(|pending| pending.kind == kind && pending.id == id)
        {
            pending.streaming &= streaming;
            // A creation in this batch stays a creation; otherwise the latest
            // (most specific) event type wins.
            if pending.event != kind.created_event() || kind == Kind::Item {
                pending.event = event;
            }
            return;
        }
        self.pending.push(Pending {
            kind,
            id: id.to_owned(),
            event,
            streaming,
        });
    }

    /// Some change must be committed now.
    pub fn has_urgent(&self) -> bool {
        self.pending.iter().any(|pending| !pending.streaming)
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Takes every pending change as rows and domain events.
    pub fn drain(&mut self) -> Result<Drained, serde_json::Error> {
        let pending = std::mem::take(&mut self.pending);
        let mut drained = Drained::default();
        for Pending {
            kind, id, event, ..
        } in pending
        {
            let Some(entity) = self.entity(kind, &id)? else {
                continue;
            };
            drained.rows.push(EntityRow {
                kind: kind.as_str().to_owned(),
                id,
                ordinal: entity.ordinal,
                json: entity.json.clone(),
            });
            drained.events.push(PendingEvent {
                event_type: event,
                payload: entity.json,
                run_id: entity.run_id,
                node_id: entity.node_id,
            });
        }
        Ok(drained)
    }

    /// Every entity as store rows (full rewrite, e.g. after recovery).
    pub fn rows(&self) -> Result<Vec<EntityRow>, serde_json::Error> {
        let mut rows = Vec::new();
        let mut push = |kind: Kind, id: &str| -> Result<(), serde_json::Error> {
            if let Some(entity) = self.entity(kind, id)? {
                rows.push(EntityRow {
                    kind: kind.as_str().to_owned(),
                    id: id.to_owned(),
                    ordinal: entity.ordinal,
                    json: entity.json,
                });
            }
            Ok(())
        };
        push(Kind::Thread, &self.thread.id.clone())?;
        for id in self.runs.iter().map(|run| run.id.clone()) {
            push(Kind::Run, &id)?;
        }
        for id in self.attempts.iter().map(|a| a.id.clone()) {
            push(Kind::Attempt, &id)?;
        }
        for id in self.sessions.iter().map(|s| s.id.clone()) {
            push(Kind::Session, &id)?;
        }
        for id in self.provider_threads.iter().map(|t| t.id.clone()) {
            push(Kind::ProviderThread, &id)?;
        }
        for id in self.provider_turns.iter().map(|t| t.id.clone()) {
            push(Kind::ProviderTurn, &id)?;
        }
        for id in self.requests.iter().map(|r| r.id.clone()) {
            push(Kind::Request, &id)?;
        }
        for id in self.messages.iter().map(|m| m.id.clone()) {
            push(Kind::Message, &id)?;
        }
        for id in self.plans.iter().map(|p| p.id.clone()) {
            push(Kind::Plan, &id)?;
        }
        for id in self.items.iter().map(|i| i.id.clone()) {
            push(Kind::Item, &id)?;
        }
        for id in self.subagents.iter().map(|s| s.id.clone()) {
            push(Kind::Subagent, &id)?;
        }
        Ok(rows)
    }

    fn entity(&self, kind: Kind, id: &str) -> Result<Option<Entity>, serde_json::Error> {
        fn find<'a, T>(
            list: &'a [T],
            id: &str,
            key: impl Fn(&T) -> &str,
        ) -> Option<(usize, &'a T)> {
            list.iter().enumerate().rev().find(|(_, x)| key(x) == id)
        }
        fn build<T: Serialize>(
            value: &T,
            ordinal: i64,
            run_id: Option<&String>,
            node_id: Option<&String>,
        ) -> Result<Entity, serde_json::Error> {
            Ok(Entity {
                json: serde_json::to_value(value)?,
                ordinal,
                run_id: run_id.cloned(),
                node_id: node_id.cloned(),
            })
        }
        let entity = match kind {
            Kind::Thread => (self.thread.id == id)
                .then(|| build(&self.thread, 0, None, None))
                .transpose()?,
            Kind::Run => find(&self.runs, id, |r| &r.id)
                .map(|(_, r)| build(r, r.ordinal as i64, Some(&r.id), r.root_node_id.as_ref()))
                .transpose()?,
            Kind::Attempt => find(&self.attempts, id, |a| &a.id)
                .map(|(i, a)| build(a, i as i64, Some(&a.run_id), Some(&a.root_node_id)))
                .transpose()?,
            Kind::Session => find(&self.sessions, id, |s| &s.id)
                .map(|(i, s)| build(s, i as i64, None, None))
                .transpose()?,
            Kind::ProviderThread => find(&self.provider_threads, id, |t| &t.id)
                .map(|(i, t)| build(t, i as i64, None, None))
                .transpose()?,
            Kind::ProviderTurn => find(&self.provider_turns, id, |t| &t.id)
                .map(|(i, t)| build(t, i as i64, None, Some(&t.node_id)))
                .transpose()?,
            Kind::Request => find(&self.requests, id, |r| &r.id)
                .map(|(i, r)| build(r, i as i64, None, Some(&r.node_id)))
                .transpose()?,
            Kind::Message => find(&self.messages, id, |m| &m.id)
                .map(|(i, m)| build(m, i as i64, m.run_id.as_ref(), m.node_id.as_ref()))
                .transpose()?,
            Kind::Plan => find(&self.plans, id, |p| &p.id)
                .map(|(i, p)| build(p, i as i64, p.run_id.as_ref(), Some(&p.node_id)))
                .transpose()?,
            Kind::Item => find(&self.items, id, |item| &item.id)
                .map(|(_, item)| {
                    build(
                        item,
                        item.ordinal as i64,
                        item.run_id.as_ref(),
                        item.node_id.as_ref(),
                    )
                })
                .transpose()?,
            Kind::Subagent => find(&self.subagents, id, |s| &s.id)
                .map(|(i, s)| build(s, i as i64, s.run_id.as_ref(), Some(&s.parent_node_id)))
                .transpose()?,
        };
        Ok(entity)
    }

    pub fn run(&self, id: &str) -> Option<&Run> {
        self.runs.iter().rev().find(|run| run.id == id)
    }

    pub fn run_mut(&mut self, id: &str) -> Option<&mut Run> {
        self.runs.iter_mut().rev().find(|run| run.id == id)
    }

    pub fn subagent(&self, id: &str) -> Option<&Subagent> {
        self.subagents
            .iter()
            .rev()
            .find(|subagent| subagent.id == id)
    }

    pub fn subagent_mut(&mut self, id: &str) -> Option<&mut Subagent> {
        self.subagents
            .iter_mut()
            .rev()
            .find(|subagent| subagent.id == id)
    }

    /// The goal of the thread's provider thread (omp goal mode).
    pub fn goal(&self) -> Option<&ProviderGoal> {
        self.provider_threads
            .iter()
            .find_map(|thread| thread.goal.as_ref())
    }

    pub fn item(&self, id: &str) -> Option<&TurnItem> {
        self.items.iter().rev().find(|item| item.id == id)
    }

    pub fn item_mut(&mut self, id: &str) -> Option<&mut TurnItem> {
        self.items.iter_mut().rev().find(|item| item.id == id)
    }

    pub fn message_mut(&mut self, id: &str) -> Option<&mut ConversationMessage> {
        self.messages
            .iter_mut()
            .rev()
            .find(|message| message.id == id)
    }

    pub fn request(&self, id: &str) -> Option<&RuntimeRequest> {
        self.requests.iter().rev().find(|request| request.id == id)
    }

    pub fn request_mut(&mut self, id: &str) -> Option<&mut RuntimeRequest> {
        self.requests
            .iter_mut()
            .rev()
            .find(|request| request.id == id)
    }

    /// The last run whose status is `preparing|starting|running|waiting`.
    pub fn active_run(&self) -> Option<&Run> {
        self.runs.iter().rev().find(|run| run.status.is_active())
    }

    /// Queued runs in queue order.
    pub fn queued_runs(&self) -> Vec<&Run> {
        let mut queued: Vec<&Run> = self
            .runs
            .iter()
            .filter(|run| run.status == RunStatus::Queued)
            .collect();
        queued.sort_by_key(|run| {
            (
                run.queue_position.flatten().unwrap_or(u64::MAX),
                run.ordinal,
            )
        });
        queued
    }

    /// The latest run that was ever started (queued runs and runs cancelled
    /// while queued do not count).
    pub fn latest_run(&self) -> Option<&Run> {
        self.runs
            .iter()
            .rev()
            .find(|run| run.status != RunStatus::Queued && run.started_at.is_some())
            .or_else(|| self.active_run())
    }

    /// Feed rows: items ordered by ordinal then id, minus hidden ones
    /// (`packages/shared/src/orchestrationV2Timeline.ts:72-108`).
    pub fn visible_items(&self) -> Vec<&TurnItem> {
        let mut visible: Vec<&TurnItem> = self
            .items
            .iter()
            .filter(|item| self.is_visible(item))
            .collect();
        visible.sort_by(|a, b| a.ordinal.cmp(&b.ordinal).then_with(|| a.id.cmp(&b.id)));
        visible
    }

    fn is_visible(&self, item: &TurnItem) -> bool {
        let run_status = item
            .run_id
            .as_deref()
            .and_then(|run_id| self.run(run_id))
            .map(|run| run.status);
        if run_status == Some(RunStatus::RolledBack) {
            return false;
        }
        !matches!(
            &item.kind,
            TurnItemKind::UserMessage(message)
                if message.input_intent == UserMessageInputIntent::QueuedTurn
                    && run_status == Some(RunStatus::Cancelled)
        )
    }

    /// The `OrchestrationV2ThreadProjection` wire shape.
    pub fn to_wire(&self) -> ThreadProjection {
        let mut projection = ThreadProjection::empty(self.thread.clone(), self.updated_at());
        let thread_id = self.thread.id.clone();
        projection.visible_turn_items = self
            .visible_items()
            .into_iter()
            .enumerate()
            .map(|(position, item)| ProjectedTurnItem {
                position: position as u64,
                visibility: TurnItemVisibility::Local,
                source_thread_id: thread_id.clone(),
                source_item_id: item.id.clone(),
                item: item.clone(),
            })
            .collect();
        let mut runs = self.runs.clone();
        runs.sort_by_key(|run| run.ordinal);
        projection.runs = runs;
        projection.attempts = self.attempts.clone();
        projection.subagents = self.subagents.clone();
        projection.provider_sessions = self.sessions.clone();
        projection.provider_threads = self.provider_threads.clone();
        projection.provider_turns = self.provider_turns.clone();
        projection.runtime_requests = self.requests.clone();
        projection.messages = self.messages.clone();
        projection.plans = self.plans.clone();
        let mut items = self.items.clone();
        items.sort_by(|a, b| a.ordinal.cmp(&b.ordinal).then_with(|| a.id.cmp(&b.id)));
        projection.turn_items = items;
        projection
    }

    /// Latest change time across the thread, its runs and items.
    pub fn updated_at(&self) -> String {
        let mut latest = self.thread.updated_at.clone();
        let candidates = self
            .items
            .iter()
            .map(|item| &item.updated_at)
            .chain(self.runs.iter().map(|run| &run.requested_at))
            .chain(self.runs.iter().filter_map(|run| run.completed_at.as_ref()));
        for at in candidates {
            if *at > latest {
                latest.clone_from(at);
            }
        }
        latest
    }

    /// The `OrchestrationV2ThreadShell` row for thread lists.
    pub fn shell(&self) -> ThreadShell {
        let thread = &self.thread;
        let active = self.active_run();
        let latest = self.latest_run();
        let status = match (active, latest) {
            (Some(run), _) | (None, Some(run)) => ShellThreadStatus::from(run.status),
            (None, None) => ShellThreadStatus::Idle,
        };
        let activity = active.and_then(|run| match run.status {
            RunStatus::Preparing => Some(ActivityRunStatus::Preparing),
            RunStatus::Starting => Some(ActivityRunStatus::Starting),
            RunStatus::Running => Some(ActivityRunStatus::Running),
            RunStatus::Waiting => Some(ActivityRunStatus::Waiting),
            _ => None,
        });
        let pending_request = self
            .requests
            .iter()
            .find(|request| request.status == RuntimeRequestStatus::Pending)
            .map(|request| PendingRuntimeRequestSummary {
                id: request.id.clone(),
                kind: request.kind,
                created_at: request.created_at.clone(),
                blocking: Some(true),
            });
        let visible = self.visible_items();
        let latest_visible_message = visible.iter().rev().find_map(|item| match &item.kind {
            TurnItemKind::UserMessage(message)
                if message.input_intent != UserMessageInputIntent::QueuedTurn
                    || item.status != j3_protocol::t3::orchestration::TurnItemStatus::Pending =>
            {
                Some(LatestVisibleMessageSummary {
                    id: message.message_id.clone(),
                    role: MessageRole::User,
                    text: message.text.clone(),
                    updated_at: item.updated_at.clone(),
                })
            }
            TurnItemKind::AssistantMessage(message) => Some(LatestVisibleMessageSummary {
                id: message.message_id.clone(),
                role: MessageRole::Assistant,
                text: message.text.clone(),
                updated_at: item.updated_at.clone(),
            }),
            _ => None,
        });
        let latest_user_message_at = self
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .filter(|message| {
                message
                    .run_id
                    .as_deref()
                    .and_then(|run_id| self.run(run_id))
                    .is_none_or(|run| run.status != RunStatus::Queued)
            })
            .map(|message| message.created_at.clone())
            .max();
        let stop_requested = active.is_some_and(|run| {
            self.items.iter().any(|item| {
                item.run_id.as_deref() == Some(run.id.as_str())
                    && matches!(item.kind, TurnItemKind::RunInterruptRequest(_))
            })
        });
        let last_error = latest
            .filter(|run| run.status == RunStatus::Failed)
            .and_then(|run| {
                self.items.iter().rev().find_map(|item| match &item.kind {
                    TurnItemKind::Error(error)
                        if item.run_id.as_deref() == Some(run.id.as_str()) =>
                    {
                        Some(error.failure.clone())
                    }
                    _ => None,
                })
            });
        ThreadShell {
            created_by: thread.created_by,
            creation_source: thread.creation_source,
            id: thread.id.clone(),
            project_id: thread.project_id.clone(),
            title: thread.title.clone(),
            provider_instance_id: thread.provider_instance_id.clone(),
            model_selection: thread.model_selection.clone(),
            runtime_mode: thread.runtime_mode,
            interaction_mode: thread.interaction_mode,
            branch: thread.branch.clone(),
            worktree_path: thread.worktree_path.clone(),
            linked_pull_request: None,
            pull_requests: None,
            branch_pull_request: None,
            lineage: thread.lineage.clone(),
            forked_from: thread.forked_from.clone(),
            active_provider_thread_id: thread.active_provider_thread_id.clone(),
            history_origin: None,
            link_origin: None,
            delegated_from: None,
            latest_run_id: latest.map(|run| run.id.clone()),
            latest_run_requested_at: latest.map(|run| Some(run.requested_at.clone())),
            latest_run_started_at: latest.map(|run| run.started_at.clone()),
            latest_run_completed_at: latest.map(|run| run.completed_at.clone()),
            latest_run_stop_requested: stop_requested.then_some(true),
            active_run_id: active.map(|run| run.id.clone()),
            activity_run_started_at: Some(active.and_then(|run| run.started_at.clone())),
            activity_run_status: Some(activity),
            blocked_on_thread_wait: None,
            status,
            last_error: last_error
                .as_ref()
                .map(|failure| Some(failure.message.clone())),
            last_error_class: last_error.as_ref().map(|failure| Some(failure.class)),
            last_error_code: last_error.map(|failure| failure.code),
            usage_limit_reset_at: None,
            pending_runtime_request: pending_request,
            pending_guest_message_count: None,
            latest_visible_message,
            latest_user_message_at,
            latest_user_authored_message_at: None,
            has_actionable_proposed_plan: false,
            pending_background_tasks: Vec::new(),
            provider_instance_history: vec![thread.provider_instance_id.clone()],
            goal: self.goal().cloned().map(Some),
            item_count: self.items.len() as u64,
            visible_item_count: visible.len() as u64,
            created_at: thread.created_at.clone(),
            updated_at: self.updated_at(),
            archived_at: thread.archived_at.clone(),
            settled_override: thread.settled_override,
            settled_at: thread.settled_at.clone(),
            unsettled_at: thread.unsettled_at.clone(),
            snoozed_until: thread.snoozed_until.clone(),
            snoozed_at: thread.snoozed_at.clone(),
            limit_recovery: None,
            pinned_at: thread.pinned_at.clone(),
            auto_settle_disabled_at: thread.auto_settle_disabled_at.clone(),
            pin_order_key: thread.pin_order_key.clone(),
            active_order_key: thread.active_order_key.clone(),
            last_visited_at: Some(thread.last_visited_at.clone()),
            opens: None,
            title_regeneration: None,
            deleted_at: thread.deleted_at.clone(),
        }
    }
}

struct Entity {
    json: Value,
    ordinal: i64,
    run_id: Option<String>,
    node_id: Option<String>,
}
