//! The navigation sidebar: environments → projects → threads, as one tree
//! with keyboard navigation, per-row actions and context menus.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    list::ListItem,
    menu::{PopupMenu, PopupMenuItem},
    spinner::Spinner,
    tooltip::Tooltip,
    tree::{Tree, TreeEntry, TreeEvent, TreeItem, TreeState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollStrategy, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, div, rems,
};
use j3_client::ThreadActivity;
use time::OffsetDateTime;

use crate::{environment::Environment, feed::relative_age};

/// A request from the sidebar; the workspace carries it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarEvent {
    OpenThread { env: SharedString, thread_id: String },
    NewThread { env: SharedString, project_id: String },
    AddProject { env: SharedString },
    RenameThread { env: SharedString, thread_id: String },
    ArchiveThread { env: SharedString, thread_id: String },
    DeleteThread { env: SharedString, thread_id: String },
    DeleteProject { env: SharedString, project_id: String },
    ShowArchived { env: SharedString },
    RemoveEnvironment { env: SharedString },
    AddEnvironment,
}

/// What a tree row stands for, with the values its row shows.
#[derive(Debug, Clone, PartialEq)]
enum Node {
    Environment {
        env: SharedString,
        local: bool,
        status: Option<&'static str>,
        detail: SharedString,
    },
    Project {
        env: SharedString,
        project_id: String,
        root: SharedString,
    },
    Thread {
        env: SharedString,
        thread_id: String,
        activity: ThreadActivity,
        age: Option<SharedString>,
    },
    Note,
}

/// Tree item ids; `|` cannot occur in environment keys' meaning here
/// because ids are only ever parsed through [`Node`].
pub fn environment_node_id(env: &str) -> SharedString {
    format!("e|{env}").into()
}

pub fn project_node_id(env: &str, project_id: &str) -> SharedString {
    format!("p|{env}|{project_id}").into()
}

pub fn thread_node_id(env: &str, thread_id: &str) -> SharedString {
    format!("t|{env}|{thread_id}").into()
}

/// The environment and project a sidebar selection points into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionContext {
    pub env: SharedString,
    pub project_id: Option<String>,
    pub thread_id: Option<String>,
}

