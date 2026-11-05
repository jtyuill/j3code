//! Rendering of one feed row (a visible turn item). Pure presentation: the
//! thread view owns the retained state passed in [`RowContext`].

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    shimmer::ShimmerText,
    spinner::Spinner,
    text::{TextView, TextViewState},
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Entity, FontWeight, HighlightStyle, ImageCacheError, ImageSource,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, Pixels, SharedString,
    SharedUri, StatefulInteractiveElement as _, Styled as _, StyledImage as _, StyledText,
    TestSupportExt as _, WeakEntity, Window, div, img, px, relative,
};
use j3_client::{
    ThreadState,
    t3::{
        chat_attachment::ChatAttachment,
        orchestration::{
            PlanStepStatus, SubagentItem, SubagentStatus, TurnItem, TurnItemKind, TurnItemStatus,
            UserInputRequestItem, UserMessageInputIntent,
        },
    },
};
use serde_json::Value;

use crate::{
    environment::{Environment, ImageLoad, ImageRef},
    feed::{DiffKind, Tone, ToolBody, ToolIcon, ToolStatus, tool_view},
    keywords, math, mention, message_text,
    thread_view::ThreadView,
};

/// Tallest a tool's output image is drawn.
const MAX_OUTPUT_IMAGE_HEIGHT: Pixels = px(360.);

/// Longest tool detail shown, in lines; the rest is summarized.
const MAX_DETAIL_LINES: usize = 400;

/// Retained state a row reads while rendering.
pub struct RowContext {
    pub markdown: Rc<HashMap<String, Entity<TextViewState>>>,
    pub expanded: Rc<HashSet<String>>,
    pub view: WeakEntity<ThreadView>,
    pub environment: Entity<Environment>,
    /// The thread the rows belong to, for entities items refer to (subagents).
    pub state: Arc<ThreadState>,
    /// Typeset TeX math in markdown (the `math_rendering` preference).
    pub math: bool,
}

/// Rows of agent activity (tool calls, thinking, plan updates) that read as
/// one list when they follow each other.
fn is_activity(item: &TurnItem) -> bool {
    matches!(
        item.kind,
        TurnItemKind::CommandExecution(_)
            | TurnItemKind::FileChange(_)
            | TurnItemKind::FileSearch(_)
            | TurnItemKind::WebSearch(_)
            | TurnItemKind::DynamicTool(_)
            | TurnItemKind::Reasoning(_)
            | TurnItemKind::TodoList(_)
    )
}

/// `previous` is the row shown above, which sets the space between them.
/// The space sits above a row, not below it, so appending a row never
/// changes the height of the one before.
pub fn render_item(
    item: &TurnItem,
    previous: Option<&TurnItem>,
    row: &RowContext,
    _: &mut Window,
    cx: &mut App,
) -> AnyElement {
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
                .child(math::text_view(
                    TextView::new(state)
                        .selectable(true)
                        .stream_fade(message.streaming)
                        .image_source(markdown_images(&row.environment, &item.thread_id)),
                    row.math,
                ))
                .into_any_element(),
            None => div()
                .text_sm()
                .child(message.text.clone())
                .into_any_element(),
        },
        TurnItemKind::Reasoning(reasoning) => reasoning_row(&id, reasoning.streaming, row, cx),
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
        TurnItemKind::Subagent(subagent) => subagent_row(item, subagent, row, cx),
        // Stop requests, unknown kinds and kinds added later render nothing
        // (`feed::is_hidden` also drops them before rendering).
        _ => div().into_any_element(),
    };
    // Messages are named by their text so the row is identifiable to
    // accessibility clients (and UI tests) like the text on screen.
    let label = match &item.kind {
        TurnItemKind::UserMessage(message) => Some(message.text.clone()),
        TurnItemKind::AssistantMessage(message) => Some(message.text.clone()),
        _ => None,
    };
    div()
        .id(SharedString::from(format!("feed-{id}")))
        .test_support()
        .when_some(label, |this, label| this.aria_label(label))
        .w_full()
        .px_4()
        .map(|this| match previous {
            None => this,
            Some(previous) if is_activity(previous) && is_activity(item) => this.pt_1(),
            // The 44px messages always had between them.
            Some(_) => this.pt_11(),
        })
        .child(content)
        .into_any_element()
}

