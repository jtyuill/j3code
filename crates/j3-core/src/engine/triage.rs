//! Thread triage, the server side of the Inbox workflow (`docs/triage.md`,
//! with the decisions of its §12.1 taking precedence): the triage commands,
//! what a sent message does to a thread's triage state, and the auto-settle
//! rule.
//!
//! Everything here is a pure function of the thread's [`Triage`], the
//! derived [`Facts`] of its row, and the time `now` (epoch milliseconds).
//! "Bump" means `bumpedAt = now` (triage §6.1).

use j3_protocol::v2::{
    entities::{Settled, Thread, ThreadStatus, Triage, TurnStatus, TurnSummary},
    envelope::Error,
};

use crate::clock::{iso, parse_ms};

pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// Threads quiet this long settle on their own (triage §8).
pub const AUTO_SETTLE_AFTER_MS: i64 = 3 * DAY_MS;

/// What triage reads from the thread row (triage §2, "status inputs").
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    pub archived: bool,
    pub status: ThreadStatus,
    /// A turn is active (`activeTurnId` is set).
    pub active: bool,
    pub pending_request: bool,
    pub queued: u32,
    pub latest_turn: Option<&'a TurnSummary>,
    pub latest_user_message_at: Option<&'a str>,
    pub created_at: &'a str,
}

impl<'a> Facts<'a> {
    /// The facts of a thread row.
    pub fn of(row: &'a Thread) -> Self {
        Self {
            archived: row.archived_at.is_some(),
            status: row.status,
            active: row.active_turn_id.is_some(),
            pending_request: row.pending_request.is_some(),
            queued: row.queued,
            latest_turn: row.latest_turn.as_ref(),
            latest_user_message_at: row.latest_user_message_at.as_deref(),
            created_at: &row.created_at,
        }
    }
}

fn refuse(message: &str) -> Error {
    Error::rejected(message)
}

fn ms(field: &Option<String>) -> Option<i64> {
    field.as_deref().and_then(parse_ms)
}

fn bump(triage: &mut Triage, now: i64) {
    triage.bumped_at = iso(now);
}

fn not_archived(facts: &Facts) -> Result<(), Error> {
    if facts.archived {
        Err(refuse("This thread is archived."))
    } else {
        Ok(())
    }
}

/// A trimmed, valid order key (triage §5.4): lowercase ASCII letters, not
/// ending in `a`.
fn order_key(raw: &str) -> Result<String, Error> {
    let key = raw.trim();
    if key.is_empty() {
        return Err(refuse("The order key can't be empty."));
    }
    let valid = key.bytes().all(|byte| byte.is_ascii_lowercase()) && !key.ends_with('a');
    if !valid {
        return Err(refuse("The order key isn't valid."));
    }
    Ok(key.to_owned())
}

/// `triage.visit`: moves the read watermark forward (triage §4.2). Never
/// bumps; allowed on archived threads.
pub fn visit(triage: &mut Triage, visited_at: &str) -> Result<(), Error> {
    let at =
        parse_ms(visited_at).ok_or_else(|| refuse("The visit time is not a valid timestamp."))?;
    if ms(&triage.visited_at).is_none_or(|old| at > old) {
        triage.visited_at = Some(iso(at));
    }
    Ok(())
}

/// `triage.markUnread` (triage §4.3): the watermark goes just before the
/// latest turn's end, and a passed snooze is retired so the thread does not
/// read "Woke" again. Never bumps; allowed on archived threads.
pub fn mark_unread(triage: &mut Triage, facts: &Facts, now: i64) -> Result<(), Error> {
    let ended = facts
        .latest_turn
        .and_then(|turn| turn.ended_at.as_deref())
        .and_then(parse_ms)
        .ok_or_else(|| refuse("This thread has no finished turn to mark unread."))?;
    triage.visited_at = Some(iso(ended - 1));
    if let Some(until) = ms(&triage.snoozed_until)
        && until <= now
    {
        triage.last_wake_at = triage.snoozed_until.take();
        triage.snoozed_at = None;
    }
    Ok(())
}

