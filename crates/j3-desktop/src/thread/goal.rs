//! The goal chip above the composer and the "Set goal" dialog (spec §5.2, item 4).
//!
//! The chip shows omp's goal for the thread: its objective, status and usage, with Pause
//! (while active), Resume (paused or out of budget) and Drop (Clear once complete). A goal is
//! created from the header's actions menu, "Set goal…", which opens a dialog drawn here into
//! the overlay layer: an objective and an optional token budget.

use j3_client::t3::orchestration::{ProviderGoal, ProviderGoalStatus};
use rune::{El, TextEdit, Tx, col, icon, row, spacer, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Dialog, Kit, TextInput};

use super::chrome_edit;
use crate::{
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
    theme::Status,
};

/// A goal button's command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalCommand {
    Pause,
    Resume,
    Drop,
}

/// The "Set goal" dialog's form.
pub struct GoalDialog {
    pub objective: TextEdit,
    pub budget: TextEdit,
    /// The last confirm found no objective.
    pub objective_missing: bool,
    /// The last confirm found a budget that isn't a number.
    pub budget_invalid: bool,
}

/// Which field of the goal dialog a `TextInput` edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoalField {
    Objective,
    Budget,
}

impl J3 {
    /// Runs a goal button's command on `key`'s thread.
    pub fn goal_command(&mut self, key: &ThreadKey, command: GoalCommand) {
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let thread_id = key.thread_id.clone();
        let what = match command {
            GoalCommand::Pause => "pause the goal",
            GoalCommand::Resume => "resume the goal",
            GoalCommand::Drop => "drop the goal",
        };
        self.run_command(
            what,
            async move {
                match command {
                    GoalCommand::Pause => connection.pause_goal(&thread_id).await,
                    GoalCommand::Resume => connection.resume_goal(&thread_id).await,
                    GoalCommand::Drop => connection.drop_goal(&thread_id).await,
                }
            },
            |_, _, _| {},
        );
    }

    /// Opens the "Set goal" dialog for `key`'s thread (closing any other dialog).
    pub fn open_goal_dialog(&mut self, key: &ThreadKey) {
        self.close_dialog();
        let Some(thread) = self.threads.get_mut(key) else {
            return;
        };
        let mut objective = TextEdit::new("");
        objective.focused = true;
        thread.chrome.goal_dialog = Some(GoalDialog {
            objective,
            budget: TextEdit::new(""),
            objective_missing: false,
            budget_invalid: false,
        });
    }

    pub fn close_goal_dialog(&mut self, key: &ThreadKey) {
        if let Some(thread) = self.threads.get_mut(key) {
            thread.chrome.goal_dialog = None;
        }
    }

    /// "Set Goal": creates the goal (`connection.create_goal`) and closes the dialog, or
    /// marks what's missing: an objective, or a budget that is a whole number.
    pub fn confirm_goal_dialog(&mut self, key: &ThreadKey) {
        let Some(dialog) = self
            .threads
            .get_mut(key)
            .and_then(|thread| thread.chrome.goal_dialog.as_mut())
        else {
            return;
        };
        let objective = dialog.objective.text.trim().to_owned();
        let budget = parse_budget(&dialog.budget.text);
        dialog.objective_missing = objective.is_empty();
        dialog.budget_invalid = budget.is_err();
        let (false, Ok(budget)) = (objective.is_empty(), budget) else {
            return;
        };
        self.close_goal_dialog(key);
        let Some(connection) = self.thread_connection(key) else {
            return;
        };
        let thread_id = key.thread_id.clone();
        self.run_command(
            "set the goal",
            async move { connection.create_goal(&thread_id, &objective, budget).await },
            |_, _, _| {},
        );
    }
}

/// The token budget typed in the goal dialog: `,`, `_` and spaces are ignored; empty means
/// none.
pub fn parse_budget(text: &str) -> Result<Option<u64>, std::num::ParseIntError> {
    let digits: String = text
        .chars()
        .filter(|c| !matches!(c, ',' | '_' | ' '))
        .collect();
    if digits.is_empty() {
        Ok(None)
    } else {
        digits.parse().map(Some)
    }
}

