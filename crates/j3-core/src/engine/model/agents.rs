//! omp goal mode and subagents (wire.md §4.4 `goal`, `subagent`).
//!
//! - Goal: `get_state.goal`, `goal_updated` and `goal` command results set
//!   the thread row's `goal`; a change of objective or state also adds a
//!   `goal` item to the feed.
//! - Subagents: `subagent_lifecycle` / `subagent_progress` frames (omp's
//!   `progress` subscription level) and the `task` tool's result each keep
//!   one `subagent` item per subagent. Its result is the final assistant
//!   text of the subagent's transcript (`get_subagent_messages`), with its
//!   last output lines as the fallback.

use j3_protocol::v2::entities::{Goal, GoalState, ItemKind, ItemStatus, SubagentItem};
use serde_json::{Map, Value};

use super::{Effect, ThreadModel, truncate};
use crate::{
    omp::omp_rpc::{
        self, AgentMessage, AssistantContent, GoalModeState, SubagentLifecyclePayload,
        SubagentLifecycleStatus, SubagentMessages, SubagentProgressPayload,
    },
    v2::tools,
};

/// Longest activity text in a subagent's progress line.
const MAX_ACTIVITY: usize = 200;

/// omp's goal.
fn goal(goal: &omp_rpc::Goal) -> Option<Goal> {
    let objective = goal.objective.trim();
    if objective.is_empty() {
        return None;
    }
    Some(Goal {
        objective: objective.to_owned(),
        state: match goal.status {
            omp_rpc::GoalStatus::Active => GoalState::Active,
            omp_rpc::GoalStatus::Paused => GoalState::Paused,
            omp_rpc::GoalStatus::BudgetLimited => GoalState::BudgetLimited,
            omp_rpc::GoalStatus::Complete => GoalState::Complete,
            omp_rpc::GoalStatus::Dropped => GoalState::Dropped,
        },
        tokens_used: u64::try_from(goal.tokens_used).ok(),
        budget: goal
            .token_budget
            .and_then(|budget| u64::try_from(budget).ok()),
    })
}

