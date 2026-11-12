//! The reader's place in a feed, kept while the feed is off screen. A feed's viewport is
//! mounted only while its thread is the one shown (another thread, Settings or the agents hub
//! takes its place), and a new viewport opens at its end; so when a feed leaves the screen
//! with the reader somewhere up it, the row at the top of the view and how far its top stood
//! from the viewport's are remembered, and put back when the feed shows again. A reader at
//! the end has no place: the feed opens at its end and follows it, as always.
//!
//! The place is a row and not a scroll offset because the list is virtual: a new viewport
//! knows only estimated heights for the rows it hasn't built, so the same offset is another
//! row. Rows and the viewport write where they draw into `Live` values (no handler, no view
//! run), read here when the feed leaves.

use std::time::Duration;

use rune::{Live, Tx};

use super::{FeedUi, anchor::VIEWPORT};
use crate::{app::J3, sidebar::ThreadKey, theme::Metrics};

/// A rect nothing has drawn into since its feed was last shown.
const UNDRAWN: [f32; 4] = [0.0, 0.0, 0.0, -1.0];
/// Seconds a returning feed is given to build the row and land on it.
const RETURN_FOR: f64 = 2.0;
/// Points off the place that count as there.
const NEAR: f32 = 0.75;

/// Where an element last drew, `[x, y, w, h]` in window points.
pub(super) struct Drawn(pub Live<[f32; 4]>);

impl Default for Drawn {
    fn default() -> Self {
        Self(Live::new(UNDRAWN))
    }
}

impl Drawn {
    /// The rect, once drawn since the feed was last shown.
    fn get(&self) -> Option<[f32; 4]> {
        Some(self.0.get()).filter(|rect| rect[3] >= 0.0)
    }
}

/// The row at the top of the view and where its top stood under the viewport's (negative:
/// above it, the row running out of the top), in window points.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Place {
    item: String,
    top: f32,
}

impl FeedUi {
    /// The value item `id`'s row writes its drawn rect into.
    pub(super) fn row_rect(&self, id: &str) -> Live<[f32; 4]> {
        self.rects
            .borrow_mut()
            .entry(id.to_owned())
            .or_default()
            .0
            .clone()
    }

    /// Whether the reader is away from the feed's end (the jump button shows).
    pub fn is_away(&self) -> bool {
        self.away
    }

    /// The first row in view that reaches below the viewport's top.
    fn top_row(&self) -> Option<Place> {
        let view = self.view_rect.get()?;
        let rects = self.rects.borrow();
        self.in_view.borrow().iter().find_map(|id| {
            let row = rects.get(id)?.get()?;
            (row[1] + row[3] > view[1] + NEAR).then(|| Place {
                item: id.clone(),
                top: row[1] - view[1],
            })
        })
    }

    /// The feed left the screen: remembers the reader's place, or none at the end. A feed
    /// that leaves before it got back to its place keeps that one.
    fn leave(&mut self) {
        if self.returning.is_none() {
            let [offset, max, _] = self.scroll.get();
            self.place = (max - offset > 1.0).then(|| self.top_row()).flatten();
        }
        self.returning = None;
        // What draws next time starts over: a row's old rect is not where it is.
        self.view_rect.0.set(UNDRAWN);
        for rect in self.rects.borrow().values() {
            rect.0.set(UNDRAWN);
        }
        self.in_view.borrow_mut().clear();
    }

    /// The feed shows again at `now`: the row to bring back (its key) and where its top
    /// stood, if it has a place.
    fn enter(&mut self, now: f64) -> Option<(String, f32)> {
        let place = self.place.as_ref()?;
        self.returning = Some(now + RETURN_FOR);
        Some((format!("feed-{}", place.item), place.top))
    }
}

impl J3 {
    /// The thread whose feed is on screen: the open one, unless Settings or its agents hub
    /// covers it.
    fn shown_feed(&self) -> Option<ThreadKey> {
        let key = self.open_thread_key().filter(|_| !self.route.settings)?;
        (!self.hubs.get(&key).is_some_and(|hub| hub.open)).then_some(key)
    }

    /// Before a frame, and from the handlers that bring a feed back (so the frame that
    /// first draws it draws it there): when the feed on screen changed, the one that left
    /// remembers its place and the one that came is sent back to its own; then, until it
    /// is there, the returning feed's row is put where it stood (its neighbours measure as
    /// they build, which moves it).
    pub(crate) fn poll_place(&mut self, tx: &mut Tx) {
        let now = self.clock.t;
        let zoom = Metrics::new(self.prefs.ui()).zoom;
        let shown = self.shown_feed();
        if shown != self.feed_shown {
            if let Some(thread) = self
                .feed_shown
                .take()
                .and_then(|key| self.threads.get_mut(&key))
            {
                thread.feed.leave();
            }
            if let Some((row, top)) = shown
                .as_ref()
                .and_then(|key| self.threads.get_mut(key))
                .and_then(|thread| thread.feed.enter(now))
            {
                // A virtual row far from the end isn't built until it is revealed; then it
                // is placed, in the same commit.
                tx.scroll_into_view(row.clone());
                tx.scroll_key_to(row, top / zoom);
            }
            self.feed_shown = shown.clone();
        }
        let Some(feed) = shown
            .and_then(|key| self.threads.get_mut(&key))
            .map(|thread| &mut thread.feed)
        else {
            return;
        };
        let (Some(until), Some(place)) = (feed.returning, feed.place.as_ref()) else {
            return;
        };
        let stands = feed.view_rect.get().and_then(|view| {
            let row = feed.rects.borrow().get(&place.item)?.get()?;
            Some(row[1] - view[1])
        });
        match stands {
            Some(top) if (top - place.top).abs() <= NEAR => feed.returning = None,
            _ if now >= until => feed.returning = None,
            Some(_) => tx.scroll_key_to(format!("feed-{}", place.item), place.top / zoom),
            None => {}
        }
        if feed.returning.is_some() {
            tx.wake_after(Duration::from_millis(16));
        }
    }

    /// Scrolls `key`'s feed to its end at once (the jump button).
    pub fn jump_to_feed_end(&mut self, key: &ThreadKey, tx: &mut Tx) {
        if let Some(thread) = self.threads.get_mut(key) {
            thread.feed.returning = None;
        }
        tx.scroll_to_bottom_instant(VIEWPORT);
    }
}
