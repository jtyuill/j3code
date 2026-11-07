//! Notifications (the old `window.push_notification`): a queue of toasts drawn bottom-right
//! with rune-kit's `Toast`/`ToastRegion`. Kinds Info/Success/Warning/Error, an optional
//! action button running a closure over the app state, and an optional thread a click
//! opens (attention alerts, spec §8.8). Each toast hides after [`SHOWN_FOR`] seconds.
//!
//! ```ignore
//! s.toasts.error("Couldn’t push: …");
//! s.toasts.push(Toast::new(ToastKind::Success, "Committed").action("View PR", |s, tx| ..));
//! ```

use std::rc::Rc;

use rune::{Color, Icon, Tx, icon};
use rune_kit::{Kit, ToastRegion};

use crate::{
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
    theme::Status,
};

/// Seconds a toast stays up.
pub const SHOWN_FOR: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

impl ToastKind {
    fn glyph(self, k: &Kit) -> (&'static Icon, Color) {
        let status = Status::of(k);
        match self {
            Self::Info => (&icons::INFO, status.info),
            Self::Success => (&icons::CIRCLE_CHECK, status.success),
            Self::Warning => (&icons::TRIANGLE_ALERT, status.warning),
            Self::Error => (&icons::CIRCLE_ALERT, status.danger),
        }
    }
}

/// What a toast's action button runs.
pub type ToastAction = Rc<dyn Fn(&mut J3, &mut Tx)>;

/// One notification.
pub struct Toast {
    pub kind: ToastKind,
    pub title: String,
    pub description: Option<String>,
    /// The button label and what it runs; the toast closes after.
    pub action: Option<(String, ToastAction)>,
    /// A click on the toast opens this thread.
    pub thread: Option<ThreadKey>,
    /// Replaces an earlier toast with the same key instead of stacking (one per thread).
    pub key: Option<String>,
}

impl Toast {
    pub fn new(kind: ToastKind, title: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            description: None,
            action: None,
            thread: None,
            key: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn action(
        mut self,
        label: impl Into<String>,
        f: impl Fn(&mut J3, &mut Tx) + 'static,
    ) -> Self {
        self.action = Some((label.into(), Rc::new(f)));
        self
    }

    pub fn thread(mut self, thread: ThreadKey) -> Self {
        self.key = Some(thread.element_id("attention"));
        self.thread = Some(thread);
        self
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
}

struct Entry {
    id: u64,
    toast: Toast,
    /// When it hides (`clock.t` seconds); set by the first poll after it was pushed.
    expires: Option<f64>,
}

#[derive(Default)]
pub struct Toasts {
    list: Vec<Entry>,
    next: u64,
}

impl Toasts {
    /// Shows `toast`, replacing one with the same key; returns its id.
    pub fn push(&mut self, toast: Toast) -> u64 {
        if let Some(key) = &toast.key {
            self.list
                .retain(|entry| entry.toast.key.as_ref() != Some(key));
        }
        self.next += 1;
        self.list.push(Entry {
            id: self.next,
            toast,
            expires: None,
        });
        self.next
    }

    pub fn info(&mut self, message: impl Into<String>) -> u64 {
        self.push(Toast::new(ToastKind::Info, message))
    }

    pub fn success(&mut self, message: impl Into<String>) -> u64 {
        self.push(Toast::new(ToastKind::Success, message))
    }

    pub fn warning(&mut self, message: impl Into<String>) -> u64 {
        self.push(Toast::new(ToastKind::Warning, message))
    }

    pub fn error(&mut self, message: impl Into<String>) -> u64 {
        self.push(Toast::new(ToastKind::Error, message))
    }

    /// An attention toast for `thread`: a click opens it; one per thread.
    pub fn push_thread(
        &mut self,
        kind: ToastKind,
        message: impl Into<String>,
        thread: ThreadKey,
    ) -> u64 {
        self.push(Toast::new(kind, message).thread(thread))
    }

    pub fn dismiss(&mut self, id: u64) {
        self.list.retain(|entry| entry.id != id);
    }

    /// The toasts on screen, oldest first: (id, toast).
    pub fn iter(&self) -> impl Iterator<Item = (u64, &Toast)> {
        self.list.iter().map(|entry| (entry.id, &entry.toast))
    }

    /// Times new toasts and hides expired ones; true when the list changed.
    pub(crate) fn poll(&mut self, t: f64, tx: &mut Tx) -> bool {
        let before = self.list.len();
        self.list
            .retain(|entry| entry.expires.is_none_or(|at| at > t));
        for entry in &mut self.list {
            if entry.expires.is_none() {
                entry.expires = Some(t + SHOWN_FOR);
            }
        }
        if let Some(next) = self.list.iter().filter_map(|e| e.expires).reduce(f64::min) {
            tx.wake_after(std::time::Duration::from_secs_f64((next - t).max(0.0)));
        }
        self.list.len() != before
    }
}

/// The toast region in the overlay layer.
pub fn view(s: &J3, ui: &mut Ui) {
    if s.toasts.list.is_empty() {
        return;
    }
    let k = ui.k;
    let mut region = ToastRegion::new();
    for (id, toast) in s.toasts.iter() {
        let (glyph, color) = toast.kind.glyph(k);
        let key = toast.key.clone().unwrap_or_else(|| format!("toast-{id}"));
        let mut t = rune_kit::Toast::new(key, toast.title.clone())
            .lead(icon(glyph, 16.0).color(color))
            .on_close(move |s: &mut J3| s.toasts.dismiss(id));
        if let Some(description) = &toast.description {
            t = t.description(description.clone());
        }
        if let Some((label, action)) = &toast.action {
            let action = action.clone();
            t = t.action(k, label, true, move |s: &mut J3, tx: &mut Tx| {
                s.toasts.dismiss(id);
                action(s, tx);
            });
        }
        let mut el = t.build(k);
        if let Some(thread) = toast.thread.clone() {
            el = el.on_press_tx(move |s: &mut J3, tx: &mut Tx| {
                s.toasts.dismiss(id);
                s.open_thread(thread.clone(), tx);
            });
        }
        region = region.toast(el);
    }
    ui.layer.push(region.build(k));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_has_one_toast_and_toasts_expire() {
        let mut toasts = Toasts::default();
        let key = ThreadKey::new("local", "t1");
        toasts.push_thread(ToastKind::Warning, "a needs attention", key.clone());
        toasts.push_thread(ToastKind::Success, "a finished", key);
        toasts.error("Couldn’t archive");
        assert_eq!(toasts.iter().count(), 2);
        let mut tx = Tx::default();
        assert!(!toasts.poll(1.0, &mut tx));
        assert!(!toasts.poll(5.9, &mut tx));
        assert!(toasts.poll(6.1, &mut tx));
        assert_eq!(toasts.iter().count(), 0);
    }
}
