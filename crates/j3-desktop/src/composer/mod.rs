//! The composer (spec §7): the message draft of one thread or of the new-thread surface.
//! Multi-line text, pasted or dropped images and files (uploaded as they are added), one model and
//! thinking picker, the usage gauge, `/`, `^` and `@` completion, `^model` mentions shown as
//! chips (the submitted text stays the typed `^slug`), magic keywords painted with omp's
//! gradient, omp's word prediction as ghost text and dictation into the draft. Submitting sends
//! to the thread (queued behind an active run) or launches the new thread.
//!
//! The pieces live in submodules by capability: [`draft`] (editing rules), [`menu`]
//! (completions), [`attachments`], [`picker`], [`usage`], [`mic`] and [`view`].

mod attachments;
mod draft;
pub mod fixture;
mod menu;
mod mic;
mod picker;
mod usage;
mod view;

#[cfg(test)]
mod tests;

use j3_client::{
    entities::{ModelRef, ThinkingLevel},
    methods::{Delivery, RefreshParams, SendParams},
};
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
    thread::btw,
};
use attachments::PendingAttachment;
#[cfg(test)]
pub(crate) use attachments::Upload;
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
/// uploads' attachment ids and the chosen model with its thinking level.
#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    pub text: String,
    pub attachments: Vec<String>,
    pub model: ModelRef,
}

/// One composer's state.
#[derive(Default)]
pub struct ComposerState {
    pub edit: TextEdit,
    /// Why the last submit was refused or failed, or a dictation note.
    pub error: Option<String>,
    /// A send is on its way.
    pub busy: bool,
    pub attachments: Vec<PendingAttachment>,
    next_attachment: u64,
    /// The model the user picked, by slug; `None` follows the thread's or project's
    /// preference.
    model: Option<String>,
    /// The thinking level the user picked.
    level: Option<ThinkingLevel>,
    picker: PickerState,
    /// The usage popover is open.
    usage_open: bool,
    /// The environment this composer's usage button watches.
    usage_watch: Option<String>,
    /// The `/`, `^` or `@` completion menu's rows (at most 32) and its selection.
    pub completions: Vec<Completion>,
    pub completion_ix: usize,
    /// The `@` query the menu's paths answer or await; a reply for another is stale.
    path_query: Option<String>,
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
    /// The project's own slash commands were asked for.
    commands_requested: bool,
}

/// What a composer works against, resolved from its key.
pub(crate) struct Target<'a> {
    pub env: Option<&'a Environment>,
    /// The project (slash commands and `@` paths are per project), once listed.
    pub project_id: Option<String>,
    /// The thread's model, or the project's default for a new thread.
    pub preferred: Option<ModelRef>,
    /// The thread has an active turn (Send becomes Queue or Stop).
    pub running: bool,
}

/// A model's thinking levels.
pub(crate) struct Levels {
    /// The levels the model offers.
    pub options: Vec<ThinkingLevel>,
    /// The level the next submission carries; `None` leaves it to omp.
    pub chosen: Option<ThinkingLevel>,
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

