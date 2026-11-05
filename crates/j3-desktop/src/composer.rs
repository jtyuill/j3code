//! The message composer: multi-line text, pasted images (uploaded as they
//! are added), one model-and-thinking picker, `/` completion and `^model`
//! mentions. Magic keywords and accepted or pasted mentions become atomic
//! inline tokens (the keyword with omp's gradient, the mention as a model
//! chip) whose text stays in the draft, so [`Submission::text`] is exactly
//! what was typed. omp's word prediction paints the rest of the word at the
//! caret as ghost text, and the microphone button dictates into the draft
//! (both from this machine's omp, see [`crate::assist`]). It reports a
//! [`Submission`]; its owner sends it and calls [`Composer::finish`].

use std::{
    ops::Range,
    path::Path,
    sync::{Arc, LazyLock},
    time::Instant,
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::base::input::{InlineToken, InlineTokenContext};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    combobox::{Combobox, ComboboxEvent, ComboboxState},
    h_flex,
    input::{
        Enter, Escape, IndentInline, InputEvent, MoveDown, MoveRight, MoveUp, Textarea,
        TextareaState,
    },
    popover::Popover,
    searchable_list::{SearchableListDelegate, SearchableListItem},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClipboardEntry, ClipboardItem, ContentMask, Context,
    Entity, EntityInputHandler as _, EventEmitter, FocusHandle, Focusable, HighlightStyle, Image,
    ImageFormat, ImageSource, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton,
    ObjectFit, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, StyledText, Subscription, Task, TestSupportExt as _, TextAlign,
    TextRun, WeakEntity, Window, canvas, div, img, px, rems,
};
use j3_client::t3::{chat_attachment::ChatAttachment, model::ModelSelection};

use crate::{
    agent_hub::DoubleTap,
    assist::{Assist, AssistEvent, Phase},
    completion::{self, Catalog, Completion, CompletionKind},
    environment::Environment,
    keywords, logos, mention,
    models::{self, ModelChoice},
    prediction::{self, Follow, Ghost, Provisional},
    preferences::Preferences,
    usage::{self, UsagePanel, UsageStore},
};

/// Largest image the composer uploads (the server's image limit).
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Completion rows shown at once.
const MAX_COMPLETIONS: usize = 8;
/// Models a `^` mention lists (the menu scrolls past [`MAX_COMPLETIONS`]).
const MAX_MODEL_COMPLETIONS: usize = 20;

/// The process-wide origin of the keyword shimmer, so every composer shows
/// the same phase.
static SHIMMER_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// What the composer asks its owner to send.
#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub model: ModelSelection,
}

pub enum ComposerEvent {
    Submit(Submission),
    Stop,
    /// ←← in the empty composer: open the agents hub.
    OpenAgentHub,
}

/// Whether the thread has an active run, which turns Send into Queue (or
/// Stop, with nothing to queue).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Running,
}

/// What the composer's one button does, and so which icon it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendAction {
    /// Starts a run.
    Send,
    /// Holds the draft until the current run ends.
    Queue,
    /// Interrupts the current run; there is no draft to queue.
    Stop,
}

impl SendAction {
    fn icon(self) -> IconName {
        match self {
            Self::Send => IconName::ArrowUp,
            Self::Queue => IconName::Inbox,
            Self::Stop => IconName::Square,
        }
    }

    /// The accessible name; the button shows no text.
    fn name(self) -> &'static str {
        match self {
            Self::Send => "Send",
            Self::Queue => "Queue",
            Self::Stop => "Stop",
        }
    }
}

enum Upload {
    Uploading,
    Done(ChatAttachment),
    Failed(SharedString),
}

struct PendingImage {
    id: u64,
    name: SharedString,
    image: Arc<Image>,
    upload: Upload,
}

/// A capture dictating into the draft, and where its words are.
struct Dictation {
    capture: String,
    phase: Phase,
    /// Byte offset of the transcript in the draft: the caret when the
    /// capture started.
    start: usize,
    /// The transcript as it stands in the draft, its separating space
    /// included.
    inserted: String,
    /// The draft has a word right before `start`, so the transcript starts
    /// with a space.
    separate: bool,
    /// Cleared once the user edits the transcript's text; later updates
    /// then leave the draft alone.
    tracking: bool,
}

#[derive(Clone)]
struct ModelItem {
    choice: ModelChoice,
    favorite: bool,
    /// Listed under its provider's header, which already names the
    /// provider; the row's qualifier leaves it out.
    in_provider_section: bool,
    /// Where the row's star saves the favourite.
    preferences: WeakEntity<Preferences>,
}

impl ModelItem {
    /// The whole row: the model's label (its qualifier muted), then the
    /// star and the chosen check, so the stars line up at the right edge.
    fn row(&self, checked: bool, cx: &App) -> gpui_kit::Div {
        let slug = self.choice.slug();
        let label = &self.choice.label;
        let qualifier = if self.in_provider_section {
            label.section_qualifier()
        } else {
            label.qualifier()
        };
        let theme = cx.theme();
        let (icon, color) = if self.favorite {
            (Lucide::StarFill, theme.warning)
        } else {
            (Lucide::Star, theme.muted_foreground.opacity(0.6))
        };
        let preferences = self.preferences.clone();
        let model = slug.to_owned();
        h_flex()
            .w_full()
            .gap_2()
            .min_w_0()
            .child(div().flex_none().child(label.name().clone()))
            .when_some(qualifier.cloned(), |this, qualifier| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(qualifier),
                )
            })
            .child(div().flex_1())
            .child(
                // The row confirms on click; the star's own mouse-down stops
                // that, so starring does not pick the model.
                div()
                    .flex_none()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id(SharedString::from(format!("favorite-{slug}")))
                            .test_support()
                            .aria_label(if self.favorite {
                                "Unstar model"
                            } else {
                                "Star model"
                            })
                            .p_0p5()
                            .rounded(theme.radius)
                            .hover(|this| this.bg(theme.muted))
                            .child(Icon::new(icon).xsmall().text_color(color))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                let _ = preferences.update(cx, |preferences, cx| {
                                    preferences.update_ui(|ui| ui.toggle_favorite(&model), cx)
                                });
                            }),
                    ),
            )
            .child(div().flex_none().w(px(14.)).when(checked, |this| {
                this.child(Icon::new(IconName::Check).xsmall())
            }))
    }
}

impl SearchableListItem for ModelItem {
    type Value = (String, String);

    fn title(&self) -> SharedString {
        self.choice.label.text().clone()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        self.row(false, cx)
    }

    fn value(&self) -> &Self::Value {
        &self.choice.key
    }

    fn matches(&self, query: &str) -> bool {
        self.choice.name.to_lowercase().contains(query)
            || self.choice.slug().to_lowercase().contains(query)
    }
}

/// The model list: the starred models first, then one section per
/// provider, headed by its logo and name.
struct ModelGroups {
    /// `(provider, models)`; `None` is the favourites section.
    groups: Vec<(Option<String>, Vec<ModelItem>)>,
    /// The shown sections after the last search: `(group, shown rows)`.
    shown: Vec<(usize, Vec<usize>)>,
}

