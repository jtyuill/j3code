//! Settings, as a full-window view: the thread workflow and its options,
//! appearance (zoom, fonts and font sizes), notifications, environments, projects and archived threads. Preference
//! changes apply and save at once; environment and project commands go to
//! the workspace.

use std::{collections::HashMap, rc::Rc};

use gpui_kit::component::{
    ActiveTheme as _, IconName, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    h_flex,
    setting::{
        NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage,
        Settings,
    },
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, WeakEntity, Window, div, rems,
};
use j3_client::workflow::{AgentOrder, StatusIndicators};

use crate::{
    actions::{CloseSettings, SETTINGS},
    archived::{ArchivedView, Unarchived},
    environment::{Environment, distinct_environments},
    preferences::Preferences,
    titlebar,
    ui_settings::{
        CODE_FONT_SIZE_MAX, CODE_FONT_SIZE_MIN, DEFAULT_CODE_FONT_SIZE,
        DEFAULT_INTERFACE_FONT_SIZE, DEFAULT_TERMINAL_FONT_SIZE, DEFAULT_TEXT_MEASURE,
        INTERFACE_FONT_SIZE_MAX, INTERFACE_FONT_SIZE_MIN, TERMINAL_FONT_SIZE_MAX,
        TERMINAL_FONT_SIZE_MIN, TEXT_MEASURE_MAX, TEXT_MEASURE_MIN, UiSettings, Workflow,
        ZOOM_LEVELS,
    },
};

/// A request from Settings; the workspace carries it out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsEvent {
    Close,
    AddEnvironment,
    RemoveEnvironment {
        env: SharedString,
    },
    AddProject {
        env: SharedString,
    },
    RemoveProject {
        env: SharedString,
        project_id: String,
    },
    OpenThread {
        env: SharedString,
        thread_id: String,
    },
}

/// A page Settings can open on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPage {
    General,
    Archived,
}

impl SettingsPage {
    /// Its index among the rendered pages (General, Notifications,
    /// Environments, Projects, Archived threads).
    fn index(self) -> usize {
        match self {
            Self::General => 0,
            Self::Archived => 4,
        }
    }
}

