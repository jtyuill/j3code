//! Dialog bodies: adding a project through a remote directory browser,
//! adding a remote environment, and a custom snooze time.

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonGroup, ButtonVariants as _},
    dialog::{DialogAction, DialogClose, DialogFooter},
    h_flex,
    input::{Input, InputEvent, InputState},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, rems,
};
use j3_client::{
    t3::filesystem::FilesystemBrowseEntry,
    workflow::{CustomSnooze, SnoozeUnit, TimeZone, resolve_custom_snooze},
};
use time::OffsetDateTime;

use crate::environment::Environment;

/// The footer every dialog here uses: Cancel and the primary result.
pub fn footer(confirm: &'static str) -> DialogFooter {
    DialogFooter::new()
        .gap_2()
        .child(DialogClose::new().child(Button::new("dialog-cancel").outline().label("Cancel")))
        .child(DialogAction::new().child(Button::new("dialog-confirm").primary().label(confirm)))
}

/// A project's default title: the last segment of its path.
pub fn title_from_path(path: &str) -> String {
    path.trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}

enum Listing {
    Loading,
    Loaded {
        parent: SharedString,
        entries: Vec<FilesystemBrowseEntry>,
    },
    Failed(SharedString),
}

/// Emitted once the project exists.
pub struct ProjectCreated {
    pub project_id: String,
}

