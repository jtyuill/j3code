//! Settings › General (spec §8.2): the thread workflow and its inbox and herdr options,
//! appearance (zoom, font families, font sizes), conversation (text width, math), the
//! dictation model, and the composer's usage windows.

use j3_client::herdr::{AgentOrder, StatusIndicators};
use rune::{El, Tx, col};
use rune_kit::{Combobox, ComboboxOption, ComboboxState, Kit, Select, SelectAppearance, Switch};

use super::{
    Fonts,
    controls::{Group, choice, item, stepper},
};
use crate::{
    app::{J3, Ui},
    speech::models::SpeechModel,
    ui_settings::{
        CODE_FONT_SIZE_MAX, CODE_FONT_SIZE_MIN, INTERFACE_FONT_SIZE_MAX, INTERFACE_FONT_SIZE_MIN,
        TERMINAL_FONT_SIZE_MAX, TERMINAL_FONT_SIZE_MIN, TEXT_MEASURE_MAX, TEXT_MEASURE_MIN,
        UiSettings, Workflow, ZOOM_LEVELS,
    },
};

/// The font pickers' first option: no family chosen.
const DEFAULT_FONT: &str = "__default__";

const WORKFLOWS: [(Workflow, &str); 2] = [(Workflow::Inbox, "Inbox"), (Workflow::Herdr, "herdr")];
const INDICATORS: [(StatusIndicators, &str); 2] = [
    (StatusIndicators::Dots, "Dots"),
    (StatusIndicators::Symbols, "Symbols"),
];
const ORDERS: [(AgentOrder, &str); 2] = [
    (AgentOrder::Spaces, "By space"),
    (AgentOrder::Attention, "By attention"),
];

/// A segmented setting over `options`, the current value selected; picking saves it.
fn segmented_setting<T: Copy + PartialEq + 'static>(
    k: &Kit,
    id: &str,
    options: &'static [(T, &'static str)],
    current: T,
    set: fn(&mut UiSettings, T),
) -> El {
    let labels: Vec<&str> = options.iter().map(|(_, label)| *label).collect();
    let selected = options.iter().position(|(value, _)| *value == current);
    choice(k, id, &labels, selected, move |s: &mut J3, ix, tx| {
        let value = options[ix].0;
        s.update_ui(|ui| set(ui, value), tx);
    })
}

/// A switch over a boolean preference.
fn switch(k: &Kit, key: &str, title: &str, on: bool, flip: fn(&mut UiSettings)) -> El {
    Switch::new(on)
        .label(title)
        .hide_label()
        .on_toggle_tx(move |s: &mut J3, tx: &mut Tx| s.update_ui(flip, tx))
        .build(k)
        .key(key.to_owned())
}

/// A font-size stepper over `min..=max` saving through `set`.
fn size(
    k: &Kit,
    id: &str,
    value: f32,
    range: (f32, f32, f32),
    set: fn(&mut UiSettings, f32),
) -> El {
    stepper(k, id, value, range, move |s: &mut J3, size, tx| {
        s.update_ui(|ui| set(ui, size), tx)
    })
}

pub(super) fn view(s: &J3, ui: &mut Ui) -> El {
    let k = ui.k;
    let prefs = s.prefs.ui();
    let workflow = Group::new("settings-workflow").title("Thread workflow").row(item(
        k,
        "workflow",
        "Workflow",
        Some("Inbox settles finished threads onto shelves. herdr lists every agent by what it needs from you."),
        segmented_setting(k, "workflow", &WORKFLOWS, prefs.workflow, |ui, v| ui.workflow = v),
    ));
    let inbox = Group::new("settings-inbox").title("Inbox").row(item(
        k,
        "working-shelf",
        "Working shelf",
        Some("Running threads wait on their own shelf. Turn off to keep them in Active and arrange Active by dragging."),
        switch(k, "working-shelf", "Working shelf", prefs.working_shelf, |ui| {
            ui.working_shelf = !ui.working_shelf
        }),
    ));
    let herdr = Group::new("settings-herdr")
        .title("herdr")
        .row(item(
            k,
            "status-indicators",
            "Status indicators",
            Some("Dots: ● working, blocked, done; ○ idle. Symbols: a spinning ring working, × blocked, ✓ done, ○ idle."),
            segmented_setting(k, "status-indicators", &INDICATORS, prefs.status_indicators, |ui, v| {
                ui.status_indicators = v
            }),
        ))
        .row(item(
            k,
            "agent-order",
            "Agent order",
            Some("By space follows the project order. By attention puts blocked, then done agents first."),
            segmented_setting(k, "agent-order", &ORDERS, prefs.agent_order, |ui, v| ui.agent_order = v),
        ));
    let conversation = Group::new("settings-conversation")
        .title("Conversation")
        .row(item(
            k,
            "text-width",
            "Text width",
            Some("Widest the conversation text and composer get, in rem (the interface font size). Lower is a tighter, easier-to-read column."),
            size(
                k,
                "text-width",
                prefs.measure(),
                (TEXT_MEASURE_MIN, TEXT_MEASURE_MAX, 2.0),
                |ui, v| ui.text_measure = v,
            ),
        ))
        .row(item(
            k,
            "render-math",
            "Render math",
            Some("Typeset LaTeX in messages."),
            switch(k, "render-math", "Render math", prefs.math_rendering, |ui| {
                ui.math_rendering = !ui.math_rendering
            }),
        ));
    let usage = Group::new("settings-usage")
        .title("Usage")
        .row(usage_item(
            k,
            prefs,
            "anthropic",
            "Anthropic",
            "Show session and weekly usage beside the model while an Anthropic model is chosen.",
        ))
        .row(usage_item(
            k,
            prefs,
            "opencode-go",
            "OpenCode Go",
            "Show session, weekly and monthly usage beside the model while an OpenCode Go model is chosen.",
        ));
    let appearance = appearance(s, ui);
    let dictation = dictation(s, ui);
    let k = ui.k;
    col()
        .key("settings-general-page")
        .w_full()
        .gap(24.0)
        .child(workflow.build(k))
        .child(inbox.build(k))
        .child(herdr.build(k))
        .child(appearance)
        .child(conversation.build(k))
        .child(dictation)
        .child(usage.build(k))
}

