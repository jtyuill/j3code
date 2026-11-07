//! The per-thread terminal drawer (spec §5.4, §8.6): tabs of shells on the thread's server
//! under the chat, kept per `(environment, thread)` for the session, so a thread opened again
//! shows its tabs, scrollback and height as they were. ⌃` toggles it; hiding it returns
//! focus to the composer.
//!
//! Each tab is a [`Terminal`]: a [`screen::Screen`] (`alacritty_terminal`) fed by the
//! server's `terminal.attach` stream through the inbox, a serialised writer for input, and a
//! hidden one-cell text field at the cursor that carries keyboard focus (keyed
//! `terminal-{id}`, so `KeyContext::Terminal` applies) and the input method's composition.
//! Sizing, selection, wheel and keys arrive through the view's handlers ([`input`]); the
//! grid draws in [`render`].

pub mod fixture;
mod input;
mod keys;
mod palette;
mod render;
mod screen;
#[cfg(test)]
mod tests;
mod view;

use std::{cell::Cell, time::Duration};

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use j3_client::{
    Connection, Error,
    t3::terminal::{
        MAX_COLS, MAX_ROWS, TerminalAttachStreamEvent, TerminalSessionSnapshot,
        TerminalSessionStatus,
    },
};
use rune::{TextEdit, TextStyle, Tx};
use tokio::sync::mpsc;

pub use keys::{key_input, paste_input};
pub use palette::Palette;
pub use screen::{GridSize, Screen};
pub use view::view;

use crate::{app::J3, sidebar::ThreadKey};

/// How long a size must hold before the server's PTY follows it, in seconds.
const RESIZE_DEBOUNCE: f64 = 0.12;
/// Wait before re-attaching after the connection dropped, in seconds.
const REATTACH_DELAY: f64 = 2.0;
/// Most input per `terminal.write` (the contract's limit).
const MAX_WRITE: usize = 65_536;
/// Line height as a multiple of the font size.
const LINE_HEIGHT: f32 = 1.35;
/// Empty space around the grid, in points.
pub const PADDING: f32 = 6.0;
/// The drawer's default height and its range, in points.
pub const DEFAULT_HEIGHT: f32 = 280.0;
pub const MIN_HEIGHT: f32 = 120.0;
pub const MAX_HEIGHT: f32 = 2000.0;

/// Where a terminal's shell is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalStatus {
    /// Waiting for the server's snapshot.
    Connecting,
    Running,
    /// The shell ended; it can be restarted.
    Exited {
        code: Option<i64>,
        signal: Option<i64>,
    },
    /// `terminal.close` ended it.
    Closed,
    /// The shell could not start or the server refused the terminal.
    Failed(String),
}

impl TerminalStatus {
    /// What the status strip says, None while the shell runs (or is being reached).
    pub fn message(&self) -> Option<String> {
        Some(match self {
            TerminalStatus::Exited {
                signal: Some(signal),
                ..
            } => format!("Shell ended by signal {signal}"),
            TerminalStatus::Exited {
                code: Some(code), ..
            } => format!("Shell exited with code {code}"),
            TerminalStatus::Exited { .. } => "Shell exited".to_owned(),
            TerminalStatus::Closed => "Terminal closed".to_owned(),
            TerminalStatus::Failed(message) => message.clone(),
            TerminalStatus::Connecting | TerminalStatus::Running => return None,
        })
    }

    fn of(snapshot: &TerminalSessionSnapshot) -> Self {
        match snapshot.status {
            TerminalSessionStatus::Starting | TerminalSessionStatus::Running => {
                TerminalStatus::Running
            }
            TerminalSessionStatus::Exited => TerminalStatus::Exited {
                code: snapshot.exit_code,
                signal: snapshot.exit_signal,
            },
            TerminalSessionStatus::Error => {
                TerminalStatus::Failed("The shell could not be started.".to_owned())
            }
        }
    }
}

/// The grid's cell geometry at a font size: the mono font, the advance of `m` and the line
/// height (1.35 × the size, rounded).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    pub font: TextStyle,
    pub width: f32,
    pub height: f32,
}

impl CellMetrics {
    pub fn new(font_size: f32) -> Self {
        let height = (font_size * LINE_HEIGHT).round();
        let font = TextStyle::new(font_size, 400.0).mono().lh(height);
        let width = rune::text::advance("m", font);
        Self {
            font,
            width: if width > 0.0 { width } else { font_size * 0.6 },
            height,
        }
    }