/// `triage.settle` (triage §6.1). `settled_at` defaults to now.
pub fn settle(
    triage: &mut Triage,
    facts: &Facts,
    now: i64,
    settled_at: Option<&str>,
) -> Result<(), Error> {
    not_archived(facts)?;
    if facts.pending_request {
        return Err(refuse(
            "This thread is waiting on a question. Answer it before settling.",
        ));
    }
    if facts.active {
        return Err(refuse(
            "This thread is still running. Stop it before settling.",
        ));
    }
    if facts.queued > 0 {
        return Err(refuse(
            "This thread has a queued message. Send it or remove it from the queue before settling.",
        ));
    }
    let at = match settled_at {
        Some(text) => {
            parse_ms(text).ok_or_else(|| refuse("The settle time is not a valid timestamp."))?
        }
        None => now,
    };
    let was_settled = triage.settled == Some(Settled::Settled);
    let was_snoozed = triage.snoozed_until.is_some();
    let keep = was_settled && triage.settled_at.is_some() && triage.pinned_at.is_none();
    if !keep {
        triage.settled_at = Some(iso(at));
    }
    triage.settled = Some(Settled::Settled);
    triage.unsettled_at = None;
    triage.pinned_at = None;
    triage.pin_order = None;
    triage.active_order = None;
    triage.snoozed_until = None;
    triage.snoozed_at = None;
    if !was_settled || was_snoozed {
        bump(triage, now);
    }
    Ok(())
}

/// `triage.unsettle`: back to Active, opting out of auto-settle until the
/// next sent message.
pub fn unsettle(triage: &mut Triage, facts: &Facts, now: i64) -> Result<(), Error> {
    not_archived(facts)?;
    let was_active = triage.settled == Some(Settled::Active);
    triage.settled = Some(Settled::Active);
    triage.settled_at = None;
    if !was_active || triage.unsettled_at.is_none() {
        triage.unsettled_at = Some(iso(now));
    }
    if !was_active {
        bump(triage, now);
    }
    Ok(())
}

/// `triage.snooze` (triage §7.1 and §12.1 decisions 1 and 5): refused while
/// a question is open, something is queued or a turn is starting. Snoozing a
/// settled thread un-settles it, so it wakes into Active.
pub fn snooze(triage: &mut Triage, facts: &Facts, now: i64, until: &str) -> Result<(), Error> {
    not_archived(facts)?;
    let wake = parse_ms(until)
        .filter(|wake| *wake > now)
        .ok_or_else(|| refuse("The snooze time is not in the future."))?;
    if facts.pending_request {
        return Err(refuse(
            "This thread is waiting on a question and can't be snoozed.",
        ));
    }
    if facts.queued > 0 {
        return Err(refuse(
            "This thread has a queued message and can't be snoozed.",
        ));
    }
    if facts.status == ThreadStatus::Starting {
        return Err(refuse(
            "This thread is starting a turn and can't be snoozed.",
        ));
    }
    if triage.settled == Some(Settled::Settled) {
        triage.settled = None;
        triage.settled_at = None;
        triage.unsettled_at = Some(iso(now));
    }
    triage.snoozed_until = Some(iso(wake));
    triage.snoozed_at = Some(iso(now));
    bump(triage, now);
    Ok(())
}

/// `triage.wake`: ends a snooze now (triage §7.5); a no-op without one.
pub fn wake(triage: &mut Triage, facts: &Facts, now: i64) -> Result<(), Error> {
    not_archived(facts)?;
    if triage.snoozed_until.take().is_some() {
        triage.snoozed_at = None;
        triage.last_wake_at = Some(iso(now));
        bump(triage, now);
    }
    Ok(())
}

/// `triage.pin`, optionally at order key `order`. Pinning un-settles and
/// wakes the thread.
pub fn pin(triage: &mut Triage, facts: &Facts, now: i64, order: Option<&str>) -> Result<(), Error> {
    not_archived(facts)?;
    let order = order.map(order_key).transpose()?;
    let mut changed = false;
    if triage.pinned_at.is_none() {
        triage.pinned_at = Some(iso(now));
        if order.is_some() {
            triage.pin_order = order;
        }
        changed = true;
    }
    if triage.settled == Some(Settled::Settled) {
        triage.settled = Some(Settled::Active);
        triage.settled_at = None;
        changed = true;
    }
    if triage.snoozed_until.take().is_some() {
        triage.snoozed_at = None;
        triage.last_wake_at = Some(iso(now));
        changed = true;
    }
    if changed {
        bump(triage, now);
    }
    Ok(())
}

/// `triage.unpin`.
pub fn unpin(triage: &mut Triage, facts: &Facts, now: i64) -> Result<(), Error> {
    not_archived(facts)?;
    triage.pin_order = None;
    if triage.pinned_at.take().is_some() {
        bump(triage, now);
    }
    Ok(())
}

