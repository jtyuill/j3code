//! Snooze menus: t3's preset settings resolved to concrete wake times in the
//! user's time zone, and the custom snooze dialog (`threadSettled.ts:174-369`,
//! `DEFAULT_SNOOZE_PRESETS` in `packages/contracts/src/settings.ts:117-123`).
//!
//! Set times use calendar days in the local zone, not 24-hour steps, so a
//! wake lands on the right day across DST changes (threadSettled.ts:202-209).
//! Times read like `toLocaleTimeString` in an English locale ("6:00 PM").

use jiff::{
    Timestamp, Zoned,
    civil::{Date, Weekday},
    tz::{AmbiguousOffset, TimeZone},
};
use time::OffsetDateTime;

use super::view::now_ms;

/// The day of an `at` preset (`SnoozePresetDay`, settings.ts:89-99).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnoozeDay {
    Today,
    Tomorrow,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl SnoozeDay {
    fn name(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Tomorrow => "tomorrow",
            Self::Monday => "monday",
            Self::Tuesday => "tuesday",
            Self::Wednesday => "wednesday",
            Self::Thursday => "thursday",
            Self::Friday => "friday",
            Self::Saturday => "saturday",
            Self::Sunday => "sunday",
        }
    }

    /// `WEEKDAY_INDEX` (threadSettled.ts:176-184): Sunday is 0.
    fn weekday_index(self) -> Option<i8> {
        Some(match self {
            Self::Sunday => 0,
            Self::Monday => 1,
            Self::Tuesday => 2,
            Self::Wednesday => 3,
            Self::Thursday => 4,
            Self::Friday => 5,
            Self::Saturday => 6,
            Self::Today | Self::Tomorrow => return None,
        })
    }
}

/// One snooze menu setting (`SnoozePresetSetting`, settings.ts:101-112).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnoozePresetSetting {
    /// A fixed time from now.
    After { minutes: u32 },
    /// A local time of day; a weekday is its next occurrence after today.
    At {
        day: SnoozeDay,
        hour: u8,
        minute: u8,
    },
}

impl SnoozePresetSetting {
    /// `after:60`, `at:tomorrow:09:00` (`snoozePresetId`, threadSettled.ts:247-251).
    pub fn id(&self) -> String {
        match *self {
            Self::After { minutes } => format!("after:{minutes}"),
            Self::At { day, hour, minute } => format!("at:{}:{hour:02}:{minute:02}", day.name()),
        }
    }
}

/// t3's default menu: in 1 hour, in 3 hours, this evening 18:00, tomorrow
/// 09:00, next week Monday 09:00.
pub const DEFAULT_SNOOZE_PRESETS: [SnoozePresetSetting; 5] = [
    SnoozePresetSetting::After { minutes: 60 },
    SnoozePresetSetting::After { minutes: 180 },
    SnoozePresetSetting::At {
        day: SnoozeDay::Today,
        hour: 18,
        minute: 0,
    },
    SnoozePresetSetting::At {
        day: SnoozeDay::Tomorrow,
        hour: 9,
        minute: 0,
    },
    SnoozePresetSetting::At {
        day: SnoozeDay::Monday,
        hour: 9,
        minute: 0,
    },
];

/// A resolved menu row (`SnoozePreset`, threadSettled.ts:186-191).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnoozePreset {
    /// The setting's id; pass it to [`pick_snooze_preset`].
    pub id: String,
    /// "In 3 hours", "This evening", "Tomorrow", "Next week", "Friday"…
    pub label: String,
    /// The wake time, with a weekday when the label does not name the day:
    /// "6:00 PM", "Thu 2:00 AM".
    pub when_label: String,
    pub snoozed_until: OffsetDateTime,
}

fn zoned(at: OffsetDateTime, zone: &TimeZone) -> Zoned {
    let nanos = at.unix_timestamp_nanos();
    Timestamp::from_nanosecond(nanos)
        .unwrap_or(if nanos < 0 {
            Timestamp::MIN
        } else {
            Timestamp::MAX
        })
        .to_zoned(zone.clone())
}

