//! Settings (spec §8.2): a full-window page over the shell with General, Notifications,
//! Environments, Projects and Archived threads; every change is saved at once through
//! `J3::update_ui`. Escape and ⌘, leave it (`Action::CloseSettings`, `Action::OpenSettings`).

pub mod controls;
mod general;
mod pages;

use std::sync::Arc;

use rune::{Cx, El, ScrollSpec, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, ComboboxState, shell::Page as ShellPage};

use crate::{
    app::{J3, Ui},
    archived::Archives,
};

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

/// The installed font families, for the font pickers.
#[derive(Debug, Clone, Default)]
pub enum Fonts {
    #[default]
    Unknown,
    Loading,
    Loaded(Arc<[String]>),
}

#[derive(Default)]
pub struct SettingsState {
    pub page: Page,
    /// The zoom listbox is open.
    pub zoom_open: bool,
    pub ui_font: ComboboxState,
    pub mono_font: ComboboxState,
    pub speech_model: ComboboxState,
    pub fonts: Fonts,
    /// Each environment's archived threads, loaded when Settings opens.
    pub archived: Archives,
}

impl J3 {
    /// Opens Settings on `page`; each opening loads the archives afresh.
    pub fn open_settings(&mut self, page: Page, tx: &mut Tx) {
        self.settings.archived.clear();
        self.settings.zoom_open = false;
        self.settings.ui_font.close();
        self.settings.mono_font.close();
        self.settings.speech_model.close();
        self.route.settings = true;
        self.close_new_thread_picker();
        if matches!(self.settings.fonts, Fonts::Unknown) {
            self.load_fonts();
        }
        self.show_settings_page(page, tx);
    }

    /// Shows `page` of the open Settings.
    pub fn show_settings_page(&mut self, page: Page, _: &mut Tx) {
        self.settings.page = page;
        self.settings.zoom_open = false;
        self.load_missing_archives();
    }

    /// While the Archived page shows: loads the archive of any environment listed without
    /// one (added, or reached for the first time, while the page is open). True when a load
    /// started.
    pub(crate) fn poll_settings(&mut self, _: &mut Tx) -> bool {
        self.load_missing_archives()
    }

    fn load_missing_archives(&mut self) -> bool {
        if !self.route.settings || self.settings.page != Page::Archived {
            return false;
        }
        let missing: Vec<String> = self
            .envs
            .distinct()
            .into_iter()
            .map(|env| env.key().to_owned())
            .filter(|key| self.settings.archived.get(key).is_none())
            .collect();
        for env in &missing {
            self.load_archived(env);
        }
        !missing.is_empty()
    }

    /// Leaves Settings (nothing when it isn't open); the sidebar takes focus.
    pub fn close_settings(&mut self, tx: &mut Tx) {
        if !self.route.settings {
            return;
        }
        self.route.settings = false;
        self.settings.zoom_open = false;
        self.focus_sidebar(tx);
    }

    /// Lists the installed font families off the UI thread (once Settings opens).
    pub fn load_fonts(&mut self) {
        self.settings.fonts = Fonts::Loading;
        self.spawn_blocking(|| {
            let families = installed_families();
            move |s: &mut J3, _: &mut Tx| s.settings.fonts = Fonts::Loaded(families.into())
        });
    }
}

/// Every installed font family, sorted, without hidden (`.`-prefixed) system families.
fn installed_families() -> Vec<String> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let mut families: Vec<String> = db
        .faces()
        .flat_map(|face| face.families.iter().map(|(name, _)| name.clone()))
        .filter(|name| !name.is_empty() && !name.starts_with('.'))
        .collect();
    families.sort_by_key(|name| name.to_lowercase());
    families.dedup();
    families
}

/// The Settings page: its tabs, then the chosen page.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui) -> El {
    let k = ui.k;
    let done = Button::new("Done")
        .variant(ButtonVariant::Outline)
        .on_press_tx(|s: &mut J3, tx: &mut Tx| s.close_settings(tx))
        .build(k)
        .key("close-settings");
    let mut tabs = col()
        .key("settings-pages")
        .role("tablist")
        .label("Settings pages")
        .w(200.0)
        .gap(2.0)
        .pt(16.0)
        .px(8.0);
    for page in Page::ALL {
        let on = s.settings.page == page;
        let mut tab = row()
            .key(page.key())
            .role("tab")
            .label(page.title())
            .focusable(true)
            .focus_selected(on)
            .focus_ring(2.0, 1.0, k.c.border_focus)
            .w_full()
            .h(30.0)
            .px(10.0)
            .center_y()
            .radius(6.0)
            .child(
                text(page.title())
                    .font(if on {
                        k.ty.text_small_medium
                    } else {
                        k.ty.text_small
                    })
                    .color(if on { k.c.text_base } else { k.c.text_muted })
                    .a11y_hidden(),
            )
            .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.show_settings_page(page, tx));
        tab = if on {
            tab.fill(k.c.layer_02)
        } else {
            tab.hover(k.c.hover)
        };
        tabs = tabs.child(tab);
    }
    let body = match s.settings.page {
        Page::General => general::view(s, ui),
        Page::Notifications => pages::notifications(s, ui),
        Page::Environments => pages::environments(s, ui),
        Page::Projects => pages::projects(s, ui),
        Page::Archived => pages::archived(s, ui),
    };
    let page = col()
        .key("settings-page")
        .w_full()
        .max_w(720.0)
        .self_center()
        .padding(24.0)
        .gap(24.0)
        .child(body);
    let content = row().key("settings").w_full().h_full().child(tabs).child(
        col()
            .key("settings-scroll")
            .grow(1.0)
            .min_w(0.0)
            .min_h(0.0)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .child(page),
    );
    ShellPage::new(content)
        .title("Settings")
        .trailing(done)
        .titlebar_inset(cx.titlebar_inset())
        .build(k)
}

#[cfg(test)]
mod tests;
