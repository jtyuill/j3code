//! Rendering of one feed row (a visible turn item). Pure presentation: the
//! thread view owns the retained state passed in [`RowContext`].

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
    h_flex,
    spinner::Spinner,
    text::{TextView, TextViewState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Entity, ImageSource, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, WeakEntity, Window, div, img, relative,
};
use j3_client::t3::{
    chat_attachment::ChatAttachment,
    orchestration::{
        PlanStepStatus, TurnItem, TurnItemKind, TurnItemStatus, UserInputRequestItem,
        UserMessageInputIntent,
    },
};
use serde_json::Value;

use crate::{
    environment::{Environment, ImageLoad},
    feed::{ToolIcon, tool_summary},
    thread_view::ThreadView,
};

/// Longest tool detail shown, in lines; the rest is summarized.
const MAX_DETAIL_LINES: usize = 400;

/// Retained state a row reads while rendering.
pub struct RowContext {
    pub markdown: Rc<HashMap<String, Entity<TextViewState>>>,
    pub expanded: Rc<HashSet<String>>,
    pub view: WeakEntity<ThreadView>,
    pub environment: Entity<Environment>,
}

pub fn render_item(item: &TurnItem, row: &RowContext, _: &mut Window, cx: &mut App) -> AnyElement {
    let id = item.id.clone();
    let content = match &item.kind {
        TurnItemKind::UserMessage(message) => user_message(
            &id,
            &message.text,
            &message.attachments,
            message.input_intent,
            row,
            cx,
        ),
        TurnItemKind::AssistantMessage(message) => match row.markdown.get(&id) {
            Some(state) => div()
                .w_full()
                .text_sm()
                .child(
                    TextView::new(state)
                        .selectable(true)
                        .stream_fade(message.streaming),
                )
                .into_any_element(),
            None => div().text_sm().child(message.text.clone()).into_any_element(),
        },
        TurnItemKind::Reasoning(reasoning) => {
            reasoning_row(&id, reasoning.streaming, row, cx)
        }
        TurnItemKind::UserInputRequest(request) => question_row(item.status, request, cx),
        TurnItemKind::TodoList(todo) => {
            let done = todo
                .steps
                .iter()
                .filter(|step| step.status == PlanStepStatus::Completed)
                .count();
            notice_row(
                IconName::CircleCheck,
                cx.theme().muted_foreground,
                format!("Updated the plan · {done}/{} done", todo.steps.len()),
                cx,
            )
        }
        TurnItemKind::Error(error) => {
            let mut message = error.failure.message.clone();
            if let Some(retry) = &error.retry {
                match retry.max_attempts {
                    Some(max) => message.push_str(&format!(" (retry {} of {max})", retry.attempt)),
                    None => message.push_str(&format!(" (retry {})", retry.attempt)),
                }
            }
            notice_row(IconName::CircleAlert, cx.theme().danger, message, cx)
        }
        TurnItemKind::SystemNotice(notice) => {
            notice_row(IconName::Info, cx.theme().info, notice.message.clone(), cx)
        }
        TurnItemKind::RunInterruptResult(result) => notice_row(
            IconName::Square,
            cx.theme().muted_foreground,
            result.message.clone(),
            cx,
        ),
        TurnItemKind::Compaction(compaction) => {
            let tokens = match (compaction.before_token_count, compaction.after_token_count) {
                (Some(before), Some(after)) => format!(" · {before} → {after} tokens"),
                _ => String::new(),
            };
            let theme = cx.theme();
            h_flex()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().flex_1().h_px().bg(theme.border))
                .child(format!("Context compacted{tokens}"))
                .child(div().flex_1().h_px().bg(theme.border))
                .into_any_element()
        }
        TurnItemKind::CommandExecution(_)
        | TurnItemKind::FileChange(_)
        | TurnItemKind::FileSearch(_)
        | TurnItemKind::WebSearch(_)
        | TurnItemKind::DynamicTool(_) => tool_row(item, row, cx),
        // Stop requests, unknown kinds and kinds added later render nothing
        // (`feed::is_hidden` also drops them before rendering).
        _ => {
            div().into_any_element()
        }
    };
    div()
        .id(SharedString::from(format!("feed-{id}")))
        .w_full()
        .px_4()
        .pb_3()
        .child(content)
        .into_any_element()
}

