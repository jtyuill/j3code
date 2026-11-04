//! One terminal of a thread: an `alacritty_terminal` grid fed by the
//! server's `terminal.attach` stream and painted as monospace text runs.
//! Keystrokes, pastes and resizes go back over `terminal.write` /
//! `terminal.resize`, so the shell runs on the thread's host.

use std::{cell::RefCell, rc::Rc, time::Duration};

use alacritty_terminal::{
    Term,
    event::{Event as TermEvent, EventListener, WindowSize},
    grid::{Dimensions, Scroll},
    index::{Column, Point as GridPoint, Side},
    selection::{Selection, SelectionType},
    term::{Config, TermMode, cell::Flags, point_to_viewport, viewport_to_point},
    vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb},
};
use gpui_kit::component::{
    ActiveTheme as _, Theme,
    input::{Copy, Paste},
};
use gpui_kit::{
    App, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, FocusHandle,
    Focusable, Font, FontStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    KeyDownEvent, Keystroke, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement as _,
    Pixels, Render, Rgba, Role, ScrollEvent, ShapedLine, SharedString,
    StatefulInteractiveElement as _, StrikethroughStyle, Styled as _, Task, TestSupportExt as _,
    TextAlign, TextRun, UTF16Selection, UnderlineStyle, Window, canvas, div, fill, font, outline,
    point, px, size,
};
use j3_client::{
    Connection, Error,
    t3::terminal::{
        MAX_COLS, MAX_ROWS, TerminalAttachStreamEvent, TerminalSessionSnapshot,
        TerminalSessionStatus,
    },
};
use tokio::sync::mpsc;

use crate::actions::TERMINAL;

/// How long a size must hold before the server's PTY follows it.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(120);
/// Wait before re-attaching after the connection dropped.
const REATTACH_DELAY: Duration = Duration::from_secs(2);
/// Most input per `terminal.write` (the contract's limit, in characters).
const MAX_WRITE: usize = 65_536;
/// Line height as a multiple of the font size.
const LINE_HEIGHT: f32 = 1.35;
/// Empty space around the grid.
const PADDING: Pixels = px(6.);

/// Where the terminal's shell is.
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
    Failed(SharedString),
}

/// The grid size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GridSize {
    cols: u16,
    rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

/// The terminal's requests to the host (answers it wants written back).
#[derive(Clone, Default)]
struct Listener(Rc<RefCell<Vec<TermEvent>>>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        if matches!(
            event,
            TermEvent::PtyWrite(_)
                | TermEvent::ColorRequest(..)
                | TermEvent::TextAreaSizeRequest(_)
        ) {
            self.0.borrow_mut().push(event);
        }
    }
}

/// Cell geometry of the last layout.
#[derive(Debug, Clone, Copy)]
struct Metrics {
    origin: gpui_kit::Point<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
}

pub struct TerminalView {
    connection: Connection,
    thread_id: String,
    terminal_id: String,
    term: Term<Listener>,
    parser: Processor,
    listener: Listener,
    size: GridSize,
    /// Whether the size came from a layout (the attach waits for one).
    sized: bool,
    metrics: Option<Metrics>,
    status: TerminalStatus,
    focus: FocusHandle,
    input: mpsc::UnboundedSender<String>,
    /// Input typed while connecting, sent once the snapshot arrives.
    pending: String,
    selecting: bool,
    /// IME composition not yet committed, shown at the cursor.
    marked: Option<String>,
    /// The cursor cell of the last layout, where the IME window goes.
    cursor_bounds: Option<Bounds<Pixels>>,
    /// Wheel movement not yet worth a whole line.
    scroll_rest: Pixels,
    attach: Option<Task<()>>,
    resize: Option<Task<()>>,
    _writer: Task<()>,
}

/// Splits `text` into pieces of at most `max` bytes on character boundaries.
fn chunks(text: &str, max: usize) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let mut end = rest.len().min(max);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            // A character longer than `max` still goes, whole.
            end = rest.chars().next().map_or(rest.len(), char::len_utf8);
        }
        let (piece, tail) = rest.split_at(end);
        rest = tail;
        Some(piece)
    })
}

fn new_term(size: GridSize, listener: &Listener) -> Term<Listener> {
    Term::new(Config::default(), &size, listener.clone())
}

