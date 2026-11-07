//! The control in the thread header: the quick action (`git-quick-action`) — a running
//! action's phase with a spinner while busy — and its menu (`git-menu`): Commit…, Push, Pull,
//! and View PR or Create PR.

use j3_client::{
    t3::git::GitStackedAction,
    workflow::git::{self, QuickActionKind},
};
use rune::{Cx, El, Tx, row, text};
use rune_kit::{
    Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Menu, MenuItem, Spinner,
};

use super::{WORKING_LABEL, commit};
use crate::{
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
};

/// `key`'s git control in the thread header (content-sized); empty until a repository
/// status is known, but kept while an action runs so its phase stays on screen.
pub fn view(s: &J3, _cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let control = row().key("git-control").gap(2.0).center_y();
    let Some(git) = s.git.get(key) else {
        return control;
    };
    let status = git.status.as_ref().filter(|status| status.is_repo);
    let busy = git.busy();
    if status.is_none() && !busy {
        return control;
    }
    let k = ui.k;
    let quick = git::quick_action(status, busy);
    let menu = git::menu(status, busy);
    let label = if busy {
        WORKING_LABEL.to_owned()
    } else {
        quick.label.to_owned()
    };
    let mut button = Button::new(label.clone())
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .disabled(quick.disabled);
    if busy {
        let kit = crate::theme::kit(k.dark, s.prefs.ui());
        button = button.label_with(move |font, ink| {
            row()
                .gap(6.0)
                .center_y()
                .child(Spinner::new().size(12.0).color(ink).build(&kit))
                .child(text(label).font(font).color(ink))
        });
    } else if let QuickActionKind::Hint(hint) = quick.kind {
        button = button.help(hint);
    }
    let quick_key = key.clone();
    let primary = button
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| match quick.kind {
            QuickActionKind::Run(action) => s.run_git_action(&quick_key, action, None),
            QuickActionKind::Pull => s.git_pull(&quick_key),
            QuickActionKind::OpenPr => s.open_git_pr(&quick_key, tx),
            QuickActionKind::Hint(_) => {}
        })
        .build(k)
        .key("git-quick-action");
    let toggle = key.clone();
    let trigger = IconButton::new(&icons::CHEVRON_DOWN)
        .variant(IconButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .label("Git actions")
        .expanded(git.menu_open)
        .on_press(move |s: &mut J3| {
            if let Some(git) = s.git.get_mut(&toggle) {
                git.menu_open = !git.menu_open;
            }
        })
        .build(k)
        .key("git-menu");
    let item = |label: &str, enabled: bool, run: fn(&mut J3, &ThreadKey, &mut Tx)| {
        let key = key.clone();
        MenuItem::new(label)
            .disabled(!enabled)
            .on_select_tx(move |s: &mut J3, tx: &mut Tx| {
                close_menu(s, &key);
                run(s, &key, tx);
            })
    };
    let mut dropdown = Menu::new("git-control-menu")
        .trigger(trigger)
        .open(git.menu_open)
        .on_close({
            let key = key.clone();
            move |s: &mut J3| close_menu(s, &key)
        })
        .item(item("Commit…", menu.commit, |s, key, _| {
            s.open_commit_dialog(key)
        }))
        .item(item("Push", menu.push, |s, key, _| {
            s.run_git_action(key, GitStackedAction::Push, None)
        }))
        .item(item("Pull", menu.pull, |s, key, _| s.git_pull(key)));
    if menu.view_pr {
        dropdown = dropdown
            .separator()
            .item(item("View PR", true, |s, key, tx| s.open_git_pr(key, tx)));
    } else if menu.create_pr {
        dropdown = dropdown
            .separator()
            .item(item("Create PR", true, |s, key, _| {
                s.run_git_action(key, GitStackedAction::CreatePr, None)
            }));
    }
    let dropdown = dropdown.build(k, ui.layer);
    commit::view(git, ui, key);
    control.child(primary).child(dropdown)
}

fn close_menu(s: &mut J3, key: &ThreadKey) {
    if let Some(git) = s.git.get_mut(key) {
        git.menu_open = false;
    }
}
