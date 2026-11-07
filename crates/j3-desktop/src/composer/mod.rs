//! The composer (spec §7): the message draft of one thread or of the new-thread surface.
//! Multi-line text, pasted or dropped images (uploaded as they are added), one model and
//! thinking picker, the usage gauge, `/` and `^` completion, `^model` mentions shown as chips
//! (the submitted text stays the typed `^slug`), magic keywords painted with omp's gradient,
//! omp's word prediction as ghost text and dictation into the draft. Submitting sends to the
//! thread (queued behind an active run) or launches the new thread.
//!
//! The pieces live in submodules by capability: [`draft`] (editing rules), [`menu`]
//! (completions), [`images`], [`picker`], [`usage`], [`mic`] and [`view`].

mod draft;
pub mod fixture;
mod images;
mod menu;
mod mic;
mod picker;
mod usage;
mod view;

#[cfg(test)]
mod tests;

use j3_client::SendMode;
use j3_client::t3::{chat_attachment::ChatAttachment, model::ModelSelection};
use rune::{TextEdit, Tx};

#[cfg(test)]
pub(crate) use draft::keyword_tokens;
pub use draft::plain_text;
pub use view::view;

use crate::{
    app::{J3, Main},
    completion::{Catalog, Completion},
    environment::Environment,
    hub::DoubleTap,
    models::{self, ModelChoice},
    prediction::{Ghost, Provisional},
    sidebar::ThreadKey,
};
use images::PendingImage;
use mic::Dictation;
use picker::PickerState;

/// Whose draft: an open thread's, or the new-thread surface's for one environment and
/// project (a composer is reused only for the same pair).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ComposerKey {
    Thread(ThreadKey),
    NewThread {
        env: String,
        project: Option<String>,
    },
}

impl ComposerKey {
    /// The element key of this composer's text field.
    pub fn input_key(&self) -> String {
        match self {
            Self::Thread(key) => key.element_id("composer-input"),
            Self::NewThread { env, project } => {
                format!(
                    "composer-input-new-{env}-{}",
                    project.as_deref().unwrap_or("")
                )
            }
        }
    }

    /// The environment the draft goes to.
    pub fn env(&self) -> &str {
        match self {
            Self::Thread(key) => &key.env,
            Self::NewThread { env, .. } => env,
        }
    }
}

/// What a submit hands its destination: the trimmed text (mentions as typed `^slug`), the
/// uploaded images and the chosen model with its thinking level.
#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub model: ModelSelection,
}

/// One composer's state.
#[derive(Default)]
pub struct ComposerState {
    pub edit: TextEdit,
    /// Why the last submit was refused or failed, or a dictation note.
    pub error: Option<String>,
    /// A send is on its way.
    pub busy: bool,
    pub images: Vec<PendingImage>,
    next_image: u64,
    /// The model the user picked; `None` follows the thread's or project's preference.
    model: Option<(String, String)>,
    /// The thinking level the user picked.
    level: Option<String>,
    picker: PickerState,
    /// The usage popover is open.
    usage_open: bool,
    /// The environment this composer's usage button watches.
    usage_watch: Option<String>,
    /// The `/` or `^` completion menu's rows (at most 32) and its selection.
    pub completions: Vec<Completion>,
    pub completion_ix: usize,
    /// omp's suggestion for the rest of the word at the caret.
    pub ghost: Option<Ghost>,
    /// The draft right after a Tab accept, while its space is provisional.
    provisional: Option<Provisional>,
    /// Prediction requests on their way.
    predicting: usize,
    /// The capture dictating into this draft.
    dictation: Option<Dictation>,
    /// Send was pressed while dictating: the draft sends when the capture ends.
    send_after_dictation: bool,
    /// ← taps in the empty draft.
    left: DoubleTap,
    /// The field's focus as the last poll saw it.
    focused: bool,
    /// When (`Clock::t`) the keyword shimmer repaints next, while it runs.
    shimmer_at: Option<f64>,
    /// The workspace's commands were asked for.
    workspace_requested: bool,
}

/// What a composer works against, resolved from its key.
pub(crate) struct Target<'a> {
    pub env: Option<&'a Environment>,
    /// The project's workspace root (commands and skills are per workspace).
    pub cwd: String,
    /// The thread's model, or the project's default for a new thread.
    pub preferred: Option<ModelSelection>,
    /// The thread has an active run (Send becomes Queue or Stop).
    pub running: bool,
}