/// A switch for showing `provider`'s usage windows in the composer.
fn usage_item(
    k: &Kit,
    prefs: &UiSettings,
    provider: &'static str,
    name: &str,
    description: &str,
) -> El {
    let on = prefs.shows_usage(provider);
    let key = format!("usage-{provider}");
    item(
        k,
        &key,
        name,
        Some(description),
        Switch::new(on)
            .label(name)
            .hide_label()
            .on_toggle_tx(move |s: &mut J3, tx: &mut Tx| {
                s.update_ui(|ui| ui.set_shows_usage(provider, !on), tx)
            })
            .build(k)
            .key(key.clone()),
    )
}

/// `75%`, `110%`, …
fn zoom_label(zoom: f32) -> String {
    format!("{:.0}%", zoom * 100.0)
}

/// Zoom, the two font families and the font sizes.
fn appearance(s: &J3, ui: &mut Ui) -> El {
    let k = ui.k;
    let prefs = s.prefs.ui();
    let levels: Vec<String> = ZOOM_LEVELS.iter().map(|&z| zoom_label(z)).collect();
    let current = levels
        .iter()
        .zip(ZOOM_LEVELS)
        .find(|(_, level)| (level - prefs.zoom()).abs() < 1e-3)
        .map(|(label, _)| label.clone());
    let zoom = Select::new("zoom", &levels)
        .appearance(SelectAppearance::Field)
        .width(160.0)
        .label("Zoom")
        .current(current.as_deref())
        .open(s.settings.zoom_open)
        .on_toggle(|s: &mut J3| s.settings.zoom_open = !s.settings.zoom_open)
        .on_close(|s: &mut J3| s.settings.zoom_open = false)
        .on_choose(|s: &mut J3, label: &str, tx: &mut Tx| {
            s.settings.zoom_open = false;
            if let Some(zoom) = ZOOM_LEVELS
                .iter()
                .copied()
                .find(|&z| zoom_label(z) == label)
            {
                s.update_ui(|ui| ui.zoom = zoom, tx);
            }
        })
        .build(k, ui.layer);
    let ui_font = font_picker(
        s,
        ui,
        "ui-font",
        "Interface font",
        "System default",
        &s.settings.ui_font,
        prefs.ui_font.as_deref(),
        ui_font_state,
        |ui, font| ui.ui_font = font,
    );
    let mono_font = font_picker(
        s,
        ui,
        "mono-font",
        "Code font",
        "Default monospace",
        &s.settings.mono_font,
        prefs.mono_font.as_deref(),
        mono_font_state,
        |ui, font| ui.mono_font = font,
    );
    let k = ui.k;
    Group::new("settings-appearance")
        .title("Appearance")
        .row(item(
            k,
            "zoom",
            "Zoom",
            Some("Scales the whole window. View › Zoom In and Zoom Out step through the sizes."),
            zoom,
        ))
        .row(item(
            k,
            "ui-font",
            "Interface font",
            Some("Used for messages, the sidebar and controls."),
            ui_font,
        ))
        .row(item(
            k,
            "mono-font",
            "Code font",
            Some("Used for tool calls, their output and code."),
            mono_font,
        ))
        .row(item(
            k,
            "interface-font-size",
            "Interface font size",
            Some("In px at 100% zoom. Spacing and controls grow and shrink with it."),
            size(
                k,
                "interface-font-size",
                prefs.interface_font_size(),
                (INTERFACE_FONT_SIZE_MIN, INTERFACE_FONT_SIZE_MAX, 1.0),
                |ui, v| ui.interface_font_size = v,
            ),
        ))
        .row(item(
            k,
            "code-font-size",
            "Code font size",
            Some("In px at 100% zoom. Used for tool output and the file preview."),
            size(
                k,
                "code-font-size",
                prefs.code_font_size(),
                (CODE_FONT_SIZE_MIN, CODE_FONT_SIZE_MAX, 1.0),
                |ui, v| ui.code_font_size = v,
            ),
        ))
        .row(item(
            k,
            "terminal-font-size",
            "Terminal font size",
            Some("In px at 100% zoom."),
            size(
                k,
                "terminal-font-size",
                prefs.terminal_font_size(),
                (TERMINAL_FONT_SIZE_MIN, TERMINAL_FONT_SIZE_MAX, 1.0),
                |ui, v| ui.terminal_font_size = v,
            ),
        ))
        .build(k)
}

