//! The window root: the thread sidebar plus the main pane (a thread or a new
//! thread), or Settings over the whole window. Owns the environments, the
//! preferences and every workspace command.

use std::path::Path;

use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, Theme, WindowExt as _,
    button::{Button, ButtonVariant, ButtonVariants as _},
    h_flex,
    input::{Input, InputState},
    notification::{Notification, NotificationType},
    resizable::{h_resizable, resizable_panel},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Decorations, Entity, FocusHandle, Focusable, Global,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, WeakEntity,
    Window, base::InteractiveElementExt as _, div, px,
};
use j3_client::{
    Connection,
    t3::orchestration::SettledOverride,
    workflow::{AgentNotification, effective_snoozed, forward_target, has_unseen_completion},
};
use time::OffsetDateTime;

use crate::{
    actions::{
        AddEnvironment, AddProject, ArchiveThread, DeleteThread, FocusComposer, FocusSidebar,
        GotoAgent, JumpToThread, NewThread, NextThread, OpenSettings, PreviousThread, RenameThread,
        ResetZoom, SettleThread, ShowArchived, StopRun, ToggleFiles, ToggleTerminal, WORKSPACE,
        ZoomIn, ZoomOut,
    },
    attention::{Alert, AttentionMonitor},
    dialogs::{EnvironmentForm, ProjectCreated, ProjectPicker, SnoozeForm, footer},
    environment::{Environment, EnvironmentKind, LOCAL_KEY},
    goto::{GotoJump, GotoPicker},
    new_thread::{NewThreadEvent, NewThreadView},
    preferences::Preferences,
    services::Services,
    settings_view::{SettingsEvent, SettingsPage, SettingsView},
    sidebar::{ProjectKey, Sidebar, SidebarEvent, ThreadCommand, ThreadKey},
    thread_view::ThreadView,
    titlebar,
};

/// The workspace of the (single) window, for app-level commands.
struct WorkspaceHandle(WeakEntity<Workspace>);

impl Global for WorkspaceHandle {}

enum Main {
    Empty,
    Thread(Entity<ThreadView>),
    NewThread(Entity<NewThreadView>),
}

/// Marks attention toasts, one per thread.
struct AttentionToast;

