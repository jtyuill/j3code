//! Applies thread domain events to a projection exactly like t3's
//! client-runtime `applyOrchestrationV2ProjectionEvent`
//! (`packages/client-runtime/src/state/orchestrationV2Projection.ts`), with the
//! timeline visibility rules of `packages/shared/src/orchestrationV2Timeline.ts`.
//!
//! j3 snapshots are always the complete timeline (no history cursor), so the
//! partial-timeline branches of the t3 reducer never apply.

use std::collections::{HashMap, HashSet};

use j3_protocol::t3::orchestration::{
    DomainEvent, DomainEventBody, ProjectedTurnItem, RunAttempt, RunAttemptStatus, RunStatus,
    ThreadProjection, TurnItem, TurnItemKind, TurnItemVisibility, UserMessageInputIntent,
};

/// Entities replaced by id.
trait Entity {
    fn entity_id(&self) -> &str;
}

macro_rules! entity {
    ($($ty:ty),*) => {$(
        impl Entity for $ty {
            fn entity_id(&self) -> &str {
                &self.id
            }
        }
    )*};
}

entity!(
    j3_protocol::t3::orchestration::Run,
    RunAttempt,
    j3_protocol::t3::orchestration::ProviderSession,
    j3_protocol::t3::orchestration::ProviderThread,
    j3_protocol::t3::orchestration::ProviderTurn,
    j3_protocol::t3::orchestration::RuntimeRequest,
    j3_protocol::t3::orchestration::ConversationMessage,
    j3_protocol::t3::orchestration::PlanArtifact,
    j3_protocol::t3::orchestration::Subagent
);

