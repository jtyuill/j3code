//! A thread's terminal drawer (t3's `ThreadTerminalDrawer`): a tab per
//! terminal (`term-1`, `term-2`, ...), `+` for another, × to end one, and a
//! status line with Restart once a shell exited. One drawer per thread
//! lives for the app session, so a thread opened again shows its drawer,
//! tabs, scrollback and height as they were.

use std::collections::HashMap;

use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    resizable::ResizableState,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EntityId, EventEmitter, FocusHandle,
    Focusable, Global, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    Window, div, px,
};
use j3_client::Connection;

use crate::{
    actions::shortcut,
    environment::Environment,
    terminal_view::{TerminalStatus, TerminalView},
};

/// The drawer's keyboard shortcut (`ToggleTerminal`).
pub const TOGGLE_KEYS: &str = "ctrl-`";

/// The drawer asks its thread view to hide it (its last tab closed, or its
/// close button).
pub struct HideDrawer;

/// Every thread's drawer, by environment and thread.
#[derive(Default)]
struct Drawers(HashMap<(EntityId, String), Entity<TerminalDrawer>>);

impl Global for Drawers {}

/// The thread's drawer, if one was opened this session.
pub fn existing(
    environment: &Entity<Environment>,
    thread_id: &str,
    cx: &App,
) -> Option<Entity<TerminalDrawer>> {
    cx.try_global::<Drawers>()?
        .0
        .get(&(environment.entity_id(), thread_id.to_owned()))
        .cloned()
}

/// The thread's drawer, created (closed, without terminals) on first use.
pub fn for_thread(
    environment: &Entity<Environment>,
    thread_id: &str,
    cx: &mut App,
) -> Entity<TerminalDrawer> {
    if let Some(drawer) = existing(environment, thread_id, cx) {
        return drawer;
    }
    let connection = environment.read(cx).connection().clone();
    let drawer = cx.new(|cx| TerminalDrawer::new(connection, thread_id, cx));
    cx.default_global::<Drawers>().0.insert(
        (environment.entity_id(), thread_id.to_owned()),
        drawer.clone(),
    );
    drawer
}

pub struct TerminalDrawer {
    connection: Connection,
    thread_id: String,
    terminals: Vec<Entity<TerminalView>>,
    active: usize,
    open: bool,
    /// The chat/drawer split, so its height survives the thread view.
    split: Entity<ResizableState>,
    focus: FocusHandle,
    /// One per terminal, in tab order: a terminal's status redraws the tabs.
    observers: Vec<Subscription>,
}

impl EventEmitter<HideDrawer> for TerminalDrawer {}

