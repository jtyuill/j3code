//! The question card's view: the header (Bell, "omp is asking", Dismiss) and the current
//! question with its progress, choices, field, error and actions.

use rune::{El, Tx, col, icon, row, spacer, text};
use rune_kit::{
    Button, ButtonVariant, Checkbox, ControlSize, Kit, RadioGroup, RadioItem, TextInput,
};

use super::{Item, MISSING, Nav, QuestionCard, Questionnaire};
use crate::{
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
    theme::Status,
    thread::chrome_edit,
};

/// `key`'s question card, when omp is asking.
pub fn card(ui: &Ui, key: &ThreadKey, card: &QuestionCard) -> El {
    let k = ui.k;
    let warning = Status::of(k).warning;
    let dismiss_key = key.clone();
    let header = row()
        .key("question-header")
        .w_full()
        .gap(8.0)
        .center_y()
        .child(icon(&icons::BELL, 12.0).color(warning))
        .child(
            text("omp is asking")
                .font(k.ty.text_xsmall_medium)
                .color(warning),
        )
        .child(spacer())
        .child(
            Button::new("Dismiss")
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut J3| s.dismiss_question(&dismiss_key))
                .build(k)
                .key("dismiss-question"),
        );
    let body = match &card.form {
        Ok(form) => questionnaire(k, key, form),
        Err(problem) => text(format!("This question can’t be shown: {problem}"))
            .key("question-error")
            .font(k.ty.text_small)
            .color(Status::of(k).danger)
            .wrap(),
    };
    col()
        .key("question-card")
        .w_full()
        .gap(8.0)
        .px(12.0)
        .py(8.0)
        .radius(k.radius.lg)
        .border(1.0, warning)
        .child(header)
        .child(body)
}

/// The current question: progress, title, description, choices, field, error, actions.
fn questionnaire(k: &Kit, key: &ThreadKey, form: &Questionnaire) -> El {
    let ix = form.current();
    let item = &form.items[ix];
    let mut body = col().key("questionnaire").w_full().gap(8.0);
    if form.items.len() > 1 {
        body = body.child(
            text(format!("Question {} of {}", ix + 1, form.items.len()))
                .key("question-progress")
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted),
        );
    }
    body = body.child(
        text(&item.title)
            .key("question-title")
            .font(k.ty.text_small_medium)
            .color(k.c.text_base)
            .wrap(),
    );
    if !item.description.trim().is_empty() {
        body = body.child(
            text(&item.description)
                .key("question-description")
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    if !item.choices.is_empty() {
        body = body.child(choices(k, key, ix, item));
    }
    if let Some(input) = &item.input {
        let (edit_key, change_key, submit_key) = (key.clone(), key.clone(), key.clone());
        body = body.child(
            TextInput::new(input)
                .width(None)
                .placeholder("Type an answer…")
                .label("Custom answer")
                .invalid(item.missing)
                .on_edit(move |s: &mut J3| {
                    chrome_edit(s, &edit_key, |chrome| {
                        chrome
                            .question
                            .as_mut()?
                            .form
                            .as_mut()
                            .ok()?
                            .items
                            .get_mut(ix)?
                            .input
                            .as_mut()
                    })
                })
                .on_change(move |s: &mut J3, _: &mut Tx| s.answer_typed(&change_key, ix))
                .on_submit(move |s: &mut J3, _: &mut Tx| {
                    s.navigate_question(&submit_key, Nav::Confirm)
                })
                .build(k)
                .key(format!("question-input-{}", item.id)),
        );
    }
    if item.missing {
        body = body.child(
            text(MISSING)
                .key("question-problem")
                .font(k.ty.text_xsmall)
                .color(Status::of(k).danger),
        );
    }
    body.child(actions(k, key, form))
}

fn choices(k: &Kit, key: &ThreadKey, ix: usize, item: &Item) -> El {
    if item.multiple {
        let mut list = col().key("question-choices").gap(6.0);
        for choice in &item.choices {
            let (key, value) = (key.clone(), choice.value.clone());
            let mut checkbox = Checkbox::new(item.is_chosen(&choice.value)).label(&choice.label);
            if let Some(description) = &choice.description {
                checkbox = checkbox.description(description);
            }
            list = list.child(
                checkbox
                    .on_toggle_tx(move |s: &mut J3, tx: &mut Tx| {
                        s.choose_answer(&key, ix, &value, tx)
                    })
                    .build(k)
                    .key(format!("question-choice-{}-{}", item.id, choice.value)),
            );
        }
        return list;
    }
    // The title above names the group; RadioGroup's own label would repeat it.
    let mut group = RadioGroup::new().invalid(item.missing);
    for choice in &item.choices {
        let (key, value) = (key.clone(), choice.value.clone());
        let mut radio = RadioItem::new(&choice.label, item.is_chosen(&choice.value));
        if let Some(description) = &choice.description {
            radio = radio.description(description);
        }
        group = group
            .item(radio.on_select_tx(move |s: &mut J3, tx: &mut Tx| {
                s.choose_answer(&key, ix, &value, tx)
            }));
    }
    group.build(k).key("question-choices")
}

/// Previous, then Next or Submit at the trailing end.
fn actions(k: &Kit, key: &ThreadKey, form: &Questionnaire) -> El {
    let button = |label: &str, id: &str, variant: ButtonVariant, nav: Nav| {
        let key = key.clone();
        Button::new(label)
            .variant(variant)
            .control_size(ControlSize::Small)
            .on_press(move |s: &mut J3| s.navigate_question(&key, nav))
            .build(k)
            .key(id.to_owned())
    };
    let mut bar = row().key("question-actions").w_full().gap(8.0).center_y();
    if form.current() > 0 {
        bar = bar.child(button(
            "Previous",
            "question-previous",
            ButtonVariant::Outline,
            Nav::Previous,
        ));
    }
    bar = bar.child(spacer());
    if form.is_last() {
        bar.child(button(
            "Submit",
            "question-submit",
            ButtonVariant::Contrast,
            Nav::Submit,
        ))
    } else {
        bar.child(button(
            "Next",
            "question-next",
            ButtonVariant::Contrast,
            Nav::Next,
        ))
    }
}
