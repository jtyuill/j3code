//! herdr mode (spec §4.2): Spaces (one row per project, with its rolled-up attention) and
//! Agents (every thread with its state glyph), as herdr's sidebar.

use std::collections::HashMap;

use j3_client::workflow::{
    AgentOrder, AgentStatus, EnvThread, StatusIndicators, agent_status, is_listed, rollup,
    sort_agents_by_attention, sort_agents_by_space,
};
use rune::{Color, El, Tx, col, el, row, spacer, text};
use rune_kit::{
    ControlSize, IconButton, IconButtonVariant, Kit,
    shell::{Sidebar, SidebarRow},
};

use super::{EnvSnapshot, ProjectKey, ThreadKey};
use crate::{
    actions::{Action, shortcut_label},
    app::{J3, Ui},
    icons,
    theme::Status,
};

/// One project.
#[derive(Debug, Clone)]
pub struct SpaceRow {
    pub project: ProjectKey,
    pub title: String,
    pub env_label: String,
    pub status: AgentStatus,
    /// The space's highest-attention thread, opened by a click.
    pub target: Option<ThreadKey>,
}

impl SpaceRow {
    /// The row's element key: `space-{env}-{project}`.
    pub fn element_id(&self) -> String {
        format!("space-{}-{}", self.project.env, self.project.project_id)
    }
}

/// One thread.
#[derive(Debug, Clone)]
pub struct AgentRow {
    pub key: ThreadKey,
    pub title: String,
    pub env_label: String,
    pub project: String,
    pub status: AgentStatus,
}

#[derive(Debug, Default)]
pub struct HerdrRows {
    pub spaces: Vec<SpaceRow>,
    pub agents: Vec<AgentRow>,
    pub indicators: StatusIndicators,
    pub agent_order: AgentOrder,
    /// Shown instead of rows while no environment has loaded.
    pub note: Option<&'static str>,
}

impl HerdrRows {
    pub fn build(
        snapshots: &[EnvSnapshot],
        indicators: StatusIndicators,
        order: AgentOrder,
    ) -> Self {
        let mut spaces = Vec::new();
        let mut rank: HashMap<(&str, &str), usize> = HashMap::new();
        let mut threads: Vec<EnvThread> = Vec::new();
        for env in snapshots {
            let shell = &env.state.shell;
            for project in &shell.projects {
                rank.insert((env.key, project.id.as_str()), spaces.len());
                spaces.push(SpaceRow {
                    project: ProjectKey {
                        env: env.key.to_owned(),
                        project_id: project.id.clone(),
                    },
                    title: project.title.clone(),
                    env_label: env.label.to_owned(),
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
                        environment: env.key,
                        thread,
                    }),
            );
        }
        let space_of = |thread: &EnvThread| {
            rank.get(&(thread.environment, thread.thread.project_id.as_str()))
                .copied()
        };

        // Each space's status and the thread its row opens: the agent with the highest
        // attention, the most recent change breaking ties.
        let mut by_attention = threads.clone();
        sort_agents_by_attention(&mut by_attention);
        let mut statuses: Vec<Vec<AgentStatus>> = vec![Vec::new(); spaces.len()];
        for thread in &by_attention {
            let Some(ix) = space_of(thread) else {
                continue;
            };
            statuses[ix].push(agent_status(thread.thread));
            if spaces[ix].target.is_none() {
                spaces[ix].target = Some(ThreadKey::new(thread.environment, &thread.thread.id));
            }
        }
        for (space, statuses) in spaces.iter_mut().zip(statuses) {
            space.status = rollup(statuses);
        }

        match order {
            AgentOrder::Attention => threads = by_attention,
            AgentOrder::Spaces => sort_agents_by_space(&mut threads, |thread| {
                space_of(thread).unwrap_or(usize::MAX)
            }),
        }
        let labels: HashMap<&str, &str> =
            snapshots.iter().map(|env| (env.key, env.label)).collect();
        let agents = threads
            .iter()
            .map(|thread| AgentRow {
                key: ThreadKey::new(thread.environment, &thread.thread.id),
                title: display_title(&thread.thread.title),
                env_label: labels
                    .get(thread.environment)
                    .map_or_else(String::new, |label| (*label).to_owned()),
                project: space_of(thread).map_or_else(String::new, |ix| spaces[ix].title.clone()),
                status: agent_status(thread.thread),
            })
            .collect();
        Self {
            spaces,
            agents,
            indicators,
            agent_order: order,
            note: (!snapshots.iter().any(|env| env.state.shell.loaded)).then_some("Connecting…"),
        }
    }

    /// Agents in list order, for next/previous and jump.
    pub fn order(&self) -> Vec<ThreadKey> {
        self.agents.iter().map(|agent| agent.key.clone()).collect()
    }
}

/// A thread title for a row (`Untitled` when blank).
pub fn display_title(title: &str) -> String {
    if title.trim().is_empty() {
        "Untitled".to_owned()
    } else {
        title.to_owned()
    }
}

/// herdr's palette roles: working yellow, blocked red, done teal, idle green, unknown dim.
pub fn status_color(status: AgentStatus, k: &Kit) -> Color {
    let s = Status::of(k);
    match status {
        AgentStatus::Working => s.warning,
        AgentStatus::Blocked => s.danger,
        AgentStatus::Done => s.info,
        AgentStatus::Idle => s.success,
        AgentStatus::Unknown => s.muted,
    }
}

/// The glyph in a fixed 16 pt slot, so labels keep one leading edge.
pub fn glyph(status: AgentStatus, indicators: StatusIndicators, k: &Kit) -> El {
    row().w(16.0).center().child(
        text(status.glyph(indicators))
            .font(k.ty.text_small)
            .color(status_color(status, k)),
    )
}

