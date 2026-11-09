//! Snooze menu presets and the Custom… dialog (`docs/triage.md` §7.1).
//!
//! Calendar presets resolve in the caller's time zone with calendar days, so
//! a wake lands on the right day across daylight saving changes.

use jiff::civil::{Date, Time, Weekday};
use jiff::tz::{AmbiguousOffset, TimeZone};
use jiff::{Span, Timestamp, Zoned};
use time::OffsetDateTime;

/// The day of an `At` preset.
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

    fn weekday(self) -> Option<Weekday> {
        match self {
            Self::Today | Self::Tomorrow => None,
            Self::Monday => Some(Weekday::Monday),
            Self::Tuesday => Some(Weekday::Tuesday),
            Self::Wednesday => Some(Weekday::Wednesday),
            Self::Thursday => Some(Weekday::Thursday),
            Self::Friday => Some(Weekday::Friday),
            Self::Saturday => Some(Weekday::Saturday),
            Self::Sunday => Some(Weekday::Sunday),
        }
    }
}

/// One snooze menu entry as a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnoozePresetSetting {
    /// `minutes` from now.
    After { minutes: u32 },
    /// At `hour:minute` local time on `day`.
    At {
        day: SnoozeDay,
        hour: u8,
        minute: u8,
    },
}

impl SnoozePresetSetting {
    /// The stable id: `"after:60"`, `"at:tomorrow:09:00"`.
    pub fn id(&self) -> String {
        match *self {
            Self::After { minutes } => format!("after:{minutes}"),
            Self::At { day, hour, minute } => format!("at:{}:{hour:02}:{minute:02}", day.name()),
        }
    }
}

/// The snooze menu: in 1 hour, in 3 hours, 18:00 today, 09:00 tomorrow,
/// 09:00 next Monday.
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

/// A resolved menu entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SnoozePreset {
    /// The setting's id.
    pub id: String,
    /// "In 1 hour", "This evening", "Tomorrow", "Next week", "Thursday", …
    pub label: String,
    /// The wake time, e.g. "6:00 PM", or "Thu 2:00 AM" when the label does
    /// not name the day and the wake is on another date.
    pub when_label: String,
    pub until: OffsetDateTime,
}

fn to_timestamp(time: OffsetDateTime) -> Option<Timestamp> {
    Timestamp::from_nanosecond(time.unix_timestamp_nanos()).ok()
}

fn from_timestamp(timestamp: Timestamp) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(timestamp.as_nanosecond()).ok()
}

