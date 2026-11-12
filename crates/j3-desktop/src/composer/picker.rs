//! The model and thinking picker (spec §7.4): one trigger (provider logo, model, level) and a
//! searchable menu with the starred models first, then one section per provider headed by
//! its logo, a star per row that toggles the favourite without choosing, and the thinking
//! levels in the footer. Drawn from rune-kit's popover building blocks and `TextInput`, as
//! `Combobox` is, because its rows are sections with headers, stars and a footer.

use j3_client::entities::ThinkingLevel;
use rune::{El, Key, TextEdit, Tx, col, el, icon, row, spacer, text};
use rune_kit::{Placement, TextInput, popover, scrim};

use super::ComposerKey;
use crate::{
    app::{J3, Ui},
    icons, logos,
    models::{self, ModelChoice},
};

/// The menu's open state, search and highlighted row.
#[derive(Default)]
pub(super) struct PickerState {
    pub open: bool,
    pub search: TextEdit,
    /// The highlighted row: its index among the shown rows (a starred model shows twice,
    /// under Favorites and under its provider, and only one of them is highlighted).
    pub active: Option<usize>,
}

/// A shown row: its model's slug and its element key.
struct Row {
    model: String,
    key: String,
}

/// One shown section: `None` is Favorites, else the provider; rows index the choices.
struct Section {
    provider: Option<String>,
    rows: Vec<usize>,
}

/// The sections `query` shows: starred models first, then each provider's; a query naming
/// a provider shows all its models, otherwise rows match by name or slug.
fn sections(choices: &[ModelChoice], favorites: &[String], query: &str) -> Vec<Section> {
    let query = query.trim().to_lowercase();
    let matches = |choice: &ModelChoice| {
        choice.name.to_lowercase().contains(&query) || choice.slug().to_lowercase().contains(&query)
    };
    let starred: Vec<usize> = favorites
        .iter()
        .filter_map(|slug| choices.iter().position(|c| c.slug() == slug))
        .filter(|&ix| matches(&choices[ix]))
        .collect();
    let mut shown = Vec::new();
    if !starred.is_empty() {
        shown.push(Section {
            provider: None,
            rows: starred,
        });
    }
    for (provider, rows) in models::by_provider(choices) {
        let names_provider = !query.is_empty()
            && (provider.contains(&query)
                || logos::provider_name(provider)
                    .to_lowercase()
                    .contains(&query));
        let rows: Vec<usize> = if names_provider {
            rows
        } else {
            rows.into_iter()
                .filter(|&ix| matches(&choices[ix]))
                .collect()
        };
        if !rows.is_empty() {
            shown.push(Section {
                provider: Some(provider.to_owned()),
                rows,
            });
        }
    }
    shown
}

impl J3 {
    fn toggle_picker(&mut self, key: &ComposerKey, tx: &mut Tx) {
        let open = !self.composer(key).picker.open;
        let picker = &mut self.composer(key).picker;
        picker.open = open;
        picker.search.clear();
        picker.search.focused = open;
        let current = self.composer_choice(key).map(|c| c.slug().to_owned());
        let rows = self.picker_rows(key);
        let active = current.and_then(|current| rows.iter().position(|r| r.model == current));
        self.composer(key).picker.active = active;
        if let Some(row) = active.map(|i| &rows[i]) {
            tx.scroll_into_view(row.key.clone());
        }
        tx.instant();
    }

    fn close_picker(&mut self, key: &ComposerKey) {
        let composer = self.composer(key);
        composer.picker.open = false;
        composer.picker.search.focused = false;
        composer.edit.focused = true;
    }

    /// Chooses the model `slug` and closes the menu.
    pub fn choose_model(&mut self, key: &ComposerKey, slug: String) {
        self.composer(key).model = Some(slug);
        self.close_picker(key);
    }

    /// Chooses thinking `level` for the next submissions.
    pub fn choose_level(&mut self, key: &ComposerKey, level: ThinkingLevel) {
        self.composer(key).level = Some(level);
    }

    /// The rows shown now, in order.
    fn picker_rows(&self, key: &ComposerKey) -> Vec<Row> {
        let Some(env) = self.envs.get(key.env()) else {
            return Vec::new();
        };
        let choices = env.models();
        let query = self
            .composers
            .get(key)
            .map_or("", |c| c.picker.search.text.as_str());
        sections(choices, &self.prefs.ui().favorite_models, query)
            .iter()
            .flat_map(|section| {
                section.rows.iter().map(|&ix| Row {
                    model: choices[ix].slug().to_owned(),
                    key: row_key(&choices[ix], section.provider.is_some()),
                })
            })
            .collect()
    }

