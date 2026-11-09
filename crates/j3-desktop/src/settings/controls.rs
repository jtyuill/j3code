//! The building blocks Settings and the dialogs lay their forms out with: titled groups of
//! setting rows (title and description beside a control), segmented choices whose segments
//! carry the old button ids (`workflow-inbox`, `snooze-unit-hours`, …), and number steppers.

use rune::{El, Shadow, TextStyle, Tx, col, el, row, text};
use rune_kit::{ControlSize, IconButton, IconButtonVariant, Kit};

use crate::{app::J3, icons};

/// The id of a segment labelled `label` in segmented control `id`: `{id}-{label}`, the label
/// lowercased with spaces as dashes (`agent-order-by-space`).
pub fn segment_key(id: &str, label: &str) -> String {
    format!("{id}-{}", label.to_lowercase().replace(' ', "-"))
}

/// A segmented choice: one segment per `(key, label)`, the `selected` one raised. Picking a
/// segment runs `pick` with its index.
pub fn segmented(
    k: &Kit,
    key: &str,
    label: &str,
    segments: &[(String, String)],
    selected: Option<usize>,
    pick: impl Fn(&mut J3, usize, &mut Tx) + Clone + 'static,
) -> El {
    let c = &k.c;
    let font = TextStyle {
        line_height: 13.0,
        ..k.ty.ui
    };
    let mut group = row()
        .key(key)
        .role("radiogroup")
        .label(label)
        .focus_group()
        .h(28.0)
        .gap(2.0)
        .padding(0.0)
        .center_y()
        .radius(6.0)
        .fill(c.layer_01)
        .box_shadow(Shadow {
            spread: 0.5,
            color: c.border_base,
            ..Default::default()
        });
    let n = segments.len();
    for (ix, (segment_key, label)) in segments.iter().enumerate() {
        let on = selected == Some(ix);
        let pick = pick.clone();
        let mut segment = row()
            .key(segment_key.clone())
            .role("radio")
            .label(label.clone())
            .position_in_set(ix + 1, n)
            .focusable(true)
            .focus_selected(on)
            .focus_ring(2.0, 1.0, c.border_focus)
            .h(28.0)
            .px(12.0)
            .center()
            .radius(6.0)
            .child(
                text(label.clone())
                    .font(font)
                    .color(if on { c.text_base } else { c.text_muted })
                    .a11y_hidden(),
            )
            .on_press_tx(move |s: &mut J3, tx: &mut Tx| pick(s, ix, tx));
        segment = if on {
            segment.fill(c.bg_base).box_shadow(Shadow {
                spread: 0.5,
                color: c.border_strong,
                ..Default::default()
            })
        } else {
            segment.hover(c.hover)
        };
        group = group.child(segment);
    }
    group
}

/// [`segmented`] over static labels keyed by [`segment_key`].
pub fn choice(
    k: &Kit,
    id: &str,
    labels: &[&str],
    selected: Option<usize>,
    pick: impl Fn(&mut J3, usize, &mut Tx) + Clone + 'static,
) -> El {
    let segments: Vec<(String, String)> = labels
        .iter()
        .map(|label| (segment_key(id, label), (*label).to_owned()))
        .collect();
    segmented(k, id, id, &segments, selected, pick)
}

