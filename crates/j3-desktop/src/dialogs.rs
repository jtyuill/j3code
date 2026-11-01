//! Dialog bodies: adding a project through a remote directory browser,
//! adding a remote environment, and renaming a thread.

use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, Icon, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
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
use j3_client::t3::filesystem::FilesystemBrowseEntry;

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
    pub fn new(environment: Entity<Environment>, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            self.title.update(cx, |input, cx| input.set_value(title, window, cx));
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
                        .child(Icon::new(IconName::Folder).xsmall().text_color(theme.muted_foreground))
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
            url: cx.new(|cx| InputState::new(window, cx).placeholder("revived or http://revived:3780")),
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
