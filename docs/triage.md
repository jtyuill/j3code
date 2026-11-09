# Thread triage (desktop Inbox)

j3's own behaviour spec for the desktop thread list: per-thread status, sections and order, unread, settle, snooze, pin and reorder, auto-settle. The server and the desktop implement it against protocol v2 (see [m8.md D6](m8.md#d6-desktop-thread-triage-is-kept-but-re-specified)). Fields and commands marked *current wire name* are the names they had when this spec was written; `docs/protocol/wire.md` §4.2.2 and §5.8 give their v2 names.

Vocabulary: a **thread** is one conversation; a **turn** is one prompt run until omp's `session_settled`; a **pending request** is an open ask/select/confirm/input/editor prompt from omp; the **queue** holds messages sent while a turn runs. "Now" is the wall clock in UTC; timestamps compare as epoch milliseconds. A timestamp that does not parse counts as absent unless a rule says otherwise.

## 1. Scope

The desktop sidebar follows one of two workflows, chosen in Settings ▸ General and stored as `ui.workflow` in `desktop.json`:

| Workflow | Persisted value | Covered here |
|---|---|---|
| **Inbox** (default): Pinned, Active, then the Working, Snoozed and Settled shelves | `"inbox"` (an older value is migrated on load) | Yes |
| **herdr**: spaces with rolled-up attention, then every agent with a state glyph | `"herdr"` | No. It reads thread status only and has no triage state. |

The git quick action (commit/push/pull) lives beside the workflows in the client crate but is not a workflow and is out of scope.

Inbox triage state lives on the server, per thread. The desktop derives everything else from the thread list row on each render. The phone app is out of scope.

## 2. Thread fields

The server keeps these on each thread and sends them on every thread list row. Names are *current wire names* (camelCase JSON).

| Field | Meaning |
|---|---|
| `settledOverride` | `null`, `"settled"` or `"active"`. `"settled"`: the thread is in Settled. `"active"`: the user un-settled it, which also blocks auto-settle. `null`: no explicit choice. |
| `settledAt` | When the thread settled. Set by settle, cleared by unsettle, pin of a settled thread, and a sent message. |
| `unsettledAt` | When it last came back from Settled. Feeds the Active order. |
| `snoozedUntil` | The wake time. Stays set after the timer passes until something clears it (see §7). |
| `snoozedAt` | When the snooze was set. |
| `lastSnoozeWakeAt` | When the last snooze ended by an explicit wake (wake, pin, mark-unread retiring a passed snooze). |
| `pinnedAt` | Set while pinned. |
| `pinOrderKey` | Position within Pinned (an order key, §5.4); null for a keyless pin. |
| `activeOrderKey` | Position within Active when Active is manually ordered; null for keyless. |
| `autoSettleDisabledAt` | Set while auto-settle is off for this thread. |
| `lastVisitedAt` | The read watermark. |
| `archivedAt`, `deletedAt` | Set when archived or deleted. |

The status inputs are derived by the server from the thread's turns and requests, and are read-only for triage:

| Field | Derivation |
|---|---|
| `status` | The active turn's status, else the latest turn's, else `idle`. Turn statuses: `preparing`, `queued`, `starting`, `running`, `waiting`, `completed`, `interrupted`, `failed`, `cancelled`, `rolled_back`, plus `idle`. |
| `activityRunStatus` | The active turn's status when it is `preparing`/`starting`/`running`/`waiting`, else null. |
| `activeRunId`, `latestRunId` | The active turn and the newest turn. |
| `latestRunRequestedAt`, `latestRunStartedAt`, `latestRunCompletedAt` | The newest turn's timestamps. |
| `activityRunStartedAt` | The active turn's start. |
| `pendingRuntimeRequest` | The first pending request, with its kind. Every omp request (ask/select/confirm/input/editor) has kind `user_input`. |
| `lastErrorClass` | The failure class of the newest turn if it failed (`usage_limit` matters here). |
| `latestUserMessageAt` | Newest user message, not counting queued ones. |
| `activeProviderThreadId` | Set once omp has a session for the thread. |
| `lineage` | Parent, relationship (`subagent`, `fork`), side-chat flag. |
| `updatedAt` | Last change time, as used by triage (see the bump rules in §6). |

Fields the client reads but j3's server never sets: `blockedOnThreadWait`, `pendingGuestMessageCount`, `latestUserAuthoredMessageAt`, `hasActionableProposedPlan`, `pendingBackgroundTasks`. The rules that use them are given anyway so the reimplementation can decide whether to keep them. They are marked *(unreached in j3)*.

## 3. Per-thread status

### 3.1 Runtime and latest turn

Two derived values feed the status:

- **Latest turn**: present iff `latestRunId` is set. Its status is `status`, except that `idle` reads as `completed`. Its completion time is `latestRunCompletedAt`. If that field is absent (not null, but missing) and the status is `idle` or terminal, `updatedAt` stands in.
- **Runtime**: present iff `latestRunId` or `activeProviderThreadId` is set, or background work holds completion *(unreached in j3)*. Its status is `idle` when background work holds completion and `status` ≠ `failed`; otherwise `activityRunStatus` when set; otherwise `status`.

A status is **active** if it is one of `preparing`, `queued`, `starting`, `running`, `waiting`.

### 3.2 Status ladder

The first matching row wins.

| # | Status | Condition | Row label |
|---|---|---|---|
| 1 | Approval | Pending request whose kind is not `user_input` or `auth_refresh` *(unreached in j3)* | Approval |
| 2 | Input | Pending request of kind `user_input` | Input |
| 3 | Request | `pendingGuestMessageCount` > 0 *(unreached in j3)* | Request |
| 4 | Ready | No runtime | none |
| 5 | Waiting | Runtime `running` and `blockedOnThreadWait` *(unreached in j3)* | Waiting |
| 6 | Working | Runtime status active | Working |
| 7 | Waiting | Runtime `idle` | Waiting |
| 8 | Limited | Runtime `failed` and `lastErrorClass` = `usage_limit` | Limited |
| 9 | Failed | Runtime `failed` | Failed |
| 10 | Ready | Anything else | "Done" if unread (§4), else none |