    /// The grid that fits a `[width, height]` area (padding included), clamped to
    /// 2..=`MAX_COLS` × 2..=`MAX_ROWS`.
    pub fn grid(&self, [width, height]: [f32; 2]) -> GridSize {
        let fit = |space: f32, cell: f32, max: u16| {
            (((space - 2.0 * PADDING) / cell).floor().max(0.0) as u16).clamp(2, max)
        };
        GridSize {
            cols: fit(width, self.width, MAX_COLS),
            rows: fit(height, self.height, MAX_ROWS),
        }
    }
}

/// One shell of a drawer, `term-N`.
pub struct Terminal {
    pub id: String,
    screen: Screen,
    status: TerminalStatus,
    /// Input typed while connecting, sent once the snapshot arrives.
    pending: String,
    /// The writer task's queue (`terminal.write`, one call at a time).
    writer: mpsc::UnboundedSender<String>,
    /// Keyboard focus and the input method's composition (see the module docs). Its text is
    /// always sent on at once.
    edit: TextEdit,
    /// Wheel movement not yet worth a whole line.
    scroll_rest: f32,
    /// The view's last laid-out `[width, height]`.
    area: Option<[f32; 2]>,
    /// Whether the size came from a layout: the first attach waits for one.
    sized: bool,
    /// The running attach stream's task.
    attach: Option<tokio::task::AbortHandle>,
    /// Which attach events still apply: older streams' events are dropped.
    generation: u64,
    /// Clock time to re-attach at after the connection dropped.
    reattach_at: Option<f64>,
    /// Clock time to resize the server's PTY at.
    resize_at: Option<f64>,
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if let Some(attach) = self.attach.take() {
            attach.abort();
        }
    }
}

impl Terminal {
    fn new(id: String, writer: mpsc::UnboundedSender<String>) -> Self {
        Self {
            id,
            screen: Screen::new(GridSize::INITIAL),
            status: TerminalStatus::Connecting,
            pending: String::new(),
            writer,
            edit: TextEdit::new(""),
            scroll_rest: 0.0,
            area: None,
            sized: false,
            attach: None,
            generation: 0,
            reattach_at: None,
            resize_at: None,
        }
    }

