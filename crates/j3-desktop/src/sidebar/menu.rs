//! A T3 row's context menu (spec §4.3): t3's thread action menu (`buildThreadActionMenuItems`,
//! threadActionMenu.logic.ts:117-216) without what j3 lacks (branches, guests, regeneration,
//! copy, project settings). One menu is open at a time, at the secondary press.

use j3_client::workflow::{TimeZone, pick_snooze_preset, resolve_snooze_presets};
use rune::{El, PointerPress, Tx};
use rune_kit::{Kit, Menu, MenuEntry, MenuItem};
use time::OffsetDateTime;

use super::{ThreadCommand, ThreadKey, t3::T3Row};
use crate::{app::J3, dialogs::Dialog};

/// The open row menu: whose, where (layer points: the press's window point over the zoom
/// when it opened, so the menu stays by its row if the zoom changes), and which submenu
/// shows.
#[derive(Debug, Clone, PartialEq)]
pub struct RowMenu {
    pub key: ThreadKey,
    pub at: [f32; 2],
    pub sub: Option<Submenu>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submenu {
    Snooze,
    AutoSettle,
}

impl J3 {
    /// Opens `key`'s row menu at window point `at`.
    pub fn open_row_menu(&mut self, key: ThreadKey, at: PointerPress) {
        let zoom = self.prefs.ui().zoom();
        self.sidebar.filter_menu = false;
        self.sidebar.row_menu = Some(RowMenu {
            key,
            at: [at.x / zoom, at.y / zoom],
            sub: None,
        });
    }

    pub fn close_row_menu(&mut self) {
        self.sidebar.row_menu = None;
    }

    fn toggle_submenu(&mut self, sub: Submenu) {
        if let Some(menu) = &mut self.sidebar.row_menu {
            menu.sub = (menu.sub != Some(sub)).then_some(sub);
        }
    }

    /// One command on `key` from the menu, which closes.
    fn menu_command(&mut self, key: &ThreadKey, command: ThreadCommand) {
        self.close_row_menu();
        self.apply_thread_commands(vec![(key.clone(), command)]);
    }

    /// Snoozes `key` with the preset `id` the menu showed (resolved again now: a duration
    /// starts at the pick).
    fn snooze_preset(&mut self, key: &ThreadKey, id: &str) {
        let zone = TimeZone::system();
        let now = self.clock.now;
        let shown = resolve_snooze_presets(now, &zone);
        if let Some(preset) = pick_snooze_preset(&shown, id, now, &zone) {
            self.menu_command(key, ThreadCommand::Snooze(preset.snoozed_until));
        } else {
            self.close_row_menu();
        }
    }
}

/// The menu for `row`, opened by `menu`, into `layer`. Returns its (empty) anchor element.
pub fn build(row: &T3Row, menu: &RowMenu, now: OffsetDateTime, k: &Kit, layer: &mut Vec<El>) -> El {
    let key = &row.key;
    let command = |label: &str, command: ThreadCommand| {
        let key = key.clone();
        MenuItem::new(label).on_select(move |s: &mut J3| s.menu_command(&key, command.clone()))
    };
    let mut m = Menu::new("thread-menu")
        .at(menu.at[0], menu.at[1])
        .open(true)
        .min_width(200.0)
        .on_close(|s: &mut J3| s.close_row_menu())
        .item(if row.pinned {
            command("Unpin thread", ThreadCommand::Unpin)
        } else {
            command("Pin thread", ThreadCommand::Pin(None))
        })
        .item(if row.settled {
            command("Un-settle thread", ThreadCommand::Unsettle)
        } else {
            command("Settle thread", ThreadCommand::Settle)
        });
    m = if row.section == j3_client::workflow::T3Section::Snoozed {
        m.item(command("Wake thread", ThreadCommand::Wake))
    } else if row.snoozable {
        m.item(
            MenuItem::new("Snooze")
                .submenu(snooze_entries(key, now), menu.sub == Some(Submenu::Snooze))
                .on_select(|s: &mut J3| s.toggle_submenu(Submenu::Snooze)),
        )
    } else {
        m.item(MenuItem::new("Snooze").disabled(true))
    };
    let rename = key.clone();
    let archive = key.clone();
    let delete = key.clone();
    m.separator()
        .item(
            MenuItem::new("Rename thread…").on_select_tx(move |s: &mut J3, tx: &mut Tx| {
                s.close_row_menu();
                let dialog = Dialog::rename_thread(s, rename.clone());
                s.open_dialog(dialog, tx);
            }),
        )
        .item(command("Mark unread", ThreadCommand::MarkUnread).disabled(!row.has_completion))
        .item(
            MenuItem::new("Auto-settle")
                .submenu(
                    auto_settle_entries(key, row.auto_settle),
                    menu.sub == Some(Submenu::AutoSettle),
                )
                .on_select(|s: &mut J3| s.toggle_submenu(Submenu::AutoSettle)),
        )
        .separator()
        .item(
            MenuItem::new("Archive thread").on_select(move |s: &mut J3| {
                s.close_row_menu();
                s.archive_thread(archive.clone());
            }),
        )
        .item(
            MenuItem::new("Delete…")
                .danger(true)
                .on_select_tx(move |s: &mut J3, tx: &mut Tx| {
                    s.close_row_menu();
                    let dialog = Dialog::delete_thread(s, delete.clone());
                    s.open_dialog(dialog, tx);
                }),
        )
        .build(k, layer)
}

/// The snooze presets (each with its wake time under it), then "Custom…" (the custom snooze
/// dialog).
fn snooze_entries(key: &ThreadKey, now: OffsetDateTime) -> Vec<MenuEntry> {
    let zone = TimeZone::system();
    let mut entries: Vec<MenuEntry> = resolve_snooze_presets(now, &zone)
        .into_iter()
        .map(|preset| {
            let key = key.clone();
            let id = preset.id.clone();
            MenuEntry::item(
                MenuItem::new(preset.label)
                    .description(preset.when_label)
                    .on_select(move |s: &mut J3| s.snooze_preset(&key, &id)),
            )
        })
        .collect();
    let key = key.clone();
    entries.push(MenuEntry::Separator);
    entries.push(MenuEntry::item(MenuItem::new("Custom…").on_select_tx(
        move |s: &mut J3, tx: &mut Tx| {
            s.close_row_menu();
            s.open_dialog(Dialog::custom_snooze(key.clone()), tx);
        },
    )));
    entries
}

/// Enabled / Disabled, the current one checked.
fn auto_settle_entries(key: &ThreadKey, enabled: bool) -> Vec<MenuEntry> {
    [("Enabled", true), ("Disabled", false)]
        .into_iter()
        .map(|(label, value)| {
            let key = key.clone();
            MenuEntry::item(MenuItem::new(label).checked(enabled == value).on_select(
                move |s: &mut J3| s.menu_command(&key, ThreadCommand::AutoSettle(value)),
            ))
        })
        .collect()
}
