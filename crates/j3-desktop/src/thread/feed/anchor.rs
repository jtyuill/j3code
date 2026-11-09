//! The sent message held at the top of the feed while its reply comes in, as the big chat apps
//! do. Sending in the open thread glides the feed until the new message sits [`PROMPT_TOP`]
//! under the viewport's top, and room is kept below the rows so the reply writes downward into
//! the view instead of pushing the conversation up past the reader.
//!
//! While the reply fits under the message, the room is a viewport less the message, so the
//! reply never runs out of it however far its growth trails its text, and the feed doesn't
//! follow its end. Once the drawn reply runs past the bottom of the view, the room goes and
//! the feed follows its end again for a reader who scrolls down to it, as it always does: the
//! reply keeps writing below the fold for one who doesn't. A moment after the run ends
//! ([`REST_DELAY`], once the growth has landed) a reply that stayed short keeps only the room
//! that holds the message at the top.

use std::time::Duration;

use j3_client::{ThreadState, ThreadSync, t3::orchestration::TurnItemKind};
use rune::{Cx, Tx};

use super::{END_PAD, FeedUi};
use crate::{app::J3, feed::is_hidden, motion, sidebar::ThreadKey};

/// Space between the viewport's top and a held message.
pub const PROMPT_TOP: f32 = 24.0;
/// Seconds after the run ends before the room below shrinks: the reply's growth lands first,
/// so the shrink never pulls the end up under the reader.
pub const REST_DELAY: f64 = 1.0;
/// Most seconds the glide to a newly held message is renewed. Rows above it may still be
/// climbing into their height (a previous reply's paced growth), and the feed's end reaches
/// the message's place only once they land; until then the glide rides that end.
const AIM_FOR: f64 = 3.0;
/// The feed's scroll viewport: `rune_kit::scroll_bar` keys it `feed` inside `feed-area`.
const VIEWPORT: &str = "feed-area/feed";
/// Points the drawn reply must run past the view's bottom before the room goes, so the reader
/// isn't left standing at the new end (which would start following on its own).
const OVERFLOW: f32 = 8.0;

/// The message the feed holds at its top.
#[derive(Debug, Clone)]
pub struct Anchor {
    /// Its turn item id.
    item: String,
    /// The glide to it hasn't started: its row hasn't been laid out yet.
    pending: bool,
    /// Polls it has stayed pending: past the first, its row didn't mount (the reader was far
    /// up the feed), so it is revealed.
    waited: u8,
    /// Until when the glide is renewed while the feed isn't there yet.
    aim_until: f64,
    /// Its run was seen active, so its end can be told.
    ran: bool,
    /// The drawn reply ran past the bottom of the view: no more room, and the feed follows its
    /// end for a reader who is there.
    overflowed: bool,
    /// When the room shrinks: `REST_DELAY` after the run was seen ending.
    rest_at: Option<f64>,
    rested: bool,
}

/// Where the held message is, from the layout its view last read: the scroll offset that puts
/// it at the top, and its height from there down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Held {
    pub offset: f32,
    pub head: f32,
}

impl Anchor {
    fn new(item: String) -> Self {
        Self {
            item,
            pending: true,
            waited: 0,
            aim_until: 0.0,
            ran: false,
            overflowed: false,
            rest_at: None,
            rested: false,
        }
    }

    /// The reply may still grow and fits under the message: the room stays a viewport tall
    /// and the feed doesn't follow its end.
    pub fn holds(&self) -> bool {
        !self.rested && !self.overflowed
    }

    pub fn item(&self) -> &str {
        &self.item
    }
}

/// The newest user message shown in `state`'s feed.
fn latest_prompt(state: &ThreadState) -> Option<&str> {
    state
        .feed()
        .iter()
        .rev()
        .map(|projected| &projected.item)
        .find(|item| matches!(item.kind, TurnItemKind::UserMessage(_)) && !is_hidden(item))
        .map(|item| item.id.as_str())
}

impl FeedUi {
    /// The held message, if any.
    pub fn anchor(&self) -> Option<&Anchor> {
        self.anchor.as_ref()
    }

    /// A new state replaces `old` for the thread: a user message new to an open, caught-up
    /// feed becomes the anchor, and the anchor's run ending starts its rest.
    pub(crate) fn track_anchor(
        &mut self,
        old: &ThreadState,
        new: &ThreadState,
        open: bool,
        now: f64,
        tx: &mut Tx,
    ) {
        let caught_up = old.projection.is_some()
            && old.sync == ThreadSync::Live
            && new.sync == ThreadSync::Live;
        if open
            && caught_up
            && let Some(id) = latest_prompt(new)
            && latest_prompt(old) != Some(id)
        {
            self.anchor = Some(Anchor::new(id.to_owned()));
            self.held.set(None);
        }
        let Some(anchor) = self.anchor.as_mut() else {
            return;
        };
        if new.active_run().is_some() {
            anchor.ran = true;
            anchor.rest_at = None;
            anchor.rested = false;
        } else if anchor.ran && !anchor.rested && anchor.rest_at.is_none() {
            anchor.rest_at = Some(now + REST_DELAY);
            tx.wake_after(Duration::from_secs_f64(REST_DELAY));
        }
    }