/// A number stepper: − value + within `min..=max` by `step`; `set` gets the new value.
/// Keys: `{id}-decrement`, `{id}-value`, `{id}-increment`.
pub fn stepper(
    k: &Kit,
    id: &str,
    value: f32,
    (min, max, step): (f32, f32, f32),
    set: impl Fn(&mut J3, f32, &mut Tx) + Clone + 'static,
) -> El {
    let down = (value - step).max(min);
    let up = (value + step).min(max);
    let button = |glyph, label: &str, to: f32, enabled: bool, key: String| {
        let set = set.clone();
        IconButton::new(glyph)
            .variant(IconButtonVariant::Ghost)
            .control_size(ControlSize::Small)
            .label(label)
            .disabled(!enabled)
            .on_press_tx(move |s: &mut J3, tx: &mut Tx| set(s, to, tx))
            .build(k)
            .key(key)
    };
    row()
        .key(id)
        .role("group")
        .gap(2.0)
        .center_y()
        .padding(2.0)
        .radius(6.0)
        .border(1.0, k.c.border_base)
        .child(button(
            &icons::MINUS,
            "Decrease",
            down,
            value > min,
            format!("{id}-decrement"),
        ))
        .child(
            el().key(format!("{id}-value")).w(32.0).center().child(
                text(format_number(value))
                    .font(k.ty.ui)
                    .color(k.c.text_base),
            ),
        )
        .child(button(
            &icons::PLUS,
            "Increase",
            up,
            value < max,
            format!("{id}-increment"),
        ))
}

/// `16` for whole numbers, `16.5` otherwise.
fn format_number(value: f32) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

/// One setting: its title and description on the left, the control on the right.
pub fn item(k: &Kit, key: &str, title: &str, description: Option<&str>, control: El) -> El {
    let mut words = col().grow(1.0).shrink().min_w(0.0).gap(2.0).child(
        text(title)
            .font(k.ty.text_small_medium)
            .color(k.c.text_base),
    );
    if let Some(description) = description {
        words = words.child(
            text(description)
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .wrap(),
        );
    }
    row()
        .key(format!("setting-{key}"))
        .w_full()
        .gap(16.0)
        .px(16.0)
        .py(12.0)
        .center_y()
        .child(words)
        .child(control)
}

/// A settings group: an optional title and description over a card of rows (divided by
/// hairlines), then an optional footer (an "Add …" button).
pub struct Group {
    key: String,
    title: Option<String>,
    description: Option<String>,
    rows: Vec<El>,
    footer: Option<El>,
}

impl Group {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            title: None,
            description: None,
            rows: Vec::new(),
            footer: None,
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn row(mut self, row: El) -> Self {
        self.rows.push(row);
        self
    }

    pub fn footer(mut self, footer: El) -> Self {
        self.footer = Some(footer);
        self
    }

    pub fn build(self, k: &Kit) -> El {
        let mut group = col().key(self.key).w_full().gap(8.0);
        if let Some(title) = self.title {
            group = group.child(
                text(title)
                    .font(k.ty.text_small_medium)
                    .color(k.c.text_muted)
                    .px(4.0),
            );
        }
        if let Some(description) = self.description {
            group = group.child(
                text(description)
                    .font(k.ty.text_xsmall)
                    .color(k.c.text_muted)
                    .wrap()
                    .px(4.0),
            );
        }
        if !self.rows.is_empty() {
            let mut card = col()
                .w_full()
                .radius(8.0)
                .fill(k.c.bg_base)
                .border(1.0, k.c.border_muted);
            let n = self.rows.len();
            for (ix, row) in self.rows.into_iter().enumerate() {
                card = card.child(row);
                if ix + 1 < n {
                    card = card.child(el().w_full().h(1.0).fill(k.c.border_muted).a11y_hidden());
                }
            }
            group = group.child(card);
        }
        if let Some(footer) = self.footer {
            group = group.child(row().w_full().child(footer));
        }
        group
    }
}

/// A muted line standing in for an empty list ("No projects").
pub fn placeholder(k: &Kit, line: &str) -> El {
    row()
        .w_full()
        .px(16.0)
        .py(12.0)
        .child(text(line).font(k.ty.text_small).color(k.c.text_muted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_keys_follow_the_old_button_ids() {
        assert_eq!(segment_key("workflow", "Inbox"), "workflow-inbox");
        assert_eq!(
            segment_key("agent-order", "By space"),
            "agent-order-by-space"
        );
        assert_eq!(format_number(16.0), "16");
        assert_eq!(format_number(1.5), "1.5");
    }
}