impl ModelGroups {
    fn new(
        choices: &[ModelChoice],
        favorites: &[String],
        preferences: &WeakEntity<Preferences>,
    ) -> Self {
        let item = |choice: &ModelChoice, in_provider_section: bool| ModelItem {
            choice: choice.clone(),
            favorite: favorites.iter().any(|f| f == choice.slug()),
            in_provider_section,
            preferences: preferences.clone(),
        };
        let starred: Vec<ModelItem> = favorites
            .iter()
            .filter_map(|slug| choices.iter().find(|c| c.slug() == slug))
            .map(|choice| item(choice, false))
            .collect();
        let groups: Vec<(Option<String>, Vec<ModelItem>)> = (!starred.is_empty())
            .then_some((None, starred))
            .into_iter()
            .chain(
                models::by_provider(choices)
                    .into_iter()
                    .map(|(provider, rows)| {
                        let items = rows
                            .into_iter()
                            .map(|ix| item(&choices[ix], true))
                            .collect();
                        (Some(provider.to_owned()), items)
                    }),
            )
            .collect();
        let shown = groups
            .iter()
            .enumerate()
            .map(|(ix, (_, items))| (ix, (0..items.len()).collect()))
            .collect();
        Self { groups, shown }
    }
}

impl SearchableListDelegate for ModelGroups {
    type Item = ModelItem;

    fn sections_count(&self, _: &App) -> usize {
        self.shown.len()
    }

    fn items_count(&self, section: usize) -> usize {
        self.shown.get(section).map_or(0, |(_, rows)| rows.len())
    }

    fn item(&self, ix: IndexPath) -> Option<&ModelItem> {
        let (group, rows) = self.shown.get(ix.section)?;
        self.groups[*group].1.get(*rows.get(ix.row)?)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        ModelItem: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.shown
            .iter()
            .enumerate()
            .find_map(|(section, (group, rows))| {
                let items = &self.groups[*group].1;
                let row = rows.iter().position(|&row| items[row].value() == value)?;
                Some(IndexPath::default().section(section).row(row))
            })
    }

    /// A query naming the provider shows all its models.
    fn perform_search(&mut self, query: &str, _: &mut Window, _: &mut App) -> Task<()> {
        let query = query.trim().to_lowercase();
        self.shown = self
            .groups
            .iter()
            .enumerate()
            .filter_map(|(ix, (provider, items))| {
                let names_provider = provider.as_deref().is_some_and(|provider| {
                    provider.contains(&query)
                        || logos::provider_name(provider)
                            .to_lowercase()
                            .contains(&query)
                });
                let rows: Vec<usize> = if names_provider {
                    (0..items.len()).collect()
                } else {
                    (0..items.len())
                        .filter(|&row| items[row].matches(&query))
                        .collect()
                };
                (!rows.is_empty()).then_some((ix, rows))
            })
            .collect();
        Task::ready(())
    }

    /// Rows render themselves at full width, with their own check, so the
    /// stars line up (the default wrapper sizes a row to its text).
    fn render_item(
        &self,
        _: IndexPath,
        item: &ModelItem,
        checked: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        Some(item.row(checked, cx).into_any_element())
    }

    fn render_section_header(
        &self,
        section: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let (group, _) = self.shown.get(section)?;
        let header = h_flex()
            .gap_1p5()
            .px_2()
            .pt_1p5()
            .pb_0p5()
            .text_xs()
            .text_color(cx.theme().muted_foreground);
        Some(match &self.groups[*group].0 {
            Some(provider) => header
                .child(logos::provider_logo(provider, px(14.), cx))
                .child(logos::provider_name(provider).to_owned())
                .into_any_element(),
            None => header
                .child(
                    Icon::new(Lucide::StarFill)
                        .xsmall()
                        .text_color(cx.theme().warning),
                )
                .child("Favorites")
                .into_any_element(),
        })
    }
}

#[derive(Clone)]
struct LevelItem {
    id: String,
    label: SharedString,
}

