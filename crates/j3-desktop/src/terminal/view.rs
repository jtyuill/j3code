//! The drawer (tabs, `+`, hide, the status strip with Restart, a drag edge for its height)
//! and the terminal element: role `terminal` with the visible text as its value, the grid,
//! the cursor, and the focus field at the cursor that also shows an input method's
//! composition there, underlined.

use rune::{
    Color, Cursor, Cx, Drag, El, Phase, Tx, col, edit_access, el, row, spacer, text, text_input,
};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit};

use super::{CellMetrics, Drawer, MAX_HEIGHT, MIN_HEIGHT, PADDING, Palette, Terminal, render};
use crate::{
    actions::{Action, shortcut_label},
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
};

/// `key`'s drawer while it is open: tab strip, the selected terminal, and why its shell
/// isn't running.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let Some(drawer) = s.terminals.get(key).filter(|d| d.open) else {
        return col();
    };
    let k = ui.k;
    let dark = cx.system_dark();
    drawer.dark.set(dark);
    let palette = Palette::new(k, dark);
    // The chat above keeps some room.
    let room = cx
        .layout_size("thread-main")
        .map_or(MAX_HEIGHT, |[_, h]| (h - MIN_HEIGHT).max(MIN_HEIGHT));
    let height = drawer.height.min(room);
    let body = drawer.active().map(|terminal| {
        let focused = terminal.edit.focused;
        terminal_view(
            terminal,
            key,
            &palette,
            CellMetrics::new(ui.m.terminal),
            focused,
        )
    });
    col()
        .key("terminal-drawer")
        .w_full()
        .h(height)
        .fill(palette.background)
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_muted)
        .child(tabs(drawer, key, k))
        .child(
            el().key("terminal-body")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .pl(8.0)
                .pt(4.0)
                .children(body),
        )
        .children(drawer.active().and_then(|t| status(t, key, k)))
        .child(resize_edge(key, height, s.prefs.ui().zoom()))
}

/// The top edge: dragging it sets the drawer's height.
fn resize_edge(key: &ThreadKey, height: f32, zoom: f32) -> El {
    let key = key.clone();
    el().key("terminal-resize")
        .pin(Some(0.0), Some(-3.0), Some(0.0), None)
        .h(6.0)
        .cursor(Cursor::ResizeRow)
        .on_drag(move |s: &mut J3, drag: &mut Drag, _: &mut Tx| {
            if drag.phase == Phase::Down {
                drag.data = height;
            }
            if let Some(drawer) = s.terminals.get_mut(&key) {
                drawer.height = (drag.data - drag.dy / zoom).clamp(MIN_HEIGHT, MAX_HEIGHT);
            }
        })
}

/// "Terminal N" tabs with their close buttons, `+`, and the hide chevron.
fn tabs(drawer: &Drawer, key: &ThreadKey, k: &Kit) -> El {
    let mut strip = row()
        .key("terminal-tabs")
        .w_full()
        .h(32.0)
        .px(8.0)
        .gap(4.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted);
    for (index, terminal) in drawer.tabs.iter().enumerate() {
        let active = index == drawer.active;
        let label = terminal.label();
        let (select, close) = (
            (key.clone(), terminal.id.clone()),
            (key.clone(), terminal.id.clone()),
        );
        let close = IconButton::new(&icons::X)
            .variant(IconButtonVariant::GhostMuted)
            .control_size(ControlSize::Mini)
            .label("Close terminal")
            .help("Close terminal")
            .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.close_terminal(&close.0, &close.1, tx))
            .build(k)
            .key(format!("terminal-close-{}", terminal.id));
        let tab = row()
            .key(format!("terminal-tab-{}", terminal.id))
            .role("tab")
            .label(label.clone())
            .h(24.0)
            .pl(8.0)
            .pr(2.0)
            .gap(4.0)
            .center_y()
            .radius(k.radius.sm)
            .fill(if active {
                k.c.surface_raised_base
            } else {
                Color::TRANSPARENT
            })
            .hover(if active {
                k.c.surface_raised_base
            } else {
                k.c.hover
            })
            .cursor(Cursor::Pointer)
            .on_press(move |s: &mut J3| s.select_terminal(&select.0, &select.1))
            .child(text(label).font(k.ty.text_xsmall).color(if active {
                k.c.text_base
            } else {
                k.c.text_muted
            }))
            .child(close);
        strip = strip.child(tab);
    }
    let new = {
        let key = key.clone();
        IconButton::new(&icons::PLUS)
            .variant(IconButtonVariant::GhostMuted)
            .control_size(ControlSize::Mini)
            .label("New terminal")
            .help("New terminal")
            .on_press(move |s: &mut J3| s.new_terminal(&key))
            .build(k)
            .key("terminal-new")
    };
    let hide = {
        let key = key.clone();
        IconButton::new(&icons::CHEVRON_DOWN)
            .variant(IconButtonVariant::GhostMuted)
            .control_size(ControlSize::Mini)
            .label("Hide terminal")
            .help(format!(
                "Hide terminal ({})",
                shortcut_label(Action::ToggleTerminal)
            ))
            .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.hide_terminal(&key, tx))
            .build(k)
            .key("terminal-hide")
    };
    strip.child(new).child(spacer()).child(hide)
}