pub struct SettingsView {
    preferences: Entity<Preferences>,
    environments: Vec<Entity<Environment>>,
    archived: HashMap<SharedString, Entity<ArchivedView>>,
    /// Gives each opening its own [`Settings`] state, so it starts on the
    /// requested page.
    generation: u64,
    page: SettingsPage,
    focus_handle: FocusHandle,
    /// Installed font families, for the font pickers.
    fonts: Rc<[SharedString]>,
    _environment_subscriptions: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(
        preferences: Entity<Preferences>,
        environments: Vec<Entity<Environment>>,
        page: SettingsPage,
        generation: u64,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscriptions = vec![cx.observe(&preferences, |_, _, cx| cx.notify())];
        let mut view = Self {
            preferences,
            environments: Vec::new(),
            archived: HashMap::new(),
            generation,
            page,
            focus_handle: cx.focus_handle(),
            fonts: cx
                .text_system()
                .all_font_names()
                .into_iter()
                .map(SharedString::from)
                .collect(),
            _environment_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        view.set_environments(environments, cx);
        view
    }

    pub fn set_environments(
        &mut self,
        environments: Vec<Entity<Environment>>,
        cx: &mut Context<Self>,
    ) {
        self._environment_subscriptions = environments
            .iter()
            .map(|env| cx.observe(env, |_, _, cx| cx.notify()))
            .collect();
        let keys: Vec<SharedString> = environments
            .iter()
            .map(|env| env.read(cx).key().clone())
            .collect();
        self.archived.retain(|key, _| keys.contains(key));
        for env in &environments {
            let key = env.read(cx).key().clone();
            if self.archived.contains_key(&key) {
                continue;
            }
            let view = cx.new(|cx| ArchivedView::new(env.clone(), cx));
            let subscription_key = key.clone();
            self._subscriptions
                .push(cx.subscribe(&view, move |_, _, event: &Unarchived, cx| {
                    cx.emit(SettingsEvent::OpenThread {
                        env: subscription_key.clone(),
                        thread_id: event.thread_id.clone(),
                    })
                }));
            self.archived.insert(key, view);
        }
        self.environments = environments;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        self.focus_handle.focus(window, cx);
    }

    fn emit(view: &WeakEntity<Self>, event: SettingsEvent, cx: &mut App) {
        let _ = view.update(cx, |_, cx| cx.emit(event));
    }

    /// One segmented control: a button per option, the current one selected.
    fn segmented<T: Copy + PartialEq + 'static>(
        id: &'static str,
        options: &[(T, &'static str)],
        current: T,
        preferences: &Entity<Preferences>,
        set: fn(&mut UiSettings, T),
    ) -> ButtonGroup {
        let values: Vec<T> = options.iter().map(|(value, _)| *value).collect();
        let preferences = preferences.clone();
        ButtonGroup::new(id)
            .outline()
            .small()
            .children(options.iter().map(|(value, label)| {
                Button::new(SharedString::from(format!("{id}-{}", label.to_lowercase())))
                    .label(*label)
                    .selected(*value == current)
            }))
            .on_click(move |clicked: &Vec<usize>, _, cx| {
                let Some(&ix) = clicked.first() else {
                    return;
                };
                let value = values[ix];
                preferences.update(cx, |preferences, cx| {
                    preferences.update_ui(|ui| set(ui, value), cx)
                });
            })
    }

    fn general_page(&self) -> SettingPage {
        let preferences = self.preferences.clone();
        let segmented_item =
            |title: &'static str,
             description: &'static str,
             render: fn(&Entity<Preferences>, &App) -> ButtonGroup| {
                let preferences = preferences.clone();
                SettingItem::new(
                    title,
                    SettingField::render(move |_, _, cx| render(&preferences, cx)),
                )
                .description(description)
            };
        let shelf = preferences.clone();
        let shelf_set = preferences.clone();
        SettingPage::new("General")
            .icon(IconName::Settings)
            .default_open(true)
            .group(SettingGroup::new().title("Thread workflow").item(segmented_item(
                "Workflow",
                "T3 settles finished threads onto shelves. herdr lists every agent by what it needs from you.",
                |preferences, cx| {
                    Self::segmented(
                        "workflow",
                        &[(Workflow::T3, "T3"), (Workflow::Herdr, "herdr")],
                        preferences.read(cx).ui().workflow,
                        preferences,
                        |ui, value| ui.workflow = value,
                    )
                },
            )))
            .group(
                SettingGroup::new().title("T3").item(
                    SettingItem::new(
                        "Working shelf",
                        SettingField::switch(
                            move |cx| shelf.read(cx).ui().working_shelf,
                            move |value, cx| {
                                shelf_set.update(cx, |preferences, cx| {
                                    preferences.update_ui(|ui| ui.working_shelf = value, cx)
                                })
                            },
                        )
                        .default_value(true),
                    )
                    .description(
                        "Running threads wait on their own shelf. Turn off to keep them in Active and arrange Active by dragging.",
                    ),
                ),
            )
            .group(
                SettingGroup::new()
                    .title("herdr")
                    .item(segmented_item(
                        "Status indicators",
                        "Dots: ● working, blocked, done; ○ idle. Symbols: ◐ working, × blocked, ✓ done, ○ idle.",
                        |preferences, cx| {
                            Self::segmented(
                                "status-indicators",
                                &[
                                    (StatusIndicators::Dots, "Dots"),
                                    (StatusIndicators::Symbols, "Symbols"),
                                ],
                                preferences.read(cx).ui().status_indicators,
                                preferences,
                                |ui, value| ui.status_indicators = value,
                            )
                        },
                    ))
                    .item(segmented_item(
                        "Agent order",
                        "By space follows the project order. By attention puts blocked, then done agents first.",
                        |preferences, cx| {
                            Self::segmented(
                                "agent-order",
                                &[
                                    (AgentOrder::Spaces, "By space"),
                                    (AgentOrder::Attention, "By attention"),
                                ],
                                preferences.read(cx).ui().agent_order,
                                preferences,
                                |ui, value| ui.agent_order = value,
                            )
                        },
                    )),
            )
            .group(self.appearance_group())
            .group(SettingGroup::new().title("Conversation").item({
                let read = preferences.clone();
                let write = preferences.clone();
                SettingItem::new(
                    "Text width",
                    SettingField::number_input(
                        NumberFieldOptions {
                            min: TEXT_MEASURE_MIN as f64,
                            max: TEXT_MEASURE_MAX as f64,
                            step: 2.,
                        },
                        move |cx| read.read(cx).ui().text_measure as f64,
                        move |value, cx| {
                            write.update(cx, |preferences, cx| {
                                preferences.update_ui(
                                    |ui| ui.text_measure = value as f32,
                                    cx,
                                )
                            })
                        },
                    )
                    .default_value(DEFAULT_TEXT_MEASURE as f64),
                )
                .description(
                    "Widest the conversation text and composer get, in rem (the interface font size). Lower is a tighter, easier-to-read column.",
                )
            })
            .item({
                let read = preferences.clone();
                let write = preferences.clone();
                SettingItem::new(
                    "Render math",
                    SettingField::switch(
                        move |cx| read.read(cx).ui().math_rendering,
                        move |value, cx| {
                            write.update(cx, |preferences, cx| {
                                preferences.update_ui(|ui| ui.math_rendering = value, cx)
                            })
                        },
                    )
                    .default_value(true),
                )
                .description("Typeset LaTeX in messages.")
            }))
    }

    /// Zoom, the two font families and the font sizes.
    fn appearance_group(&self) -> SettingGroup {
        let size_item = |title: &'static str,
                         description: &'static str,
                         (min, max, default): (f32, f32, f32),
                         get: fn(&UiSettings) -> f32,
                         set: fn(&mut UiSettings, f32)| {
            let read = self.preferences.clone();
            let write = self.preferences.clone();
            SettingItem::new(
                title,
                SettingField::number_input(
                    NumberFieldOptions {
                        min: min as f64,
                        max: max as f64,
                        step: 1.,
                    },
                    move |cx| get(read.read(cx).ui()) as f64,
                    move |value, cx| {
                        let size = (value as f32).round().clamp(min, max);
                        write.update(cx, |preferences, cx| {
                            preferences.update_ui(|ui| set(ui, size), cx)
                        })
                    },
                )
                .default_value(default as f64),
            )
            .description(description)
        };
        let font_item = |title: &'static str,
                         description: &'static str,
                         default_label: &'static str,
                         get: fn(&UiSettings) -> Option<&str>,
                         set: fn(&mut UiSettings, Option<String>)| {
            let options = std::iter::once((SharedString::default(), default_label.into()))
                .chain(self.fonts.iter().map(|font| (font.clone(), font.clone())))
                .collect();
            let read = self.preferences.clone();
            let write = self.preferences.clone();
            SettingItem::new(
                title,
                SettingField::scrollable_dropdown(
                    options,
                    move |cx| {
                        get(read.read(cx).ui())
                            .unwrap_or_default()
                            .to_owned()
                            .into()
                    },
                    move |value: SharedString, cx| {
                        let font = (!value.is_empty()).then(|| value.to_string());
                        write.update(cx, |preferences, cx| {
                            preferences.update_ui(|ui| set(ui, font), cx)
                        })
                    },
                )
                .default_value(SharedString::default()),
            )
            .description(description)
        };
        let read = self.preferences.clone();
        let write = self.preferences.clone();
        SettingGroup::new()
            .title("Appearance")
            .item(
                SettingItem::new(
                    "Zoom",
                    SettingField::dropdown(
                        ZOOM_LEVELS
                            .iter()
                            .map(|level| {
                                (
                                    SharedString::from(level.to_string()),
                                    SharedString::from(format!("{:.0}%", level * 100.)),
                                )
                            })
                            .collect(),
                        move |cx| read.read(cx).ui().zoom().to_string().into(),
                        move |value: SharedString, cx| {
                            let Ok(zoom) = value.parse::<f32>() else {
                                return;
                            };
                            write.update(cx, |preferences, cx| {
                                preferences.update_ui(|ui| ui.zoom = zoom, cx)
                            })
                        },
                    )
                    .default_value(SharedString::from(1f32.to_string())),
                )
                .description(
                    "Scales the whole window. View › Zoom In and Zoom Out step through the sizes.",
                ),
            )
            .item(font_item(
                "Interface font",
                "Used for messages, the sidebar and controls.",
                "System default",
                |ui| ui.ui_font.as_deref(),
                |ui, font| ui.ui_font = font,
            ))
            .item(font_item(
                "Code font",
                "Used for tool calls, their output and code.",
                "Default monospace",
                |ui| ui.mono_font.as_deref(),
                |ui, font| ui.mono_font = font,
            ))
            .item(size_item(
                "Interface font size",
                "In px at 100% zoom. Spacing and controls grow and shrink with it.",
                (
                    INTERFACE_FONT_SIZE_MIN,
                    INTERFACE_FONT_SIZE_MAX,
                    DEFAULT_INTERFACE_FONT_SIZE,
                ),
                UiSettings::interface_font_size,
                |ui, size| ui.interface_font_size = size,
            ))
            .item(size_item(
                "Code font size",
                "In px at 100% zoom. Used for tool output and the file preview.",
                (
                    CODE_FONT_SIZE_MIN,
                    CODE_FONT_SIZE_MAX,
                    DEFAULT_CODE_FONT_SIZE,
                ),
                UiSettings::code_font_size,
                |ui, size| ui.code_font_size = size,
            ))
            .item(size_item(
                "Terminal font size",
                "In px at 100% zoom.",
                (
                    TERMINAL_FONT_SIZE_MIN,
                    TERMINAL_FONT_SIZE_MAX,
                    DEFAULT_TERMINAL_FONT_SIZE,
                ),
                UiSettings::terminal_font_size,
                |ui, size| ui.terminal_font_size = size,
            ))
    }

    fn notifications_page(&self) -> SettingPage {
        let switch = |title: &'static str,
                      description: &'static str,
                      get: fn(&UiSettings) -> bool,
                      set: fn(&mut UiSettings, bool)| {
            let read = self.preferences.clone();
            let write = self.preferences.clone();
            SettingItem::new(
                title,
                SettingField::switch(
                    move |cx| get(read.read(cx).ui()),
                    move |value, cx| {
                        write.update(cx, |preferences, cx| {
                            preferences.update_ui(|ui| set(ui, value), cx)
                        })
                    },
                )
                .default_value(true),
            )
            .description(description)
        };
        SettingPage::new("Notifications")
            .icon(IconName::Bell)
            .group(
                SettingGroup::new()
                    .description(
                        "When a thread needs an answer or a thread you aren’t viewing finishes.",
                    )
                    .item(switch(
                        "Toasts",
                        "Show a notification in the window.",
                        |ui| ui.toasts,
                        |ui, value| ui.toasts = value,
                    ))
                    .item(switch(
                        "Sounds",
                        "Chime while j3 is in the background: one sound for a finished run, another for a question.",
                        |ui| ui.sounds,
                        |ui, value| ui.sounds = value,
                    )),
            )
    }

    fn environments_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.weak_entity();
        let rows = self.environments.iter().map(|env| {
            let env = env.clone();
            let view = view.clone();
            SettingItem::render(move |_, _, cx| {
                let environment = env.read(cx);
                let key = environment.key().clone();
                let status: SharedString = environment.status_label().unwrap_or("Connected").into();
                let detail: SharedString = environment.status_detail().into();
                let theme = cx.theme();
                h_flex()
                    .id(SharedString::from(format!("environment-{key}")))
                    .w_full()
                    .gap_3()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().child(environment.label().clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .truncate()
                                    .child(environment.connection().http_base().to_owned()),
                            ),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("environment-status-{key}")))
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(status)
                            .tooltip(move |window, cx| {
                                Tooltip::new(detail.clone()).build(window, cx)
                            }),
                    )
                    .map(|this| {
                        if environment.is_local() {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("This computer"),
                            )
                        } else {
                            let view = view.clone();
                            this.child(
                                Button::new(SharedString::from(format!(
                                    "remove-environment-{key}"
                                )))
                                .small()
                                .ghost()
                                .label("Remove…")
                                .on_click(move |_, _, cx| {
                                    Self::emit(
                                        &view,
                                        SettingsEvent::RemoveEnvironment { env: key.clone() },
                                        cx,
                                    )
                                }),
                            )
                        }
                    })
            })
        });
        let add = view.clone();
        SettingPage::new("Environments").icon(IconName::Globe).group(
            SettingGroup::new()
                .description("The j3 servers this window connects to. This computer’s own server is always listed.")
                .items(rows)
                .footer(move |_, _| {
                    let add = add.clone();
                    Button::new("add-environment")
                        .small()
                        .outline()
                        .icon(IconName::Plus)
                        .label("Add environment…")
                        .on_click(move |_, _, cx| Self::emit(&add, SettingsEvent::AddEnvironment, cx))
                }),
        )
    }

    fn projects_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.weak_entity();
        let groups = distinct_environments(&self.environments, cx)
            .into_iter()
            .map(|env| {
                let environment = env.read(cx);
                let key = environment.key().clone();
                let shell = &environment.state().shell;
                let rows: Vec<SettingItem> = shell
                    .projects
                    .iter()
                    .map(|project| {
                        let view = view.clone();
                        let key = key.clone();
                        let project_id = project.id.clone();
                        let title: SharedString = project.title.clone().into();
                        let root: SharedString = project.workspace_root.clone().into();
                        SettingItem::render(move |_, _, cx| {
                            let view = view.clone();
                            let event = SettingsEvent::RemoveProject {
                                env: key.clone(),
                                project_id: project_id.clone(),
                            };
                            h_flex()
                                .w_full()
                                .gap_3()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().text_sm().child(title.clone()))
                                        .child(
                                            div()
                                                .text_xs()
                                                .truncate()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(root.clone()),
                                        ),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "remove-project-{key}-{project_id}"
                                    )))
                                    .small()
                                    .ghost()
                                    .label("Remove…")
                                    .on_click(move |_, _, cx| Self::emit(&view, event.clone(), cx)),
                                )
                        })
                    })
                    .collect();
                let empty = rows.is_empty();
                let add = view.clone();
                let add_key = key.clone();
                SettingGroup::new()
                    .title(environment.label().clone())
                    .when_empty(empty, shell.loaded)
                    .items(rows)
                    .footer(move |_, _| {
                        let add = add.clone();
                        let env = add_key.clone();
                        Button::new(SharedString::from(format!("add-project-{env}")))
                            .small()
                            .outline()
                            .icon(IconName::Plus)
                            .label("Add project…")
                            .on_click(move |_, _, cx| {
                                Self::emit(&add, SettingsEvent::AddProject { env: env.clone() }, cx)
                            })
                    })
            })
            .collect::<Vec<_>>();
        SettingPage::new("Projects")
            .icon(IconName::Folder)
            .groups(groups)
    }

    fn archived_page(&self, cx: &App) -> SettingPage {
        let groups = distinct_environments(&self.environments, cx)
            .into_iter()
            .filter_map(|env| {
                let key = env.read(cx).key().clone();
                let view = self.archived.get(&key)?.clone();
                Some(
                    SettingGroup::new()
                        .title(env.read(cx).label().clone())
                        .item(SettingItem::render(move |_, _, _| view.clone())),
                )
            })
            .collect::<Vec<_>>();
        SettingPage::new("Archived threads")
            .icon(IconName::Inbox)
            .groups(groups)
    }
}