pub struct Workspace {
    environments: Vec<Entity<Environment>>,
    preferences: Entity<Preferences>,
    sidebar: Entity<Sidebar>,
    main: Main,
    settings: Option<Entity<SettingsView>>,
    settings_generation: u64,
    attention: AttentionMonitor,
    /// The open thread's completion last visited because it finished while
    /// open, so a completion is acknowledged once.
    visited_completion: Option<(ThreadKey, String)>,
    focus_handle: FocusHandle,
    /// The header row height the traffic lights were last centred on.
    #[cfg(target_os = "macos")]
    traffic_lights_row: gpui_kit::Pixels,
    _main_subscription: Option<Subscription>,
    _settings_subscription: Option<Subscription>,
    /// Keeps the open dialog's subscription alive.
    _dialog_subscription: Option<Subscription>,
    _environment_subscriptions: Vec<Subscription>,
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
                hostname.clone(),
                &local_url,
                EnvironmentKind::Local(local_status),
                cx,
            )
        });
        let mut environments = vec![local];
        let preferences = cx.new(|_| Preferences::load(config_path, &hostname));
        for remote in preferences.read(cx).remotes().to_vec() {
            environments.push(cx.new(|cx| {
                Environment::new(
                    remote.url.clone(),
                    remote.label.clone(),
                    &remote.url,
                    EnvironmentKind::Remote,
                    cx,
                )
            }));
        }

        let sidebar = cx.new(|cx| Sidebar::new(environments.clone(), preferences.clone(), cx));
        let subscriptions = vec![
            cx.subscribe_in(&sidebar, window, Self::on_sidebar_event),
            cx.observe(&preferences, |_, preferences, cx| {
                crate::appearance::apply(&preferences, cx);
                cx.notify();
            }),
            cx.observe_window_appearance(window, |this, window, cx| {
                Theme::sync_system_appearance(Some(window), cx);
                // The reloaded theme brings its own fonts.
                crate::appearance::apply(&this.preferences, cx);
            }),
        ];
        crate::appearance::apply(&preferences, cx);
        let handle = WorkspaceHandle(cx.weak_entity());
        cx.set_global(handle);
        // Start with keyboard focus in the sidebar so arrows and the
        // workspace shortcuts work before anything is clicked.
        sidebar.update(cx, |sidebar, cx| sidebar.focus(window, cx));
        let mut workspace = Self {
            environments: Vec::new(),
            preferences,
            sidebar,
            main: Main::Empty,
            settings: None,
            settings_generation: 0,
            attention: AttentionMonitor::default(),
            visited_completion: None,
            focus_handle: cx.focus_handle(),
            #[cfg(target_os = "macos")]
            traffic_lights_row: px(0.),
            _main_subscription: None,
            _settings_subscription: None,
            _dialog_subscription: None,
            _environment_subscriptions: Vec::new(),
            _subscriptions: subscriptions,
        };
        workspace.set_environments(environments, window, cx);
        workspace
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

    /// Replaces the environment list everywhere it is shown.
    fn set_environments(
        &mut self,
        environments: Vec<Entity<Environment>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._environment_subscriptions = environments
            .iter()
            .map(|env| cx.observe_in(env, window, Self::on_environment_changed))
            .collect();
        self.environments = environments.clone();
        let sidebar_environments = environments.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_environments(sidebar_environments, cx)
        });
        if let Some(settings) = &self.settings {
            settings.update(cx, |settings, cx| {
                settings.set_environments(environments, cx)
            });
        }
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

    /// Zooms one level in (`steps` > 0) or out (< 0), or back to 100% (0),
    /// and saves it.
    fn zoom(&mut self, steps: i32, cx: &mut Context<Self>) {
        self.preferences.update(cx, |preferences, cx| {
            preferences.update_ui(
                |ui| {
                    ui.zoom = if steps == 0 { 1. } else { ui.zoom_step(steps) };
                },
                cx,
            )
        });
    }

    /// The open thread.
    fn open_key(&self, cx: &App) -> Option<ThreadKey> {
        let view = self.current_thread()?.read(cx);
        Some(ThreadKey::new(
            view.environment().read(cx).key().clone(),
            view.thread_id(),
        ))
    }

    /// The thread a thread command applies to: the open one.
    fn target_thread(&self, cx: &App) -> Option<(Entity<Environment>, String)> {
        let view = self.current_thread()?.read(cx);
        Some((view.environment().clone(), view.thread_id().to_owned()))
    }

    /// Environment and project for "new thread"/"add project": the sidebar's
    /// project filter, else the open thread's, else the local environment.
    fn target_context(&self, cx: &App) -> (Entity<Environment>, Option<String>) {
        if let Some(scope) = self.sidebar.read(cx).project_scope()
            && let Some(env) = self.environment(&scope.env, cx)
        {
            return (env, Some(scope.project_id.clone()));
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
        let open = self.open_key(cx);
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.select_thread(open, cx));
        cx.notify();
    }

    fn open_thread(&mut self, key: &ThreadKey, window: &mut Window, cx: &mut Context<Self>) {
        self.close_settings(window, cx);
        if self.open_key(cx).as_ref() == Some(key) {
            return;
        }
        let Some(env) = self.environment(&key.env, cx) else {
            return;
        };
        let preferences = self.preferences.clone();
        let view = cx.new(|cx| ThreadView::new(env, preferences, &key.thread_id, window, cx));
        self.set_main(Main::Thread(view), None, cx);
    }

    fn new_thread(
        &mut self,
        env: Entity<Environment>,
        project_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_settings(window, cx);
        let environments = self.environments.clone();
        let preferences = self.preferences.clone();
        let view =
            cx.new(|cx| NewThreadView::new(environments, env, preferences, project_id, window, cx));
        let subscription = cx.subscribe_in(
            &view,
            window,
            |this, _, event: &NewThreadEvent, window, cx| {
                let NewThreadEvent::Launched { env, thread_id } = event;
                this.open_thread(&ThreadKey::new(env.clone(), thread_id.clone()), window, cx);
                if let Some(view) = this.current_thread().cloned() {
                    view.update(cx, |view, cx| view.focus_composer(window, cx));
                }
            },
        );
        view.update(cx, |view, cx| view.focus_composer(window, cx));
        self.set_main(Main::NewThread(view), Some(subscription), cx);
    }

    fn new_thread_in(
        &mut self,
        scope: Option<ProjectKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (env, project) = match scope.and_then(|scope| {
            self.environment(&scope.env, cx)
                .map(|env| (env, Some(scope.project_id)))
        }) {
            Some(target) => target,
            None => self.target_context(cx),
        };
        self.new_thread(env, project, window, cx);
    }

    fn open_settings(&mut self, page: SettingsPage, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_generation += 1;
        let preferences = self.preferences.clone();
        let environments = self.environments.clone();
        let generation = self.settings_generation;
        let view = cx.new(|cx| SettingsView::new(preferences, environments, page, generation, cx));
        self._settings_subscription = Some(cx.subscribe_in(&view, window, Self::on_settings_event));
        view.update(cx, |view, cx| view.focus(window, cx));
        self.settings = Some(view);
        cx.notify();
    }

    fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_none() {
            return;
        }
        self._settings_subscription = None;
        self.sidebar
            .update(cx, |sidebar, cx| sidebar.focus(window, cx));
        cx.notify();
    }

    fn on_settings_event(
        &mut self,
        _: &Entity<SettingsView>,
        event: &SettingsEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SettingsEvent::Close => self.close_settings(window, cx),
            SettingsEvent::AddEnvironment => self.add_environment(window, cx),
            SettingsEvent::RemoveEnvironment { env } => {
                self.remove_environment(env.clone(), window, cx)
            }
            SettingsEvent::AddProject { env } => {
                if let Some(env) = self.environment(env, cx) {
                    self.add_project(env, false, window, cx);
                }
            }
            SettingsEvent::RemoveProject { env, project_id } => {
                if let Some(env) = self.environment(env, cx) {
                    self.delete_project(env, project_id.clone(), window, cx);
                }
            }
            SettingsEvent::OpenThread { env, thread_id } => {
                self.open_thread(&ThreadKey::new(env.clone(), thread_id.clone()), window, cx)
            }
        }
    }

    fn on_sidebar_event(
        &mut self,
        _: &Entity<Sidebar>,
        event: &SidebarEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let env = |key: &ThreadKey, this: &Self, cx: &App| this.environment(&key.env, cx);
        match event {
            SidebarEvent::OpenThread(key) => self.open_thread(key, window, cx),
            SidebarEvent::NewThread(scope) => self.new_thread_in(scope.clone(), window, cx),
            SidebarEvent::AddProject => {
                let (env, _) = self.target_context(cx);
                self.add_project(env, true, window, cx);
            }
            SidebarEvent::Commands(commands) => {
                self.run_thread_commands(commands.clone(), window, cx)
            }
            SidebarEvent::CustomSnooze(key) => self.custom_snooze(key.clone(), window, cx),
            SidebarEvent::RenameThread(key) => {
                if let Some(env) = env(key, self, cx) {
                    self.rename_thread(env, key.thread_id.clone(), window, cx);
                }
            }
            SidebarEvent::ArchiveThread(key) => {
                if let Some(env) = env(key, self, cx) {
                    self.archive_thread(env, key.thread_id.clone(), window, cx);
                }
            }
            SidebarEvent::DeleteThread(key) => {
                if let Some(env) = env(key, self, cx) {
                    self.delete_thread(env, key.thread_id.clone(), window, cx);
                }
            }
        }
    }

    /// Raises attention toasts and, while the window is in the background,
    /// sounds for `env`'s status changes, and acknowledges a completion of
    /// the open thread (it was watched).
    fn on_environment_changed(
        &mut self,
        env: Entity<Environment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (key, state) = {
            let env = env.read(cx);
            (env.key().clone(), env.state().clone())
        };
        let open = self.open_key(cx).filter(|open| open.env == key);
        // The open thread is only watched while the window has focus: a run
        // that finishes behind another app alerts like any other.
        let focused = window.is_window_active();
        let alerts = self.attention.observe(
            &key,
            &state.shell,
            open.as_ref()
                .filter(|_| focused)
                .map(|open| open.thread_id.as_str()),
        );
        if !alerts.is_empty() {
            let ui = self.preferences.read(cx).ui().clone();
            for alert in alerts {
                if ui.sounds && !focused {
                    alert.sound().play(Services::global(cx).runtime());
                }
                // The open thread's own run ending (heard in the
                // background) needs no toast: it is on screen.
                let on_screen = alert.kind == AgentNotification::Finished
                    && open
                        .as_ref()
                        .is_some_and(|open| open.thread_id == alert.thread_id);
                if ui.toasts && !on_screen {
                    self.push_attention_toast(alert, window, cx);
                }
            }
        }
        if let Some(open) = open
            && let Some(thread) = state.shell.thread(&open.thread_id)
            && has_unseen_completion(thread)
            && let Some(Some(completed)) = &thread.latest_run_completed_at
        {
            let mark = (open.clone(), completed.clone());
            if self.visited_completion.as_ref() != Some(&mark) {
                self.visited_completion = Some(mark);
                let connection = env.read(cx).connection().clone();
                cx.spawn(async move |_, _| {
                    if let Err(e) = connection.visit(&open.thread_id).await {
                        tracing::debug!("visit {}: {e}", open.thread_id);
                    }
                })
                .detach();
            }
        }
    }

    fn push_attention_toast(&mut self, alert: Alert, window: &mut Window, cx: &mut Context<Self>) {
        let key = ThreadKey::new(alert.env.clone(), alert.thread_id.clone());
        let message: SharedString = alert.message().into();
        let kind = match alert.kind {
            AgentNotification::NeedsAttention => NotificationType::Warning,
            AgentNotification::Finished => NotificationType::Success,
        };
        let workspace = cx.weak_entity();
        let toast_id = key.element_id("attention");
        window.push_notification(
            Notification::new()
                .id1::<AttentionToast>(toast_id.clone())
                .with_type(kind)
                .content(move |_, _, _| {
                    div()
                        .id(toast_id.clone())
                        .test_support()
                        .aria_label(message.clone())
                        .text_sm()
                        .child(message.clone())
                        .into_any_element()
                })
                .on_click(move |_, window, cx| {
                    let key = key.clone();
                    let _ = workspace.update(cx, |this, cx| this.open_thread(&key, window, cx));
                }),
            cx,
        );
    }

    /// Shows a failed command's error as a notification.
    fn notify_error(window: &mut Window, message: String, cx: &mut App) {
        window.push_notification(
            Notification::new()
                .message(message)
                .with_type(NotificationType::Error),
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

    /// Runs thread commands in order, stopping at the first failure (a later
    /// command can depend on an earlier one, e.g. un-settle before reorder).
    fn run_thread_commands(
        &mut self,
        commands: Vec<(ThreadKey, ThreadCommand)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let commands: Vec<(Connection, String, ThreadCommand)> = commands
            .into_iter()
            .filter_map(|(key, command)| {
                let env = self.environment(&key.env, cx)?;
                Some((env.read(cx).connection().clone(), key.thread_id, command))
            })
            .collect();
        if commands.is_empty() {
            return;
        }
        self.run_command(
            window,
            cx,
            "update the thread",
            async move {
                for (connection, id, command) in commands {
                    let id = id.as_str();
                    match command {
                        ThreadCommand::Settle => connection.settle(id).await,
                        ThreadCommand::Unsettle => connection.unsettle(id).await,
                        ThreadCommand::Snooze(until) => connection.snooze(id, until).await,
                        ThreadCommand::Wake => connection.unsnooze(id).await,
                        ThreadCommand::Pin(key) => connection.pin(id, key.as_deref()).await,
                        ThreadCommand::Unpin => connection.unpin(id).await,
                        ThreadCommand::ReorderPinned(key) => {
                            connection.reorder_pinned(id, &key).await
                        }
                        ThreadCommand::ReorderActive(key) => {
                            connection.reorder_active(id, &key).await
                        }
                        ThreadCommand::MarkUnread => connection.mark_unread(id).await,
                        ThreadCommand::AutoSettle(enabled) => {
                            connection.set_auto_settle(id, enabled).await
                        }
                    }?;
                }
                Ok(())
            },
            |_, _, _, _| {},
        );
    }

    /// t3's settle-and-advance (⌘⇧S): un-settles a settled open thread;
    /// otherwise settles it and opens the next row that is not settled or
    /// snoozed, else a new thread in its project.
    fn settle_open_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.open_key(cx) else {
            return;
        };
        let Some(env) = self.environment(&key.env, cx) else {
            return;
        };
        let Some(thread) = env.read(cx).state().shell.thread(&key.thread_id).cloned() else {
            return;
        };
        if thread.settled_override == Some(SettledOverride::Settled) {
            self.run_thread_commands(vec![(key, ThreadCommand::Unsettle)], window, cx);
            return;
        }
        let now = OffsetDateTime::now_utc();
        let order = self.sidebar.read(cx).order();
        let parked = |candidate: &ThreadKey| {
            candidate == &key
                || self
                    .environment(&candidate.env, cx)
                    .and_then(|env| {
                        env.read(cx)
                            .state()
                            .shell
                            .thread(&candidate.thread_id)
                            .map(|thread| {
                                thread.settled_override == Some(SettledOverride::Settled)
                                    || effective_snoozed(thread, now)
                            })
                    })
                    .unwrap_or(true)
        };
        let next = forward_target(&order, &key, parked).cloned();
        self.run_thread_commands(vec![(key, ThreadCommand::Settle)], window, cx);
        match next {
            Some(next) => self.open_thread(&next, window, cx),
            None => self.new_thread(env, Some(thread.project_id), window, cx),
        }
    }

    fn custom_snooze(&mut self, key: ThreadKey, window: &mut Window, cx: &mut Context<Self>) {
        let form = cx.new(|cx| SnoozeForm::new(window, cx));
        let workspace = cx.weak_entity();
        let dialog_form = form.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let form = dialog_form.clone();
            let workspace = workspace.clone();
            let key = key.clone();
            dialog
                .title("Custom snooze")
                .child(dialog_form.clone())
                .footer(footer("Snooze"))
                .on_ok(move |_, window, cx| {
                    let Some(wake) = form.update(cx, |form, cx| form.wake(cx)) else {
                        return false;
                    };
                    let key = key.clone();
                    let _ = workspace.update(cx, |this, cx| {
                        this.run_thread_commands(
                            vec![(key, ThreadCommand::Snooze(wake))],
                            window,
                            cx,
                        )
                    });
                    true
                })
        });
        form.update(cx, |form, cx| form.focus(window, cx));
    }

    fn goto_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.sidebar.read(cx).goto_rows(cx);
        let current = self.open_key(cx);
        let picker = cx.new(|cx| GotoPicker::new(&rows, current, window, cx));
        let subscription = cx.subscribe_in(
            &picker,
            window,
            |this, _, GotoJump(key): &GotoJump, window, cx| {
                window.close_dialog(cx);
                this.open_thread(key, window, cx);
            },
        );
        let dialog_picker = picker.clone();
        let width = window.rem_size() * 36.;
        window.open_dialog(cx, move |dialog, _, _| {
            let picker = dialog_picker.clone();
            dialog
                .title("Go to agent")
                .w(width)
                .child(dialog_picker.clone())
                // Enter in the dialog jumps to the selected agent; the
                // jump closes the dialog.
                .on_ok(move |_, _, cx| {
                    picker.update(cx, |picker, cx| picker.confirm(cx));
                    false
                })
        });
        picker.update(cx, |picker, cx| picker.focus(window, cx));
        self._dialog_subscription = Some(subscription);
    }

    fn thread_title(env: &Entity<Environment>, thread_id: &str, cx: &App) -> String {
        env.read(cx)
            .state()
            .shell
            .thread(thread_id)
            .map(|thread| thread.title.clone())
            .unwrap_or_else(|| "this thread".to_owned())
    }

    fn rename_thread(
        &mut self,
        env: Entity<Environment>,
        thread_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = Self::thread_title(&env, &thread_id, cx);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title));
        let workspace = cx.weak_entity();
        let dialog_input = input.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let input = dialog_input.clone();
            let workspace = workspace.clone();
            let env = env.clone();
            let thread_id = thread_id.clone();
            dialog
                .title("Rename thread")
                .child(Input::new(&dialog_input))
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

    /// After a thread leaves the list, show nothing in its place.
    fn leave_thread(
        &mut self,
        env: &Entity<Environment>,
        thread_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open = self.current_thread().is_some_and(|view| {
            view.read(cx).thread_id() == thread_id && view.read(cx).environment() == env
        });
        if open {
            self.set_main(Main::Empty, None, cx);
            self.focus_handle.focus(window, cx);
        }
    }

    fn archive_thread(
        &mut self,
        env: Entity<Environment>,
        thread_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn delete_thread(
        &mut self,
        env: Entity<Environment>,
        thread_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = Self::thread_title(&env, &thread_id, cx);
        let workspace = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let workspace = workspace.clone();
            let env = env.clone();
            let thread_id = thread_id.clone();
            alert
                .title(format!("Delete “{title}”?"))
                .description(
                    "The conversation is removed from j3. omp’s session file stays on disk.",
                )
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
                            move |this, _, window, cx| {
                                this.leave_thread(&env, &thread_id, window, cx)
                            },
                        );
                    });
                    true
                })
        });
    }

    fn delete_project(
        &mut self,
        env: Entity<Environment>,
        project_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    /// Adds a project through the directory picker, starting on `env` (any
    /// online device can be chosen there); from the main window, a new
    /// thread opens in it afterwards.
    fn add_project(
        &mut self,
        env: Entity<Environment>,
        then_new_thread: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let environments = self.environments.clone();
        let picker = cx.new(|cx| ProjectPicker::new(&environments, env, window, cx));
        let subscription = cx.subscribe_in(
            &picker,
            window,
            move |this, _, event: &ProjectCreated, window, cx| {
                window.close_dialog(cx);
                if then_new_thread && let Some(env) = this.environment(&event.env, cx) {
                    this.new_thread(env, Some(event.project_id.clone()), window, cx);
                }
            },
        );
        // The subscription lives as long as the picker's dialog is open.
        let dialog_picker = picker.clone();
        let width = window.rem_size() * 34.;
        window.open_dialog(cx, move |dialog, _, _| {
            let picker = dialog_picker.clone();
            dialog
                .title("Add project")
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
                .on_ok(move |_, window, cx| {
                    let (label, url) = form.read(cx).values(cx);
                    let result = workspace
                        .update(cx, |this, cx| {
                            this.insert_environment(&label, &url, window, cx)
                        })
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

    fn insert_environment(
        &mut self,
        label: &str,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let remote = self
            .preferences
            .update(cx, |preferences, cx| preferences.add_remote(label, url, cx))?;
        let env = cx.new(|cx| {
            Environment::new(
                remote.url.clone(),
                remote.label.clone(),
                &remote.url,
                EnvironmentKind::Remote,
                cx,
            )
        });
        let mut environments = self.environments.clone();
        environments.push(env);
        self.set_environments(environments, window, cx);
        Ok(())
    }

    fn remove_environment(
        &mut self,
        key: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    fn drop_environment(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.preferences
            .update(cx, |preferences, cx| preferences.remove_remote(key, cx))?;
        let Some(ix) = self
            .environments
            .iter()
            .position(|env| env.read(cx).key() == key)
        else {
            return Ok(());
        };
        let mut environments = self.environments.clone();
        let env = environments.remove(ix);
        let showing = match &self.main {
            Main::Thread(view) => view.read(cx).environment() == &env,
            Main::NewThread(_) => true,
            Main::Empty => false,
        };
        if showing {
            self.set_main(Main::Empty, None, cx);
        }
        self.attention.forget(key);
        let connection = env.read(cx).connection().clone();
        cx.spawn(async move |_, _| connection.close().await)
            .detach();
        self.set_environments(environments, window, cx);
        Ok(())
    }

    fn open_adjacent(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(key) = self.sidebar.read(cx).adjacent_thread(delta) {
            self.open_thread(&key, window, cx);
        }
    }

    /// The window's own titlebar under Linux client-side decorations, with
    /// the window buttons and dragging done here. Under a server-drawn
    /// titlebar (X11 without a compositor) it draws nothing, and on macOS the
    /// panes' header row is the titlebar (see `titlebar`).
    fn render_titlebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let linux_client = cfg!(target_os = "linux")
            && matches!(window.window_decorations(), Decorations::Client { .. });
        if !linux_client {
            return div().into_any_element();
        }
        let theme = cx.theme();
        let control = |id: &'static str, icon: IconName, label: &'static str| {
            Button::new(id).ghost().xsmall().icon(icon).tooltip(label)
        };
        h_flex()
            .id("titlebar")
            .test_support()
            .flex_shrink_0()
            .h(px(36.))
            .w_full()
            .bg(theme.title_bar)
            .border_b_1()
            .border_color(theme.title_bar_border)
            .child(
                h_flex()
                    .id("titlebar-drag")
                    .flex_1()
                    .h_full()
                    .items_center()
                    .pl(px(12.))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                    .on_double_click(|_, window, _| window.zoom_window())
                    .child(
                        div()
                            .text_sm()
                            .font_medium()
                            .text_color(theme.muted_foreground)
                            .child("j3"),
                    ),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h_full()
                    .items_center()
                    .gap_1()
                    .pr_2()
                    .child(
                        control("titlebar-minimize", IconName::WindowMinimize, "Minimize")
                            .on_click(|_, window, cx| {
                                cx.stop_propagation();
                                window.minimize_window();
                            }),
                    )
                    .child(
                        control("titlebar-maximize", IconName::WindowMaximize, "Maximize")
                            .on_click(|_, window, cx| {
                                cx.stop_propagation();
                                window.zoom_window();
                            }),
                    )
                    .child(
                        control("titlebar-close", IconName::WindowClose, "Close").on_click(
                            |_, window, cx| {
                                cx.stop_propagation();
                                window.remove_window();
                            },
                        ),
                    ),
            )
            .into_any_element()
    }

    fn render_main(&self, cx: &mut Context<Self>) -> AnyElement {
        match &self.main {
            Main::Thread(view) => view.clone().into_any_element(),
            Main::NewThread(view) => view.clone().into_any_element(),
            Main::Empty => {
                let key = |keys: &str| crate::actions::shortcut(keys, cx);
                let hints = [
                    (key("secondary-n"), "New thread"),
                    (key("secondary-o"), "Add project"),
                    (
                        format!("{}  {}", key("secondary-shift-["), key("secondary-shift-]")),
                        "Previous / next thread",
                    ),
                    (key("secondary-shift-s"), "Settle thread"),
                    (key("secondary-g"), "Go to agent"),
                    (key("secondary-,"), "Settings"),
                ];
                let error = self.preferences.read(cx).error().cloned();
                let theme = cx.theme();
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .bg(theme.background)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .text_color(theme.foreground)
                            .font_medium()
                            .child("No thread open"),
                    )
                    .children(hints.into_iter().map(|(keys, what)| {
                        gpui_kit::component::h_flex()
                            .gap_3()
                            .child(div().w_24().text_right().child(keys))
                            .child(div().w_40().child(what))
                    }))
                    .when_some(error, |this, error| {
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

/// `v0.1.0 · d2112d3`: the build's version and source commit.
const VERSION_LABEL: &str = concat!(
    "v",
    env!("CARGO_PKG_VERSION"),
    " · ",
    env!("J3_GIT_COMMIT")
);

/// The build stamp in the window's bottom-right corner: small and faded so it
/// reads as chrome, not content. It has no handlers, so clicks fall through.
fn version_watermark(cx: &App) -> impl IntoElement {
    div()
        .id("version-watermark")
        .test_support()
        .absolute()
        .bottom_1()
        .right_2()
        .text_xs()
        .text_color(cx.theme().muted_foreground.opacity(0.55))
        .child(VERSION_LABEL)
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let content = match &self.settings {
            Some(settings) => settings.clone().into_any_element(),
            None => h_resizable("workspace-split")
                .child(
                    resizable_panel()
                        .size(rem * 17.)
                        .size_range(rem * 12.5..rem * 30.)
                        .child(self.sidebar.clone()),
                )
                .child(resizable_panel().child(self.render_main(cx)))
                .into_any_element(),
        };
        let titlebar = self.render_titlebar(window, cx);
        // The header row scales with the zoom; keep the traffic lights
        // centred on it.
        #[cfg(target_os = "macos")]
        {
            let row = titlebar::row_height(window);
            if row != self.traffic_lights_row {
                self.traffic_lights_row = row;
                window.set_traffic_light_position(titlebar::traffic_light_position(row));
            }
        }
        v_flex()
            .id("workspace")
            .key_context(WORKSPACE)
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(titlebar)
            .child(div().flex_1().min_h_0().child(content))
            .child(version_watermark(cx))
            .when(cfg!(target_os = "macos"), |this| {
                this.on_mouse_down(MouseButton::Left, titlebar::double_click)
            })
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                if this.settings.is_some() {
                    this.close_settings(window, cx);
                } else {
                    this.open_settings(SettingsPage::General, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &ShowArchived, window, cx| {
                this.open_settings(SettingsPage::Archived, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewThread, window, cx| {
                let (env, project) = this.target_context(cx);
                this.new_thread(env, project, window, cx);
            }))
            .on_action(cx.listener(|this, _: &AddProject, window, cx| {
                let (env, _) = this.target_context(cx);
                this.add_project(env, true, window, cx);
            }))
            .on_action(
                cx.listener(|this, _: &AddEnvironment, window, cx| {
                    this.add_environment(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &StopRun, window, cx| {
                if let Some(view) = this.current_thread().cloned() {
                    view.update(cx, |view, cx| view.stop(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                if let Some(view) = this.current_thread().cloned() {
                    view.update(cx, |view, cx| view.toggle_terminal(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleFiles, window, cx| {
                if let Some(view) = this.current_thread().cloned() {
                    view.update(cx, |view, cx| view.toggle_files(window, cx));
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
            .on_action(
                cx.listener(|this, _: &SettleThread, window, cx| {
                    this.settle_open_thread(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &GotoAgent, window, cx| this.goto_agent(window, cx)))
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                match &this.main {
                    Main::Thread(view) => view
                        .clone()
                        .update(cx, |view, cx| view.focus_composer(window, cx)),
                    Main::NewThread(view) => view
                        .clone()
                        .update(cx, |view, cx| view.focus_composer(window, cx)),
                    Main::Empty => {}
                }
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, _, cx| this.zoom(1, cx)))
            .on_action(cx.listener(|this, _: &ZoomOut, _, cx| this.zoom(-1, cx)))
            .on_action(cx.listener(|this, _: &ResetZoom, _, cx| this.zoom(0, cx)))
            .on_action(cx.listener(|this, _: &FocusSidebar, window, cx| {
                this.close_settings(window, cx);
                this.sidebar
                    .clone()
                    .update(cx, |sidebar, cx| sidebar.focus(window, cx));
            }))
            .on_action(
                cx.listener(|this, _: &NextThread, window, cx| this.open_adjacent(1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &PreviousThread, window, cx| {
                this.open_adjacent(-1, window, cx)
            }))
            .on_action(
                cx.listener(|this, JumpToThread(n): &JumpToThread, window, cx| {
                    if let Some(key) = this.sidebar.read(cx).nth_thread(*n) {
                        this.open_thread(&key, window, cx);
                    }
                }),
            )
    }
}
