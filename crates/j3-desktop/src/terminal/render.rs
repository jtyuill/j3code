//! Drawing a terminal's grid: one element per screen row (memoised on what the row shows),
//! holding its merged cell backgrounds and its text as one-line styled runs in the mono font at the
//! terminal font size, placed at their cells. Characters whose advance isn't the cell width
//! (wide CJK, glyphs from fallback fonts) start runs of their own at their cell, so the rest
//! of the row stays on the grid. The cursor is drawn into its cell (focused block) or over
//! it (beam, underline, hollow block when unfocused).

use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

use alacritty_terminal::{
    term::{TermMode, cell::Flags, point_to_viewport},
    vte::ansi::{Color as AnsiColor, CursorShape, NamedColor},
};
use rune::{Color, El, Span, TextStyle, el, text};

use super::{
    CellMetrics, PADDING, Terminal,
    palette::{Palette, dimmed},
};

/// The text style of one cell.
#[derive(Debug, Clone, Copy, PartialEq, Hash)]
struct Style {
    color: Color,
    bold: bool,
    italic: bool,
    underline: bool,
}

/// Neighbouring cells of a row drawn as one text, starting at `column`.
#[derive(Hash)]
struct Run {
    column: usize,
    /// Cells it covers.
    cells: usize,
    text: String,
    /// Style changes: byte offset in `text` and the style from there.
    styles: Vec<(usize, Style)>,
    /// Whether more cells may join (false after a character that isn't one cell wide).
    open: bool,
}

/// What one screen row shows.
#[derive(Hash, Default)]
struct Row {
    /// Merged backgrounds: first column, cells, colour.
    backgrounds: Vec<(usize, usize, Color)>,
    runs: Vec<Run>,
}

impl Row {
    fn background(&mut self, column: usize, cells: usize, color: Color) {
        match self.backgrounds.last_mut() {
            Some((start, len, last)) if *last == color && *start + *len == column => *len += cells,
            _ => self.backgrounds.push((column, cells, color)),
        }
    }

    /// Adds the text of the `cells` cells at `column`. `fits`: it advances exactly that
    /// many cells, so neighbours may share its run; one that doesn't gets a run of its own.
    /// A blank, unstyled cell joins an open run but never starts one.
    fn text(&mut self, column: usize, cells: usize, text: &str, style: Style, fits: bool) {
        let blank = text == " " && !style.underline;
        if !fits && let Some(last) = self.runs.last_mut() {
            last.open = false;
        }
        let joins = self
            .runs
            .last()
            .is_some_and(|run| run.open && run.column + run.cells == column);
        if !joins {
            if blank {
                return;
            }
            self.runs.push(Run {
                column,
                cells: 0,
                text: String::new(),
                styles: Vec::new(),
                open: true,
            });
        }
        let run = self.runs.last_mut().expect("a run was just ensured");
        if run.styles.last().is_none_or(|(_, last)| *last != style) {
            run.styles.push((run.text.len(), style));
        }
        run.text.push_str(text);
        run.cells += cells;
        run.open = fits;
    }

    /// Drops trailing blanks (they draw nothing) and empty runs.
    fn trim(&mut self) {
        for run in &mut self.runs {
            let underlined_tail = run.styles.last().is_some_and(|(_, s)| s.underline);
            if !underlined_tail {
                let len = run.text.trim_end_matches(' ').len();
                run.text.truncate(len);
                run.styles.retain(|(at, _)| *at < len);
            }
        }
        self.runs.retain(|run| !run.text.is_empty());
    }

    fn element(self, line: usize, cols: usize, cell: CellMetrics, fallback: Color) -> El {
        let mut row = el()
            .absolute(PADDING, PADDING + line as f32 * cell.height)
            .size(cols as f32 * cell.width, cell.height);
        for (column, cells, color) in self.backgrounds {
            row = row.child(
                el().absolute(column as f32 * cell.width, 0.0)
                    .size(cells as f32 * cell.width, cell.height)
                    .fill(color),
            );
        }
        for run in self.runs {
            let len = run.text.len();
            let mut spans = Vec::with_capacity(run.styles.len());
            for (i, (start, style)) in run.styles.iter().enumerate() {
                let end = run.styles.get(i + 1).map_or(len, |(next, _)| *next);
                let mut span = Span::new(*start, end).color(style.color);
                if style.bold {
                    span = span.bold();
                }
                if style.italic {
                    span = span.italic();
                }
                if style.underline {
                    span = span.underline(style.color);
                }
                spans.push(span);
            }
            row = row.child(
                text(run.text)
                    .spans(spans)
                    .font(cell.font)
                    .color(fallback)
                    .absolute(run.column as f32 * cell.width, 0.0),
            );
        }
        row
    }
}