/// `triage.orderPinned`.
pub fn order_pinned(
    triage: &mut Triage,
    facts: &Facts,
    now: i64,
    order: &str,
) -> Result<(), Error> {
    not_archived(facts)?;
    let order = order_key(order)?;
    if triage.pinned_at.is_none() {
        return Err(refuse(
            "This thread is not pinned, so it can't be reordered.",
        ));
    }
    if triage.pin_order.as_deref() != Some(order.as_str()) {
        triage.pin_order = Some(order);
        bump(triage, now);
    }
    Ok(())
}

/// `triage.orderActive`. Never bumps.
pub fn order_active(triage: &mut Triage, facts: &Facts, order: &str) -> Result<(), Error> {
    not_archived(facts)?;
    let order = order_key(order)?;
    if triage.pinned_at.is_some() || triage.settled == Some(Settled::Settled) {
        return Err(refuse(
            "This thread is not active, so it can't be reordered.",
        ));
    }
    triage.active_order = Some(order);
    Ok(())
}

/// `triage.setAutoSettle`.
pub fn set_auto_settle(
    triage: &mut Triage,
    facts: &Facts,
    now: i64,
    enabled: bool,
) -> Result<(), Error> {
    not_archived(facts)?;
    match (enabled, triage.auto_settle_off_at.is_some()) {
        (false, false) => {
            triage.auto_settle_off_at = Some(iso(now));
            bump(triage, now);
        }
        (true, true) => {
            triage.auto_settle_off_at = None;
            bump(triage, now);
        }
        _ => {}
    }
    Ok(())
}

/// The server accepted a user message, or a queued one started (triage
/// §6.2): the first one sets the read watermark, and any message clears an
/// explicit settle choice and a snooze.
pub fn on_user_message(triage: &mut Triage, now: i64) {
    if triage.visited_at.is_none() {
        triage.visited_at = Some(iso(now - 1));
    }
    if let Some(settled) = triage.settled.take() {
        triage.settled_at = None;
        if settled == Settled::Settled {
            triage.unsettled_at = Some(iso(now));
        }
        bump(triage, now);
    }
    if triage.snoozed_until.take().is_some() {
        triage.snoozed_at = None;
        bump(triage, now);
    }
}

/// The thread failed after it was snoozed (§12.1 decision 2): its latest
/// turn failed, ending (else requested) after `snoozedAt`, or `snoozedAt`
/// is unset.
fn failed_after_snooze(triage: &Triage, facts: &Facts) -> bool {
    if facts.status != ThreadStatus::Failed {
        return false;
    }
    let Some(snoozed_at) = ms(&triage.snoozed_at) else {
        return true;
    };
    facts
        .latest_turn
        .filter(|turn| turn.status == TurnStatus::Failed)
        .and_then(|turn| {
            turn.ended_at
                .as_deref()
                .or(Some(turn.requested_at.as_str()))
                .and_then(parse_ms)
        })
        .is_some_and(|failed| failed > snoozed_at)
}

/// The one "snoozed" test (§12.1 decision 2): the wake time is ahead, no
/// question is open, and the thread has not failed since snoozing.
pub fn effectively_snoozed(triage: &Triage, facts: &Facts, now: i64) -> bool {
    ms(&triage.snoozed_until).is_some_and(|until| until > now)
        && !facts.pending_request
        && !failed_after_snooze(triage, facts)
}

/// When the thread last saw activity (triage §8 "inactive since").
fn inactive_since(triage: &Triage, facts: &Facts, now: i64) -> Option<i64> {
    let turn = facts.latest_turn;
    let activity = [
        facts.latest_user_message_at.and_then(parse_ms),
        turn.and_then(|turn| parse_ms(&turn.requested_at)),
        turn.and_then(|turn| turn.started_at.as_deref().and_then(parse_ms)),
        turn.and_then(|turn| turn.ended_at.as_deref().and_then(parse_ms)),
    ]
    .into_iter()
    .flatten()
    .max()
    .or_else(|| parse_ms(facts.created_at));
    let passed_wake = ms(&triage.snoozed_until).filter(|until| *until <= now);
    [activity, ms(&triage.last_wake_at), passed_wake]
        .into_iter()
        .flatten()
        .max()
}