impl TerminalDrawer {
    fn new(connection: Connection, thread_id: &str, cx: &mut Context<Self>) -> Self {
        Self {
            connection,
            thread_id: thread_id.to_owned(),
            terminals: Vec::new(),
            active: 0,
            open: false,
            split: cx.new(|_| ResizableState::default()),
            focus: cx.focus_handle(),
            observers: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn split(&self) -> &Entity<ResizableState> {
        &self.split
    }

    /// Opens the drawer (with a first terminal when it has none) or hides
    /// it. Returns whether it is open now.
    pub fn toggle(&mut self, cx: &mut Context<Self>) -> bool {
        self.open = !self.open;
        if self.open && self.terminals.is_empty() {
            self.add_terminal(cx);
        }
        cx.notify();
        self.open
    }

    /// Moves keyboard focus to the active terminal.
    fn focus_terminal(&self, window: &mut Window, cx: &mut App) {
        if let Some(terminal) = self.terminals.get(self.active) {
            terminal.read(cx).focus_handle(cx).focus(window, cx);
        }
    }

    /// Opens another terminal, `term-N` with the lowest free N, and selects it.
    fn add_terminal(&mut self, cx: &mut Context<Self>) {
        let taken: Vec<String> = self
            .terminals
            .iter()
            .map(|terminal| terminal.read(cx).terminal_id().to_owned())
            .collect();
        let terminal_id = (1..)
            .map(|n| format!("term-{n}"))
            .find(|id| !taken.contains(id))
            .expect("some terminal id is free");
        let connection = self.connection.clone();
        let thread_id = self.thread_id.clone();
        let terminal = cx.new(|cx| TerminalView::new(connection, &thread_id, &terminal_id, cx));
        self.observers
            .push(cx.observe(&terminal, |_, _, cx| cx.notify()));
        self.terminals.push(terminal);
        self.active = self.terminals.len() - 1;
    }

    fn select(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = index;
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// Ends terminal `index`'s shell and drops its tab; the last one hides
    /// the drawer.
    fn close_terminal(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = self.terminals.remove(index);
        terminal.update(cx, |terminal, cx| terminal.close(cx));
        drop(self.observers.remove(index));
        if self.terminals.is_empty() {
            self.active = 0;
            self.hide(cx);
            return;
        }
        if self.active >= index && self.active > 0 {
            self.active -= 1;
        }
        self.focus_terminal(window, cx);
        cx.notify();
    }

    fn hide(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        cx.emit(HideDrawer);
        cx.notify();
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let tabs = self.terminals.iter().enumerate().map(|(index, terminal)| {
            let terminal_id = terminal.read(cx).terminal_id().to_owned();
            let active = index == self.active;
            let label = label(&terminal_id);
            h_flex()
                .id(SharedString::from(format!("terminal-tab-{terminal_id}")))
                .test_support()
                .aria_label(label.clone())
                .h(px(24.))
                .pl_2()
                .pr_1()
                .gap_1()
                .rounded(theme.radius)
                .text_xs()
                .cursor_pointer()
                .when(active, |this| {
                    this.bg(theme.secondary).text_color(theme.foreground)
                })
                .when(!active, |this| {
                    this.text_color(theme.muted_foreground)
                        .hover(|this| this.bg(theme.secondary.opacity(0.5)))
                })
                .on_click(cx.listener(move |this, _, window, cx| this.select(index, window, cx)))
                .child(label)
                .child(
                    Button::new(SharedString::from(format!("terminal-close-{terminal_id}")))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Close)
                        .tooltip("Close terminal")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_terminal(index, window, cx);
                        })),
                )
        });
        let hide_tooltip = format!("Hide terminal ({})", shortcut(TOGGLE_KEYS, cx));
        h_flex()
            .flex_shrink_0()
            .h(px(32.))
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .children(tabs)
            .child(
                Button::new("terminal-new")
                    .ghost()
                    .xsmall()
                    .icon(IconName::Plus)
                    .tooltip("New terminal")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.add_terminal(cx);
                        this.focus_terminal(window, cx);
                        cx.notify();
                    })),
            )
            .child(div().flex_1())
            .child(
                Button::new("terminal-hide")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ChevronDown)
                    .tooltip(hide_tooltip)
                    .on_click(cx.listener(|this, _, _, cx| this.hide(cx))),
            )
            .into_any_element()
    }

    /// Why the active shell is not running, with Restart.
    fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let terminal = self.terminals.get(self.active)?;
        let message: SharedString = match terminal.read(cx).status() {
            TerminalStatus::Exited {
                signal: Some(signal),
                ..
            } => format!("Shell ended by signal {signal}").into(),
            TerminalStatus::Exited { code, .. } => match code {
                Some(code) => format!("Shell exited with code {code}").into(),
                None => "Shell exited".into(),
            },
            TerminalStatus::Closed => "Terminal closed".into(),
            TerminalStatus::Failed(message) => message.clone(),
            TerminalStatus::Connecting | TerminalStatus::Running => return None,
        };
        let theme = cx.theme();
        let terminal = terminal.clone();
        Some(
            h_flex()
                .id("terminal-status")
                .test_support()
                .aria_label(message.clone())
                .flex_shrink_0()
                .h(px(32.))
                .px_3()
                .gap_2()
                .border_t_1()
                .border_color(theme.border)
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(message)
                .child(
                    Button::new("terminal-restart")
                        .outline()
                        .xsmall()
                        .label("Restart")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            terminal.update(cx, |terminal, cx| terminal.restart(cx));
                            this.focus_terminal(window, cx);
                        })),
                )
                .into_any_element(),
        )
    }
}

/// t3's `getTerminalLabel`: `term-2` is "Terminal 2".
fn label(terminal_id: &str) -> SharedString {
    match terminal_id.strip_prefix("term-") {
        Some(n) => format!("Terminal {n}").into(),
        None => SharedString::from(terminal_id.to_owned()),
    }
}

impl Focusable for TerminalDrawer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminals.get(self.active).map_or_else(
            || self.focus.clone(),
            |terminal| terminal.read(cx).focus_handle(cx),
        )
    }
}

impl Render for TerminalDrawer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.render_tabs(cx);
        let status = self.render_status(cx);
        v_flex()
            .id("terminal-drawer")
            .test_support()
            .size_full()
            .bg(cx.theme().background)
            .child(tabs)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .children(self.terminals.get(self.active).cloned()),
            )
            .children(status)
    }
}
