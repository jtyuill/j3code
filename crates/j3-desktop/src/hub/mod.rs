//! The agents hub (spec §5.3): the thread's subagent roster and the selected subagent's
//! assignment and transcript, in place of the feed and composer while open. ←← in an empty
//! composer opens it (only while the thread has subagents); Esc or ←← closes it and the
//! composer takes the keyboard back.
//!
//! The roster lists running subagents first, then the most recently updated. The selected
//! subagent's transcript is read from its omp session file through
//! `connection.subagent_transcript` a slice at a time (`from_byte` → `next_byte`), right
//! away while more is waiting and every [`POLL_INTERVAL`] while it runs. Keys: ↑/↓ or k/j
//! select, PageUp/PageDown scroll the transcript, x stops the selected subagent.

mod double_tap;
pub mod view;

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use j3_client::{ThreadState, t3::orchestration::Subagent};
use j3_protocol::j3::{SubagentTranscript, TranscriptEntry};
use rune::{Key, KeyEvent, TextEdit, Tx};

pub use double_tap::{DoubleTap, tap_time};
pub use view::view;

use crate::{app::J3, sidebar::ThreadKey};

/// How often the selected transcript is reread while its subagent runs (seconds).
pub const POLL_INTERVAL: f64 = 0.5;

/// The transcript's scroll view (PageUp/PageDown page it).
pub const TRANSCRIPT_KEY: &str = "hub-transcript";

/// One thread's hub.
#[derive(Default)]
pub struct AgentHub {
    pub open: bool,
    /// The selected subagent's id.
    pub selected: Option<String>,
    transcripts: HashMap<String, Transcript>,
    /// Expanded tool calls and results, by `subagent/index`.
    expanded: HashSet<String>,
    /// ←← closes the hub.
    left: DoubleTap,
    /// A transcript read is on its way.
    reading: bool,
    /// When the next read may start (clock seconds).
    next_read: f64,
    /// Where keystrokes for a field of this thread's cards go when the field (or the
    /// thread) went between building a frame and the key arriving.
    sink: TextEdit,
}

/// What has been read of one subagent's transcript.
#[derive(Default)]
pub struct Transcript {
    pub entries: Vec<TranscriptEntry>,
    next_byte: u64,
    /// At least one read succeeded.
    pub loaded: bool,
    /// Read after the subagent finished, so nothing more will come.
    settled: bool,
    /// The last read was capped: more is waiting.
    more: bool,
    pub error: Option<String>,
}

