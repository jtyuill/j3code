//! omp's agents hub for one thread: double-tap ← in an empty composer.
//!
//! The roster (left) lists the thread's subagents, running ones first; the
//! detail (right) shows the selected one's assignment and transcript, read
//! from its omp session file through `j3.getSubagentTranscript` and polled
//! while it runs. ↑/↓ or j/k select, PageUp/PageDown scroll the transcript,
//! x stops the selected subagent, Esc or ←← closes.

use std::{
    borrow::Cow,
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, StyledExt as _, WindowExt as _,
    button::Button,
    h_flex,
    notification::{Notification, NotificationType},
    spinner::Spinner,
    text::{TextView, TextViewState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, TestSupportExt as _, Window,
    div, point, px,
};
use j3_client::{ThreadState, t3::orchestration::Subagent};
use j3_protocol::j3::TranscriptEntry;

use crate::{environment::Environment, feed_view::subagent_status_label, math, mention};

/// How often the selected transcript is reread while its subagent runs.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// omp's double-tap: the second tap 40–500 ms after the first. Faster taps
/// are key repeat or a terminal burst and never count.
#[derive(Debug, Default)]
pub struct DoubleTap {
    last: Option<Instant>,
}

impl DoubleTap {
    const MIN_GAP: Duration = Duration::from_millis(40);
    const MAX_GAP: Duration = Duration::from_millis(500);

    /// A tap at `now`; true when it completes a double-tap.
    pub fn tap(&mut self, now: Instant) -> bool {
        let fired = self.last.is_some_and(|last| {
            let gap = now.saturating_duration_since(last);
            (Self::MIN_GAP..Self::MAX_GAP).contains(&gap)
        });
        self.last = (!fired).then_some(now);
        fired
    }

    /// Another key came between taps.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// The hub asks to be closed.
pub struct CloseHub;

/// What has been read of one subagent's transcript.
#[derive(Default)]
struct Transcript {
    entries: Vec<TranscriptEntry>,
    /// Markdown of `entries[i]` for user and assistant text.
    markdown: HashMap<usize, Entity<TextViewState>>,
    next_byte: u64,
    /// At least one read succeeded.
    loaded: bool,
    /// Read after the subagent finished, so nothing more will come.
    settled: bool,
    /// The last read capped: more is waiting.
    more: bool,
    error: Option<SharedString>,
}

pub struct AgentHub {
    environment: Entity<Environment>,
    thread_id: String,
    state: Arc<ThreadState>,
    selected: Option<String>,
    transcripts: HashMap<String, Transcript>,
    /// Expanded tool calls and results, by `subagent/index`.
    expanded: std::collections::HashSet<String>,
    detail_scroll: ScrollHandle,
    left: DoubleTap,
    /// Whether markdown typesets TeX math (the `math_rendering` preference).
    math: bool,
    focus_handle: FocusHandle,
    _poll: Task<()>,
}

impl EventEmitter<CloseHub> for AgentHub {}

impl AgentHub {
    pub fn new(
        environment: Entity<Environment>,
        thread_id: String,
        state: Arc<ThreadState>,
        math: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                let Ok(fetch) = this.update(cx, |hub, cx| hub.next_fetch(cx)) else {
                    break;
                };
                let more = match fetch {
                    Some((connection, thread_id, subagent_id, from_byte)) => {
                        let result = connection
                            .subagent_transcript(&thread_id, &subagent_id, from_byte)
                            .await;
                        let Ok(more) = this
                            .update(cx, |hub, cx| hub.apply(&subagent_id, from_byte, result, cx))
                        else {
                            break;
                        };
                        more
                    }
                    None => false,
                };
                if !more {
                    cx.background_executor().timer(POLL_INTERVAL).await;
                }
            }
        });
        let mut hub = Self {
            environment,
            thread_id,
            state,
            selected: None,
            transcripts: HashMap::new(),
            expanded: Default::default(),
            detail_scroll: ScrollHandle::new(),
            // ←← opened the hub: one more ←← closes it.
            left: DoubleTap::default(),
            math,
            focus_handle: cx.focus_handle(),
            _poll: poll,
        };
        hub.selected = hub.roster().first().map(|subagent| subagent.id.clone());
        hub
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus_handle.focus(window, cx);
    }

    pub fn set_state(&mut self, state: Arc<ThreadState>, cx: &mut Context<Self>) {
        self.state = state;
        if self
            .selected
            .as_ref()
            .is_none_or(|id| self.state.subagent(id).is_none())
        {
            self.selected = self.roster().first().map(|subagent| subagent.id.clone());
        }
        cx.notify();
    }

    /// Follows the math preference: the transcripts whose markdown the
    /// delimiter rewrite changes get their text again.
    pub fn set_math(&mut self, math: bool, cx: &mut Context<Self>) {
        self.math = math;
        for transcript in self.transcripts.values() {
            for (&ix, state) in &transcript.markdown {
                let Some(text) = transcript.entries.get(ix).and_then(markdown_text) else {
                    continue;
                };
                if let Cow::Owned(normalized) = math::normalize_delimiters(text) {
                    let source = if math { normalized.as_str() } else { text };
                    state.update(cx, |state, cx| state.set_text(source, cx));
                }
            }
        }
        cx.notify();
    }

    /// Running subagents first, then the most recently updated.
    fn roster(&self) -> Vec<&Subagent> {
        let mut roster: Vec<&Subagent> = self.state.subagents().iter().collect();
        roster.sort_by(|a, b| {
            b.status
                .is_active()
                .cmp(&a.status.is_active())
                .then_with(|| b.updated_at.cmp(&a.updated_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        roster
    }

    /// The read the selected transcript needs now, if any.
    fn next_fetch(&self, cx: &App) -> Option<(j3_client::Connection, String, String, u64)> {
        let id = self.selected.as_ref()?;
        let active = self.state.subagent(id)?.status.is_active();
        let transcript = self.transcripts.get(id);
        let due = transcript.is_none_or(|t| !t.loaded || t.more || active || !t.settled);
        due.then(|| {
            (
                self.environment.read(cx).connection().clone(),
                self.thread_id.clone(),
                id.clone(),
                transcript.map_or(0, |t| t.next_byte),
            )
        })
    }

    /// Applies one read; true when more is waiting right away.
    fn apply(
        &mut self,
        subagent_id: &str,
        from_byte: u64,
        result: Result<j3_protocol::j3::SubagentTranscript, j3_client::Error>,
        cx: &mut Context<Self>,
    ) -> bool {
        let active = self
            .state
            .subagent(subagent_id)
            .is_some_and(|subagent| subagent.status.is_active());
        let selected = self.selected.as_deref() == Some(subagent_id);
        let at_bottom = selected && self.at_bottom();
        let transcript = self.transcripts.entry(subagent_id.to_owned()).or_default();
        // A read that raced a reset or another read is dropped.
        if transcript.next_byte != from_byte {
            return true;
        }
        let read = match result {
            Ok(read) => read,
            Err(error) => {
                transcript.error = Some(error.to_string().into());
                // Not found yet (a subagent that just started): keep trying
                // while it runs.
                transcript.settled = !active;
                cx.notify();
                return false;
            }
        };
        if read.reset {
            transcript.entries.clear();
            transcript.markdown.clear();
        }
        let grew = !read.entries.is_empty();
        for entry in read.entries {
            if let Some(text) = markdown_text(&entry) {
                let source = math::markdown_source(text, self.math);
                let state = cx.new(|cx| TextViewState::markdown(&source, cx));
                transcript.markdown.insert(transcript.entries.len(), state);
            }
            transcript.entries.push(entry);
        }
        transcript.next_byte = read.next_byte;
        transcript.loaded = true;
        transcript.more = read.more;
        transcript.settled = !active && !read.more;
        transcript.error = None;
        if grew && at_bottom {
            self.detail_scroll.scroll_to_bottom();
        }
        cx.notify();
        read.more
    }

    fn at_bottom(&self) -> bool {
        let max = self.detail_scroll.max_offset().y;
        self.detail_scroll.offset().y <= -max + px(4.)
    }

    fn select(&mut self, id: String, cx: &mut Context<Self>) {
        if self.selected.as_ref() != Some(&id) {
            self.selected = Some(id);
            self.detail_scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let roster = self.roster();
        if roster.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|id| roster.iter().position(|subagent| &subagent.id == id))
            .unwrap_or(0);
        let next = (current as isize + delta).clamp(0, roster.len() as isize - 1) as usize;
        let id = roster[next].id.clone();
        self.select(id, cx);
    }

    fn scroll_page(&mut self, pages: f32) {
        let page = self.detail_scroll.bounds().size.height * 0.9;
        let max = self.detail_scroll.max_offset().y;
        let mut offset = self.detail_scroll.offset();
        offset.y = (offset.y - page * pages).clamp(-max, px(0.));
        self.detail_scroll.set_offset(offset);
    }

    /// Stops the selected subagent while it runs.
    fn stop_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(subagent) = self
            .selected
            .as_ref()
            .and_then(|id| self.state.subagent(id))
            .filter(|subagent| subagent.status.is_active())
        else {
            return;
        };
        let Some(run_id) = subagent.run_id.clone() else {
            return;
        };
        let subagent_id = subagent.id.clone();
        let connection = self.environment.read(cx).connection().clone();
        let thread_id = self.thread_id.clone();
        cx.spawn_in(window, async move |_, cx| {
            if let Err(e) = connection
                .stop_subagent(&thread_id, &run_id, &subagent_id)
                .await
            {
                let _ = cx.update(|window, cx| {
                    window.push_notification(
                        Notification::new()
                            .message(e.to_string())
                            .with_type(NotificationType::Error),
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        let key = keystroke.key.as_str();
        if key == "left" {
            if !event.is_held && self.left.tap(Instant::now()) {
                cx.emit(CloseHub);
            }
            cx.stop_propagation();
            return;
        }
        self.left.reset();
        match key {
            "up" | "k" => self.move_selection(-1, cx),
            "down" | "j" => self.move_selection(1, cx),
            "pageup" => self.scroll_page(-1.),
            "pagedown" => self.scroll_page(1.),
            "x" => self.stop_selected(window, cx),
            "escape" => cx.emit(CloseHub),
            _ => return,
        }
        cx.notify();
        cx.stop_propagation();
    }

    fn render_roster(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let rows: Vec<AnyElement> = self
            .roster()
            .into_iter()
            .map(|subagent| {
                let selected = self.selected.as_ref() == Some(&subagent.id);
                let active = subagent.status.is_active();
                let id = subagent.id.clone();
                let title = subagent_title(subagent);
                let status = subagent_status_label(subagent.status);
                let secondary = theme_secondary(selected, cx);
                v_flex()
                    .id(SharedString::from(format!("hub-agent-{}", subagent.id)))
                    .test_support()
                    .aria_label(format!("{}, {title}, {status}", subagent.id))
                    .gap_0p5()
                    .px_2()
                    .py_1p5()
                    .rounded(theme.radius)
                    .when(selected, |this| {
                        this.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .hover(|this| this.bg(theme.list_hover))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id.clone(), cx)))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(status_glyph(subagent, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_medium()
                                    .child(subagent.id.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(secondary)
                                    .child(status),
                            ),
                    )
                    .child(div().text_xs().truncate().child(title))
                    .when_some(
                        subagent.progress.clone().filter(|_| active),
                        |this, progress| {
                            this.child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(secondary)
                                    .child(progress),
                            )
                        },
                    )
                    .into_any_element()
            })
            .collect();
        let count = rows.len();
        v_flex()
            .w(px(300.))
            .flex_shrink_0()
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().font_medium().child("Agents"))
                    .child(if count == 1 {
                        "1 agent".to_owned()
                    } else {
                        format!("{count} agents")
                    }),
            )
            .child(
                div()
                    .id("hub-roster")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_2()
                    .child(v_flex().gap_0p5().children(rows)),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("↑↓ select · PgUp/PgDn scroll · x stop · Esc or ←← close"),
            )
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(subagent) = self
            .selected
            .as_ref()
            .and_then(|id| self.state.subagent(id))
        else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("No subagents")
                .into_any_element();
        };
        let active = subagent.status.is_active();
        let transcript = self.transcripts.get(&subagent.id);
        let stop = active.then(|| {
            Button::new("hub-stop")
                .xsmall()
                .outline()
                .icon(IconName::Square)
                .label("Stop")
                .on_click(cx.listener(|this, _, window, cx| this.stop_selected(window, cx)))
        });
        let header = h_flex()
            .gap_2()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .child(status_glyph(subagent, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .truncate()
                            .child(subagent_title(subagent)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(
                                [
                                    Some(subagent.id.clone()),
                                    subagent.model.as_deref().map(|model| {
                                        // The catalog's label when the slug is listed.
                                        let models = self.environment.read(cx).models();
                                        mention::find_choice(models, model).map_or_else(
                                            || model.to_owned(),
                                            |choice| choice.label.text().to_string(),
                                        )
                                    }),
                                    subagent.progress.clone(),
                                ]
                                .into_iter()
                                .flatten()
                                .collect::<Vec<_>>()
                                .join(" · "),
                            ),
                    ),
            )
            .when(active, |this| this.child(Spinner::new().xsmall()))
            .children(stop);

        let mut body: Vec<AnyElement> = Vec::new();
        match transcript {
            Some(transcript) if transcript.loaded => {
                for (ix, entry) in transcript.entries.iter().enumerate() {
                    let key = format!("{}/{ix}", subagent.id);
                    body.push(self.render_entry(entry, transcript.markdown.get(&ix), key, cx));
                }
                if transcript.entries.is_empty() {
                    body.push(muted_note("Nothing in the transcript yet", cx));
                }
            }
            Some(Transcript {
                error: Some(error), ..
            }) => {
                // No transcript yet: what j3 already knows.
                body.push(muted_note(error.clone(), cx));
                body.push(markdown_less("Assignment", &subagent.prompt, cx));
                if let Some(result) = &subagent.result {
                    body.push(markdown_less("Result", result, cx));
                }
            }
            _ => body.push(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().xsmall())
                    .child("Loading transcript…")
                    .into_any_element(),
            ),
        }
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(
                div()
                    .id("hub-transcript")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.detail_scroll)
                    .child(v_flex().gap_3().px_4().py_3().children(body)),
            )
            .into_any_element()
    }

    fn render_entry(
        &self,
        entry: &TranscriptEntry,
        markdown: Option<&Entity<TextViewState>>,
        key: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let math = self.math;
        let text_block = |text: &str| match markdown {
            Some(state) => {
                math::text_view(TextView::new(state).selectable(true), math).into_any_element()
            }
            None => div().child(text.to_owned()).into_any_element(),
        };
        match entry {
            TranscriptEntry::User { text } => v_flex()
                .gap_1()
                .px_3()
                .py_2()
                .rounded(theme.radius_lg)
                .bg(theme.muted)
                .text_sm()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("Assignment"),
                )
                .child(text_block(text))
                .into_any_element(),
            TranscriptEntry::Assistant { text } => {
                div().text_sm().child(text_block(text)).into_any_element()
            }
            TranscriptEntry::Thinking { text } => div()
                .text_xs()
                .italic()
                .text_color(theme.muted_foreground)
                .child(text.clone())
                .into_any_element(),
            TranscriptEntry::ToolCall {
                name,
                intent,
                arguments,
                ..
            } => self.collapsible(
                key,
                IconName::SquareTerminal,
                format!(
                    "{name}{}",
                    intent
                        .as_ref()
                        .map(|i| format!(" · {i}"))
                        .unwrap_or_default()
                ),
                arguments.clone(),
                false,
                cx,
            ),
            TranscriptEntry::ToolResult {
                name,
                text,
                is_error,
                ..
            } => self.collapsible(
                key,
                if *is_error {
                    IconName::CircleX
                } else {
                    IconName::Check
                },
                format!("{name} result"),
                text.clone(),
                *is_error,
                cx,
            ),
            TranscriptEntry::Error { text } => div()
                .text_sm()
                .text_color(theme.danger)
                .child(text.clone())
                .into_any_element(),
        }
    }

    /// A one-line tool row; clicking shows `body` in full.
    fn collapsible(
        &self,
        key: String,
        icon: IconName,
        title: String,
        body: String,
        is_error: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let expanded = self.expanded.contains(&key);
        let preview = crate::feed::one_line(&body);
        let toggle_key = key.clone();
        v_flex()
            .id(SharedString::from(format!("hub-entry-{key}")))
            .test_support()
            .gap_1()
            .text_xs()
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded.remove(&toggle_key) {
                    this.expanded.insert(toggle_key.clone());
                }
                cx.notify();
            }))
            .child(
                h_flex()
                    .gap_2()
                    .text_color(if is_error {
                        theme.danger
                    } else {
                        theme.muted_foreground
                    })
                    .child(Icon::new(icon).xsmall())
                    .child(div().flex_shrink_0().font_medium().child(title))
                    .when(!expanded, |this| {
                        this.child(div().flex_1().min_w_0().truncate().child(preview))
                    }),
            )
            .when(expanded, |this| {
                this.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(theme.radius)
                        .bg(theme.muted)
                        .text_size(theme.mono_font_size)
                        .font_family(theme.mono_font_family.clone())
                        .child(body),
                )
            })
            .into_any_element()
    }
}