Status follows the latest turn only. A thread whose newest turn completed after an earlier failure reads Ready.

### 3.3 Working timer

Working and Waiting rows show the time elapsed since the current stretch of work started:

1. If `activityRunStartedAt` is present on the row (even null), use it. Null means no timer.
2. Else, if the latest turn has no completion and is the active turn, use its start, else its request time.
3. Else no timer.

Format: under 60 s `"{s}s"`, under 60 min `"{m}m"`, else `"{h}h {m}m"`, with each part rounded down and negative values clamped to 0.

### 3.4 Receding

A row is drawn quietly (receded) unless it is selected or its status is Input or Request. Otherwise Working and Waiting rows always recede, Ready and Approval rows recede unless unread or woke, and Failed and Limited never recede.

### 3.5 Badge

A row's status word, also its accessible label suffix (`"{title}, {badge}"`), is the first that applies:

1. In the Snoozed section: "Snoozed". The visible pill shows the countdown (§7.5).
2. Woke (§7.4): "Woke".
3. The status label from §3.2.
4. In the Settled section: "Settled".

Settled rows draw no status pill, only their age. Their accessible badge still follows the order above.

## 4. Unread and visits

### 4.1 Unread

A thread is **unread** iff all of these hold:

- the latest turn has a non-empty, parseable completion time `C`;
- `lastVisitedAt` is present and non-empty;
- `lastVisitedAt` does not parse, or `C` > `lastVisitedAt`.

A thread with no visit marker is never unread. Unread only affects Ready's label ("Done") and receding. Unread failures exist as a derivation (a failed latest turn completed after the last visit, or never visited) but no desktop surface uses them.

### 4.2 What sets the watermark

| Trigger | Effect | Command (*current wire name*) |
|---|---|---|
| The server accepts the first user message of a thread whose `lastVisitedAt` is unset | `lastVisitedAt` = now − 1 ms, so a turn that completes in the same millisecond is unread | none (part of sending a message) |
| Opening a thread in the main pane (click, keyboard nav, settle-and-advance target), unless it is already open | Client sends a visit at the client's now | `thread.visit` with `visitedAt`, `opened: true` |
| While a thread is open, its row becomes unread | Client sends a visit once per (thread, completion time) pair | `thread.visit` |
| Mark unread (row menu) | `lastVisitedAt` = latest turn's completion − 1 ms | `thread.mark-unread` |

A visit moves the watermark forward only; a `visitedAt` at or before the stored one leaves it unchanged. A visit whose `visitedAt` does not parse is refused ("The visit time is not a valid timestamp."). Visits and mark-unread never change `updatedAt`.

### 4.3 Mark unread

- Refused when the latest turn has no completion time ("This thread has no finished turn to mark unread."). The menu item is disabled when the latest turn has no `endedAt`.
- If `snoozedUntil` ≤ now (a passed snooze), mark-unread retires it: `lastSnoozeWakeAt` = old `snoozedUntil`, then `snoozedUntil` and `snoozedAt` are cleared. This keeps the rewound watermark from showing "Woke" again.
- Allowed on archived threads.

## 5. Sections and order

### 5.1 Listed threads

A thread gets a row only if it is not a subagent (`lineage.relationshipToParent` ≠ `subagent`), not a side chat, not archived and not deleted. Forks are listed.

### 5.2 Section membership

The first matching row wins:

| # | Section | Condition |
|---|---|---|
| 1 | Snoozed | Effectively snoozed (§7.3) |
| 2 | Settled | `settledOverride` = `"settled"` |
| 3 | Pinned | `pinnedAt` set |
| 4 | Working | Working shelf on (setting), the thread's environment is connected, and the thread is working (below) |
| 5 | Active | Otherwise |

A thread is **working** iff it has no pending request and the runtime is active or `idle`, unless it is a plan-mode turn that finished with a plan to act on *(unreached in j3)*.

As a result: a snoozed pinned thread is in Snoozed and returns to Pinned on wake. A settled pinned thread is in Settled. A working pinned thread stays in Pinned. A failed thread is not working.

### 5.3 Per-section order

Rows from all connected environments merge into one list. Thread ids are unique only within an environment, so every final tie-break is thread id ascending, then environment key ascending. Missing or unparseable timestamps count as 0.

| Section | Order |
|---|---|
| Pinned | Keyed rows first, by `pinOrderKey` ascending (byte order). Then keyless rows, newest `createdAt` first. |
| Active, working shelf **on** (time ordered) | Newest "return" first. Return = max(`createdAt`, `unsettledAt`, latest turn requested, latest turn completed). `activeOrderKey` is ignored. |
| Active, working shelf **off** (manual order) | Keyless rows first, newest anchor first, where anchor = max(`createdAt`, `unsettledAt`). Then keyed rows by `activeOrderKey` ascending. |
| Working | Newest of max(`createdAt`, last user-sent time) first. Last user-sent time = `latestUserAuthoredMessageAt` if the row carries that field (even null), else the latest turn's request time. |
| Snoozed | Soonest `snoozedUntil` first. |
| Settled | Most recently settled first. Settled time = `settledAt` if parseable, else the latest of `latestUserMessageAt` and the latest turn's requested/started/completed times, else `updatedAt`. |

### 5.4 Order keys

Pinned and Active positions are strings over `a`–`z` read as base-26 fractions in (0, 1), compared as strings.

- **Valid key**: non-empty, lowercase ASCII letters only, and not ending in `a`.
- **Key between** `before` and `after` (either may be open): refused if a given bound is invalid or `before` ≥ `after`. Otherwise:
  - Strip the common prefix of the two bounds and recurse on the rest; a missing digit of `before` reads as `a`.
  - With first digits `da` (0 when `before` is open or empty) and `db` (26 when `after` is open or empty): if `db − da > 1`, the result is the single digit ⌈(da + db) / 2⌉.
  - Otherwise, if `after` has more than one digit, take its first digit.
  - Otherwise take `da` followed by the key between the rest of `before` and an open bound.
