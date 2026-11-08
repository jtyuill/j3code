//! Draws T3 mode (spec §4.3): the header (project filter, its chip, add project, new thread),
//! Pinned and Active at the top, the Working, Snoozed and Settled shelves at the bottom, the
//! empty states, the drag-only boundaries while a row drags, and the row context menu.

use j3_client::workflow::{T3Section, ThreadStatus, format_working_duration, snooze_wake_label};
use rune::{Color, Cx, El, Icon, PointerPress, ScrollSpec, Tx, col, el, icon, row, spacer, text};
use rune_kit::{
    Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Menu, MenuItem, State,
    shell::SidebarHeader,
};
use time::OffsetDateTime;

use super::{
    SETTLED_PAGE_COUNT, T3Rows, ThreadCommand,
    dnd::{self, DROP_ACTIVE, DROP_PINNED, SETTLED_LIST, SHELF_SETTLED},
    menu,
    t3::T3Row,
};
use crate::{
    actions::{Action, shortcut_label},
    app::{J3, Ui},
    feed::age_since,
    icons,
    theme::Status,
    ui_settings::Shelf,
};

/// What every row needs to draw.
struct Ctx<'a> {
    rows: &'a T3Rows,
    k: &'a Kit,
    now: OffsetDateTime,
    open: Option<&'a super::ThreadKey>,
    /// The drop target under the dragged row.
    over: Option<&'a str>,
}

pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui) -> El {
    let k = ui.k;
    let rows = s.t3_rows();
    let open = s.open_thread_key();
    let over = s.drop_target_under(&rows);
    let dragging = s.sidebar.drag.is_some();
    let c = Ctx {
        rows: &rows,
        k,
        now: s.clock.now,
        open: open.as_ref(),
        over: over.as_deref(),
    };
    let header = header(s, &rows, ui);

    let mut list = col()
        .key("sidebar-list")
        .role("list")
        .label("Threads")
        .focus_group()
        .focus_follows()
        .w_full()
        .px(8.0)
        .pb(8.0);
    // Fill the viewport, so the shelves sit at its bottom ("boring sidebar").
    if let Some([_, h]) = cx.layout_size("t3-list") {
        list = list.min_h(h);
    }
    if let Some(note) = rows.note {
        list = list.child(note_row("t3-note", note, k));
    }
    if rows.order().is_empty() && rows.settled_count == 0 && rows.note.is_none() {
        list = list.child(empty(&rows, k));
    }

    let mut top = col().key("t3-top").w_full().gap(2.0);
    // The Pinned boundary shows only while dragging (t3's drag-only `SidebarDragBoundary`).
    if dragging {
        top = top.child(boundary("Pinned", DROP_PINNED, &c));
    }
    top = top.child(
        col()
            .key("t3-pinned")
            .w_full()
            .gap(2.0)
            .children(rows.pinned.iter().map(|r| thread_row(r, &c))),
    );
    if dragging {
        top = top.child(boundary("Active", DROP_ACTIVE, &c));
    } else if !rows.pinned.is_empty() {
        top = top.child(
            el().key("t3-pinned-divider")
                .w_full()
                .h(1.0)
                .my(4.0)
                .fill(k.c.border_muted),
        );
    }
    top = top.child(
        col()
            .key("t3-active")
            .w_full()
            .gap(2.0)
            .children(rows.active.iter().map(|r| thread_row(r, &c))),
    );

    let mut shelves = col().key("t3-shelves").w_full().gap(2.0);
    if rows.working_count > 0 {
        shelves = shelves
            .child(shelf_header(
                Shelf::Working,
                "Working",
                rows.working_count,
                &c,
            ))
            .child(
                col()
                    .key("t3-working")
                    .w_full()
                    .gap(2.0)
                    .children(rows.working.iter().map(|r| thread_row(r, &c))),
            );
    }
    if rows.snoozed_count > 0 {
        shelves = shelves
            .child(shelf_header(
                Shelf::Snoozed,
                "Snoozed",
                rows.snoozed_count,
                &c,
            ))
            .child(
                col()
                    .key("t3-snoozed")
                    .w_full()
                    .gap(2.0)
                    .children(rows.snoozed.iter().map(|r| thread_row(r, &c))),
            );
    }
    // Like t3's `settled-header`, the Settled shelf is always there: a drop target even while
    // empty.
    if rows.has_projects {
        shelves = shelves.child(shelf_header(
            Shelf::Settled,
            "Settled",
            rows.settled_count,
            &c,
        ));
        let mut settled = col()
            .key(SETTLED_LIST)
            .w_full()
            .min_h(8.0)
            .gap(2.0)
            .radius(k.radius.md)
            .children(rows.settled.iter().map(|r| thread_row(r, &c)));
        let hidden = if rows.is_expanded(Shelf::Settled) {
            rows.settled_count.saturating_sub(rows.settled.len())
        } else {
            0
        };
        if hidden > 0 {
            settled = settled.child(show_more(hidden.min(SETTLED_PAGE_COUNT), k));
        }
        shelves = shelves.child(dnd::drop_target(
            settled,
            SETTLED_LIST.to_owned(),
            c.over == Some(SETTLED_LIST),
            k,
        ));
    }
    list = list
        .child(top)
        .child(el().key("t3-gap").w_full().grow(1.0).min_h(16.0))
        .child(shelves);

    let mut root = col()
        .key("sidebar")
        .w_full()
        .h_full()
        .min_h(0.0)
        .child(header)
        .child(
            col()
                .key("t3-list")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .scroll_edge_fade(16.0)
                .child(list),
        );
    if let Some(m) = &s.sidebar.row_menu
        && let Some(r) = rows.row(&m.key)
    {
        root = root.child(menu::build(r, m, c.now, k, ui.layer));
    }
    if let Some(drag) = &s.sidebar.drag {
        ui.layer.push(dnd::preview(drag, ui.m.zoom, k));
    }
    root
}