/// `state`'s subagents, running ones first, then the most recently updated, then by id.
pub fn roster(state: &ThreadState) -> Vec<&Subagent> {
    let mut roster: Vec<&Subagent> = state.subagents().iter().collect();
    roster.sort_by(|a, b| {
        b.status
            .is_active()
            .cmp(&a.status.is_active())
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    roster
}

impl AgentHub {
    pub fn transcript(&self, subagent_id: &str) -> Option<&Transcript> {
        self.transcripts.get(subagent_id)
    }

    pub fn is_expanded(&self, entry: &str) -> bool {
        self.expanded.contains(entry)
    }

    pub(crate) fn edit_sink(&mut self) -> &mut TextEdit {
        &mut self.sink
    }

    /// Keeps a selection that still exists, else selects the roster's first; true when the
    /// selection changed.
    fn follow(&mut self, state: &ThreadState) -> bool {
        if self
            .selected
            .as_ref()
            .is_some_and(|id| state.subagent(id).is_some())
        {
            return false;
        }
        let first = roster(state).first().map(|subagent| subagent.id.clone());
        let changed = first != self.selected;
        self.selected = first;
        self.next_read = 0.0;
        changed
    }

    /// The read the selected transcript needs now, if any: (subagent id, from byte).
    fn next_fetch(&self, state: &ThreadState) -> Option<(String, u64)> {
        let id = self.selected.as_ref()?;
        let active = state.subagent(id)?.status.is_active();
        let transcript = self.transcripts.get(id);
        let due = transcript.is_none_or(|t| match t.error {
            // Not found yet (a subagent that just started): keep trying while it runs.
            Some(_) => !t.settled,
            None => !t.loaded || t.more || active || !t.settled,
        });
        due.then(|| (id.clone(), transcript.map_or(0, |t| t.next_byte)))
    }

    /// Applies one read of `subagent_id` from `from_byte`; true when more is waiting.
    fn apply(
        &mut self,
        subagent_id: &str,
        from_byte: u64,
        result: Result<SubagentTranscript, j3_client::Error>,
        active: bool,
    ) -> bool {
        let transcript = self.transcripts.entry(subagent_id.to_owned()).or_default();
        // A read that raced a reset or another read is dropped.
        if transcript.next_byte != from_byte {
            return true;
        }
        let read = match result {
            Ok(read) => read,
            Err(error) => {
                transcript.error = Some(error.to_string());
                transcript.settled = !active;
                return false;
            }
        };
        if read.reset {
            transcript.entries.clear();
            self.expanded
                .retain(|entry| !entry.starts_with(&format!("{subagent_id}/")));
        }
        transcript.entries.extend(read.entries);
        transcript.next_byte = read.next_byte;
        transcript.loaded = true;
        transcript.more = read.more;
        transcript.settled = !active && !read.more;
        transcript.error = None;
        read.more
    }

    /// Selects the roster entry `delta` rows away (clamped); true when it moved.
    fn move_selection(&mut self, state: &ThreadState, delta: isize) -> bool {
        let roster = roster(state);
        if roster.is_empty() {
            return false;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|id| roster.iter().position(|subagent| &subagent.id == id))
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, roster.len() as isize - 1) as usize;
        let id = roster[next].id.clone();
        self.select(id)
    }

    fn select(&mut self, id: String) -> bool {
        if self.selected.as_ref() == Some(&id) {
            return false;
        }
        self.selected = Some(id);
        self.next_read = 0.0;
        true
    }
}

impl J3 {
    /// Opens `key`'s hub, like omp only when the thread has subagents; the first of the
    /// roster is selected and ←← closes it again.
    pub fn open_hub(&mut self, key: ThreadKey, _: &mut Tx) {
        let Some(thread) = self.threads.get(&key) else {
            return;
        };
        if thread.state.subagents().is_empty() {
            return;
        }
        let state = thread.state.clone();
        let hub = self.hubs.entry(key).or_default();
        hub.open = true;
        hub.left = DoubleTap::default();
        hub.follow(&state);
        hub.next_read = 0.0;
    }

    /// Closes `key`'s hub and returns focus to the composer.
    pub fn close_hub(&mut self, key: &ThreadKey, tx: &mut Tx) {
        if let Some(hub) = self.hubs.get_mut(key) {
            hub.open = false;
        }
        self.focus_composer(tx);
    }

    /// Selects subagent `id` in `key`'s hub, its transcript from the top.
    pub fn select_subagent(&mut self, key: &ThreadKey, id: &str, tx: &mut Tx) {
        if let Some(hub) = self.hubs.get_mut(key)
            && hub.select(id.to_owned())
        {
            tx.scroll_to(TRANSCRIPT_KEY, 0.0, 0.0);
        }
    }

    /// Expands or folds transcript row `entry` (`subagent/index`).
    pub fn toggle_hub_entry(&mut self, key: &ThreadKey, entry: &str) {
        if let Some(hub) = self.hubs.get_mut(key)
            && !hub.expanded.remove(entry)
        {
            hub.expanded.insert(entry.to_owned());
        }
    }

    /// Stops the selected subagent while it runs (`connection.stop_subagent`).
    pub fn stop_selected_subagent(&mut self, key: &ThreadKey) {
        let Some(state) = self.threads.get(key).map(|thread| thread.state.clone()) else {
            return;
        };
        let Some(subagent) = self
            .hubs
            .get(key)
            .and_then(|hub| hub.selected.as_ref())
            .and_then(|id| state.subagent(id))
            .filter(|subagent| subagent.status.is_active())
        else {
            return;
        };
        let Some(run_id) = subagent.run_id.clone() else {
            return;
        };
        let subagent_id = subagent.id.clone();
        self.stop_subagent(key, &run_id, &subagent_id);
    }

    /// A key for the open thread's hub while nothing has focus (the window's `on_key`):
    /// true when the hub took it.
    pub(crate) fn hub_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        if tx.focused().is_some() {
            return false;
        }
        let Some(key) = self.open_thread_key() else {
            return false;
        };
        self.hubs.get(&key).is_some_and(|hub| hub.open) && self.on_hub_key(&key, ev, tx)
    }

    /// A key for `key`'s hub: ←← and Esc close, ↑/↓ or k/j select, PageUp/PageDown scroll,
    /// x stops. Keys with modifiers pass.
    pub fn on_hub_key(&mut self, key: &ThreadKey, ev: &KeyEvent, tx: &mut Tx) -> bool {
        let mods = ev.mods;
        if mods.cmd || mods.ctrl || mods.alt || mods.logo || mods.shift {
            return false;
        }
        let now = tap_time(tx, self.clock.t);
        let Some(state) = self.threads.get(key).map(|thread| thread.state.clone()) else {
            return false;
        };
        let Some(hub) = self.hubs.get_mut(key).filter(|hub| hub.open) else {
            return false;
        };
        if ev.key == Key::Left {
            // A held key's repeats are no taps (as the composer's ←←).
            if !tx.key_repeat() && hub.left.tap(now) {
                self.close_hub(key, tx);
            }
            return true;
        }
        hub.left.reset();
        let moved = match &ev.key {
            Key::Up => hub.move_selection(&state, -1),
            Key::Down => hub.move_selection(&state, 1),
            Key::Text(t) if t == "k" => hub.move_selection(&state, -1),
            Key::Text(t) if t == "j" => hub.move_selection(&state, 1),
            Key::PageUp => {
                tx.scroll_page(TRANSCRIPT_KEY, -1);
                false
            }
            Key::PageDown => {
                tx.scroll_page(TRANSCRIPT_KEY, 1);
                false
            }
            Key::Text(t) if t == "x" => {
                self.stop_selected_subagent(key);
                false
            }
            Key::Escape => {
                self.close_hub(key, tx);
                false
            }
            _ => return false,
        };
        if moved {
            tx.scroll_to(TRANSCRIPT_KEY, 0.0, 0.0);
        }
        true
    }

    /// Starts the read the open hub's selected transcript needs, when it is due; asks for
    /// a frame when the next one will be. True when the selection changed.
    pub(crate) fn poll_hub(&mut self, key: &ThreadKey, tx: &mut Tx) -> bool {
        let Some(state) = self.threads.get(key).map(|thread| thread.state.clone()) else {
            return false;
        };
        let now = self.clock.t;
        let Some(hub) = self.hubs.get_mut(key).filter(|hub| hub.open) else {
            return false;
        };
        let changed = hub.follow(&state);
        if hub.reading {
            return changed;
        }
        let Some((subagent_id, from_byte)) = hub.next_fetch(&state) else {
            return changed;
        };
        if now < hub.next_read {
            tx.wake_after(Duration::from_secs_f64(hub.next_read - now));
            return changed;
        }
        let Some(connection) = self.thread_connection(key) else {
            return changed;
        };
        if let Some(hub) = self.hubs.get_mut(key) {
            hub.reading = true;
        }
        let (key, thread_id) = (key.clone(), key.thread_id.clone());
        self.spawn(async move {
            let result = connection
                .subagent_transcript(&thread_id, &subagent_id, from_byte)
                .await;
            move |s: &mut J3, tx: &mut Tx| s.hub_read(&key, &subagent_id, from_byte, result, tx)
        });
        changed
    }

    /// A transcript read came back: it lands, and the next is due at once while more is
    /// waiting, else after [`POLL_INTERVAL`].
    fn hub_read(
        &mut self,
        key: &ThreadKey,
        subagent_id: &str,
        from_byte: u64,
        result: Result<SubagentTranscript, j3_client::Error>,
        tx: &mut Tx,
    ) {
        let active = self
            .threads
            .get(key)
            .and_then(|thread| thread.state.subagent(subagent_id))
            .is_some_and(|subagent| subagent.status.is_active());
        let now = self.clock.t;
        let Some(hub) = self.hubs.get_mut(key) else {
            return;
        };
        hub.reading = false;
        if hub.apply(subagent_id, from_byte, result, active) {
            hub.next_read = now;
        } else {
            hub.next_read = now + POLL_INTERVAL;
            tx.wake_after(Duration::from_secs_f64(POLL_INTERVAL));
        }
    }
}

#[cfg(test)]
mod tests;
