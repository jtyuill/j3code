//! The window root: sidebar plus the main pane (a thread, a new thread, or
//! archived threads). Owns the environments and every workspace command.

use std::path::{Path, PathBuf};

use gpui_kit::component::{
    ActiveTheme as _, StyledExt as _, Theme, WindowExt as _,
    button::ButtonVariant,
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, WeakEntity, Window, div,
};
use j3_client::Connection;

use crate::{
    actions::{
        AddEnvironment, AddProject, ArchiveThread, DeleteThread, FocusComposer, FocusSidebar,
        NewThread, NextThread, PreviousThread, RenameThread, ShowArchived, StopRun, WORKSPACE,
    },
    archived::{ArchivedView, Unarchived},
    dialogs::{EnvironmentForm, ProjectCreated, ProjectPicker, footer},
    environment::{Environment, EnvironmentKind, LOCAL_KEY},
    environments::DesktopConfig,
    new_thread::{NewThreadEvent, NewThreadView},
    services::Services,
    sidebar::{Sidebar, SidebarEvent},
    thread_view::ThreadView,
};

/// The workspace of the (single) window, for app-level commands.
struct WorkspaceHandle(WeakEntity<Workspace>);

impl Global for WorkspaceHandle {}

enum Main {
    Empty,
    Thread(Entity<ThreadView>),
    NewThread(Entity<NewThreadView>),
    Archived(Entity<ArchivedView>),
}

pub struct Workspace {
    environments: Vec<Entity<Environment>>,
    config: DesktopConfig,
    config_path: Option<PathBuf>,
    config_error: Option<SharedString>,
    sidebar: Entity<Sidebar>,
    main: Main,
    focus_handle: FocusHandle,
    _main_subscription: Option<Subscription>,
    /// Keeps the open add-project dialog's subscription alive.
    _dialog_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let services = Services::global(cx);
        let local_status = services.local().status().clone();
        let local_url = services.local().url().to_owned();
        let config_path = services.config_path().map(Path::to_path_buf);
        let hostname = gethostname::gethostname().to_string_lossy().into_owned();
        let local = cx.new(|cx| {
            Environment::new(
                LOCAL_KEY,
                hostname,
                &local_url,
                EnvironmentKind::Local(local_status),
                cx,
            )
        });
        let mut environments = vec![local];

        let (config, config_error) = match &config_path {
            Some(path) => match DesktopConfig::load_or_create(path) {
                Ok(config) => (config, None),
                Err(e) => (DesktopConfig { environments: Vec::new() }, Some(e.to_string())),
            },
            None => (
                DesktopConfig { environments: Vec::new() },
                Some("No config directory; remote environments can’t be saved.".to_owned()),
            ),
        };
        for remote in &config.environments {
            environments.push(cx.new(|cx| {
                Environment::new(remote.url.clone(), remote.label.clone(), &remote.url, EnvironmentKind::Remote, cx)
            }));
        }