/// The project filter (a dropdown with "Add project…"), the active filter's chip, and the
/// add-project and new-thread buttons (scoped to the filter).
fn header(s: &J3, rows: &T3Rows, ui: &mut Ui) -> El {
    let k = ui.k;
    let filtered = rows.filter.is_some();
    let mut filter_button = IconButton::new(&icons::LIST_FILTER)
        .variant(IconButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .label("Filter threads by project")
        .help("Filter threads by project")
        .on_press(|s: &mut J3| {
            s.sidebar.row_menu = None;
            s.sidebar.filter_menu = !s.sidebar.filter_menu;
        });
    if filtered {
        filter_button = filter_button.state(State::Pressed);
    }
    let pick = |label: &str, value: Option<super::ProjectKey>| {
        MenuItem::new(label)
            .checked(value == rows.filter)
            .on_select(move |s: &mut J3| {
                s.sidebar.filter_menu = false;
                s.set_project_filter(value.clone());
            })
    };
    let mut menu = Menu::new("project-filter-menu")
        .trigger(filter_button.build(k).key("project-filter"))
        .open(s.sidebar.filter_menu)
        .min_width(200.0)
        .on_close(|s: &mut J3| s.sidebar.filter_menu = false)
        .item(pick("All projects", None));
    if !rows.projects.is_empty() {
        menu = menu.separator();
    }
    for (project, label) in &rows.projects {
        menu = menu.item(pick(label, Some(project.clone())));
    }
    // Projects are added from this menu too: the filter is where the project list lives.
    let menu = menu
        .separator()
        .item(
            MenuItem::new("Add project…")
                .icon(&icons::FOLDER_PLUS)
                .on_select_tx(|s: &mut J3, tx: &mut Tx| {
                    s.sidebar.filter_menu = false;
                    s.dispatch(Action::AddProject, tx);
                }),
        )
        .build(k, ui.layer);

    let mut bar = row()
        .key("t3-header")
        .w_full()
        .h(28.0)
        .px(8.0)
        .mb(4.0)
        .gap(4.0)
        .center_y()
        .child(menu);
    // The active filter shows as a chip next to it, which clears it.
    if let Some(label) = &rows.filter_label {
        bar = bar.child(
            row()
                .key("project-filter-chip")
                .label(label.clone())
                .shrink()
                .min_w(0.0)
                .h(22.0)
                .pl(8.0)
                .gap(2.0)
                .center_y()
                .radius(k.radius.md)
                .fill(k.c.layer_03)
                .child(
                    text(label.clone())
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_base)
                        .truncate()
                        .shrink(),
                )
                .child(
                    IconButton::new(&icons::X)
                        .variant(IconButtonVariant::GhostMuted)
                        .control_size(ControlSize::Mini)
                        .label("Show all projects")
                        .help("Show all projects")
                        .on_press(|s: &mut J3| s.set_project_filter(None))
                        .build(k)
                        .key("clear-project-filter"),
                ),
        );
    }
    let new_thread = IconButton::new(&icons::SQUARE_PEN)
        .variant(IconButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .label("New thread")
        .help(format!(
            "New thread ({})",
            shortcut_label(Action::NewThread)
        ))
        // Scoped to the filter (`J3::target_context`).
        .on_press_tx(|s: &mut J3, tx: &mut Tx| s.press_new_thread(tx))
        .build(k)
        .key("new-thread");
    bar.child(spacer())
        .child(
            IconButton::new(&icons::FOLDER_PLUS)
                .variant(IconButtonVariant::GhostMuted)
                .control_size(ControlSize::Small)
                .label("Add project")
                .help(format!(
                    "Add project ({})",
                    shortcut_label(Action::AddProject)
                ))
                .on_press_tx(|s: &mut J3, tx: &mut Tx| s.dispatch(Action::AddProject, tx))
                .build(k)
                .key("add-project"),
        )
        .child(crate::new_thread::button(s, ui, new_thread))
}

fn note_row(key: &'static str, note: &str, k: &Kit) -> El {
    row().key(key).w_full().px(8.0).py(4.0).child(
        text(note.to_owned())
            .font(k.ty.text_xsmall)
            .color(k.c.text_muted),
    )
}

/// "No projects yet" (with an Add project button), "No threads in {project} yet" or "No
/// threads yet".
fn empty(rows: &T3Rows, k: &Kit) -> El {
    let note = if !rows.has_projects {
        "No projects yet".to_owned()
    } else if rows.filter.is_some() {
        format!(
            "No threads in {} yet",
            rows.filter_label.as_deref().unwrap_or_default()
        )
    } else {
        "No threads yet".to_owned()
    };
    let mut column = col()
        .key("t3-empty-state")
        .w_full()
        .py(24.0)
        .gap(8.0)
        .center()
        .child(
            text(note.clone())
                .key("t3-empty")
                .label(note)
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted),
        );
    if !rows.has_projects {
        column = column.child(
            Button::new("Add project")
                .variant(ButtonVariant::Outline)
                .control_size(ControlSize::Small)
                .icon(&icons::PLUS)
                .on_press_tx(|s: &mut J3, tx: &mut Tx| s.dispatch(Action::AddProject, tx))
                .build(k)
                .key("empty-add-project"),
        );
    }
    column
}