fn duration_label(minutes: u32) -> String {
    let (count, unit) = if minutes != 0 && minutes.is_multiple_of(1440) {
        (minutes / 1440, "day")
    } else if minutes != 0 && minutes.is_multiple_of(60) {
        (minutes / 60, "hour")
    } else {
        (minutes, "minute")
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("In {count} {unit}{plural}")
}

fn when_label(wake: &Zoned, name_day: bool) -> String {
    let clock = wake.strftime("%-I:%M %p");
    if name_day {
        format!("{} {clock}", wake.strftime("%a"))
    } else {
        clock.to_string()
    }
}

/// Resolves one setting at `now`; None for a set time already passed.
fn resolve(
    setting: &SnoozePresetSetting,
    now: OffsetDateTime,
    zone: &TimeZone,
) -> Option<SnoozePreset> {
    let now_stamp = to_timestamp(now)?;
    let today = now_stamp.to_zoned(zone.clone()).date();
    let (label, wake, name_day) = match *setting {
        SnoozePresetSetting::After { minutes } => {
            let until = now.checked_add(time::Duration::minutes(i64::from(minutes)))?;
            let wake = to_timestamp(until)?.to_zoned(zone.clone());
            let other_day = wake.date() != today;
            (duration_label(minutes), wake, other_day)
        }
        SnoozePresetSetting::At { day, hour, minute } => {
            let gap = match day.weekday() {
                None if day == SnoozeDay::Today => 0,
                None => 1,
                Some(weekday) => match weekday.since(today.weekday()) {
                    0 => 7,
                    days => i64::from(days),
                },
            };
            let date = today.checked_add(Span::new().days(gap)).ok()?;
            let clock =
                Time::new(i8::try_from(hour).ok()?, i8::try_from(minute).ok()?, 0, 0).ok()?;
            let wake = zone
                .to_ambiguous_zoned(date.to_datetime(clock))
                .compatible()
                .ok()?;
            if wake.timestamp() <= now_stamp {
                return None;
            }
            let label = match gap {
                0 if hour < 12 => "This morning".to_owned(),
                0 if hour < 17 => "This afternoon".to_owned(),
                0 => "This evening".to_owned(),
                1 => "Tomorrow".to_owned(),
                _ if day == SnoozeDay::Monday => "Next week".to_owned(),
                7 => format!("Next {}", wake.strftime("%A")),
                _ => wake.strftime("%A").to_string(),
            };
            let name_day = label == "Next week" && wake.date() != today;
            (label, wake, name_day)
        }
    };
    Some(SnoozePreset {
        id: setting.id(),
        label,
        when_label: when_label(&wake, name_day),
        until: from_timestamp(wake.timestamp())?,
    })
}

/// [`snooze_presets_with`] for [`DEFAULT_SNOOZE_PRESETS`].
pub fn snooze_presets(now: OffsetDateTime, zone: &TimeZone) -> Vec<SnoozePreset> {
    snooze_presets_with(&DEFAULT_SNOOZE_PRESETS, now, zone)
}

/// The snooze menu at `now` in `zone`: durations start now (keeping the
/// seconds); a weekday is its next occurrence after today (the same weekday
/// is a week ahead); a set time already passed today is left out. Sorted by
/// wake minute, one entry per minute, the earlier setting winning.
pub fn snooze_presets_with(
    settings: &[SnoozePresetSetting],
    now: OffsetDateTime,
    zone: &TimeZone,
) -> Vec<SnoozePreset> {
    let minute = |preset: &SnoozePreset| preset.until.unix_timestamp().div_euclid(60);
    let mut presets: Vec<SnoozePreset> = settings
        .iter()
        .filter_map(|setting| resolve(setting, now, zone))
        .collect();
    presets.sort_by_key(minute);
    presets.dedup_by_key(|preset| minute(preset));
    presets
}

/// [`pick_snooze_preset_with`] for [`DEFAULT_SNOOZE_PRESETS`].
pub fn pick_snooze_preset(
    shown: &[SnoozePreset],
    id: &str,
    now: OffsetDateTime,
    zone: &TimeZone,
) -> Option<SnoozePreset> {
    pick_snooze_preset_with(&DEFAULT_SNOOZE_PRESETS, shown, id, now, zone)
}

/// Picking menu entry `id` at `now`: a duration starts at the pick; a set
/// time keeps the wake `shown` for it if that is still ahead, else the pick
/// does nothing (None). None for an unknown id.
pub fn pick_snooze_preset_with(
    settings: &[SnoozePresetSetting],
    shown: &[SnoozePreset],
    id: &str,
    now: OffsetDateTime,
    zone: &TimeZone,
) -> Option<SnoozePreset> {
    let setting = settings.iter().find(|setting| setting.id() == id)?;
    match setting {
        SnoozePresetSetting::After { .. } => resolve(setting, now, zone),
        SnoozePresetSetting::At { .. } => shown
            .iter()
            .find(|preset| preset.id == id && preset.until > now)
            .cloned(),
    }
}

/// The unit of a Custom… duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnoozeUnit {
    Minutes,
    Hours,
    Days,
}

/// The Custom… dialog's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CustomSnooze<'a> {
    /// Local `YYYY-MM-DD` and `HH:MM`.
    At { date: &'a str, time: &'a str },
    /// A positive, finite amount (decimals allowed) of `unit`; a day is 24 h.
    After { amount: &'a str, unit: SnoozeUnit },
}