/// The herdr sidebar: a "Spaces" header with a new-thread button, then the spaces and every
/// agent.
pub fn view(s: &J3, ui: &mut Ui) -> El {
    let k = ui.k;
    let rows = s.herdr_rows();
    let open = s.open_thread_key();
    let muted = k.c.text_muted;
    let new_thread = IconButton::new(&icons::PLUS)
        .variant(IconButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .label("New thread")
        .help(format!(
            "New thread ({})",
            shortcut_label(Action::NewThread)
        ))
        .on_press_tx(|s: &mut J3, tx: &mut Tx| s.press_new_thread(tx))
        .build(k)
        .key("new-thread");
    let header = row()
        .key("herdr-header")
        .w_full()
        .h(28.0)
        .pl(8.0)
        .center_y()
        .child(section_title("Spaces", k))
        .child(spacer())
        .child(crate::new_thread::button(s, ui, new_thread));
    let mut sidebar = Sidebar::new().label("Agents").header(header);
    if let Some(note) = rows.note {
        sidebar = sidebar.child(placeholder("herdr-note", note, k));
    }
    let mut spaces = col().key("herdr-spaces").w_full();
    for space in &rows.spaces {
        spaces = spaces.child(space_row(space, rows.indicators, k));
    }
    if rows.spaces.is_empty() && rows.note.is_none() {
        spaces = spaces.child(placeholder("herdr-no-spaces", "No projects", k));
    }
    let order_label = match rows.agent_order {
        AgentOrder::Spaces => "by space",
        AgentOrder::Attention => "by attention",
    };
    let heading = row()
        .key("herdr-agents-heading")
        .w_full()
        .mt(8.0)
        .pt(8.0)
        .px(8.0)
        .pb(4.0)
        .center_y()
        .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_muted)
        .child(section_title("Agents", k))
        .child(spacer())
        .child(text(order_label).font(k.ty.text_xsmall).color(muted));
    let mut agents = col().key("herdr-agents").w_full();
    for agent in &rows.agents {
        let selected = open.as_ref() == Some(&agent.key);
        agents = agents.child(agent_row(agent, selected, rows.indicators, k));
    }
    if rows.agents.is_empty() && rows.note.is_none() {
        agents = agents.child(placeholder("herdr-no-agents", "No agents", k));
    }
    sidebar.child(spaces).child(heading).child(agents).build(k)
}

fn section_title(title: &'static str, k: &Kit) -> El {
    text(title)
        .font(k.ty.text_xsmall_medium)
        .color(k.c.text_muted)
}

fn placeholder(key: &'static str, note: &'static str, k: &Kit) -> El {
    row()
        .key(key)
        .w_full()
        .px(8.0)
        .py(4.0)
        .child(text(note).font(k.ty.text_xsmall).color(k.c.text_muted))
}

/// A space: its rolled-up glyph, title and environment; a click opens its most urgent agent
/// (or starts a thread in an empty space), `+` starts a thread in it.
fn space_row(space: &SpaceRow, indicators: StatusIndicators, k: &Kit) -> El {
    let project = space.project.clone();
    let new_in = space.project.clone();
    let target = space.target.clone();
    let add = IconButton::new(&icons::PLUS)
        .variant(IconButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .label("New thread")
        .help("New thread")
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.new_thread_in(Some(new_in.clone()), tx))
        .build(k)
        .key(format!("new-thread-{}-{}", project.env, project.project_id))
        .focusable(false);
    SidebarRow::new(space.title.clone())
        .avatar(glyph(space.status, indicators, k))
        .detail(space.env_label.clone())
        .trailing(add)
        .on_select(move |s: &mut J3, tx: &mut Tx| match &target {
            Some(key) => s.open_thread(key.clone(), tx),
            None => s.new_thread_in(Some(project.clone()), tx),
        })
        .build(k)
        .key(space.element_id())
        .label(format!("{}, {}", space.title, space.status.label()))
}

/// An agent: its glyph and `"{env} · {project}"`, then its title (medium when open).
fn agent_row(agent: &AgentRow, selected: bool, indicators: StatusIndicators, k: &Kit) -> El {
    let c = &k.c;
    let key = agent.key.clone();
    let place = row()
        .key("place")
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            glyph(agent.status, indicators, k)
                .key(agent.key.element_id("glyph"))
                .label(agent.status.glyph(indicators)),
        )
        .child(
            text(format!("{} · {}", agent.env_label, agent.project))
                .font(k.ty.text_xsmall)
                .color(c.text_muted)
                .truncate()
                .shrink(),
        );
    let title = row()
        .key("title")
        .w_full()
        .gap(8.0)
        .child(el().w(16.0))
        .child(
            text(agent.title.clone())
                .font(if selected {
                    k.ty.text_small_medium
                } else {
                    k.ty.text_small
                })
                .color(if selected { c.text_base } else { c.text_muted })
                .truncate()
                .shrink(),
        );
    let mut item = col()
        .key(agent.key.element_id("agent"))
        .role("listitem")
        .label(format!("{}, {}", agent.title, agent.status.label()))
        .w_full()
        .px(8.0)
        .py(4.0)
        .radius(k.radius.md)
        .focusable(true)
        .focus_selected(selected)
        .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.open_thread(key.clone(), tx))
        .child(place)
        .child(title);
    item = k.interactive(item);
    if selected {
        item = item.fill(c.pressed);
    }
    item.role("listitem")
}