fn offset_date_time(at: &Zoned) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(at.timestamp().as_nanosecond()).ok()
}

/// `toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })`.
fn time_of_day_label(at: &Zoned) -> String {
    at.strftime("%-I:%M %p").to_string()
}

fn pluralize(count: u32, unit: &str) -> String {
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

/// "3 hours", "90 minutes", "2 days" (`formatSnoozeDuration`, threadSettled.ts:215-220).
fn duration_label(minutes: u32) -> String {
    if minutes.is_multiple_of(24 * 60) {
        pluralize(minutes / (24 * 60), "day")
    } else if minutes.is_multiple_of(60) {
        pluralize(minutes / 60, "hour")
    } else {
        pluralize(minutes, "minute")
    }
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn sunday_index(weekday: Weekday) -> i8 {
    weekday.to_sunday_zero_offset()
}

/// `snoozePresetDaysAhead` (threadSettled.ts:226-233).
fn days_ahead(day: SnoozeDay, now: &Zoned) -> i8 {
    match day {
        SnoozeDay::Today => 0,
        SnoozeDay::Tomorrow => 1,
        _ => {
            let target = day.weekday_index().unwrap_or(0);
            match (target - sunday_index(now.weekday()) + 7) % 7 {
                0 => 7,
                ahead => ahead,
            }
        }
    }
}

/// Labels follow the actual day gap, so a Monday option on a Sunday reads
/// "Tomorrow" (`snoozePresetLabel`, threadSettled.ts:235-245).
fn label(setting: SnoozePresetSetting, days_ahead: i8) -> String {
    match setting {
        SnoozePresetSetting::After { minutes } => format!("In {}", duration_label(minutes)),
        SnoozePresetSetting::At { hour, .. } if days_ahead == 0 => match hour {
            0..12 => "This morning",
            12..17 => "This afternoon",
            _ => "This evening",
        }
        .to_owned(),
        SnoozePresetSetting::At { .. } if days_ahead == 1 => "Tomorrow".to_owned(),
        SnoozePresetSetting::At {
            day: SnoozeDay::Monday,
            ..
        } => "Next week".to_owned(),
        SnoozePresetSetting::At { day, .. } if days_ahead == 7 => {
            format!("Next {}", capitalize(day.name()))
        }
        SnoozePresetSetting::At { day, .. } => capitalize(day.name()),
    }
}

/// `resolveSnoozePreset` (threadSettled.ts:253-282); `None` for a set time
/// already past today.
fn resolve_preset(
    setting: SnoozePresetSetting,
    now: &Zoned,
    format_time: fn(&Zoned) -> String,
) -> Option<SnoozePreset> {
    let (wake, days_ahead) = match setting {
        SnoozePresetSetting::After { minutes } => {
            let wake = now.timestamp() + jiff::SignedDuration::from_mins(i64::from(minutes));
            (wake.to_zoned(now.time_zone().clone()), 0)
        }
        SnoozePresetSetting::At { day, hour, minute } => {
            let days_ahead = days_ahead(day, now);
            let date = now
                .date()
                .checked_add(jiff::Span::new().days(days_ahead))
                .ok()?;
            let wake = date
                .at(i8::try_from(hour).ok()?, i8::try_from(minute).ok()?, 0, 0)
                .to_zoned(now.time_zone().clone())
                .ok()?;
            // A today option whose time has passed is left out until tomorrow.
            if wake.timestamp() <= now.timestamp() {
                return None;
            }
            (wake, days_ahead)
        }
    };
    let label = label(setting, days_ahead);
    // Durations and "Next week" don't name the wake day, so a wake on another day shows it.
    let label_names_day = matches!(setting, SnoozePresetSetting::At { .. }) && label != "Next week";
    let time = format_time(&wake);
    let when_label = if label_names_day || wake.date() == now.date() {
        time
    } else {
        format!("{} {time}", wake.strftime("%a"))
    };
    Some(SnoozePreset {
        id: setting.id(),
        label,
        when_label,
        snoozed_until: offset_date_time(&wake)?,
    })
}

fn resolve_all(
    settings: &[SnoozePresetSetting],
    now: OffsetDateTime,
    zone: &TimeZone,
    format_time: fn(&Zoned) -> String,
) -> Vec<SnoozePreset> {
    let now = zoned(now, zone);
    // By minute, so a duration (which keeps the clock's seconds) and a set
    // time in the same minute tie; the stable sort keeps the first setting.
    let minute = |preset: &SnoozePreset| now_ms(preset.snoozed_until).div_euclid(60_000);
    let mut presets: Vec<SnoozePreset> = settings
        .iter()
        .filter_map(|setting| resolve_preset(*setting, &now, format_time))
        .collect();
    presets.sort_by_key(minute);
    presets.dedup_by(|later, earlier| minute(later) == minute(earlier));
    presets
}

/// The default snooze menu at `now` in `zone`: soonest first, one row per
/// wake minute (`resolveSnoozePresets`, threadSettled.ts:284-304).
pub fn resolve_snooze_presets(now: OffsetDateTime, zone: &TimeZone) -> Vec<SnoozePreset> {
    resolve_snooze_presets_with(&DEFAULT_SNOOZE_PRESETS, now, zone)
}

/// [`resolve_snooze_presets`] for custom settings.
pub fn resolve_snooze_presets_with(
    settings: &[SnoozePresetSetting],
    now: OffsetDateTime,
    zone: &TimeZone,
) -> Vec<SnoozePreset> {
    resolve_all(settings, now, zone, time_of_day_label)
}

/// The wake for a menu row the user picked at `now`. A duration starts at
/// the pick (even when the menu merged it with a set time); a set time keeps
/// the wake it showed, if still ahead (`pickSnoozePreset`,
/// threadSettled.ts:306-324).
pub fn pick_snooze_preset(
    shown: &[SnoozePreset],
    id: &str,
    now: OffsetDateTime,
    zone: &TimeZone,
) -> Option<SnoozePreset> {
    pick_snooze_preset_with(&DEFAULT_SNOOZE_PRESETS, shown, id, now, zone)
}

/// [`pick_snooze_preset`] for custom settings.
pub fn pick_snooze_preset_with(
    settings: &[SnoozePresetSetting],
    shown: &[SnoozePreset],
    id: &str,
    now: OffsetDateTime,
    zone: &TimeZone,
) -> Option<SnoozePreset> {
    pick(settings, shown, id, now, zone, time_of_day_label)
}

fn pick(
    settings: &[SnoozePresetSetting],
    shown: &[SnoozePreset],
    id: &str,
    now: OffsetDateTime,
    zone: &TimeZone,
    format_time: fn(&Zoned) -> String,
) -> Option<SnoozePreset> {
    let ahead = shown
        .iter()
        .find(|preset| preset.id == id && preset.snoozed_until > now)
        .cloned();
    settings
        .iter()
        .find(|setting| matches!(setting, SnoozePresetSetting::After { .. }) && setting.id() == id)
        .and_then(|setting| resolve_preset(*setting, &zoned(now, zone), format_time))
        .or(ahead)
}

/// The unit of a custom duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnoozeUnit {
    Minutes,
    Hours,
    Days,
}

/// The custom snooze dialog's input (`CustomSnoozeInput`, threadSettled.ts:337-343).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CustomSnooze<'a> {
    /// A local date (`YYYY-MM-DD`) and time (`HH:MM`).
    At { date: &'a str, time: &'a str },
    /// An amount as typed ("1.5") of `unit`; days are 24 hours.
    After { amount: &'a str, unit: SnoozeUnit },
}

/// The wake for a custom snooze, `None` when the input is malformed, names a
/// local time that does not exist (rolled-over dates, DST gaps), or is not
/// in the future (`resolveCustomSnooze`, threadSettled.ts:345-361).
pub fn resolve_custom_snooze(
    input: CustomSnooze<'_>,
    now: OffsetDateTime,
    zone: &TimeZone,
) -> Option<OffsetDateTime> {
    let wake = match input {
        CustomSnooze::After { amount, unit } => {
            let amount: f64 = amount.trim().parse().ok()?;
            if !amount.is_finite() || amount <= 0.0 {
                return None;
            }
            let unit_ms = match unit {
                SnoozeUnit::Minutes => 60_000.0,
                SnoozeUnit::Hours => 3_600_000.0,
                SnoozeUnit::Days => 86_400_000.0,
            };
            let wake_ms = now_ms(now) as f64 + amount * unit_ms;
            // JS `Date` range; beyond it `new Date` is invalid.
            if !wake_ms.is_finite() || wake_ms.abs() > 8.64e15 {
                return None;
            }
            OffsetDateTime::from_unix_timestamp_nanos(wake_ms as i128 * 1_000_000).ok()?
        }
        CustomSnooze::At { date, time } => {
            let date = parse_date(date)?;
            let (hour, minute) = parse_time(time)?;
            let ambiguous = zone.to_ambiguous_zoned(date.at(hour, minute, 0, 0));
            if matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. }) {
                return None;
            }
            // A repeated hour picks the earlier instant, like `new Date`.
            offset_date_time(&ambiguous.compatible().ok()?)?
        }
    };
    (wake > now).then_some(wake)
}

