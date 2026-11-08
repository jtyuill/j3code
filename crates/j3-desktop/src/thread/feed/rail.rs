//! The activity rail (spec §6.3): tool calls, reasoning and notices share one layout. A
//! glyph column on the left, a hairline joining the glyphs of consecutive rows, then the
//! row's one-line header and, under it, its body.
//!
//! Each row draws its own part of the line: from its top edge down to its glyph when the
//! row above is on the rail too, and from its glyph down to its bottom edge when the row
//! below is. The space between rows sits at the top of the lower row (`top`), so the two
//! halves meet.

use rune::{Color, El, Icon, col, el, icon};
use rune_kit::Kit;

use super::disclosure;

/// The header's height; the glyph is centred in it.
pub const HEADER: f32 = 24.0;
/// A glyph's size.
pub const GLYPH: f32 = 14.0;
/// The glyph column's width and the space after it.
const COLUMN: f32 = 20.0;
const GAP: f32 = 10.0;
/// The space between the rail's line and a glyph.
const CLEARANCE: f32 = 2.0;

/// Whether a rail row's neighbours are on the rail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rail {
    pub above: bool,
    pub below: bool,
}

/// `glyph` drawn in `color` at the rail's size.
pub fn glyph(glyph: &'static Icon, color: Color) -> El {
    icon(glyph, GLYPH).color(color)
}

/// A rail row's header: `title`, then `description` in the room left (cut with an ellipsis),
/// the `meta` parts and the disclosure slot: a chevron when `expanded` is given, else the same
/// room left empty so meta lines up from row to row.
pub fn header(
    k: &Kit,
    title: El,
    description: Option<El>,
    meta: Vec<El>,
    expanded: Option<bool>,
) -> El {
    let room = match description {
        Some(description) => description.truncate().grow(1.0).shrink(),
        None => el().grow(1.0),
    };
    let mut header = rune::row()
        .w_full()
        .h(HEADER)
        .gap(8.0)
        .center_y()
        .child(title)
        .child(room);
    for part in meta {
        header = header.child(part);
    }
    let slot = el().w(12.0).h(12.0);
    header.child(match expanded {
        Some(expanded) => slot.child(disclosure(k, expanded)),
        None => slot,
    })
}

/// A row on the rail: `glyph` beside `header`, `body` under the header, `top` points of
/// space above both. The line stops [`CLEARANCE`] short of the glyph.
pub fn row(k: &Kit, rail: Rail, top: f32, glyph: El, header: El, body: Option<El>) -> El {
    let line = |on: bool| {
        let segment = el().w(1.0);
        if on {
            segment.fill(k.c.border_base)
        } else {
            segment
        }
    };
    let inset = (HEADER - GLYPH) / 2.0 - CLEARANCE;
    let column = col()
        .w(COLUMN)
        .items_center()
        .child(line(rail.above).h(top + inset))
        .child(el().w(COLUMN).h(HEADER - 2.0 * inset).center().child(glyph))
        .child(line(rail.below).grow(1.0));
    let mut main = col().grow(1.0).shrink().pt(top).child(header);
    if let Some(body) = body {
        main = main.child(body);
    }
    rune::row()
        .w_full()
        .items_stretch()
        .gap(GAP)
        .child(column)
        .child(main)
}
