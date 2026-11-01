//! A new thread: pick the environment and project, write the first message,
//! and launch.

use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, IndexPath, Sizable as _,
    h_flex,
    select::{Select, SelectEvent, SelectItem, SelectState},
    v_flex,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div, rems,
};

use crate::{
    composer::{Composer, ComposerEvent, Submission},
    environment::Environment,
};

pub enum NewThreadEvent {
    Launched { env: SharedString, thread_id: String },
}

#[derive(Clone)]
struct EnvironmentItem {
    key: SharedString,
    label: SharedString,
}

impl SelectItem for EnvironmentItem {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.label.clone()
    }

    fn value(&self) -> &SharedString {
        &self.key
    }
}

#[derive(Clone)]
struct ProjectItem {
    id: String,
    title: SharedString,
    root: SharedString,
}

impl SelectItem for ProjectItem {
    type Value = String;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        v_flex().child(self.title.clone()).child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(self.root.clone()),
        )
    }

    fn value(&self) -> &String {
        &self.id
    }
}

pub struct NewThreadView {
    environments: Vec<Entity<Environment>>,
    environment: Entity<Environment>,
    project_id: Option<String>,
    environment_select: Entity<SelectState<Vec<EnvironmentItem>>>,
    project_select: Entity<SelectState<Vec<ProjectItem>>>,
    projects: Vec<ProjectItem>,
    composer: Option<Entity<Composer>>,
    focus_handle: FocusHandle,
    _composer_subscription: Option<Subscription>,
    _environment_subscription: Subscription,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NewThreadEvent> for NewThreadView {}

impl NewThreadView {
    pub fn new(
        environments: Vec<Entity<Environment>>,
        environment: Entity<Environment>,
        project_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let items: Vec<EnvironmentItem> = environments
            .iter()
            .map(|env| {
                let env = env.read(cx);
                EnvironmentItem {
                    key: env.key().clone(),
                    label: env.label().clone(),
                }
            })
            .collect();
        let selected = environments
            .iter()
            .position(|env| env == &environment)
            .map(IndexPath::new);
        let environment_select = cx.new(|cx| SelectState::new(items, selected, window, cx));
        let project_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx).searchable(true));
        let subscriptions = vec![
            cx.subscribe_in(
                &environment_select,
                window,
                |this, _, event: &SelectEvent<Vec<EnvironmentItem>>, window, cx| {
                    let SelectEvent::Confirm(Some(key)) = event else {
                        return;
                    };
                    if let Some(env) = this.environments.iter().find(|e| e.read(cx).key() == key).cloned() {
                        this.set_environment(env, None, window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &project_select,
                window,
                |this, _, event: &SelectEvent<Vec<ProjectItem>>, window, cx| {
                    if let SelectEvent::Confirm(Some(id)) = event {
                        this.project_id = Some(id.clone());
                        this.sync_composer(window, cx);
                    }
                },
            ),
        ];
        let environment_subscription = cx.observe_in(&environment, window, |this, _, window, cx| {
            this.sync_projects(window, cx)
        });
        let mut view = Self {
            environments,
            environment,
            project_id,
            environment_select,
            project_select,
            projects: Vec::new(),
            composer: None,
            focus_handle: cx.focus_handle(),
            _composer_subscription: None,
            _environment_subscription: environment_subscription,
            _subscriptions: subscriptions,
        };
        view.sync_projects(window, cx);
        view
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        if let Some(composer) = &self.composer {
            composer.update(cx, |composer, cx| composer.focus(window, cx));
        }
    }

    fn set_environment(
        &mut self,
        environment: Entity<Environment>,
        project_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._environment_subscription = cx.observe_in(&environment, window, |this, _, window, cx| {
            this.sync_projects(window, cx)
        });
        self.environment = environment;
        self.project_id = project_id;
        self.projects.clear();
        self.sync_projects(window, cx);
    }

    fn sync_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let projects: Vec<ProjectItem> = self
            .environment
            .read(cx)
            .state()
            .shell
            .projects
            .iter()
            .map(|project| ProjectItem {
                id: project.id.clone(),
                title: project.title.clone().into(),
                root: project.workspace_root.clone().into(),
            })
            .collect();
        let same = projects.len() == self.projects.len()
            && projects
                .iter()
                .zip(&self.projects)
                .all(|(a, b)| a.id == b.id && a.title == b.title);
        if !same {
            if self.project_id.as_ref().is_none_or(|id| !projects.iter().any(|p| &p.id == id)) {
                self.project_id = projects.first().map(|p| p.id.clone());
            }
            self.projects = projects.clone();
            let selected = self.project_id.clone();
            self.project_select.update(cx, |select, cx| {
                select.set_items(projects, window, cx);
                match &selected {
                    Some(id) => select.set_selected_value(id, window, cx),
                    None => select.set_selected_index(None, window, cx),
                }
            });
        }
        self.sync_composer(window, cx);
    }

    fn sync_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.environment.read(cx);
        let project = self
            .project_id
            .as_ref()
            .and_then(|id| env.state().shell.project(id));
        let Some(project) = project else {
            self.composer = None;
            self._composer_subscription = None;
            cx.notify();
            return;
        };
        let cwd = project.workspace_root.clone();
        let preferred = project.default_model_selection.clone();
        let current = self.composer.as_ref().is_some_and(|composer| {
            let composer = composer.read(cx);
            composer.environment() == &self.environment && composer.cwd() == cwd
        });
        if current {
            return;
        }
        let environment = self.environment.clone();
        let composer = cx.new(|cx| Composer::new(environment, cwd, preferred, window, cx));
        self._composer_subscription = Some(cx.subscribe_in(&composer, window, Self::on_composer_event));
        composer.update(cx, |composer, cx| composer.focus(window, cx));
        self.composer = Some(composer);
        cx.notify();
    }

