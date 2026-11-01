//! omp goal mode and subagents → t3's provider goal and subagent entities
//! (`docs/protocol/j3-extensions.md`).
//!
//! - Goal: `get_state.goal`, `goal_updated` and `goal` command results set
//!   `ProviderThread.goal` (and so `ThreadShell.goal`).
//! - Subagents: `subagent_lifecycle` / `subagent_progress` frames (subscription
//!   level `progress`) become one `subagents` entry plus one `subagent` turn
//!   item each; the result is the final assistant text of the subagent's
//!   transcript (`get_subagent_messages`), with its last output lines as the
//!   fallback.

use j3_protocol::t3::orchestration::{
    Actor, NativeRefStrength, ProviderGoal, ProviderGoalStatus, ProviderRef, Subagent,
    SubagentItem, SubagentOrigin, SubagentStatus, TurnItemKind, TurnItemStatus,
};
use serde_json::{Map, Value};

use super::{Effect, ThreadModel, now, truncate};
use crate::{
    engine::projection::{DRIVER, Kind, PROVIDER_INSTANCE},
    omp::omp_rpc::{
        AgentMessage, AssistantContent, Goal, GoalModeState, GoalStatus, SubagentLifecyclePayload,
        SubagentLifecycleStatus, SubagentMessages, SubagentProgressPayload,
    },
};

/// Longest activity text in a subagent's progress line.
const MAX_ACTIVITY: usize = 200;

/// omp goal → t3 provider goal; `None` for a dropped goal (t3 has no
/// dropped state: the goal is gone).
fn provider_goal(goal: &Goal) -> Option<ProviderGoal> {
    let status = match goal.status {
        GoalStatus::Active => ProviderGoalStatus::Active,
        GoalStatus::Paused => ProviderGoalStatus::Paused,
        GoalStatus::BudgetLimited => ProviderGoalStatus::BudgetLimited,
        GoalStatus::Complete => ProviderGoalStatus::Complete,
        GoalStatus::Dropped => return None,
    };
    let objective = goal.objective.trim();
    if objective.is_empty() {
        return None;
    }
    Some(ProviderGoal {
        objective: objective.to_owned(),
        status,
        tokens_used: u64::try_from(goal.tokens_used).ok(),
        token_budget: goal.token_budget.map(|budget| u64::try_from(budget).ok()),
        time_used_seconds: Some(goal.time_used_seconds.max(0.0).round() as u64),
        checks: None,
        last_check: None,
    })
}

fn item_status(status: SubagentStatus) -> TurnItemStatus {
    match status {
        SubagentStatus::Idle => TurnItemStatus::Idle,
        SubagentStatus::Pending => TurnItemStatus::Pending,
        SubagentStatus::Running => TurnItemStatus::Running,
        SubagentStatus::Waiting => TurnItemStatus::Waiting,
        SubagentStatus::Completed => TurnItemStatus::Completed,
        SubagentStatus::Failed => TurnItemStatus::Failed,
        SubagentStatus::Cancelled => TurnItemStatus::Cancelled,
        SubagentStatus::Interrupted => TurnItemStatus::Interrupted,
    }
}

fn text<'a>(progress: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    progress
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// `AgentProgress.status` → subagent status.
fn progress_status(progress: &Map<String, Value>) -> Option<SubagentStatus> {
    Some(match text(progress, "status")? {
        "pending" => SubagentStatus::Pending,
        "running" => SubagentStatus::Running,
        "completed" => SubagentStatus::Completed,
        "failed" => SubagentStatus::Failed,
        "aborted" => SubagentStatus::Cancelled,
        _ => return None,
    })
}

/// `12345` → `12.3k`.
fn compact_count(count: u64) -> String {
    if count < 1000 {
        return count.to_string();
    }
    let thousands = format!("{:.1}", count as f64 / 1000.0);
    format!("{}k", thousands.trim_end_matches(".0"))
}