/// Picks a directory on an environment's host and creates a project there.
pub struct ProjectPicker {
    environment: Entity<Environment>,
    path: Entity<InputState>,
    title: Entity<InputState>,
    title_edited: bool,
    listing: Listing,
    revision: u64,
    creating: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ProjectCreated> for ProjectPicker {}

impl ProjectPicker {
    pub fn new(
        environment: Entity<Environment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let path = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("~/Developer/project")
                .default_value("~/")
        });
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let subscriptions = vec![
            cx.subscribe_in(&path, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::Change = event {
                    this.on_path_changed(window, cx);
                }
            }),
            cx.subscribe(&title, |this, _, event: &InputEvent, _| {
                if let InputEvent::Change = event {
                    this.title_edited = true;
                }
            }),
        ];
        let mut picker = Self {
            environment,
            path,
            title,
            title_edited: false,
            listing: Listing::Loading,
            revision: 0,
            creating: false,
            error: None,
            _subscriptions: subscriptions,
        };
        picker.browse(cx);
        picker
    }

    pub fn focus(&self, window: &mut Window, cx: &mut gpui_kit::App) {
        self.path.update(cx, |input, cx| input.focus(window, cx));
    }

    fn on_path_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.title_edited {
            let title = title_from_path(&self.path.read(cx).value());
            self.title
                .update(cx, |input, cx| input.set_value(title, window, cx));
        }
        self.browse(cx);
    }

    fn browse(&mut self, cx: &mut Context<Self>) {
        self.revision += 1;
        let revision = self.revision;
        let partial = self.path.read(cx).value().to_string();
        let connection = self.environment.read(cx).connection().clone();
        self.listing = Listing::Loading;
        cx.spawn(async move |this, cx| {
            let result = connection.browse(&partial).await;
            let _ = this.update(cx, |this, cx| {
                if this.revision != revision {
                    return;
                }
                this.listing = match result {
                    Ok(result) => Listing::Loaded {
                        parent: result.parent_path.into(),
                        entries: result.entries,
                    },
                    Err(e) => Listing::Failed(e.to_string().into()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_entry(&mut self, full_path: &str, window: &mut Window, cx: &mut Context<Self>) {
        let path = format!("{}/", full_path.trim_end_matches('/'));
        self.path.update(cx, |input, cx| {
            input.set_value(path.clone(), window, cx);
            let end = path.len();
            input.set_selected_range(end..end, cx);
        });
        self.on_path_changed(window, cx);
    }

    /// Creates the project; emits [`ProjectCreated`] on success.
    pub fn create(&mut self, cx: &mut Context<Self>) {
        if self.creating {
            return;
        }
        let path = self.path.read(cx).value().trim().to_owned();
        let title = self.title.read(cx).value().trim().to_owned();
        if path.is_empty() {
            self.error = Some("Choose a folder.".into());
            cx.notify();
            return;
        }
        let title = if title.is_empty() {
            title_from_path(&path)
        } else {
            title
        };
        let connection = self.environment.read(cx).connection().clone();
        self.creating = true;
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = connection.create_project(&title, &path).await;
            let _ = this.update(cx, |this, cx| {
                this.creating = false;
                match result {
                    Ok(project) => cx.emit(ProjectCreated {
                        project_id: project.id,
                    }),
                    Err(e) => this.error = Some(e.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for ProjectPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let listing = match &self.listing {
            Listing::Loading => h_flex()
                .p_2()
                .child(Spinner::new().xsmall())
                .into_any_element(),
            Listing::Failed(error) => div()
                .p_2()
                .text_xs()
                .text_color(theme.danger)
                .child(error.clone())
                .into_any_element(),
            Listing::Loaded { parent, entries } => v_flex()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(parent.clone()),
                )
                .when(entries.is_empty(), |this| {
                    this.child(
                        div()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("No folders"),
                    )
                })
                .children(entries.iter().map(|entry| {
                    let full_path = entry.full_path.clone();
                    h_flex()
                        .id(SharedString::from(format!("browse-{}", entry.full_path)))
                        .gap_2()
                        .px_2()
                        .py_0p5()
                        .rounded(theme.radius)
                        .text_sm()
                        .hover(|this| this.bg(theme.accent))
                        .child(
                            Icon::new(IconName::Folder)
                                .xsmall()
                                .text_color(theme.muted_foreground),
                        )
                        .child(entry.name.clone())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_entry(&full_path, window, cx);
                        }))
                }))
                .into_any_element(),
        };
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_medium().child("Folder"))
                    .child(Input::new(&self.path).small())
                    .child(
                        div()
                            .id("folder-listing")
                            .h(rems(13.))
                            .overflow_y_scroll()
                            .rounded(theme.radius)
                            .border_1()
                            .border_color(theme.border)
                            .p_1()
                            .child(listing),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_medium().child("Name"))
                    .child(Input::new(&self.title).small()),
            )
            .when(self.creating, |this| {
                this.child(
                    h_flex()
                        .gap_2()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(Spinner::new().xsmall())
                        .child("Adding project…"),
                )
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(theme.danger).child(error))
            })
    }
}

/// A label and URL for a new remote environment.
pub struct EnvironmentForm {
    label: Entity<InputState>,
    url: Entity<InputState>,
    error: Option<SharedString>,
}

impl EnvironmentForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            label: cx.new(|cx| InputState::new(window, cx).placeholder("revived")),
            url: cx.new(|cx| {
                InputState::new(window, cx).placeholder("revived or http://revived:3780")
            }),
            error: None,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut gpui_kit::App) {
        self.label.update(cx, |input, cx| input.focus(window, cx));
    }

    pub fn values(&self, cx: &gpui_kit::App) -> (String, String) {
        (
            self.label.read(cx).value().to_string(),
            self.url.read(cx).value().to_string(),
        )
    }

    pub fn set_error(&mut self, error: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(error.into());
        cx.notify();
    }
}

impl Render for EnvironmentForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_medium().child("Name"))
                    .child(Input::new(&self.label).small()),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().font_medium().child("Host or URL"))
                    .child(Input::new(&self.url).small())
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Port 3780 is used when none is given."),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(theme.danger).child(error))
            })
    }
}

/// How a custom snooze is given (t3's "Schedule type").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnoozeSchedule {
    At,
    After,
}

/// t3's custom snooze: a local date and time, or a duration from now.
pub struct SnoozeForm {
    schedule: SnoozeSchedule,
    date: Entity<InputState>,
    time: Entity<InputState>,
    amount: Entity<InputState>,
    unit: SnoozeUnit,
    error: Option<SharedString>,
}