/// `upsertEntity`: replace in place, else append.
fn upsert<T: Entity>(items: &mut Vec<T>, item: T) {
    match items
        .iter()
        .position(|candidate| candidate.entity_id() == item.entity_id())
    {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
}

/// Applies one event. Returns whether the projection changed (an event for
/// another thread, or of a type this build does not know, changes nothing).
pub(crate) fn apply_event(projection: &mut ThreadProjection, event: DomainEvent) -> bool {
    if event.thread_id != projection.thread.id {
        return false;
    }
    let occurred_at = event.occurred_at;
    match event.body {
        DomainEventBody::ThreadCreated(thread)
        | DomainEventBody::ThreadArchived(thread)
        | DomainEventBody::ThreadUnarchived(thread)
        | DomainEventBody::ThreadDeleted(thread)
        | DomainEventBody::ThreadMetadataUpdated(thread)
        | DomainEventBody::ThreadRuntimeModeUpdated(thread)
        | DomainEventBody::ThreadInteractionModeUpdated(thread)
        | DomainEventBody::ThreadModelSelectionUpdated(thread)
        | DomainEventBody::ThreadSettled(thread)
        | DomainEventBody::ThreadUnsettled(thread)
        | DomainEventBody::ThreadSnoozed(thread)
        | DomainEventBody::ThreadUnsnoozed(thread)
        | DomainEventBody::ThreadPinned(thread)
        | DomainEventBody::ThreadAutoSettleSet(thread)
        | DomainEventBody::ThreadUnpinned(thread)
        | DomainEventBody::ThreadPinReordered(thread)
        | DomainEventBody::ThreadActiveReordered(thread)
        | DomainEventBody::ThreadVisited(thread)
        | DomainEventBody::ThreadMarkedUnread(thread) => {
            projection.thread = thread;
        }
        DomainEventBody::RunCreated(run) | DomainEventBody::RunUpdated(run) => {
            upsert(&mut projection.runs, run);
            retain_visible(projection);
        }
        DomainEventBody::RunAttemptCreated(attempt)
        | DomainEventBody::RunAttemptUpdated(attempt) => {
            upsert(&mut projection.attempts, attempt);
            retain_visible(projection);
        }
        DomainEventBody::ProviderSessionAttached(session)
        | DomainEventBody::ProviderSessionUpdated(session) => {
            upsert(&mut projection.provider_sessions, session);
        }
        DomainEventBody::ProviderThreadUpdated(thread) => {
            upsert(&mut projection.provider_threads, thread);
        }
        DomainEventBody::ProviderTurnUpdated(mut turn) => {
            // A turn update without token usage keeps the usage already known.
            if turn.token_usage.is_none() {
                turn.token_usage = projection
                    .provider_turns
                    .iter()
                    .find(|existing| existing.id == turn.id)
                    .and_then(|existing| existing.token_usage.clone());
            }
            upsert(&mut projection.provider_turns, turn);
        }
        DomainEventBody::RuntimeRequestUpdated(request) => {
            upsert(&mut projection.runtime_requests, request);
        }
        DomainEventBody::MessageUpdated(message) => {
            upsert(&mut projection.messages, message);
        }
        DomainEventBody::PlanUpdated(plan) => {
            upsert(&mut projection.plans, plan);
        }
        DomainEventBody::SubagentUpdated(subagent) => {
            upsert(&mut projection.subagents, subagent);
        }
        DomainEventBody::TurnItemUpdated(item) => apply_turn_item(projection, item),
        DomainEventBody::Unknown(_) => return false,
    }
    projection.updated_at = occurred_at;
    true
}

fn apply_turn_item(projection: &mut ThreadProjection, mut item: TurnItem) {
    // Streamed updates land on the newest items, so search from the end.
    let index = projection
        .turn_items
        .iter()
        .rposition(|candidate| candidate.id == item.id);
    let mut previous_was_interrupt_request = false;
    match index {
        Some(index) => {
            let previous = &projection.turn_items[index];
            if let Some(deferred) = &previous.deferred_work {
                item.deferred_work = Some(deferred.clone());
            }
            previous_was_interrupt_request =
                matches!(previous.kind, TurnItemKind::RunInterruptRequest(_));
            projection.turn_items[index] = item.clone();
        }
        None => projection.turn_items.push(item.clone()),
    }
    // Only interrupt requests can change another item's visibility.
    if previous_was_interrupt_request || matches!(item.kind, TurnItemKind::RunInterruptRequest(_)) {
        retain_visible(projection);
    }
    if is_turn_item_visible(&item, projection) {
        upsert_visible(&mut projection.visible_turn_items, item);
    } else {
        remove_visible(&mut projection.visible_turn_items, &item.id);
    }
}

fn input_intent(item: &TurnItem) -> Option<UserMessageInputIntent> {
    match &item.kind {
        TurnItemKind::UserMessage(message) => Some(message.input_intent),
        _ => None,
    }
}

/// `isOrchestrationV2TurnItemVisible`.
fn is_turn_item_visible(item: &TurnItem, projection: &ThreadProjection) -> bool {
    let run_status = item.run_id.as_deref().and_then(|run_id| {
        projection
            .runs
            .iter()
            .find(|run| run.id == run_id)
            .map(|run| run.status)
    });
    if run_status == Some(RunStatus::RolledBack) {
        return false;
    }
    // Cancelled queued input never reached the provider.
    if run_status == Some(RunStatus::Cancelled)
        && input_intent(item) == Some(UserMessageInputIntent::QueuedTurn)
    {
        return false;
    }
    !is_superseded_interrupt(item, &projection.attempts, &projection.turn_items)
}

/// `isOrchestrationV2SupersededInterrupt`: a stop result of a superseded
/// attempt without a matching stop request.
fn is_superseded_interrupt(item: &TurnItem, attempts: &[RunAttempt], items: &[TurnItem]) -> bool {
    let (TurnItemKind::RunInterruptResult(_), Some(run_id), Some(node_id)) =
        (&item.kind, &item.run_id, &item.node_id)
    else {
        return false;
    };
    let superseded = attempts.iter().any(|attempt| {
        &attempt.run_id == run_id
            && &attempt.root_node_id == node_id
            && attempt.status == RunAttemptStatus::Superseded
    });
    superseded
        && !items.iter().any(|candidate| {
            matches!(candidate.kind, TurnItemKind::RunInterruptRequest(_))
                && candidate.run_id.as_ref() == Some(run_id)
        })
}

/// `activeVisibleTurnItems`: drops local rows that are no longer visible
/// (`createOrchestrationV2TurnItemVisibility`).
fn retain_visible(projection: &mut ThreadProjection) {
    let statuses: HashMap<&str, RunStatus> = projection
        .runs
        .iter()
        .map(|run| (run.id.as_str(), run.status))
        .collect();
    let mut superseded_roots: HashMap<&str, HashSet<&str>> = HashMap::new();
    for attempt in &projection.attempts {
        if attempt.status == RunAttemptStatus::Superseded {
            superseded_roots
                .entry(attempt.run_id.as_str())
                .or_default()
                .insert(attempt.root_node_id.as_str());
        }
    }
    let interrupt_runs: HashSet<Option<&str>> = projection
        .turn_items
        .iter()
        .filter(|item| matches!(item.kind, TurnItemKind::RunInterruptRequest(_)))
        .map(|item| item.run_id.as_deref())
        .collect();
    let is_visible = |item: &TurnItem| {
        let status = item
            .run_id
            .as_deref()
            .and_then(|run_id| statuses.get(run_id).copied());
        if status == Some(RunStatus::RolledBack) {
            return false;
        }
        if status == Some(RunStatus::Cancelled)
            && input_intent(item) == Some(UserMessageInputIntent::QueuedTurn)
        {
            return false;
        }
        !(matches!(item.kind, TurnItemKind::RunInterruptResult(_))
            && match (item.run_id.as_deref(), item.node_id.as_deref()) {
                (Some(run_id), Some(node_id)) => {
                    superseded_roots
                        .get(run_id)
                        .is_some_and(|roots| roots.contains(node_id))
                        && !interrupt_runs.contains(&Some(run_id))
                }
                _ => false,
            })
    };
    let before = projection.visible_turn_items.len();
    let mut rows = std::mem::take(&mut projection.visible_turn_items);
    rows.retain(|row| row.visibility != TurnItemVisibility::Local || is_visible(&row.item));
    if rows.len() != before {
        renumber(&mut rows);
    }
    projection.visible_turn_items = rows;
}

fn renumber(rows: &mut [ProjectedTurnItem]) {
    for (position, row) in rows.iter_mut().enumerate() {
        row.position = position as u64;
    }
}

/// `removeVisibleItem`.
fn remove_visible(rows: &mut Vec<ProjectedTurnItem>, source_item_id: &str) {
    if let Some(index) = rows
        .iter()
        .position(|row| row.source_item_id == source_item_id)
    {
        rows.remove(index);
        renumber(rows);
    }
}

/// `upsertVisibleTurnItem` (complete timeline): a row whose ordinal did not
/// change is replaced in place; otherwise the row moves before the first later
/// local row (by ordinal, then id).
fn upsert_visible(rows: &mut Vec<ProjectedTurnItem>, item: TurnItem) {
    let index = rows.iter().rposition(|row| row.source_item_id == item.id);
    if let Some(index) = index {
        let previous = &rows[index];
        if previous.visibility == TurnItemVisibility::Local
            && previous.source_thread_id == item.thread_id
            && previous.item.ordinal == item.ordinal
        {
            rows[index].item = item;
            return;
        }
        rows.remove(index);
    }
    let insertion = rows
        .iter()
        .position(|row| {
            row.visibility == TurnItemVisibility::Local
                && (row.item.ordinal > item.ordinal
                    || (row.item.ordinal == item.ordinal && row.item.id > item.id))
        })
        .unwrap_or(rows.len());
    rows.insert(
        insertion,
        ProjectedTurnItem {
            position: 0,
            visibility: TurnItemVisibility::Local,
            source_thread_id: item.thread_id.clone(),
            source_item_id: item.id.clone(),
            item,
        },
    );
    renumber(rows);
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn item(id: &str, ordinal: u64, run_id: &str, kind: Value) -> TurnItem {
        let mut value = json!({
            "id": id, "threadId": "T", "runId": run_id, "nodeId": "node_1",
            "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
            "parentItemId": null, "ordinal": ordinal, "status": "completed", "title": null,
            "startedAt": null, "completedAt": null, "updatedAt": "2026-11-01T12:00:00.000Z",
        });
        for (key, field) in kind.as_object().unwrap() {
            value[key] = field.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    fn assistant(id: &str, ordinal: u64, text: &str) -> TurnItem {
        item(
            id,
            ordinal,
            "run_1",
            json!({"type": "assistant_message", "messageId": id, "text": text, "streaming": true}),
        )
    }

    fn projection() -> ThreadProjection {
        serde_json::from_value(json!({
            "thread": {"createdBy": "user", "creationSource": "web", "id": "T", "projectId": "P",
                "title": "t", "providerInstanceId": "omp",
                "modelSelection": {"instanceId": "omp", "model": "m"}, "runtimeMode": "full-access",
                "interactionMode": "default", "branch": null, "worktreePath": null,
                "activeProviderThreadId": null,
                "lineage": {"parentThreadId": null, "relationshipToParent": null, "rootThreadId": "T"},
                "forkedFrom": null, "createdAt": "x", "updatedAt": "x", "archivedAt": null,
                "deletedAt": null},
            "runs": [], "attempts": [], "nodes": [], "subagents": [], "providerSessions": [],
            "providerThreads": [], "providerTurns": [], "runtimeRequests": [], "messages": [],
            "plans": [], "turnItems": [], "checkpointScopes": [], "checkpoints": [],
            "contextHandoffs": [], "contextTransfers": [], "visibleTurnItems": [],
            "updatedAt": "x"
        }))
        .unwrap()
    }

    fn event(body: Value) -> DomainEvent {
        let mut value =
            json!({"id": "e", "threadId": "T", "occurredAt": "2026-11-01T12:00:01.000Z"});
        for (key, field) in body.as_object().unwrap() {
            value[key] = field.clone();
        }
        serde_json::from_value(value).unwrap()
    }

    fn turn_item_event(item: &TurnItem) -> DomainEvent {
        event(json!({"type": "turn-item.updated", "payload": item}))
    }

    fn feed_ids(projection: &ThreadProjection) -> Vec<(u64, String)> {
        projection
            .visible_turn_items
            .iter()
            .map(|row| (row.position, row.source_item_id.clone()))
            .collect()
    }

    #[test]
    fn items_are_ordered_by_ordinal_and_replaced_in_place() {
        let mut p = projection();
        assert!(apply_event(
            &mut p,
            turn_item_event(&assistant("b", 2, "x"))
        ));
        assert!(apply_event(
            &mut p,
            turn_item_event(&assistant("a", 1, "y"))
        ));
        assert!(apply_event(
            &mut p,
            turn_item_event(&assistant("c", 2, "z"))
        ));
        assert_eq!(
            feed_ids(&p),
            vec![(0, "a".into()), (1, "b".into()), (2, "c".into())]
        );
        assert!(apply_event(
            &mut p,
            turn_item_event(&assistant("b", 2, "x more"))
        ));
        let TurnItemKind::AssistantMessage(message) = &p.visible_turn_items[1].item.kind else {
            panic!()
        };
        assert_eq!(message.text, "x more");
        assert_eq!(p.turn_items.len(), 3);
        assert_eq!(p.updated_at, "2026-11-01T12:00:01.000Z");
        // A changed ordinal moves the row.
        assert!(apply_event(
            &mut p,
            turn_item_event(&assistant("a", 3, "y"))
        ));
        assert_eq!(
            feed_ids(&p),
            vec![(0, "b".into()), (1, "c".into()), (2, "a".into())]
        );
    }

    #[test]
    fn cancelled_queued_input_and_rolled_back_runs_are_hidden() {
        let mut p = projection();
        let queued = item(
            "q",
            1,
            "run_2",
            json!({"type": "user_message", "createdBy": "user", "creationSource": "web",
                   "messageId": "m", "inputIntent": "queued_turn", "text": "later", "attachments": []}),
        );
        apply_event(&mut p, turn_item_event(&queued));
        apply_event(&mut p, turn_item_event(&assistant("a", 2, "x")));
        assert_eq!(p.visible_turn_items.len(), 2);
        let run = |id: &str, status: &str| {
            event(json!({"type": "run.updated", "payload": {
                "id": id, "threadId": "T", "ordinal": 1, "providerInstanceId": "omp",
                "modelSelection": {"instanceId": "omp", "model": "m"}, "providerThreadId": null,
                "userMessageId": "m", "rootNodeId": null, "activeAttemptId": null, "status": status,
                "requestedAt": "x", "startedAt": null, "completedAt": null, "checkpointId": null,
                "contextHandoffId": null}}))
        };
        apply_event(&mut p, run("run_2", "cancelled"));
        assert_eq!(feed_ids(&p), vec![(0, "a".into())]);
        apply_event(&mut p, run("run_1", "rolled_back"));
        assert!(p.visible_turn_items.is_empty());
        // A later update of a hidden item stays hidden.
        apply_event(&mut p, turn_item_event(&assistant("a", 2, "x2")));
        assert!(p.visible_turn_items.is_empty());
    }

    #[test]
    fn events_for_other_threads_and_unknown_types_change_nothing() {
        let mut p = projection();
        let mut other = turn_item_event(&assistant("a", 1, "x"));
        other.thread_id = "U".into();
        assert!(!apply_event(&mut p, other));
        assert!(!apply_event(
            &mut p,
            event(json!({"type": "node.updated", "payload": {}}))
        ));
        assert!(p.turn_items.is_empty());
    }
}