    pub fn status(&self) -> &TerminalStatus {
        &self.status
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// "Terminal 2" for `term-2` (t3's `getTerminalLabel`).
    pub fn label(&self) -> String {
        label(&self.id)
    }

    /// Asks for keyboard focus on the next frame. The field model is replaced (it never holds
    /// text), so Rune takes it as a new request even when the field was focused when it last
    /// left the screen; a composition in progress keeps its model.
    fn request_focus(&mut self) {
        if self.edit.preedit.is_none() {
            self.edit = TextEdit::new("");
        }
        self.edit.focused = true;
    }

    /// Sends input to the shell. Until the session's snapshot arrived the server may not
    /// have it yet, so input waits for it.
    fn send(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if self.status == TerminalStatus::Connecting {
            self.pending.push_str(&text);
        } else {
            let _ = self.writer.send(text);
        }
    }

    /// Input from the user: back to the live screen, selection cleared.
    fn type_input(&mut self, text: String) {
        self.screen.scroll_to_bottom();
        self.screen.clear_selection();
        self.send(text);
    }

    /// A fresh screen replaying the session's history; input typed meanwhile goes now.
    fn reset(&mut self, snapshot: &TerminalSessionSnapshot) {
        self.status = TerminalStatus::of(snapshot);
        self.screen.reset(&snapshot.history);
        let pending = std::mem::take(&mut self.pending);
        if self.status == TerminalStatus::Running {
            self.send(pending);
        }
    }

    /// Program output; answers what it asked of the terminal.
    fn feed(&mut self, data: &str, palette: &Palette, cell: CellMetrics, zoom: f32) {
        for request in self.screen.feed(data.as_bytes()) {
            match request {
                TermEvent::PtyWrite(text) => self.send(text),
                TermEvent::ColorRequest(index, format) => {
                    let color = palette.color(self.screen.term().colors(), index);
                    self.send(format(palette::to_rgb(color)));
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    let size = self.screen.size();
                    self.send(format(WindowSize {
                        num_lines: size.rows,
                        num_cols: size.cols,
                        cell_width: (cell.width * zoom).round() as u16,
                        cell_height: (cell.height * zoom).round() as u16,
                    }));
                }
                _ => {}
            }
        }
    }
}

/// "Terminal N" for `term-N`; other ids as they are.
pub fn label(terminal_id: &str) -> String {
    match terminal_id.strip_prefix("term-") {
        Some(n) => format!("Terminal {n}"),
        None => terminal_id.to_owned(),
    }
}

/// One thread's drawer.
pub struct Drawer {
    pub open: bool,
    /// The shells in tab order.
    pub tabs: Vec<Terminal>,
    /// The selected tab's index.
    pub active: usize,
    /// The drawer's height in points ([`MIN_HEIGHT`]..=[`MAX_HEIGHT`]).
    pub height: f32,
    /// What the field accessor returns for a terminal that is gone.
    scratch: TextEdit,
    /// The appearance the drawer last drew in, for answering colour queries.
    dark: Cell<bool>,
}

impl Default for Drawer {
    fn default() -> Self {
        Self {
            open: false,
            tabs: Vec::new(),
            active: 0,
            height: DEFAULT_HEIGHT,
            scratch: TextEdit::new(""),
            dark: Cell::new(true),
        }
    }
}

impl Drawer {
    pub fn active(&self) -> Option<&Terminal> {
        self.tabs.get(self.active)
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn terminal(&self, id: &str) -> Option<&Terminal> {
        self.tabs.iter().find(|t| t.id == id)
    }

    fn terminal_mut(&mut self, id: &str) -> Option<&mut Terminal> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    /// The lowest free `term-N`.
    fn free_id(&self) -> String {
        (1..)
            .map(|n| format!("term-{n}"))
            .find(|id| self.position(id).is_none())
            .expect("some terminal id is free")
    }

    /// Keyboard focus to the selected terminal's field, off the others'.
    fn focus_active(&mut self) {
        let active = self.active;
        for (index, terminal) in self.tabs.iter_mut().enumerate() {
            if index == active {
                terminal.request_focus();
            } else {
                terminal.edit.focused = false;
            }
        }
    }
}

/// Whether the focused element `key` is a terminal, which takes Tab and Ctrl-letters.
pub fn owns_focus(key: &str) -> bool {
    key.starts_with("terminal-")
}

impl J3 {
    /// Opens `key`'s drawer (with a first terminal on first use) and focuses its terminal, or
    /// hides it, refocusing the composer.
    pub fn toggle_terminal(&mut self, key: ThreadKey, tx: &mut Tx) {
        let drawer = self.terminals.entry(key.clone()).or_default();
        if drawer.open {
            self.hide_terminal(&key, tx);
            return;
        }
        drawer.open = true;
        if drawer.tabs.is_empty() {
            self.add_terminal(&key);
        }
        if let Some(drawer) = self.terminals.get_mut(&key) {
            drawer.focus_active();
        }
    }

    /// Hides `key`'s drawer (its shells keep running) and focuses the composer.
    pub fn hide_terminal(&mut self, key: &ThreadKey, tx: &mut Tx) {
        if let Some(drawer) = self.terminals.get_mut(key) {
            drawer.open = false;
            for terminal in &mut drawer.tabs {
                terminal.edit.focused = false;
            }
        }
        self.focus_composer(tx);
    }

    /// Opens another terminal in `key`'s drawer, selects and focuses it.
    pub fn new_terminal(&mut self, key: &ThreadKey) {
        self.add_terminal(key);
        if let Some(drawer) = self.terminals.get_mut(key) {
            drawer.focus_active();
        }
    }

    /// Adds `term-N` (the lowest free N) to `key`'s drawer and selects it. Its shell is
    /// attached once the view laid it out.
    fn add_terminal(&mut self, key: &ThreadKey) {
        let connection = self.envs.get(&key.env).map(|env| env.connection().clone());
        let drawer = self.terminals.entry(key.clone()).or_default();
        let id = drawer.free_id();
        let writer = spawn_writer(
            &self.services.runtime,
            connection,
            key.thread_id.clone(),
            id.clone(),
        );
        let drawer = self.terminals.entry(key.clone()).or_default();
        drawer.tabs.push(Terminal::new(id, writer));
        drawer.active = drawer.tabs.len() - 1;
    }