pub struct Composer {
    environment: Entity<Environment>,
    cwd: String,
    input: Entity<TextareaState>,
    images: Vec<PendingImage>,
    next_image: u64,
    /// The environment's labelled models the picker was last built with.
    choices: Arc<[ModelChoice]>,
    /// Starred model slugs the picker was last built with.
    favorites: Vec<String>,
    preferences: Entity<Preferences>,
    /// The model picker; the chosen thinking level is in its footer.
    model: Entity<ComboboxState<ModelGroups>>,
    level_descriptor: Option<String>,
    levels: Vec<LevelItem>,
    level: Option<String>,
    /// The usage button's popover: this environment, the model's provider.
    usage: Entity<UsagePanel>,
    usage_store: Entity<UsageStore>,
    preferred: Option<ModelSelection>,
    catalog: Catalog,
    workspace_requested: bool,
    completions: Vec<Completion>,
    completion_ix: usize,
    run_state: RunState,
    busy: bool,
    error: Option<SharedString>,
    /// ← taps in the empty composer.
    left: DoubleTap,
    /// Set while [`Composer::sync_tokens`] edits the input, so the changes
    /// it causes do not start another pass.
    syncing: bool,
    /// Repaints the input every [`keywords::SHIMMER_FRAME`] while it is
    /// focused and holds a keyword token; ends by itself afterwards.
    shimmer: Option<Task<()>>,
    /// This machine's word prediction and dictation.
    assist: Entity<Assist>,
    /// omp's suggestion for the rest of the word at the caret; shown while
    /// the draft and caret are still the ones it follows.
    ghost: Option<Ghost>,
    /// The draft right after a Tab accept, while its space is provisional.
    provisional: Option<Provisional>,
    /// The newest prediction request; replacing it drops the older answer.
    predicting: Option<Task<()>>,
    /// The capture dictating into this draft.
    dictation: Option<Dictation>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    /// `preferred` preselects a model (the thread's, or a project default).
    /// `preferences` holds the starred models.
    pub fn new(
        environment: Entity<Environment>,
        preferences: Entity<Preferences>,
        cwd: String,
        preferred: Option<ModelSelection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 12)
                .submit_on_enter(true)
                .placeholder("Message omp…")
        });
        let model = cx.new(|cx| {
            let items = ModelGroups::new(&[], &[], &preferences.downgrade());
            ComboboxState::new(items, Vec::new(), window, cx).searchable(true)
        });
        let usage = cx.new(|cx| UsagePanel::new(environment.clone(), cx));
        let usage_store = UsageStore::global(cx);
        let assist = Assist::global(cx);
        let subscriptions = vec![
            cx.subscribe_in(&input, window, Self::on_input_event),
            cx.subscribe_in(
                &model,
                window,
                |this, _, event: &ComboboxEvent<ModelGroups>, _, cx| {
                    let ComboboxEvent::Change(keys) = event else {
                        return;
                    };
                    let Some(key) = keys.first() else {
                        return;
                    };
                    if let Some(choice) = this.choices.iter().find(|c| &c.key == key).cloned() {
                        this.preferred = Some(models::model_selection(&choice, None));
                        this.sync_levels(&choice, cx);
                    }
                },
            ),
            cx.observe_in(&environment, window, |this, _, window, cx| {
                this.sync_config(window, cx)
            }),
            cx.observe_in(&preferences, window, |this, _, window, cx| {
                this.sync_favorites(window, cx);
                // Settings can switch the inline usage windows on or off.
                cx.notify();
            }),
            // The button shows the cached peak.
            cx.observe(&usage_store, |_, _, cx| cx.notify()),
            cx.subscribe_in(&assist, window, Self::on_assist_event),
            // Another composer's capture holds the microphone.
            cx.observe(&assist, |_, _, cx| cx.notify()),
            // A capture never outlives the draft it writes into.
            cx.on_release(|this, cx| {
                if this.dictation.is_some() {
                    this.assist
                        .update(cx, |assist, cx| assist.stop_dictation(cx));
                }
            }),
        ];
        let mut composer = Self {
            environment,
            cwd,
            input,
            images: Vec::new(),
            next_image: 0,
            choices: Arc::default(),
            favorites: preferences.read(cx).ui().favorite_models.clone(),
            preferences,
            model,
            level_descriptor: None,
            levels: Vec::new(),
            level: None,
            usage,
            usage_store,
            preferred,
            catalog: Catalog::default(),
            workspace_requested: false,
            completions: Vec::new(),
            completion_ix: 0,
            run_state: RunState::Idle,
            busy: false,
            error: None,
            left: DoubleTap::default(),
            syncing: false,
            shimmer: None,
            assist,
            ghost: None,
            provisional: None,
            predicting: None,
            dictation: None,
            _subscriptions: subscriptions,
        };
        composer.sync_config(window, cx);
        composer
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn environment(&self) -> &Entity<Environment> {
        &self.environment
    }

    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    pub fn set_run_state(
        &mut self,
        run_state: RunState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.run_state == run_state {
            return;
        }
        self.run_state = run_state;
        let placeholder = match run_state {
            RunState::Idle => "Message omp…",
            RunState::Running => "Queue a message…",
        };
        self.input.update(cx, |input, cx| {
            input.set_placeholder(placeholder, window, cx)
        });
        cx.notify();
    }

    /// The owner finished sending the last submission: clear on success,
    /// keep the draft and show `error` on failure.
    pub fn finish(
        &mut self,
        result: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        match result {
            Ok(()) => {
                self.error = None;
                self.images.clear();
                self.completions.clear();
                self.ghost = None;
                self.provisional = None;
                self.predicting = None;
                self.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
                self.sync_shimmer(window, cx);
            }
            Err(error) => self.error = Some(error.into()),
        }
        cx.notify();
    }

    fn on_input_event(
        &mut self,
        input: &Entity<TextareaState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Change => {
                let (text, cursor) = {
                    let input = input.read(cx);
                    (input.value(), input.cursor())
                };
                self.completions = match completion::slash_query(&text, cursor) {
                    Some(query) => self.catalog.complete(query),
                    None => self.model_completions(&text, cursor),
                };
                self.completions.truncate(MAX_COMPLETIONS * 4);
                self.completion_ix = 0;
                if self.error.take().is_some() || !self.completions.is_empty() {
                    cx.notify();
                }
                cx.notify();
                self.follow_draft(&text, cursor, window, cx);
                self.sync_tokens(window, cx);
            }
            // Shift+Enter inserts a newline, yet the textarea still reports it.
            InputEvent::PressEnter { shift: false, .. } => self.submit(window, cx),
            InputEvent::Focus => {
                self.sync_shimmer(window, cx);
                // Start the predictor before the first keystroke needs it.
                self.assist.read(cx).warm(cx);
            }
            InputEvent::Blur => {
                self.sync_shimmer(window, cx);
                // Repaint the keywords in their static colours.
                self.input.update(cx, |_, cx| cx.notify());
            }
            InputEvent::PressEnter { .. } => {}
        }
    }

    /// Moves the word prediction along with an edit: settles a provisional
    /// space, keeps the ghost the user is typing through (or reports the
    /// one typed past), and asks omp about the new draft.
    fn follow_draft(
        &mut self,
        text: &str,
        cursor: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(provisional) = self.provisional.take() {
            if provisional.text == text && provisional.cursor == cursor {
                // The accept's own change.
                self.provisional = Some(provisional);
            } else if let Some((range, replacement)) = provisional.settle(text, cursor) {
                let utf16 = prediction::utf16_offset(text, range.start)
                    ..prediction::utf16_offset(text, range.end);
                // Its change event brings the settled draft back here.
                self.input.update(cx, |input, cx| {
                    input.replace_text_in_range(Some(utf16), &replacement, window, cx)
                });
                return;
            }
        }
        if let Some(ghost) = self.ghost.take() {
            match ghost.follow(text, cursor) {
                Follow::Unchanged => {
                    self.ghost = Some(ghost);
                    return;
                }
                Follow::TypedThrough(rest) => self.ghost = rest,
                Follow::Diverged => {
                    let at = prediction::utf16_offset(&ghost.text, ghost.cursor);
                    self.assist
                        .read(cx)
                        .feedback(ghost.text, at, ghost.suffix, false, cx);
                }
                Follow::Unrelated => {}
            }
        }
        self.request_prediction(text, cursor, cx);
    }

    /// Asks omp for the rest of the word ending at the caret, replacing any
    /// request still in flight. omp predicts only at the end of a line, and
    /// never into a selection, a completion menu or a dictation.
    fn request_prediction(&mut self, text: &str, cursor: usize, cx: &mut Context<Self>) {
        self.predicting = None;
        let wanted = self.dictation.is_none()
            && self.completions.is_empty()
            && !text.trim().is_empty()
            && prediction::at_line_end(text, cursor)
            && self.input.read(cx).selected_range().is_empty();
        if !wanted {
            self.ghost = None;
            return;
        }
        let answer = self.assist.read(cx).predict(
            text.to_owned(),
            prediction::utf16_offset(text, cursor),
            cx,
        );
        let text = text.to_owned();
        self.predicting = Some(cx.spawn(async move |this, cx| {
            let suffix = answer.await;
            let _ = this.update(cx, |this, cx| {
                let input = this.input.read(cx);
                // An answer for a draft that has moved on is no use.
                if input.cursor() != cursor || *input.value() != *text {
                    return;
                }
                let ghost = suffix.map(|suffix| Ghost {
                    text,
                    cursor,
                    suffix,
                });
                if this.ghost != ghost {
                    this.ghost = ghost;
                    cx.notify();
                }
            });
        }));
    }

    /// The ghost to paint: one that still follows the draft and caret, with
    /// nothing else claiming the spot.
    fn shown_ghost(&self, cx: &App) -> Option<&Ghost> {
        let ghost = self.ghost.as_ref()?;
        if self.dictation.is_some() || !self.completions.is_empty() {
            return None;
        }
        let input = self.input.read(cx);
        (input.selected_range().is_empty() && ghost.matches(&input.value(), input.cursor()))
            .then_some(ghost)
    }

    /// Types the shown ghost: Tab with a provisional space after it, →
    /// without one. Returns whether there was one to take.
    fn accept_ghost(&mut self, space: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(ghost) = self.shown_ghost(cx).cloned() else {
            return false;
        };
        self.ghost = None;
        let at = prediction::utf16_offset(&ghost.text, ghost.cursor);
        let insert = if space {
            format!("{} ", ghost.suffix)
        } else {
            ghost.suffix.clone()
        };
        self.assist
            .read(cx)
            .feedback(ghost.text, at, ghost.suffix, true, cx);
        let (text, cursor) = self.input.update(cx, |input, cx| {
            input.insert(insert, window, cx);
            (input.value().to_string(), input.cursor())
        });
        self.provisional = space.then_some(Provisional { text, cursor });
        cx.notify();
        true
    }

    /// The microphone button: start dictating at the caret, stop a capture
    /// that is recording (or cancel one still starting). Nothing while the
    /// last words are transcribed.
    fn toggle_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.dictation.as_ref().map(|d| d.phase) {
            Some(Phase::Transcribing) => {}
            Some(Phase::Starting | Phase::Recording) => {
                self.assist
                    .update(cx, |assist, cx| assist.stop_dictation(cx));
            }
            None => {
                let owner = cx.entity_id();
                let Some(capture) = self
                    .assist
                    .update(cx, |assist, cx| assist.start_dictation(owner, cx))
                else {
                    self.error = Some("Another composer is dictating.".into());
                    cx.notify();
                    return;
                };
                let (start, separate) = {
                    let input = self.input.read(cx);
                    let text = input.value();
                    let start = input.cursor();
                    let before = text.get(..start).and_then(|head| head.chars().next_back());
                    (start, before.is_some_and(|c| !c.is_whitespace()))
                };
                self.ghost = None;
                self.provisional = None;
                self.predicting = None;
                self.error = None;
                self.dictation = Some(Dictation {
                    capture,
                    phase: Phase::Starting,
                    start,
                    inserted: String::new(),
                    separate,
                    tracking: true,
                });
                // The button took focus; the words land at the caret.
                self.focus(window, cx);
                cx.notify();
            }
        }
    }

    fn on_assist_event(
        &mut self,
        _: &Entity<Assist>,
        event: &AssistEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let capture = match event {
            AssistEvent::Phase { capture, .. }
            | AssistEvent::Transcript { capture, .. }
            | AssistEvent::End { capture, .. } => capture,
        };
        let Some(dictation) = self.dictation.as_mut().filter(|d| d.capture == *capture) else {
            return;
        };
        match event {
            AssistEvent::Phase { phase, .. } => {
                dictation.phase = *phase;
                cx.notify();
            }
            AssistEvent::Transcript { text, .. } => self.write_transcript(text, window, cx),
            AssistEvent::End { text, error, .. } => {
                if !text.trim().is_empty() {
                    self.write_transcript(text, window, cx);
                }
                // A start cancelled before the microphone opened heard
                // nothing, and says nothing.
                let silent = self
                    .dictation
                    .take()
                    .is_some_and(|d| d.phase != Phase::Starting && d.inserted.is_empty());
                let note: Option<SharedString> = match error {
                    Some(error) => Some(format!("Dictation failed: {error}").into()),
                    None if silent => Some("No speech detected.".into()),
                    None => None,
                };
                // After the change event the transcript's edit queued, which
                // would clear it.
                if let Some(note) = note {
                    cx.defer_in(window, move |this, _, cx| {
                        this.error = Some(note);
                        cx.notify();
                    });
                }
                cx.notify();
            }
        }
    }

    /// Puts the capture's transcript so far in place of the previous one.
    /// Once the user has edited that text, the draft is theirs: later
    /// transcripts are dropped rather than overwrite it.
    fn write_transcript(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dictation) = self.dictation.as_mut().filter(|d| d.tracking) else {
            return;
        };
        let value = self.input.read(cx).value();
        let end = dictation.start + dictation.inserted.len();
        if value.get(dictation.start..end) != Some(dictation.inserted.as_str()) {
            dictation.tracking = false;
            return;
        }
        let text = text.trim();
        let replacement = match text {
            "" => String::new(),
            _ if dictation.separate => format!(" {text}"),
            _ => text.to_owned(),
        };
        if replacement == dictation.inserted {
            return;
        }
        let range = prediction::utf16_offset(&value, dictation.start)
            ..prediction::utf16_offset(&value, end);
        dictation.inserted = replacement;
        let replacement = dictation.inserted.as_str();
        self.input.update(cx, |input, cx| {
            input.replace_text_in_range(Some(range), replacement, window, cx)
        });
    }

    /// The models a `^` mention being typed at `cursor` offers.
    fn model_completions(&self, text: &str, cursor: usize) -> Vec<Completion> {
        let Some((_, query)) = mention::query_at(text, cursor) else {
            return Vec::new();
        };
        mention::rank(&self.choices, &self.favorites, query, MAX_MODEL_COMPLETIONS)
            .into_iter()
            .filter_map(|ix| self.choices.get(ix))
            .map(|choice| Completion {
                kind: CompletionKind::Model(choice.slug().to_owned()),
                label: choice.label.text().to_string(),
                description: None,
                hint: None,
            })
            .collect()
    }

    /// Keeps the input's tokens in step with its text: a magic keyword that
    /// is followed by another character (so `orchestrated` never flickers)
    /// and a `^selector` of a listed model become tokens, and a keyword
    /// token whose word no longer stands alone is plain text again. Token
    /// text always equals the text it replaces, so no offset moves and the
    /// selection is restored after the edits. A second pass over the result
    /// changes nothing, so the `Change` events the edits emit settle.
    fn sync_tokens(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        self.syncing = true;
        self.rewrite_tokens(window, cx);
        self.syncing = false;
        self.sync_shimmer(window, cx);
    }

    fn rewrite_tokens(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input.clone();
        let (text, tokens, selection, composing) = input.update(cx, |input, cx| {
            (
                input.value(),
                input.tokens().to_vec(),
                input.selected_range(),
                input.marked_text_range(window, cx).is_some(),
            )
        });
        // An IME composition owns the text; edit nothing until it commits.
        if composing {
            return;
        }
        let keywords = keywords::find(&text);
        let mentions_allowed = mention::allowed(&text);

        let stale = |id: &str, range: &Range<usize>| {
            if id.starts_with("kw:") {
                !keywords.iter().any(|found| found.range == *range)
            } else {
                id.starts_with("model:") && !mentions_allowed
            }
        };
        let (unwrap, keep): (Vec<_>, Vec<_>) = tokens
            .iter()
            .map(|span| span.range())
            .zip(tokens.iter().map(|span| span.token().id().clone()))
            .partition(|(range, id)| stale(id, range));
        let mut occupied: Vec<Range<usize>> = keep.into_iter().map(|(range, _)| range).collect();

        let free = |occupied: &[Range<usize>], range: &Range<usize>| {
            !occupied
                .iter()
                .any(|taken| taken.start < range.end && range.start < taken.end)
        };
        let mut wrap: Vec<(Range<usize>, InlineToken)> = Vec::new();
        for found in &keywords {
            if found.range.end < text.len() && free(&occupied, &found.range) {
                let word = &text[found.range.clone()];
                occupied.push(found.range.clone());
                wrap.push((
                    found.range.clone(),
                    InlineToken::new(format!("kw:{}", found.keyword.word), word),
                ));
            }
        }
        for mention in mention::mentions(&text) {
            if mention.range.end < text.len()
                && free(&occupied, &mention.range)
                && let Some(choice) = mention::find_choice(&self.choices, mention.selector)
            {
                occupied.push(mention.range.clone());
                wrap.push((
                    mention.range.clone(),
                    InlineToken::new(
                        format!("model:{}", mention.selector),
                        &text[mention.range.clone()],
                    )
                    .with_label(choice.label.text().clone()),
                ));
            }
        }
        if unwrap.is_empty() && wrap.is_empty() {
            return;
        }

        input.update(cx, |input, cx| {
            for (range, _) in &unwrap {
                // Identical text, replaced as a plain edit: the edit drops the
                // token it overlaps.
                let utf16 = text[..range.start].encode_utf16().count()
                    ..text[..range.end].encode_utf16().count();
                input.replace_text_in_range(Some(utf16), &text[range.clone()], window, cx);
            }
            for (range, token) in wrap {
                // Rejected while composing; the next change retries.
                let _ = input.replace_range_with_token(range, token, window, cx);
            }
            if input.selected_range() != selection {
                input.set_selected_range(selection, cx);
            }
        });
    }

    /// Starts or stops the keyword shimmer: a repaint of the input every
    /// frame while it is focused and holds a keyword token.
    fn sync_shimmer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.shimmer_wanted(window, cx) {
            if let Some(task) = self.shimmer.take() {
                task.detach();
            }
            return;
        }
        if self.shimmer.is_some() {
            return;
        }
        self.shimmer = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(keywords::SHIMMER_FRAME)
                    .await;
                let running = this.update_in(cx, |this, window, cx| {
                    let wanted = this.shimmer_wanted(window, cx);
                    if wanted {
                        this.input.update(cx, |_, cx| cx.notify());
                    } else if let Some(task) = this.shimmer.take() {
                        // This task: let it end instead of cancelling it.
                        task.detach();
                    }
                    wanted
                });
                if !running.unwrap_or(false) {
                    break;
                }
            }
        }));
    }

    /// Whether the input is focused and holds a keyword token.
    fn shimmer_wanted(&self, window: &Window, cx: &App) -> bool {
        let input = self.input.read(cx);
        input.focus_handle(cx).is_focused(window)
            && input
                .tokens()
                .iter()
                .any(|span| span.token().id().starts_with("kw:"))
    }

    /// omp's ←← in an empty composer opens the agents hub; any other key
    /// or a non-empty composer breaks the pair.
    fn on_capture_key_down(
        &mut self,
        event: &KeyDownEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let left = keystroke.key == "left"
            && !keystroke.modifiers.modified()
            && !event.is_held
            && self.input.read(cx).value().is_empty();
        if !left {
            self.left.reset();
        } else if self.left.tap(Instant::now()) {
            cx.emit(ComposerEvent::OpenAgentHub);
        }
    }

    /// Whether there is anything to send: text or an image.
    fn has_draft(&self, cx: &App) -> bool {
        !self.input.read(cx).value().trim().is_empty() || !self.images.is_empty()
    }

    fn send_action(&self, cx: &App) -> SendAction {
        match self.run_state {
            RunState::Idle => SendAction::Send,
            RunState::Running if self.has_draft(cx) => SendAction::Queue,
            RunState::Running => SendAction::Stop,
        }
    }

    fn submit(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        // The transcript is still arriving; send it once it has.
        if self.busy || self.dictation.is_some() {
            return;
        }
        let text = self.input.read(cx).value().trim().to_owned();
        if text.is_empty() && self.images.is_empty() {
            return;
        }
        let mut attachments = Vec::with_capacity(self.images.len());
        for image in &self.images {
            match &image.upload {
                Upload::Done(attachment) => attachments.push(attachment.clone()),
                Upload::Uploading => {
                    self.error = Some("Wait for images to finish uploading.".into());
                    cx.notify();
                    return;
                }
                Upload::Failed(_) => {
                    self.error = Some("Remove the images that failed to upload.".into());
                    cx.notify();
                    return;
                }
            }
        }
        let Some(model) = self.selection(cx) else {
            self.error = Some("Choose a model.".into());
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        self.completions.clear();
        cx.emit(ComposerEvent::Submit(Submission {
            text,
            attachments,
            model,
        }));
        cx.notify();
    }

    /// The chosen model.
    fn choice(&self, cx: &App) -> Option<&ModelChoice> {
        let key = self.model.read(cx).selected_value()?;
        self.choices.iter().find(|c| c.key == key)
    }

    /// The model selection to send, with the chosen thinking level.
    pub fn selection(&self, cx: &App) -> Option<ModelSelection> {
        Some(models::model_selection(
            self.choice(cx)?,
            self.level_descriptor.as_deref().zip(self.level.as_deref()),
        ))
    }

    fn sync_config(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let environment = self.environment.read(cx);
        let state = environment.state().clone();
        let Some(config) = state.config.as_ref() else {
            return;
        };
        let live = environment.is_live();
        let connection = environment.connection().clone();

        let mut catalog = Catalog::for_workspace(config, &self.cwd);
        if let Some(fetched) = state.workspaces.get(&self.cwd) {
            catalog.merge(&fetched.slash_commands, &fetched.skills);
        }
        if !catalog.has_workspace && !self.workspace_requested && live {
            self.workspace_requested = true;
            let cwd = self.cwd.clone();
            cx.spawn(async move |_, _| {
                if let Err(e) = connection.refresh_workspace(&cwd).await {
                    tracing::warn!("loading commands for {cwd}: {e}");
                }
            })
            .detach();
        }
        self.catalog = catalog;

        let choices = self.environment.read(cx).models().clone();
        if choices != self.choices {
            let current = self.selection(cx).or_else(|| self.preferred.clone());
            let initial = models::initial_choice(&choices, current.as_ref());
            self.choices = choices;
            let key = initial.map(|ix| self.choices[ix].key.clone());
            let items = self.model_groups();
            self.model.update(cx, |model, cx| {
                model.set_items(items, window, cx);
                model.set_selected_values(key.as_slice(), window, cx);
            });
            if let Some(choice) = initial.map(|ix| self.choices[ix].clone()) {
                self.sync_levels(&choice, cx);
            }
        }
        cx.notify();
    }

    fn model_groups(&self) -> ModelGroups {
        ModelGroups::new(
            &self.choices,
            &self.favorites,
            &self.preferences.downgrade(),
        )
    }

    /// Rebuilds the model list when the starred models change, keeping the
    /// chosen model.
    fn sync_favorites(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let favorites = &self.preferences.read(cx).ui().favorite_models;
        if *favorites == self.favorites {
            return;
        }
        self.favorites = favorites.clone();
        let items = self.model_groups();
        let key = self.choice(cx).map(|choice| choice.key.clone());
        self.model.update(cx, |model, cx| {
            model.set_items(items, window, cx);
            model.set_selected_values(key.as_slice(), window, cx);
        });
        cx.notify();
    }

    fn sync_levels(&mut self, choice: &ModelChoice, cx: &mut Context<Self>) {
        let provider = Some(choice.provider.clone());
        self.usage
            .update(cx, |usage, cx| usage.set_provider(provider, cx));
        let state = self.environment.read(cx).state().clone();
        let descriptor = state
            .config
            .as_ref()
            .and_then(|config| models::thinking_descriptor(config, choice))
            .cloned();
        let preferred_level = self.level.clone().or_else(|| {
            let descriptor = descriptor.as_ref()?;
            self.preferred
                .as_ref()
                .and_then(|p| models::selection_level(p, &descriptor.id))
                .map(str::to_owned)
        });
        self.level_descriptor = descriptor.as_ref().map(|d| d.id.clone());
        self.levels = descriptor
            .iter()
            .flat_map(|d| &d.options)
            .map(|option| LevelItem {
                id: option.id.clone(),
                label: option.label.clone().into(),
            })
            .collect();
        self.level = descriptor
            .as_ref()
            .and_then(|d| models::initial_level(d, preferred_level.as_deref()));
        cx.notify();
    }

    fn accept_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(completion) = self.completions.get(self.completion_ix).cloned() else {
            return;
        };
        if let CompletionKind::Model(slug) = &completion.kind {
            self.accept_model(slug, window, cx);
            return;
        }
        let text = completion::apply(&self.input.read(cx).value(), &completion);
        let end = text.len();
        self.completions.clear();
        self.input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.set_selected_range(end..end, cx);
        });
        // `set_value` does not emit Change, so the menu stays closed.
        self.completions.clear();
        cx.notify();
    }

    /// Replaces the `^query` before the cursor with the token of the model
    /// `slug` and a space, leaving the caret after the space.
    fn accept_model(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.completions.clear();
        let (text, cursor) = {
            let input = self.input.read(cx);
            (input.value(), input.cursor())
        };
        if let Some((range, _)) = mention::query_at(&text, cursor)
            && let Some(choice) = mention::find_choice(&self.choices, slug)
        {
            let token = InlineToken::new(format!("model:{slug}"), format!("^{slug}"))
                .with_label(choice.label.text().clone());
            let end = range.start + 1 + slug.len();
            self.input.update(cx, |input, cx| {
                if input
                    .replace_range_with_token(range, token, window, cx)
                    .is_ok()
                {
                    input.set_selected_range(end..end, cx);
                    input.insert(" ", window, cx);
                }
            });
        }
        cx.notify();
    }

    fn add_image(
        &mut self,
        format: ImageFormat,
        bytes: Vec<u8>,
        name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let mime = match format {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Webp => "image/webp",
            ImageFormat::Gif => "image/gif",
            _ => {
                self.error = Some("Paste a PNG, JPEG, WebP or GIF image.".into());
                cx.notify();
                return;
            }
        };
        if bytes.len() > MAX_IMAGE_BYTES {
            self.error = Some("Images must be 10 MB or smaller.".into());
            cx.notify();
            return;
        }
        let id = self.next_image;
        self.next_image += 1;
        let extension = mime.trim_start_matches("image/").replace("jpeg", "jpg");
        let name = name.unwrap_or_else(|| format!("pasted-{}.{extension}", id + 1));
        self.images.push(PendingImage {
            id,
            name: name.clone().into(),
            image: Arc::new(Image::from_bytes(format, bytes.clone())),
            upload: Upload::Uploading,
        });
        let connection = self.environment.read(cx).connection().clone();
        cx.spawn(async move |this, cx| {
            let result = connection.upload_image(&name, mime, bytes).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(image) = this.images.iter_mut().find(|image| image.id == id) {
                    image.upload = match result {
                        Ok(attachment) => Upload::Done(attachment),
                        Err(e) => Upload::Failed(e.to_string().into()),
                    };
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn on_paste(weak: &WeakEntity<Self>, item: &ClipboardItem, cx: &mut App) -> bool {
        let mut images = Vec::new();
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    images.push((image.format, image.bytes.clone(), None))
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    for path in paths.paths() {
                        if let Some(format) = format_for_path(path)
                            && let Ok(bytes) = std::fs::read(path)
                        {
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
                            images.push((format, bytes, name));
                        }
                    }
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if images.is_empty() {
            return false;
        }
        let _ = weak.update(cx, |this, cx| {
            for (format, bytes, name) in images {
                this.add_image(format, bytes, name, cx);
            }
        });
        true
    }

    fn render_completions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.completions.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let start = self
            .completion_ix
            .saturating_sub(MAX_COMPLETIONS - 1)
            .min(self.completions.len().saturating_sub(MAX_COMPLETIONS));
        let rows = self
            .completions
            .iter()
            .enumerate()
            .skip(start)
            .take(MAX_COMPLETIONS)
            .map(|(ix, completion)| {
                let selected = ix == self.completion_ix;
                // Models show their label; their slug keeps the row's id.
                let (key, model) = match &completion.kind {
                    CompletionKind::Model(slug) => (slug, true),
                    _ => (&completion.label, false),
                };
                h_flex()
                    .id(SharedString::from(format!("completion-{key}")))
                    .test_support()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .text_sm()
                    .when(selected, |this| {
                        this.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .when(!selected, |this| {
                        this.hover(|this| this.bg(theme.accent.opacity(0.5)))
                    })
                    .child(
                        div()
                            .when(model, |this| this.flex_1().min_w_0().truncate())
                            .when(!model, |this| {
                                this.font_family(theme.mono_font_family.clone())
                            })
                            .child(completion.label.clone()),
                    )
                    .when_some(completion.hint.clone(), |this, hint| {
                        this.child(div().text_color(theme.muted_foreground).child(hint))
                    })
                    .when_some(completion.description.clone(), |this, description| {
                        this.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(description),
                        )
                    })
                    .when(completion.kind == CompletionKind::Skill, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("skill"),
                        )
                    })
                    .when(model, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("model"),
                        )
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.completion_ix = ix;
                        this.accept_completion(window, cx);
                    }))
            });
        Some(
            v_flex()
                .absolute()
                .bottom_full()
                .left_0()
                .right_0()
                .mb_1()
                .p_1()
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .border_1()
                .border_color(theme.border)
                .rounded(theme.radius_lg)
                .shadow_md()
                .children(rows)
                .into_any_element(),
        )
    }

    fn render_images(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.images.is_empty() {
            return None;
        }
        let theme = cx.theme();
        Some(
            h_flex()
                .gap_2()
                .px_3()
                .pt_2()
                .children(self.images.iter().map(|image| {
                    let id = image.id;
                    let (status, failed) = match &image.upload {
                        Upload::Uploading => {
                            (Some(Spinner::new().xsmall().into_any_element()), None)
                        }
                        Upload::Done(_) => (None, None),
                        Upload::Failed(error) => (None, Some(error.clone())),
                    };
                    h_flex()
                        .id(SharedString::from(format!("composer-image-{id}")))
                        .gap_1()
                        .p_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(if failed.is_some() {
                            theme.danger
                        } else {
                            theme.border
                        })
                        .child(
                            img(ImageSource::Image(image.image.clone()))
                                .size_10()
                                .rounded(theme.radius)
                                .object_fit(ObjectFit::Cover),
                        )
                        .child(
                            v_flex()
                                .max_w(rems(9.))
                                .child(div().text_xs().truncate().child(image.name.clone()))
                                .when_some(failed.clone(), |this, error| {
                                    this.child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.danger)
                                            .truncate()
                                            .child(error),
                                    )
                                }),
                        )
                        .children(status)
                        .child(
                            Button::new(SharedString::from(format!("remove-image-{id}")))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Close)
                                .tooltip("Remove image")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.images.retain(|image| image.id != id);
                                    cx.notify();
                                })),
                        )
                        .when_some(failed, |this, error| {
                            this.tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(error.clone())
                                    .build(window, cx)
                            })
                        })
                }))
                .into_any_element(),
        )
    }

    /// One button for both the model and its thinking level: it shows the
    /// provider logo, model and level; its menu lists models by provider
    /// with the levels in the footer. The logo names the provider, so the
    /// label leaves it out.
    fn render_model_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let choice = self.choice(cx);
        let provider = choice.map(|choice| choice.provider.clone());
        let name: SharedString = choice.map_or("Model".into(), |c| c.label.section_text().clone());
        let level_label = self
            .level
            .as_ref()
            .and_then(|id| self.levels.iter().find(|level| &level.id == id))
            .map(|level| level.label.clone());
        let levels = self.levels.clone();
        let level = self.level.clone();
        let weak = cx.weak_entity();
        div()
            .flex_none()
            .max_w(rems(22.))
            .child(
                Combobox::new(&self.model)
                    .xsmall()
                    .appearance(false)
                    .search_placeholder("Search models")
                    .menu_width(rems(24.))
                    .menu_max_h(rems(26.))
                    .render_trigger(move |_, _, cx| {
                        let muted = cx.theme().muted_foreground;
                        h_flex()
                            .gap_1p5()
                            .min_w_0()
                            .when_some(provider.as_deref(), |this, provider| {
                                this.child(logos::provider_logo(provider, px(14.), cx))
                            })
                            .child(div().min_w_0().truncate().child(name.clone()))
                            .when_some(level_label.clone(), |this, label| {
                                this.child(div().flex_none().text_color(muted).child(label))
                            })
                            .child(
                                Icon::new(IconName::ChevronDown)
                                    .xsmall()
                                    .flex_none()
                                    .text_color(muted),
                            )
                    })
                    .when(!levels.is_empty(), |this| {
                        this.footer(move |_, cx| {
                            render_levels(&levels, level.as_deref(), &weak, cx)
                        })
                    }),
            )
            .into_any_element()
    }

    /// The gauge beside the model: the model provider's highest cached
    /// usage on this environment, or, for a provider whose usage Settings
    /// shows inline, each window's usage; opening it refreshes and lists
    /// the limits.
    fn render_usage_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let env = self.environment.read(cx);
        let choice = self.choice(cx);
        let label = choice.and_then(|choice| {
            let snapshot = self.usage_store.read(cx).snapshot(env.key())?;
            if self.preferences.read(cx).ui().shows_usage(&choice.provider) {
                if let Some(breakdown) = usage::breakdown(snapshot, &choice.provider) {
                    return Some(breakdown);
                }
            }
            usage::peak(snapshot, &choice.provider).map(|peak| format!("{:.0}%", peak * 100.))
        });
        let tooltip = match choice {
            Some(choice) => format!(
                "{} usage limits on {}",
                logos::provider_name(&choice.provider),
                env.label()
            ),
            None => "Usage limits".to_owned(),
        };
        let panel = self.usage.clone();
        let refresh = self.usage.downgrade();
        Popover::new("usage-popover")
            .anchor(Anchor::BottomLeft)
            .trigger(
                Button::new("usage")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::Gauge))
                    .when_some(label, |this, label| this.label(label))
                    .tooltip(tooltip),
            )
            .on_open_change(move |open, _, cx| {
                if *open {
                    let _ = refresh.update(cx, |usage, cx| usage.refresh(cx));
                }
            })
            .content(move |_, _, _| panel.clone())
            .into_any_element()
    }

    /// The microphone, left of Send: dictates into the draft from this
    /// machine's microphone. While the model loads it spins (a click
    /// cancels), while recording it is red (a click stops), and while the
    /// last words are transcribed it waits.
    fn render_mic(&self, cx: &mut Context<Self>) -> AnyElement {
        let elsewhere = self
            .assist
            .read(cx)
            .capture()
            .is_some_and(|(owner, _)| owner != cx.entity_id());
        let button = Button::new("dictate").xsmall().rounded(px(999.));
        let button = match self.dictation.as_ref().map(|d| d.phase) {
            None => button
                .ghost()
                .icon(Icon::new(Lucide::Mic))
                .accessibility_label("Dictate")
                .tooltip(if elsewhere {
                    "Another composer is dictating"
                } else {
                    "Dictate"
                })
                .disabled(elsewhere),
            Some(Phase::Starting) => button
                .ghost()
                .icon(Spinner::new().xsmall())
                .accessibility_label("Cancel dictation")
                .tooltip("Starting the microphone… (click to cancel)"),
            Some(Phase::Recording) => button
                .custom(recording_style(cx))
                .icon(Icon::new(Lucide::Mic))
                .accessibility_label("Stop dictation")
                .tooltip("Stop dictation"),
            Some(Phase::Transcribing) => button
                .ghost()
                .icon(Icon::new(Lucide::Mic))
                .accessibility_label("Transcribing")
                .tooltip("Transcribing…")
                .loading(true),
        };
        button
            .on_click(cx.listener(|this, _, window, cx| this.toggle_dictation(window, cx)))
            .into_any_element()
    }
}

