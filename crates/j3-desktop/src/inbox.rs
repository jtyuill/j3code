//! The only path from async work (tokio tasks, worker threads) to UI state: a thread-safe
//! queue of updates to [`J3`], drained by the app's `poll` before every frame.

use std::sync::{Arc, Mutex};

use rune::{Tx, Waker};

use crate::app::J3;

/// One change to the app state, applied on the UI thread.
pub type Update = Box<dyn FnOnce(&mut J3, &mut Tx) + Send>;

#[derive(Clone, Default)]
pub struct Inbox {
    updates: Arc<Mutex<Vec<Update>>>,
    waker: Arc<Mutex<Option<Waker>>>,
}

impl Inbox {
    /// Queues `f` and wakes the UI so the next frame's `poll` runs it.
    pub fn post(&self, f: impl FnOnce(&mut J3, &mut Tx) + Send + 'static) {
        lock(&self.updates).push(Box::new(f));
        self.wake();
    }

    /// Asks for a frame (a no-op until [`Self::set_waker`] and in headless runs, where every
    /// frame polls anyway). Callable from a view, e.g. after recording a fetch request.
    pub fn wake(&self) {
        let waker = lock(&self.waker).clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    /// Installs the shell's waker (from `AppSpec::setup`).
    pub fn set_waker(&self, waker: Waker) {
        *lock(&self.waker) = Some(waker);
    }

    /// A waker that wakes the UI through this inbox, for APIs that want a `rune::Waker`
    /// before the shell's exists (`rune::ImageCache`).
    pub fn waker(&self) -> Waker {
        let inbox = self.clone();
        Arc::new(move || inbox.wake())
    }

    /// Runs every queued update in order, including ones queued while draining; true if any ran.
    pub fn drain(&self, s: &mut J3, tx: &mut Tx) -> bool {
        let mut ran = false;
        loop {
            let updates = std::mem::take(&mut *lock(&self.updates));
            if updates.is_empty() {
                return ran;
            }
            ran = true;
            for update in updates {
                update(s, tx);
            }
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