impl TerminalView {
    pub fn new(
        connection: Connection,
        thread_id: &str,
        terminal_id: &str,
        cx: &mut Context<Self>,
    ) -> Self {
        let (input, mut queue) = mpsc::unbounded_channel::<String>();
        let writer = {
            let connection = connection.clone();
            let (thread_id, terminal_id) = (thread_id.to_owned(), terminal_id.to_owned());
            // One write at a time keeps keystrokes in order; what queues up
            // meanwhile goes in the next write.
            cx.spawn(async move |_, _| {
                while let Some(mut data) = queue.recv().await {
                    while let Ok(more) = queue.try_recv() {
                        data.push_str(&more);
                    }
                    for piece in chunks(&data, MAX_WRITE) {
                        if let Err(error) = connection
                            .write_terminal(&thread_id, &terminal_id, piece.to_owned())
                            .await
                        {
                            tracing::debug!(%error, terminal = %terminal_id, "terminal write");
                        }
                    }
                }
            })
        };
        let listener = Listener::default();
        let size = GridSize { cols: 80, rows: 24 };
        Self {
            connection,
            thread_id: thread_id.to_owned(),
            terminal_id: terminal_id.to_owned(),
            term: new_term(size, &listener),
            parser: Processor::new(),
            listener,
            size,
            sized: false,
            metrics: None,
            status: TerminalStatus::Connecting,
            focus: cx.focus_handle(),
            input,
            pending: String::new(),
            selecting: false,
            marked: None,
            cursor_bounds: None,
            scroll_rest: px(0.),
            attach: None,
            resize: None,
            _writer: writer,
        }
    }

    pub fn terminal_id(&self) -> &str {
        &self.terminal_id
    }

    pub fn status(&self) -> &TerminalStatus {
        &self.status
    }