/// The model menu's footer: the thinking levels, `level` highlighted.
fn render_levels(
    levels: &[LevelItem],
    level: Option<&str>,
    composer: &WeakEntity<Composer>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    h_flex()
        .flex_wrap()
        .gap_1()
        .px_1()
        .py_0p5()
        .text_xs()
        .child(
            div()
                .mr_1()
                .text_color(theme.muted_foreground)
                .child("Thinking"),
        )
        .children(levels.iter().map(|item| {
            let selected = level == Some(item.id.as_str());
            let id = item.id.clone();
            let composer = composer.clone();
            div()
                .id(SharedString::from(format!("thinking-{id}")))
                .px_1p5()
                .py_0p5()
                .rounded(theme.radius)
                .cursor_pointer()
                .when(selected, |this| {
                    this.bg(theme.accent).text_color(theme.accent_foreground)
                })
                .when(!selected, |this| {
                    this.hover(|this| this.bg(theme.accent.opacity(0.5)))
                })
                .child(item.label.clone())
                .on_click(move |_, _, cx| {
                    let _ = composer.update(cx, |this, cx| {
                        this.level = Some(id.clone());
                        cx.notify();
                    });
                })
        }))
        .into_any_element()
}

/// The send button's colours: a quiet grey disc that lights up with the
/// accent once there is something to send, and a red tint for Stop. (The
/// theme's primary button is a white disc on the dark theme.)
fn send_style(action: SendAction, ready: bool, cx: &App) -> ButtonCustomVariant {
    let theme = cx.theme();
    let (color, foreground) = match action {
        SendAction::Stop => (theme.danger.opacity(0.18), theme.danger),
        _ if ready => (theme.info.opacity(0.22), theme.info),
        _ => (theme.secondary, theme.muted_foreground),
    };
    ButtonCustomVariant::new(cx)
        .color(color)
        .foreground(foreground)
        .hover(color.opacity(1.).blend(theme.foreground.opacity(0.08)))
        .active(color.opacity(1.).blend(theme.foreground.opacity(0.14)))
}