pub struct Sidebar {
    tree: Entity<TreeState>,
    environments: Vec<Entity<Environment>>,
    nodes: Rc<HashMap<SharedString, Node>>,
    /// Thread node → its project, for [`Sidebar::selection_context`].
    thread_projects: HashMap<SharedString, String>,
    collapsed: HashSet<SharedString>,
    selected: Option<SharedString>,
    _environment_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Sidebar {
    pub fn new(environments: Vec<Entity<Environment>>, cx: &mut Context<Self>) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let subscriptions = vec![
            cx.subscribe(&tree, |this, _, event: &TreeEvent, _| match event {
                TreeEvent::Expanded(id) => {
                    this.collapsed.remove(id);
                }
                TreeEvent::Collapsed(id) => {
                    this.collapsed.insert(id.clone());
                }
            }),
            cx.observe(&tree, |this, tree, cx| this.on_tree_changed(&tree, cx)),
        ];
        let mut sidebar = Self {
            tree,
            environments: Vec::new(),
            nodes: Rc::default(),
            thread_projects: HashMap::new(),
            collapsed: HashSet::new(),
            selected: None,
            _environment_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        sidebar.set_environments(environments, cx);
        sidebar
    }

    pub fn set_environments(&mut self, environments: Vec<Entity<Environment>>, cx: &mut Context<Self>) {
        self._environment_subscriptions = environments
            .iter()
            .map(|env| cx.observe(env, |this, _, cx| this.rebuild(cx)))
            .collect();
        self.environments = environments;
        self.rebuild(cx);
    }

    /// Highlights a thread (expanding its ancestors) without emitting.
    pub fn select_thread(&mut self, env: &str, thread_id: &str, cx: &mut Context<Self>) {
        let id = thread_node_id(env, thread_id);
        self.selected = Some(id.clone());
        self.tree.update(cx, |tree, cx| {
            tree.reveal_item(&id, ScrollStrategy::Center, cx);
            let ix = tree.index_of(&id);
            tree.set_selected_index(ix, cx);
        });
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.tree
            .update(cx, |tree, cx| tree.set_selected_index(None, cx));
    }

    /// Where a new thread or project would go given the current selection.
    pub fn selection_context(&self) -> Option<SelectionContext> {
        let id = self.selected.as_ref()?;
        Some(match self.nodes.get(id)? {
            Node::Environment { env, .. } => SelectionContext {
                env: env.clone(),
                project_id: None,
                thread_id: None,
            },
            Node::Project {
                env, project_id, ..
            } => SelectionContext {
                env: env.clone(),
                project_id: Some(project_id.clone()),
                thread_id: None,
            },
            Node::Thread { env, thread_id, .. } => SelectionContext {
                env: env.clone(),
                project_id: self.thread_projects.get(id).cloned(),
                thread_id: Some(thread_id.clone()),
            },
            Node::Note => return None,
        })
    }

    /// The thread `delta` rows away from the selection, among visible
    /// thread rows, wrapping around.
    pub fn adjacent_thread(&self, delta: isize, cx: &App) -> Option<(SharedString, String)> {
        let tree = self.tree.read(cx);
        let threads: Vec<(usize, &Node)> = (0..)
            .map_while(|ix| tree.entry(ix).map(|entry| (ix, entry)))
            .filter_map(|(ix, entry)| match self.nodes.get(&entry.item().id) {
                Some(node @ Node::Thread { .. }) => Some((ix, node)),
                _ => None,
            })
            .collect();
        if threads.is_empty() {
            return None;
        }
        let current = tree.selected_index();
        let position = current.and_then(|ix| threads.iter().position(|(i, _)| *i == ix));
        let next = match position {
            Some(pos) => (pos as isize + delta).rem_euclid(threads.len() as isize) as usize,
            None if delta >= 0 => 0,
            None => threads.len() - 1,
        };
        match threads[next].1 {
            Node::Thread { env, thread_id, .. } => Some((env.clone(), thread_id.clone())),
            _ => None,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.tree.update(cx, |tree, cx| tree.focus(window, cx));
    }

    fn on_tree_changed(&mut self, tree: &Entity<TreeState>, cx: &mut Context<Self>) {
        let selected = tree.read(cx).selected_item().map(|item| item.id.clone());
        if selected == self.selected {
            return;
        }
        self.selected = selected.clone();
        if let Some(Node::Thread { env, thread_id, .. }) =
            selected.as_ref().and_then(|id| self.nodes.get(id))
        {
            cx.emit(SidebarEvent::OpenThread {
                env: env.clone(),
                thread_id: thread_id.clone(),
            });
        }
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let now = OffsetDateTime::now_utc();
        let mut nodes = HashMap::new();
        let mut thread_projects = HashMap::new();
        let mut roots = Vec::with_capacity(self.environments.len());
        for environment in &self.environments {
            let environment = environment.read(cx);
            let key = environment.key().clone();
            let env_id = environment_node_id(&key);
            let shell = &environment.state().shell;
            let mut children = Vec::new();
            if !shell.loaded {
                let id: SharedString = format!("n|{key}").into();
                nodes.insert(id.clone(), Node::Note);
                let text = environment.status_label().unwrap_or("Loading…");
                children.push(TreeItem::new(id, text));
            } else if shell.projects.is_empty() {
                let id: SharedString = format!("n|{key}").into();
                nodes.insert(id.clone(), Node::Note);
                children.push(TreeItem::new(id, "No projects"));
            }
            for project in &shell.projects {
                let project_id = project_node_id(&key, &project.id);
                let threads = shell.threads_of(&project.id);
                let thread_items: Vec<TreeItem> = threads
                    .iter()
                    .map(|thread| {
                        let id = thread_node_id(&key, &thread.id);
                        nodes.insert(
                            id.clone(),
                            Node::Thread {
                                env: key.clone(),
                                thread_id: thread.id.clone(),
                                activity: ThreadActivity::of(thread),
                                age: relative_age(&thread.updated_at, now).map(Into::into),
                            },
                        );
                        thread_projects.insert(id.clone(), project.id.clone());
                        let title = if thread.title.trim().is_empty() {
                            "Untitled"
                        } else {
                            thread.title.as_str()
                        };
                        TreeItem::new(id, title.to_owned())
                    })
                    .collect();
                nodes.insert(
                    project_id.clone(),
                    Node::Project {
                        env: key.clone(),
                        project_id: project.id.clone(),
                        root: project.workspace_root.clone().into(),
                    },
                );
                let expanded = !self.collapsed.contains(&project_id);
                children.push(
                    TreeItem::new(project_id, project.title.clone())
                        .children(thread_items)
                        .expanded(expanded),
                );
            }
            nodes.insert(
                env_id.clone(),
                Node::Environment {
                    env: key.clone(),
                    local: environment.is_local(),
                    status: environment.status_label(),
                    detail: environment.status_detail().into(),
                },
            );
            let expanded = !self.collapsed.contains(&env_id);
            roots.push(
                TreeItem::new(env_id, environment.label().clone())
                    .children(children)
                    .expanded(expanded),
            );
        }
        self.nodes = Rc::new(nodes);
        self.thread_projects = thread_projects;
        let selected = self.selected.clone();
        self.tree.update(cx, |tree, cx| {
            tree.set_items(roots, cx);
            if let Some(id) = selected {
                let ix = tree.index_of(&id);
                tree.set_selected_index(ix, cx);
            }
        });
        cx.notify();
    }

    fn emit_later(weak: &WeakEntity<Self>, event: SidebarEvent, cx: &mut App) {
        let _ = weak.update(cx, |_, cx| cx.emit(event));
    }

    fn render_row(
        weak: &WeakEntity<Self>,
        nodes: &HashMap<SharedString, Node>,
        ix: usize,
        entry: &TreeEntry,
        selected: bool,
        cx: &mut App,
    ) -> ListItem {
        let item = entry.item();
        let node = nodes.get(&item.id).cloned().unwrap_or(Node::Note);
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let indent = div()
            .flex_shrink_0()
            .w(rems(0.875 * entry.depth() as f32));
        let disclosure = div().flex_shrink_0().size_4().when(entry.is_folder(), |this| {
            this.child(
                Icon::new(if entry.is_expanded() {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .xsmall()
                .text_color(muted),
            )
        });
        let label = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .child(item.label.clone());
        let row = ListItem::new(ix)
            .selected(selected)
            .px_2()
            .py_0p5()
            .text_sm()
            .rounded(theme.radius)
            .accessibility_label(item.label.clone());
        match node {
            Node::Environment {
                env,
                local,
                status,
                detail,
            } => {
                let weak = weak.clone();
                row.child(
                    h_flex()
                        .id(SharedString::from(format!("env-row-{env}")))
                        .gap_1()
                        .child(disclosure)
                        .child(
                            Icon::new(if local {
                                IconName::HardDrive
                            } else {
                                IconName::Globe
                            })
                            .small()
                            .text_color(muted),
                        )
                        .child(label.font_medium())
                        .when_some(status, |this, status| {
                            this.child(div().text_xs().text_color(muted).child(status))
                        })
                        .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx)),
                )
                .suffix(move |_, _| {
                    let weak = weak.clone();
                    let env = env.clone();
                    Button::new(SharedString::from(format!("add-project-{env}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("Add project…")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            Self::emit_later(&weak, SidebarEvent::AddProject { env: env.clone() }, cx);
                        })
                })
            }
            Node::Project {
                env,
                project_id,
                root,
            } => {
                let weak = weak.clone();
                row.child(
                    h_flex()
                        .id(SharedString::from(format!("project-row-{env}-{project_id}")))
                        .gap_1()
                        .child(indent)
                        .child(disclosure)
                        .child(
                            Icon::new(if entry.is_expanded() {
                                IconName::FolderOpen
                            } else {
                                IconName::Folder
                            })
                            .small()
                            .text_color(muted),
                        )
                        .child(label)
                        .tooltip(move |window, cx| Tooltip::new(root.clone()).build(window, cx)),
                )
                .suffix(move |_, _| {
                    let weak = weak.clone();
                    let event = SidebarEvent::NewThread {
                        env: env.clone(),
                        project_id: project_id.clone(),
                    };
                    Button::new(SharedString::from(format!("new-thread-{env}-{project_id}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("New thread")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            Self::emit_later(&weak, event.clone(), cx);
                        })
                })
            }
            Node::Thread { activity, age, .. } => {
                let indicator = div()
                    .flex_shrink_0()
                    .size_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|this| match activity {
                        ThreadActivity::Running => this.child(Spinner::new().xsmall()),
                        ThreadActivity::WaitingForInput => this.child(
                            Icon::new(IconName::Bell)
                                .xsmall()
                                .text_color(theme.warning),
                        ),
                        ThreadActivity::Error => this.child(
                            Icon::new(IconName::CircleAlert)
                                .xsmall()
                                .text_color(theme.danger),
                        ),
                        ThreadActivity::Idle => this,
                    });
                let status = match activity {
                    ThreadActivity::Running => "Running",
                    ThreadActivity::WaitingForInput => "Waiting for input",
                    ThreadActivity::Error => "Error",
                    ThreadActivity::Idle => "Idle",
                };
                row.accessibility_label(format!("{}, {status}", item.label))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(indent)
                            .child(indicator)
                            .child(label)
                            .when_some(age, |this, age| {
                                this.child(div().flex_shrink_0().text_xs().text_color(muted).child(age))
                            }),
                    )
            }
            Node::Note => row.disabled(true).child(
                h_flex()
                    .gap_1()
                    .child(indent)
                    .child(div().size_4())
                    .child(label.text_xs().text_color(muted)),
            ),
        }
    }

    fn context_menu(
        weak: &WeakEntity<Self>,
        nodes: &HashMap<SharedString, Node>,
        entry: &TreeEntry,
        menu: PopupMenu,
    ) -> PopupMenu {
        let item = |label: &'static str, event: SidebarEvent| {
            let weak = weak.clone();
            PopupMenuItem::new(label).on_click(move |_, _, cx| {
                Self::emit_later(&weak, event.clone(), cx);
            })
        };
        match nodes.get(&entry.item().id) {
            Some(Node::Environment { env, local, .. }) => {
                let menu = menu
                    .item(item("Add Project…", SidebarEvent::AddProject { env: env.clone() }))
                    .item(item(
                        "Archived Threads",
                        SidebarEvent::ShowArchived { env: env.clone() },
                    ));
                if *local {
                    menu
                } else {
                    menu.separator().item(item(
                        "Remove Environment",
                        SidebarEvent::RemoveEnvironment { env: env.clone() },
                    ))
                }
            }
            Some(Node::Project {
                env, project_id, ..
            }) => menu
                .item(item(
                    "New Thread",
                    SidebarEvent::NewThread {
                        env: env.clone(),
                        project_id: project_id.clone(),
                    },
                ))
                .separator()
                .item(item(
                    "Remove Project…",
                    SidebarEvent::DeleteProject {
                        env: env.clone(),
                        project_id: project_id.clone(),
                    },
                )),
            Some(Node::Thread { env, thread_id, .. }) => {
                let thread = |make: fn(SharedString, String) -> SidebarEvent| {
                    make(env.clone(), thread_id.clone())
                };
                menu.item(item(
                    "Rename…",
                    thread(|env, thread_id| SidebarEvent::RenameThread { env, thread_id }),
                ))
                .item(item(
                    "Archive",
                    thread(|env, thread_id| SidebarEvent::ArchiveThread { env, thread_id }),
                ))
                .separator()
                .item(item(
                    "Delete…",
                    thread(|env, thread_id| SidebarEvent::DeleteThread { env, thread_id }),
                ))
            }
            Some(Node::Note) | None => menu,
        }
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        let nodes = self.nodes.clone();
        let menu_weak = weak.clone();
        let menu_nodes = self.nodes.clone();
        let add_weak = weak.clone();
        let theme = cx.theme();
        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h_9()
                    .px_3()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        div()
                            .text_xs()
                            .font_medium()
                            .text_color(theme.muted_foreground)
                            .child("Environments"),
                    )
                    .child(
                        Button::new("add-environment")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .tooltip("Add environment…")
                            .on_click(move |_, _, cx| {
                                Self::emit_later(&add_weak, SidebarEvent::AddEnvironment, cx);
                            }),
                    ),
            )
            .child(
                div().flex_1().min_h_0().px_1().py_1().child(
                    Tree::new(&self.tree, move |ix, entry, selected, _, cx| {
                        Self::render_row(&weak, &nodes, ix, entry, selected, cx)
                    })
                    .context_menu(move |_, entry, menu, _, _| {
                        Self::context_menu(&menu_weak, &menu_nodes, entry, menu)
                    }),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_ids_are_namespaced_per_environment() {
        assert_eq!(environment_node_id("local"), "e|local");
        assert_eq!(
            project_node_id("http://shieldy:3780", "p1"),
            "p|http://shieldy:3780|p1"
        );
        assert_ne!(thread_node_id("local", "t1"), thread_node_id("http://x:1", "t1"));
    }
}