/// Why the selected shell isn't running, with Restart; None while it runs.
fn status(terminal: &Terminal, key: &ThreadKey, k: &Kit) -> Option<El> {
    let message = terminal.status().message()?;
    let (key, id) = (key.clone(), terminal.id.clone());
    Some(
        row()
            .key("terminal-status")
            .label(message.clone())
            .w_full()
            .h(32.0)
            .px(12.0)
            .gap(8.0)
            .center_y()
            .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_muted)
            .child(
                text(message)
                    .font(k.ty.text_xsmall)
                    .color(k.c.text_muted)
                    .truncate()
                    .shrink(),
            )
            .child(
                Button::new("Restart")
                    .variant(ButtonVariant::Outline)
                    .control_size(ControlSize::Small)
                    .on_press(move |s: &mut J3| s.restart_terminal(&key, &id))
                    .build(k)
                    .key("terminal-restart"),
            ),
    )
}

/// One terminal: its grid, cursor and focus field, sized by its owner. Its laid-out size
/// sets the grid's; presses select, the wheel scrolls.
fn terminal_view(
    terminal: &Terminal,
    key: &ThreadKey,
    palette: &Palette,
    cell: CellMetrics,
    focused: bool,
) -> El {
    let id = terminal.id.clone();
    let (rows, cursor) = render::grid(terminal, palette, cell, focused);
    let (column, line) = cursor.as_ref().map_or((0, 0), |c| c.at);
    let composing = terminal.edit.preedit.is_some();
    let cols = usize::from(terminal.screen.size().cols);
    let field_width = if composing {
        (cols.saturating_sub(column).max(1)) as f32 * cell.width
    } else {
        1.0
    };
    let field = {
        let (key, id) = (key.clone(), id.clone());
        let (on_key, on_paste, on_change) = (
            (key.clone(), id.clone()),
            (key.clone(), id.clone()),
            (key.clone(), id.clone()),
        );
        text_input(&terminal.edit, cell.font)
            .single_line()
            .key(format!("terminal-{id}"))
            .label(terminal.label())
            .absolute(
                PADDING + column as f32 * cell.width,
                PADDING + line as f32 * cell.height,
            )
            .size(field_width, cell.height)
            .fill(if composing {
                palette.background
            } else {
                Color::TRANSPARENT
            })
            .color(palette.foreground)
            .caret(Color::TRANSPARENT, Color::TRANSPARENT)
            .no_focus_ring()
            .on_edit(edit_access(move |s: &mut J3| s.terminal_edit(&key, &id)))
            .on_input_key(move |s: &mut J3, ev, tx| s.terminal_key(&on_key.0, &on_key.1, ev, tx))
            .on_input_paste(move |s: &mut J3, content, _| {
                s.terminal_paste(&on_paste.0, &on_paste.1, content.text());
                true
            })
            .on_input_change(move |s: &mut J3, _| s.terminal_committed(&on_change.0, &on_change.1))
    };
    let (area, drag, wheel) = (
        (key.clone(), id.clone()),
        (key.clone(), id.clone()),
        (key.clone(), id.clone()),
    );
    el().key(format!("terminal-view-{id}"))
        .role("terminal")
        .label(format!("Terminal {id}"))
        .a11y_value(terminal.screen.text())
        .w_full()
        .h_full()
        .fill(palette.background)
        .clip_rounded(0.0)
        .cursor(Cursor::Text)
        .preserve_focus_on_press(true)
        .on_layout_tx(move |s: &mut J3, rect, tx| {
            s.terminal_area(&area.0, &area.1, [rect[2], rect[3]], tx)
        })
        .on_drag(move |s: &mut J3, d: &mut Drag, _: &mut Tx| s.terminal_drag(&drag.0, &drag.1, d))
        .on_wheel(move |s: &mut J3, ev, _| s.terminal_wheel(&wheel.0, &wheel.1, &ev))
        .children(rows)
        .children(cursor.and_then(|c| c.overlay))
        .child(field)
}
