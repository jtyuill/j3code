//! The app's motion policy: how much a change animates follows how often it happens, so the
//! things done all day never wait on an animation and the rare ones can take their time.
//!
//! - **Constant** (typing, terminal output, scrolling, dragging a divider): no motion. Every
//!   change lands in the frame it happens.
//! - **Frequent** (tool rows arriving in a live turn, expanding a row, toggling the terminal or
//!   the file pane, switching threads): at most a short fade or glide under [`QUICK`] that
//!   never holds content back — layout is final at once, only presentation catches up.
//! - **Occasional** (menus, popovers, cards above the composer): Rune's and the kit's short
//!   defaults (0.2–0.25 s).
//! - **Rare** (dialogs, settings, the agents hub, toasts): keep the kit's and Rune's fuller
//!   transitions; this is where motion has room to be beautiful.
//!
//! Rune animates by default (a 0.3 s spring on layout and colour, a 0.3 s fade on every
//! element mounted after the first frame), so constant and frequent surfaces opt out here.

use rune::{Spring, SpringPair, Transition};

/// Frequent motion: a crisp, critically damped glide that reads as a response, not a wait.
pub const QUICK: Spring = Spring::new(0.18, 0.0);

/// A feed row arriving whole: a short fade in, a shorter fade out.
pub const ARRIVE: SpringPair = SpringPair::new(Spring::new(0.16, 0.0), Spring::new(0.12, 0.0));

/// How a feed row that arrives complete (a tool call, a notice, a sent message) enters.
pub fn arrive() -> Transition {
    Transition::opacity().springs(ARRIVE)
}

/// A reply's words fading in as each paragraph arrives: a reply streams whole blocks, never word
/// by word, and their words fade in over this long on Rune's reveal stagger, so a paragraph
/// washes in down the page rather than landing as a slab.
pub const WORD_FADE: f32 = 0.3;