    /// The visible screen as text, one line per row, trailing blanks trimmed.
    pub fn screen_text(&self) -> String {
        let mut text = String::new();
        let mut row = String::new();
        let mut line = None;
        for cell in self.term.grid().display_iter() {
            if line != Some(cell.point.line) {
                if line.is_some() {
                    text.push_str(row.trim_end());
                    text.push('\n');
                    row.clear();
                }
                line = Some(cell.point.line);
            }
            if !cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                row.push(cell.c);
            }
        }
        text.push_str(row.trim_end());
        text.trim_end().to_owned()
    }

    /// Starts a new shell after the last one exited.
    pub fn restart(&mut self, cx: &mut Context<Self>) {
        self.start_attach(true, cx);
    }

    /// Ends the shell on the server (`terminal.close`).
    pub fn close(&self, cx: &mut App) {
        let connection = self.connection.clone();
        let (thread_id, terminal_id) = (self.thread_id.clone(), self.terminal_id.clone());
        cx.spawn(async move |_| {
            if let Err(error) = connection.close_terminal(&thread_id, &terminal_id).await {
                tracing::debug!(%error, terminal = %terminal_id, "closing terminal");
            }
        })
        .detach();
    }

    /// Attaches (again), replacing the current stream; the server's
    /// snapshot resets the screen. A dropped connection re-attaches.
    fn start_attach(&mut self, restart: bool, cx: &mut Context<Self>) {
        self.status = TerminalStatus::Connecting;
        let connection = self.connection.clone();
        let (thread_id, terminal_id) = (self.thread_id.clone(), self.terminal_id.clone());
        self.attach = Some(cx.spawn(async move |this, cx| {
            let mut restart = restart;
            loop {
                let Ok(size) = this.read_with(cx, |this, _| this.size) else {
                    return;
                };
                let mut session = connection.attach_terminal(
                    &thread_id,
                    &terminal_id,
                    Some((size.cols, size.rows)),
                    restart,
                );
                restart = false;
                let failure = loop {
                    match session.next().await {
                        None => break None,
                        Some(Ok(event)) => {
                            if this
                                .update(cx, |this, cx| this.on_event(event, cx))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Some(Err(error)) => break Some(error),
                    }
                };
                let retry = matches!(failure, Some(Error::Disconnected(_) | Error::Interrupted));
                let updated = this.update(cx, |this, cx| {
                    match failure {
                        None if this.status != TerminalStatus::Closed => {
                            this.status = TerminalStatus::Closed;
                        }
                        Some(error) if !retry => {
                            this.status = TerminalStatus::Failed(error.to_string().into());
                        }
                        _ => {}
                    }
                    cx.notify();
                });
                if updated.is_err() || !retry {
                    return;
                }
                cx.background_executor().timer(REATTACH_DELAY).await;
            }
        }));
    }

    fn on_event(&mut self, event: TerminalAttachStreamEvent, cx: &mut Context<Self>) {
        match event {
            TerminalAttachStreamEvent::Snapshot(snapshot) => self.reset(&snapshot.snapshot, cx),
            TerminalAttachStreamEvent::Restarted(restarted) => self.reset(&restarted.snapshot, cx),
            TerminalAttachStreamEvent::Output(output) => self.feed(&output.data, cx),
            TerminalAttachStreamEvent::Exited(exited) => {
                self.status = TerminalStatus::Exited {
                    code: exited.exit_code,
                    signal: exited.exit_signal,
                };
            }
            TerminalAttachStreamEvent::Closed(_) => self.status = TerminalStatus::Closed,
            TerminalAttachStreamEvent::Error(error) => {
                self.status = TerminalStatus::Failed(error.message.into());
            }
            TerminalAttachStreamEvent::Cleared(_) => {
                self.term = new_term(self.size, &self.listener);
            }
            TerminalAttachStreamEvent::Activity(_) | TerminalAttachStreamEvent::Unknown(_) => {
                return;
            }
        }
        cx.notify();
    }

    /// A fresh screen replaying the session's history.
    fn reset(&mut self, snapshot: &TerminalSessionSnapshot, cx: &mut Context<Self>) {
        self.term = new_term(self.size, &self.listener);
        self.parser = Processor::new();
        self.listener.0.borrow_mut().clear();
        self.status = match snapshot.status {
            TerminalSessionStatus::Starting | TerminalSessionStatus::Running => {
                TerminalStatus::Running
            }
            TerminalSessionStatus::Exited => TerminalStatus::Exited {
                code: snapshot.exit_code,
                signal: snapshot.exit_signal,
            },
            TerminalSessionStatus::Error => {
                TerminalStatus::Failed("The shell could not be started.".into())
            }
        };
        // Answers to queries in old output went to the shell long ago.
        self.parser
            .advance(&mut self.term, snapshot.history.as_bytes());
        self.listener.0.borrow_mut().clear();
        let pending = std::mem::take(&mut self.pending);
        if self.status == TerminalStatus::Running {
            self.send(pending);
        }
        cx.notify();
    }

    fn feed(&mut self, data: &str, cx: &mut Context<Self>) {
        self.parser.advance(&mut self.term, data.as_bytes());
        let requests: Vec<TermEvent> = self.listener.0.borrow_mut().drain(..).collect();
        for request in requests {
            match request {
                TermEvent::PtyWrite(text) => self.send(text),
                TermEvent::ColorRequest(index, format) => {
                    let color = to_rgb(self.color(index, cx.theme()));
                    self.send(format(color));
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    if let Some(metrics) = self.metrics {
                        self.send(format(WindowSize {
                            num_lines: self.size.rows,
                            num_cols: self.size.cols,
                            cell_width: f32::from(metrics.cell_width) as u16,
                            cell_height: f32::from(metrics.line_height) as u16,
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    /// Sends input to the shell. Until the session's snapshot arrived the
    /// server may not have it yet, so input waits for it.
    fn send(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        if self.status == TerminalStatus::Connecting {
            self.pending.push_str(&text);
        } else {
            let _ = self.input.send(text);
        }
    }

    /// Follows a new layout size: the screen reflows now, the shell's PTY
    /// after [`RESIZE_DEBOUNCE`]. The first size starts the attach.
    fn set_size(&mut self, size: GridSize, cx: &mut Context<Self>) {
        if self.sized && size == self.size {
            return;
        }
        self.size = size;
        self.term.resize(size);
        if !std::mem::replace(&mut self.sized, true) {
            self.start_attach(false, cx);
            return;
        }
        let connection = self.connection.clone();
        let (thread_id, terminal_id) = (self.thread_id.clone(), self.terminal_id.clone());
        self.resize = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(RESIZE_DEBOUNCE).await;
            if let Err(error) = connection
                .resize_terminal(&thread_id, &terminal_id, size.cols, size.rows)
                .await
            {
                tracing::debug!(%error, terminal = %terminal_id, "terminal resize");
            }
        }));
    }

    // -----------------------------------------------------------------------
    // Input
    // -----------------------------------------------------------------------

    /// Non-text keys; typed text comes through [`EntityInputHandler`].
    fn on_key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = key_input(&event.keystroke, *self.term.mode()) else {
            return;
        };
        cx.stop_propagation();
        self.type_input(text, cx);
    }

    /// Input from the user: back to the live screen, selection cleared.
    fn type_input(&mut self, text: String, cx: &mut Context<Self>) {
        self.term.scroll_display(Scroll::Bottom);
        self.term.selection = None;
        self.send(text);
        cx.notify();
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = self
            .term
            .selection_to_string()
            .filter(|text| !text.is_empty())
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let input = paste_input(&text, *self.term.mode());
        self.type_input(input, cx);
    }

    /// The grid cell under a window position, and which half of it.
    fn grid_point(&self, position: gpui_kit::Point<Pixels>) -> Option<(GridPoint, Side)> {
        let metrics = self.metrics?;
        let x = f32::from((position.x - metrics.origin.x).max(px(0.)));
        let y = f32::from((position.y - metrics.origin.y).max(px(0.)));
        let cell = x / f32::from(metrics.cell_width);
        let column = (cell as usize).min(usize::from(self.size.cols) - 1);
        let line =
            ((y / f32::from(metrics.line_height)) as usize).min(usize::from(self.size.rows) - 1);
        let side = if cell.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        let viewport = GridPoint::new(line, Column(column));
        Some((
            viewport_to_point(self.term.grid().display_offset(), viewport),
            side,
        ))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus.focus(window, cx);
        if let Some((point, side)) = self.grid_point(event.position) {
            let kind = match event.click_count {
                1 => SelectionType::Simple,
                2 => SelectionType::Semantic,
                _ => SelectionType::Lines,
            };
            self.term.selection = Some(Selection::new(kind, point, side));
            self.selecting = true;
        }
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            return;
        }
        if let (Some((point, side)), Some(selection)) = (
            self.grid_point(event.position),
            self.term.selection.as_mut(),
        ) {
            selection.update(point, side);
            cx.notify();
        }
    }

    fn on_mouse_up(&mut self, _: &gpui_kit::MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        if self
            .term
            .selection
            .as_ref()
            .is_some_and(Selection::is_empty)
        {
            self.term.selection = None;
            cx.notify();
        }
    }

    fn on_wheel(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        cx.stop_propagation();
        self.scroll_rest += event.delta.pixel_delta(metrics.line_height).y;
        let lines = (self.scroll_rest / metrics.line_height) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_rest -= metrics.line_height * lines as f32;
        let mode = *self.term.mode();
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !event.modifiers.shift
        {
            // Full-screen programs (less, vim) scroll by cursor keys.
            let key = if lines > 0 { 'A' } else { 'B' };
            let prefix = if mode.contains(TermMode::APP_CURSOR) {
                "\x1bO"
            } else {
                "\x1b["
            };
            self.send(format!("{prefix}{key}").repeat(lines.unsigned_abs() as usize));
        } else {
            self.term.scroll_display(Scroll::Delta(lines));
            cx.notify();
        }
    }

    // -----------------------------------------------------------------------
    // Layout and paint
    // -----------------------------------------------------------------------

    /// The colour of palette entry `index` (0-255, then foreground,
    /// background, cursor), as the program set it or from the theme.
    fn color(&self, index: usize, theme: &Theme) -> Hsla {
        if let Some(rgb) = self.term.colors()[index] {
            return from_rgb(rgb);
        }
        palette(index, theme)
    }

    fn resolve(&self, color: Color, theme: &Theme) -> Hsla {
        match color {
            Color::Spec(rgb) => from_rgb(rgb),
            Color::Indexed(index) => self.color(usize::from(index), theme),
            Color::Named(named) => match named {
                NamedColor::BrightForeground => self.color(NamedColor::Foreground as usize, theme),
                NamedColor::DimForeground => theme.muted_foreground,
                named if (named as usize) >= NamedColor::DimBlack as usize => {
                    let base = named as usize - NamedColor::DimBlack as usize;
                    self.color(base, theme).opacity(0.7)
                }
                named => self.color(named as usize, theme),
            },
        }
    }

    /// Sizes the grid to `bounds` and lays out its cells.
    fn layout(
        &mut self,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TerminalLayout {
        // The theme's mono font at the terminal font size.
        let font_family = cx.theme().mono_font_family.clone();
        let font_size = crate::appearance::terminal_font_size(cx);
        let base_font = font(font_family);
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&base_font);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map(|advance| advance.width)
            .unwrap_or(font_size * 0.6);
        let line_height = (font_size * LINE_HEIGHT).round();
        let origin = bounds.origin + point(PADDING, PADDING);
        let inner = bounds.size - size(PADDING * 2., PADDING * 2.);
        let fit = |space: Pixels, cell: Pixels, max: u16| {
            ((f32::from(space) / f32::from(cell)).floor() as u16).clamp(2, max)
        };
        let grid = GridSize {
            cols: fit(inner.width, cell_width, MAX_COLS),
            rows: fit(inner.height, line_height, MAX_ROWS),
        };
        self.metrics = Some(Metrics {
            origin,
            cell_width,
            line_height,
        });
        self.set_size(grid, cx);
        let theme = cx.theme();

        let focused = self.focus.is_focused(window);
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        let cursor_cell = (content.mode.contains(TermMode::SHOW_CURSOR)
            && content.cursor.shape != CursorShape::Hidden)
            .then(|| point_to_viewport(display_offset, content.cursor.point))
            .flatten()
            .filter(|cell| cell.line < usize::from(grid.rows));
        let block_cursor = focused && content.cursor.shape == CursorShape::Block;
        let default_bg = theme.background;
        let cursor_color = self.color(NamedColor::Cursor as usize, theme);

        let mut layout = TerminalLayout {
            line_height,
            backgrounds: Vec::new(),
            runs: Vec::new(),
            cursor: None,
            marked: None,
        };
        let mut batch: Option<Batch> = None;
        let cell_origin = |line: usize, column: usize| {
            origin + point(cell_width * column as f32, line_height * line as f32)
        };
        for cell in content.display_iter {
            let flags = cell.flags;
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let Some(at) = point_to_viewport(display_offset, cell.point) else {
                continue;
            };
            let width = if flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let mut fg = self.resolve(cell.fg, theme);
            let mut bg = match cell.bg {
                Color::Named(NamedColor::Background) => None,
                color => Some(self.resolve(color, theme)),
            };
            if flags.contains(Flags::INVERSE) {
                let back = bg.unwrap_or(default_bg);
                bg = Some(fg);
                fg = back;
            }
            if flags.contains(Flags::DIM) {
                fg = fg.opacity(0.7);
            }
            if content
                .selection
                .is_some_and(|selection| selection.contains(cell.point))
            {
                bg = Some(theme.selection);
            }
            let is_cursor = cursor_cell == Some(at);
            if is_cursor {
                let bounds = Bounds::new(
                    cell_origin(at.line, at.column.0),
                    size(cell_width * width as f32, line_height),
                );
                if block_cursor {
                    bg = Some(cursor_color);
                    fg = default_bg;
                } else {
                    layout.cursor = Some((bounds, cursor_color, focused, content.cursor.shape));
                }
            }
            if let Some(bg) = bg {
                let bounds = Bounds::new(
                    cell_origin(at.line, at.column.0),
                    size(cell_width * width as f32, line_height),
                );
                match layout.backgrounds.last_mut() {
                    Some((last, color))
                        if *color == bg
                            && last.top() == bounds.top()
                            && last.right() == bounds.left() =>
                    {
                        last.size.width += bounds.size.width;
                    }
                    _ => layout.backgrounds.push((bounds, bg)),
                }
            }
            if flags.contains(Flags::HIDDEN) {
                continue;
            }
            let style = CellStyle {
                color: fg,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                undercurl: flags.contains(Flags::UNDERCURL),
                strike: flags.contains(Flags::STRIKEOUT),
            };
            let continues = batch.as_ref().is_some_and(|batch| {
                width == 1
                    && batch.width == 1
                    && batch.line == at.line
                    && batch.column + batch.cells == at.column.0
                    && batch.style == style
            });
            if !continues {
                if let Some(done) = batch.take() {
                    layout.runs.push(done.shape(
                        &base_font,
                        font_size,
                        cell_width,
                        &text_system,
                        &cell_origin,
                    ));
                }
                batch = Some(Batch {
                    line: at.line,
                    column: at.column.0,
                    cells: 0,
                    width,
                    text: String::new(),
                    style,
                });
            }
            let current = batch.as_mut().expect("a batch was just started");
            current.text.push(cell.c);
            if let Some(extra) = cell.zerowidth() {
                current.text.extend(extra);
            }
            current.cells += 1;
        }
        if let Some(done) = batch.take() {
            layout.runs.push(done.shape(
                &base_font,
                font_size,
                cell_width,
                &text_system,
                &cell_origin,
            ));
        }
        self.cursor_bounds = cursor_cell.map(|at| {
            Bounds::new(
                cell_origin(at.line, at.column.0),
                size(cell_width, line_height),
            )
        });
        // IME composition shows at the cursor, underlined, until committed.
        if let (Some(text), Some(at)) = (self.marked.as_ref(), self.cursor_bounds) {
            let run = TextRun {
                len: text.len(),
                font: base_font.clone(),
                color: theme.foreground,
                background_color: None,
                underline: Some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(theme.foreground),
                    wavy: false,
                }),
                strikethrough: None,
            };
            let line = text_system.shape_line(
                SharedString::from(text.clone()),
                font_size,
                &[run],
                Some(cell_width),
            );
            let bounds = Bounds::new(at.origin, size(line.width(), line_height));
            layout.marked = Some((bounds, default_bg, line));
        }
        layout
    }
}

/// Text style of one cell.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CellStyle {
    color: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    undercurl: bool,
    strike: bool,
}

