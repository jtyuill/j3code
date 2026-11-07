//! The new-thread surface (spec §8.1): environment and project, a composer, and launching
//! the thread with its first message; the launched thread opens with its composer focused.
//!
//! The Environment select lists distinct environments only; the Project picker is
//! searchable and disabled while the environment has none, when the composer's place says
//! how to add one. A composer (and its draft) belongs to one environment and project.

use rune::{Cx, El, Tx, col, row, text};
use rune_kit::{Combobox, ComboboxOption, ComboboxState, Select, SelectAppearance};

use crate::{
    actions::{Action, shortcut_label},
    app::{J3, Main, Ui},
    composer::ComposerKey,
    environment::LOCAL_KEY,
    sidebar::ThreadKey,
};

/// Where the next thread starts, and the pickers choosing it.
#[derive(Debug, Clone)]
pub struct NewThreadState {
    pub env: String,
    pub project: Option<String>,
    /// The Environment listbox is open.
    pub env_open: bool,
    pub project_picker: ComboboxState,
    /// `project` has been listed by its environment (so its absence means it went away).
    pub project_seen: bool,
}

impl Default for NewThreadState {
    fn default() -> Self {
        Self {
            env: LOCAL_KEY.to_owned(),
            project: None,
            env_open: false,
            project_picker: ComboboxState::default(),
            project_seen: false,
        }
    }
}

impl NewThreadState {
    pub fn composer_key(&self) -> ComposerKey {
        ComposerKey::NewThread {
            env: self.env.clone(),
            project: self.project.clone(),
        }
    }
}

impl J3 {
    /// Shows the new-thread surface for `env` and `project` (the environment's first project
    /// when `None`) with its composer focused.
    pub fn new_thread(&mut self, env: String, project: Option<String>, tx: &mut Tx) {
        self.close_settings(tx);
        self.new_thread.env = env;
        self.new_thread.project = project;
        self.new_thread.project_seen = false;
        self.new_thread.env_open = false;
        self.new_thread.project_picker.close();
        self.sync_new_thread_project();
        self.route.main = Main::NewThread;
        self.focus_composer(tx);
    }

    /// Starts the next thread on environment `env` instead, on its first project.
    pub fn choose_new_thread_env(&mut self, env: String, tx: &mut Tx) {
        self.new_thread.env_open = false;
        if self.new_thread.env == env {
            return;
        }
        self.new_thread.env = env;
        self.new_thread.project = None;
        self.new_thread.project_seen = false;
        self.sync_new_thread_project();
        self.focus_composer(tx);
    }

    /// Starts the next thread in project `project_id` of the chosen environment.
    pub fn choose_new_thread_project(&mut self, project_id: String, tx: &mut Tx) {
        if self.new_thread.project.as_deref() == Some(project_id.as_str()) {
            return;
        }
        self.new_thread.project = Some(project_id);
        self.focus_composer(tx);
    }

    /// Keeps the chosen project one the environment has: its first one when there is no
    /// choice or the chosen project went away. A chosen project not listed yet (just
    /// created) is waited for. True when the choice changed.
    fn sync_new_thread_project(&mut self) -> bool {
        let target = &mut self.new_thread;
        let Some(env) = self.envs.get(&target.env) else {
            return false;
        };
        let projects = &env.state().shell.projects;
        if let Some(id) = &target.project {
            if projects.iter().any(|p| &p.id == id) {
                target.project_seen = true;
                return false;
            }
            if !target.project_seen {
                return false;
            }
        }
        let first = projects.first().map(|p| p.id.clone());
        target.project_seen = first.is_some();
        if first == target.project {
            return false;
        }
        target.project = first;
        true
    }

    /// While the new-thread surface shows: follows its environment's projects as they load
    /// or change (a new first project takes the composer's focus).
    pub(crate) fn poll_new_thread(&mut self, tx: &mut Tx) -> bool {
        if self.route.main != Main::NewThread || !self.sync_new_thread_project() {
            return false;
        }
        if self.new_thread.project.is_some() && !self.route.settings && self.dialogs.open.is_none()
        {
            self.focus_composer(tx);
        }
        true
    }

