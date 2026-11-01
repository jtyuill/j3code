//! The message composer: multi-line text, pasted images (uploaded as they
//! are added), model and thinking pickers, and `/` completion. It reports a
//! [`Submission`]; its owner sends it and calls [`Composer::finish`].

use std::{path::Path, sync::Arc};

use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Enter, Escape, IndentInline, InputEvent, MoveDown, MoveUp, Textarea, TextareaState},
    select::{Select, SelectEvent, SelectItem, SelectState},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardEntry, ClipboardItem, Context, Entity, EventEmitter,
    FocusHandle, Focusable, Image, ImageFormat, ImageSource, InteractiveElement as _, IntoElement,
    ObjectFit, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Subscription, WeakEntity, Window, div, img, rems,
};
use j3_client::t3::{chat_attachment::ChatAttachment, model::ModelSelection};

use crate::{
    completion::{self, Catalog, Completion, CompletionKind},
    environment::Environment,
    models::{self, ModelChoice},
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

impl SelectItem for ModelItem {
    type Value = (String, String);

    fn title(&self) -> SharedString {
        self.0.name.clone().into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex().child(self.0.name.clone()).child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(self.0.slug().to_owned()),
        )
    }

    fn value(&self) -> &Self::Value {
        // The pair is stored inline so the select can compare by reference.
        &self.0.key
    }

    fn matches(&self, query: &str) -> bool {
        let query = query.to_lowercase();
        self.0.name.to_lowercase().contains(&query) || self.0.slug().to_lowercase().contains(&query)
    }
}

#[derive(Clone)]
struct LevelItem {
    id: String,
    label: SharedString,
}

impl SelectItem for LevelItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &String {
        &self.id
    }
}

pub struct Composer {
    environment: Entity<Environment>,
    cwd: String,
    input: Entity<TextareaState>,
    images: Vec<PendingImage>,
    next_image: u64,
    choices: Vec<ModelChoice>,
    model: Entity<SelectState<Vec<ModelItem>>>,
    level_descriptor: Option<String>,
    levels: Entity<SelectState<Vec<LevelItem>>>,
    preferred: Option<ModelSelection>,
    catalog: Catalog,
    workspace_requested: bool,
    completions: Vec<Completion>,
    completion_ix: usize,
    run_state: RunState,
    busy: bool,
    error: Option<SharedString>,
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
        let model = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx).searchable(true));
        let levels = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&input, window, Self::on_input_event),
            cx.subscribe_in(
                &model,
                window,
                |this, _, event: &SelectEvent<Vec<ModelItem>>, window, cx| {
                    let SelectEvent::Confirm(Some(key)) = event else {
                        return;
                    };
                    if let Some(choice) = this.choices.iter().find(|c| &c.key == key).cloned() {
                        this.preferred = Some(models::model_selection(&choice, None));
                        this.sync_levels(&choice, window, cx);
                    }
                },
            ),
            cx.observe_in(&environment, window, |this, _, window, cx| {
                this.sync_config(window, cx)
            }),
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
            levels,
            preferred,
            catalog: Catalog::default(),
            workspace_requested: false,
            completions: Vec::new(),
            completion_ix: 0,
            run_state: RunState::Idle,
            busy: false,
            error: None,
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
            InputEvent::PressEnter { secondary, .. } => self.submit(*secondary, window, cx),
            InputEvent::Focus | InputEvent::Blur => {}
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

    /// The model selection to send, with the chosen thinking level.
    pub fn selection(&self, cx: &App) -> Option<ModelSelection> {
        let key = self.model.read(cx).selected_value()?;
        let choice = self.choices.iter().find(|c| &c.key == key)?;
        let level = self.levels.read(cx).selected_value().cloned();
        Some(models::model_selection(
            choice,
            self.level_descriptor.as_deref().zip(level.as_deref()),
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
            let items: Vec<ModelItem> = self.choices.iter().cloned().map(ModelItem).collect();
            self.model.update(cx, |select, cx| {
                select.set_items(items, window, cx);
                select.set_selected_index(
                    initial.map(gpui_kit::component::IndexPath::new),
                    window,
                    cx,
                );
            });
            if let Some(choice) = initial.map(|ix| self.choices[ix].clone()) {
                self.sync_levels(&choice, window, cx);
            }
        }
        cx.notify();
    }

    fn sync_levels(&mut self, choice: &ModelChoice, window: &mut Window, cx: &mut Context<Self>) {
        let state = self.environment.read(cx).state().clone();
        let descriptor = state
            .config
            .as_ref()
            .and_then(|config| models::thinking_descriptor(config, choice))
            .cloned();
        let preferred_level = self.levels.read(cx).selected_value().cloned().or_else(|| {
            let descriptor = descriptor.as_ref()?;
            self.preferred
                .as_ref()
                .and_then(|p| models::selection_level(p, &descriptor.id))
                .map(str::to_owned)
        });
        self.level_descriptor = descriptor.as_ref().map(|d| d.id.clone());
        let items: Vec<LevelItem> = descriptor
            .iter()
            .flat_map(|d| &d.options)
            .map(|option| LevelItem {
                id: option.id.clone(),
                label: option.label.clone().into(),
            })
            .collect();
        let initial = descriptor
            .as_ref()
            .and_then(|d| models::initial_level(d, preferred_level.as_deref()));
        self.levels.update(cx, |select, cx| {
            select.set_items(items, window, cx);
            match &initial {
                Some(level) => select.set_selected_value(level, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
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
        let has_levels = self.level_descriptor.is_some();
        let completions = self.render_completions(cx);
        let images = self.render_images(cx);
        let key = |keys: &str| crate::actions::shortcut(keys, cx);
        let (enter, queue_key, newline) =
            (key("enter"), key("secondary-enter"), key("shift-enter"));
        let stop_tooltip = format!("Stop the run ({})", key("secondary-."));
        let queue_tooltip = format!("Queue after the current run ({queue_key})");
        let hint = if running {
            if can_steer {
                format!("{enter} steer · {queue_key} queue")
            } else {
                format!("{enter} queue")
            }
        } else {
            format!("{enter} send · {newline} newline")
        };
        let theme = cx.theme();
        let completions_open = !self.completions.is_empty();
        v_flex()
            .id("composer")
            .key_context("Composer")
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
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .pt_1()
                    .child(
                        Select::new(&self.model)
                            .xsmall()
                            .appearance(false)
                            .placeholder("Model")
                            .search_placeholder("Search models")
                            .menu_width(rems(20.))
                            .accessibility_label("Model")
                            .w(rems(12.)),
                    )
                    .when(has_levels, |this| {
                        this.child(
                            Select::new(&self.levels)
                                .xsmall()
                                .appearance(false)
                                .title_prefix("Thinking: ")
                                .accessibility_label("Thinking level")
                                .w(rems(9.)),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(hint),
                    )
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