    /// Selects terminal `id` of `key`'s drawer and focuses it.
    pub fn select_terminal(&mut self, key: &ThreadKey, id: &str) {
        if let Some(drawer) = self.terminals.get_mut(key)
            && let Some(index) = drawer.position(id)
        {
            drawer.active = index;
            drawer.focus_active();
        }
    }

    /// Ends terminal `id`'s shell (`terminal.close`) and drops its tab; the last one hides
    /// the drawer.
    pub fn close_terminal(&mut self, key: &ThreadKey, id: &str, tx: &mut Tx) {
        let Some(drawer) = self.terminals.get_mut(key) else {
            return;
        };
        let Some(index) = drawer.position(id) else {
            return;
        };
        drop(drawer.tabs.remove(index));
        // As the old drawer: the tab before the closed one (or the first) takes over.
        if drawer.active >= index && drawer.active > 0 {
            drawer.active -= 1;
        }
        let empty = drawer.tabs.is_empty();
        if !empty {
            drawer.focus_active();
        }
        if let Some(env) = self.envs.get(&key.env) {
            let connection = env.connection().clone();
            let (thread_id, terminal_id) = (key.thread_id.clone(), id.to_owned());
            self.services.runtime.spawn(async move {
                if let Err(error) = connection.close_terminal(&thread_id, &terminal_id).await {
                    tracing::debug!(%error, terminal = %terminal_id, "closing terminal");
                }
            });
        }
        if empty {
            self.hide_terminal(key, tx);
        }
    }

    /// Starts a new shell for terminal `id` after the last one exited, and focuses it.
    pub fn restart_terminal(&mut self, key: &ThreadKey, id: &str) {
        self.select_terminal(key, id);
        self.attach_terminal(key, id, true);
    }

    /// Attaches terminal `id` (again), replacing its current stream; the server's snapshot
    /// resets the screen. `restart` starts a new shell when the last one ended.
    fn attach_terminal(&mut self, key: &ThreadKey, id: &str, restart: bool) {
        let connection = self.envs.get(&key.env).map(|env| env.connection().clone());
        let runtime = self.services.runtime.clone();
        let inbox = self.inbox.clone();
        let Some(terminal) = self
            .terminals
            .get_mut(key)
            .and_then(|drawer| drawer.terminal_mut(id))
        else {
            return;
        };
        if let Some(attach) = terminal.attach.take() {
            attach.abort();
        }
        terminal.generation += 1;
        terminal.reattach_at = None;
        let Some(connection) = connection else {
            terminal.status = TerminalStatus::Failed("The environment is gone.".to_owned());
            return;
        };
        terminal.status = TerminalStatus::Connecting;
        let generation = terminal.generation;
        let size = terminal.screen.size();
        let (key, id) = (key.clone(), id.to_owned());
        let task = runtime.spawn(async move {
            let mut session = connection.attach_terminal(
                &key.thread_id,
                &id,
                Some((size.cols, size.rows)),
                restart,
            );
            loop {
                let next = session.next().await;
                let done = !matches!(next, Some(Ok(_)));
                let (key, id) = (key.clone(), id.clone());
                inbox.post(move |s: &mut J3, tx: &mut Tx| {
                    s.terminal_attached(&key, &id, generation, next, tx)
                });
                if done {
                    return;
                }
            }
        });
        terminal.attach = Some(task.abort_handle());
    }