/// A token count like `950`, `12.3k` or `1.2M`.
pub fn format_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.),
    }
}

/// What the goal chip says about usage: tokens of budget and time.
pub fn goal_usage(goal: &ProviderGoal) -> String {
    let mut parts = Vec::new();
    match (goal.tokens_used, goal.token_budget.flatten()) {
        (Some(used), Some(budget)) => parts.push(format!(
            "{} / {} tokens",
            format_tokens(used),
            format_tokens(budget)
        )),
        (Some(used), None) => parts.push(format!("{} tokens", format_tokens(used))),
        (None, Some(budget)) => parts.push(format!("budget {} tokens", format_tokens(budget))),
        (None, None) => {}
    }
    if let Some(seconds) = goal.time_used_seconds.filter(|s| *s > 0) {
        parts.push(if seconds < 60 {
            format!("{seconds}s")
        } else {
            format!("{}m", seconds / 60)
        });
    }
    parts.join(" · ")
}

pub fn status_label(status: ProviderGoalStatus) -> &'static str {
    match status {
        ProviderGoalStatus::Active => "Active",
        ProviderGoalStatus::Paused => "Paused",
        ProviderGoalStatus::Blocked => "Blocked",
        ProviderGoalStatus::UsageLimited => "Usage limited",
        ProviderGoalStatus::BudgetLimited => "Budget reached",
        ProviderGoalStatus::Complete => "Complete",
    }
}