/// Whether characters advance exactly one cell, by character and font size, for one font
/// epoch.
#[derive(Default)]
struct Fits {
    epoch: u64,
    known: HashMap<(char, u32), bool>,
}

/// Most characters [`fits_cell`] remembers before starting over.
const FITS_LIMIT: usize = 4096;

/// Whether `c` advances exactly one cell in `font` (every ASCII glyph of a monospace font
/// does; fallback glyphs often don't). Remembered per character and font size.
fn fits_cell(c: char, font: TextStyle, width: f32) -> bool {
    if c.is_ascii() {
        return true;
    }
    thread_local! {
        static FITS: RefCell<Fits> = RefCell::default();
    }
    FITS.with_borrow_mut(|fits| {
        let epoch = rune::text::font_epoch();
        if fits.epoch != epoch || fits.known.len() >= FITS_LIMIT {
            fits.epoch = epoch;
            fits.known.clear();
        }
        *fits
            .known
            .entry((c, font.size.to_bits()))
            .or_insert_with(|| {
                (rune::text::advance(c.encode_utf8(&mut [0; 4]), font) - width).abs() < 0.01
            })
    })
}

/// Terminal `t`'s cursor cell (column, line) and shape, when it shows on screen.
pub fn cursor_at(t: &Terminal) -> Option<(usize, usize, CursorShape)> {
    let content = t.screen.term().renderable_content();
    let shape = content.cursor.shape;
    (content.mode.contains(TermMode::SHOW_CURSOR) && shape != CursorShape::Hidden)
        .then(|| point_to_viewport(content.display_offset, content.cursor.point))
        .flatten()
        .filter(|p| p.line < usize::from(t.screen.size().rows))
        .map(|p| (p.column.0, p.line, shape))
}

/// Where the cursor is and how it draws.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CursorCell {
    column: usize,
    line: usize,
    /// 2 on a wide character.
    cells: usize,
    shape: CursorShape,
}

/// What a frame of terminal `t` shows: its rows and its cursor. A focused block cursor is
/// painted into its cell (inverted); the others are overlays.
fn model(
    t: &Terminal,
    palette: &Palette,
    cell: CellMetrics,
    focused: bool,
) -> (Vec<Row>, Option<CursorCell>) {
    let term = t.screen.term();
    let rows = usize::from(t.screen.size().rows);
    let mut cursor = cursor_at(t).map(|(column, line, shape)| CursorCell {
        column,
        line,
        cells: 1,
        shape,
    });
    let content = term.renderable_content();
    let colors = content.colors;
    let offset = content.display_offset;
    let block = focused && cursor.is_some_and(|c| c.shape == CursorShape::Block);
    let mut models: Vec<Row> = (0..rows).map(|_| Row::default()).collect();
    let selection = content.selection;
    for indexed in content.display_iter {
        let flags = indexed.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let Some(at) = point_to_viewport(offset, indexed.point) else {
            continue;
        };
        let Some(row) = models.get_mut(at.line) else {
            continue;
        };
        let column = at.column.0;
        let cells = if flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        let mut fg = palette.resolve(colors, indexed.fg);
        let mut bg = match indexed.bg {
            AnsiColor::Named(NamedColor::Background) => None,
            color => Some(palette.resolve(colors, color)),
        };
        if flags.contains(Flags::INVERSE) {
            let back = bg.unwrap_or(palette.background);
            bg = Some(fg);
            fg = back;
        }
        if flags.contains(Flags::DIM) {
            fg = dimmed(fg);
        }
        if selection.is_some_and(|s| s.contains(indexed.point)) {
            bg = Some(palette.selection);
        }
        if let Some(cursor) = cursor
            .as_mut()
            .filter(|c| (c.column, c.line) == (column, at.line))
        {
            cursor.cells = cells;
            if block {
                bg = Some(palette.cursor);
                fg = palette.background;
            }
        }
        if let Some(bg) = bg {
            row.background(column, cells, bg);
        }
        let mut text = String::new();
        if flags.contains(Flags::HIDDEN) {
            text.push(' ');
        } else {
            text.push(indexed.c);
            if let Some(extra) = indexed.zerowidth() {
                text.extend(extra);
            }
        }
        let style = Style {
            color: fg,
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline: flags.intersects(Flags::ALL_UNDERLINES),
        };
        let fits = cells == 1 && fits_cell(indexed.c, cell.font, cell.width);
        row.text(column, cells, &text, style, fits);
    }
    for row in &mut models {
        row.trim();
    }
    (models, cursor)
}