/// Neighbouring cells of one row with the same style, shaped together.
struct Batch {
    line: usize,
    column: usize,
    cells: usize,
    /// Cells per character: 2 for a wide character (always alone).
    width: usize,
    text: String,
    style: CellStyle,
}

impl Batch {
    fn shape(
        self,
        base: &Font,
        font_size: Pixels,
        cell_width: Pixels,
        text_system: &gpui_kit::WindowTextSystem,
        cell_origin: &impl Fn(usize, usize) -> gpui_kit::Point<Pixels>,
    ) -> (gpui_kit::Point<Pixels>, ShapedLine) {
        let mut font = base.clone();
        if self.style.bold {
            font.weight = FontWeight::BOLD;
        }
        if self.style.italic {
            font.style = FontStyle::Italic;
        }
        let color = self.style.color;
        let run = TextRun {
            len: self.text.len(),
            font,
            color,
            background_color: None,
            underline: self.style.underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(color),
                wavy: self.style.undercurl,
            }),
            strikethrough: self.style.strike.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(color),
            }),
        };
        let line = text_system.shape_line(
            self.text.into(),
            font_size,
            &[run],
            Some(cell_width * self.width as f32),
        );
        (cell_origin(self.line, self.column), line)
    }
}

/// What a frame paints: cell backgrounds, text runs and a non-block cursor.
struct TerminalLayout {
    line_height: Pixels,
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    runs: Vec<(gpui_kit::Point<Pixels>, ShapedLine)>,
    /// Bounds, colour, focused, shape.
    cursor: Option<(Bounds<Pixels>, Hsla, bool, CursorShape)>,
    /// IME composition: its background, colour and text.
    marked: Option<(Bounds<Pixels>, Hsla, ShapedLine)>,
}