fn subagent_title(subagent: &Subagent) -> String {
    subagent
        .title
        .clone()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| crate::feed::one_line(&subagent.prompt))
}

fn theme_secondary(selected: bool, cx: &App) -> gpui_kit::Hsla {
    let theme = cx.theme();
    if selected {
        theme.accent_foreground
    } else {
        theme.muted_foreground
    }
}

fn status_glyph(subagent: &Subagent, cx: &App) -> AnyElement {
    use j3_client::t3::orchestration::SubagentStatus as S;
    let theme = cx.theme();
    let (icon, color) = match subagent.status {
        S::Pending | S::Running | S::Waiting => (Lucide::LoaderCircle, theme.primary),
        S::Completed => (Lucide::CircleCheck, theme.success),
        S::Failed => (Lucide::CircleX, theme.danger),
        S::Cancelled | S::Interrupted => (Lucide::CircleSlash, theme.warning),
        S::Idle => (Lucide::Circle, theme.muted_foreground),
    };
    Icon::new(icon).small().text_color(color).into_any_element()
}

fn muted_note(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
        .into_any_element()
}

/// A labelled plain-text block (for the fallback when no transcript reads).
fn markdown_less(label: &'static str, text: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .gap_1()
        .text_sm()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(label),
        )
        .child(text.to_owned())
        .into_any_element()
}