pub(crate) fn subagent_status_label(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Idle => "Idle",
        SubagentStatus::Pending => "Starting",
        SubagentStatus::Running => "Running",
        SubagentStatus::Waiting => "Waiting",
        SubagentStatus::Completed => "Done",
        SubagentStatus::Failed => "Failed",
        SubagentStatus::Cancelled => "Stopped",
        SubagentStatus::Interrupted => "Interrupted",
    }
}

/// A subagent the agent started: title, model, status, live progress, and
/// (expanded) its assignment and final answer; Stop while it runs.
fn subagent_row(
    item: &TurnItem,
    subagent: &SubagentItem,
    row: &RowContext,
    cx: &App,
) -> AnyElement {
    let id = item.id.clone();
    let entity = row.state.subagent(&subagent.subagent_id);
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
    let expanded = row.expanded.contains(&id);
    let theme = cx.theme();
    let stop = entity
        .filter(|_| active)
        .and_then(|s| Some((s.run_id.clone()?, s.id.clone())))
        .map(|(run_id, subagent_id)| {
            let view = row.view.clone();
            Button::new(SharedString::from(format!("stop-subagent-{subagent_id}")))
                .xsmall()
                .ghost()
                .icon(IconName::Square)
                .label("Stop")
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    let (run_id, subagent_id) = (run_id.clone(), subagent_id.clone());
                    let _ = view.update(cx, |view, cx| {
                        view.stop_subagent(run_id, subagent_id, window, cx)
                    });
                })
        });
    let mono = theme.mono_font_family.clone();
    let summary = result.as_deref().map(crate::feed::one_line);
    let mut label = format!(
        "{title}, {}",
        status.map_or("Unknown", subagent_status_label)
    );
    if let Some(progress) = progress.as_deref().filter(|_| active || summary.is_none()) {
        label.push_str(&format!(", {progress}"));
    }
    if let Some(summary) = &summary {
        label.push_str(&format!(", {summary}"));
    }
    v_flex()
        .id(SharedString::from(format!(
            "subagent-{}",
            subagent.subagent_id
        )))
        .test_support()
        .aria_label(label)
        .gap_1()
        .px_3()
        .py_2()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(if failed { theme.danger } else { theme.border })
        .child(
            h_flex()
                .id(SharedString::from(format!("subagent-toggle-{id}")))
                .gap_2()
                .text_sm()
                .on_click(toggle(&row.view, id.clone()))
                .child(
                    Icon::new(IconName::Bot)
                        .small()
                        .text_color(theme.muted_foreground),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .font_medium()
                        .child(title),
                )
                .when_some(model, |this, model| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .font_family(mono.clone())
                            .text_color(theme.muted_foreground)
                            .child(model),
                    )
                })
                .when(active, |this| this.child(Spinner::new().xsmall()))
                .when_some(status, |this, status| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(if failed {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            })
                            .child(subagent_status_label(status)),
                    )
                })
                .children(stop)
                .child(disclosure(expanded, cx)),
        )
        .when_some(
            progress.filter(|_| active || summary.is_none()),
            |this, progress| {
                this.child(
                    div()
                        .pl_6()
                        .text_xs()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(progress),
                )
            },
        )
        .when(expanded, |this| {
            this.child(
                v_flex()
                    .pl_6()
                    .gap_1()
                    .text_xs()
                    .child(div().text_color(theme.muted_foreground).child("Assignment"))
                    .child(div().whitespace_normal().child(subagent.prompt.clone()))
                    .when_some(result, |this, result| {
                        this.child(
                            div()
                                .pt_1()
                                .text_color(theme.muted_foreground)
                                .child("Result"),
                        )
                        .child(div().whitespace_normal().child(result))
                    }),
            )
        })
        .when(!expanded, |this| {
            this.when_some(summary, |this, summary| {
                this.child(div().pl_6().text_xs().truncate().child(summary))
            })
        })
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
            let load = match ImageRef::attachment(attachment) {
                Some(image) => row
                    .environment
                    .update(cx, |environment, cx| environment.image(&image, cx)),
                None => ImageLoad::Failed,
            };
            attachment_thumbnail(&format!("{id}-{ix}"), attachment, load, cx)
        })
        .collect();
    let rich = {
        let choices = row.environment.read(cx).models();
        message_text::rich(text, |selector| {
            mention::find_choice(choices, selector).map(|choice| choice.label.text().to_string())
        })
    };
    let theme = cx.theme();
    let mut highlights = Vec::with_capacity(rich.spans.len());
    for span in &rich.spans {
        match span.kind {
            message_text::SpanKind::Keyword(keyword) => {
                let colors = keywords::colors(keyword, 0.0);
                let chars = rich.text[span.range.clone()].char_indices();
                for ((offset, ch), color) in chars.zip(colors) {
                    let start = span.range.start + offset;
                    highlights.push((
                        start..start + ch.len_utf8(),
                        HighlightStyle {
                            color: Some(color),
                            ..Default::default()
                        },
                    ));
                }
            }
            message_text::SpanKind::Model => highlights.push((
                span.range.clone(),
                HighlightStyle {
                    color: Some(theme.info),
                    font_weight: Some(FontWeight::BOLD),
                    background_color: Some(theme.info.opacity(0.15)),
                    ..Default::default()
                },
            )),
        }
    }
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
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(badge),
                    )
                })
                .when(!images.is_empty(), |this| {
                    this.child(h_flex().flex_wrap().gap_2().children(images))
                })
                .when(!text.is_empty(), |this| {
                    this.child(
                        div()
                            .whitespace_normal()
                            .child(StyledText::new(rich.text).with_highlights(highlights)),
                    )
                }),
        )
        .into_any_element()
}

