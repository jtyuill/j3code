//! What the terminal view's handlers do: its laid-out size (the grid reflows now, the PTY
//! follows after a debounce, the first size attaches), keys, pastes and the input method
//! through its focus field, selection by pointer, and the wheel.

use std::time::Duration;

use alacritty_terminal::{index::Side, term::TermMode};
use rune::{Drag, KeyEvent, KeyPlatform, Phase, TextEdit, Tx, WheelEvent};

use super::{CellMetrics, PADDING, RESIZE_DEBOUNCE, Terminal, keys};
use crate::{app::J3, sidebar::ThreadKey};

impl J3 {
    fn terminal_mut(&mut self, key: &ThreadKey, id: &str) -> Option<&mut Terminal> {
        self.terminals.get_mut(key)?.terminal_mut(id)
    }

    fn cell_metrics(&self) -> CellMetrics {
        CellMetrics::new(self.prefs.ui().terminal_font_size())
    }

    /// Terminal `id`'s focus field, for its `on_edit` accessor (a scratch field once the
    /// terminal is gone).
    pub(super) fn terminal_edit(&mut self, key: &ThreadKey, id: &str) -> &mut TextEdit {
        let drawer = self.terminals.entry(key.clone()).or_default();
        match drawer.position(id) {
            Some(index) => &mut drawer.tabs[index].edit,
            None => &mut drawer.scratch,
        }
    }

    /// Terminal `id` was laid out `[width, height]` points: the grid follows at once, the
    /// server's PTY after [`RESIZE_DEBOUNCE`]. The first size attaches the shell.
    pub(super) fn terminal_area(&mut self, key: &ThreadKey, id: &str, area: [f32; 2], tx: &mut Tx) {
        let now = self.clock.t;
        let grid = self.cell_metrics().grid(area);
        let Some(terminal) = self.terminal_mut(key, id) else {
            return;
        };
        terminal.area = Some(area);
        let resized = terminal.screen.resize(grid);
        if !std::mem::replace(&mut terminal.sized, true) {
            self.attach_terminal(key, id, false);
        } else if resized {
            terminal.resize_at = Some(now + RESIZE_DEBOUNCE);
            tx.wake_after(Duration::from_secs_f64(RESIZE_DEBOUNCE));
        }
    }

    /// A key while terminal `id`'s field has focus: the copy and paste chords, then the
    /// app's shortcuts (in `KeyContext::Terminal`, which leaves Tab and, off macOS,
    /// Ctrl-letters to the shell), then the shell. False passes the key on.
    pub(super) fn terminal_key(
        &mut self,
        key: &ThreadKey,
        id: &str,
        ev: &KeyEvent,
        tx: &mut Tx,
    ) -> bool {
        let platform = KeyPlatform::current();
        if keys::is_copy(ev, platform) {
            if let Some(text) = self
                .terminal_mut(key, id)
                .and_then(|terminal| terminal.screen.selected_text())
            {
                tx.copy(text);
            }
            return true;
        }
        if keys::is_paste(ev, platform) {
            let (key, id) = (key.clone(), id.to_owned());
            tx.read_clipboard(move |s: &mut J3, content, _| {
                s.terminal_paste(&key, &id, content.text())
            });
            return true;
        }
        if crate::app::on_key(self, ev, tx) {
            return true;
        }
        let base = tx.key_base();
        let Some(terminal) = self.terminal_mut(key, id) else {
            return false;
        };
        match keys::key_input(ev, base, terminal.screen.mode(), platform) {
            Some(input) => {
                terminal.type_input(input);
                true
            }
            None => false,
        }
    }

    /// Pasted `text`: newlines as Enter, bracketed when the program asked for it.
    pub(super) fn terminal_paste(&mut self, key: &ThreadKey, id: &str, text: &str) {
        if let Some(terminal) = self.terminal_mut(key, id) {
            let input = keys::paste_input(text, terminal.screen.mode());
            terminal.type_input(input);
        }
    }

    /// The focus field's text changed: what the input method committed goes to the shell.
    pub(super) fn terminal_committed(&mut self, key: &ThreadKey, id: &str) {
        if let Some(terminal) = self.terminal_mut(key, id)
            && !terminal.edit.text.is_empty()
        {
            let text = terminal.edit.text.clone();
            terminal.edit.clear();
            terminal.type_input(text);
        }
    }

    /// A pointer gesture on terminal `id`: a press focuses it and starts a selection (1
    /// click: characters, 2: words, 3: lines), a drag extends it, and a plain click leaves
    /// none.
    pub(super) fn terminal_drag(&mut self, key: &ThreadKey, id: &str, drag: &Drag) {
        let cell = self.cell_metrics();
        if drag.phase == Phase::Down {
            self.select_terminal(key, id);
        }
        let Some(terminal) = self.terminal_mut(key, id) else {
            return;
        };
        let x = (drag.x - PADDING).max(0.0) / cell.width;
        let column = x as usize;
        let line = ((drag.y - PADDING).max(0.0) / cell.height) as usize;
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        match drag.phase {
            Phase::Down => terminal.screen.select(column, line, side, drag.clicks),
            Phase::Move => terminal.screen.extend(column, line, side),
            Phase::Up => terminal.screen.finish_selection(),
        }
    }

    /// The wheel over terminal `id`: the display scrolls through the scrollback; a
    /// full-screen program (alternate screen) gets Up/Down arrows instead unless Shift is
    /// held.
    pub(super) fn terminal_wheel(&mut self, key: &ThreadKey, id: &str, ev: &WheelEvent) -> bool {
        let line = self.cell_metrics().height;
        let Some(terminal) = self.terminal_mut(key, id) else {
            return false;
        };
        terminal.scroll_rest += ev.dy;
        let lines = (terminal.scroll_rest / line) as i32;
        if lines == 0 {
            return true;
        }
        terminal.scroll_rest -= line * lines as f32;
        let mode = terminal.screen.mode();
        if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && !ev.mods.shift {
            let key = if lines > 0 { 'A' } else { 'B' };
            let prefix = if mode.contains(TermMode::APP_CURSOR) {
                "\x1bO"
            } else {
                "\x1b["
            };
            terminal.send(format!("{prefix}{key}").repeat(lines.unsigned_abs() as usize));
        } else {
            terminal.screen.scroll(lines);
        }
        true
    }
}
