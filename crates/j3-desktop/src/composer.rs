//! The message composer: multi-line text, pasted images (uploaded as they
//! are added), one model-and-thinking picker, and `/` completion. It reports a
//! [`Submission`]; its owner sends it and calls [`Composer::finish`].

use std::{path::Path, sync::Arc, time::Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, IndexPath, Sizable as _,
    button::{Button, ButtonVariants as _},
    combobox::{Combobox, ComboboxEvent, ComboboxState},
    h_flex,
    input::{Enter, Escape, IndentInline, InputEvent, MoveDown, MoveUp, Textarea, TextareaState},
    popover::Popover,
    searchable_list::{SearchableListDelegate, SearchableListItem},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, AppContext as _, ClipboardEntry, ClipboardItem, Context, Entity,
    EventEmitter, FocusHandle, Focusable, Image, ImageFormat, ImageSource, InteractiveElement as _,
    IntoElement, KeyDownEvent, ObjectFit, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription, Task,
    TestSupportExt as _, WeakEntity, Window, div, img, px, rems,
};
use j3_client::t3::{chat_attachment::ChatAttachment, model::ModelSelection};

use crate::{
    agent_hub::DoubleTap,
    completion::{self, Catalog, Completion, CompletionKind},
    environment::Environment,
    logos,
    models::{self, ModelChoice},
    usage::{self, UsagePanel, UsageStore},
};

/// Largest image the composer uploads (the server's image limit).
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// Completion rows shown at once.
const MAX_COMPLETIONS: usize = 8;

/// What the composer asks its owner to send.
#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub model: ModelSelection,
    /// The user asked to queue (secondary send) rather than steer/start.
    pub queue: bool,
}

pub enum ComposerEvent {
    Submit(Submission),
    Stop,
    /// ←← in the empty composer: open the agents hub.
    OpenAgentHub,
}

/// Whether the thread has an active run, which changes what Enter does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Idle,
    Running { can_steer: bool },
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

#[derive(Clone)]
struct ModelItem(ModelChoice);

impl SearchableListItem for ModelItem {
    type Value = (String, String);

    fn title(&self) -> SharedString {
        self.0.name.clone().into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let id = self
            .0
            .slug()
            .split_once('/')
            .map_or(self.0.slug(), |(_, id)| id);
        h_flex()
            .gap_2()
            .min_w_0()
            .child(div().flex_none().child(self.0.name.clone()))
            .when(id != self.0.name, |this| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(id.to_owned()),
                )
            })
    }

    fn value(&self) -> &Self::Value {
        &self.0.key
    }

    fn matches(&self, query: &str) -> bool {
        self.0.name.to_lowercase().contains(query) || self.0.slug().to_lowercase().contains(query)
    }
}

/// The model list, one section per provider, headed by its logo and name.
struct ModelGroups {
    /// `(provider, models)`.
    groups: Vec<(String, Vec<ModelItem>)>,
    /// The shown sections after the last search: `(group, shown rows)`.
    shown: Vec<(usize, Vec<usize>)>,
}

