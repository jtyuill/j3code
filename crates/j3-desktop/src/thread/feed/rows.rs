//! The non-tool, non-assistant rows of the feed (spec §6.2): user messages, reasoning, the
//! user-input request card, notices, the compaction rule and subagent cards.

use j3_client::t3::{
    chat_attachment::ChatAttachment,
    orchestration::{
        SubagentItem, SubagentStatus, TurnItem, TurnItemStatus, UserInputRequestItem,
        UserMessageInputIntent,
    },
};
use rune::{Color, Easing, El, Icon, LoopSpec, Span, col, el, icon, memo, rich, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, Kit, Spinner};

use super::{Ctx, disclosure, tool::INDENT};
use crate::{
    app::J3, environment::ImageRef, feed, icons, images::ImageLoad, keywords, math::view::doc,
    mention, message_text, theme::Status,
};

/// A user message: a right-aligned bubble with its badge, attachments and text, mentions as
/// model chips and magic keywords in their gradients.
pub fn user_message(
    s: &J3,
    cx: &Ctx,
    id: &str,
    message: &str,
    attachments: &[ChatAttachment],
    intent: UserMessageInputIntent,
) -> El {
    let k = cx.k;
    let badge = match intent {
        UserMessageInputIntent::Steer => Some("Steer"),
        UserMessageInputIntent::PromotedQueuedToSteer => Some("Steer (from queue)"),
        UserMessageInputIntent::QueuedTurn => Some("Queued"),
        UserMessageInputIntent::TurnStart => None,
    };
    let mut bubble = col()
        .max_w(cx.width * 0.8)
        .shrink()
        .gap(4.0)
        .px(12.0)
        .py(8.0)
        .radius(k.radius.lg)
        .fill(k.c.layer_02);
    if let Some(badge) = badge {
        bubble = bubble.child(text(badge).font(k.ty.text_xsmall).color(k.c.text_muted));
    }
    if !attachments.is_empty() {
        let mut strip = row().flex_wrap().gap(8.0);
        for (ix, attachment) in attachments.iter().enumerate() {
            strip = strip.child(attachment_thumbnail(
                s,
                cx,
                &format!("{id}-{ix}"),
                attachment,
            ));
        }
        bubble = bubble.child(strip);
    }
    if !message.is_empty() {
        let models = s.envs.get(&cx.key.env).map(|env| env.models().clone());
        let rich_text = message_text::rich(message, |selector| {
            let choices = models.as_deref()?;
            mention::find_choice(choices, selector).map(|choice| choice.label.text().to_owned())
        });
        let info = Status::of(k).info;
        let mut spans = Vec::with_capacity(rich_text.spans.len());
        for span in &rich_text.spans {
            match span.kind {
                message_text::SpanKind::Keyword(keyword) => {
                    let chars = rich_text.text[span.range.clone()].char_indices();
                    for ((offset, ch), color) in chars.zip(keywords::colors(keyword, 0.0)) {
                        let start = span.range.start + offset;
                        let mut s = Span::new(start, start + ch.len_utf8());
                        s.color = Some(color);
                        spans.push(s);
                    }
                }
                message_text::SpanKind::Model => spans.push(
                    Span::new(span.range.start, span.range.end)
                        .bold()
                        .color(info)
                        .bg(k.alpha(info, 0.15), 2.0),
                ),
            }
        }
        bubble = bubble.child(
            rich(rich_text.text, spans)
                .font(k.ty.body)
                .color(k.c.text_base)
                .wrap()
                .selectable(true)
                .selection_color(k.selection()),
        );
    }
    row().w_full().justify_end().child(bubble)
}

/// An attachment as a 64 point frame: the picture, a spinner while it loads, or its name.
fn attachment_thumbnail(s: &J3, cx: &Ctx, id: &str, attachment: &ChatAttachment) -> El {
    let k = cx.k;
    let name = match attachment {
        ChatAttachment::Image(image) => image.name.clone(),
        ChatAttachment::File(file) => file.name.clone(),
        ChatAttachment::Unknown(other) => other.name.clone(),
    };
    let frame = el()
        .key(format!("attachment-{id}"))
        .label(name.clone())
        .size(64.0, 64.0)
        .center()
        .radius(k.radius.md)
        .clip_rounded(k.radius.md)
        .border(0.5, k.c.border_base);
    let load = match ImageRef::attachment(attachment) {
        Some(image) => s.image(&cx.key.env, &image),
        None => ImageLoad::Failed,
    };
    match load {
        ImageLoad::Loaded(image) => {
            if !image.is_ready() {
                cx.wait(&image);
            }
            frame.pixels_fit(image.pixels(), rune::SurfaceFit::Fill)
        }
        ImageLoad::Loading => frame.child(Spinner::new().size(12.0).build(k)),
        ImageLoad::Failed => frame.padding(4.0).child(
            text(name)
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .truncate(),
        ),
    }
}