fn digits(text: &str) -> Option<i16> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// `/^\d{4}-\d{2}-\d{2}$/`, and a real calendar date.
fn parse_date(text: &str) -> Option<Date> {
    let [year, month, day] = <[&str; 3]>::try_from(text.split('-').collect::<Vec<_>>()).ok()?;
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    Date::new(
        digits(year)?,
        i8::try_from(digits(month)?).ok()?,
        i8::try_from(digits(day)?).ok()?,
    )
    .ok()
}

/// `/^\d{2}:\d{2}$/`, and a real time of day.
fn parse_time(text: &str) -> Option<(i8, i8)> {
    let (hour, minute) = text.split_once(':')?;
    if hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let hour = i8::try_from(digits(hour)?).ok()?;
    let minute = i8::try_from(digits(minute)?).ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zone with US daylight saving (America/Los_Angeles rules), so the
    /// tests do not depend on the machine's tz database.
    fn los_angeles() -> TimeZone {
        TimeZone::posix("PST8PDT,M3.2.0,M11.1.0").expect("valid POSIX TZ")
    }

    /// `new Date(year, month - 1, day, hour, minute)` in `zone`.
    fn local(
        zone: &TimeZone,
        year: i16,
        month: i8,
        day: i8,
        hour: i8,
        minute: i8,
    ) -> OffsetDateTime {
        let at = jiff::civil::date(year, month, day)
            .at(hour, minute, 0, 0)
            .to_zoned(zone.clone())
            .expect("valid local time");
        offset_date_time(&at).unwrap()
    }

    /// The test formatter (threadSnoozed.test.ts:287-288): `H:MM`.
    fn hm(at: &Zoned) -> String {
        format!("{}:{:02}", at.hour(), at.minute())
    }

    fn at(day: SnoozeDay, hour: u8, minute: u8) -> SnoozePresetSetting {
        SnoozePresetSetting::At { day, hour, minute }
    }

    fn after(minutes: u32) -> SnoozePresetSetting {
        SnoozePresetSetting::After { minutes }
    }

    fn menu(now: OffsetDateTime, settings: &[SnoozePresetSetting]) -> Vec<String> {
        resolve_all(settings, now, &los_angeles(), hm)
            .into_iter()
            .map(|preset| format!("{} {}", preset.label, preset.when_label))
            .collect()
    }

    #[test]
    fn lists_custom_presets_soonest_first_and_names_the_day_when_the_label_does_not() {
        // threadSnoozed.test.ts:293-303 — Wednesday 20:00.
        let zone = los_angeles();
        let now = local(&zone, 2026, 4, 8, 20, 0);
        assert_eq!(
            menu(
                now,
                &[
                    at(SnoozeDay::Monday, 18, 0),
                    at(SnoozeDay::Tomorrow, 21, 0),
                    after(360),
                    after(180),
                ]
            ),
            [
                "In 3 hours 23:00",
                "In 6 hours Thu 2:00",
                "Tomorrow 21:00",
                "Next week Mon 18:00"
            ]
        );
    }

    #[test]
    fn drops_a_passed_today_preset_and_moves_a_same_day_weekday_to_next_week() {
        // threadSnoozed.test.ts:305-316 — Monday 18:30.
        let zone = los_angeles();
        let now = local(&zone, 2026, 4, 13, 18, 30);
        let presets = resolve_snooze_presets_with(
            &[at(SnoozeDay::Today, 18, 0), at(SnoozeDay::Monday, 9, 0)],
            now,
            &zone,
        );
        assert_eq!(
            presets
                .iter()
                .map(|preset| preset.id.as_str())
                .collect::<Vec<_>>(),
            ["at:monday:09:00"]
        );
        assert_eq!(presets[0].snoozed_until, local(&zone, 2026, 4, 20, 9, 0));
    }

    #[test]
    fn names_the_real_day_gap() {
        // threadSnoozed.test.ts:318-337
        let zone = los_angeles();
        let theo = [
            at(SnoozeDay::Tomorrow, 21, 0),
            at(SnoozeDay::Monday, 18, 0),
            at(SnoozeDay::Friday, 17, 0),
        ];
        // Sunday 14:00.
        assert_eq!(
            menu(local(&zone, 2026, 8, 30, 14, 0), &theo),
            ["Tomorrow 18:00", "Tomorrow 21:00", "Friday 17:00"]
        );
        // Friday 10:00: today's 17:00 is a week away.
        assert_eq!(
            menu(local(&zone, 2026, 9, 4, 10, 0), &theo),
            ["Tomorrow 21:00", "Next week Mon 18:00", "Next Friday 17:00"]
        );
    }

    #[test]
    fn shows_one_row_when_two_presets_wake_at_the_same_time() {
        // threadSnoozed.test.ts:339-347 — Sunday 07:01.
        let zone = los_angeles();
        assert_eq!(
            menu(local(&zone, 2026, 8, 30, 7, 1), &DEFAULT_SNOOZE_PRESETS),
            [
                "In 1 hour 8:01",
                "In 3 hours 10:01",
                "This evening 18:00",
                "Tomorrow 9:00"
            ]
        );
    }

    #[test]
    fn a_duration_and_a_set_time_in_the_same_minute_are_one_row() {
        // threadSnoozed.test.ts:349-358
        let zone = los_angeles();
        let now = local(&zone, 2026, 4, 8, 15, 0) + time::Duration::seconds(25);
        assert_eq!(
            menu(now, &[after(180), at(SnoozeDay::Today, 18, 0)]),
            ["In 3 hours 18:00"]
        );
    }

    #[test]
    fn picking_a_duration_starts_it_at_the_pick() {
        // threadSnoozed.test.ts:362-376
        let zone = los_angeles();
        let settings = [at(SnoozeDay::Today, 18, 30), after(180)];
        let shown = resolve_all(&settings, local(&zone, 2026, 4, 8, 15, 0), &zone, hm);
        let picked = pick(
            &settings,
            &shown,
            "after:180",
            local(&zone, 2026, 4, 8, 15, 30),
            &zone,
            hm,
        );
        assert_eq!(
            picked.map(|preset| preset.snoozed_until),
            Some(local(&zone, 2026, 4, 8, 18, 30))
        );
    }

    #[test]
    fn picking_a_set_time_keeps_its_shown_wake_across_midnight() {
        // threadSnoozed.test.ts:378-387
        let zone = los_angeles();
        let shown = resolve_snooze_presets(local(&zone, 2026, 8, 30, 23, 59), &zone);
        let picked = pick_snooze_preset(
            &shown,
            "at:tomorrow:09:00",
            local(&zone, 2026, 8, 31, 0, 1),
            &zone,
        );
        assert_eq!(
            picked.map(|preset| preset.snoozed_until),
            Some(local(&zone, 2026, 8, 31, 9, 0))
        );
    }

    #[test]
    fn calendar_days_stay_on_the_right_day_across_dst() {
        // threadSettled.ts:202-209: at 23:30 the night before spring-forward,
        // "tomorrow 09:00" is the next calendar day, 8.5 real hours away
        // (9.5 on the wall clock, minus the skipped hour).
        let zone = los_angeles();
        let now = local(&zone, 2027, 3, 13, 23, 30);
        let presets = resolve_snooze_presets_with(&[at(SnoozeDay::Tomorrow, 9, 0)], now, &zone);
        assert_eq!(presets[0].snoozed_until, local(&zone, 2027, 3, 14, 9, 0));
        assert_eq!(
            presets[0].snoozed_until - now,
            time::Duration::minutes(8 * 60 + 30)
        );
    }

    #[test]
    fn default_labels_read_like_an_english_locale() {
        let zone = los_angeles();
        let presets = resolve_snooze_presets(local(&zone, 2026, 8, 30, 7, 1), &zone);
        assert_eq!(presets[2].label, "This evening");
        assert_eq!(presets[2].when_label, "6:00 PM");
        assert_eq!(presets[0].when_label, "8:01 AM");
    }

    // --- customSnooze.test.ts ---

    #[test]
    fn rejects_invalid_or_non_future_calendar_input() {
        // customSnooze.test.ts:10-18
        let zone = los_angeles();
        let now = local(&zone, 2026, 9, 14, 14, 30);
        for (date, time) in [
            ("2026-09-14", "14:30"),
            ("2027-02-29", "09:00"),
            ("2026-09-15", "24:00"),
        ] {
            assert_eq!(
                resolve_custom_snooze(CustomSnooze::At { date, time }, now, &zone),
                None
            );
        }
        assert_eq!(
            resolve_custom_snooze(
                CustomSnooze::At {
                    date: "2026-09-15",
                    time: "09:05"
                },
                now,
                &zone
            ),
            Some(local(&zone, 2026, 9, 15, 9, 5))
        );
    }

    #[test]
    fn resolves_durations_from_the_confirmation_time() {
        // customSnooze.test.ts:20-30
        let zone = los_angeles();
        let confirmed_at = local(&zone, 2026, 9, 14, 14, 30) + time::Duration::minutes(5);
        for (unit, amount, elapsed) in [
            (SnoozeUnit::Minutes, "45", time::Duration::minutes(45)),
            (SnoozeUnit::Hours, "1.5", time::Duration::minutes(90)),
            (SnoozeUnit::Days, "2", time::Duration::hours(48)),
        ] {
            assert_eq!(
                resolve_custom_snooze(CustomSnooze::After { amount, unit }, confirmed_at, &zone),
                Some(confirmed_at + elapsed)
            );
        }
    }

    #[test]
    fn rejects_invalid_durations() {
        // customSnooze.test.ts:32-34
        let zone = los_angeles();
        let now = local(&zone, 2026, 9, 14, 14, 30);
        for amount in ["0", "NaN", "1e300"] {
            let input = CustomSnooze::After {
                amount,
                unit: SnoozeUnit::Hours,
            };
            assert_eq!(resolve_custom_snooze(input, now, &zone), None);
        }
    }

    #[test]
    fn rejects_nonexistent_local_times_at_spring_forward() {
        // customSnooze.test.ts:36-44
        let zone = los_angeles();
        let input = CustomSnooze::At {
            date: "2027-03-14",
            time: "02:30",
        };
        let now = time::macros::datetime!(2027-03-01 0:00 UTC);
        assert_eq!(resolve_custom_snooze(input, now, &zone), None);
    }

    #[test]
    fn duration_days_are_24_hours_across_dst() {
        // customSnooze.test.ts:46-51
        let zone = los_angeles();
        let before = local(&zone, 2027, 3, 13, 12, 0);
        let input = CustomSnooze::After {
            amount: "1",
            unit: SnoozeUnit::Days,
        };
        assert_eq!(
            resolve_custom_snooze(input, before, &zone),
            Some(local(&zone, 2027, 3, 14, 13, 0))
        );
    }
}