    /// The search field's keys: ↑ ↓ move, Enter chooses, Escape closes.
    fn picker_key(&mut self, key: &ComposerKey, ev: &rune::KeyEvent, tx: &mut Tx) -> bool {
        if self.composer(key).picker.search.preedit().is_some() {
            return false;
        }
        let rows = self.picker_rows(key);
        let at = self.composer(key).picker.active.filter(|&i| i < rows.len());
        let delta: isize = match ev.key {
            Key::Escape => {
                self.close_picker(key);
                return true;
            }
            Key::Enter => {
                if let Some(row) = at.or((!rows.is_empty()).then_some(0)).map(|i| &rows[i]) {
                    self.choose_model(key, row.model.clone());
                }
                return true;
            }
            Key::Down => 1,
            Key::Up => -1,
            _ => return false,
        };
        if !rows.is_empty() {
            let next = match at {
                Some(at) => (at as isize + delta).rem_euclid(rows.len() as isize) as usize,
                None if delta > 0 => 0,
                None => rows.len() - 1,
            };
            self.composer(key).picker.active = Some(next);
            tx.scroll_into_view(rows[next].key.clone());
        }
        true
    }

    /// The search changed: the first match is highlighted.
    fn picker_searched(&mut self, key: &ComposerKey) {
        self.composer(key).picker.active = Some(0);
    }
}

/// A row's element key: under Favorites or under its provider.
fn row_key(choice: &ModelChoice, in_provider_section: bool) -> String {
    if in_provider_section {
        format!("model-{}", choice.slug())
    } else {
        format!("model-favorite-{}", choice.slug())
    }
}

/// The trigger, and the menu in the layer while it is open.
pub(super) fn view(s: &J3, ui: &mut Ui, key: &ComposerKey) -> El {
    let k = ui.k;
    let m = ui.m;
    let choice = s.composer_choice(key);
    let levels = s.composer_levels(key);
    let level_label = levels
        .as_ref()
        .and_then(|levels| levels.chosen)
        .map(models::level_label);
    let open = s.composers.get(key).is_some_and(|c| c.picker.open);
    let mut trigger = row()
        .key("model-picker")
        .role("button")
        .label("Model")
        .help("Model and thinking level")
        .max_w(m.rems(22.0))
        .min_w(0.0)
        .h(28.0)
        .px(8.0)
        .gap(6.0)
        .center_y()
        .radius(k.radius.md)
        .hover(k.c.hover);
    if let Some(choice) = choice {
        trigger = trigger.child(logos::provider_logo(&choice.provider, 14.0, k));
    }
    trigger = trigger.child(
        text(choice.map_or("Model", |c| c.label.section_text()))
            .font(k.ty.text_small)
            .color(k.c.text_base)
            .shrink()
            .truncate(),
    );
    if let Some(level) = level_label {
        trigger = trigger.child(text(level).font(k.ty.text_small).color(k.c.text_muted));
    }
    let toggle = key.clone();
    let trigger = trigger
        .child(icon(&icons::CHEVRON_DOWN, 12.0).color(k.c.icon_muted))
        .expanded(open)
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.toggle_picker(&toggle, tx));
    if open {
        menu(s, ui, key);
    }
    trigger
}

/// The open menu: search, sections, thinking levels.
fn menu(s: &J3, ui: &mut Ui, key: &ComposerKey) {
    let k = ui.k;
    let m = ui.m;
    let Some(composer) = s.composers.get(key) else {
        return;
    };
    let choices: &[ModelChoice] = s.envs.get(key.env()).map_or(&[], |env| env.models());
    let favorites = &s.prefs.ui().favorite_models;
    let current = s.composer_choice(key).map(ModelChoice::slug);
    let picker = &composer.picker;
    let shown = sections(choices, favorites, &picker.search.text);

    let (edit_key, key_key, change_key) = (key.clone(), key.clone(), key.clone());
    let search = TextInput::new(&picker.search)
        .plain()
        .leading_icon(k.glyph.search)
        .height(36.0)
        .width(None)
        .placeholder("Search models")
        .label("Search models")
        .on_edit(move |s: &mut J3| &mut s.composer(&edit_key).picker.search)
        .on_key(move |s: &mut J3, ev, tx| s.picker_key(&key_key, ev, tx))
        .on_change(move |s: &mut J3, _| s.picker_searched(&change_key))
        .build(k)
        .key("model-search");

    let mut list = col().key("model-rows").w_full().padding(4.0);
    let mut shown_ix = 0;
    if shown.is_empty() {
        list = list.child(
            row().h(28.0).px(8.0).center_y().child(
                text("No models")
                    .font(k.ty.text_small)
                    .color(k.c.text_muted),
            ),
        );
    }
    for section in &shown {
        let header = match &section.provider {
            Some(provider) => row()
                .key(format!("model-section-{provider}"))
                .child(logos::provider_logo(provider, 14.0, k))
                .child(
                    text(logos::provider_name(provider))
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_muted),
                ),
            None => row()
                .key("model-section-favorites")
                .child(icon(&icons::STAR_FILL, 12.0).color(crate::theme::Status::of(k).warning))
                .child(
                    text("Favorites")
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_muted),
                ),
        };
        list = list.child(header.gap(6.0).px(8.0).pt(6.0).pb(2.0).center_y());
        for &ix in &section.rows {
            let choice = &choices[ix];
            list = list.child(model_row(
                ui,
                key,
                choice,
                section.provider.is_some(),
                favorites.iter().any(|f| f == choice.slug()),
                current == Some(choice.slug()),
                picker.active == Some(shown_ix),
            ));
            shown_ix += 1;
        }
    }

    let mut body = col()
        .key("model-menu")
        .w(m.rems(24.0))
        .max_h(m.rems(26.0))
        .min_h(0.0)
        .child(row().key("model-search-row").w_full().px(4.0).child(search))
        .child(el().w_full().h(1.0).fill(k.c.border_weak))
        .child(popover::fit_scroll(list));
    if let Some(levels) = s.composer_levels(key)
        && !levels.options.is_empty()
    {
        body = body
            .child(el().w_full().h(1.0).fill(k.c.border_weak))
            .child(level_chips(ui, key, &levels.options, levels.chosen));
    }
    let close = key.clone();
    ui.layer.push(scrim("model-menu-scrim", move |s: &mut J3| {
        s.close_picker(&close)
    }));
    let surface = popover::raised(k, body.preserve_focus_on_press(true), k.radius.md);
    ui.layer.push(popover::anchored(
        k,
        surface,
        "model-picker",
        Placement::TopStart,
        4.0,
    ));
}