/// Reasoning: a toggle row, "Thinking…" shimmering while it streams, else "Thought";
/// expanded, the reasoning as muted markdown under a rule.
pub fn reasoning(s: &J3, cx: &Ctx, item: &TurnItem, reasoning: &str, streaming: bool) -> El {
    let k = cx.k;
    let id = &item.id;
    let expanded = cx.expanded(id);
    let (key, toggled) = (cx.key.clone(), id.clone());
    let label = if streaming {
        let now = s.clock.t;
        memo(
            format!("thinking-{id}"),
            (k.dark, k.ty.text_xsmall.size.to_bits()),
            || shimmer(k, "Thinking…", now),
        )
        .key(format!("thinking-{id}"))
    } else {
        text("Thought").font(k.ty.text_xsmall).color(k.c.text_muted)
    };
    let toggle = row()
        .key(format!("reasoning-toggle-{id}"))
        .gap(8.0)
        .center_y()
        .cursor(rune::Cursor::Pointer)
        .on_press(move |s: &mut J3| s.toggle_feed_row(&key, &toggled))
        .child(icon(&icons::BRAIN, 16.0).color(k.c.text_muted))
        .child(label)
        .child(disclosure(k, expanded));
    let mut column = col().w_full().gap(4.0).child(toggle);
    if expanded && !reasoning.trim().is_empty() {
        let look = cx.look(k.c.text_muted, streaming);
        let doc = doc(s, &cx.key.env, &item.thread_id, reasoning, look);
        cx.wait_all(&doc.wait);
        column = column.child(
            el().w_full()
                .ml(INDENT)
                .pl(12.0)
                .border_sides([0.0, 0.0, 0.0, 2.0], k.c.border_base)
                .child(doc.view(s, &format!("md-{id}"), look)),
        );
    }
    column
}

/// `label` with a highlight sweeping through it: a window slides across the dim label while
/// the bright copy inside it slides back by as much, so the copy stays on the label. Both
/// loops start on the shared clock's period boundaries, so a rebuilt row keeps the sweep.
fn shimmer(k: &Kit, label: &str, now: f64) -> El {
    const BAND: f32 = 30.0;
    const SPEED: f32 = 90.0;
    let font = k.ty.text_xsmall;
    let width = label.chars().count() as f32 * font.size * 0.62;
    let period = f64::from((width + BAND) / SPEED);
    let delay = (-now / period).rem_euclid(1.0) * period;
    let spec = |from: f32, to: f32| LoopSpec {
        delay,
        ..LoopSpec::slide(period, [from, 0.0], [to, 0.0]).easing(Easing::LINEAR)
    };
    let (start, end) = (-BAND, width);
    let lit = el()
        .key("shimmer-lit")
        .absolute(0.0, 0.0)
        .w(BAND)
        .h_full()
        .looping(spec(-start / BAND, -end / BAND))
        .child(
            text(label.to_owned())
                .font(font)
                .color(k.c.text_base)
                .absolute(0.0, 0.0),
        );
    let sheen = el()
        .key("shimmer-sheen")
        .absolute(0.0, 0.0)
        .w(BAND)
        .h_full()
        .clip_fade(BAND / 2.0, 0.0, BAND / 2.0, 0.0)
        .looping(spec(start / BAND, end / BAND))
        .a11y_hidden()
        .child(lit);
    row()
        .child(text(label.to_owned()).font(font).color(k.c.text_muted))
        .child(sheen)
}

/// A question the agent asked: a bordered card (warning while it waits) with each question
/// and its answer.
pub fn question(cx: &Ctx, status: TurnItemStatus, request: &UserInputRequestItem) -> El {
    let k = cx.k;
    let tone = Status::of(k);
    let answers = request
        .question_answer
        .as_ref()
        .map(|answer| &answer.answers);
    let waiting = matches!(status, TurnItemStatus::Waiting | TurnItemStatus::Pending);
    let mut card = col()
        .w_full()
        .gap(4.0)
        .px(12.0)
        .py(8.0)
        .radius(k.radius.lg)
        .border(
            1.0,
            if waiting {
                tone.warning
            } else {
                k.c.border_base
            },
        );
    for question in &request.questions {
        let answer = answers
            .and_then(|answers| answers.get(&question.id))
            .map(feed::answer_text);
        let answer = match answer {
            Some(answer) if !answer.is_empty() => row()
                .gap(4.0)
                .center_y()
                .child(icon(&icons::CHECK, 12.0).color(tone.success))
                .child(
                    text(answer)
                        .font(k.ty.body_medium)
                        .color(k.c.text_base)
                        .wrap(),
                ),
            _ if waiting => text("Waiting for your answer")
                .font(k.ty.text_xsmall)
                .color(tone.warning),
            _ => text("No answer")
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted),
        };
        card = card.child(
            col()
                .w_full()
                .gap(2.0)
                .child(
                    text(question.header.clone())
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_muted),
                )
                .child(
                    text(question.question.clone())
                        .font(k.ty.body)
                        .color(k.c.text_base)
                        .wrap(),
                )
                .child(answer),
        );
    }
    card
}