/// A group's placeholder row while it has nothing to list.
trait EmptyGroup {
    fn when_empty(self, empty: bool, loaded: bool) -> Self;
}

impl EmptyGroup for SettingGroup {
    fn when_empty(self, empty: bool, loaded: bool) -> Self {
        if !empty {
            return self;
        }
        let text = if loaded {
            "No projects"
        } else {
            "Connecting…"
        };
        self.item(SettingItem::render(move |_, _, cx| {
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(text)
        }))
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pages = [
            self.general_page(),
            self.notifications_page(),
            self.environments_page(cx),
            self.projects_page(cx),
            self.archived_page(cx),
        ];
        // Settings covers the whole window, so its header row clears the
        // traffic lights.
        let inset = titlebar::traffic_lights_inset(window);
        let theme = cx.theme();
        v_flex()
            .id("settings-view")
            .key_context(SETTINGS)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|_, _: &CloseSettings, _, cx| cx.emit(SettingsEvent::Close)))
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h(rems(titlebar::ROW_HEIGHT))
                    .px_4()
                    .when_some(inset, |this, inset| this.pl(inset))
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_sm().font_medium().child("Settings"))
                    .child(
                        Button::new("close-settings")
                            .small()
                            .outline()
                            .label("Done")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::Close))),
                    ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    Settings::new(ElementId::NamedInteger("settings".into(), self.generation))
                        .default_selected_index(SelectIndex {
                            page_ix: self.page.index(),
                            group_ix: None,
                        })
                        .pages(pages),
                ),
            )
    }
}