/// The goal chip: star, objective, status, usage and the commands the status allows.
pub fn chip(ui: &Ui, key: &ThreadKey, goal: &ProviderGoal) -> El {
    let k = ui.k;
    let status = goal.status;
    let palette = Status::of(k);
    let color = match status {
        ProviderGoalStatus::Active => k.c.icon_accent,
        ProviderGoalStatus::Complete => palette.success,
        ProviderGoalStatus::Blocked
        | ProviderGoalStatus::UsageLimited
        | ProviderGoalStatus::BudgetLimited => palette.warning,
        ProviderGoalStatus::Paused => palette.muted,
    };
    let command = |id: &str, label: &str, command: GoalCommand| {
        let key = key.clone();
        Button::new(label)
            .variant(ButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .on_press(move |s: &mut J3| s.goal_command(&key, command))
            .build(k)
            .key(id.to_owned())
    };
    let usage = goal_usage(goal);
    let mut chip = row()
        .key("goal")
        .label(format!("{}, {}", goal.objective, status_label(status)))
        .w_full()
        .gap(8.0)
        .px(12.0)
        .py(6.0)
        .center_y()
        .radius(k.radius.lg)
        .border(1.0, k.c.border_muted)
        .child(icon(&icons::STAR, 12.0).color(color))
        .child(
            text(&goal.objective)
                .font(k.ty.text_small)
                .color(k.c.text_base)
                .truncate()
                .grow(1.0)
                .min_w(0.0),
        )
        .child(
            text(status_label(status))
                .font(k.ty.text_xsmall)
                .color(color),
        );
    if !usage.is_empty() {
        chip = chip.child(text(usage).font(k.ty.text_xsmall).color(k.c.text_muted));
    }
    if status == ProviderGoalStatus::Active {
        chip = chip.child(command("goal-pause", "Pause", GoalCommand::Pause));
    }
    if matches!(
        status,
        ProviderGoalStatus::Paused | ProviderGoalStatus::BudgetLimited
    ) {
        chip = chip.child(command("goal-resume", "Resume", GoalCommand::Resume));
    }
    let drop = if status == ProviderGoalStatus::Complete {
        "Clear"
    } else {
        "Drop"
    };
    chip.child(command("goal-drop", drop, GoalCommand::Drop))
}

/// The "Set goal" dialog of `key`'s thread into the overlay layer, while open.
pub fn dialog(ui: &mut Ui, key: &ThreadKey, dialog: &GoalDialog) {
    let k = ui.k;
    let form = col()
        .key("goal-form")
        .px(16.0)
        .gap(12.0)
        .child(field(
            k,
            key,
            "Objective",
            &dialog.objective,
            GoalField::Objective,
            "What should omp keep working towards?",
            dialog.objective_missing,
        ))
        .child(field(
            k,
            key,
            "Token budget",
            &dialog.budget,
            GoalField::Budget,
            "No limit",
            dialog.budget_invalid,
        ));
    let (cancel_key, confirm_key, close_key) = (key.clone(), key.clone(), key.clone());
    let footer = row()
        .key("dialog-footer")
        .w_full()
        .gap(8.0)
        .padding(16.0)
        .child(spacer())
        .child(
            Button::new("Cancel")
                .variant(ButtonVariant::Ghost)
                .on_press(move |s: &mut J3| s.close_goal_dialog(&cancel_key))
                .build(k)
                .key("dialog-cancel"),
        )
        .child(
            Button::new("Set Goal")
                .variant(ButtonVariant::Contrast)
                .on_press(move |s: &mut J3| s.confirm_goal_dialog(&confirm_key))
                .build(k)
                .key("dialog-confirm"),
        );
    Dialog::new("set-goal", "Set goal")
        .open(true)
        .fit()
        .width(400.0)
        .on_close(move |s: &mut J3| s.close_goal_dialog(&close_key))
        .child(col().child(form).child(footer))
        .build(k, ui.layer);
}

/// A labelled field of the goal dialog; Enter confirms.
fn field(
    k: &Kit,
    key: &ThreadKey,
    label: &str,
    edit: &TextEdit,
    which: GoalField,
    placeholder: &str,
    invalid: bool,
) -> El {
    let (edit_key, submit_key) = (key.clone(), key.clone());
    let id = match which {
        GoalField::Objective => "goal-objective",
        GoalField::Budget => "goal-budget",
    };
    col()
        .gap(4.0)
        .child(
            text(label)
                .font(k.ty.text_small_medium)
                .color(k.c.text_base),
        )
        .child(
            TextInput::new(edit)
                .width(None)
                .placeholder(placeholder)
                .label(label)
                .invalid(invalid)
                .on_edit(move |s: &mut J3| {
                    chrome_edit(s, &edit_key, |chrome| {
                        let dialog = chrome.goal_dialog.as_mut()?;
                        Some(match which {
                            GoalField::Objective => &mut dialog.objective,
                            GoalField::Budget => &mut dialog.budget,
                        })
                    })
                })
                .on_submit(move |s: &mut J3, _: &mut Tx| s.confirm_goal_dialog(&submit_key))
                .build(k)
                .key(id),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_ignore_separators_and_empty_means_none() {
        assert_eq!(parse_budget(""), Ok(None));
        assert_eq!(parse_budget(" "), Ok(None));
        assert_eq!(parse_budget("200,000"), Ok(Some(200_000)));
        assert_eq!(parse_budget("1_000 000"), Ok(Some(1_000_000)));
        assert!(parse_budget("lots").is_err());
        assert!(parse_budget("-5").is_err());
    }

    #[test]
    fn usage_reads_tokens_budget_and_time() {
        let goal = |used, budget, seconds| ProviderGoal {
            tokens_used: used,
            token_budget: Some(budget),
            time_used_seconds: seconds,
            ..serde_json::from_value(serde_json::json!({"objective": "o", "status": "active"}))
                .unwrap()
        };
        assert_eq!(
            goal_usage(&goal(Some(48_210), Some(200_000), Some(1_260))),
            "48.2k / 200.0k tokens · 21m"
        );
        assert_eq!(
            goal_usage(&goal(Some(950), None, Some(42))),
            "950 tokens · 42s"
        );
        assert_eq!(
            goal_usage(&goal(None, Some(1_500_000), None)),
            "budget 1.5M tokens"
        );
        assert_eq!(goal_usage(&goal(None, None, Some(0))), "");
    }
}