/// A drop line shown while dragging, labelled with the section it leads into.
fn boundary(label: &'static str, target: &'static str, c: &Ctx) -> El {
    let k = c.k;
    let over = c.over == Some(target);
    let line = row()
        .key(target)
        .w_full()
        .gap(8.0)
        .px(8.0)
        .py(4.0)
        .center_y()
        .radius(k.radius.md)
        .child(text(label).font(k.ty.text_xsmall).color(if over {
            k.c.text_accent
        } else {
            k.c.text_muted
        }))
        .child(el().grow(1.0).h(1.0).fill(k.c.border_base));
    dnd::drop_target(line, target.to_owned(), over, k)
}

/// A shelf's header: chevron, title and count; a click folds it (saved in
/// `ui.expandedShelves`). The Settled header takes drops.
fn shelf_header(shelf: Shelf, title: &'static str, count: usize, c: &Ctx) -> El {
    let key = match shelf {
        Shelf::Working => "shelf-working",
        Shelf::Snoozed => "shelf-snoozed",
        Shelf::Settled => SHELF_SETTLED,
    };
    let header = SidebarHeader::new(title)
        .count(count)
        .collapsed(!c.rows.is_expanded(shelf))
        .on_toggle(move |s: &mut J3, tx: &mut Tx| s.toggle_shelf(shelf, tx))
        .build(c.k)
        .key(key)
        .label(format!("{title} {count}"))
        .mt(8.0);
    if shelf == Shelf::Settled {
        dnd::drop_target(header, key.to_owned(), c.over == Some(key), c.k)
    } else {
        header
    }
}