fn ui_font_state(s: &mut J3) -> &mut ComboboxState {
    &mut s.settings.ui_font
}

fn mono_font_state(s: &mut J3) -> &mut ComboboxState {
    &mut s.settings.mono_font
}

/// A searchable picker of the installed font families, `default_label` first.
#[allow(clippy::too_many_arguments)]
fn font_picker(
    s: &J3,
    ui: &mut Ui,
    key: &str,
    title: &str,
    default_label: &str,
    state: &ComboboxState,
    chosen: Option<&str>,
    access: for<'a> fn(&'a mut J3) -> &'a mut ComboboxState,
    set: fn(&mut UiSettings, Option<String>),
) -> El {
    let mut options = vec![ComboboxOption::new(DEFAULT_FONT, default_label)];
    let mut empty = "No fonts match";
    match &s.settings.fonts {
        Fonts::Loaded(families) => {
            options.extend(
                families
                    .iter()
                    .map(|family| ComboboxOption::new(family, family)),
            );
        }
        Fonts::Unknown | Fonts::Loading => empty = "Loading fonts…",
    }
    // A chosen family that isn't installed (or isn't listed yet) still reads as chosen.
    if let Some(font) = chosen
        && !options.iter().any(|o| o.id == font)
    {
        options.push(ComboboxOption::new(font, font));
    }
    Combobox::new(key, state, &options, access)
        .label(title)
        .current(Some(chosen.unwrap_or(DEFAULT_FONT)))
        .search_placeholder("Search fonts")
        .empty_message(empty)
        .width(240.0)
        .max_height(320.0)
        .on_choose(move |s: &mut J3, id: &str, tx: &mut Tx| {
            let font = (id != DEFAULT_FONT).then(|| id.to_owned());
            s.update_ui(|ui| set(ui, font), tx);
        })
        .build(ui.k, ui.layer)
}

/// The speech-model picker's first option: the model recommended for this machine.
const RECOMMENDED_MODEL: &str = "recommended";

/// The dictation model: the one recommended for this machine, or one picked by name.
fn dictation(s: &J3, ui: &mut Ui) -> El {
    let engine = &s.services.speech;
    let hardware = engine.hardware().describe();
    let options: Vec<ComboboxOption> = std::iter::once(ComboboxOption::new(
        RECOMMENDED_MODEL,
        format!("Recommended: {}", engine.recommended().title()),
    ))
    .chain(
        SpeechModel::ALL
            .iter()
            .map(|model| ComboboxOption::new(model.key(), model.title())),
    )
    .collect();
    let current = s
        .prefs
        .ui()
        .dictation_model()
        .map_or(RECOMMENDED_MODEL, SpeechModel::key);
    let picker = Combobox::new(
        "speech-model",
        &s.settings.speech_model,
        &options,
        speech_model_state,
    )
    .label("Speech model")
    .current(Some(current))
    .search_placeholder("Search models")
    .width(300.0)
    .on_choose(|s: &mut J3, id: &str, tx: &mut Tx| {
        let model = SpeechModel::from_key(id).map(|model| model.key().to_owned());
        s.update_ui(|ui| ui.dictation_model = model, tx);
    })
    .build(ui.k, ui.layer);
    let description = format!(
        "English, transcribed on this machine by Whisper ({hardware}). A model downloads the first time you dictate with it. Larger models are more accurate and slower."
    );
    Group::new("settings-dictation")
        .title("Dictation")
        .row(item(
            ui.k,
            "speech-model",
            "Speech model",
            Some(&description),
            picker,
        ))
        .build(ui.k)
}

fn speech_model_state(s: &mut J3) -> &mut ComboboxState {
    &mut s.settings.speech_model
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_levels_read_as_percentages() {
        let labels: Vec<String> = ZOOM_LEVELS.iter().map(|&z| zoom_label(z)).collect();
        assert_eq!(labels.first().map(String::as_str), Some("75%"));
        assert_eq!(labels.last().map(String::as_str), Some("200%"));
        assert!(labels.iter().any(|l| l == "100%") && labels.iter().any(|l| l == "110%"));
    }
}