impl SnoozeForm {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            schedule: SnoozeSchedule::After,
            date: cx.new(|cx| InputState::new(window, cx).placeholder("YYYY-MM-DD")),
            time: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("HH:MM")
                    .default_value("09:00")
            }),
            amount: cx.new(|cx| InputState::new(window, cx).default_value("1")),
            unit: SnoozeUnit::Hours,
            error: None,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut gpui_kit::App) {
        let input = match self.schedule {
            SnoozeSchedule::At => &self.date,
            SnoozeSchedule::After => &self.amount,
        };
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    /// The wake time, or why the input has none (shown under the form).
    pub fn wake(&mut self, cx: &mut Context<Self>) -> Option<OffsetDateTime> {
        let date = self.date.read(cx).value();
        let time = self.time.read(cx).value();
        let amount = self.amount.read(cx).value();
        let input = match self.schedule {
            SnoozeSchedule::At => CustomSnooze::At {
                date: date.trim(),
                time: time.trim(),
            },
            SnoozeSchedule::After => CustomSnooze::After {
                amount: amount.trim(),
                unit: self.unit,
            },
        };
        let wake = resolve_custom_snooze(input, OffsetDateTime::now_utc(), &TimeZone::system());
        self.error = wake
            .is_none()
            .then(|| "That snooze time has passed or isn’t valid. Choose another time.".into());
        cx.notify();
        wake
    }

    fn segmented<T: Copy + PartialEq + 'static>(
        id: &'static str,
        options: &[(T, &'static str)],
        current: T,
        cx: &mut Context<Self>,
        set: fn(&mut Self, T),
    ) -> ButtonGroup {
        let values: Vec<T> = options.iter().map(|(value, _)| *value).collect();
        ButtonGroup::new(id)
            .outline()
            .small()
            .children(options.iter().map(|(value, label)| {
                Button::new(SharedString::from(format!("{id}-{}", label.to_lowercase())))
                    .label(*label)
                    .selected(*value == current)
            }))
            .on_click(cx.listener(move |this, clicked: &Vec<usize>, window, cx| {
                if let Some(&ix) = clicked.first() {
                    set(this, values[ix]);
                    this.error = None;
                    this.focus(window, cx);
                    cx.notify();
                }
            }))
    }
}

impl Render for SnoozeForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let schedule = Self::segmented(
            "snooze-schedule",
            &[
                (SnoozeSchedule::After, "Duration"),
                (SnoozeSchedule::At, "Date and time"),
            ],
            self.schedule,
            cx,
            |this, value| this.schedule = value,
        );
        let unit = Self::segmented(
            "snooze-unit",
            &[
                (SnoozeUnit::Minutes, "Minutes"),
                (SnoozeUnit::Hours, "Hours"),
                (SnoozeUnit::Days, "Days"),
            ],
            self.unit,
            cx,
            |this, value| this.unit = value,
        );
        let theme = cx.theme();
        let field = |label: &'static str, input: &Entity<InputState>| {
            v_flex()
                .gap_1()
                .flex_1()
                .child(div().text_sm().font_medium().child(label))
                .child(Input::new(input).small())
        };
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Choose when the thread returns."),
            )
            .child(schedule)
            .map(|this| match self.schedule {
                SnoozeSchedule::At => this.child(
                    h_flex()
                        .gap_2()
                        .child(field("Date", &self.date))
                        .child(field("Time", &self.time)),
                ),
                SnoozeSchedule::After => this.child(field("Amount", &self.amount)).child(unit),
            })
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(theme.danger).child(error))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_titles_come_from_the_last_path_segment() {
        assert_eq!(title_from_path("~/Developer/j3code/"), "j3code");
        assert_eq!(title_from_path("/tmp/j3-desktop-smoke"), "j3-desktop-smoke");
        assert_eq!(title_from_path("  /srv/app// "), "app");
        assert_eq!(title_from_path("/"), "");
    }
}