fn show_more(more: usize, k: &Kit) -> El {
    let label = format!("Show {more} more");
    k.interactive(
        row()
            .key("settled-show-more")
            .role("button")
            .label(label.clone())
            .w_full()
            .h(28.0)
            .px(8.0)
            .gap(8.0)
            .center_y()
            .radius(k.radius.md)
            .focusable(false)
            .on_press(|s: &mut J3| s.show_more_settled())
            .child(icon(&icons::CHEVRON_DOWN, 12.0).color(k.c.icon_muted))
            .child(text(label).font(k.ty.text_small).color(k.c.text_muted)),
    )
}

/// The colour and icon of a status pill (t3's `SidebarThreadRow` classes, as theme roles).
pub fn status_look(status: ThreadStatus, k: &Kit) -> (Color, &'static Icon) {
    let s = Status::of(k);
    match status {
        ThreadStatus::Working => (s.info, &icons::CIRCLE_DASHED),
        ThreadStatus::Waiting => (s.muted, &icons::CIRCLE_DOT_DASHED),
        ThreadStatus::Approval | ThreadStatus::Input | ThreadStatus::Request => {
            (s.warning, &icons::MESSAGE_CIRCLE_QUESTION_MARK)
        }
        ThreadStatus::Failed | ThreadStatus::Limited => (s.danger, &icons::CIRCLE_ALERT),
        ThreadStatus::Ready => (s.success, &icons::CIRCLE_CHECK),
    }
}

/// The trailing status of a row: the snooze countdown, Woke, a status pill (with the working
/// time), or the row's age.
fn status(r: &T3Row, c: &Ctx) -> El {
    let k = c.k;
    let slot = || row().key("status").gap(4.0).center_y();
    let pill = |color: Color, glyph: &'static Icon, label: String, medium: bool| {
        slot().child(icon(glyph, 12.0).color(color)).child(
            text(label)
                .font(if medium {
                    k.ty.text_xsmall_medium
                } else {
                    k.ty.text_xsmall
                })
                .color(color),
        )
    };
    if r.section == T3Section::Snoozed
        && let Some(until) = r.snoozed_until
    {
        return pill(
            Status::of(k).info,
            &icons::ALARM_CLOCK,
            snooze_wake_label(until, c.now),
            false,
        );
    }
    if r.woke {
        return pill(
            Status::of(k).warning,
            &icons::ALARM_CLOCK,
            "Woke".into(),
            true,
        );
    }
    if r.section != T3Section::Settled
        && let Some(label) = r.status.label(r.unread)
    {
        let (color, glyph) = status_look(r.status, k);
        let mut e = pill(color, glyph, label.to_owned(), true);
        if let Some(since) = r.working_since {
            e = e.child(
                text(format_working_duration(c.now - since))
                    .font(k.ty.text_xsmall)
                    .color(k.c.text_muted),
            );
        }
        return e;
    }
    let mut e = slot();
    if let Some(time) = r.time {
        e = e.child(
            text(age_since(time, c.now))
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted),
        );
    }
    e
}