    /// Launches a thread on `project` of `env` with the submission of composer `key` (its
    /// text, uploaded images and model) as the first message, then opens it.
    pub fn launch_new_thread(
        &mut self,
        key: ComposerKey,
        env: String,
        project: Option<String>,
        submission: crate::composer::Submission,
        _: &mut Tx,
    ) {
        let Some(project) = project else {
            self.finish_composer(
                &key,
                Err("Add a project to this environment to start a thread.".into()),
            );
            return;
        };
        let Some(environment) = self.envs.get(&env) else {
            self.finish_composer(&key, Err("The environment is gone.".into()));
            return;
        };
        let connection = environment.connection().clone();
        self.spawn(async move {
            let result = connection
                .launch(
                    &project,
                    &submission.text,
                    submission.attachments,
                    submission.model,
                )
                .await;
            move |s: &mut J3, tx: &mut Tx| match result {
                Ok(thread_id) => {
                    s.finish_composer(&key, Ok(()));
                    s.open_thread(ThreadKey::new(env, thread_id), tx);
                    s.focus_composer(tx);
                }
                Err(e) => s.finish_composer(&key, Err(e.to_string())),
            }
        });
    }
}

/// The new-thread surface: where it starts, then the composer (or how to add a project).
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui) -> El {
    let target = &s.new_thread;
    let pickers = pickers(s, ui);
    let k = ui.k;
    let body = match &target.project {
        Some(_) => crate::composer::view(s, cx, ui, &target.composer_key()),
        None => row()
            .key("new-thread-empty")
            .w_full()
            .padding(16.0)
            .radius(12.0)
            .border(1.0, k.c.border_base)
            .child(
                text(format!(
                    "Add a project to this environment to start a thread ({}).",
                    shortcut_label(Action::AddProject)
                ))
                .font(k.ty.text_small)
                .color(k.c.text_muted)
                .wrap(),
            ),
    };
    col()
        .key("new-thread-view")
        .w_full()
        .h_full()
        .justify_end()
        .child(
            col()
                .key("new-thread-bottom")
                .w_full()
                // The composer is the measure wide; the side padding sits outside it.
                .max_w(ui.m.measure + 32.0)
                .self_center()
                .gap(8.0)
                .px(16.0)
                .pt(16.0)
                .pb(12.0)
                .child(pickers)
                .child(body),
        )
}

/// "Environment" (distinct environments) and "Project" (searchable; disabled when the
/// environment has none).
fn pickers(s: &J3, ui: &mut Ui) -> El {
    let k = ui.k;
    let target = &s.new_thread;
    let envs = s.envs.distinct();
    let labels: Vec<&str> = envs.iter().map(|env| env.label()).collect();
    let current = s.envs.get(&target.env).map(|env| env.label());
    let environment = Select::new("new-thread-environment", &labels)
        .appearance(SelectAppearance::Field)
        .width(ui.m.rems(12.0))
        .label("Environment")
        .current(current)
        .open(target.env_open)
        .on_toggle(|s: &mut J3| s.new_thread.env_open = !s.new_thread.env_open)
        .on_close(|s: &mut J3| s.new_thread.env_open = false)
        .on_choose(|s: &mut J3, label: &str, tx: &mut Tx| {
            let chosen = s
                .envs
                .distinct()
                .into_iter()
                .find(|env| env.label() == label)
                .map(|env| env.key().to_owned());
            match chosen {
                Some(env) => s.choose_new_thread_env(env, tx),
                None => s.new_thread.env_open = false,
            }
        })
        .build(k, ui.layer);
    let projects: Vec<ComboboxOption> = s
        .envs
        .get(&target.env)
        .map(|env| {
            env.state()
                .shell
                .projects
                .iter()
                .map(|project| {
                    let mut option = ComboboxOption::new(&project.id, &project.title);
                    option.detail = project.workspace_root.clone();
                    option
                })
                .collect()
        })
        .unwrap_or_default();
    let project = Combobox::new(
        "new-thread-project",
        &target.project_picker,
        &projects,
        project_picker,
    )
    .label("Project")
    .current(target.project.as_deref())
    .placeholder("No projects")
    .search_placeholder("Search projects")
    .width(ui.m.rems(16.0))
    .disabled(projects.is_empty())
    .on_choose(|s: &mut J3, id: &str, tx: &mut Tx| s.choose_new_thread_project(id.to_owned(), tx))
    .build(k, ui.layer);
    let label = |line: &str| {
        text(line.to_owned())
            .font(k.ty.text_xsmall)
            .color(k.c.text_muted)
    };
    row()
        .key("new-thread-target")
        .w_full()
        .gap(8.0)
        .center_y()
        .justify_center()
        .child(label("Environment"))
        .child(environment)
        .child(label("Project"))
        .child(project)
}

fn project_picker(s: &mut J3) -> &mut ComboboxState {
    &mut s.new_thread.project_picker
}