/// The microphone while it records: the Stop button's red tint.
fn recording_style(cx: &App) -> ButtonCustomVariant {
    let theme = cx.theme();
    let color = theme.danger.opacity(0.18);
    ButtonCustomVariant::new(cx)
        .color(color)
        .foreground(theme.danger)
        .hover(color.opacity(1.).blend(theme.foreground.opacity(0.08)))
        .active(color.opacity(1.).blend(theme.foreground.opacity(0.14)))
}

/// omp's suggestion `suffix` painted dim right after the caret at byte
/// `cursor`, in the input's own font. It is painted after the input, from
/// the layout the input just painted, and clipped to the input's text area.
/// Its accessible label is the suggestion.
fn render_ghost(
    input: Entity<TextareaState>,
    cursor: usize,
    suffix: SharedString,
    cx: &App,
) -> impl IntoElement {
    let color = cx.theme().muted_foreground.opacity(0.6);
    let label = suffix.clone();
    let paint = canvas(
        |_, _, _| {},
        move |_, (), window, cx| {
            let input = input.read(cx);
            if !input.focus_handle(cx).is_focused(window) {
                return;
            }
            let (Some(caret), Some(clip)) = (
                input.range_to_bounds(&(cursor..cursor)),
                input.text_bounds(),
            ) else {
                return;
            };
            let style = window.text_style();
            let run = TextRun {
                len: suffix.len(),
                font: style.font(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let font_size = style.font_size.to_pixels(window.rem_size());
            let line = window
                .text_system()
                .shape_line(suffix.clone(), font_size, &[run], None);
            window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
                let _ = line.paint(
                    caret.origin,
                    caret.size.height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            });
        },
    )
    .size_0();
    div()
        .id("composer-ghost")
        .test_support()
        .aria_label(label)
        .absolute()
        .size_0()
        .child(paint)
}

fn format_for_path(path: &Path) -> Option<ImageFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::Webp),
        "gif" => Some(ImageFormat::Gif),
        _ => None,
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