/// Whether the auto-settle sweep settles the thread now, and as of when
/// (triage §8): `Some(inactive since)` for a quiet candidate whose last
/// activity is more than `after_ms` ago.
pub fn auto_settle_at(triage: &Triage, facts: &Facts, now: i64, after_ms: i64) -> Option<i64> {
    let candidate = !facts.archived
        && triage.settled.is_none()
        && triage.pinned_at.is_none()
        && triage.auto_settle_off_at.is_none()
        && !facts.pending_request
        && !facts.active
        && facts.queued == 0
        && !effectively_snoozed(triage, facts, now);
    if !candidate {
        return None;
    }
    inactive_since(triage, facts, now).filter(|since| *since < now - after_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: &str = "2026-11-01T00:00:00.000Z";

    fn at(text: &str) -> i64 {
        parse_ms(text).unwrap()
    }

    fn fresh() -> Triage {
        Triage {
            bumped_at: T0.to_owned(),
            ..Triage::default()
        }
    }

    fn idle<'a>(turn: Option<&'a TurnSummary>) -> Facts<'a> {
        Facts {
            archived: false,
            status: ThreadStatus::Idle,
            active: false,
            pending_request: false,
            queued: 0,
            latest_turn: turn,
            latest_user_message_at: turn.map(|turn| turn.requested_at.as_str()),
            created_at: T0,
        }
    }

    fn turn(status: TurnStatus, requested: &str, ended: Option<&str>) -> TurnSummary {
        TurnSummary {
            id: "turn_1".into(),
            status,
            requested_at: requested.into(),
            started_at: Some(requested.into()),
            ended_at: ended.map(str::to_owned),
            error_kind: None,
        }
    }

    #[test]
    fn visits_move_forward_only_and_never_bump() {
        let mut triage = fresh();
        visit(&mut triage, "2026-11-02T00:00:00Z").unwrap();
        assert_eq!(
            triage.visited_at.as_deref(),
            Some("2026-11-02T00:00:00.000Z")
        );
        visit(&mut triage, "2026-11-01T12:00:00Z").unwrap();
        assert_eq!(
            triage.visited_at.as_deref(),
            Some("2026-11-02T00:00:00.000Z")
        );
        assert!(visit(&mut triage, "soon").is_err());
        assert_eq!(triage.bumped_at, T0);
    }

    #[test]
    fn mark_unread_rewinds_before_the_end_and_retires_a_passed_snooze() {
        let done = turn(
            TurnStatus::Completed,
            "2026-11-02T00:00:00Z",
            Some("2026-11-02T00:05:00Z"),
        );
        let running = turn(TurnStatus::Running, "2026-11-02T00:00:00Z", None);
        let mut triage = fresh();
        let error = mark_unread(&mut triage, &idle(Some(&running)), at(T0)).unwrap_err();
        assert_eq!(
            error.message,
            "This thread has no finished turn to mark unread."
        );
        triage.snoozed_until = Some("2026-11-02T01:00:00.000Z".into());
        triage.snoozed_at = Some("2026-11-02T00:30:00.000Z".into());
        let now = at("2026-11-02T02:00:00Z");
        mark_unread(&mut triage, &idle(Some(&done)), now).unwrap();
        assert_eq!(
            triage.visited_at.as_deref(),
            Some("2026-11-02T00:04:59.999Z")
        );
        assert_eq!(
            triage.last_wake_at.as_deref(),
            Some("2026-11-02T01:00:00.000Z")
        );
        assert!(triage.snoozed_until.is_none() && triage.snoozed_at.is_none());
        assert_eq!(triage.bumped_at, T0);
    }

    #[test]
    fn settle_preconditions_and_a_second_settle_keeps_its_time() {
        let mut triage = fresh();
        let mut facts = idle(None);
        facts.active = true;
        facts.status = ThreadStatus::Running;
        let error = settle(&mut triage, &facts, at(T0), None).unwrap_err();
        assert_eq!(
            error.message,
            "This thread is still running. Stop it before settling."
        );
        facts.pending_request = true;
        assert!(
            settle(&mut triage, &facts, at(T0), None)
                .unwrap_err()
                .message
                .contains("question")
        );
        let mut facts = idle(None);
        facts.queued = 1;
        assert!(
            settle(&mut triage, &facts, at(T0), None)
                .unwrap_err()
                .message
                .contains("queued")
        );
        let mut facts = idle(None);
        facts.archived = true;
        assert_eq!(
            settle(&mut triage, &facts, at(T0), None)
                .unwrap_err()
                .message,
            "This thread is archived."
        );

        let facts = idle(None);
        triage.active_order = Some("m".into());
        triage.unsettled_at = Some(T0.into());
        let first = at("2026-11-02T00:00:00Z");
        settle(&mut triage, &facts, first, None).unwrap();
        assert_eq!(triage.settled, Some(Settled::Settled));
        assert_eq!(
            triage.settled_at.as_deref(),
            Some("2026-11-02T00:00:00.000Z")
        );
        assert!(triage.active_order.is_none() && triage.unsettled_at.is_none());
        assert_eq!(triage.bumped_at, "2026-11-02T00:00:00.000Z");
        settle(&mut triage, &facts, first + 1000, None).unwrap();
        assert_eq!(
            triage.settled_at.as_deref(),
            Some("2026-11-02T00:00:00.000Z")
        );
        assert_eq!(triage.bumped_at, "2026-11-02T00:00:00.000Z");
    }

    #[test]
    fn unsettle_opts_out_and_pin_unsettles() {
        let facts = idle(None);
        let mut triage = fresh();
        settle(&mut triage, &facts, at(T0), None).unwrap();
        let now = at("2026-11-02T00:00:00Z");
        pin(&mut triage, &facts, now, Some("m")).unwrap();
        assert_eq!(triage.settled, Some(Settled::Active));
        assert!(triage.settled_at.is_none());
        assert_eq!(triage.pin_order.as_deref(), Some("m"));
        order_pinned(&mut triage, &facts, now, "q").unwrap();
        assert_eq!(triage.pin_order.as_deref(), Some("q"));
        let pinned_at = triage.pinned_at.clone();
        assert!(pinned_at.is_some());
        assert_eq!(
            order_active(&mut triage, &facts, "c").unwrap_err().message,
            "This thread is not active, so it can't be reordered."
        );
        unpin(&mut triage, &facts, now).unwrap();
        assert!(triage.pinned_at.is_none() && triage.pin_order.is_none());
        assert_eq!(
            order_pinned(&mut triage, &facts, now, "q")
                .unwrap_err()
                .message,
            "This thread is not pinned, so it can't be reordered."
        );
        assert_eq!(
            order_active(&mut triage, &facts, " ").unwrap_err().message,
            "The order key can't be empty."
        );
        let bumped = triage.bumped_at.clone();
        order_active(&mut triage, &facts, "c").unwrap();
        assert_eq!(triage.bumped_at, bumped, "active reorder never bumps");

        let mut triage = fresh();
        unsettle(&mut triage, &facts, now).unwrap();
        assert_eq!(triage.settled, Some(Settled::Active));
        assert_eq!(
            triage.unsettled_at.as_deref(),
            Some("2026-11-02T00:00:00.000Z")
        );
    }

    #[test]
    fn snooze_gate_wake_and_settled_snoozes() {
        let now = at("2026-11-02T00:00:00Z");
        let mut triage = fresh();
        let mut facts = idle(None);
        assert_eq!(
            snooze(&mut triage, &facts, now, "2026-11-01T00:00:00Z")
                .unwrap_err()
                .message,
            "The snooze time is not in the future."
        );
        facts.pending_request = true;
        assert!(
            snooze(&mut triage, &facts, now, "2026-11-03T00:00:00Z")
                .unwrap_err()
                .message
                .contains("question")
        );
        facts.pending_request = false;
        facts.queued = 2;
        assert!(
            snooze(&mut triage, &facts, now, "2026-11-03T00:00:00Z")
                .unwrap_err()
                .message
                .contains("queued")
        );
        facts.queued = 0;
        facts.status = ThreadStatus::Starting;
        assert!(snooze(&mut triage, &facts, now, "2026-11-03T00:00:00Z").is_err());

        let facts = idle(None);
        settle(&mut triage, &facts, now, None).unwrap();
        snooze(&mut triage, &facts, now, "2026-11-03T00:00:00+00:00").unwrap();
        assert_eq!(
            triage.snoozed_until.as_deref(),
            Some("2026-11-03T00:00:00.000Z")
        );
        assert!(triage.settled.is_none() && triage.settled_at.is_none());
        assert!(triage.unsettled_at.is_some());

        let later = now + 1000;
        wake(&mut triage, &facts, later).unwrap();
        assert!(triage.snoozed_until.is_none());
        assert_eq!(triage.last_wake_at, Some(iso(later)));
        assert_eq!(triage.bumped_at, iso(later));
        wake(&mut triage, &facts, later + 1000).unwrap();
        assert_eq!(
            triage.last_wake_at,
            Some(iso(later)),
            "a second wake changes nothing"
        );
        assert_eq!(triage.bumped_at, iso(later));

        snooze(&mut triage, &facts, later, "2026-11-04T00:00:00Z").unwrap();
        pin(&mut triage, &facts, later + 5, None).unwrap();
        assert!(triage.snoozed_until.is_none());
        assert_eq!(triage.last_wake_at, Some(iso(later + 5)));
    }

    #[test]
    fn a_sent_message_clears_settle_and_snooze_and_sets_the_first_visit() {
        let now = at("2026-11-02T00:00:00Z");
        let mut triage = fresh();
        triage.settled = Some(Settled::Settled);
        triage.settled_at = Some(T0.into());
        triage.snoozed_until = Some("2026-11-05T00:00:00.000Z".into());
        triage.snoozed_at = Some(T0.into());
        on_user_message(&mut triage, now);
        assert!(triage.settled.is_none() && triage.settled_at.is_none());
        assert_eq!(triage.unsettled_at, Some(iso(now)));
        assert!(triage.snoozed_until.is_none() && triage.last_wake_at.is_none());
        assert_eq!(triage.visited_at, Some(iso(now - 1)));
        assert_eq!(triage.bumped_at, iso(now));
    }

    #[test]
    fn effective_snooze_wakes_on_questions_and_later_failures() {
        let now = at("2026-11-02T00:00:00Z");
        let mut triage = fresh();
        triage.snoozed_until = Some("2026-11-03T00:00:00.000Z".into());
        triage.snoozed_at = Some("2026-11-01T12:00:00.000Z".into());
        let facts = idle(None);
        assert!(effectively_snoozed(&triage, &facts, now));
        let mut asking = facts;
        asking.pending_request = true;
        assert!(!effectively_snoozed(&triage, &asking, now));
        let early = turn(
            TurnStatus::Failed,
            "2026-11-01T00:00:00Z",
            Some("2026-11-01T01:00:00Z"),
        );
        let mut failed = idle(Some(&early));
        failed.status = ThreadStatus::Failed;
        assert!(
            effectively_snoozed(&triage, &failed, now),
            "failed before the snooze"
        );
        let late = turn(
            TurnStatus::Failed,
            "2026-11-01T13:00:00Z",
            Some("2026-11-01T14:00:00Z"),
        );
        let mut failed = idle(Some(&late));
        failed.status = ThreadStatus::Failed;
        assert!(!effectively_snoozed(&triage, &failed, now));
        triage.snoozed_until = Some("not a time".into());
        assert!(!effectively_snoozed(&triage, &facts, now));
    }

    #[test]
    fn auto_settle_waits_three_days_and_counts_wakes_as_activity() {
        let ended = "2026-11-01T00:00:00.000Z";
        let done = turn(TurnStatus::Completed, "2026-10-31T23:00:00Z", Some(ended));
        let facts = idle(Some(&done));
        let triage = fresh();
        let exactly = at(ended) + AUTO_SETTLE_AFTER_MS;
        assert_eq!(
            auto_settle_at(&triage, &facts, exactly, AUTO_SETTLE_AFTER_MS),
            None
        );
        assert_eq!(
            auto_settle_at(&triage, &facts, exactly + 1, AUTO_SETTLE_AFTER_MS),
            Some(at(ended))
        );

        let mut woke = fresh();
        woke.last_wake_at = Some("2026-11-02T00:00:00.000Z".into());
        assert_eq!(
            auto_settle_at(&woke, &facts, exactly + 1, AUTO_SETTLE_AFTER_MS),
            None
        );
        let mut passed = fresh();
        passed.snoozed_until = Some("2026-11-02T00:00:00.000Z".into());
        assert_eq!(
            auto_settle_at(&passed, &facts, exactly + 1, AUTO_SETTLE_AFTER_MS),
            None
        );
        assert!(
            auto_settle_at(
                &passed,
                &facts,
                at("2026-11-05T00:00:01Z"),
                AUTO_SETTLE_AFTER_MS
            )
            .is_some()
        );

        for opted_out in [
            Triage {
                pinned_at: Some(T0.into()),
                ..fresh()
            },
            Triage {
                auto_settle_off_at: Some(T0.into()),
                ..fresh()
            },
            Triage {
                settled: Some(Settled::Active),
                ..fresh()
            },
        ] {
            assert_eq!(
                auto_settle_at(&opted_out, &facts, exactly + 1, AUTO_SETTLE_AFTER_MS),
                None
            );
        }
        let mut queued = facts;
        queued.queued = 1;
        assert_eq!(
            auto_settle_at(&triage, &queued, exactly + 1, AUTO_SETTLE_AFTER_MS),
            None
        );
    }
}