fn attachment_thumbnail(
    id: &str,
    attachment: &ChatAttachment,
    load: ImageLoad,
    cx: &App,
) -> AnyElement {
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

/// Markdown images of a thread's messages, fetched through its environment
/// (see [`ImageRef::markdown`]).
fn markdown_images(
    environment: &Entity<Environment>,
    thread_id: &str,
) -> impl Fn(&SharedUri) -> ImageSource + Send + Sync + 'static {
    let environment = environment.downgrade();
    let thread_id = thread_id.to_owned();
    move |uri| {
        let image = ImageRef::markdown(uri.as_ref(), &thread_id);
        let environment = environment.clone();
        ImageSource::Custom(Arc::new(move |window, cx| {
            let load = environment
                .upgrade()?
                .update(cx, |environment, cx| environment.image(&image, cx));
            match load {
                ImageLoad::Loaded(image) => image.use_render_image(window, cx).map(Ok),
                ImageLoad::Loading => None,
                ImageLoad::Failed => Some(Err(ImageCacheError::Asset(
                    "the image could not be loaded".into(),
                ))),
            }
        }))
    }
}

/// Image `index` of a tool's output, as large as it is up to the feed's
/// width and [`MAX_OUTPUT_IMAGE_HEIGHT`].
fn output_image(item: &TurnItem, index: u64, row: &RowContext, cx: &mut App) -> AnyElement {
    let image = ImageRef::ToolOutput {
        thread_id: item.thread_id.clone(),
        item_id: item.id.clone(),
        index,
    };
    let load = row
        .environment
        .update(cx, |environment, cx| environment.image(&image, cx));
    let theme = cx.theme();
    let frame = div()
        .id(SharedString::from(format!(
            "tool-image-{}-{index}",
            item.id
        )))
        .test_support()
        .aria_label(match load {
            ImageLoad::Loaded(_) => "Tool output image",
            ImageLoad::Loading => "Loading image",
            ImageLoad::Failed => "Image unavailable",
        })
        .flex()
        .items_center()
        .justify_center()
        .max_w_full()
        .overflow_hidden()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border);
    match load {
        ImageLoad::Loaded(image) => frame
            .child(
                img(ImageSource::Image(image))
                    .max_w_full()
                    .max_h(MAX_OUTPUT_IMAGE_HEIGHT)
                    .object_fit(ObjectFit::Contain),
            )
            .into_any_element(),
        ImageLoad::Loading => frame
            .size_16()
            .child(Spinner::new().xsmall())
            .into_any_element(),
        ImageLoad::Failed => frame
            .px_2()
            .py_1()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child("Image unavailable")
            .into_any_element(),
    }
}

