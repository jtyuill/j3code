//! "Custom snooze" (spec §8.4, `docs/triage.md` §7.1 Custom…): a duration from now (amount,
//! default 1, of minutes, hours or days) or a local date and time (`YYYY-MM-DD`, `HH:MM`,
//! default 09:00), resolved in the system time zone. A time that has passed or doesn't parse
//! is refused with a line under the form.

use j3_client::inbox::{CustomSnooze, SnoozeUnit, TimeZone, custom_snooze};
use rune::{El, TextEdit, Tx, col, row, text};
use rune_kit::{Kit, TextInput};
use time::OffsetDateTime;

use super::{Dialog, Field};
use crate::{
    app::J3,
    settings::controls::choice,
    sidebar::{ThreadCommand, ThreadKey},
    theme::Status,
};

/// The refusal under the form.
pub const INVALID: &str = "That snooze time has passed or isn’t valid. Choose another time.";

/// How a custom snooze is given: a duration or a date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    After,
    At,
}

const SCHEDULES: [(Schedule, &str); 2] = [
    (Schedule::After, "Duration"),
    (Schedule::At, "Date and time"),
];
const UNITS: [(SnoozeUnit, &str); 3] = [
    (SnoozeUnit::Minutes, "Minutes"),
    (SnoozeUnit::Hours, "Hours"),
    (SnoozeUnit::Days, "Days"),
];

/// The Custom snooze dialog's form, for thread `key`.
pub struct SnoozeForm {
    pub key: ThreadKey,
    pub schedule: Schedule,
    pub amount: TextEdit,
    pub unit: SnoozeUnit,
    pub date: TextEdit,
    pub time: TextEdit,
    pub error: Option<String>,
}

impl SnoozeForm {
    pub fn new(key: ThreadKey) -> Self {
        let mut amount = TextEdit::new("1");
        amount.focused = true;
        amount.set_selection(0, amount.text.len());
        Self {
            key,
            schedule: Schedule::After,
            amount,
            unit: SnoozeUnit::Hours,
            date: TextEdit::new(""),
            time: TextEdit::new("09:00"),
            error: None,
        }
    }

    /// The wake time the form gives at `now` in `zone`, or `None` when it has passed or
    /// doesn't parse.
    pub fn wake(&self, now: OffsetDateTime, zone: &TimeZone) -> Option<OffsetDateTime> {
        let input = match self.schedule {
            Schedule::At => CustomSnooze::At {
                date: self.date.text.trim(),
                time: self.time.text.trim(),
            },
            Schedule::After => CustomSnooze::After {
                amount: self.amount.text.trim(),
                unit: self.unit,
            },
        };
        custom_snooze(input, now, zone)
    }

    /// Focuses the first field of the chosen schedule.
    fn focus(&mut self) {
        match self.schedule {
            Schedule::After => self.amount.focused = true,
            Schedule::At => self.date.focused = true,
        }
    }
}

impl J3 {
    fn snooze_form(&mut self) -> Option<&mut SnoozeForm> {
        match &mut self.dialogs.open {
            Some(Dialog::CustomSnooze(form)) => Some(form),
            _ => None,
        }
    }

    /// Snoozes the form's thread until the time it gives, or says why it can't.
    pub(super) fn confirm_snooze(&mut self) {
        let now = self.clock.now;
        let Some(form) = self.snooze_form() else {
            return;
        };
        match form.wake(now, &TimeZone::system()) {
            Some(wake) => {
                let key = form.key.clone();
                self.close_dialog();
                self.apply_thread_commands(vec![(key, ThreadCommand::Snooze(wake))]);
            }
            None => form.error = Some(INVALID.to_owned()),
        }
    }
}

/// The form's body.
pub(super) fn body(k: &Kit, form: &SnoozeForm) -> El {
    let schedule = SCHEDULES.iter().position(|(s, _)| *s == form.schedule);
    let labels = SCHEDULES.map(|(_, label)| label);
    let field = |title: &str, edit: &TextEdit, which: Field, placeholder: &str, key: &str| {
        col()
            .gap(6.0)
            .grow(1.0)
            .child(
                text(title.to_owned())
                    .font(k.ty.text_small_medium)
                    .color(k.c.text_base),
            )
            .child(
                TextInput::new(edit)
                    .width(None)
                    .placeholder(placeholder)
                    .label(title)
                    .on_edit(move |s: &mut J3| s.dialogs.field(which))
                    .on_submit(|s: &mut J3, tx: &mut Tx| s.confirm_dialog(tx))
                    .build(k)
                    .key(key.to_owned()),
            )
    };
    let mut column = col()
        .key("snooze-form")
        .w_full()
        .gap(12.0)
        .child(
            text("Choose when the thread returns.")
                .font(k.ty.text_small)
                .color(k.c.text_muted),
        )
        .child(row().child(choice(
            k,
            "snooze-schedule",
            &labels,
            schedule,
            |s, ix, _| {
                if let Some(form) = s.snooze_form() {
                    form.schedule = SCHEDULES[ix].0;
                    form.error = None;
                    form.focus();
                }
            },
        )));
    column = match form.schedule {
        Schedule::At => column.child(
            row()
                .w_full()
                .gap(8.0)
                .child(field(
                    "Date",
                    &form.date,
                    Field::SnoozeDate,
                    "YYYY-MM-DD",
                    "snooze-date",
                ))
                .child(field(
                    "Time",
                    &form.time,
                    Field::SnoozeTime,
                    "HH:MM",
                    "snooze-time",
                )),
        ),
        Schedule::After => {
            let unit = UNITS.iter().position(|(u, _)| *u == form.unit);
            let labels = UNITS.map(|(_, label)| label);
            column
                .child(field(
                    "Amount",
                    &form.amount,
                    Field::SnoozeAmount,
                    "1",
                    "snooze-amount",
                ))
                .child(
                    row().child(choice(k, "snooze-unit", &labels, unit, |s, ix, _| {
                        if let Some(form) = s.snooze_form() {
                            form.unit = UNITS[ix].0;
                            form.error = None;
                            form.focus();
                        }
                    })),
                )
        }
    };
    if let Some(error) = &form.error {
        column = column.child(
            text(error.clone())
                .key("dialog-error")
                .font(k.ty.text_xsmall)
                .color(Status::of(k).danger)
                .wrap(),
        );
    }
    column
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn the_form_starts_at_one_hour_and_refuses_past_or_garbled_times() {
        let now = datetime!(2026-11-07 16:30 UTC);
        let utc = TimeZone::UTC;
        let mut form = SnoozeForm::new(ThreadKey::new("local", "t"));
        assert_eq!(form.wake(now, &utc), Some(now + time::Duration::hours(1)));
        form.unit = SnoozeUnit::Minutes;
        form.amount.replace_text("90");
        assert_eq!(
            form.wake(now, &utc),
            Some(now + time::Duration::minutes(90))
        );
        form.amount.replace_text("soon");
        assert_eq!(form.wake(now, &utc), None);
        form.schedule = Schedule::At;
        form.date.replace_text("2026-11-08");
        assert_eq!(form.time.text, "09:00");
        assert_eq!(form.wake(now, &utc), Some(datetime!(2026-11-08 09:00 UTC)));
        form.date.replace_text("2026-11-01");
        assert_eq!(form.wake(now, &utc), None);
    }
}