impl ModelGroups {
    fn new(choices: &[ModelChoice]) -> Self {
        let groups: Vec<(String, Vec<ModelItem>)> = models::by_provider(choices)
            .into_iter()
            .map(|(provider, rows)| {
                let items = rows
                    .into_iter()
                    .map(|ix| ModelItem(choices[ix].clone()))
                    .collect();
                (provider.to_owned(), items)
            })
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
                let rows: Vec<usize> = if provider.contains(&query)
                    || logos::provider_name(provider)
                        .to_lowercase()
                        .contains(&query)
                {
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

    fn render_section_header(
        &self,
        section: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let (group, _) = self.shown.get(section)?;
        let provider = &self.groups[*group].0;
        Some(
            h_flex()
                .gap_1p5()
                .px_2()
                .pt_1p5()
                .pb_0p5()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(logos::provider_logo(provider, px(14.), cx))
                .child(logos::provider_name(provider).to_owned())
                .into_any_element(),
        )
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
    choices: Vec<ModelChoice>,
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
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ComposerEvent> for Composer {}

impl Composer {
    /// `preferred` preselects a model (the thread's, or a project default).
    pub fn new(
        environment: Entity<Environment>,
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
            ComboboxState::new(ModelGroups::new(&[]), Vec::new(), window, cx).searchable(true)
        });
        let usage = cx.new(|cx| UsagePanel::new(environment.clone(), cx));
        let usage_store = UsageStore::global(cx);
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
            // The button shows the cached peak.
            cx.observe(&usage_store, |_, _, cx| cx.notify()),
        ];
        let mut composer = Self {
            environment,
            cwd,
            input,
            images: Vec::new(),
            next_image: 0,
            choices: Vec::new(),
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
            RunState::Running { can_steer: true } => "Steer the run…",
            RunState::Running { can_steer: false } => "Queue a message…",
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
                self.input
                    .update(cx, |input, cx| input.set_value("", window, cx));
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
                self.completions = completion::slash_query(&text, cursor)
                    .map(|query| self.catalog.complete(query))
                    .unwrap_or_default();
                self.completions.truncate(MAX_COMPLETIONS * 4);
                self.completion_ix = 0;
                if self.error.take().is_some() || !self.completions.is_empty() {
                    cx.notify();
                }
                cx.notify();
            }
            // Shift+Enter inserts a newline, yet the textarea still reports it.
            InputEvent::PressEnter {
                secondary,
                shift: false,
            } => self.submit(*secondary, window, cx),
            InputEvent::PressEnter { .. } | InputEvent::Focus | InputEvent::Blur => {}
        }
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

    fn submit(&mut self, queue: bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
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
            queue,
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

        let choices = models::model_choices(config);
        if choices != self.choices {
            let current = self.selection(cx).or_else(|| self.preferred.clone());
            let initial = models::initial_choice(&choices, current.as_ref());
            self.choices = choices;
            let key = initial.map(|ix| self.choices[ix].key.clone());
            let items = ModelGroups::new(&self.choices);
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
                h_flex()
                    .id(SharedString::from(format!(
                        "completion-{}",
                        completion.label
                    )))
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
                            .font_family(theme.mono_font_family.clone())
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
    /// with the levels in the footer.
    fn render_model_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let choice = self.choice(cx);
        let provider = choice.map(|choice| choice.provider.clone());
        let name: SharedString = choice.map_or("Model".into(), |c| c.name.clone().into());
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
    /// usage on this environment; opening it refreshes and lists the limits.
    fn render_usage_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let env = self.environment.read(cx);
        let choice = self.choice(cx);
        let peak = choice.and_then(|choice| {
            let snapshot = self.usage_store.read(cx).snapshot(env.key())?;
            usage::peak(snapshot, &choice.provider)
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
                    .when_some(peak, |this, peak| {
                        this.label(format!("{:.0}%", peak * 100.))
                    })
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
        let running = matches!(self.run_state, RunState::Running { .. });
        let can_steer = matches!(self.run_state, RunState::Running { can_steer: true });
        let completions = self.render_completions(cx);
        let images = self.render_images(cx);
        let model_picker = self.render_model_picker(cx);
        let usage_button = self.render_usage_button(cx);
        let key = |keys: &str| crate::actions::shortcut(keys, cx);
        let stop_tooltip = format!("Stop the run ({})", key("secondary-."));
        let queue_tooltip = format!("Queue after the current run ({})", key("secondary-enter"));
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
            .children(completions)
            .children(images)
            .child(
                div().px_1().pt_1().child(
                    Textarea::new(&self.input)
                        .appearance(false)
                        .text_sm()
                        .aria_label("Message")
                        .on_paste(move |item, _, cx| Self::on_paste(&weak, item, cx)),
                ),
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
                    .when(running, |this| {
                        this.child(
                            Button::new("stop-run")
                                .xsmall()
                                .outline()
                                .icon(IconName::Square)
                                .label("Stop")
                                .tooltip(stop_tooltip)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::Stop))),
                        )
                    })
                    .when(running, |this| {
                        this.child(
                            Button::new("queue-message")
                                .xsmall()
                                .outline()
                                .label("Queue")
                                .tooltip(queue_tooltip)
                                .disabled(self.busy)
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.submit(true, window, cx)
                                    }),
                                ),
                        )
                    })
                    .child(
                        Button::new("send-message")
                            .xsmall()
                            .primary()
                            .icon(IconName::ArrowUp)
                            .label(if can_steer {
                                "Steer"
                            } else if running {
                                "Queue"
                            } else {
                                "Send"
                            })
                            .loading(self.busy)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.submit(false, window, cx)),
                            ),
                    ),
            )
    }
}
