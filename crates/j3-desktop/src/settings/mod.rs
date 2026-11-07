//! Settings (spec §8.2): a full-window page over the shell with General, Notifications,
//! Environments, Projects and Archived threads; every change is saved at once through
//! `J3::update_ui`. Escape and ⌘, leave it (`Action::CloseSettings`, `Action::OpenSettings`).

use rune::{Cx, El, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, Switch, shell::Page as ShellPage};

use crate::app::{J3, Ui};

/// One settings page, in sidebar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    #[default]
    General,
    Notifications,
    Environments,
    Projects,
    Archived,
}

impl Page {
    pub const ALL: [Page; 5] = [
        Page::General,
        Page::Notifications,
        Page::Environments,
        Page::Projects,
        Page::Archived,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Notifications => "Notifications",
            Page::Environments => "Environments",
            Page::Projects => "Projects",
            Page::Archived => "Archived threads",
        }
    }

    /// The element key of the page's tab.
    pub fn key(self) -> &'static str {
        match self {
            Page::General => "settings-general",
            Page::Notifications => "settings-notifications",
            Page::Environments => "settings-environments",
            Page::Projects => "settings-projects",
            Page::Archived => "settings-archived",
        }
    }
}

#[derive(Debug, Default)]
pub struct SettingsState {
    pub page: Page,
}

impl J3 {
    /// Opens Settings on `page`.
    pub fn open_settings(&mut self, page: Page, _: &mut Tx) {
        self.settings.page = page;
        self.route.settings = true;
    }

    /// Leaves Settings (nothing when it isn't open).
    pub fn close_settings(&mut self, _: &mut Tx) {
        self.route.settings = false;
    }
}

/// The Settings page: its tabs, then the chosen page.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui) -> El {
    let k = ui.k;
    let done = Button::new("Done")
        .variant(ButtonVariant::Neutral)
        .on_press_tx(|s: &mut J3, tx: &mut Tx| s.close_settings(tx))
        .build(k)
        .key("close-settings");
    let mut tabs = col().key("settings-pages").w(200.0).gap(2.0).pt(16.0).px(8.0);
    for page in Page::ALL {
        let variant = if s.settings.page == page {
            ButtonVariant::Neutral
        } else {
            ButtonVariant::Ghost
        };
        tabs = tabs.child(
            Button::new(page.title())
                .variant(variant)
                .on_press(move |s: &mut J3| s.settings.page = page)
                .build(k)
                .key(page.key()),
        );
    }
    let body = match s.settings.page {
        Page::Notifications => notifications(s, ui),
        Page::Environments => environments(s, ui),
        page => col().child(text(page.title()).font(k.ty.body_medium).color(k.c.text_base)),
    };
    let content = row()
        .key("settings")
        .w_full()
        .h_full()
        .child(tabs)
        .child(col().key("settings-page").grow(1.0).min_w(0.0).padding(24.0).gap(16.0).child(body));
    ShellPage::new(content)
        .title("Settings")
        .trailing(done)
        .titlebar_inset(cx.titlebar_inset())
        .build(k)
}

fn notifications(s: &J3, ui: &Ui) -> El {
    let k = ui.k;
    let ui_settings = s.prefs.ui();
    col()
        .gap(16.0)
        .child(
            Switch::new(ui_settings.toasts)
                .label("Toasts")
                .description(
                    "When a thread needs an answer or a thread you aren’t viewing finishes.",
                )
                .on_toggle_tx(|s: &mut J3, tx: &mut Tx| s.update_ui(|ui| ui.toasts = !ui.toasts, tx))
                .build(k)
                .key("toasts"),
        )
        .child(
            Switch::new(ui_settings.sounds)
                .label("Sounds")
                .description(
                    "When a thread needs an answer or a thread you aren’t viewing finishes.",
                )
                .on_toggle_tx(|s: &mut J3, tx: &mut Tx| s.update_ui(|ui| ui.sounds = !ui.sounds, tx))
                .build(k)
                .key("sounds"),
        )
}

fn environments(s: &J3, ui: &Ui) -> El {
    let k = ui.k;
    let mut list = col().key("environments").gap(12.0);
    for env in s.envs.iter() {
        let status = env.status_label().unwrap_or("Connected");
        let detail = if env.is_local() {
            "This computer".to_owned()
        } else {
            env.connection().http_base().to_owned()
        };
        list = list.child(
            row()
                .key(format!("environment-{}", env.key()))
                .w_full()
                .gap(12.0)
                .center_y()
                .child(
                    col()
                        .grow(1.0)
                        .child(text(env.label()).font(k.ty.text_small_medium).color(k.c.text_base))
                        .child(text(detail).font(k.ty.text_xsmall).color(k.c.text_muted)),
                )
                .child(
                    text(status)
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_muted)
                        .help(env.status_detail()),
                ),
        );
    }
    list
}
