//! herdr mode: Spaces (one row per project, with its rolled-up attention)
//! and Agents (every thread with its state glyph), as herdr's sidebar.

use std::collections::HashMap;

use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, Theme,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::ListItem,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};
use j3_client::workflow::{
    AgentOrder, AgentStatus, EnvThread, StatusIndicators, agent_status, is_listed, rollup,
    sort_agents_by_attention, sort_agents_by_space,
};

use super::{EnvSnapshot, ProjectKey, Sidebar, SidebarEvent, ThreadKey};
use crate::ui_settings::UiSettings;

/// One project.
pub struct SpaceRow {
    pub project: ProjectKey,
    pub title: SharedString,
    pub env_label: SharedString,
    pub status: AgentStatus,
    /// The space's highest-attention thread, opened by a click.
    pub target: Option<ThreadKey>,
}

/// One thread.
pub struct AgentRow {
    pub key: ThreadKey,
    pub title: SharedString,
    pub env_label: SharedString,
    pub project: SharedString,
    pub status: AgentStatus,
}

#[derive(Default)]
pub struct HerdrRows {
    pub spaces: Vec<SpaceRow>,
    pub agents: Vec<AgentRow>,
    pub indicators: StatusIndicators,
    pub agent_order: AgentOrder,
    /// Shown instead of rows while no environment has loaded.
    pub note: Option<SharedString>,
}

impl HerdrRows {
    pub fn build(snapshots: &[EnvSnapshot], ui: &UiSettings) -> Self {
        let mut spaces = Vec::new();
        let mut rank: HashMap<(&str, &str), usize> = HashMap::new();
        let mut threads: Vec<EnvThread> = Vec::new();
        for env in snapshots {
            let shell = &env.state.shell;
            for project in &shell.projects {
                rank.insert((env.key.as_ref(), project.id.as_str()), spaces.len());
                spaces.push(SpaceRow {
                    project: ProjectKey {
                        env: env.key.clone(),
                        project_id: project.id.clone(),
                    },
                    title: project.title.clone().into(),
                    env_label: env.label.clone(),
                    status: AgentStatus::Unknown,
                    target: None,
                });
            }
            threads.extend(
                shell
                    .threads
                    .iter()
                    .filter(|thread| is_listed(thread))
                    .map(|thread| EnvThread {
                        environment: env.key.as_ref(),
                        thread,
                    }),
            );
        }
        let space_of = |thread: &EnvThread| {
            rank.get(&(thread.environment, thread.thread.project_id.as_str()))
                .copied()
        };

        // Each space's status and the thread its row opens: the agent with
        // the highest attention, the most recent change breaking ties.
        let mut by_attention = threads.clone();
        sort_agents_by_attention(&mut by_attention);
        let mut statuses: Vec<Vec<AgentStatus>> = vec![Vec::new(); spaces.len()];
        for thread in &by_attention {
            let Some(ix) = space_of(thread) else {
                continue;
            };
            statuses[ix].push(agent_status(thread.thread));
            if spaces[ix].target.is_none() {
                spaces[ix].target = Some(ThreadKey::new(
                    SharedString::from(thread.environment.to_owned()),
                    thread.thread.id.clone(),
                ));
            }
        }
        for (space, statuses) in spaces.iter_mut().zip(statuses) {
            space.status = rollup(statuses);
        }

        match ui.agent_order {
            AgentOrder::Attention => threads = by_attention,
            AgentOrder::Spaces => sort_agents_by_space(&mut threads, |thread| {
                space_of(thread).unwrap_or(usize::MAX)
            }),
        }
        let labels: HashMap<&str, &SharedString> = snapshots
            .iter()
            .map(|env| (env.key.as_ref(), &env.label))
            .collect();
        let agents = threads
            .iter()
            .map(|thread| {
                let project = space_of(thread)
                    .map(|ix| spaces[ix].title.clone())
                    .unwrap_or_default();
                AgentRow {
                    key: ThreadKey::new(
                        SharedString::from(thread.environment.to_owned()),
                        thread.thread.id.clone(),
                    ),
                    title: display_title(&thread.thread.title),
                    env_label: labels
                        .get(thread.environment)
                        .map(|label| (*label).clone())
                        .unwrap_or_default(),
                    project,
                    status: agent_status(thread.thread),
                }
            })
            .collect();
        let note = (!snapshots.iter().any(|env| env.state.shell.loaded))
            .then(|| SharedString::from("Connecting…"));
        Self {
            spaces,
            agents,
            indicators: ui.status_indicators,
            agent_order: ui.agent_order,
            note,
        }
    }

    /// Agents in list order, for next/previous and jump.
    pub fn order(&self) -> Vec<ThreadKey> {
        self.agents.iter().map(|agent| agent.key.clone()).collect()
    }
}

/// A thread title for a row (`Untitled` when blank).
pub(crate) fn display_title(title: &str) -> SharedString {
    if title.trim().is_empty() {
        "Untitled".into()
    } else {
        title.to_owned().into()
    }
}