/// A model row: name, muted qualifier, the star, and the check when chosen.
fn model_row(
    ui: &Ui,
    key: &ComposerKey,
    choice: &ModelChoice,
    in_provider_section: bool,
    favorite: bool,
    checked: bool,
    active: bool,
) -> El {
    let k = ui.k;
    let slug = choice.slug().to_owned();
    let label = &choice.label;
    let qualifier = if in_provider_section {
        label.section_qualifier()
    } else {
        label.qualifier()
    };
    let (star, star_color) = if favorite {
        (&icons::STAR_FILL, crate::theme::Status::of(k).warning)
    } else {
        (&icons::STAR, k.alpha(k.c.text_muted, 0.6))
    };
    let favorite_slug = slug.clone();
    let star = row()
        .key(format!("favorite-{slug}"))
        .role("button")
        .label(if favorite {
            "Unstar model"
        } else {
            "Star model"
        })
        .size(20.0, 20.0)
        .center()
        .radius(k.radius.sm)
        .hover(k.c.hover)
        .child(icon(star, 12.0).color(star_color))
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| {
            let slug = favorite_slug.clone();
            s.update_ui(move |ui| ui.toggle_favorite(&slug), tx);
        });
    let mut item = row()
        .key(row_key(choice, in_provider_section))
        .role("option")
        .label(label.text())
        .checked(checked)
        .w_full()
        .h(28.0)
        .px(8.0)
        .gap(8.0)
        .center_y()
        .radius(k.radius.sm)
        .fill(if active {
            k.c.hover
        } else {
            rune::Color::TRANSPARENT
        })
        .hover(k.c.hover)
        .child(
            text(label.name())
                .font(k.ty.text_small)
                .color(k.c.text_base),
        );
    if let Some(qualifier) = qualifier {
        item = item.child(
            text(qualifier)
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .shrink()
                .truncate(),
        );
    }
    let choose = key.clone();
    let model = slug;
    item.child(spacer())
        .child(star)
        .child(
            el().w(14.0)
                .children(checked.then(|| icon(k.glyph.check_small, 14.0).color(k.c.icon_base))),
        )
        .on_press(move |s: &mut J3| s.choose_model(&choose, model.clone()))
}

/// The footer: "Thinking" and one chip per level, the chosen one highlighted.
fn level_chips(
    ui: &Ui,
    key: &ComposerKey,
    levels: &[ThinkingLevel],
    chosen: Option<ThinkingLevel>,
) -> El {
    let k = ui.k;
    let chips = levels.iter().map(|&level| {
        let selected = chosen == Some(level);
        let choose = key.clone();
        let label = models::level_label(level);
        row()
            .key(format!("thinking-{}", models::level_id(level)))
            .role("button")
            .label(label)
            .checked(selected)
            .px(6.0)
            .py(2.0)
            .radius(k.radius.sm)
            .fill(if selected {
                k.c.bg_accent
            } else {
                rune::Color::TRANSPARENT
            })
            .hover(if selected { k.c.bg_accent } else { k.c.hover })
            .child(text(label).font(k.ty.text_xsmall).color(if selected {
                k.c.text_contrast
            } else {
                k.c.text_base
            }))
            .on_press(move |s: &mut J3| s.choose_level(&choose, level))
    });
    row()
        .key("thinking-levels")
        .flex_wrap()
        .gap(4.0)
        .px(8.0)
        .py(6.0)
        .center_y()
        .child(
            text("Thinking")
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .mr(4.0),
        )
        .children(chips)
}