    /// One item of terminal `id`'s attach stream (`None`: it ended).
    fn terminal_attached(
        &mut self,
        key: &ThreadKey,
        id: &str,
        generation: u64,
        next: Option<Result<TerminalAttachStreamEvent, Error>>,
        tx: &mut Tx,
    ) {
        let now = self.clock.t;
        let ui = self.prefs.ui();
        let (cell, zoom) = (CellMetrics::new(ui.terminal_font_size()), ui.zoom());
        let Some(drawer) = self.terminals.get_mut(key) else {
            return;
        };
        let dark = drawer.dark.get();
        let Some(terminal) = drawer.terminal_mut(id) else {
            return;
        };
        if terminal.generation != generation {
            return;
        }
        match next {
            Some(Ok(event)) => match event {
                TerminalAttachStreamEvent::Snapshot(snapshot) => terminal.reset(&snapshot.snapshot),
                TerminalAttachStreamEvent::Restarted(restarted) => {
                    terminal.reset(&restarted.snapshot)
                }
                TerminalAttachStreamEvent::Output(output) => {
                    let k = crate::theme::kit(dark, self.prefs.ui());
                    terminal.feed(&output.data, &Palette::new(&k, dark), cell, zoom);
                }
                TerminalAttachStreamEvent::Exited(exited) => {
                    terminal.status = TerminalStatus::Exited {
                        code: exited.exit_code,
                        signal: exited.exit_signal,
                    };
                }
                TerminalAttachStreamEvent::Closed(_) => terminal.status = TerminalStatus::Closed,
                TerminalAttachStreamEvent::Error(error) => {
                    terminal.status = TerminalStatus::Failed(error.message);
                }
                TerminalAttachStreamEvent::Cleared(_) => terminal.screen.clear(),
                TerminalAttachStreamEvent::Activity(_) | TerminalAttachStreamEvent::Unknown(_) => {}
            },
            None => {
                terminal.attach = None;
                terminal.status = TerminalStatus::Closed;
            }
            Some(Err(Error::Disconnected(_) | Error::Interrupted)) => {
                // A dropped connection comes back: attach again after a pause.
                terminal.attach = None;
                terminal.reattach_at = Some(now + REATTACH_DELAY);
                tx.wake_after(Duration::from_secs_f64(REATTACH_DELAY));
            }
            Some(Err(error)) => {
                terminal.attach = None;
                terminal.status = TerminalStatus::Failed(error.to_string());
            }
        }
    }

    /// The timers of every terminal: re-attaching after a dropped connection, the debounced
    /// PTY resize, and following the terminal font size. True when one changed state.
    pub(crate) fn poll_terminals(&mut self, tx: &mut Tx) -> bool {
        if self.terminals.is_empty() {
            return false;
        }
        let now = self.clock.t;
        let cell = CellMetrics::new(self.prefs.ui().terminal_font_size());
        let mut changed = false;
        let mut reattach = Vec::new();
        let mut resize = Vec::new();
        for (key, drawer) in &mut self.terminals {
            for terminal in &mut drawer.tabs {
                if let Some(area) = terminal.area
                    && terminal.sized
                    && terminal.screen.resize(cell.grid(area))
                {
                    terminal.resize_at = Some(now + RESIZE_DEBOUNCE);
                    tx.wake_after(Duration::from_secs_f64(RESIZE_DEBOUNCE));
                    changed = true;
                }
                if terminal.reattach_at.is_some_and(|at| at <= now) {
                    reattach.push((key.clone(), terminal.id.clone()));
                }
                if terminal.resize_at.is_some_and(|at| at <= now) {
                    terminal.resize_at = None;
                    resize.push((key.clone(), terminal.id.clone(), terminal.screen.size()));
                }
            }
        }
        for (key, id) in reattach {
            self.attach_terminal(&key, &id, false);
            changed = true;
        }
        for (key, id, size) in resize {
            let Some(env) = self.envs.get(&key.env) else {
                continue;
            };
            let connection = env.connection().clone();
            self.services.runtime.spawn(async move {
                if let Err(error) = connection
                    .resize_terminal(&key.thread_id, &id, size.cols, size.rows)
                    .await
                {
                    tracing::debug!(%error, terminal = %id, "terminal resize");
                }
            });
        }
        changed
    }
}

/// The task writing terminal `terminal_id`'s input, one `terminal.write` at a time so
/// keystrokes stay in order; what queues up meanwhile goes in the next write, in pieces of
/// at most [`MAX_WRITE`]. It ends when the terminal (its sender) is dropped. Without a
/// connection, input goes nowhere.
fn spawn_writer(
    runtime: &tokio::runtime::Handle,
    connection: Option<Connection>,
    thread_id: String,
    terminal_id: String,
) -> mpsc::UnboundedSender<String> {
    let (sender, mut queue) = mpsc::unbounded_channel::<String>();
    if let Some(connection) = connection {
        runtime.spawn(async move {
            while let Some(mut data) = queue.recv().await {
                while let Ok(more) = queue.try_recv() {
                    data.push_str(&more);
                }
                for piece in screen::chunks(&data, MAX_WRITE) {
                    if let Err(error) = connection
                        .write_terminal(&thread_id, &terminal_id, piece.to_owned())
                        .await
                    {
                        tracing::debug!(%error, terminal = %terminal_id, "terminal write");
                    }
                }
            }
        });
    }
    sender
}