fn user_message(
    id: &str,
    text: &str,
    attachments: &[ChatAttachment],
    intent: UserMessageInputIntent,
    row: &RowContext,
    cx: &mut App,
) -> AnyElement {
    let badge = match intent {
        UserMessageInputIntent::Steer => Some("Steer"),
        UserMessageInputIntent::PromotedQueuedToSteer => Some("Steer (from queue)"),
        UserMessageInputIntent::QueuedTurn => Some("Queued"),
        UserMessageInputIntent::TurnStart => None,
    };
    let images: Vec<AnyElement> = attachments
        .iter()
        .enumerate()
        .map(|(ix, attachment)| {
            let load = row
                .environment
                .update(cx, |environment, cx| environment.image(attachment, cx));
            attachment_thumbnail(&format!("{id}-{ix}"), attachment, load, cx)
        })
        .collect();
    let theme = cx.theme();
    h_flex()
        .w_full()
        .justify_end()
        .child(
            v_flex()
                .max_w(relative(0.8))
                .gap_1()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .bg(theme.secondary)
                .text_color(theme.secondary_foreground)
                .text_sm()
                .when_some(badge, |this, badge| {
                    this.child(div().text_xs().text_color(theme.muted_foreground).child(badge))
                })
                .when(!images.is_empty(), |this| {
                    this.child(h_flex().flex_wrap().gap_2().children(images))
                })
                .when(!text.is_empty(), |this| {
                    this.child(div().whitespace_normal().child(text.to_owned()))
                }),
        )
        .into_any_element()
}

fn attachment_thumbnail(id: &str, attachment: &ChatAttachment, load: ImageLoad, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let name = match attachment {
        ChatAttachment::Image(image) => image.name.clone(),
        ChatAttachment::File(file) => file.name.clone(),
        ChatAttachment::Unknown(other) => other.name.clone(),
    };
    let frame = div()
        .id(SharedString::from(format!("attachment-{id}")))
        .size_16()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .overflow_hidden();
    match load {
        ImageLoad::Loaded(image) => frame
            .child(
                img(ImageSource::Image(image))
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            )
            .into_any_element(),
        ImageLoad::Loading => frame.child(Spinner::new().xsmall()).into_any_element(),
        ImageLoad::Failed => frame
            .p_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().truncate().child(name))
            .into_any_element(),
    }
}

fn toggle(view: &WeakEntity<ThreadView>, id: String) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let view = view.clone();
    move |_, _, cx| {
        let _ = view.update(cx, |view, cx| view.toggle_expanded(&id, cx));
    }
}

fn disclosure(expanded: bool, cx: &App) -> Icon {
    Icon::new(if expanded {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    })
    .xsmall()
    .text_color(cx.theme().muted_foreground)
}

fn reasoning_row(id: &str, streaming: bool, row: &RowContext, cx: &App) -> AnyElement {
    let expanded = row.expanded.contains(id);
    let theme = cx.theme();
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .id(SharedString::from(format!("reasoning-toggle-{id}")))
                .gap_1()
                .text_xs()
                .text_color(theme.muted_foreground)
                .cursor_default()
                .hover(|this| this.text_color(theme.foreground))
                .child(disclosure(expanded, cx))
                .child(if streaming { "Thinking…" } else { "Thought" })
                .when(streaming, |this| this.child(Spinner::new().xsmall()))
                .on_click(toggle(&row.view, id.to_owned())),
        )
        .when(expanded, |this| {
            this.when_some(row.markdown.get(id), |this, state| {
                this.child(
                    div()
                        .pl_4()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(TextView::new(state).selectable(true)),
                )
            })
        })
        .into_any_element()
}