- **Spread keys** for `n` rows: the smallest width `w ≥ 2` with 26^w > 2(n+1). Let step = 26^w/(n+1). Row `i` (0-based) gets round(step·(i+1)), plus 1 if that value is a multiple of 26, written as `w` base-26 digits.
- **Reorder plan** for moving row `M` within a section's visible rows (already rearranged). Keys of rows hidden from view (filtered out, collapsed) are reserved and never reused.
  1. If each neighbour of `M` is either absent or keyed, take the key between them. While that key equals a reserved key, take the key between it and the after-neighbour. If a key results, write only that key for `M`.
  2. Otherwise compute spread keys for (visible + reserved) rows, drop the reserved ones, and assign the rest in order to the visible rows. Write only the keys that differ from the row's current key.

Keys are trimmed by the server; an empty key is refused ("The order key can't be empty.").

### 5.5 Rendered list and shelves

The list renders in this order:

1. The Pinned rows, then a divider.
2. The Active rows.
3. A Working shelf header with its rows, only when Working is non-empty.
4. A Snoozed shelf header with its rows, only when Snoozed is non-empty.
5. A Settled shelf header, which is always present, with its rows.

- Shelf headers read `"{Working|Snoozed|Settled} {count}"`, where the count includes rows hidden by collapse or paging.
- Shelves start collapsed. Expanding or collapsing saves the set of expanded shelves to `ui.expandedShelves` in `desktop.json`.
- A collapsed shelf still shows the open thread's row if that thread belongs to it.
- Settled pages: 10 rows at first, and "Show N more" adds 25. Toggling the Settled shelf resets paging to 10. Paging is not persisted.
- A project filter (not persisted) limits rows to one project. Rows hidden by the filter still reserve their order keys.
- Keyboard navigation (↑/↓, next/previous thread, ⌘1…) walks the visible rows in display order and wraps. With nothing open it starts at the first row (next) or the last (previous).

### 5.6 Clock

The list re-lays itself at the earliest of:

- 50 ms after the next `snoozedUntil` that is still in the future, across listed threads;
- 1 s later, if any visible row shows a working timer;
- otherwise, the next whole minute, for relative ages.

## 6. Settle, unsettle, pin, reorder

### 6.1 Server commands

Every command is refused on an archived thread ("This thread is archived."), except visit and mark-unread. "Bump" means `updatedAt` = now.

| Action | Command (*current wire name*) | Preconditions (refusal text) | Effect | Bumps when |
|---|---|---|---|---|
| Settle | `thread.settle` (optional `settledAt`, which must parse) | No pending request ("This thread is waiting on an approval or question. Answer it before settling."). No active turn ("This thread is still running. Stop it before settling."). Nothing queued ("This thread has a queued message. Send it or remove it from the queue before settling."). | `settledOverride` = settled. `settledAt` = given or now, unless already settled with `settledAt` set and not pinned, in which case it is kept. Clears `unsettledAt`, `pinnedAt`, `pinOrderKey`, `activeOrderKey`, `snoozedUntil`, `snoozedAt`. | Not already settled, or it was snoozed |
| Unsettle | `thread.unsettle` (`reason: user`) | none | `settledOverride` = active. `settledAt` cleared. `unsettledAt` = now, kept if already active. | Not already active |
| Pin | `thread.pin` (optional `orderKey`) | Valid key | If not pinned: `pinnedAt` = now, and `pinOrderKey` = key if one is given. If settled: `settledOverride` = active and `settledAt` cleared. Snooze cleared, and if a snooze was set, `lastSnoozeWakeAt` = now. | Newly pinned, was settled, or was snoozed |
| Unpin | `thread.unpin` | none | Clears `pinnedAt` and `pinOrderKey`. | Was pinned |
| Pin reorder | `thread.pin.reorder` (`orderKey`) | Pinned ("This thread is not pinned, so it can't be reordered.") | `pinOrderKey` = key. | Key changed |
| Active reorder | `thread.active.reorder` (`orderKey`) | Not pinned and not settled ("This thread is not active, so it can't be reordered.") | `activeOrderKey` = key. | Never |
| Auto-settle opt-out | `thread.auto-settle.set` (`enabled`) | none | Disabling sets `autoSettleDisabledAt` = now, keeping the first timestamp if it is already disabled. Enabling clears it. | Value changed |

Every accepted command is applied and broadcast, even when it changes nothing.

### 6.2 What a sent message does

When the server accepts a user message, or a queued one starts:

- If `lastVisitedAt` is unset: `lastVisitedAt` = now − 1 ms.
- If `settledOverride` is set (either value): it is cleared to null and `settledAt` cleared. `unsettledAt` = now if it was settled, else unchanged. Bump.
- If `snoozedUntil` is set: snooze cleared. Bump. `lastSnoozeWakeAt` is not set.

### 6.3 Desktop entry points

| Entry | Pinned/Active row | Settled row | Snoozed row | Working row |
|---|---|---|---|---|
| Hover action | Settle | Un-settle | Wake | none |
| Row menu, item 1 | "Pin thread" (no key) or "Unpin thread" | same | same | same |
| Row menu, item 2 | "Settle thread" or "Un-settle thread" (by `settledOverride`) | same | same | same |
| Row menu, item 3 | "Snooze ▸", disabled if snoozing is not allowed (§7.1) | same | "Wake thread" | "Snooze ▸" |

The rest of the row menu: "Rename thread…", "Mark unread" (disabled without a completed turn), "Auto-settle ▸ Enabled / Disabled" (current value checked), "Archive thread", "Delete…". Choosing an item closes the menu.

**Settle-and-advance** (⌘⇧S, on the open thread):

- If the open thread is settled: un-settle it and stay on it.
- Otherwise settle it and open the next row in visible display order, wrapping, skipping rows that are the current thread, settled, effectively snoozed, or unknown. If no such row exists, open a new thread in the settled thread's project.

Commands from one gesture run in order on their servers and stop at the first failure.