fn toggle(
    view: &WeakEntity<ThreadView>,
    id: String,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
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
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .hover(|this| this.text_color(theme.foreground))
                .child(Icon::new(Lucide::Brain).small())
                .map(|this| {
                    if streaming {
                        // A highlight sweeping through the label instead of a
                        // spinner; the row keeps the same size.
                        this.child(
                            ShimmerText::new("Thinking…")
                                .id(SharedString::from(format!("thinking-{id}"))),
                        )
                    } else {
                        this.child("Thought")
                    }
                })
                .child(disclosure(expanded, cx))
                .on_click(toggle(&row.view, id.to_owned())),
        )
        .when(expanded, |this| {
            this.when_some(row.markdown.get(id), |this, state| {
                this.child(
                    div()
                        .ml(TOOL_INDENT)
                        .pl_3()
                        .border_l_2()
                        .border_color(theme.border)
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(math::text_view(
                            TextView::new(state).selectable(true),
                            row.math,
                        )),
                )
            })
        })
        .into_any_element()
}

/// Where a tool's body starts: under its title, past the icon.
const TOOL_INDENT: Pixels = px(22.);

fn tool_icon(icon: ToolIcon) -> Icon {
    match icon {
        ToolIcon::Terminal => Icon::new(IconName::SquareTerminal),
        ToolIcon::Read => Icon::new(IconName::FileText),
        ToolIcon::Edit => Icon::new(Lucide::FilePen),
        ToolIcon::Write => Icon::new(Lucide::FilePlus),
        ToolIcon::Delete => Icon::new(Lucide::FileX),
        ToolIcon::Grep => Icon::new(IconName::Search),
        ToolIcon::Glob => Icon::new(Lucide::FolderSearch),
        ToolIcon::Web => Icon::new(IconName::Globe),
        ToolIcon::Eval => Icon::new(Lucide::Code),
        ToolIcon::Task => Icon::new(Lucide::Bot),
        ToolIcon::Todo => Icon::new(Lucide::ListTodo),
        ToolIcon::Wait => Icon::new(Lucide::Hourglass),
        ToolIcon::Message => Icon::new(Lucide::MessagesSquare),
        ToolIcon::Device => Icon::new(Lucide::Plug),
        ToolIcon::Process => Icon::new(IconName::Cpu),
        ToolIcon::Lsp => Icon::new(Lucide::Braces),
        ToolIcon::Ast => Icon::new(Lucide::ScanSearch),
        ToolIcon::Debug => Icon::new(Lucide::Bug),
        ToolIcon::Browser => Icon::new(Lucide::AppWindow),
        ToolIcon::Github => Icon::new(IconName::Github),
        ToolIcon::Memory => Icon::new(Lucide::Brain),
        ToolIcon::Tool => Icon::new(Lucide::Wrench),
    }
}