impl Render for Composer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        let focus = self.focus_handle(cx);
        let action = self.send_action(cx);
        let completions = self.render_completions(cx);
        let images = self.render_images(cx);
        let model_picker = self.render_model_picker(cx);
        let usage_button = self.render_usage_button(cx);
        let mic = self.render_mic(cx);
        let ghost = self
            .shown_ghost(cx)
            .map(|ghost| (ghost.cursor, SharedString::from(ghost.suffix.clone())));
        let key = |keys: &str| crate::actions::shortcut(keys, cx);
        let tooltip = match action {
            SendAction::Send => format!("Send ({})", key("enter")),
            SendAction::Queue => format!("Queue after the current run ({})", key("enter")),
            SendAction::Stop => format!("Stop the run ({})", key("secondary-.")),
        };
        // An empty catalog is nearly always a server problem (e.g. omp not
        // found); say what the server reported instead of showing nothing.
        let catalog_note: Option<SharedString> = if self.choices.is_empty() {
            let state = self.environment.read(cx).state();
            state
                .config
                .as_ref()
                .and_then(|config| config.providers.first())
                .map(|provider| {
                    provider
                        .message
                        .clone()
                        .unwrap_or_else(|| "Loading models…".to_owned())
                        .into()
                })
        } else {
            None
        };
        let theme = cx.theme();
        let completions_open = !self.completions.is_empty();
        v_flex()
            .id("composer")
            .test_support()
            .key_context("Composer")
            .capture_key_down(cx.listener(Self::on_capture_key_down))
            .relative()
            .w_full()
            .bg(theme.background)
            .border_1()
            .border_color(theme.input)
            .rounded(theme.radius_lg)
            .when(completions_open, |this| {
                this.capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                    this.completion_ix = (this.completion_ix + 1) % this.completions.len();
                    cx.stop_propagation();
                    cx.notify();
                }))
                .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                    let len = this.completions.len();
                    this.completion_ix = (this.completion_ix + len - 1) % len;
                    cx.stop_propagation();
                    cx.notify();
                }))
                .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                    this.accept_completion(window, cx);
                    cx.stop_propagation();
                }))
                .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                    if action.shift {
                        return;
                    }
                    this.accept_completion(window, cx);
                    cx.stop_propagation();
                }))
                .capture_action(cx.listener(|this, _: &Escape, _, cx| {
                    this.completions.clear();
                    cx.stop_propagation();
                    cx.notify();
                }))
            })
            .when(ghost.is_some(), |this| {
                this.capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                    if this.accept_ghost(true, window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(
                    |this, _: &MoveRight, window, cx| {
                        if this.accept_ghost(false, window, cx) {
                            cx.stop_propagation();
                        }
                    },
                ))
            })
            .children(completions)
            .children(images)
            .child(
                // `text_sm` here too, so the ghost is set like the draft.
                div()
                    .px_1()
                    .pt_1()
                    .text_sm()
                    .child(
                        Textarea::new(&self.input)
                            .appearance(false)
                            .text_sm()
                            .aria_label("Message")
                            .token(move |ctx, window, cx| {
                                render_token(ctx, focus.is_focused(window), cx)
                            })
                            .on_paste(move |item, _, cx| Self::on_paste(&weak, item, cx)),
                    )
                    .when_some(ghost, |this, (cursor, suffix)| {
                        this.child(render_ghost(self.input.clone(), cursor, suffix, cx))
                    }),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().px_3().text_xs().text_color(theme.danger).child(error))
            })
            .when_some(catalog_note, |this, note| {
                this.child(
                    div()
                        .px_3()
                        .pb_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(note),
                )
            })
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .pt_1()
                    .child(model_picker)
                    .child(usage_button)
                    .child(div().flex_1())
                    .child(mic)
                    .child({
                        let ready = action == SendAction::Stop || self.has_draft(cx);
                        Button::new("send-message")
                            .xsmall()
                            .rounded(px(999.))
                            .custom(send_style(action, ready, cx))
                            .icon(action.icon())
                            .accessibility_label(action.name())
                            .tooltip(tooltip)
                            .loading(self.busy)
                            .on_click(cx.listener(move |this, _, window, cx| match action {
                                SendAction::Stop => cx.emit(ComposerEvent::Stop),
                                SendAction::Send | SendAction::Queue => this.submit(window, cx),
                            }))
                    }),
            )
    }
}