/// One line from an `AgentProgress` record: what it is doing now, tool calls
/// so far and tokens used, e.g. `Reading the config · 3 tool calls · 14.2k tokens`.
fn progress_summary(progress: &Map<String, Value>) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(activity) = text(progress, "currentToolIntent")
        .or_else(|| text(progress, "lastIntent"))
        .or_else(|| text(progress, "currentTool"))
    {
        parts.push(truncate(activity, MAX_ACTIVITY));
    }
    let count = |key: &str| progress.get(key).and_then(Value::as_u64).filter(|n| *n > 0);
    if let Some(tools) = count("toolCount") {
        let plural = if tools == 1 { "" } else { "s" };
        parts.push(format!("{tools} tool call{plural}"));
    }
    if let Some(tokens) = count("tokens") {
        parts.push(format!("{} tokens", compact_count(tokens)));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The text of the last assistant message that has any.
fn final_text(messages: &[AgentMessage]) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        let AgentMessage::Assistant(message) = message else {
            return None;
        };
        let text = message
            .content
            .iter()
            .flatten()
            .filter_map(|block| match block {
                AssistantContent::Text(block) => block.text.as_deref(),
                _ => None,
            })
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        (!text.is_empty()).then_some(text)
    })
}

impl ThreadModel {
    // -----------------------------------------------------------------------
    // Goal
    // -----------------------------------------------------------------------

    /// `goal_updated` and `goal` command results: the goal as omp has it now.
    pub fn apply_goal(&mut self, goal: Option<&Goal>) {
        self.set_goal(goal.and_then(provider_goal));
    }

    /// `get_state.goal`. omp forgets a goal once it is complete, so a
    /// completed goal stays until another goal replaces it or it is dropped.
    pub fn apply_goal_state(&mut self, state: Option<&GoalModeState>) {
        match state {
            Some(state) => self.apply_goal(Some(&state.goal)),
            None if self
                .proj
                .goal()
                .is_some_and(|goal| goal.status == ProviderGoalStatus::Complete) => {}
            None => self.set_goal(None),
        }
    }

    /// The thread pursues an active goal; omp must stay up for it.
    pub fn goal_active(&self) -> bool {
        self.proj
            .goal()
            .is_some_and(|goal| goal.status == ProviderGoalStatus::Active)
    }

    /// Drops a completed goal, which only j3 still shows. Returns whether
    /// there was one.
    pub fn clear_completed_goal(&mut self) -> bool {
        let complete = self
            .proj
            .goal()
            .is_some_and(|goal| goal.status == ProviderGoalStatus::Complete);
        if complete {
            self.set_goal(None);
        }
        complete
    }

    fn set_goal(&mut self, goal: Option<ProviderGoal>) {
        let pth = self.pth();
        if let Some(thread) = self.proj.provider_threads.iter_mut().find(|t| t.id == pth)
            && thread.goal != goal
        {
            thread.goal = goal;
            thread.updated_at = now();
            self.proj.touch_updated(Kind::ProviderThread, &pth);
        }
    }

    /// The omp process is gone: its running subagents ended with it, and omp
    /// restores an active goal paused when it starts again.
    pub fn session_closed(&mut self) {
        let running: Vec<String> = self
            .proj
            .subagents
            .iter()
            .filter(|subagent| subagent.status.is_active())
            .map(|subagent| subagent.id.clone())
            .collect();
        for id in running {
            self.update_subagent(&id, false, |subagent| {
                subagent.status = SubagentStatus::Interrupted;
                subagent.completed_at = Some(now());
            });
        }
        if let Some(mut goal) = self.proj.goal().cloned()
            && goal.status == ProviderGoalStatus::Active
        {
            goal.status = ProviderGoalStatus::Paused;
            self.set_goal(Some(goal));
        }
    }

    /// Left over from a previous omp process (see [`Self::session_closed`]).
    pub(super) fn has_live_agent_state(&self) -> bool {
        self.goal_active()
            || self
                .proj
                .subagents
                .iter()
                .any(|subagent| subagent.status.is_active())
    }

    // -----------------------------------------------------------------------
    // Subagents
    // -----------------------------------------------------------------------

