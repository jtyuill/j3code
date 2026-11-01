//! herdr's goto picker: every agent, grouped by space, with search and the
//! single-key state filters (`b` blocked, `w` working, `i` idle, `d` done,
//! `a` all). `/` moves to the search field; Enter jumps.

use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, h_flex,
    input::{Input, InputEvent, InputState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window, div,
};
use j3_client::workflow::{AgentStatus, StatusIndicators};

use crate::{
    actions::GOTO,
    sidebar::{HerdrRows, ThreadKey, herdr_glyph},
};

/// The picker chose a thread.
pub struct GotoJump(pub ThreadKey);

struct Entry {
    key: ThreadKey,
    title: SharedString,
    status: AgentStatus,
    /// Lowercase words searched: title, project, environment, status.
    haystack: String,
}

struct Group {
    heading: SharedString,
    entries: Vec<Entry>,
}

pub struct GotoPicker {
    groups: Vec<Group>,
    indicators: StatusIndicators,
    query: Entity<InputState>,
    filter: Option<AgentStatus>,
    /// Index into the visible entries.
    selected: usize,
    current: Option<ThreadKey>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<GotoJump> for GotoPicker {}

impl GotoPicker {
    /// `rows` in space order; `current` is the open thread.
    pub fn new(
        rows: &HerdrRows,
        current: Option<ThreadKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut groups: Vec<Group> = Vec::new();
        for agent in &rows.agents {
            let heading: SharedString = format!("{} · {}", agent.project, agent.env_label).into();
            let entry = Entry {
                key: agent.key.clone(),
                title: agent.title.clone(),
                status: agent.status,
                haystack: format!(
                    "{} {} {} {}",
                    agent.title,
                    agent.project,
                    agent.env_label,
                    agent.status.label()
                )
                .to_lowercase(),
            };
            match groups.last_mut() {
                Some(group) if group.heading == heading => group.entries.push(entry),
                _ => groups.push(Group {
                    heading,
                    entries: vec![entry],
                }),
            }
        }
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search agents"));
        let subscriptions = vec![cx.subscribe_in(
            &query,
            window,
            |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    this.selected = 0;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.confirm(cx),
                InputEvent::Focus | InputEvent::Blur => {}
            },
        )];
        let mut picker = Self {
            groups,
            indicators: rows.indicators,
            query,
            filter: None,
            selected: 0,
            current: current.clone(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        if let Some(ix) = current.and_then(|current| {
            picker
                .visible(cx)
                .iter()
                .position(|entry| entry.key == current)
        }) {
            picker.selected = ix;
        }
        picker
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus_handle.focus(window, cx);
    }

    fn matches(&self, entry: &Entry, words: &[String]) -> bool {
        self.filter.is_none_or(|filter| entry.status == filter)
            && words
                .iter()
                .all(|word| entry.haystack.contains(word.as_str()))
    }

    fn words(&self, cx: &App) -> Vec<String> {
        self.query
            .read(cx)
            .value()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect()
    }

    /// Visible entries in order.
    fn visible(&self, cx: &App) -> Vec<&Entry> {
        let words = self.words(cx);
        self.groups
            .iter()
            .flat_map(|group| group.entries.iter())
            .filter(|entry| self.matches(entry, &words))
            .collect()
    }

    /// Jumps to the selected agent.
    pub fn confirm(&mut self, cx: &mut Context<Self>) {
        let key = self
            .visible(cx)
            .get(self.selected)
            .map(|entry| entry.key.clone());
        if let Some(key) = key {
            cx.emit(GotoJump(key));
        }
    }

    fn set_filter(&mut self, filter: Option<AgentStatus>, cx: &mut Context<Self>) {
        self.filter = filter;
        self.selected = 0;
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.visible(cx).len();
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, count as isize - 1) as usize;
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        if keystroke.modifiers.modified() {
            return;
        }
        let in_search = self.query.read(cx).focus_handle(cx).is_focused(window);
        match keystroke.key.as_str() {
            "up" => self.move_selection(-1, cx),
            "down" => self.move_selection(1, cx),
            "enter" if !in_search => self.confirm(cx),
            "/" if !in_search => {
                self.query.update(cx, |input, cx| input.focus(window, cx));
            }
            key if !in_search => {
                let filter = match key {
                    "a" => {
                        self.query
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        self.set_filter(None, cx);
                        return;
                    }
                    _ => [
                        AgentStatus::Blocked,
                        AgentStatus::Working,
                        AgentStatus::Idle,
                        AgentStatus::Done,
                    ]
                    .into_iter()
                    .find(|status| key.len() == 1 && status.filter_key() == key.chars().next()),
                };
                match filter {
                    Some(filter) => self.set_filter(Some(filter), cx),
                    None => return,
                }
            }
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Focusable for GotoPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GotoPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let words = self.words(cx);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let mut index = 0;
        let mut total = 0;
        let groups: Vec<_> = self
            .groups
            .iter()
            .filter_map(|group| {
                let rows: Vec<_> = group
                    .entries
                    .iter()
                    .filter(|entry| self.matches(entry, &words))
                    .map(|entry| {
                        let selected = index == self.selected;
                        let ix = index;
                        index += 1;
                        let current = self.current.as_ref() == Some(&entry.key);
                        h_flex()
                            .id(entry.key.element_id("goto"))
                            .test_support()
                            .aria_label(format!("{}, {}", entry.title, entry.status.label()))
                            .gap_2()
                            .px_2()
                            .py_1()
                            .rounded(theme.radius)
                            .text_sm()
                            .when(selected, |this| {
                                this.bg(theme.accent).text_color(theme.accent_foreground)
                            })
                            .hover(|this| this.bg(theme.list_hover))
                            .child(herdr_glyph(entry.status, self.indicators, theme))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .when(current, |this| this.font_medium())
                                    .child(entry.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(if selected {
                                        theme.accent_foreground
                                    } else {
                                        muted
                                    })
                                    .child(entry.status.label()),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.selected = ix;
                                this.confirm(cx);
                            }))
                    })
                    .collect();
                total += rows.len();
                (!rows.is_empty()).then(|| {
                    v_flex()
                        .gap_0p5()
                        .child(
                            div()
                                .px_2()
                                .pt_2()
                                .text_xs()
                                .font_medium()
                                .text_color(muted)
                                .child(group.heading.clone()),
                        )
                        .children(rows)
                })
            })
            .collect();
        let filter_hint = |key: char, label: &'static str, status: Option<AgentStatus>| {
            let active = self.filter == status;
            div()
                .text_xs()
                .text_color(if active { theme.foreground } else { muted })
                .when(active, |this| this.font_medium())
                .child(format!("{key} {label}"))
        };
        v_flex()
            .id("goto-picker")
            .test_support()
            .key_context(GOTO)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .gap_2()
            .child(Input::new(&self.query))
            .child(
                h_flex()
                    .gap_3()
                    .child(filter_hint('b', "blocked", Some(AgentStatus::Blocked)))
                    .child(filter_hint('w', "working", Some(AgentStatus::Working)))
                    .child(filter_hint('i', "idle", Some(AgentStatus::Idle)))
                    .child(filter_hint('d', "done", Some(AgentStatus::Done)))
                    .child(filter_hint('a', "all", None))
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(muted).child(if total == 1 {
                        "1 agent".to_owned()
                    } else {
                        format!("{total} agents")
                    })),
            )
            .child(div().id("goto-list").max_h_96().overflow_y_scroll().child(
                v_flex().children(groups).when(total == 0, |this| {
                    this.child(
                        div()
                            .px_2()
                            .py_2()
                            .text_sm()
                            .text_color(muted)
                            .child("No matching agents"),
                    )
                }),
            ))
    }
}