/// Terminal `t`'s grid layer: its rows (each memoised under `terminal-row-{id}-{line}`) and
/// a non-block cursor, built again only when the screen, focus, colours or cell size changed.
pub fn grid(t: &Terminal, palette: &Palette, cell: CellMetrics, focused: bool) -> El {
    let mut hasher = DefaultHasher::new();
    let metrics = (
        cell.width.to_bits(),
        cell.height.to_bits(),
        cell.font.size.to_bits(),
    );
    (t.screen.revision(), focused, palette, metrics).hash(&mut hasher);
    rune::memo::memo_with_hash(format!("terminal-grid-{}", t.id), hasher.finish(), || {
        let (rows, cursor) = grid_parts(t, palette, cell, focused);
        el().key(format!("terminal-screen-{}", t.id))
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .children(rows)
            .children(cursor)
    })
}

fn grid_parts(
    t: &Terminal,
    palette: &Palette,
    cell: CellMetrics,
    focused: bool,
) -> (Vec<El>, Option<El>) {
    let cols = usize::from(t.screen.size().cols);
    let (models, cursor) = model(t, palette, cell, focused);
    let fallback = palette.foreground;
    let elements = models
        .into_iter()
        .enumerate()
        .filter(|(_, model)| !(model.backgrounds.is_empty() && model.runs.is_empty()))
        .map(|(line, model)| {
            let mut hasher = DefaultHasher::new();
            let metrics = (
                cell.width.to_bits(),
                cell.height.to_bits(),
                cell.font.size.to_bits(),
            );
            (&model, cols, metrics, fallback).hash(&mut hasher);
            rune::memo::memo_with_hash(
                format!("terminal-row-{}-{line}", t.id),
                hasher.finish(),
                move || model.element(line, cols, cell, fallback),
            )
        })
        .collect();
    let overlay = cursor.and_then(|c| {
        let x = PADDING + c.column as f32 * cell.width;
        let y = PADDING + c.line as f32 * cell.height;
        let width = c.cells as f32 * cell.width;
        let mark = || el().key(format!("terminal-cursor-{}", t.id));
        match c.shape {
            CursorShape::Block if focused => None,
            CursorShape::Beam if focused => Some(
                mark()
                    .absolute(x, y)
                    .size(2.0, cell.height)
                    .fill(palette.cursor),
            ),
            CursorShape::Underline if focused => Some(
                mark()
                    .absolute(x, y + cell.height - 2.0)
                    .size(width, 2.0)
                    .fill(palette.cursor),
            ),
            _ => Some(
                mark()
                    .absolute(x, y)
                    .size(width, cell.height)
                    .border(1.0, palette.cursor),
            ),
        }
    });
    (elements, overlay)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(color: Color) -> Style {
        Style {
            color,
            bold: false,
            italic: false,
            underline: false,
        }
    }

    #[test]
    fn rows_merge_styles_and_backgrounds_and_skip_blanks() {
        let (a, b) = (Color::WHITE, Color::BLACK);
        let mut row = Row::default();
        for (column, c) in "  ab cd  ".chars().enumerate() {
            let color = if column < 4 { a } else { b };
            row.text(column, 1, &c.to_string(), style(color), true);
        }
        row.background(2, 1, a);
        row.background(3, 2, a);
        row.background(6, 1, b);
        row.trim();
        assert_eq!(row.backgrounds, [(2, 3, a), (6, 1, b)]);
        assert_eq!(row.runs.len(), 1);
        let run = &row.runs[0];
        assert_eq!((run.column, run.text.as_str()), (2, "ab cd"));
        assert_eq!(run.styles, [(0, style(a)), (2, style(b))]);
    }

    #[test]
    fn a_wide_character_is_a_run_of_its_own() {
        let mut row = Row::default();
        let s = style(Color::WHITE);
        row.text(0, 1, "a", s, true);
        row.text(1, 2, "漢", s, false);
        row.text(3, 1, "b", s, true);
        row.text(4, 1, "c", s, true);
        let runs: Vec<(usize, &str)> = row
            .runs
            .iter()
            .map(|r| (r.column, r.text.as_str()))
            .collect();
        assert_eq!(runs, [(0, "a"), (1, "漢"), (3, "bc")]);
    }

    fn fed(bytes: &str, focused: bool) -> (Palette, Vec<Row>, Option<CursorCell>) {
        let mut t = Terminal::new("term-1".into(), tokio::sync::mpsc::unbounded_channel().0);
        t.screen
            .resize(super::super::GridSize { cols: 12, rows: 3 });
        t.screen.feed(bytes.as_bytes());
        let palette = Palette::new(rune_theme_opencode::kit(true));
        let (rows, cursor) = model(&t, &palette, CellMetrics::new(12.0), focused);
        (palette, rows, cursor)
    }

    #[test]
    fn colours_bold_and_inverse_reach_the_runs() {
        let (p, rows, _) = fed(
            "\x1b[1;31mred\x1b[0m \x1b[7minv\x1b[0m\x1b[38;5;21mb",
            false,
        );
        let run = &rows[0].runs[0];
        assert_eq!(run.text, "red invb");
        let red = Style {
            bold: true,
            ..style(p.entry(1))
        };
        assert_eq!(
            run.styles,
            [
                (0, red),
                (3, style(p.foreground)),
                (4, style(p.background)),
                (7, style(p.entry(21)))
            ]
        );
        // Inverse paints the foreground behind its cells.
        assert_eq!(rows[0].backgrounds, [(4, 3, p.foreground)]);
    }

    #[test]
    fn a_wrapped_line_reflows_into_one_run_on_a_wider_screen() {
        let mut t = Terminal::new("term-1".into(), tokio::sync::mpsc::unbounded_channel().0);
        t.screen.feed(super::super::fixture::OUTPUT.as_bytes());
        t.screen.resize(super::super::GridSize {
            cols: 150,
            rows: 12,
        });
        let palette = Palette::new(rune_theme_opencode::kit(true));
        let (rows, _) = model(&t, &palette, CellMetrics::new(12.0), true);
        let run = &rows[1].runs[0];
        assert_eq!(rows[1].runs.len(), 1);
        assert_eq!(run.column, 0);
        assert!(
            run.text.ends_with("scripts  release.tar.gz"),
            "{}",
            run.text
        );
        // A leading blank doesn't start a run: ` M file` starts at the M.
        assert_eq!(rows[4].runs[0].column, 1);
    }

    #[test]
    fn long_lines_wrap_onto_the_next_row() {
        let (_, rows, cursor) = fed("abcdefghijklmnop", false);
        assert_eq!(rows[0].runs[0].text, "abcdefghijkl");
        assert_eq!(
            (rows[1].runs[0].column, rows[1].runs[0].text.as_str()),
            (0, "mnop")
        );
        let cursor = cursor.unwrap();
        assert_eq!((cursor.column, cursor.line), (4, 1));
    }

    #[test]
    fn a_focused_block_cursor_inverts_its_cell() {
        let (p, rows, cursor) = fed("ab\x1b[D", true);
        assert_eq!(
            cursor.map(|c| (c.column, c.line, c.shape)),
            Some((1, 0, CursorShape::Block))
        );
        assert_eq!(rows[0].backgrounds, [(1, 1, p.cursor)]);
        assert_eq!(rows[0].runs[0].styles[1], (1, style(p.background)));
        // Unfocused, the cell keeps its colours (the view outlines it instead).
        let (_, rows, _) = fed("ab\x1b[D", false);
        assert!(rows[0].backgrounds.is_empty());
        // A beam (DECSCUSR 6) never inverts.
        let (_, rows, cursor) = fed("\x1b[6 qab", true);
        assert_eq!(cursor.map(|c| c.shape), Some(CursorShape::Beam));
        assert!(rows[0].backgrounds.is_empty());
    }
}
