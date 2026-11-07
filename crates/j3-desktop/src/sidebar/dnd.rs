//! T3's drag and drop (spec §4.3): every row outside Working drags; a card with its title
//! follows the pointer, and the drop target under it (a row, the drag-only Pinned / Active
//! boundaries, the Settled header or the Settled shelf) lights up. A drop runs the commands
//! `T3Rows::plan` makes from `j3_client::workflow::plan_drop`.
//!
//! Drop targets report where they are drawn (`El::on_drawn_layout_tx`, live only), so a drop
//! finds what is under the pointer in window points, whatever the zoom or scroll.

use rune::{Drag, El, Phase, Tx, row, text};
use rune_kit::Kit;

use super::{T3Rows, ThreadKey, t3::T3Row};
use crate::app::J3;

/// Pointer travel (points) before a press on a row becomes a drag.
const DRAG_START: f32 = 4.0;

/// The drop target of the Pinned boundary (t3's `PinnedHeader` slot).
pub const DROP_PINNED: &str = "drop-pinned";
/// The drop target of the Active boundary (t3's `PinnedDivider` slot).
pub const DROP_ACTIVE: &str = "drop-active";
/// The Settled shelf's header.
pub const SHELF_SETTLED: &str = "shelf-settled";
/// The Settled shelf's rows and empty space (t3's `SettledPlaceholder`).
pub const SETTLED_LIST: &str = "t3-settled";

/// A row being dragged.
#[derive(Debug, Clone)]
pub struct DragState {
    pub key: ThreadKey,
    pub title: String,
    /// The pointer in window points.
    pub pointer: [f32; 2],
}

impl T3Rows {
    /// The list slot drop target `target` (an element key) stands for, if it takes this drop.
    pub fn target_slot(&self, target: &str) -> Option<usize> {
        use j3_client::workflow::ListMarker;
        match target {
            DROP_PINNED => self.marker_index(ListMarker::PinnedHeader),
            DROP_ACTIVE => self.marker_index(ListMarker::PinnedDivider),
            SHELF_SETTLED => self.marker_index(ListMarker::SettledHeader),
            SETTLED_LIST => self.marker_index(ListMarker::SettledPlaceholder),
            row => self
                .rows()
                .find(|r| r.key.element_id("thread") == row)
                .and_then(|r| self.drop_slot(r)),
        }
    }
}

impl J3 {
    /// The drop target under the dragged row's pointer: the smallest drawn target that holds
    /// it and takes drops now.
    pub fn drop_target_under(&self, rows: &T3Rows) -> Option<String> {
        let drag = self.sidebar.drag.as_ref()?;
        let [x, y] = drag.pointer;
        self.sidebar
            .drop_rects
            .iter()
            .filter(|(_, [rx, ry, w, h])| x >= *rx && x < rx + w && y >= *ry && y < ry + h)
            .filter(|(target, _)| rows.target_slot(target).is_some())
            .min_by(|a, b| (a.1[2] * a.1[3]).total_cmp(&(b.1[2] * b.1[3])))
            .map(|(target, _)| target.clone())
    }

    /// Ends a drag at the pointer: runs the drop's commands, if any.
    fn drop_dragged(&mut self) {
        let Some(drag) = self.sidebar.drag.clone() else {
            return;
        };
        let rows = self.t3_rows();
        let commands = self
            .drop_target_under(&rows)
            .and_then(|target| rows.target_slot(&target))
            .map(|over| rows.plan(&drag.key, over))
            .unwrap_or_default();
        self.sidebar.drag = None;
        // Rows that left the list (filtered, folded, gone) report again when they return.
        self.sidebar
            .drop_rects
            .retain(|target, _| rows.target_slot(target).is_some());
        if !commands.is_empty() {
            self.apply_thread_commands(commands);
        }
    }
}

/// Makes `e` a drop target named `target`: it reports where it is drawn, and lights up while
/// the dragged row is over it.
pub fn drop_target(e: El, target: String, over: bool, k: &Kit) -> El {
    let e = if over { e.fill(k.c.hover) } else { e };
    e.on_drawn_layout_tx(move |s: &mut J3, rect, tx: &mut Tx| {
        s.sidebar.drop_rects.insert(target.clone(), rect);
        tx.live_only();
    })
}

/// Makes row `r` draggable.
pub fn draggable(e: El, r: &T3Row) -> El {
    let key = r.key.clone();
    let title = r.title.clone();
    e.on_drag(move |s: &mut J3, d: &mut Drag, tx: &mut Tx| match d.phase {
        Phase::Down => {}
        Phase::Move => {
            let pointer = d.window_point();
            match &mut s.sidebar.drag {
                Some(drag) => drag.pointer = pointer,
                None if d.dx.hypot(d.dy) >= DRAG_START => {
                    s.sidebar.row_menu = None;
                    s.sidebar.drag = Some(DragState {
                        key: key.clone(),
                        title: title.clone(),
                        pointer,
                    });
                }
                None => {}
            }
        }
        Phase::Up => {
            if d.cancelled() {
                s.sidebar.drag = None;
            } else {
                s.drop_dragged();
            }
            tx.instant();
        }
    })
}

/// The card that follows the pointer (in layer points: window points over the zoom).
pub fn preview(drag: &DragState, zoom: f32, k: &Kit) -> El {
    let c = &k.c;
    let [x, y] = drag.pointer;
    k.elevate(
        row()
            .key("drag-preview")
            .absolute(x / zoom + 12.0, y / zoom + 8.0)
            .max_w(280.0)
            .px(8.0)
            .py(4.0)
            .radius(k.radius.md)
            .fill(c.layer_01)
            .border(1.0, c.border_base)
            .child(
                text(drag.title.clone())
                    .font(k.ty.text_small)
                    .color(c.text_base)
                    .truncate(),
            )
            .a11y_hidden(),
        &k.elevation.floating,
    )
}