    /// Reads where the held message is laid out (`row`, with `top_pad` points of its own space
    /// above the message) in the viewport laid out at `viewport`, both unscrolled `[x, y, w, h]`.
    pub(super) fn measure_held(
        &self,
        viewport: Option<[f32; 4]>,
        row: Option<[f32; 4]>,
        top_pad: f32,
    ) {
        if let (Some(viewport), Some(row)) = (viewport, row) {
            let y = row[1] - viewport[1];
            let offset = (y + top_pad - PROMPT_TOP).max(0.0);
            self.held.set(Some(Held {
                offset,
                head: y + row[3] - offset,
            }));
        }
    }

    /// The room kept below the rows, in points. `viewport` and `last` (the last row) are
    /// unscrolled layouts, when laid out.
    pub(super) fn tail(&self, viewport: Option<[f32; 4]>, last: Option<[f32; 4]>) -> f32 {
        let (Some(anchor), Some(viewport)) = (&self.anchor, viewport) else {
            return 0.0;
        };
        let height = viewport[3];
        let Some(held) = self.held.get() else {
            // Not laid out yet: room enough for the glide to reach it.
            return height;
        };
        let live = (height - held.head - END_PAD).max(0.0);
        if anchor.holds() {
            return live;
        }
        last.map_or(live, |[_, y, _, h]| {
            let below = y + h - viewport[1] - held.offset;
            (height - below - END_PAD).max(0.0)
        })
    }
}

impl J3 {
    /// Before a frame, for the open thread `key`: starts the glide to a newly held message once
    /// its row is laid out (revealing it if it didn't mount: the reader was far up), renews the
    /// glide while the feed isn't there yet ([`AIM_FOR`]), drops the room once the drawn reply
    /// runs past the view, and lets it shrink once the run ended `REST_DELAY` ago. True when
    /// the feed must draw again.
    pub(super) fn poll_anchor(&mut self, key: &ThreadKey, tx: &mut Tx) -> bool {
        let now = self.clock.t;
        let Some(feed) = self.threads.get_mut(key).map(|thread| &mut thread.feed) else {
            return false;
        };
        let (held, [scroll, max, view]) = (feed.held.get(), feed.scroll.get());
        let Some(anchor) = feed.anchor.as_mut() else {
            return false;
        };
        if anchor.pending {
            if held.is_some() {
                anchor.pending = false;
                anchor.aim_until = now + AIM_FOR;
            } else {
                anchor.waited = anchor.waited.saturating_add(1);
                if anchor.waited == 2 {
                    tx.scroll_into_view(format!("feed-{}", anchor.item));
                }
            }
        }
        if let Some(held) = held
            && now < anchor.aim_until
        {
            if (scroll - held.offset).abs() > 0.5 {
                // A request arriving mid-glide keeps its speed, so renewing it is one motion.
                tx.scroll_to_with(VIEWPORT, 0.0, held.offset, motion::QUICK);
                tx.wake_after(Duration::from_millis(16));
            } else {
                anchor.aim_until = 0.0;
            }
        }
        // The scroll's end, less the room, is where the drawn rows end: past the view's bottom
        // with the message at the top, the reply no longer fits.
        if let Some(held) = held
            && anchor.holds()
            && view > 0.0
        {
            let room = (view - held.head - END_PAD).max(0.0);
            if max - room >= held.offset + OVERFLOW {
                anchor.overflowed = true;
                return true;
            }
        }
        if !anchor.rested && anchor.rest_at.is_some_and(|at| now >= at) {
            anchor.rested = true;
            return true;
        }
        false
    }

    /// Opening `key`'s thread drops a glide that didn't finish.
    pub(crate) fn settle_anchor(&mut self, key: &ThreadKey) {
        if let Some(anchor) = self
            .threads
            .get_mut(key)
            .and_then(|thread| thread.feed.anchor.as_mut())
        {
            anchor.pending = false;
            anchor.aim_until = 0.0;
        }
    }
}

/// The unscrolled layouts of the viewport, the held message's row and the last row.
pub(super) fn reads(cx: &Cx<J3>, held: Option<&str>, last: Option<&str>) -> [Option<[f32; 4]>; 3] {
    [Some(VIEWPORT), held, last].map(|key| key.and_then(|key| cx.layout(key)))
}
