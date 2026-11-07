//! The new-thread surface (spec §8.1): environment and project, a composer, and launching
//! the thread with its first message; the launched thread opens with its composer focused.

use rune::{Cx, El, Tx, col, text};

use crate::{
    app::{J3, Main, Ui},
    composer::ComposerKey,
    environment::LOCAL_KEY,
    sidebar::ThreadKey,
};

/// Where the next thread starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewThreadState {
    pub env: String,
    pub project: Option<String>,
}

impl Default for NewThreadState {
    fn default() -> Self {
        Self {
            env: LOCAL_KEY.to_owned(),
            project: None,
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
        let project = project.or_else(|| {
            self.envs
                .get(&env)
                .and_then(|e| e.state().shell.projects.first())
                .map(|p| p.id.clone())
        });
        self.new_thread = NewThreadState { env, project };
        self.route.main = Main::NewThread;
        self.focus_composer(tx);
    }

    /// Launches a thread on the chosen project with the composer's submission (its text,
    /// uploaded images and model) as the first message, then opens it.
    pub fn launch_new_thread(&mut self, submission: crate::composer::Submission, _: &mut Tx) {
        let key = self.new_thread.composer_key();
        let env = self.new_thread.env.clone();
        let Some(project) = self.new_thread.project.clone() else {
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
        self.composer(&key).busy = true;
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

/// The new-thread surface: where it starts, then the composer.
pub fn view(s: &J3, cx: &mut Cx<J3>, ui: &mut Ui) -> El {
    let k = ui.k;
    let target = s.new_thread.clone();
    let env = s.envs.get(&target.env);
    let place = match (env, &target.project) {
        (Some(env), Some(project)) => {
            let title = env
                .state()
                .shell
                .project(project)
                .map_or(project.as_str(), |p| p.title.as_str());
            format!("{title} · {}", env.label())
        }
        _ => "Add a project to this environment to start a thread".to_owned(),
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
                .max_w(ui.m.measure)
                .self_center()
                .gap(8.0)
                .px(16.0)
                .pt(16.0)
                .pb(12.0)
                .child(
                    text(place)
                        .key("new-thread-target")
                        .font(k.ty.text_small)
                        .color(k.c.text_muted),
                )
                .child(crate::composer::view(s, cx, ui, &target.composer_key())),
        )
}