/// The text of an entry shown as markdown: user and assistant text.
fn markdown_text(entry: &TranscriptEntry) -> Option<&str> {
    match entry {
        TranscriptEntry::User { text } | TranscriptEntry::Assistant { text } => Some(text.as_str()),
        _ => None,
    }
}

impl Focusable for AgentHub {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AgentHub {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let roster = self.render_roster(cx);
        let detail = self.render_detail(cx);
        h_flex()
            .id("agent-hub")
            .test_support()
            .key_context("AgentHub")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .items_start()
            .bg(cx.theme().background)
            .child(roster)
            .child(detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_tap_needs_a_second_tap_within_the_window() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut tap = DoubleTap::default();
        assert!(!tap.tap(t0));
        assert!(tap.tap(ms(200)));
        // The pair is consumed: the next tap starts over.
        assert!(!tap.tap(ms(300)));

        // Too slow: the late tap becomes the first of a new pair.
        let mut tap = DoubleTap::default();
        assert!(!tap.tap(t0));
        assert!(!tap.tap(ms(500)));
        assert!(tap.tap(ms(600)));

        // Key-repeat speed never fires.
        let mut tap = DoubleTap::default();
        assert!(!tap.tap(t0));
        assert!(!tap.tap(ms(20)));
        assert!(!tap.tap(ms(39)));

        // Another key in between cancels.
        let mut tap = DoubleTap::default();
        assert!(!tap.tap(t0));
        tap.reset();
        assert!(!tap.tap(ms(100)));
    }
}