/// A tool call the way omp's TUI draws it: `icon Title description meta`,
/// then a framed preview of its body (the command and the end of its
/// output, the start of a diff, the first matches) that expands in place.
fn tool_row(item: &TurnItem, row: &RowContext, cx: &mut App) -> AnyElement {
    let Some(view) = tool_view(item) else {
        return div().into_any_element();
    };
    let images: Vec<AnyElement> = (0..view.images as u64)
        .map(|index| output_image(item, index, row, cx))
        .collect();
    let cx: &App = cx;
    let id = item.id.clone();
    let expanded = row.expanded.contains(&id);
    let theme = cx.theme();
    let mono = theme.mono_font_family.clone();
    let total = view.body.as_ref().map_or(0, ToolBody::lines);
    let expandable = view.body.is_some() && (view.preview == 0 || total > view.preview || expanded);
    let icon_color = match view.status {
        ToolStatus::Running => theme.info,
        ToolStatus::Failed => theme.danger,
        ToolStatus::Done => theme.muted_foreground,
    };
    let label = [view.title.as_str(), view.description.as_str()].join(" ");
    // The description and meta are smaller than the title: line them up on
    // its baseline.
    let title_font = (&theme.font_family, 0.875);
    let description_shift = crate::appearance::baseline_shift(
        cx,
        (
            if view.prose {
                &theme.font_family
            } else {
                &mono
            },
            0.75,
        ),
        title_font,
    );
    let meta_shift = crate::appearance::baseline_shift(cx, (&theme.font_family, 0.75), title_font);
    let header = h_flex()
        .id(SharedString::from(format!("tool-toggle-{id}")))
        .test_support()
        .aria_label(label)
        .gap_2()
        .px_1()
        .mx_neg_1()
        .rounded(theme.radius)
        .text_sm()
        .when(expandable, |this| {
            this.cursor_pointer()
                .hover(|this| this.bg(theme.muted.opacity(0.5)))
                .on_click(toggle(&row.view, id.clone()))
        })
        .when_some(view.intent.clone(), |this, intent| {
            this.tooltip(move |window, cx| Tooltip::new(intent.clone()).build(window, cx))
        })
        .child(tool_icon(view.icon).small().text_color(icon_color))
        .child(
            div()
                .flex_shrink_0()
                .font_medium()
                .child(view.title.clone()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .relative()
                .top(description_shift)
                .truncate()
                .text_xs()
                .when(!view.prose, |this| this.font_family(mono.clone()))
                .text_color(theme.muted_foreground)
                .child(view.description.clone()),
        )
        .children(view.meta.iter().map(|(text, tone)| {
            div()
                .flex_shrink_0()
                .relative()
                .top(meta_shift)
                .text_xs()
                .text_color(match tone {
                    Tone::Muted => theme.muted_foreground,
                    Tone::Added => theme.success,
                    Tone::Removed | Tone::Failed => theme.danger,
                })
                .child(text.clone())
        }))
        .map(|this| match view.status {
            ToolStatus::Running => this.child(Spinner::new().xsmall()),
            ToolStatus::Failed => this.child(
                Icon::new(IconName::CircleX)
                    .xsmall()
                    .text_color(theme.danger),
            ),
            ToolStatus::Done => this,
        })
        .when(expandable, |this| this.child(disclosure(expanded, cx)));
    let body = view
        .body
        .as_ref()
        .filter(|_| expanded || view.preview > 0)
        .map(|body| {
            let limit = if expanded {
                MAX_DETAIL_LINES
            } else {
                view.preview
            };
            let hidden = total.saturating_sub(limit);
            v_flex()
                .ml(TOOL_INDENT)
                .mt_1()
                .child(render_body(body, limit, expanded, view.status, cx))
                .when(hidden > 0 || (expanded && total > view.preview), |this| {
                    this.child(
                        div()
                            .id(SharedString::from(format!("tool-more-{id}")))
                            .pt_0p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .cursor_pointer()
                            .hover(|this| this.text_color(theme.foreground))
                            .on_click(toggle(&row.view, id.clone()))
                            .child(match (expanded, hidden) {
                                (true, 0) => "Show less".to_owned(),
                                (true, hidden) => format!("… {hidden} lines not shown"),
                                (false, hidden) => {
                                    format!(
                                        "… {hidden} more {}",
                                        if hidden == 1 { "line" } else { "lines" }
                                    )
                                }
                            }),
                    )
                })
        });
    v_flex()
        .child(header)
        .children(body)
        .when(!images.is_empty(), |this| {
            this.child(
                h_flex()
                    .ml(TOOL_INDENT)
                    .mt_1()
                    .flex_wrap()
                    .gap_2()
                    .children(images),
            )
        })
        .into_any_element()
}

/// A framed monospace block, like the TUI's boxed tool output.
fn frame(cx: &App) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.theme();
    v_flex()
        .id("tool-frame")
        .w_full()
        .overflow_hidden()
        .rounded(theme.radius)
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary.opacity(0.35))
        .text_size(theme.mono_font_size)
        .line_height(relative(1.45))
        .font_family(theme.mono_font_family.clone())
}

