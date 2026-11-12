//! The app's motion policy: how much a change animates follows how often it happens and who
//! asked for it, so the things done all day never wait on an animation and the rare ones can
//! take their time.
//!
//! - **Constant** (typing, terminal output, scrolling, dragging a divider, switching to a
//!   thread or the new-thread surface, a list refiltering under the field being typed in): no
//!   motion. Every change lands in the frame it happens.
//! - **Arriving on its own** (everything a turn brings: reply text, tool rows, their status,
//!   the cards above the composer and the room they take from the feed; the header's git
//!   control following the repository): no motion either. Nobody asked for the change, so
//!   nothing should move under the reader. The chat column (`thread::layout`), the feed's
//!   stack and the header's trailing group are `instant_changes`.
//! - **Frequent** (toggling the terminal or the file pane): no motion.
//! - **Occasional** (menus, popovers): Rune's and the kit's short defaults (0.2–0.25 s).
//! - **Rare** (dialogs, settings, the agents hub, toasts): keep the kit's and Rune's fuller
//!   transitions; this is where motion has room to be beautiful.
//!
//! Rune animates by default (a 0.3 s spring on layout and colour, a fade on every element
//! mounted after the first frame), so everything but the last two opts out. `instant_changes`
//! lands an element's subtree but not its own entrance, and a growth stack takes it from the
//! stack, not its rows: mark the container whose contents change.