impl TerminalLayout {
    fn paint(self, window: &mut Window, cx: &mut App) {
        for (bounds, color) in self.backgrounds {
            window.paint_quad(fill(bounds, color));
        }
        for (origin, line) in &self.runs {
            let _ = line.paint(*origin, self.line_height, TextAlign::Left, None, window, cx);
        }
        if let Some((bounds, color, focused, shape)) = self.cursor {
            let quad = match shape {
                CursorShape::Beam if focused => fill(
                    Bounds::new(bounds.origin, size(px(2.), bounds.size.height)),
                    color,
                ),
                CursorShape::Underline if focused => fill(
                    Bounds::new(
                        bounds.origin + point(px(0.), bounds.size.height - px(2.)),
                        size(bounds.size.width, px(2.)),
                    ),
                    color,
                ),
                _ => outline(bounds, color, gpui_kit::BorderStyle::Solid),
            };
            window.paint_quad(quad);
        }
        if let Some((bounds, background, line)) = self.marked {
            window.paint_quad(fill(bounds, background));
            let _ = line.paint(
                bounds.origin,
                self.line_height,
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }
}

fn from_rgb(rgb: Rgb) -> Hsla {
    Rgba {
        r: f32::from(rgb.r) / 255.,
        g: f32::from(rgb.g) / 255.,
        b: f32::from(rgb.b) / 255.,
        a: 1.,
    }
    .into()
}

fn to_rgb(color: Hsla) -> Rgb {
    let rgba = color.to_rgb();
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    Rgb {
        r: channel(rgba.r),
        g: channel(rgba.g),
        b: channel(rgba.b),
    }
}

/// The theme's palette: the 16 ANSI colours from its base colours, the
/// xterm 6×6×6 cube and grey ramp, then foreground, background and cursor.
fn palette(index: usize, theme: &Theme) -> Hsla {
    let dark = theme.is_dark();
    match index {
        0 => {
            if dark {
                theme.muted
            } else {
                theme.foreground
            }
        }
        1 => theme.red,
        2 => theme.green,
        3 => theme.yellow,
        4 => theme.blue,
        5 => theme.magenta,
        6 => theme.cyan,
        7 => {
            if dark {
                theme.foreground.opacity(0.85)
            } else {
                theme.muted_foreground
            }
        }
        8 => theme.muted_foreground,
        9 => theme.red_light,
        10 => theme.green_light,
        11 => theme.yellow_light,
        12 => theme.blue_light,
        13 => theme.magenta_light,
        14 => theme.cyan_light,
        15 => {
            if dark {
                theme.foreground
            } else {
                theme.muted_foreground
            }
        }
        16..=231 => {
            let n = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { 55 + v as u8 * 40 };
            from_rgb(Rgb {
                r: level(n / 36),
                g: level(n / 6 % 6),
                b: level(n % 6),
            })
        }
        232..=255 => {
            let grey = 8 + (index - 232) as u8 * 10;
            from_rgb(Rgb {
                r: grey,
                g: grey,
                b: grey,
            })
        }
        257 => theme.background,
        _ => theme.foreground,
    }
}

/// `text` as typed input: bracketed when the program asked for it, else
/// with newlines as Enter.
fn paste_input(text: &str, mode: TermMode) -> String {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        // A pasted end marker must not end the paste early.
        let text = text.replace('\x1b', "");
        format!("\x1b[200~{text}\x1b[201~")
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r")
    }
}

fn meta(alt: bool, text: &str) -> String {
    if alt {
        format!("\x1b{text}")
    } else {
        text.to_owned()
    }
}

/// The C0 control byte for ctrl+`key` (xterm's table), if it has one.
fn control_byte(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    Some(match c.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '/' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

/// What a non-text keystroke sends to the shell (xterm sequences): editing
/// and cursor keys, F-keys, Ctrl and Alt (as Meta: ESC prefix)
/// combinations. Cursor keys follow application cursor mode. `None` for
/// ⌘ shortcuts, bare modifiers and plain typed text, which arrives through
/// the input handler (IME, dead keys, the emoji picker).
pub fn key_input(keystroke: &Keystroke, mode: TermMode) -> Option<String> {
    let modifiers = &keystroke.modifiers;
    if modifiers.platform {
        return None;
    }
    let (shift, alt, control) = (modifiers.shift, modifiers.alt, modifiers.control);
    let code = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    let cursor = |end: char| {
        if code > 1 {
            format!("\x1b[1;{code}{end}")
        } else if mode.contains(TermMode::APP_CURSOR) {
            format!("\x1bO{end}")
        } else {
            format!("\x1b[{end}")
        }
    };
    let function = |end: char| {
        if code > 1 {
            format!("\x1b[1;{code}{end}")
        } else {
            format!("\x1bO{end}")
        }
    };
    let tilde = |n: u8| {
        if code > 1 {
            format!("\x1b[{n};{code}~")
        } else {
            format!("\x1b[{n}~")
        }
    };
    let key = keystroke.key.as_str();
    Some(match key {
        "up" => cursor('A'),
        "down" => cursor('B'),
        "right" => cursor('C'),
        "left" => cursor('D'),
        "home" => cursor('H'),
        "end" => cursor('F'),
        "insert" => tilde(2),
        "delete" => tilde(3),
        "pageup" => tilde(5),
        "pagedown" => tilde(6),
        "f1" => function('P'),
        "f2" => function('Q'),
        "f3" => function('R'),
        "f4" => function('S'),
        "f5" => tilde(15),
        "f6" => tilde(17),
        "f7" => tilde(18),
        "f8" => tilde(19),
        "f9" => tilde(20),
        "f10" => tilde(21),
        "f11" => tilde(23),
        "f12" => tilde(24),
        "enter" => meta(alt, "\r"),
        "tab" if shift => "\x1b[Z".to_owned(),
        "tab" => meta(alt, "\t"),
        "escape" => meta(alt, "\x1b"),
        "backspace" if control => meta(alt, "\x08"),
        "backspace" => meta(alt, "\x7f"),
        "space" if control => meta(alt, "\0"),
        "space" if alt => meta(true, " "),
        _ => {
            if control && let Some(byte) = control_byte(key) {
                return Some(meta(alt, &char::from(byte).to_string()));
            }
            if !alt || key.chars().count() != 1 {
                return None;
            }
            // Option composes characters on macOS (⌥s is ß); as Meta it
            // sends ESC and the key itself.
            let text = match keystroke.key_char.as_deref() {
                Some(typed) if typed.is_ascii() => typed.to_owned(),
                _ if shift => key.to_uppercase(),
                _ => key.to_owned(),
            };
            meta(true, &text)
        }
    })
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Typed text, IME composition and dead keys. The terminal has no editable
/// text of its own: committed text goes to the shell, and the composition
/// is shown at the cursor until then.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: &mut Option<std::ops::Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .marked
            .as_ref()
            .map_or(0, |text| text.encode_utf16().count());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.marked
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        self.type_input(text.to_owned(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<std::ops::Range<usize>>,
        text: &str,
        _: Option<std::ops::Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!text.is_empty()).then(|| text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: std::ops::Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds
    }

    fn character_index_for_point(
        &mut self,
        _: gpui_kit::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity();
        let focus = self.focus.clone();
        div()
            .id(SharedString::from(format!("terminal-{}", self.terminal_id)))
            .test_support()
            .role(Role::Terminal)
            .aria_label(SharedString::from(format!("Terminal {}", self.terminal_id)))
            .aria_value(self.screen_text())
            .key_context(TERMINAL)
            .track_focus(&self.focus)
            .size_full()
            .overflow_hidden()
            .bg(cx.theme().background)
            .cursor_text()
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_scroll(cx.listener(Self::on_wheel))
            .child(
                canvas(
                    {
                        let view = view.clone();
                        move |bounds, window, cx| {
                            view.update(cx, |this, cx| this.layout(bounds, window, cx))
                        }
                    },
                    move |bounds, layout, window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, view), cx);
                        layout.paint(window, cx)
                    },
                )
                .size_full(),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &'static str) -> Option<String> {
        keys_in(text, TermMode::default())
    }

    fn keys_in(text: &'static str, mode: TermMode) -> Option<String> {
        let mut keystroke = Keystroke::parse(text).unwrap();
        if keystroke.key_char.is_none()
            && keystroke.key.chars().count() == 1
            && !keystroke.modifiers.control
            && !keystroke.modifiers.platform
        {
            keystroke.key_char = Some(if keystroke.modifiers.shift {
                keystroke.key.to_uppercase()
            } else {
                keystroke.key.clone()
            });
        }
        key_input(&keystroke, mode)
    }

    #[test]
    fn typed_text_is_left_to_the_input_handler() {
        assert_eq!(keys("a"), None);
        assert_eq!(keys("shift-a"), None);
        assert_eq!(keys("space"), None);
        assert_eq!(keys("1"), None);
        let mut composed = Keystroke::parse("é").unwrap();
        composed.key_char = Some("é".into());
        assert_eq!(key_input(&composed, TermMode::default()), None);
        // Space with a modifier is still a key.
        assert_eq!(keys("ctrl-space").as_deref(), Some("\0"));
        assert_eq!(keys("alt-space").as_deref(), Some("\x1b "));
    }

    #[test]
    fn editing_keys_send_control_characters() {
        assert_eq!(keys("enter").as_deref(), Some("\r"));
        assert_eq!(keys("backspace").as_deref(), Some("\x7f"));
        assert_eq!(keys("ctrl-backspace").as_deref(), Some("\x08"));
        assert_eq!(keys("tab").as_deref(), Some("\t"));
        assert_eq!(keys("shift-tab").as_deref(), Some("\x1b[Z"));
        assert_eq!(keys("escape").as_deref(), Some("\x1b"));
        assert_eq!(keys("delete").as_deref(), Some("\x1b[3~"));
        assert_eq!(keys("insert").as_deref(), Some("\x1b[2~"));
        assert_eq!(keys("pageup").as_deref(), Some("\x1b[5~"));
        assert_eq!(keys("pagedown").as_deref(), Some("\x1b[6~"));
    }

    #[test]
    fn cursor_keys_follow_application_cursor_mode() {
        assert_eq!(keys("up").as_deref(), Some("\x1b[A"));
        assert_eq!(keys("left").as_deref(), Some("\x1b[D"));
        assert_eq!(keys("home").as_deref(), Some("\x1b[H"));
        assert_eq!(keys("end").as_deref(), Some("\x1b[F"));
        assert_eq!(
            keys_in("down", TermMode::APP_CURSOR).as_deref(),
            Some("\x1bOB")
        );
        assert_eq!(
            keys_in("home", TermMode::APP_CURSOR).as_deref(),
            Some("\x1bOH")
        );
        // Modified cursor keys use the CSI 1;m form in either mode.
        assert_eq!(keys("shift-up").as_deref(), Some("\x1b[1;2A"));
        assert_eq!(
            keys_in("ctrl-right", TermMode::APP_CURSOR).as_deref(),
            Some("\x1b[1;5C")
        );
        assert_eq!(keys("alt-left").as_deref(), Some("\x1b[1;3D"));
    }

    #[test]
    fn function_keys() {
        assert_eq!(keys("f1").as_deref(), Some("\x1bOP"));
        assert_eq!(keys("f4").as_deref(), Some("\x1bOS"));
        assert_eq!(keys("f5").as_deref(), Some("\x1b[15~"));
        assert_eq!(keys("f12").as_deref(), Some("\x1b[24~"));
        assert_eq!(keys("shift-f1").as_deref(), Some("\x1b[1;2P"));
        assert_eq!(keys("ctrl-f5").as_deref(), Some("\x1b[15;5~"));
    }

    #[test]
    fn control_and_meta_combinations() {
        assert_eq!(keys("ctrl-c").as_deref(), Some("\x03"));
        assert_eq!(keys("ctrl-a").as_deref(), Some("\x01"));
        assert_eq!(keys("ctrl-z").as_deref(), Some("\x1a"));
        assert_eq!(keys("ctrl-[").as_deref(), Some("\x1b"));
        assert_eq!(keys("ctrl-\\").as_deref(), Some("\x1c"));
        assert_eq!(keys("ctrl-space").as_deref(), Some("\0"));
        assert_eq!(keys("ctrl-alt-x").as_deref(), Some("\x1b\x18"));
        assert_eq!(keys("alt-b").as_deref(), Some("\x1bb"));
        assert_eq!(keys("alt-shift-b").as_deref(), Some("\x1bB"));
        assert_eq!(keys("alt-backspace").as_deref(), Some("\x1b\x7f"));
        // macOS Option composes ß for ⌥s; as Meta it is ESC s.
        let mut option_s = Keystroke::parse("alt-s").unwrap();
        option_s.key_char = Some("ß".into());
        assert_eq!(
            key_input(&option_s, TermMode::default()).as_deref(),
            Some("\x1bs")
        );
    }

    #[test]
    fn shortcuts_and_bare_modifiers_are_not_input() {
        assert_eq!(keys("cmd-c"), None);
        assert_eq!(keys("cmd-v"), None);
        let mut shift = Keystroke::parse("shift").unwrap();
        shift.key_char = None;
        assert_eq!(key_input(&shift, TermMode::default()), None);
    }

    #[test]
    fn pastes_are_bracketed_when_asked() {
        assert_eq!(paste_input("a\nb", TermMode::default()), "a\rb");
        assert_eq!(paste_input("a\r\nb", TermMode::default()), "a\rb");
        assert_eq!(
            paste_input("x\x1b[201~y", TermMode::BRACKETED_PASTE),
            "\x1b[200~x[201~y\x1b[201~"
        );
    }

    #[test]
    fn writes_split_on_character_boundaries() {
        let text = "aé🦀b";
        let pieces: Vec<&str> = chunks(text, 3).collect();
        assert_eq!(pieces.concat(), text);
        assert!(pieces.iter().all(|piece| piece.len() <= 4));
        assert_eq!(chunks("", 3).count(), 0);
    }

    #[test]
    fn the_xterm_cube_and_grey_ramp() {
        assert_eq!(
            to_rgb(from_rgb(Rgb { r: 1, g: 2, b: 3 })),
            Rgb { r: 1, g: 2, b: 3 }
        );
        let theme = Theme::default();
        assert_eq!(to_rgb(palette(16, &theme)), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            to_rgb(palette(231, &theme)),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(to_rgb(palette(196, &theme)), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(to_rgb(palette(232, &theme)), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            to_rgb(palette(255, &theme)),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }
}