### 6.4 Drag and drop

Dragging a row shows a drop line at the Pinned boundary. The section a slot belongs to is decided by the markers before it: after the Pinned divider it is Active, after a shelf header it is that shelf.

- Rows in Working never move. Nothing drops into Working or Snoozed.
- Dropping onto a Settled row or the Settled header settles the dragged row, unless it is already settled.
- Drop slot: a Pinned row's own slot; an Active row's own slot when Active is manually ordered, else the Active placeholder (top of Active); for Settled, the Settled placeholder.
- **Into Pinned** from Pinned: if the order changed, run the reorder plan (§5.4) over the new Pinned order and send a pin reorder per assignment. No assignments means no-op.
- **Into Pinned** from elsewhere: pin the row with its planned key (or no key if the plan did not assign it), then send pin reorders for the other rows' assignments.
- **Into Active, time ordered**: a no-op from Active. From elsewhere: unpin if it was pinned, unsettle if it was settled, wake if it was in Snoozed, in that order. No keys are written.
- **Into Active, manual order**: a no-op if it came from Active and the order is unchanged. Otherwise the same lifecycle commands, then an active reorder per assignment.
- **Rows on servers that cannot store order** (offline). The planner takes an optional writable set per section; the desktop passes "all writable" today.
  - Drop if the moved row is not writable, or if any assignment targets a non-writable row.
  - Drop an Active placement that writes a key above a frozen row (offline and keyless), or a Pinned placement that writes a key below one.
  - Keyed offline rows count as fixed bounds.
- **Servers without settlement support**: drops into or out of Settled are refused. The desktop always declares support.

## 7. Snooze

### 7.1 Setting a snooze

**Client gate (menu enabled):** there is no pending request, no guest message *(unreached)*, and no queued turn start. A **queued turn start** means either of:

- the runtime is `preparing`, `queued` or `starting`;
- `latestUserMessageAt` is within ±2 min of now and no turn has adopted it: there is no latest turn, or the latest turn's requested, started and completed times are all present-and-earlier or absent.

**Server command:** `thread.snooze` with `snoozedUntil` (*current wire name*). Refusals, in order:

1. Archived.
2. Wake not after now ("The snooze time is not in the future.").
3. Pending request ("This thread is waiting on an approval or question and can't be snoozed.").
4. Something queued ("This thread has a queued message and can't be snoozed.").

It sets `snoozedUntil` (normalized to millisecond ISO) and `snoozedAt` = now, and bumps. Pin and settle state are untouched.

**Menu presets.** These are computed in the system time zone with calendar days, so wakes land on the right day across DST. The defaults are, as settings:

| id | Meaning |
|---|---|
| `after:60` | In 1 hour |
| `after:180` | In 3 hours |
| `at:today:18:00` | 18:00 today |
| `at:tomorrow:09:00` | 09:00 tomorrow |
| `at:monday:09:00` | 09:00 next Monday |

Resolving presets:

- `after:N` wakes at now + N minutes, keeping the clock's seconds.
- A weekday wakes at its next occurrence after today; the same weekday as today means 7 days ahead.
- An `at` preset whose time has already passed today is left out.
- Presets are sorted by wake minute and deduplicated per minute; the earlier setting wins ties.
- Labels:
  - `after:N` reads "In N minutes/hours/days": days when N is a whole number of days, hours when it is a whole number of hours, else minutes, with singular for 1.
  - `at` presets 0 days ahead read "This morning" (before 12), "This afternoon" (12–16) or "This evening" (17+).
  - 1 day ahead reads "Tomorrow".
  - Monday reads "Next week".
  - 7 days ahead reads "Next {Weekday}".
  - Otherwise "{Weekday}". Labels follow the real day gap, so Monday seen on a Sunday reads "Tomorrow".
- The secondary line is the time as `"6:00 PM"`. When the label does not name the day ("In …" or "Next week") and the wake is on another date, it is prefixed with the short weekday (`"Thu 2:00 AM"`).
- Picking a preset resolves the menu again at pick time. A duration preset starts at the pick; a set-time preset keeps the wake it showed if that wake is still ahead, else the pick does nothing.

**Custom… dialog.**

- *Duration*: amount (default "1", decimals allowed) of Minutes, Hours (default) or Days. A day is 24 h. The amount must be finite and > 0.
- *Date and time*: `YYYY-MM-DD` and `HH:MM` (default "09:00") in local time. A non-existent local time (rolled-over date, DST gap) is refused. A repeated hour takes the earlier instant.
- The result must be after now, or the dialog shows "That snooze time has passed or isn’t valid. Choose another time." and stays open.

### 7.2 Raised hand

A snoozed thread **raises its hand** when any of these hold:

- it has a pending request, or a guest message *(unreached)*;
- the runtime is `failed` and the failure is after the snooze: the failure time (the failed latest turn's completion, else `updatedAt`) is after `snoozedAt`, or `snoozedAt` is unset.

Completions, new turns and ongoing work never raise a hand.

### 7.3 Effectively snoozed

A thread is effectively snoozed iff `snoozedUntil` parses, is after now, and the thread has not raised its hand. A malformed `snoozedUntil` never hides a thread.

### 7.4 Woke

**Wake time** (only while `snoozedUntil` parses):

- If the hand is raised: the failure time if it is after the snooze, else the runtime's `updatedAt`, else `snoozedAt`. An old failure never replaces the time of a request wake.
- Otherwise, once `snoozedUntil` ≤ now: `snoozedUntil`.
- Otherwise none.

**Woke marker** ("Woke" badge, alarm-clock pill, row does not recede) shows iff:

- the row carries `lastVisitedAt` (null counts as present);
- `settledOverride` ≠ settled;
- there is a wake time `W`;
- `lastVisitedAt` is null, unparseable, or earlier than `W`.

Visiting at or after `W` clears it.

### 7.5 Ending a snooze

| Cause | Fields after |
|---|---|
| Timer passes | Nothing changes server side. The row leaves Snoozed by derivation and shows Woke until visited. |
| Raised hand | Same: derived, no write. |
| Wake (`thread.unsnooze`, `reason: user`; hover, menu, drag to Active) | `snoozedUntil` and `snoozedAt` cleared. If a snooze was set: `lastSnoozeWakeAt` = now and bump; otherwise a no-op. |
| Pin | Cleared, and `lastSnoozeWakeAt` = now if one was set. |
| Settle | Cleared; `lastSnoozeWakeAt` not set. |
| User message | Cleared; `lastSnoozeWakeAt` not set. |
| Mark unread after the timer passed | `lastSnoozeWakeAt` = old `snoozedUntil`, then cleared. |

Countdown label for a snoozed row:

| Time left | Label |
|---|---|
| ≤ 0 | "now" |
| < 1 h | `"{⌈minutes⌉}m"`, at least 1 |
| < 1 d | `"{⌈hours⌉}h"` |
| otherwise | `"{⌈days⌉}d"` |

## 8. Auto-settle

The server settles quiet threads by itself.

- **Sweep**: once at server start, then every 5 minutes, with a threshold of 3 days.
- **Candidate**: all of these hold:
  - not archived and not deleted;
  - `settledOverride` is null, so an explicit unsettle opts the thread out until its next user message resets the override;
  - not pinned and `autoSettleDisabledAt` unset;
  - no pending request, no guest message, `activityRunStatus` null, no pending background tasks;
  - no queued turn start (server form: `latestUserMessageAt` within ±2 min of now, `status` ≠ failed, and no turn has adopted the message, as in §7.1);
  - not snoozed (server form: `snoozedUntil` after now, no pending request or guest message, and not "failed after snoozing", where that means `status` = failed and either `snoozedAt` is unset or `latestRunCompletedAt` > `snoozedAt`).
- **Inactive since**: the latest of these:
  - the activity time, which is the latest of `latestUserMessageAt` and the latest turn's requested, started and completed times, or else `createdAt`;
  - `lastSnoozeWakeAt`;
  - `snoozedUntil`, if it is ≤ now.
- **Due** iff inactive since < now − 3 days.
- **Settle**: the thread is settled with `settledAt` = the inactive-since time, not now, under the settle rules of §6.1.
- **Race guard**: the sweep picks threads from stored rows, then asks each thread to re-check on live state. The settle is refused if the thread changed after the stored row's `updatedAt`, or if `settledOverride` is set by then. User actions always win.
- Refusals are logged at debug level; no event is emitted.

## 9. Persistence and ownership

| State | Owner | Stored in | Written by (*current wire name*) | Survives restart |
|---|---|---|---|---|
| `settledOverride`, `settledAt`, `unsettledAt` | Server | Thread projection and the `threads.shell` JSON row in SQLite | `thread.settle`, `thread.unsettle`, `thread.pin`, sent message, auto-settle sweep | Yes |
| `snoozedUntil`, `snoozedAt`, `lastSnoozeWakeAt` | Server | same | `thread.snooze`, `thread.unsnooze`, `thread.pin`, `thread.settle`, `thread.mark-unread`, sent message | Yes |
| `pinnedAt`, `pinOrderKey` | Server | same | `thread.pin`, `thread.unpin`, `thread.pin.reorder`, `thread.settle` | Yes |
| `activeOrderKey` | Server | same | `thread.active.reorder`, `thread.settle` | Yes |
| `autoSettleDisabledAt` | Server | same | `thread.auto-settle.set` | Yes |
| `lastVisitedAt` | Server | same | `thread.visit`, `thread.mark-unread`, first sent message | Yes |
| Status inputs (§2) | Server, derived from turns and requests | same | turn lifecycle | Yes |
| Workflow choice | Desktop | `desktop.json` `ui.workflow` | Settings ▸ General | Yes |
| Working shelf on/off (default on) | Desktop | `desktop.json` `ui.workingShelf` | Settings ▸ General "Working shelf" | Yes |
| Expanded shelves | Desktop | `desktop.json` `ui.expandedShelves` | shelf header click | Yes |
| Project filter, Settled paging, open menu, drag, last auto-visited completion | Desktop memory | none | | No |
| Snooze presets | Desktop constant | none | | n/a |

All triage state is per thread and per server. Two desktops on one server see the same Inbox, apart from the desktop-local rows of this table.

## 10. Rules

Each rule is testable on its own. "Thread" means a listed thread unless the rule says otherwise.

**Status**

1. A pending `user_input` request reads Input even while running and blocked on a thread wait. A non-`user_input` request reads Approval, above Input.
2. A guest message reads Request, below Approval and Input.
3. A failed latest turn with class `usage_limit` reads Limited. The same thread reads Working once a turn runs, and Ready once a turn completes. Limited never recedes.
4. A failed latest turn reads Failed. A completed one reads Ready even with a stored error. Runtime `idle` reads Waiting.
5. Running reads Working; running and blocked on a thread wait reads Waiting; a pending input beats the wait; clearing the wait reads Working again.
6. A completed turn with unawaited background command/monitor work reads Ready, unread, not receded, not working. Awaited work reads Waiting, receded, working.
7. No working timer when the latest turn completed and there is no activity start. A newer queued or cancelled turn shares the activity start. An active turn without a start uses its request time.
8. Duration labels: 59 s → "59s", 125 s → "2m", 3900 s → "1h 5m".
9. Ready shows no label when read and "Done" when unread. Input shows "Input"; Failed shows "Failed" regardless of unread.

**Unread**

10. Without `lastVisitedAt` (absent or null), a completed thread is not unread.
11. Visited before the completion: unread. Visited at the completion's millisecond: read.
12. A failure is unseen if never visited or visited before it, and seen if visited after it.
13. The first accepted user message sets `lastVisitedAt` while the turn is still active.
14. A visit with an earlier time than the stored one leaves `lastVisitedAt` unchanged. A visit never changes `updatedAt`.
15. Mark unread sets `lastVisitedAt` to completion − 1 ms, leaves `updatedAt` unchanged, and is refused while the only turn is still active.

**Sections and order**

16. Subagents and side chats get no row; forks do.
17. Archived threads get no row. Settled threads go to Settled even when pinned. A snoozed pinned thread goes to Snoozed and returns to Pinned once the timer passes.
18. Snoozed rows sort soonest wake first. The next-wake time is the earliest future `snoozedUntil`.
19. A running thread goes to Working only when the shelf is on and its environment is connected. A pinned running thread stays Pinned.
20. Pinned: keyed rows by key, then keyless rows newest `createdAt` first. Equal keys tie-break by id.
21. Manual Active: keyless rows first by newest of created/unsettled, then keyed rows by key. Equal keys or times tie-break by thread id, then environment.
22. Time-ordered Active: the thread whose turn finished last tops the list, ahead of a newer thread and a recently reopened one. A running turn without a completion counts its request time.
23. Working: newest user send first; a thread with no send falls back to `createdAt`.
24. Settled: newest settled time first, using the latest turn completion when `settledAt` is absent. Equal times tie-break by id. Settled time prefers `settledAt`, then latest activity, then `updatedAt`, and skips a malformed `settledAt`.
25. Planning a reorder for 1200 keyless rows yields keys that sort them in the requested order.
26. Repeated insertion at the top or in the middle of a keyed list always yields strictly ordered keys.
27. Key-between returns nothing for invalid bounds or bounds out of order.
28. A reorder never reuses a hidden row's key, whether as midpoint or spread key.
29. Spread keys are unique and each has room before it.

**Drag and drop**

30. A drop slot inside Working or Snoozed has no target. A Working row never moves.
31. Drop verbs: into Pinned pins, into Settled settles, from Pinned to Active unpins, from Settled to Active unsettles, from Snoozed to Active wakes. Same-section moves have no verb.
32. With Active time ordered, a drop into Active only changes lifecycle (unpin/unsettle/wake) and writes no keys; from Active it is a no-op.
33. Offline handling: an offline drag source is refused; a keyed offline row is kept as a bound; rebalancing that would rewrite an offline key is refused; placements across a frozen keyless row are refused in Active (above it) and in Pinned (below it); placements beside one that need no frozen write are allowed; fallback spread assignments are checked against the boundary too.
34. Settling via drop is refused from Settled to Settled and on servers without settlement support.
35. Desktop: the hover action on an Active row sends Settle. Dragging onto the Settled shelf sends Settle and does not open the row. Dragging onto the Pinned boundary sends Pin with a key first.

**Settle-and-advance and menu**

36. Settle-and-advance from a row opens the next row that is not settled or snoozed. From the last row it wraps. With none left it opens a new thread in the project. On a settled thread it sends Unsettle and stays.
37. Forward navigation skips parked rows, wraps, and returns nothing when the current row is the only one or is not in the list.
38. The row menu sends Pin(no key), MarkUnread, AutoSettle(false), and Snooze(a time after now). Custom… opens the dialog. A settled row offers Un-settle; a snoozed row offers Wake.
39. A collapsed shelf hides its rows from the list and from keyboard order, but keeps the open thread's row. Expanding a shelf persists it.

**Settle and pin (server)**

40. Settle is refused while a turn is active and while a request is pending. A second settle keeps `settledAt` and `updatedAt`.
41. Unsettle sets override active, clears `settledAt`, and sets `unsettledAt`.
42. Active reorder is refused on settled and pinned threads, refused with an empty key, and never changes `updatedAt`.
43. Pin with a key stores it. Pin reorder changes the key and keeps `pinnedAt`. Unpin clears both. Pin reorder after unpin is refused.
44. Settle clears `activeOrderKey` and `unsettledAt`. Pinning a settled thread sets override active and clears `settledAt`.
45. Archived threads refuse pin and settle.

**Snooze**

46. Snooze is refused for a past time, while a request is pending (checked first), and while a message is queued.
47. Pinning a snoozed thread clears the snooze and sets `lastSnoozeWakeAt`.
48. Unsnooze clears and records the wake. A second unsnooze changes neither `lastSnoozeWakeAt` nor `updatedAt`.
49. A sent message on a settled, snoozed thread clears the override to null, clears `settledAt`, sets `unsettledAt`, and clears the snooze.
50. Effective snooze: a malformed wake time never hides. A pending request of any kind wakes early. A failure completed after the snooze wakes, and so does any failure when `snoozedAt` is unset. A failure completed before the snooze does not. Ongoing work and turns completing after the snooze do not wake.
51. Wake time reported: the failure's completion for a failure wake; the runtime's `updatedAt` for a request wake; `snoozedUntil` once the timer passes. An early failure wake keeps its time after the timer also passes.
52. The client snooze gate refuses pending requests, a user message within 2 min that no turn has adopted, and a `starting` runtime. It allows an hour-old message, and clears once a turn requested at the message time exists. A message 3 min in the future is not queued.
53. Woke shows until `lastVisitedAt` ≥ the wake time. A null or invalid `lastVisitedAt` still shows it. It hides while still snoozed, when settled, when the row lacks `lastVisitedAt` entirely, and when `snoozedUntil` is cleared.
54. A row snoozed for 3 s leaves the list, the clock arms for ≤ 3.1 s, and the row returns badged "Woke" with no user action.
55. Countdown labels: 0 → "now", 20 s → "1m", 45 min → "45m", 61 min → "2h", 30 h → "2d".
56. Presets: listed soonest first; a passed "today" preset is dropped; a same-weekday preset moves to next week; labels follow the real day gap; two presets in the same minute show once; a duration and a set time in the same minute show once.
57. Picking a duration starts it at the pick. Picking a set time keeps its shown wake across midnight. Calendar presets stay on the right day across DST. Time labels read like "6:00 PM".
58. Custom input: invalid or non-future dates are refused; durations resolve from confirmation time; invalid amounts are refused; local times in a spring-forward gap are refused; a day is 24 h across DST.

**Auto-settle**

59. Not due at exactly 3 days after the last activity, due 1 ms later, with `settledAt` = the last activity.
60. A passed snooze timer counts as activity at its wake time, and so does `lastSnoozeWakeAt`.
61. Opted-out, pinned and unsettled (override active) threads never auto-settle.
62. A thread changed after the sweep's snapshot is not settled, and a thread already settled is not settled again.
63. In a sweep, only the quiet thread settles. Pinned, opted-out and snoozed threads stay. A thread woken recently settles only after its wake + 3 days. A second sweep settles nothing new. An explicit unsettle survives later sweeps.
64. Every triage field (override, pin key, opt-out, snooze, settled state) survives a server restart, in both the projection and the list row.
65. The server's queued-start test: a message 1 s old with an older turn is queued, and so is one with no turn. It is not queued outside ±2 min, once a turn requested at the message time exists, or when the status is failed.

## 11. Test map

| Rules | Tests | File |
|---|---|---|
| 1 | `awaiting_input_beats_a_running_runtime_below_approval` | `crates/j3-client/src/workflow/status.rs` |
| 2 | `a_waiting_guest_message_is_a_request_below_the_agents_own` | same |
| 3 | `usage_limit_stops_stay_limited_until_the_thread_recovers` | same |
| 4 | `failed_only_while_the_latest_run_failed` | same |
| 5 | `a_thread_wait_reads_waiting_and_resumes_working` | same |
| 6 | `unseen_completion_with_background_work`, `the_working_shelf_takes_running_threads_and_background_waits_only` | same |
| 7, 8 | `working_started_at_does_not_invent_a_start` | same |
| 9 | `labels_follow_the_mobile_list` | same |
| 10–12 | `unread_completions_and_failures` | same |
| 13–15, 40–49 | `engine_app_triage_commands` | `crates/j3-core/tests/engine_app.rs` |
| 15 (round trip) | `settle_workflow_calls` | `crates/j3-client/tests/client.rs` |
| 16 | `subagents_and_side_chats_get_no_row` | `crates/j3-client/src/workflow/sections.rs` |
| 17 | `ordered_sections_exclude_settled_snoozed_and_archived_rows`, `a_settled_pinned_thread_goes_to_the_settled_shelf`, `snooze_hides_a_pinned_thread_and_wake_restores_it` | same |
| 18 | `snoozed_rows_wake_soonest_first_and_set_the_next_wake` | same |
| 19 | `working_threads_fold_into_the_shelf_unless_disconnected` | same |
| 20 | `keyed_pinned_threads_lead_keyless_ones_in_creation_order`, `equal_pin_keys_break_by_id` | same |
| 21 | `new_and_reopened_threads_lead_the_saved_order`, `honors_a_saved_active_order_and_leaves_new_threads_above_it`, `equal_keys_and_times_break_by_thread_then_environment` | same |
| 22 | `the_thread_that_finished_last_tops_the_inbox`, `a_return_without_a_server_stamp_sorts_by_its_last_stamp` | same |
| 23 | `working_threads_order_by_the_last_message_sent` | same |
| 24 | `a_turn_completion_counts_as_activity_for_auto_settled_threads`, `settled_ties_break_by_id` (sections.rs); `settled_timestamp_prefers_the_stamp_then_activity_then_updated_at` (status.rs) | both |
| 25 | `materializes_a_large_active_list_in_the_requested_order` | sections.rs |
| 26–29 | `stays_strictly_ordered_under_repeated_top_insertion`, `stays_strictly_ordered_under_repeated_middle_insertion`, `returns_none_for_corrupt_or_out_of_order_bounds`, `keeps_hidden_slots_available_between_visible_neighbors`, `materializes_keyless_rows_without_overwriting_hidden_slots`, `spread_keys_are_unique_and_insertable` | `crates/j3-client/src/workflow/order.rs` |
| 30 | `never_drops_into_the_working_shelf`, `a_working_thread_never_moves` | sections.rs |
| 31 | `drop_verbs` | same |
| 32 | `only_changes_lifecycle_when_the_inbox_is_time_ordered` | same |
| 33 | `arranges_the_rows_it_can_write`, `replans_a_disconnected_drag_source`, `preserves_a_keyed_offline_bound`, `rejects_rebalancing_a_keyed_offline_bound`, `rejects_crossing_the_frozen_keyless_boundary`, `allows_realizable_moves_beside_a_frozen_keyless_row`, `checks_fallback_assignments_at_the_boundary`, `rejects_impossible_pin_or_unpin_placement_across_the_boundary` | same |
| 34 | `settling_drops_and_unsupported_servers` | same |
| 35 | `the_hover_action_settles_and_dropping_on_settled_settles` | `crates/j3-desktop/src/sidebar/tests.rs` |
| 36 | `settle_and_advance_follows_the_sidebar_order` (sidebar/tests.rs); `t3_sections_settle_and_advance_pin_and_reorder` (`crates/j3-desktop/src/ui_tests/sidebar.rs`) | both |
| 37 | `forward_navigation_skips_parked_rows_and_wraps` | sections.rs |
| 38 | `the_row_menu_runs_thread_commands` | sidebar/tests.rs |
| 39 | `rows_say_their_status_and_collapsed_shelves_keep_the_open_thread`, `t3_lists_pinned_then_active_by_return_and_navigation_wraps` | same |
| 50 | `never_hides_on_malformed_wake_data`, `wakes_early_when_blocked_on_the_user`, `stays_snoozed_while_the_agent_keeps_working`, `stays_snoozed_when_a_run_requested_after_the_snooze_completes`, `raised_hand_for_approvals_input_and_failures` (status.rs); `snooze_mirror_wakes_on_requests_and_failures` (`crates/j3-core/src/engine/model/triage.rs`) | both |
| 51 | `uses_the_wake_event_time_despite_newer_metadata`, `reports_the_timer_wake_after_an_already_requested_run_completes`, `reports_hand_raise_activity_instead_of_a_completion`, `keeps_the_early_wake_after_the_scheduled_time_passes` | status.rs |
| 52 | `can_snooze_refuses_blocked_on_you_work`, `can_snooze_refuses_a_queued_turn_start`, `queued_state_clears_when_a_run_adopts_the_message` | same |
| 53 | `keeps_a_timer_wake_until_acknowledged_at_its_timestamp`, `woke_stays_hidden_while_snoozed_settled_or_untracked`, `keeps_an_early_failure_wake_acknowledged_after_the_scheduled_time` | same |
| 54 | `time_moves_rows_by_itself` (sidebar/tests.rs); `t3_snooze_hides_a_thread_until_it_wakes` (ui_tests/sidebar.rs) | both |
| 55 | `snooze_wake_labels_and_next_wake` | status.rs |
| 56, 57 | `lists_custom_presets_soonest_first_and_names_the_day_when_the_label_does_not`, `drops_a_passed_today_preset_and_moves_a_same_day_weekday_to_next_week`, `names_the_real_day_gap`, `shows_one_row_when_two_presets_wake_at_the_same_time`, `a_duration_and_a_set_time_in_the_same_minute_are_one_row`, `picking_a_duration_starts_it_at_the_pick`, `picking_a_set_time_keeps_its_shown_wake_across_midnight`, `calendar_days_stay_on_the_right_day_across_dst`, `default_labels_read_like_an_english_locale` | `crates/j3-client/src/workflow/snooze.rs` |
| 58 | `rejects_invalid_or_non_future_calendar_input`, `resolves_durations_from_the_confirmation_time`, `rejects_invalid_durations`, `rejects_nonexistent_local_times_at_spring_forward`, `duration_days_are_24_hours_across_dst` | same |
| 59–61 | `auto_settlement_waits_for_inactivity_and_snooze_wakes` | triage.rs |
| 62 | `auto_settle_yields_to_changes_after_the_snapshot` | same |
| 63, 64 | `engine_app_auto_settle_sweep_and_persistence` | engine_app.rs |
| 65 | `queued_turn_start_grace` | triage.rs |

Rules without a dedicated test today: the receding table (§3.4, partly covered by 3 and 6), the Settled paging sizes (§5.5), the clock's 50 ms and 1 s ticks (§5.6, partly covered by 54), the auto-visit of an open thread that becomes unread (§4.2), and the server's sweep interval.

## 12. Known inconsistencies today

The reimplementation should decide each of these explicitly.

1. **Two snooze gates.** The client allows snoozing when there is no pending request and no queued turn start (§7.1). The server refuses on a pending request or a queued message, and does not look at the 2-minute window or the runtime's starting states. Each side can accept what the other refuses.
2. **Two "snoozed" tests.**
   - For a failure wake, the client falls back to `updatedAt` when the failed turn has no completion time. The server then treats the thread as still snoozed.
   - The server's queued-start test ignores runtime status and excludes `status` = failed. The client's test includes `preparing`/`queued`/`starting` and has no failed exclusion.
3. **Settle offered where the server refuses.** The hover Settle (Pinned/Active) and the menu's "Settle thread" show for running threads and threads with a pending request; the server then refuses. With the Working shelf on, running unpinned threads are in Working and have no hover Settle, but the menu still offers Settle.
4. **Mark unread on the open thread.** The desktop auto-visits the open thread whenever its row becomes unread, once per (thread, completion). If the thread was read by opening it rather than by that auto-visit, marking it unread while it is open triggers the auto-visit and reads it again at once. The auto-visit is also not gated on window focus, though nearby code treats an unfocused window as "not watching".
5. **Snoozing a settled thread** is allowed. The row then sits in Snoozed (Snoozed beats Settled) and returns to Settled on wake. Settle clears snooze, but snooze does not clear settle.
6. **Keyless pins sort by `createdAt`, not `pinnedAt`.** The newest-created keyless pin leads, whatever the pin order was.
7. **Passed snoozes are never cleared by the server.** `snoozedUntil` stays set after the timer passes until a wake, pin, settle, user message or mark-unread. Auto-settle reads it as the last activity.
8. **Unreached inputs.** Approval, Request, the thread-wait Waiting, the background-work rules and the plan exception depend on fields j3's server never sets. Runtime `idle` (Waiting) arises only for a thread with an omp session and no turns.
9. **Unread failures** are computed (rule 12) but no desktop surface reads them. A failed row shows "Failed" whether or not it was seen.

### 12.1 Decisions (2026-11-09)

The v2 reimplementation follows these, and they override the rules above where they conflict. The guiding rule: the server decides, and a client offers an action only when the server would accept it.

1. **One snooze gate.** Snoozing is allowed when the thread has no open request, nothing queued (`queued > 0`) and no turn starting (status `starting`). The server enforces it; the client offers Snooze under the same condition. The ±2-minute "message not yet adopted" window is gone: v2 tells clients directly whether a turn is starting or queued.
2. **One "snoozed" test.** A thread is effectively snoozed iff `snoozedUntil` is after now, there is no open request, and it has not failed after snoozing. "Failed after snoozing" means status `failed` and the failed latest turn's `endedAt` (else `requestedAt`) is after `snoozedAt`, or `snoozedAt` is unset. Server (auto-settle) and client (sections) use this same rule.
3. **Settle only where the server accepts it.** Hover Settle and the menu's Settle are hidden for threads that are running, starting, waiting or stopping, or that have something queued.
4. **Auto-visit.** The open thread is marked visited once per newly finished turn, and only while the window is focused. Mark unread on the open thread is not undone by it.
5. **Snoozing clears settle.** `triage.snooze` on a settled thread clears `settled`/`settledAt` (as unsettle does, setting `unsettledAt`), so it wakes into Active.
6. **Keyless pins sort by `pinnedAt`**, newest first.
7. **Passed snoozes stay.** `snoozedUntil` is kept after it passes, because Woke needs it; auto-settle keeps counting a passed wake as activity. This is intended.
8. **Unreached inputs are dropped.** Approval, Request, the thread-wait Waiting, the background-work rules and the plan exception are not reimplemented. Every open request shows as Input.
9. **Unread failures are dropped**; a failed row shows "Failed" whether or not it was seen.
