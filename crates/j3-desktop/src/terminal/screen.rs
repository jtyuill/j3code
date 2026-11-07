//! One terminal's screen: an `alacritty_terminal::Term` and the VTE parser feeding it, with
//! the grid size, selection, scrolling and the requests a program makes of the terminal
//! (`PtyWrite`, `ColorRequest`, `TextAreaSizeRequest`). No I/O: the terminal feeds it bytes
//! from the server and writes what it asks back.

use std::{cell::RefCell, rc::Rc};

use alacritty_terminal::{
    Term,
    event::{Event as TermEvent, EventListener},
    grid::{Dimensions, Scroll},
    index::{Column, Point, Side},
    selection::{Selection, SelectionType},
    term::{Config, TermMode, cell::Flags, viewport_to_point},
    vte::ansi::Processor,
};

/// The grid size in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl GridSize {
    /// The size before the first layout.
    pub const INITIAL: GridSize = GridSize { cols: 80, rows: 24 };
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

/// What the terminal asks of its host: answers to write back to the shell.
#[derive(Clone, Default)]
pub struct Listener(Rc<RefCell<Vec<TermEvent>>>);

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

pub struct Screen {
    term: Term<Listener>,
    parser: Processor,
    listener: Listener,
    size: GridSize,
}

impl Screen {
    pub fn new(size: GridSize) -> Self {
        let listener = Listener::default();
        Self {
            term: Term::new(Config::default(), &size, listener.clone()),
            parser: Processor::new(),
            listener,
            size,
        }
    }