/// What an inline token of the draft renders as: a model chip for a
/// `^selector`, the keyword's gradient (animated while the input is
/// `focused`) for a magic keyword.
fn render_token(ctx: &InlineTokenContext, focused: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let token = ctx.token();
    let id = token.id().clone();
    let chip = div()
        .id(id.clone())
        .test_support()
        .flex()
        .items_center()
        .h(ctx.line_height())
        .max_w(ctx.available_width())
        .rounded(theme.radius);
    if id.starts_with("model:") {
        let label = token.label().clone();
        return chip
            .aria_label(label.clone())
            .gap_1()
            .px_1()
            .text_color(theme.info)
            .bg(if ctx.is_selected() {
                theme.selection
            } else {
                theme.info.opacity(0.15)
            })
            .child(Icon::new(Lucide::Bot).xsmall().flex_none())
            .child(div().min_w_0().truncate().child(label))
            .into_any_element();
    }
    let word = token.text().clone();
    let keyword = keywords::KEYWORDS.iter().find(|k| k.word == &*word);
    let chip = chip
        .aria_label(word.clone())
        .when(ctx.is_selected(), |this| this.bg(theme.selection));
    match keyword {
        Some(keyword) => {
            let phase = if focused {
                keywords::shimmer_phase(SHIMMER_START.elapsed())
            } else {
                0.0
            };
            let highlights: Vec<_> = word
                .char_indices()
                .zip(keywords::colors(keyword, phase))
                .map(|((start, ch), color)| {
                    (
                        start..start + ch.len_utf8(),
                        HighlightStyle {
                            color: Some(color),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            chip.child(StyledText::new(word).with_highlights(highlights))
                .into_any_element()
        }
        None => chip.child(word).into_any_element(),
    }
}