    /// `subagent_lifecycle`: started (again, when revived), completed, failed
    /// or aborted.
    pub(super) fn apply_subagent_lifecycle(
        &mut self,
        payload: &SubagentLifecyclePayload,
        effects: &mut Vec<Effect>,
    ) {
        let status = match payload.status {
            SubagentLifecycleStatus::Started => SubagentStatus::Running,
            SubagentLifecycleStatus::Completed => SubagentStatus::Completed,
            SubagentLifecycleStatus::Failed => SubagentStatus::Failed,
            SubagentLifecycleStatus::Aborted => SubagentStatus::Cancelled,
        };
        if !self.ensure_subagent(
            &payload.id,
            &payload.agent,
            payload.description.as_deref(),
            None,
            payload.parent_tool_call_id.as_deref(),
        ) {
            return;
        }
        let finished = matches!(status, SubagentStatus::Completed | SubagentStatus::Failed);
        let output = if finished {
            self.subagent_output
                .get(&payload.id)
                .map(|lines| lines.join("\n"))
                .filter(|output| !output.trim().is_empty())
        } else {
            None
        };
        if status == SubagentStatus::Running {
            self.subagent_output.remove(&payload.id);
        }
        let title = payload
            .description
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned);
        self.update_subagent(&payload.id, false, |subagent| {
            subagent.status = status;
            if let Some(title) = title {
                subagent.title = Some(title);
            }
            if status.is_active() {
                subagent.result = None;
                subagent.completed_at = None;
            } else {
                subagent.completed_at = Some(now());
                if let Some(output) = output {
                    subagent.result = Some(output);
                }
            }
        });
        if finished {
            effects.push(Effect::FetchSubagentResult {
                subagent_id: payload.id.clone(),
            });
        }
    }

    /// `subagent_progress`: prompt, model, activity and counts.
    pub(super) fn apply_subagent_progress(&mut self, payload: &SubagentProgressPayload) {
        let progress = &payload.progress;
        let Some(id) = text(progress, "id") else {
            return;
        };
        let prompt = payload
            .assignment
            .as_deref()
            .unwrap_or(&payload.task)
            .trim()
            .to_owned();
        let description = text(progress, "description");
        if !self.ensure_subagent(
            id,
            &payload.agent,
            description,
            Some(&prompt),
            payload.parent_tool_call_id.as_deref(),
        ) {
            return;
        }
        if let Some(output) = progress.get("recentOutput").and_then(Value::as_array) {
            let lines = output
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            self.subagent_output.insert(id.to_owned(), lines);
        }
        let summary = progress_summary(progress);
        let model = text(progress, "resolvedModelIdentity")
            .or_else(|| text(progress, "resolvedModel"))
            .map(str::to_owned);
        let title = description.map(str::to_owned);
        let status = progress_status(progress);
        self.update_subagent(id, true, |subagent| {
            subagent.prompt = prompt;
            if title.is_some() {
                subagent.title = title;
            }
            if model.is_some() {
                subagent.model = model;
            }
            subagent.progress = summary;
            // Lifecycle frames own the end states; a stale progress record
            // never reopens a finished subagent.
            if let Some(status) = status
                && subagent.status.is_active()
            {
                subagent.status = status;
                if !status.is_active() {
                    subagent.completed_at = Some(now());
                }
            }
        });
    }

    /// `get_subagent_messages` of a finished subagent: its final answer.
    pub fn apply_subagent_transcript(&mut self, subagent_id: &str, transcript: &SubagentMessages) {
        let finished = self.proj.subagent(subagent_id).is_some_and(|subagent| {
            matches!(
                subagent.status,
                SubagentStatus::Completed | SubagentStatus::Failed
            )
        });
        if !finished {
            return;
        }
        if let Some(result) = final_text(&transcript.messages) {
            self.update_subagent(subagent_id, false, |subagent| {
                subagent.result = Some(result);
            });
        }
    }

    /// The subagent entity and its turn item, created on first sight in the
    /// live run (else the latest run). Returns false when there is no run to
    /// attach it to.
    fn ensure_subagent(
        &mut self,
        id: &str,
        agent: &str,
        description: Option<&str>,
        prompt: Option<&str>,
        parent_tool_call_id: Option<&str>,
    ) -> bool {
        if self.proj.subagent(id).is_some() {
            return true;
        }
        let (run_id, node_id) = match (&self.live, self.proj.latest_run()) {
            (Some(live), _) => (live.run_id.clone(), live.node_id.clone()),
            (None, Some(run)) => match &run.root_node_id {
                Some(node_id) => (run.id.clone(), node_id.clone()),
                None => return false,
            },
            (None, None) => return false,
        };
        let title = description
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(agent)
            .to_owned();
        let prompt = prompt.unwrap_or_default().to_owned();
        let at = now();
        self.proj.subagents.push(Subagent {
            id: id.to_owned(),
            thread_id: self.proj.thread.id.clone(),
            run_id: Some(run_id.clone()),
            parent_node_id: node_id.clone(),
            origin: SubagentOrigin::ProviderNative,
            created_by: Actor::Agent,
            driver: DRIVER.to_owned(),
            provider_instance_id: PROVIDER_INSTANCE.to_owned(),
            provider_thread_id: None,
            child_thread_id: None,
            remote_child: None,
            native_task_ref: Some(ProviderRef {
                driver: DRIVER.to_owned(),
                native_id: Some(id.to_owned()),
                strength: NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            prompt: prompt.clone(),
            title: Some(title.clone()),
            model: None,
            model_selection: None,
            completion_wake: None,
            completion_delivery: None,
            status: SubagentStatus::Pending,
            progress: None,
            workflow: None,
            result: None,
            started_at: Some(at.clone()),
            completed_at: None,
            updated_at: at,
            prompt_omitted: None,
            result_omitted: None,
        });
        self.proj.touch_updated(Kind::Subagent, id);
        let parent_item_id = parent_tool_call_id.and_then(|call_id| {
            self.live
                .as_ref()
                .and_then(|live| live.tools.get(call_id).cloned().flatten())
        });
        let item_id = self.push_item(
            Some(run_id),
            Some(node_id),
            TurnItemStatus::Pending,
            Some(title),
            TurnItemKind::Subagent(SubagentItem {
                subagent_id: id.to_owned(),
                origin: SubagentOrigin::ProviderNative,
                driver: DRIVER.to_owned(),
                provider_instance_id: PROVIDER_INSTANCE.to_owned(),
                child_thread_id: None,
                remote_child: None,
                prompt,
                progress: None,
                result: None,
                prompt_omitted: None,
                result_omitted: None,
            }),
        );
        if let Some(item) = self.proj.item_mut(&item_id) {
            item.parent_item_id = parent_item_id;
        }
        true
    }

    /// Updates a subagent and mirrors it onto its turn item. Progress-only
    /// changes (`streaming`, same status) may wait for the coalescing window.
    fn update_subagent(&mut self, id: &str, streaming: bool, update: impl FnOnce(&mut Subagent)) {
        let Some(subagent) = self.proj.subagent_mut(id) else {
            return;
        };
        let before = subagent.clone();
        update(subagent);
        if *subagent == before {
            return;
        }
        subagent.updated_at = now();
        let streaming = streaming && subagent.status == before.status;
        let snapshot = subagent.clone();
        if streaming {
            self.proj.touch_streaming(Kind::Subagent, id);
        } else {
            self.proj.touch_updated(Kind::Subagent, id);
        }
        let item_id = self
            .proj
            .items
            .iter()
            .rev()
            .find(
                |item| matches!(&item.kind, TurnItemKind::Subagent(body) if body.subagent_id == id),
            )
            .map(|item| item.id.clone());
        if let Some(item_id) = item_id {
            self.update_item(&item_id, streaming, |item| {
                item.status = item_status(snapshot.status);
                item.title = snapshot.title;
                item.completed_at = snapshot.completed_at;
                if let TurnItemKind::Subagent(body) = &mut item.kind {
                    body.prompt = snapshot.prompt;
                    body.progress = snapshot.progress;
                    body.result = snapshot.result;
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn progress_line() {
        let progress =
            json!({"currentToolIntent": "Reading the config", "toolCount": 3, "tokens": 14_230})
                .as_object()
                .cloned()
                .unwrap();
        assert_eq!(
            progress_summary(&progress).as_deref(),
            Some("Reading the config · 3 tool calls · 14.2k tokens")
        );
        let quiet = json!({"toolCount": 1, "tokens": 999})
            .as_object()
            .cloned()
            .unwrap();
        assert_eq!(
            progress_summary(&quiet).as_deref(),
            Some("1 tool call · 999 tokens")
        );
        assert_eq!(compact_count(2000), "2k");
        assert!(progress_summary(&Map::new()).is_none());
    }
}