/// An icon and a message (todo updates, errors, notices, interrupts).
pub fn notice(k: &Kit, glyph: &'static Icon, color: Color, message: String) -> El {
    row()
        .w_full()
        .gap(8.0)
        .items_start()
        .child(el().pt(2.0).child(icon(glyph, 16.0).color(color)))
        .child(
            text(message)
                .font(k.ty.body)
                .color(k.c.text_base)
                .wrap()
                .grow(1.0)
                .shrink(),
        )
}

/// A hairline rule with the compaction label in its middle.
pub fn compaction(k: &Kit, label: String) -> El {
    let line = || el().grow(1.0).h(0.5).fill(k.c.border_base);
    row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(line())
        .child(text(label).font(k.ty.text_xsmall).color(k.c.text_muted))
        .child(line())
}

/// A subagent the agent started: title, model, status, live progress and, expanded, its
/// assignment and result; Stop while it runs.
pub fn subagent(cx: &Ctx, item: &TurnItem, subagent: &SubagentItem) -> El {
    let k = cx.k;
    let tone = Status::of(k);
    let id = &item.id;
    let entity = cx.state.subagent(&subagent.subagent_id);
    let status = entity.map(|s| s.status);
    let active = status.is_some_and(SubagentStatus::is_active);
    let title = entity
        .and_then(|s| s.title.clone())
        .or_else(|| item.title.clone())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "Subagent".to_owned());
    let model = entity.and_then(|s| s.model.clone());
    let progress = entity
        .and_then(|s| s.progress.clone())
        .or_else(|| subagent.progress.clone());
    let result = entity
        .and_then(|s| s.result.clone())
        .or_else(|| subagent.result.clone());
    let failed = matches!(status, Some(SubagentStatus::Failed));
    let expanded = cx.expanded(id);
    let summary = result.as_deref().map(feed::one_line);
    let progress = progress.filter(|_| active || summary.is_none());
    let mut label = format!(
        "{title}, {}",
        status.map_or("Unknown", feed::subagent_status_label)
    );
    if let Some(progress) = &progress {
        label.push_str(&format!(", {progress}"));
    }
    if let Some(summary) = &summary {
        label.push_str(&format!(", {summary}"));
    }
    let (key, toggled) = (cx.key.clone(), id.clone());
    let mut toggle = row()
        .key(format!("subagent-toggle-{id}"))
        .grow(1.0)
        .min_w(0.0)
        .gap(8.0)
        .center_y()
        .cursor(rune::Cursor::Pointer)
        .on_press(move |s: &mut J3| s.toggle_feed_row(&key, &toggled))
        .child(icon(&icons::BOT, 16.0).color(k.c.text_muted))
        .child(
            text(title)
                .font(k.ty.body_medium)
                .color(k.c.text_base)
                .truncate()
                .grow(1.0),
        );
    if let Some(model) = model {
        toggle = toggle.child(
            text(model)
                .font(rune::TextStyle {
                    size: k.ty.text_xsmall.size,
                    line_height: k.ty.text_xsmall.line_height,
                    ..k.ty.mono
                })
                .color(k.c.text_muted),
        );
    }
    if active {
        toggle = toggle.child(Spinner::new().size(12.0).build(k));
    }
    if let Some(status) = status {
        toggle = toggle.child(
            text(feed::subagent_status_label(status))
                .font(k.ty.text_xsmall)
                .color(if failed { tone.danger } else { k.c.text_muted }),
        );
    }
    let mut header = row().w_full().gap(8.0).center_y().child(toggle);
    if let Some((run_id, subagent_id)) = entity
        .filter(|_| active)
        .and_then(|s| Some((s.run_id.clone()?, s.id.clone())))
    {
        let key = cx.key.clone();
        header = header.child(
            Button::new("Stop")
                .icon(&icons::SQUARE)
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Mini)
                .on_press(move |s: &mut J3| s.stop_subagent(&key, &run_id, &subagent_id))
                .build(k)
                .key(format!("stop-subagent-{}", subagent.subagent_id)),
        );
    }
    header = header.child(disclosure(k, expanded));
    let mut card = col()
        .key(format!("subagent-{}", subagent.subagent_id))
        .label(label)
        .w_full()
        .gap(4.0)
        .px(12.0)
        .py(8.0)
        .radius(k.radius.lg)
        .border(1.0, if failed { tone.danger } else { k.c.border_base })
        .child(header);
    let small = |t: String, color: Color| text(t).font(k.ty.text_xsmall).color(color);
    if let Some(progress) = progress {
        card = card.child(
            el().w_full()
                .pl(24.0)
                .child(small(progress, k.c.text_muted).truncate()),
        );
    }
    if expanded {
        let mut detail = col()
            .w_full()
            .pl(24.0)
            .gap(4.0)
            .child(small("Assignment".into(), k.c.text_muted))
            .child(small(subagent.prompt.clone(), k.c.text_base).wrap());
        if let Some(result) = result {
            detail = detail
                .child(el().pt(4.0).child(small("Result".into(), k.c.text_muted)))
                .child(small(result, k.c.text_base).wrap());
        }
        card = card.child(detail);
    } else if let Some(summary) = summary {
        card = card.child(
            el().w_full()
                .pl(24.0)
                .child(small(summary, k.c.text_base).truncate()),
        );
    }
    card
}