fn tool_row(item: &TurnItem, row: &RowContext, cx: &App) -> AnyElement {
    let Some(summary) = tool_summary(item) else {
        return div().into_any_element();
    };
    let id = item.id.clone();
    let expanded = row.expanded.contains(&id);
    let theme = cx.theme();
    let icon = match summary.icon {
        ToolIcon::Terminal => IconName::SquareTerminal,
        ToolIcon::File => IconName::FileText,
        ToolIcon::Search => IconName::Search,
        ToolIcon::Web => IconName::Globe,
        ToolIcon::Tool => IconName::Settings2,
    };
    let has_detail = summary.detail.is_some();
    let detail = summary.detail.filter(|_| expanded).map(|detail| {
        let lines: Vec<&str> = detail.lines().collect();
        let (shown, more) = if lines.len() > MAX_DETAIL_LINES {
            (
                lines[lines.len() - MAX_DETAIL_LINES..].join("\n"),
                Some(lines.len() - MAX_DETAIL_LINES),
            )
        } else {
            (detail.clone(), None)
        };
        v_flex()
            .ml_5()
            .mt_1()
            .p_2()
            .rounded(theme.radius)
            .bg(theme.muted)
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .when_some(more, |this, more| {
                this.child(
                    div()
                        .text_color(theme.muted_foreground)
                        .child(format!("… {more} earlier lines")),
                )
            })
            .child(div().whitespace_normal().child(shown))
    });
    v_flex()
        .child(
            h_flex()
                .id(SharedString::from(format!("tool-toggle-{id}")))
                .gap_2()
                .text_sm()
                .rounded(theme.radius)
                .when(has_detail, |this| {
                    this.hover(|this| this.bg(theme.muted.opacity(0.5)))
                        .on_click(toggle(&row.view, id.clone()))
                })
                .child(
                    Icon::new(icon)
                        .small()
                        .text_color(if summary.failed {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        }),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .font_medium()
                        .font_family(theme.mono_font_family.clone())
                        .text_xs()
                        .child(summary.title),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .font_family(theme.mono_font_family.clone())
                        .child(summary.subtitle),
                )
                .when(summary.running, |this| this.child(Spinner::new().xsmall()))
                .when(summary.failed, |this| {
                    this.child(div().text_xs().text_color(theme.danger).child("failed"))
                })
                .when(has_detail, |this| this.child(disclosure(expanded, cx))),
        )
        .children(detail)
        .into_any_element()
}

fn answer_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(answer_text)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn question_row(status: TurnItemStatus, request: &UserInputRequestItem, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let answers = request
        .question_answer
        .as_ref()
        .map(|answer| &answer.answers);
    let waiting = matches!(status, TurnItemStatus::Waiting | TurnItemStatus::Pending);
    v_flex()
        .gap_1()
        .px_3()
        .py_2()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(if waiting { theme.warning } else { theme.border })
        .text_sm()
        .children(request.questions.iter().map(|question| {
            let answer = answers
                .and_then(|answers| answers.get(&question.id))
                .map(answer_text);
            v_flex()
                .gap_0p5()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(question.header.clone()),
                )
                .child(div().child(question.question.clone()))
                .child(match answer {
                    Some(answer) if !answer.is_empty() => h_flex()
                        .gap_1()
                        .child(Icon::new(IconName::Check).xsmall().text_color(theme.success))
                        .child(div().font_medium().child(answer))
                        .into_any_element(),
                    _ if waiting => div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child("Waiting for your answer")
                        .into_any_element(),
                    _ => div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("No answer")
                        .into_any_element(),
                })
        }))
        .into_any_element()
}

fn notice_row(icon: IconName, color: gpui_kit::Hsla, message: String, cx: &App) -> AnyElement {
    h_flex()
        .gap_2()
        .items_start()
        .text_sm()
        .child(div().pt_0p5().child(Icon::new(icon).small().text_color(color)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(cx.theme().foreground)
                .child(message),
        )
        .into_any_element()
}
