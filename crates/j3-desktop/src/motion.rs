//! The app's motion policy: how much a change animates follows how often it happens, so the
//! things done all day never wait on an animation and the rare ones can take their time.
//!
//! - **Constant** (typing, terminal output, scrolling, dragging a divider, a reply's text
//!   streaming in, switching to a thread or the new-thread surface, a list refiltering under
//!   the field being typed in): no motion. Every change lands in the frame it happens.
//! - **Frequent** (tool rows arriving in a live turn, expanding a row, toggling the terminal or
//!   the file pane): at most a short fade or glide that never holds content back — layout
//!   is final at once, only presentation catches up. Feed rows and the sent-message scroll no
//!   longer animate.
//! - **Occasional** (menus, popovers, cards above the composer): Rune's and the kit's short
//!   defaults (0.2–0.25 s).
//! - **Rare** (dialogs, settings, the agents hub, toasts): keep the kit's and Rune's fuller
//!   transitions; this is where motion has room to be beautiful.
//!
//! Rune animates by default (a 0.3 s spring on layout and colour, a 0.3 s fade on every
//! element mounted after the first frame), so constant and frequent surfaces opt out here.