        let sidebar = cx.new(|cx| Sidebar::new(environments.clone(), cx));
        let subscriptions = vec![
            cx.subscribe_in(&sidebar, window, Self::on_sidebar_event),
            cx.observe_window_appearance(window, |_, window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
            }),
        ];
        let handle = WorkspaceHandle(cx.weak_entity());
        cx.set_global(handle);
        // Start with keyboard focus in the sidebar so arrows and the
        // workspace shortcuts work before anything is clicked.
        sidebar.update(cx, |sidebar, cx| sidebar.focus(window, cx));
        Self {
            environments,
            config,
            config_path,
            config_error: config_error.map(Into::into),
            sidebar,
            main: Main::Empty,
            focus_handle: cx.focus_handle(),
            _main_subscription: None,
            _dialog_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    /// Connections of the window's workspace, for closing them on quit.
    pub fn global_connections(cx: &App) -> Vec<Connection> {
        cx.try_global::<WorkspaceHandle>()
            .and_then(|handle| handle.0.upgrade())
            .map(|workspace| {
                workspace
                    .read(cx)
                    .environments
                    .iter()
                    .map(|env| env.read(cx).connection().clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn environment(&self, key: &str, cx: &App) -> Option<Entity<Environment>> {
        self.environments
            .iter()
            .find(|env| env.read(cx).key() == key)
            .cloned()
    }

    fn current_thread(&self) -> Option<&Entity<ThreadView>> {
        match &self.main {
            Main::Thread(view) => Some(view),
            _ => None,
        }
    }

    /// The thread a thread command applies to: the open one.
    fn target_thread(&self, cx: &App) -> Option<(Entity<Environment>, String)> {
        let view = self.current_thread()?.read(cx);
        Some((view.environment().clone(), view.thread_id().to_owned()))
    }

    /// Environment and project for "new thread"/"add project": the sidebar
    /// selection, else the open thread's, else the local environment.
    fn target_context(&self, cx: &App) -> (Entity<Environment>, Option<String>) {
        if let Some(context) = self.sidebar.read(cx).selection_context()
            && let Some(env) = self.environment(&context.env, cx)
        {
            return (env, context.project_id);
        }
        if let Some((env, thread_id)) = self.target_thread(cx) {
            let project = env
                .read(cx)
                .state()
                .shell
                .thread(&thread_id)
                .map(|thread| thread.project_id.clone());
            return (env, project);
        }
        (self.environments[0].clone(), None)
    }

    fn set_main(&mut self, main: Main, subscription: Option<Subscription>, cx: &mut Context<Self>) {
        self.main = main;
        self._main_subscription = subscription;
        cx.notify();
    }

    fn open_thread(&mut self, env_key: &str, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.current_thread()
            && view.read(cx).thread_id() == thread_id
            && view.read(cx).environment().read(cx).key() == env_key
        {
            return;
        }
        let Some(env) = self.environment(env_key, cx) else {
            return;
        };
        let view = cx.new(|cx| ThreadView::new(env, thread_id, window, cx));
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select_thread(env_key, thread_id, cx));
        self.set_main(Main::Thread(view), None, cx);
    }

    fn new_thread(
        &mut self,
        env: Entity<Environment>,
        project_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let environments = self.environments.clone();
        let view = cx.new(|cx| NewThreadView::new(environments, env, project_id, window, cx));
        let subscription = cx.subscribe_in(&view, window, |this, _, event: &NewThreadEvent, window, cx| {
            let NewThreadEvent::Launched { env, thread_id } = event;
            this.open_thread(env, thread_id, window, cx);
            if let Some(view) = this.current_thread().cloned() {
                view.update(cx, |view, cx| view.focus_composer(window, cx));
            }
        });
        view.update(cx, |view, cx| view.focus_composer(window, cx));
        self.set_main(Main::NewThread(view), Some(subscription), cx);
    }

    fn show_archived(&mut self, env: Entity<Environment>, window: &mut Window, cx: &mut Context<Self>) {
        let key = env.read(cx).key().clone();
        let view = cx.new(|cx| ArchivedView::new(env, cx));
        let subscription = cx.subscribe_in(&view, window, move |this, _, event: &Unarchived, window, cx| {
            this.open_thread(&key, &event.thread_id, window, cx);
        });
        self.sidebar.update(cx, |sidebar, cx| sidebar.clear_selection(cx));
        self.set_main(Main::Archived(view), Some(subscription), cx);
    }

    fn on_sidebar_event(
        &mut self,
        _: &Entity<Sidebar>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let env = |key: &SharedString, this: &Self, cx: &App| this.environment(key, cx);
        match event {
            SidebarEvent::OpenThread { env, thread_id } => self.open_thread(env, thread_id, window, cx),
            SidebarEvent::NewThread { env: key, project_id } => {
                if let Some(env) = env(key, self, cx) {
                    self.new_thread(env, Some(project_id.clone()), window, cx);
                }
            }
            SidebarEvent::AddProject { env: key } => {
                if let Some(env) = env(key, self, cx) {
                    self.add_project(env, window, cx);
                }
            }
            SidebarEvent::RenameThread { env: key, thread_id } => {
                if let Some(env) = env(key, self, cx) {
                    self.rename_thread(env, thread_id.clone(), window, cx);
                }
            }
            SidebarEvent::ArchiveThread { env: key, thread_id } => {
                if let Some(env) = env(key, self, cx) {
                    self.archive_thread(env, thread_id.clone(), window, cx);
                }
            }
            SidebarEvent::DeleteThread { env: key, thread_id } => {
                if let Some(env) = env(key, self, cx) {
                    self.delete_thread(env, thread_id.clone(), window, cx);
                }
            }
            SidebarEvent::DeleteProject { env: key, project_id } => {
                if let Some(env) = env(key, self, cx) {
                    self.delete_project(env, project_id.clone(), window, cx);
                }
            }
            SidebarEvent::ShowArchived { env: key } => {
                if let Some(env) = env(key, self, cx) {
                    self.show_archived(env, window, cx);
                }
            }
            SidebarEvent::RemoveEnvironment { env: key } => self.remove_environment(key.clone(), window, cx),
            SidebarEvent::AddEnvironment => self.add_environment(window, cx),
        }
    }

    /// Shows a failed command's error as a notification.
    fn notify_error(window: &mut Window, message: String, cx: &mut App) {
        window.push_notification(
            gpui_kit::component::notification::Notification::new()
                .message(message)
                .with_type(gpui_kit::component::notification::NotificationType::Error),
            cx,
        );
    }

    /// Runs a server command for `env` and reports a failure.
    fn run_command<T: 'static>(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        what: &'static str,
        command: impl Future<Output = Result<T, j3_client::Error>> + 'static,
        then: impl FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let result = command.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(value) => then(this, value, window, cx),
                Err(e) => Self::notify_error(window, format!("Couldn’t {what}: {e}"), cx),
            });
        })
        .detach();
    }

    fn thread_title(env: &Entity<Environment>, thread_id: &str, cx: &App) -> String {
        env.read(cx)
            .state()
            .shell
            .thread(thread_id)
            .map(|thread| thread.title.clone())
            .unwrap_or_else(|| "this thread".to_owned())
    }

    fn rename_thread(&mut self, env: Entity<Environment>, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = Self::thread_title(&env, &thread_id, cx);
        let input = cx.new(|cx| {
            gpui_kit::component::input::InputState::new(window, cx).default_value(title)
        });
        let workspace = cx.weak_entity();
        let dialog_input = input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let input = dialog_input.clone();
            let workspace = workspace.clone();
            let env = env.clone();
            let thread_id = thread_id.clone();
            dialog
                .title("Rename thread")
                .child(gpui_kit::component::input::Input::new(&dialog_input))
                .footer(footer("Rename"))
                .on_ok(move |_, window, cx| {
                    let title = input.read(cx).value().trim().to_owned();
                    if title.is_empty() {
                        return false;
                    }
                    let connection = env.read(cx).connection().clone();
                    let thread_id = thread_id.clone();
                    let _ = workspace.update(cx, |this, cx| {
                        this.run_command(
                            window,
                            cx,
                            "rename the thread",
                            async move { connection.rename(&thread_id, &title).await },
                            |_, _, _, _| {},
                        );
                    });
                    true
                })
        });
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    /// After a thread leaves the list, show the next one (or nothing).
    fn leave_thread(&mut self, env: &Entity<Environment>, thread_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let open = self
            .current_thread()
            .is_some_and(|view| view.read(cx).thread_id() == thread_id && view.read(cx).environment() == env);
        if open {
            self.sidebar.update(cx, |sidebar, cx| sidebar.clear_selection(cx));
            self.set_main(Main::Empty, None, cx);
            self.focus_handle.focus(window, cx);
        }
    }

    fn archive_thread(&mut self, env: Entity<Environment>, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let connection = env.read(cx).connection().clone();
        let id = thread_id.clone();
        self.run_command(
            window,
            cx,
            "archive the thread",
            async move { connection.archive(&id).await },
            move |this, _, window, cx| this.leave_thread(&env, &thread_id, window, cx),
        );
    }

    fn delete_thread(&mut self, env: Entity<Environment>, thread_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = Self::thread_title(&env, &thread_id, cx);
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let workspace = workspace.clone();
            let env = env.clone();
            let thread_id = thread_id.clone();
            alert
                .title(format!("Delete “{title}”?"))
                .description("The conversation is removed from j3. omp’s session file stays on disk.")
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let env = env.clone();
                    let thread_id = thread_id.clone();
                    let _ = workspace.update(cx, |this, cx| {
                        let connection = env.read(cx).connection().clone();
                        let id = thread_id.clone();
                        this.run_command(
                            window,
                            cx,
                            "delete the thread",
                            async move { connection.delete_thread(&id).await },
                            move |this, _, window, cx| this.leave_thread(&env, &thread_id, window, cx),
                        );
                    });
                    true
                })
        });
    }

    fn delete_project(&mut self, env: Entity<Environment>, project_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let state = env.read(cx).state().clone();
        let Some(project) = state.shell.project(&project_id) else {
            return;
        };
        let title = project.title.clone();
        let threads = state.shell.threads_of(&project_id).len();
        if threads > 0 {
            Self::notify_error(
                window,
                format!("“{title}” still has {threads} thread(s). Archive or delete them first."),
                cx,
            );
            return;
        }
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let workspace = workspace.clone();
            let env = env.clone();
            let project_id = project_id.clone();
            alert
                .title(format!("Remove “{title}”?"))
                .description("Files on disk aren’t deleted.")
                .confirm()
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let connection = env.read(cx).connection().clone();
                    let project_id = project_id.clone();
                    let _ = workspace.update(cx, |this, cx| {
                        this.run_command(
                            window,
                            cx,
                            "remove the project",
                            async move { connection.delete_project(&project_id).await },
                            |_, _, _, _| {},
                        );
                    });
                    true
                })
        });
    }

    fn add_project(&mut self, env: Entity<Environment>, window: &mut Window, cx: &mut Context<Self>) {
        let label = env.read(cx).label().clone();
        let key = env.read(cx).key().clone();
        let picker = cx.new(|cx| ProjectPicker::new(env, window, cx));
        let subscription = cx.subscribe_in(&picker, window, move |this, _, event: &ProjectCreated, window, cx| {
            window.close_dialog(cx);
            if let Some(env) = this.environment(&key, cx) {
                this.new_thread(env, Some(event.project_id.clone()), window, cx);
            }
        });
        // The subscription lives as long as the picker's dialog is open.
        let dialog_picker = picker.clone();
        let width = window.rem_size() * 34.;
        window.open_dialog(cx, move |dialog, _, _| {
            let picker = dialog_picker.clone();
            dialog
                .title(format!("Add project on {label}"))
                .w(width)
                .child(dialog_picker.clone())
                .footer(footer("Add Project"))
                .on_ok(move |_, _, cx| {
                    picker.update(cx, |picker, cx| picker.create(cx));
                    false
                })
        });
        picker.update(cx, |picker, cx| picker.focus(window, cx));
        self._dialog_subscription = Some(subscription);
    }

    fn add_environment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| EnvironmentForm::new(window, cx));
        let workspace = cx.weak_entity();
        let dialog_form = form.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let form = dialog_form.clone();
            let workspace = workspace.clone();
            dialog
                .title("Add environment")
                .child(dialog_form.clone())
                .footer(footer("Add"))
                .on_ok(move |_, _, cx| {
                    let (label, url) = form.read(cx).values(cx);
                    let result = workspace
                        .update(cx, |this, cx| this.insert_environment(&label, &url, cx))
                        .unwrap_or_else(|_| Err("The window closed.".to_owned()));
                    match result {
                        Ok(()) => true,
                        Err(error) => {
                            form.update(cx, |form, cx| form.set_error(error, cx));
                            false
                        }
                    }
                })
        });
        form.update(cx, |form, cx| form.focus(window, cx));
    }

    fn insert_environment(&mut self, label: &str, url: &str, cx: &mut Context<Self>) -> Result<(), String> {
        let mut config = self.config.clone();
        let remote = config.add(label, url).map_err(|e| e.to_string())?;
        if let Some(path) = &self.config_path {
            config.save(path).map_err(|e| e.to_string())?;
        }
        self.config = config;
        let env = cx.new(|cx| {
            Environment::new(remote.url.clone(), remote.label.clone(), &remote.url, EnvironmentKind::Remote, cx)
        });
        self.environments.push(env);
        let environments = self.environments.clone();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_environments(environments, cx));
        Ok(())
    }

    fn remove_environment(&mut self, key: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(env) = self.environment(&key, cx) else {
            return;
        };
        let label = env.read(cx).label().clone();
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let workspace = workspace.clone();
            let key = key.clone();
            alert
                .title(format!("Remove “{label}”?"))
                .description("Its threads stay on that server.")
                .confirm()
                .ok_text("Remove")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let key = key.clone();
                    let _ = workspace.update(cx, |this, cx| {
                        if let Err(error) = this.drop_environment(&key, window, cx) {
                            Self::notify_error(window, error, cx);
                        }
                    });
                    true
                })
        });
    }

    fn drop_environment(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let mut config = self.config.clone();
        config.remove(key);
        if let Some(path) = &self.config_path {
            config.save(path).map_err(|e| format!("Couldn’t save the environments: {e}"))?;
        }
        self.config = config;
        let Some(ix) = self.environments.iter().position(|env| env.read(cx).key() == key) else {
            return Ok(());
        };
        let env = self.environments.remove(ix);
        let showing = match &self.main {
            Main::Thread(view) => view.read(cx).environment() == &env,
            Main::Archived(view) => view.read(cx).environment() == &env,
            Main::NewThread(_) => true,
            Main::Empty => false,
        };
        if showing {
            self.set_main(Main::Empty, None, cx);
            self.focus_handle.focus(window, cx);
        }
        let connection = env.read(cx).connection().clone();
        cx.spawn(async move |_, _| connection.close().await).detach();
        let environments = self.environments.clone();
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.set_environments(environments, cx));
        Ok(())
    }

    fn select_adjacent(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((env, thread_id)) = self.sidebar.read(cx).adjacent_thread(delta, cx) {
            self.open_thread(&env, &thread_id, window, cx);
        }
    }

    fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.main {
            Main::Thread(view) => view.clone().into_any_element(),
            Main::NewThread(view) => view.clone().into_any_element(),
            Main::Archived(view) => view.clone().into_any_element(),
            Main::Empty => {
                let theme = cx.theme();
                let hint = |keys: &'static str, what: &'static str| {
                    gpui_kit::component::h_flex()
                        .gap_3()
                        .child(
                            div()
                                .w_16()
                                .text_right()
                                .child(keys),
                        )
                        .child(what)
                };
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .bg(theme.background)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(div().text_color(theme.foreground).font_medium().child("No thread open"))
                    .child(hint("⌘N", "New thread"))
                    .child(hint("⌘O", "Add project"))
                    .child(hint("⌘[  ⌘]", "Previous / next thread"))
                    .child(hint("⌘0", "Focus the sidebar"))
                    .when_some(self.config_error.clone(), |this, error| {
                        this.child(div().text_xs().text_color(theme.danger).child(error))
                    })
                    .into_any_element()
            }
        }
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = self.render_main(cx);
        let rem = window.rem_size();
        div()
            .id("workspace")
            .key_context(WORKSPACE)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .on_action(cx.listener(|this, _: &NewThread, window, cx| {
                let (env, project) = this.target_context(cx);
                this.new_thread(env, project, window, cx);
            }))
            .on_action(cx.listener(|this, _: &AddProject, window, cx| {
                let (env, _) = this.target_context(cx);
                this.add_project(env, window, cx);
            }))
            .on_action(cx.listener(|this, _: &AddEnvironment, window, cx| this.add_environment(window, cx)))
            .on_action(cx.listener(|this, _: &StopRun, window, cx| {
                if let Some(view) = this.current_thread().cloned() {
                    view.update(cx, |view, cx| view.stop(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &RenameThread, window, cx| {
                if let Some((env, thread_id)) = this.target_thread(cx) {
                    this.rename_thread(env, thread_id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ArchiveThread, window, cx| {
                if let Some((env, thread_id)) = this.target_thread(cx) {
                    this.archive_thread(env, thread_id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &DeleteThread, window, cx| {
                if let Some((env, thread_id)) = this.target_thread(cx) {
                    this.delete_thread(env, thread_id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowArchived, window, cx| {
                let (env, _) = this.target_context(cx);
                this.show_archived(env, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| match &this.main {
                Main::Thread(view) => view.clone().update(cx, |view, cx| view.focus_composer(window, cx)),
                Main::NewThread(view) => view.clone().update(cx, |view, cx| view.focus_composer(window, cx)),
                _ => {}
            }))
            .on_action(cx.listener(|this, _: &FocusSidebar, window, cx| {
                this.sidebar.clone().update(cx, |sidebar, cx| sidebar.focus(window, cx));
            }))
            .on_action(cx.listener(|this, _: &NextThread, window, cx| this.select_adjacent(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousThread, window, cx| {
                this.select_adjacent(-1, window, cx)
            }))
            .child(
                h_resizable("workspace-split")
                    .child(
                        resizable_panel()
                            .size(rem * 17.)
                            .size_range(rem * 12.5..rem * 30.)
                            .child(self.sidebar.clone()),
                    )
                    .child(resizable_panel().child(main)),
            )
    }
}
