//! Settings › Notifications, Environments, Projects and Archived threads (spec §8.2).

use rune::{El, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Switch};

use super::controls::{Group, item, placeholder};
use crate::{
    app::{J3, Ui},
    dialogs::Dialog,
    icons,
    ui_settings::UiSettings,
};

/// A notification switch.
fn notify_switch(
    k: &Kit,
    key: &str,
    title: &str,
    description: &str,
    on: bool,
    flip: fn(&mut UiSettings),
) -> El {
    item(
        k,
        key,
        title,
        Some(description),
        Switch::new(on)
            .label(title)
            .hide_label()
            .on_toggle_tx(move |s: &mut J3, tx: &mut Tx| s.update_ui(flip, tx))
            .build(k)
            .key(key.to_owned()),
    )
}

pub(super) fn notifications(s: &J3, ui: &Ui) -> El {
    let k = ui.k;
    let prefs = s.prefs.ui();
    Group::new("settings-notifications-group")
        .description("When a thread needs an answer or a thread you aren’t viewing finishes.")
        .row(notify_switch(
            k,
            "toasts",
            "Toasts",
            "Show a notification in the window.",
            prefs.toasts,
            |ui| ui.toasts = !ui.toasts,
        ))
        .row(notify_switch(
            k,
            "sounds",
            "Sounds",
            "Chime while j3 is in the background: one sound for a finished run, another for a question.",
            prefs.sounds,
            |ui| ui.sounds = !ui.sounds,
        ))
        .build(k)
}

/// A small "Remove…" button.
fn remove_button(k: &Kit, key: String, press: impl Fn(&mut J3, &mut Tx) + 'static) -> El {
    Button::new("Remove…")
        .variant(ButtonVariant::Ghost)
        .control_size(ControlSize::Small)
        .on_press_tx(press)
        .build(k)
        .key(key)
}

/// A small outlined "Add …" button.
fn add_button(k: &Kit, label: &str, key: String, press: impl Fn(&mut J3, &mut Tx) + 'static) -> El {
    Button::new(label)
        .icon(&icons::PLUS)
        .variant(ButtonVariant::Outline)
        .control_size(ControlSize::Small)
        .on_press_tx(press)
        .build(k)
        .key(key)
}

/// A row's two lines: `title` over a muted, truncated `detail`.
fn title_detail(k: &Kit, title: &str, detail: &str) -> El {
    col()
        .grow(1.0)
        .shrink()
        .min_w(0.0)
        .gap(2.0)
        .child(
            text(title.to_owned())
                .font(k.ty.text_small)
                .color(k.c.text_base),
        )
        .child(
            text(detail.to_owned())
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .truncate(),
        )
}

pub(super) fn environments(s: &J3, ui: &Ui) -> El {
    let k = ui.k;
    let mut group = Group::new("settings-environments-group").description(
        "The j3 servers this window connects to. This computer’s own server is always listed.",
    );
    for env in s.envs.iter() {
        let key = env.key().to_owned();
        let status = text(env.status_label().unwrap_or("Connected"))
            .key(format!("environment-status-{key}"))
            .font(k.ty.text_xsmall)
            .color(k.c.text_muted)
            .help(env.status_detail());
        let end = if env.is_local() {
            text("This computer")
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
        } else {
            let remove = key.clone();
            remove_button(k, format!("remove-environment-{key}"), move |s, tx| {
                let dialog = Dialog::remove_environment(s, remove.clone());
                s.open_dialog(dialog, tx);
            })
        };
        group = group.row(
            row()
                .key(format!("environment-{key}"))
                .w_full()
                .gap(12.0)
                .px(16.0)
                .py(10.0)
                .center_y()
                .child(title_detail(k, env.label(), env.connection().http_base()))
                .child(status)
                .child(end),
        );
    }
    group
        .footer(add_button(
            k,
            "Add environment…",
            "add-environment".into(),
            |s, tx| s.open_dialog(Dialog::add_environment(), tx),
        ))
        .build(k)
}

pub(super) fn projects(s: &J3, ui: &Ui) -> El {
    let k = ui.k;
    let mut page = col().key("settings-projects-page").w_full().gap(24.0);
    for env in s.envs.distinct() {
        let env_key = env.key().to_owned();
        let shell = &env.state().shell;
        let mut group = Group::new(format!("projects-{env_key}")).title(env.label());
        for project in &shell.projects {
            let (remove_env, remove_id) = (env_key.clone(), project.id.clone());
            group = group.row(
                row()
                    .key(format!("project-{env_key}-{}", project.id))
                    .w_full()
                    .gap(12.0)
                    .px(16.0)
                    .py(10.0)
                    .center_y()
                    .child(title_detail(k, &project.title, &project.workspace_root))
                    .child(remove_button(
                        k,
                        format!("remove-project-{env_key}-{}", project.id),
                        move |s, tx| s.ask_remove_project(&remove_env, &remove_id, tx),
                    )),
            );
        }
        if shell.projects.is_empty() {
            let line = if shell.loaded {
                "No projects"
            } else {
                "Connecting…"
            };
            group = group.row(placeholder(k, line));
        }
        let add_env = env_key.clone();
        group = group.footer(add_button(
            k,
            "Add project…",
            format!("add-project-{env_key}"),
            move |s, tx| s.open_dialog(Dialog::add_project(add_env.clone(), false), tx),
        ));
        page = page.child(group.build(k));
    }
    page
}

pub(super) fn archived(s: &J3, ui: &Ui) -> El {
    let k = ui.k;
    let mut page = col().key("settings-archived-page").w_full().gap(24.0);
    for env in s.envs.distinct() {
        page = page.child(
            Group::new(format!("archived-group-{}", env.key()))
                .title(env.label())
                .row(crate::archived::view(s, ui, env.key()))
                .build(k),
        );
    }
    page
}