fn text<'a>(progress: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    progress
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// `AgentProgress.status` → item status.
fn progress_status(progress: &Map<String, Value>) -> Option<ItemStatus> {
    Some(match text(progress, "status")? {
        "pending" | "running" => ItemStatus::Running,
        "completed" => ItemStatus::Completed,
        "failed" => ItemStatus::Failed,
        "aborted" => ItemStatus::Cancelled,
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
    pub fn apply_goal(&mut self, update: Option<&omp_rpc::Goal>) {
        self.set_goal(update.and_then(goal));
    }

    /// `get_state.goal`. omp forgets a goal once it is complete, so a
    /// completed goal stays until another replaces it or it is dropped.
    pub fn apply_goal_state(&mut self, state: Option<&GoalModeState>) {
        match state {
            Some(state) => self.apply_goal(Some(&state.goal)),
            None if self.goal_is(GoalState::Complete) => {}
            None => self.set_goal(None),
        }
    }

    fn goal_is(&self, state: GoalState) -> bool {
        self.thread
            .goal
            .as_ref()
            .is_some_and(|goal| goal.state == state)
    }

    /// The thread pursues an active goal; omp must stay up for it.
    pub fn goal_active(&self) -> bool {
        self.goal_is(GoalState::Active)
    }

    /// Drops a completed goal, which only j3 still shows. Returns whether
    /// there was one.
    pub fn clear_completed_goal(&mut self) -> bool {
        let complete = self.goal_is(GoalState::Complete);
        if complete {
            self.set_goal(None);
        }
        complete
    }

    /// Sets the row's goal (a dropped goal leaves none) and records a goal
    /// item when its objective or state changed.
    fn set_goal(&mut self, goal: Option<Goal>) {
        let shown = goal.filter(|goal| goal.state != GoalState::Dropped);
        let item = match (&self.thread.goal, &shown) {
            (Some(old), Some(new)) if old.objective == new.objective && old.state == new.state => {
                None
            }
            (_, Some(new)) => Some(new.clone()),
            (Some(old), None) => Some(Goal {
                state: GoalState::Dropped,
                ..old.clone()
            }),
            (None, None) => None,
        };
        if self.thread.goal != shown {
            self.thread.goal = shown;
            self.touch_row();
        }
        if let Some(item) = item
            && let Some(turn_id) = self.feed_turn()
        {
            self.push_item(&turn_id, ItemStatus::Completed, ItemKind::Goal(item));
        }
    }

    /// The turn new agent items belong to: the live one, else the latest.
    fn feed_turn(&self) -> Option<String> {
        self.live
            .as_ref()
            .map(|live| live.turn_id.clone())
            .or_else(|| self.turns.last().map(|turn| turn.id.clone()))
    }

    /// The omp process is gone: its running subagents ended with it, and
    /// omp restores an active goal paused when it starts again.
    pub fn session_closed(&mut self) {
        let running: Vec<String> = self
            .items
            .iter()
            .filter(|item| {
                matches!(item.kind, ItemKind::Subagent(_)) && item.status == ItemStatus::Running
            })
            .map(|item| item.id.clone())
            .collect();
        for id in running {
            self.update_item(&id, true, |item| item.status = ItemStatus::Cancelled);
        }
        if let Some(mut goal) = self.thread.goal.clone()
            && goal.state == GoalState::Active
        {
            goal.state = GoalState::Paused;
            self.set_goal(Some(goal));
        }
    }

    /// Left over from an earlier omp process (see [`Self::session_closed`]).
    pub(super) fn has_live_agent_state(&self) -> bool {
        self.goal_active()
            || self.items.iter().any(|item| {
                matches!(item.kind, ItemKind::Subagent(_)) && item.status == ItemStatus::Running
            })
    }

    // -----------------------------------------------------------------------
    // Subagents
    // -----------------------------------------------------------------------

    fn subagent_item(&self, subagent_id: &str) -> Option<String> {
        self.items.iter().rev().find_map(|item| match &item.kind {
            ItemKind::Subagent(body) if body.subagent_id == subagent_id => Some(item.id.clone()),
            _ => None,
        })
    }

    /// The item of subagent `subagent.subagent_id`, created on first sight
    /// in the live turn (else the latest). None without a turn.
    fn ensure_subagent(&mut self, subagent: SubagentItem) -> Option<String> {
        if let Some(id) = self.subagent_item(&subagent.subagent_id) {
            return Some(id);
        }
        let turn_id = self.feed_turn()?;
        Some(self.push_item(&turn_id, ItemStatus::Running, ItemKind::Subagent(subagent)))
    }

    /// The subagents a `task` call spawned (its result's `details.progress`).
    pub(super) fn spawned_subagents(&mut self, result: &Value) {
        for subagent in tools::spawned_subagents(result) {
            self.ensure_subagent(subagent);
        }
    }

    /// `subagent_lifecycle`: started (again, when revived), completed,
    /// failed or aborted.
    pub(super) fn apply_subagent_lifecycle(
        &mut self,
        payload: &SubagentLifecyclePayload,
        effects: &mut Vec<Effect>,
    ) {
        let status = match payload.status {
            SubagentLifecycleStatus::Started => ItemStatus::Running,
            SubagentLifecycleStatus::Completed => ItemStatus::Completed,
            SubagentLifecycleStatus::Failed => ItemStatus::Failed,
            SubagentLifecycleStatus::Aborted => ItemStatus::Cancelled,
        };
        let Some(item_id) = self.ensure_subagent(SubagentItem {
            subagent_id: payload.id.clone(),
            agent: payload.agent.clone(),
            description: payload
                .description
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            prompt: String::new(),
            model: None,
            progress: None,
            result: None,
        }) else {
            return;
        };
        let finished = matches!(status, ItemStatus::Completed | ItemStatus::Failed);
        let output = if finished {
            self.subagent_output
                .get(&payload.id)
                .map(|lines| lines.join("\n"))
                .filter(|output| !output.trim().is_empty())
        } else {
            None
        };
        if status == ItemStatus::Running {
            self.subagent_output.remove(&payload.id);
        }
        let agent = payload.agent.trim().to_owned();
        let description = payload
            .description
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned);
        self.update_item(&item_id, true, |item| {
            item.status = status;
            if status == ItemStatus::Running {
                item.ended_at = None;
            }
            if let ItemKind::Subagent(body) = &mut item.kind {
                if !agent.is_empty() {
                    body.agent = agent;
                }
                if description.is_some() {
                    body.description = description;
                }
                if status == ItemStatus::Running {
                    body.result = None;
                } else if output.is_some() {
                    body.result = output;
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
        let Some(item_id) = self.ensure_subagent(SubagentItem {
            subagent_id: id.to_owned(),
            agent: payload.agent.clone(),
            description: text(progress, "description").map(str::to_owned),
            prompt: prompt.clone(),
            model: None,
            progress: None,
            result: None,
        }) else {
            return;
        };
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
        let description = text(progress, "description").map(str::to_owned);
        let status = progress_status(progress);
        let before = self.item(&item_id).map(|item| item.status);
        // Lifecycle frames own the end states; a stale progress record
        // never reopens a finished subagent.
        let status = status.filter(|_| before == Some(ItemStatus::Running));
        let urgent = status.is_some_and(|status| Some(status) != before);
        self.update_item(&item_id, urgent, |item| {
            if let Some(status) = status {
                item.status = status;
            }
            if let ItemKind::Subagent(body) = &mut item.kind {
                if !prompt.is_empty() {
                    body.prompt = prompt;
                }
                if description.is_some() {
                    body.description = description;
                }
                if model.is_some() {
                    body.model = model;
                }
                body.progress = summary;
            }
        });
    }

    /// `get_subagent_messages` of a finished subagent: its final answer.
    pub fn apply_subagent_transcript(&mut self, subagent_id: &str, transcript: &SubagentMessages) {
        let Some(item_id) = self.subagent_item(subagent_id) else {
            return;
        };
        let finished = self
            .item(&item_id)
            .is_some_and(|item| matches!(item.status, ItemStatus::Completed | ItemStatus::Failed));
        if let (true, Some(result)) = (finished, final_text(&transcript.messages)) {
            self.update_item(&item_id, true, |item| {
                if let ItemKind::Subagent(body) = &mut item.kind {
                    body.result = Some(result);
                }
            });
        }
    }

    /// Whether subagent `subagent_id` is still running.
    pub fn subagent_running(&self, subagent_id: &str) -> Option<bool> {
        let item_id = self.subagent_item(subagent_id)?;
        self.item(&item_id)
            .map(|item| item.status == ItemStatus::Running)
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