/// herdr's palette roles: working yellow, blocked red, done teal, idle
/// green, unknown dim.
pub(crate) fn status_color(status: AgentStatus, theme: &Theme) -> Hsla {
    match status {
        AgentStatus::Working => theme.warning,
        AgentStatus::Blocked => theme.danger,
        AgentStatus::Done => theme.info,
        AgentStatus::Idle => theme.success,
        AgentStatus::Unknown => theme.muted_foreground,
    }
}

/// The glyph in a fixed slot, so labels keep one leading edge.
pub fn glyph(status: AgentStatus, indicators: StatusIndicators, theme: &Theme) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .w_4()
        .flex()
        .justify_center()
        .text_color(status_color(status, theme))
        .child(status.glyph(indicators))
}

impl Sidebar {
    pub(super) fn render_herdr(
        &self,
        rows: std::rc::Rc<HerdrRows>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let section_title =
            |title: &'static str| div().text_xs().font_medium().text_color(muted).child(title);
        let order_label = match rows.agent_order {
            AgentOrder::Spaces => "by space",
            AgentOrder::Attention => "by attention",
        };

        let spaces = rows.spaces.iter().map(|space| {
            let project = space.project.clone();
            let target = space.target.clone();
            let new_project = space.project.clone();
            let id = SharedString::from(format!(
                "space-{}-{}",
                space.project.env, space.project.project_id
            ));
            ListItem::new(id)
                .accessibility_label(format!("{}, {}", space.title, space.status.label()))
                .px_2()
                .py_1()
                .rounded(theme.radius)
                .text_sm()
                .child(
                    h_flex()
                        .gap_2()
                        .child(glyph(space.status, rows.indicators, theme))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(space.title.clone()),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_xs()
                                .text_color(muted)
                                .child(space.env_label.clone()),
                        ),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.emit(match &target {
                        Some(key) => SidebarEvent::OpenThread(key.clone()),
                        None => SidebarEvent::NewThread(Some(project.clone())),
                    })
                }))
                .suffix({
                    let weak = cx.weak_entity();
                    move |_, _| {
                        let weak = weak.clone();
                        let project = new_project.clone();
                        Button::new(SharedString::from(format!(
                            "new-thread-{}-{}",
                            project.env, project.project_id
                        )))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("New thread")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            let _ = weak.update(cx, |_, cx| {
                                cx.emit(SidebarEvent::NewThread(Some(project.clone())))
                            });
                        })
                    }
                })
        });

        let agents =
            rows.agents.iter().map(|agent| {
                let key = agent.key.clone();
                let selected = self.selected.as_ref() == Some(&agent.key);
                ListItem::new(agent.key.element_id("agent"))
                    .accessibility_label(format!("{}, {}", agent.title, agent.status.label()))
                    .selected(selected)
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .text_xs()
                                    .child(
                                        div()
                                            .id(agent.key.element_id("glyph"))
                                            .test_support()
                                            .aria_label(agent.status.glyph(rows.indicators))
                                            .child(glyph(agent.status, rows.indicators, theme)),
                                    )
                                    .child(
                                        div().min_w_0().truncate().text_color(muted).child(
                                            format!("{} · {}", agent.env_label, agent.project),
                                        ),
                                    ),
                            )
                            .child(
                                h_flex().gap_2().child(div().flex_shrink_0().w_4()).child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .when(selected, |this| this.font_medium())
                                        .child(agent.title.clone()),
                                ),
                            ),
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(SidebarEvent::OpenThread(key.clone()))
                    }))
            });

        let empty =
            |text: &'static str| div().px_2().py_1().text_xs().text_color(muted).child(text);
        let has_spaces = !rows.spaces.is_empty();
        let has_agents = !rows.agents.is_empty();
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h_9()
                    .px_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .child(section_title("Spaces"))
                    .child(
                        Button::new("new-thread")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip("New thread")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(SidebarEvent::NewThread(None))),
                            ),
                    ),
            )
            .child(
                div()
                    .id("herdr-list")
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .px_1()
                            .py_1()
                            .when_some(rows.note.clone(), |this, note| {
                                this.child(
                                    div().px_2().py_1().text_xs().text_color(muted).child(note),
                                )
                            })
                            .child(
                                v_flex()
                                    .id("herdr-spaces")
                                    .test_support()
                                    .children(spaces)
                                    .when(!has_spaces && rows.note.is_none(), |this| {
                                        this.child(empty("No projects"))
                                    }),
                            )
                            .child(
                                h_flex()
                                    .mt_2()
                                    .pt_2()
                                    .px_2()
                                    .pb_1()
                                    .justify_between()
                                    .border_t_1()
                                    .border_color(theme.sidebar_border)
                                    .child(section_title("Agents"))
                                    .child(div().text_xs().text_color(muted).child(order_label)),
                            )
                            .child(
                                v_flex()
                                    .id("herdr-agents")
                                    .test_support()
                                    .children(agents)
                                    .when(!has_agents && rows.note.is_none(), |this| {
                                        this.child(empty("No agents"))
                                    }),
                            ),
                    ),
            )
            .into_any_element()
    }
}