/// One monospace line, coloured by `spans` (plain without any): cut with an
/// ellipsis while collapsed, wrapped once the row is expanded.
fn mono_line(text: &str, spans: &crate::syntax::Spans, expanded: bool) -> gpui_kit::Div {
    let text = if text.is_empty() { " " } else { text };
    let line = div().min_w_0().map(|this| {
        if expanded {
            this.whitespace_normal()
        } else {
            this.truncate()
        }
    });
    if spans.is_empty() {
        line.child(text.replace('\t', "    "))
    } else if text.contains('\t') {
        let (text, spans) = crate::syntax::expand_tabs(text, spans);
        line.child(StyledText::new(text).with_highlights(spans))
    } else {
        line.child(StyledText::new(text.to_owned()).with_highlights(spans.iter().cloned()))
    }
}

/// Line `ix`'s spans in `lines`; none without highlights.
fn spans(lines: Option<&crate::syntax::Lines>, ix: usize) -> &crate::syntax::Spans {
    lines.map_or(&[], |lines| lines.line(ix))
}

/// The thin labelled rule between a frame's sections (`─── Output ───`).
fn section_rule(label: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .gap_2()
        .px_2()
        .py_0p5()
        .border_t_1()
        .border_color(theme.border)
        .text_color(theme.muted_foreground)
        .font_family(theme.font_family.clone())
        .child(label.to_owned())
        .into_any_element()
}