/// A model's thinking levels.
pub(crate) struct Levels {
    /// The option descriptor's id (`thinking`).
    pub descriptor: String,
    /// `(id, label)` per level.
    pub options: Vec<(String, String)>,
    /// The level the next submission carries.
    pub chosen: Option<String>,
}

impl J3 {
    /// The composer for `key`, created on first use.
    pub fn composer(&mut self, key: &ComposerKey) -> &mut ComposerState {
        self.composers.entry(key.clone()).or_default()
    }

    /// The main pane's composer (its thread's, or the new-thread surface's when it has a
    /// project), also while Settings or the agents hub covers it.
    pub fn main_composer_key(&self) -> Option<ComposerKey> {
        match &self.route.main {
            Main::Thread(thread) => Some(ComposerKey::Thread(thread.clone())),
            Main::NewThread if self.new_thread.project.is_some() => {
                Some(self.new_thread.composer_key())
            }
            Main::NewThread | Main::Empty => None,
        }
    }

    /// Moves keyboard focus to the main pane's composer.
    pub fn focus_composer(&mut self, _: &mut Tx) {
        if let Some(key) = self.main_composer_key() {
            self.composer(&key).edit.focused = true;
        }
    }

    /// The environment, workspace, preferred model and run state of `key`'s composer.
    pub(crate) fn composer_target(&self, key: &ComposerKey) -> Target<'_> {
        let env = self.envs.get(key.env());
        let shell = env.map(|env| &env.state().shell);
        match key {
            ComposerKey::Thread(thread) => {
                let listed = shell.and_then(|shell| shell.thread(&thread.thread_id));
                let cwd = listed
                    .and_then(|t| shell?.project(&t.project_id))
                    .map(|p| p.workspace_root.clone())
                    .unwrap_or_default();
                let running = self
                    .threads
                    .get(thread)
                    .is_some_and(|t| t.state.active_run().is_some());
                Target {
                    env,
                    cwd,
                    preferred: listed.map(|t| t.model_selection.clone()),
                    running,
                }
            }
            ComposerKey::NewThread { project, .. } => {
                let project = project.as_deref().and_then(|id| shell?.project(id));
                Target {
                    env,
                    cwd: project
                        .map(|p| p.workspace_root.clone())
                        .unwrap_or_default(),
                    preferred: project.and_then(|p| p.default_model_selection.clone()),
                    running: false,
                }
            }
        }
    }

    /// The model `key`'s composer sends with: the one picked, else the preferred one, else
    /// the server's default.
    pub(crate) fn composer_choice(&self, key: &ComposerKey) -> Option<&ModelChoice> {
        let target = self.composer_target(key);
        let choices = target.env?.models();
        let picked = self
            .composers
            .get(key)
            .and_then(|c| c.model.as_ref())
            .and_then(|picked| choices.iter().find(|c| &c.key == picked));
        match picked {
            Some(choice) => Some(choice),
            None => choices.get(models::initial_choice(choices, target.preferred.as_ref())?),
        }
    }

    /// The chosen model's thinking levels and the chosen level.
    pub(crate) fn composer_levels(&self, key: &ComposerKey) -> Option<Levels> {
        let choice = self.composer_choice(key)?;
        let target = self.composer_target(key);
        let config = target.env?.state().config.as_ref()?;
        let descriptor = models::thinking_descriptor(config, choice)?;
        let picked = self.composers.get(key).and_then(|c| c.level.clone());
        let preferred = picked.or_else(|| {
            let preferred = target.preferred.as_ref()?;
            models::selection_level(preferred, &descriptor.id).map(str::to_owned)
        });
        Some(Levels {
            descriptor: descriptor.id.clone(),
            options: descriptor
                .options
                .iter()
                .map(|option| (option.id.clone(), option.label.clone()))
                .collect(),
            chosen: models::initial_level(descriptor, preferred.as_deref()),
        })
    }

    /// The model selection a submit carries, with the chosen thinking level.
    pub fn composer_selection(&self, key: &ComposerKey) -> Option<ModelSelection> {
        let choice = self.composer_choice(key)?;
        let levels = self.composer_levels(key);
        let level = levels
            .as_ref()
            .and_then(|l| Some((l.descriptor.as_str(), l.chosen.as_deref()?)));
        Some(models::model_selection(choice, level))
    }

    /// `key`'s commands and skills: the provider-wide ones merged with its workspace's.
    pub(crate) fn composer_catalog(&self, key: &ComposerKey) -> Catalog {
        let target = self.composer_target(key);
        let Some(state) = target.env.map(|env| env.state()) else {
            return Catalog::default();
        };
        let Some(config) = state.config.as_ref() else {
            return Catalog::default();
        };
        let mut catalog = Catalog::for_workspace(config, &target.cwd);
        if let Some(fetched) = state.workspaces.get(&target.cwd) {
            catalog.merge(&fetched.slash_commands, &fetched.skills);
        }
        catalog
    }

    /// Asks the server for the workspace's commands once, when it has none yet: as soon as
    /// the environment is live (polled while the composer is the main one).
    fn request_workspace(&mut self, key: &ComposerKey) {
        if self
            .composers
            .get(key)
            .is_some_and(|c| c.workspace_requested)
            || !self.envs.get(key.env()).is_some_and(|env| env.is_live())
        {
            return;
        }
        let target = self.composer_target(key);
        let Some(env) = target.env else {
            return;
        };
        let state = env.state();
        let Some(config) = state.config.as_ref() else {
            return;
        };
        let cwd = target.cwd;
        let known = cwd.is_empty()
            || state.workspaces.contains_key(&cwd)
            || Catalog::for_workspace(config, &cwd).has_workspace;
        let connection = env.connection().clone();
        self.composer(key).workspace_requested = true;
        if known {
            return;
        }
        self.services.runtime.spawn(async move {
            if let Err(e) = connection.refresh_workspace(&cwd).await {
                tracing::warn!("loading commands for {cwd}: {e}");
            }
        });
    }

    /// Whether `key`'s composer has anything to send: text or an image.
    pub(crate) fn composer_has_draft(&self, key: &ComposerKey) -> bool {
        self.composers
            .get(key)
            .is_some_and(|c| !c.edit.text.trim().is_empty() || !c.images.is_empty())
    }

    /// Sends `key`'s draft (Enter, the Send/Queue button): to its thread, queued behind an
    /// active run, or as the first message of a new thread. Refused while sending or
    /// dictating, with nothing to send, while an image uploads or after one failed, and
    /// without a model.
    pub fn submit_composer(&mut self, key: &ComposerKey, tx: &mut Tx) {
        let Some(composer) = self.composers.get(key) else {
            return;
        };
        if composer.busy || composer.dictation.is_some() {
            return;
        }
        let text = plain_text(&composer.edit).trim().to_owned();
        if text.is_empty() && composer.images.is_empty() {
            return;
        }
        let attachments = match images::attachments(&composer.images) {
            Ok(attachments) => attachments,
            Err(refusal) => {
                self.composer(key).error = Some(refusal.to_owned());
                return;
            }
        };
        let Some(model) = self.composer_selection(key) else {
            self.composer(key).error = Some("Choose a model.".to_owned());
            return;
        };
        let submission = Submission {
            text,
            attachments,
            model,
        };
        let composer = self.composer(key);
        composer.busy = true;
        composer.error = None;
        composer.completions.clear();
        match key {
            ComposerKey::Thread(thread) => self.send_to_thread(thread.clone(), submission),
            ComposerKey::NewThread { env, project } => {
                self.launch_new_thread(key.clone(), env.clone(), project.clone(), submission, tx)
            }
        }
    }

    /// Sends a submission to `thread`: started, or queued behind its active run.
    fn send_to_thread(&mut self, thread: ThreadKey, submission: Submission) {
        let key = ComposerKey::Thread(thread.clone());
        let Some(connection) = self.envs.get(&thread.env).map(|e| e.connection().clone()) else {
            self.finish_composer(&key, Err("The environment is gone.".to_owned()));
            return;
        };
        let running = self
            .threads
            .get(&thread)
            .is_some_and(|t| t.state.active_run().is_some());
        let mode = if running {
            SendMode::Queue
        } else {
            SendMode::Start
        };
        self.spawn(async move {
            let result = connection
                .send(
                    &thread.thread_id,
                    &submission.text,
                    submission.attachments,
                    Some(submission.model),
                    mode,
                )
                .await
                .map(drop)
                .map_err(|e| e.to_string());
            move |s: &mut J3, _: &mut Tx| s.finish_composer(&key, result)
        });
    }

    /// The last submission of `key` was sent (clears the draft, its images, menu and
    /// prediction) or failed (keeps the draft, shows the error).
    pub fn finish_composer(&mut self, key: &ComposerKey, result: Result<(), String>) {
        let composer = self.composer(key);
        composer.busy = false;
        match result {
            Ok(()) => {
                composer.error = None;
                composer.images.clear();
                composer.completions.clear();
                composer.ghost = None;
                composer.provisional = None;
                composer.edit.clear();
            }
            Err(error) => composer.error = Some(error),
        }
    }

    /// Drops the composers whose environment, thread or project is gone (never the main
    /// one, nor one still sending), releasing their images, usage watches and microphone.
    /// Runs when an environment changes or goes.
    pub(crate) fn prune_composers(&mut self, tx: &mut Tx) {
        let main = self.main_composer_key();
        let gone: Vec<ComposerKey> = self
            .composers
            .iter()
            .filter(|(key, c)| Some(*key) != main.as_ref() && !c.busy && self.composer_gone(key))
            .map(|(key, _)| key.clone())
            .collect();
        for key in gone {
            let Some(composer) = self.composers.remove(&key) else {
                continue;
            };
            if let Some(env) = composer.usage_watch {
                self.watch_usage(&env, false, tx);
            }
            if self
                .speech
                .capture()
                .is_some_and(|(owner, _)| *owner == key)
            {
                self.cancel_dictation(tx);
            }
        }
    }

    /// Whether `key`'s environment is gone, or its thread or project is no longer listed by
    /// its (live, so up-to-date) environment.
    fn composer_gone(&self, key: &ComposerKey) -> bool {
        let Some(env) = self.envs.get(key.env()) else {
            return true;
        };
        if !env.is_live() {
            return false;
        }
        let shell = &env.state().shell;
        match key {
            ComposerKey::Thread(thread) => shell.thread(&thread.thread_id).is_none(),
            ComposerKey::NewThread { project, .. } => project
                .as_deref()
                .is_none_or(|project| shell.project(project).is_none()),
        }
    }

    /// Per frame: the main composer's focus (warming the predictor, the keyword shimmer),
    /// its usage watch, and the microphone, released when its composer leaves the screen.
    /// True when a shimmer frame or a focus change needs a redraw.
    pub(crate) fn poll_composer(&mut self, tx: &mut Tx) -> bool {
        let main = self.main_composer_key();
        let mut changed = false;
        // Usage watches follow the main composer's environment.
        fn wanted<'a>(key: &'a ComposerKey, main: Option<&ComposerKey>) -> Option<&'a str> {
            (Some(key) == main).then(|| key.env())
        }
        let stale: Vec<ComposerKey> = self
            .composers
            .iter()
            .filter(|(key, c)| c.usage_watch.as_deref() != wanted(key, main.as_ref()))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            let watch = wanted(&key, main.as_ref()).map(str::to_owned);
            let old = std::mem::replace(&mut self.composer(&key).usage_watch, watch.clone());
            if let Some(old) = old {
                self.watch_usage(&old, false, tx);
            }
            if let Some(env) = watch {
                self.watch_usage(&env, true, tx);
            }
        }
        if let Some((owner, _)) = self.speech.capture()
            && Some(owner) != main.as_ref()
        {
            self.stop_dictation(tx);
        }
        let Some(key) = main else {
            return changed;
        };
        let now = self.clock.t;
        let composer = self.composer(&key);
        let focused = composer.edit.focused;
        if focused != composer.focused {
            composer.focused = focused;
            changed = true;
            if focused {
                // The first keystroke already gets a prediction.
                self.warm_assist();
            }
        }
        self.request_workspace(&key);
        let composer = self.composer(&key);
        if focused && draft::shows_keyword(&composer.edit) {
            if composer.shimmer_at.is_none_or(|at| now >= at) {
                composer.shimmer_at = Some(now + crate::keywords::SHIMMER_FRAME.as_secs_f64());
                tx.wake_after(crate::keywords::SHIMMER_FRAME);
                changed = true;
            }
        } else if composer.shimmer_at.take().is_some() {
            // Drawn once more, at rest.
            changed = true;
        }
        changed
    }
}