    pub fn term(&self) -> &Term<Listener> {
        &self.term
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// A blank screen of the same size (`terminal.clear`).
    pub fn clear(&mut self) {
        self.term = Term::new(Config::default(), &self.size, self.listener.clone());
        self.parser = Processor::new();
    }

    /// A fresh screen replaying `history`. Requests in old output were answered long ago, so
    /// they are dropped.
    pub fn reset(&mut self, history: &str) {
        self.clear();
        self.listener.0.borrow_mut().clear();
        self.parser.advance(&mut self.term, history.as_bytes());
        self.listener.0.borrow_mut().clear();
    }

    /// Feeds program output; returns what the program asked of the terminal.
    pub fn feed(&mut self, data: &[u8]) -> Vec<TermEvent> {
        self.parser.advance(&mut self.term, data);
        self.listener.0.borrow_mut().drain(..).collect()
    }

    /// Reflows the screen to `size`; false when it already was.
    pub fn resize(&mut self, size: GridSize) -> bool {
        if size == self.size {
            return false;
        }
        self.size = size;
        self.term.resize(size);
        true
    }

    /// The visible screen as text, one line per row, trailing blanks trimmed.
    pub fn text(&self) -> String {
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

    /// The grid point under viewport cell (`column`, `line`), clamped to the screen.
    fn point(&self, column: usize, line: usize) -> Point {
        let column = column.min(usize::from(self.size.cols) - 1);
        let line = line.min(usize::from(self.size.rows) - 1);
        viewport_to_point(
            self.term.grid().display_offset(),
            Point::new(line, Column(column)),
        )
    }

    /// Starts a selection at viewport cell (`column`, `line`): 1 click selects characters, 2
    /// words, 3 or more lines.
    pub fn select(&mut self, column: usize, line: usize, side: Side, clicks: u32) {
        let kind = match clicks {
            0 | 1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        let point = self.point(column, line);
        self.term.selection = Some(Selection::new(kind, point, side));
    }

    /// Moves the selection's end to viewport cell (`column`, `line`).
    pub fn extend(&mut self, column: usize, line: usize, side: Side) {
        let point = self.point(column, line);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    /// Drops an empty selection (a plain click).
    pub fn finish_selection(&mut self) {
        if self
            .term
            .selection
            .as_ref()
            .is_some_and(Selection::is_empty)
        {
            self.term.selection = None;
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    /// The selected text, if any.
    pub fn selected_text(&self) -> Option<String> {
        self.term
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    /// Scrolls the display `lines` into the scrollback (negative: back down).
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Back to the live screen.
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }
}

/// Splits `text` into pieces of at most `max` bytes on character boundaries.
pub fn chunks(text: &str, max: usize) -> impl Iterator<Item = &str> {
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

#[cfg(test)]
mod tests {
    use alacritty_terminal::{
        term::cell::Flags,
        vte::ansi::{Color, NamedColor},
    };

    use super::*;

    fn screen(cols: u16, rows: u16) -> Screen {
        Screen::new(GridSize { cols, rows })
    }

    #[test]
    fn output_lands_on_the_grid_with_its_colours() {
        let mut s = screen(20, 4);
        s.feed(b"plain \x1b[1;31mred\x1b[0m \x1b[38;5;196mx\x1b[48;2;1;2;3my\x1b[0m\r\n$ ");
        assert_eq!(s.text(), "plain red xy\n$");
        let grid = s.term().grid();
        let cell = |line: i32, col: usize| &grid[Point::new(line.into(), Column(col))];
        assert_eq!(cell(0, 6).fg, Color::Named(NamedColor::Red));
        assert!(cell(0, 6).flags.contains(Flags::BOLD));
        assert_eq!(cell(0, 0).fg, Color::Named(NamedColor::Foreground));
        assert_eq!(cell(0, 10).fg, Color::Indexed(196));
        assert!(matches!(cell(0, 11).bg, Color::Spec(rgb) if (rgb.r, rgb.g, rgb.b) == (1, 2, 3)));
        let cursor = s.term().grid().cursor.point;
        assert_eq!((cursor.line.0, cursor.column.0), (1, 2));
    }

    #[test]
    fn long_lines_wrap_and_reflow_on_resize() {
        let mut s = screen(10, 3);
        s.feed(b"0123456789abcde");
        assert_eq!(s.text(), "0123456789\nabcde");
        assert!(s.resize(GridSize { cols: 20, rows: 3 }));
        assert_eq!(s.text(), "0123456789abcde");
        assert!(!s.resize(GridSize { cols: 20, rows: 3 }));
    }

    #[test]
    fn a_snapshot_replays_history_without_answering_old_queries() {
        let mut s = screen(20, 3);
        s.feed(b"old");
        // A cursor position report request in the history is not answered again.
        s.reset("hello\x1b[6n\r\nworld");
        assert_eq!(s.text(), "hello\nworld");
        let requests = s.feed(b"\x1b[6n");
        assert!(
            matches!(requests.as_slice(), [TermEvent::PtyWrite(answer)] if answer == "\x1b[2;6R")
        );
        s.clear();
        assert_eq!(s.text(), "");
    }

    #[test]
    fn programs_ask_for_colours_and_the_cell_size() {
        let mut s = screen(20, 3);
        let requests = s.feed(b"\x1b]11;?\x07\x1b[14t");
        assert!(matches!(requests[0], TermEvent::ColorRequest(257, _)));
        assert!(matches!(requests[1], TermEvent::TextAreaSizeRequest(_)));
    }

    #[test]
    fn clicks_select_characters_words_and_lines() {
        let mut s = screen(20, 3);
        s.feed(b"one two three\r\nfour");
        s.select(4, 0, Side::Left, 1);
        s.extend(6, 0, Side::Right);
        assert_eq!(s.selected_text().as_deref(), Some("two"));
        s.select(9, 0, Side::Left, 2);
        assert_eq!(s.selected_text().as_deref(), Some("three"));
        s.select(1, 1, Side::Left, 3);
        assert_eq!(s.selected_text().as_deref(), Some("four\n"));
        s.select(1, 1, Side::Left, 1);
        s.finish_selection();
        assert_eq!(s.selected_text(), None);
        assert!(s.term().selection.is_none());
    }

    #[test]
    fn the_display_scrolls_into_history() {
        let mut s = screen(10, 2);
        s.feed(b"a\r\nb\r\nc\r\nd");
        assert_eq!(s.text(), "c\nd");
        s.scroll(2);
        assert_eq!(s.text(), "a\nb");
        s.scroll_to_bottom();
        assert_eq!(s.text(), "c\nd");
    }

    #[test]
    fn writes_split_on_character_boundaries() {
        let text = "aé🦀b";
        let pieces: Vec<&str> = chunks(text, 3).collect();
        assert_eq!(pieces.concat(), text);
        assert!(pieces.iter().all(|piece| piece.len() <= 4));
        assert_eq!(chunks("", 3).count(), 0);
        let long = "x".repeat(70_000);
        let sizes: Vec<usize> = chunks(&long, 65_536).map(str::len).collect();
        assert_eq!(sizes, [65_536, 4_464]);
    }
}