fn render_body(
    body: &ToolBody,
    limit: usize,
    expanded: bool,
    status: ToolStatus,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    match body {
        ToolBody::Command { command, output } => {
            let colours = crate::syntax::code(command, "bash", &theme.highlight_theme);
            let command_lines: Vec<&str> = command.lines().collect();
            let shown_command = if expanded {
                command_lines.len()
            } else {
                command_lines.len().min(3)
            };
            let output_lines: Vec<&str> = output.lines().collect();
            // The end of the output is what matters: show the tail.
            let tail = &output_lines[output_lines.len().saturating_sub(limit)..];
            frame(cx)
                .child(
                    v_flex()
                        .px_2()
                        .py_1()
                        .children(command_lines[..shown_command].iter().enumerate().map(
                            |(ix, line)| {
                                h_flex()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_color(theme.info)
                                            .child(if ix == 0 { "$" } else { " " }),
                                    )
                                    .child(mono_line(line, spans(colours.as_deref(), ix), expanded))
                            },
                        ))
                        .when(shown_command < command_lines.len(), |this| {
                            this.child(
                                div().text_color(theme.muted_foreground).child(format!(
                                    "  … {} more",
                                    command_lines.len() - shown_command
                                )),
                            )
                        }),
                )
                .when(!tail.is_empty(), |this| {
                    this.child(section_rule("Output", cx)).child(
                        v_flex()
                            .px_2()
                            .py_1()
                            .text_color(if status == ToolStatus::Failed {
                                theme.danger.opacity(0.9)
                            } else {
                                theme.foreground.opacity(0.85)
                            })
                            .children(tail.iter().map(|line| mono_line(line, &[], expanded))),
                    )
                })
                .into_any_element()
        }
        ToolBody::Diff {
            text: diff,
            language,
        } => {
            let gutter = diff
                .lines()
                .filter_map(|line| crate::feed::diff_line(line).number)
                .map(str::len)
                .max()
                .unwrap_or(0);
            let colours = crate::syntax::diff(diff, language, &theme.highlight_theme, |row| {
                let line = crate::feed::diff_line(row);
                (line.kind != DiffKind::Hunk).then_some(line.text)
            });
            frame(cx)
                .py_1()
                .children(diff.lines().take(limit).enumerate().map(|(ix, line)| {
                    let line = crate::feed::diff_line(line);
                    let (sign, color, bg) = match line.kind {
                        DiffKind::Added => ("+", theme.success, Some(theme.success.opacity(0.12))),
                        DiffKind::Removed => ("-", theme.danger, Some(theme.danger.opacity(0.12))),
                        DiffKind::Hunk => (" ", theme.info, None),
                        DiffKind::Context => (" ", theme.muted_foreground, None),
                    };
                    h_flex()
                        .px_2()
                        .gap_1p5()
                        .when_some(bg, |this, bg| this.bg(bg))
                        .when(gutter > 0, |this| {
                            this.child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(theme.muted_foreground.opacity(0.7))
                                    .child(format!("{:>gutter$}", line.number.unwrap_or(""))),
                            )
                        })
                        .child(div().flex_shrink_0().text_color(color).child(sign))
                        .child(
                            mono_line(line.text, spans(colours.as_deref(), ix), expanded)
                                .text_color(match line.kind {
                                    DiffKind::Context => theme.foreground.opacity(0.75),
                                    DiffKind::Hunk => theme.info,
                                    _ => theme.foreground,
                                }),
                        )
                }))
                .into_any_element()
        }
        ToolBody::Code {
            text,
            first_line,
            language,
        } => {
            let colours = crate::syntax::code(text, language, &theme.highlight_theme);
            let lines: Vec<&str> = text.lines().collect();
            let gutter = (first_line + lines.len()).to_string().len();
            frame(cx)
                .px_2()
                .py_1()
                .children(lines.iter().take(limit).enumerate().map(|(ix, line)| {
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground.opacity(0.7))
                                .child(format!("{:>gutter$}", first_line + ix)),
                        )
                        .child(mono_line(line, spans(colours.as_deref(), ix), expanded))
                }))
                .into_any_element()
        }
        ToolBody::List(lines) => {
            let shown = lines.len().min(limit);
            v_flex()
                .text_size(theme.mono_font_size)
                .line_height(relative(1.45))
                .font_family(theme.mono_font_family.clone())
                .children(lines[..shown].iter().enumerate().map(|(ix, line)| {
                    let last = ix + 1 == shown;
                    h_flex()
                        .gap_1()
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.muted_foreground.opacity(0.6))
                                .child(if last { "└─" } else { "├─" }),
                        )
                        .child(
                            mono_line(line, &[], expanded)
                                .text_color(theme.foreground.opacity(0.85)),
                        )
                }))
                .into_any_element()
        }
        ToolBody::Sections(sections) => {
            let mut budget = limit;
            frame(cx)
                .children(sections.iter().enumerate().map(|(ix, (label, text))| {
                    let lines: Vec<&str> = text.lines().collect();
                    let colours = crate::syntax::json(text, &theme.highlight_theme);
                    let shown = lines.len().min(budget);
                    budget -= shown;
                    v_flex()
                        .when_some(label.as_deref(), |this, label| {
                            this.child(section_rule(label, cx))
                        })
                        .when(label.is_none() && ix > 0, |this| {
                            this.border_t_1().border_color(theme.border)
                        })
                        .child(
                            v_flex()
                                .px_2()
                                .py_1()
                                .text_color(theme.foreground.opacity(0.85))
                                .children(lines[..shown].iter().enumerate().map(|(ix, line)| {
                                    mono_line(line, spans(colours.as_deref(), ix), expanded)
                                })),
                        )
                }))
                .into_any_element()
        }
    }
}

fn answer_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(answer_text).collect::<Vec<_>>().join(", "),
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
                        .child(
                            Icon::new(IconName::Check)
                                .xsmall()
                                .text_color(theme.success),
                        )
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
        .child(
            div()
                .pt_0p5()
                .child(Icon::new(icon).small().text_color(color)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_color(cx.theme().foreground)
                .child(message),
        )
        .into_any_element()
}