    fn on_composer_event(
        &mut self,
        composer: &Entity<Composer>,
        event: &ComposerEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ComposerEvent::Submit(submission) = event else {
            return;
        };
        let Some(project_id) = self.project_id.clone() else {
            return;
        };
        self.launch(project_id, submission.clone(), composer.clone(), window, cx);
    }

    fn launch(
        &mut self,
        project_id: String,
        submission: Submission,
        composer: Entity<Composer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let env = self.environment.read(cx);
        let key = env.key().clone();
        let connection = env.connection().clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = connection
                .launch(&project_id, &submission.text, submission.attachments, submission.model)
                .await;
            let _ = this.update_in(cx, |_, window, cx| {
                let outcome = result.as_ref().map(drop).map_err(ToString::to_string);
                composer.update(cx, |composer, cx| composer.finish(outcome, window, cx));
                if let Ok(thread_id) = result {
                    cx.emit(NewThreadEvent::Launched { env: key, thread_id });
                }
            });
        })
        .detach();
    }
}

impl Focusable for NewThreadView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.composer {
            Some(composer) => composer.read(cx).focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl Render for NewThreadView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let has_projects = !self.projects.is_empty();
        v_flex()
            .size_full()
            .bg(theme.background)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h_10()
                    .px_4()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().font_medium().text_sm().child("New thread")),
            )
            .child(
                div().flex_1().min_h_0().flex().items_end().justify_center().px_4().pb_3().child(
                    v_flex()
                        .w_full()
                        .max_w(rems(52.))
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Environment")
                                .child(
                                    Select::new(&self.environment_select)
                                        .xsmall()
                                        .accessibility_label("Environment")
                                        .w(rems(12.)),
                                )
                                .child("Project")
                                .child(
                                    Select::new(&self.project_select)
                                        .xsmall()
                                        .placeholder("No projects")
                                        .search_placeholder("Search projects")
                                        .accessibility_label("Project")
                                        .disabled(!has_projects)
                                        .w(rems(16.)),
                                ),
                        )
                        .child(match &self.composer {
                            Some(composer) => composer.clone().into_any_element(),
                            None => div()
                                .p_4()
                                .rounded(theme.radius_lg)
                                .border_1()
                                .border_color(theme.border)
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("Add a project to this environment to start a thread (⌘O).")
                                .into_any_element(),
                        }),
                ),
            )
    }
}