    /// The environment, project, preferred model and run state of `key`'s composer.
    pub(crate) fn composer_target(&self, key: &ComposerKey) -> Target<'_> {
        let env = self.envs.get(key.env());
        let shell = env.map(|env| &env.state().shell);
        match key {
            ComposerKey::Thread(thread) => {
                let listed = shell.and_then(|shell| shell.threads.get(&thread.thread_id));
                let running = self
                    .threads
                    .get(thread)
                    .is_some_and(|t| t.state.active_turn().is_some());
                Target {
                    env,
                    project_id: listed.map(|t| t.project_id.clone()),
                    preferred: listed.map(|t| t.model.clone()),
                    running,
                }
            }
            ComposerKey::NewThread { project, .. } => {
                let project = project.as_deref().and_then(|id| shell?.projects.get(id));
                Target {
                    env,
                    project_id: project.map(|p| p.id.clone()),
                    preferred: project.and_then(|p| p.default_model.clone()),
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
            .and_then(|picked| choices.iter().find(|c| c.slug() == picked));
        match picked {
            Some(choice) => Some(choice),
            None => choices.get(models::initial_choice(choices, target.preferred.as_ref())?),
        }
    }

    /// The chosen model's thinking levels and the chosen level: the one picked, else the
    /// preferred model's, when the chosen model offers it. None without levels.
    pub(crate) fn composer_levels(&self, key: &ComposerKey) -> Option<Levels> {
        let choice = self.composer_choice(key)?;
        if choice.thinking.is_empty() {
            return None;
        }
        let picked = self.composers.get(key).and_then(|c| c.level);
        let preferred = self
            .composer_target(key)
            .preferred
            .and_then(|model| model.thinking);
        Some(Levels {
            options: choice.thinking.clone(),
            chosen: models::initial_level(choice, picked)
                .or_else(|| models::initial_level(choice, preferred)),
        })
    }

    /// The model a submit carries, with the chosen thinking level.
    pub fn composer_selection(&self, key: &ComposerKey) -> Option<ModelRef> {
        let choice = self.composer_choice(key)?;
        let level = self.composer_levels(key).and_then(|levels| levels.chosen);
        Some(models::model_ref(choice, level))
    }

    /// `key`'s commands and skills: its project's own commands, then the server's.
    pub(crate) fn composer_catalog(&self, key: &ComposerKey) -> Catalog {
        let target = self.composer_target(key);
        let Some(env) = target.env else {
            return Catalog::default();
        };
        let Some(catalog) = env.state().catalog.as_ref() else {
            return Catalog::default();
        };
        let project = target
            .project_id
            .as_deref()
            .and_then(|id| {
                env.project_commands(id)
                    .or_else(|| catalog.project_commands.get(id).map(Vec::as_slice))
            })
            .unwrap_or_default();
        Catalog::new(catalog, project)
    }

    /// Asks the server for the project's own slash commands once, when they aren't known
    /// yet: as soon as the environment is live (polled while the composer is the main one).
    fn request_project_commands(&mut self, key: &ComposerKey) {
        if self
            .composers
            .get(key)
            .is_some_and(|c| c.commands_requested)
            || !self.envs.get(key.env()).is_some_and(|env| env.is_live())
        {
            return;
        }
        let target = self.composer_target(key);
        let Some(env) = target.env else {
            return;
        };
        let project_id = target.project_id;
        let known = project_id.as_deref().is_none_or(|id| {
            env.project_commands(id).is_some()
                || env
                    .state()
                    .catalog
                    .as_ref()
                    .is_some_and(|catalog| catalog.project_commands.contains_key(id))
        });
        let connection = env.connection().clone();
        self.composer(key).commands_requested = true;
        let (false, Some(project_id)) = (known, project_id) else {
            return;
        };
        let env = key.env().to_owned();
        let params = RefreshParams {
            project_id: Some(project_id.clone()),
            fresh: None,
        };
        self.spawn(async move {
            let result = connection.server_refresh(params).await;
            move |s: &mut J3, _: &mut Tx| match result {
                Ok(catalog) => {
                    if let Some(env) = s.envs.get_mut(&env) {
                        env.set_project_commands(catalog.project_commands);
                    }
                }
                Err(e) => tracing::warn!("loading the commands of project {project_id}: {e}"),
            }
        });
    }

    /// Whether `key`'s composer has anything to send: text or an attachment.
    pub(crate) fn composer_has_draft(&self, key: &ComposerKey) -> bool {
        self.composers
            .get(key)
            .is_some_and(|c| !c.edit.text.trim().is_empty() || !c.attachments.is_empty())
    }

    /// Sends `key`'s draft (Enter, the Send/Queue button): to its thread, queued behind an
    /// active turn, or as the first message of a new thread. Refused while sending or
    /// dictating, with nothing to send, while an attachment uploads or after one failed, and
    /// without a model.
    pub fn submit_composer(&mut self, key: &ComposerKey, tx: &mut Tx) {
        let Some(composer) = self.composers.get(key) else {
            return;
        };
        if composer.busy || composer.dictation.is_some() {
            return;
        }
        let text = plain_text(&composer.edit).trim().to_owned();
        if text.is_empty() && composer.attachments.is_empty() {
            return;
        }
        if self.submit_btw(key, &text) {
            return;
        }
        let composer = &self.composers[key];
        let attachments = match attachments::attachments(&composer.attachments) {
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
        composer.close_completions();
        match key {
            ComposerKey::Thread(thread) => self.send_to_thread(thread.clone(), submission),
            ComposerKey::NewThread { env, project } => {
                self.launch_new_thread(key.clone(), env.clone(), project.clone(), submission, tx)
            }
        }
    }

    /// Takes a `/btw question` draft as a side question (`thread::btw`) instead of a
    /// message, where the server answers them: omp answers it beside the thread. Refused
    /// without a question, with attachments (a side question is text) and before the thread
    /// exists. False for any other draft.
    fn submit_btw(&mut self, key: &ComposerKey, text: &str) -> bool {
        let Some(question) = btw::question(text) else {
            return false;
        };
        if !self
            .envs
            .get(key.env())
            .is_some_and(|env| env.has_capability(btw::CAPABILITY))
        {
            return false;
        }
        let refusal = match key {
            ComposerKey::NewThread { .. } => Some("Start the thread first, then ask /btw."),
            ComposerKey::Thread(_) if question.is_empty() => Some("Type a question after /btw."),
            ComposerKey::Thread(_) if !self.composers[key].attachments.is_empty() => {
                Some("A side question takes no attachments.")
            }
            ComposerKey::Thread(_) => None,
        };
        if let Some(refusal) = refusal {
            self.composer(key).error = Some(refusal.to_owned());
        } else if let ComposerKey::Thread(thread) = key {
            self.ask_btw(thread, question.to_owned());
            self.finish_composer(key, Ok(()));
        }
        true
    }

    /// Sends a submission to `thread`: started, or queued behind its active turn.
    fn send_to_thread(&mut self, thread: ThreadKey, submission: Submission) {
        let key = ComposerKey::Thread(thread.clone());
        let Some(connection) = self.envs.get(&thread.env).map(|e| e.connection().clone()) else {
            self.finish_composer(&key, Err("The environment is gone.".to_owned()));
            return;
        };
        let params = SendParams {
            thread_id: thread.thread_id.clone(),
            text: submission.text,
            attachments: submission.attachments,
            model: Some(submission.model),
            delivery: Delivery::Auto,
            command_id: None,
        };
        self.spawn(async move {
            let result = connection
                .turns_send(params)
                .await
                .map(drop)
                .map_err(|e| e.to_string());
            move |s: &mut J3, _: &mut Tx| s.finish_composer(&key, result)
        });
    }

    /// The last submission of `key` was sent (clears the draft, its attachments, menu and
    /// prediction) or failed (keeps the draft, shows the error).
    pub fn finish_composer(&mut self, key: &ComposerKey, result: Result<(), String>) {
        let composer = self.composer(key);
        composer.busy = false;
        match result {
            Ok(()) => {
                composer.error = None;
                composer.attachments.clear();
                composer.close_completions();
                composer.ghost = None;
                composer.provisional = None;
                composer.edit.clear();
            }
            Err(error) => composer.error = Some(error),
        }
    }

    /// Drops the composers whose environment, thread or project is gone (never the main
    /// one, nor one still sending), releasing their attachments, usage watches and microphone.
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
            ComposerKey::Thread(thread) => !shell.threads.contains_key(&thread.thread_id),
            ComposerKey::NewThread { project, .. } => project
                .as_deref()
                .is_none_or(|project| !shell.projects.contains_key(project)),
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
        self.request_project_commands(&key);
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