/// The hover action of a row: Settle, Un-settle or Wake (also in the context menu and on
/// ⌘⇧S); none while Working.
fn row_action(r: &T3Row, k: &Kit) -> Option<El> {
    let key = r.key.clone();
    let id = r.key.element_id("row-action");
    let press = move |command: ThreadCommand| {
        move |s: &mut J3| s.apply_thread_commands(vec![(key.clone(), command.clone())])
    };
    let button = match r.section {
        T3Section::Working => return None,
        T3Section::Settled => IconButton::new(&icons::UNDO_2)
            .variant(IconButtonVariant::Neutral)
            .control_size(ControlSize::Small)
            .label("Un-settle thread")
            .help("Un-settle thread")
            .on_press(press(ThreadCommand::Unsettle))
            .build(k),
        T3Section::Snoozed => IconButton::new(&icons::ALARM_CLOCK_OFF)
            .variant(IconButtonVariant::Neutral)
            .control_size(ControlSize::Small)
            .label("Wake thread now")
            .help("Wake thread now")
            .on_press(press(ThreadCommand::Wake))
            .build(k),
        // An opaque, high-contrast button: it sits over the row's text, so the Neutral
        // variant's translucent fill blends in.
        T3Section::Pinned | T3Section::Active => Button::new("Settle")
            .variant(ButtonVariant::Contrast)
            .control_size(ControlSize::Small)
            .icon(&icons::CHECK)
            .help("Settle thread")
            .on_press(press(ThreadCommand::Settle))
            .build(k),
    };
    Some(button.key(id).focusable(false))
}

/// One thread row: project, environment and status over the title (settled rows: one slim
/// line); hovering swaps the status for the row's action.
fn thread_row(r: &T3Row, c: &Ctx) -> El {
    let k = c.k;
    let selected = c.open == Some(&r.key);
    let action = row_action(r, k);
    let status = {
        let e = status(r, c).shrink();
        if action.is_some() {
            e.group_opacity(1.0, 0.0)
        } else {
            e
        }
    };
    let quiet = r.recede || r.section == T3Section::Settled;
    let title_color = if quiet { k.c.text_muted } else { k.c.text_base };
    let body = if r.section == T3Section::Settled {
        row()
            .key("line")
            .w_full()
            .gap(8.0)
            .center_y()
            .child(
                text(r.title.clone())
                    .font(k.ty.text_small)
                    .color(title_color)
                    .truncate()
                    .grow(1.0)
                    .shrink(),
            )
            .child(status)
    } else {
        let mut place = row().key("place").w_full().gap(4.0).center_y();
        if r.pinned {
            place = place.child(icon(&icons::PIN, 12.0).color(k.c.icon_muted));
        }
        place = place.child(
            text(r.project.clone())
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .truncate()
                .shrink(),
        );
        if let Some(env) = &r.env_label {
            place = place.child(
                text(format!("· {env}"))
                    .font(k.ty.text_xsmall)
                    .color(k.c.text_muted),
            );
        }
        place = place.child(spacer()).child(status);
        col().key("lines").w_full().gap(1.0).child(place).child(
            text(r.title.clone())
                .key("title")
                .font(if quiet {
                    k.ty.text_small
                } else {
                    k.ty.text_small_medium
                })
                .color(title_color)
                .truncate()
                .w_full(),
        )
    };
    let key = r.key.clone();
    let menu_key = r.key.clone();
    let mut e = col()
        .key(r.key.element_id("thread"))
        .label(r.accessibility_label())
        .w_full()
        .px(8.0)
        .py(if r.section == T3Section::Settled {
            4.0
        } else {
            6.0
        })
        .radius(k.radius.md)
        .hover_group()
        .focusable(true)
        .focus_selected(selected)
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.open_thread(key.clone(), tx))
        .on_secondary_press(move |s: &mut J3, at: PointerPress, _: &mut Tx| {
            s.open_row_menu(menu_key.clone(), at)
        })
        .child(body);
    e = k.interactive(e);
    if selected {
        e = e.fill(k.c.pressed);
    }
    if let Some(action) = action {
        e = e.child(
            row()
                .key("action")
                .pin(None, Some(0.0), Some(6.0), Some(0.0))
                .center_y()
                .group_opacity(0.0, 1.0)
                .child(action),
        );
    }
    if r.section != T3Section::Working {
        e = dnd::draggable(e, r);
    }
    if let Some(target) = c.rows.drop_slot(r).map(|_| r.key.element_id("thread")) {
        let over = c.over == Some(target.as_str());
        e = dnd::drop_target(e, target, over, k);
    }
    // After the handlers, which would make it a button.
    e.role("listitem")
}