fn parse_date(text: &str) -> Option<Date> {
    let bytes = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<i16> {
        let part = text.get(range)?;
        part.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let (year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    Date::new(year, i8::try_from(month).ok()?, i8::try_from(day).ok()?).ok()
}

fn parse_clock(text: &str) -> Option<Time> {
    let (hour, minute) = text.split_once(':')?;
    let valid = |part: &str, width: std::ops::RangeInclusive<usize>| {
        width.contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit())
    };
    if !valid(hour, 1..=2) || !valid(minute, 2..=2) {
        return None;
    }
    Time::new(hour.parse().ok()?, minute.parse().ok()?, 0, 0).ok()
}

/// The wake time from the Custom… dialog. A local time that does not exist
/// (an invalid date, or the gap at a spring-forward change) is refused; a
/// repeated local hour takes the earlier instant. None too when the input is
/// invalid or the result is not after `now`.
pub fn custom_snooze(
    input: CustomSnooze<'_>,
    now: OffsetDateTime,
    zone: &TimeZone,
) -> Option<OffsetDateTime> {
    let until = match input {
        CustomSnooze::At { date, time } => {
            let local = parse_date(date.trim())?.to_datetime(parse_clock(time.trim())?);
            let ambiguous = zone.to_ambiguous_timestamp(local);
            if matches!(ambiguous.offset(), AmbiguousOffset::Gap { .. }) {
                return None;
            }
            from_timestamp(ambiguous.earlier().ok()?)?
        }
        CustomSnooze::After { amount, unit } => {
            let amount: f64 = amount.trim().parse().ok()?;
            if !amount.is_finite() || amount <= 0.0 {
                return None;
            }
            let unit_seconds = match unit {
                SnoozeUnit::Minutes => 60.0,
                SnoozeUnit::Hours => 3600.0,
                SnoozeUnit::Days => 86_400.0,
            };
            now.checked_add(time::Duration::checked_seconds_f64(amount * unit_seconds)?)?
        }
    };
    (until > now).then_some(until)
}

#[cfg(test)]
mod tests {
    use time::format_description::well_known::Rfc3339;

    use super::*;

    fn utc(text: &str) -> OffsetDateTime {
        OffsetDateTime::parse(text, &Rfc3339).unwrap()
    }

    fn new_york() -> TimeZone {
        TimeZone::posix("EST5EDT,M3.2.0,M11.1.0").unwrap()
    }

    fn at(day: SnoozeDay, hour: u8, minute: u8) -> SnoozePresetSetting {
        SnoozePresetSetting::At { day, hour, minute }
    }

    fn after(minutes: u32) -> SnoozePresetSetting {
        SnoozePresetSetting::After { minutes }
    }

    fn rows(presets: &[SnoozePreset]) -> Vec<(&str, &str, &str)> {
        presets
            .iter()
            .map(|p| (p.id.as_str(), p.label.as_str(), p.when_label.as_str()))
            .collect()
    }

    // 2026-11-09 is a Monday.

    #[test]
    fn lists_presets_soonest_first_and_names_the_day_when_the_label_does_not() {
        let presets = snooze_presets(utc("2026-11-09T12:00:00Z"), &TimeZone::UTC);
        assert_eq!(
            rows(&presets),
            [
                ("after:60", "In 1 hour", "1:00 PM"),
                ("after:180", "In 3 hours", "3:00 PM"),
                ("at:today:18:00", "This evening", "6:00 PM"),
                ("at:tomorrow:09:00", "Tomorrow", "9:00 AM"),
                ("at:monday:09:00", "Next week", "Mon 9:00 AM"),
            ]
        );
        assert_eq!(presets[4].until, utc("2026-11-16T09:00:00Z"));

        let late = snooze_presets_with(&[after(60)], utc("2026-11-09T23:30:00Z"), &TimeZone::UTC);
        assert_eq!(rows(&late), [("after:60", "In 1 hour", "Tue 12:30 AM")]);
    }

    #[test]
    fn drops_a_passed_today_preset_and_moves_a_same_day_weekday_to_next_week() {
        let presets = snooze_presets(utc("2026-11-09T19:00:00Z"), &TimeZone::UTC);
        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "after:60",
                "after:180",
                "at:tomorrow:09:00",
                "at:monday:09:00"
            ]
        );
        assert_eq!(presets[3].until, utc("2026-11-16T09:00:00Z"));

        let tuesday = snooze_presets_with(
            &[at(SnoozeDay::Tuesday, 9, 0)],
            utc("2026-11-10T08:00:00Z"),
            &TimeZone::UTC,
        );
        assert_eq!(
            rows(&tuesday),
            [("at:tuesday:09:00", "Next Tuesday", "9:00 AM")]
        );
        assert_eq!(tuesday[0].until, utc("2026-11-17T09:00:00Z"));
    }

    #[test]
    fn names_the_real_day_gap() {
        let monday = || at(SnoozeDay::Monday, 9, 0);
        let sunday = snooze_presets_with(&[monday()], utc("2026-11-08T12:00:00Z"), &TimeZone::UTC);
        assert_eq!(rows(&sunday), [("at:monday:09:00", "Tomorrow", "9:00 AM")]);

        let wednesday = snooze_presets_with(
            &[at(SnoozeDay::Wednesday, 14, 0)],
            utc("2026-11-09T12:00:00Z"),
            &TimeZone::UTC,
        );
        assert_eq!(
            rows(&wednesday),
            [("at:wednesday:14:00", "Wednesday", "2:00 PM")]
        );

        let today = [
            at(SnoozeDay::Today, 9, 0),
            at(SnoozeDay::Today, 12, 0),
            at(SnoozeDay::Today, 16, 59),
            at(SnoozeDay::Today, 17, 0),
        ];
        let labels: Vec<String> =
            snooze_presets_with(&today, utc("2026-11-09T08:00:00Z"), &TimeZone::UTC)
                .into_iter()
                .map(|p| p.label)
                .collect();
        assert_eq!(
            labels,
            [
                "This morning",
                "This afternoon",
                "This afternoon",
                "This evening"
            ]
        );
    }

    #[test]
    fn shows_one_row_when_two_presets_wake_at_the_same_time() {
        let settings = [at(SnoozeDay::Tomorrow, 9, 0), at(SnoozeDay::Tuesday, 9, 0)];
        let presets = snooze_presets_with(&settings, utc("2026-11-09T12:00:00Z"), &TimeZone::UTC);
        assert_eq!(
            rows(&presets),
            [("at:tomorrow:09:00", "Tomorrow", "9:00 AM")]
        );
    }

    #[test]
    fn a_duration_and_a_set_time_in_the_same_minute_are_one_row() {
        let now = utc("2026-11-09T12:00:30Z");
        let set_first = snooze_presets_with(
            &[at(SnoozeDay::Today, 13, 0), after(60)],
            now,
            &TimeZone::UTC,
        );
        assert_eq!(
            rows(&set_first),
            [("at:today:13:00", "This afternoon", "1:00 PM")]
        );
        let duration_first = snooze_presets_with(
            &[after(60), at(SnoozeDay::Today, 13, 0)],
            now,
            &TimeZone::UTC,
        );
        assert_eq!(
            rows(&duration_first),
            [("after:60", "In 1 hour", "1:00 PM")]
        );
        assert_eq!(
            duration_first[0].until,
            utc("2026-11-09T13:00:30Z"),
            "keeps the clock's seconds"
        );
    }

    #[test]
    fn picking_a_duration_starts_it_at_the_pick() {
        let shown = snooze_presets(utc("2026-11-09T12:00:00Z"), &TimeZone::UTC);
        let picked = pick_snooze_preset(
            &shown,
            "after:60",
            utc("2026-11-09T12:10:00Z"),
            &TimeZone::UTC,
        )
        .unwrap();
        assert_eq!(picked.until, utc("2026-11-09T13:10:00Z"));
        assert_eq!(
            pick_snooze_preset(
                &shown,
                "after:5",
                utc("2026-11-09T12:10:00Z"),
                &TimeZone::UTC
            ),
            None
        );
    }

    #[test]
    fn picking_a_set_time_keeps_its_shown_wake_across_midnight() {
        let shown = snooze_presets(utc("2026-11-09T23:59:00Z"), &TimeZone::UTC);
        let picked = pick_snooze_preset(
            &shown,
            "at:tomorrow:09:00",
            utc("2026-11-10T00:01:00Z"),
            &TimeZone::UTC,
        )
        .unwrap();
        assert_eq!(picked.until, utc("2026-11-10T09:00:00Z"));

        let evening = snooze_presets(utc("2026-11-09T17:59:00Z"), &TimeZone::UTC);
        assert_eq!(
            pick_snooze_preset(
                &evening,
                "at:today:18:00",
                utc("2026-11-09T18:01:00Z"),
                &TimeZone::UTC
            ),
            None
        );
    }

    #[test]
    fn calendar_days_stay_on_the_right_day_across_dst() {
        // Saturday 2026-10-31 12:00 EDT; clocks fall back early on Sunday.
        let presets = snooze_presets(utc("2026-10-31T16:00:00Z"), &new_york());
        let tomorrow = presets
            .iter()
            .find(|p| p.id == "at:tomorrow:09:00")
            .unwrap();
        assert_eq!(tomorrow.until, utc("2026-11-01T14:00:00Z"));
        assert_eq!(tomorrow.when_label, "9:00 AM");
        let monday = presets.iter().find(|p| p.id == "at:monday:09:00").unwrap();
        assert_eq!(
            (monday.label.as_str(), monday.until),
            ("Next week", utc("2026-11-02T14:00:00Z"))
        );
        assert_eq!(monday.when_label, "Mon 9:00 AM");
    }

    #[test]
    fn default_labels_read_like_an_english_locale() {
        let ids: Vec<String> = DEFAULT_SNOOZE_PRESETS
            .iter()
            .map(SnoozePresetSetting::id)
            .collect();
        assert_eq!(
            ids,
            [
                "after:60",
                "after:180",
                "at:today:18:00",
                "at:tomorrow:09:00",
                "at:monday:09:00"
            ]
        );
        let labels: Vec<String> = [1, 90, 60, 120, 1440, 2880]
            .into_iter()
            .map(duration_label)
            .collect();
        assert_eq!(
            labels,
            [
                "In 1 minute",
                "In 90 minutes",
                "In 1 hour",
                "In 2 hours",
                "In 1 day",
                "In 2 days"
            ]
        );
    }

    #[test]
    fn rejects_invalid_or_non_future_calendar_input() {
        let now = utc("2026-11-09T12:00:00Z");
        let custom =
            |date, time| custom_snooze(CustomSnooze::At { date, time }, now, &TimeZone::UTC);
        assert_eq!(
            custom("2026-11-10", "09:00"),
            Some(utc("2026-11-10T09:00:00Z"))
        );
        assert_eq!(
            custom("2026-11-10", "9:05"),
            Some(utc("2026-11-10T09:05:00Z"))
        );
        assert_eq!(custom("2026-02-30", "09:00"), None, "rolled-over date");
        assert_eq!(custom("2026-13-01", "09:00"), None);
        assert_eq!(custom("11/10/2026", "09:00"), None);
        assert_eq!(custom("2026-11-10", "25:00"), None);
        assert_eq!(custom("2026-11-10", "9:5"), None);
        assert_eq!(custom("2026-11-09", "12:00"), None, "not after now");
        assert_eq!(custom("2026-11-08", "09:00"), None, "past");
    }

    #[test]
    fn resolves_durations_from_the_confirmation_time() {
        let now = utc("2026-11-09T12:00:00Z");
        let custom =
            |amount, unit| custom_snooze(CustomSnooze::After { amount, unit }, now, &TimeZone::UTC);
        assert_eq!(
            custom("1.5", SnoozeUnit::Hours),
            Some(utc("2026-11-09T13:30:00Z"))
        );
        assert_eq!(
            custom("30", SnoozeUnit::Minutes),
            Some(utc("2026-11-09T12:30:00Z"))
        );
        assert_eq!(
            custom(" 2 ", SnoozeUnit::Days),
            Some(utc("2026-11-11T12:00:00Z"))
        );
    }

    #[test]
    fn rejects_invalid_durations() {
        let now = utc("2026-11-09T12:00:00Z");
        for amount in ["0", "-1", "abc", "", "inf", "NaN", "1e400"] {
            let input = CustomSnooze::After {
                amount,
                unit: SnoozeUnit::Hours,
            };
            assert_eq!(
                custom_snooze(input, now, &TimeZone::UTC),
                None,
                "{amount:?}"
            );
        }
    }

    #[test]
    fn rejects_nonexistent_local_times_at_spring_forward() {
        let now = utc("2027-03-01T12:00:00Z");
        let custom = |date, time| custom_snooze(CustomSnooze::At { date, time }, now, &new_york());
        assert_eq!(custom("2027-03-14", "02:30"), None, "skipped hour");
        assert_eq!(
            custom("2027-03-14", "03:30"),
            Some(utc("2027-03-14T07:30:00Z"))
        );
        let fall = custom_snooze(
            CustomSnooze::At {
                date: "2026-11-01",
                time: "01:30",
            },
            utc("2026-10-01T00:00:00Z"),
            &new_york(),
        );
        assert_eq!(
            fall,
            Some(utc("2026-11-01T05:30:00Z")),
            "a repeated hour takes the earlier instant"
        );
    }

    #[test]
    fn duration_days_are_24_hours_across_dst() {
        let now = utc("2026-10-31T16:00:00Z");
        let input = CustomSnooze::After {
            amount: "1",
            unit: SnoozeUnit::Days,
        };
        assert_eq!(
            custom_snooze(input, now, &new_york()),
            Some(utc("2026-11-01T16:00:00Z"))
        );
    }
}
