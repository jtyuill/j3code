//! The file tree: the visible rows built from the loaded listings (entries, then a note per
//! directory), keyboard movement over them, and the virtualised view.

use std::borrow::Cow;

use j3_client::methods::EntryKind;
use rune::{
    Color, El, GrowthSpec, Icon, Key, KeyEvent, ScrollSpec, TextStyle, Tx, col, el, row, text,
    text_input,
};
use rune_kit::{Kit, Spinner, TreeItem, TreeRow, TreeStyle};

use super::{Effect, FilePane};
use crate::{
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
    theme::Status,
};

/// A status line under a directory's entries.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Note {
    Loading,
    Empty,
    /// The listing stopped at this many entries.
    Truncated(usize),
    Failed(String),
}

impl Note {
    fn text(&self) -> Cow<'_, str> {
        match self {
            Note::Loading => "Loading…".into(),
            Note::Empty => "Empty".into(),
            Note::Truncated(count) => format!("Too many entries; showing the first {count}").into(),
            Note::Failed(error) => error.as_str().into(),
        }
    }
}

/// One visible line of the tree.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Row {
    Entry {
        /// The element key, `file-row-{path}`.
        key: String,
        path: String,
        depth: u32,
        directory: bool,
        /// Ignored by git, itself or through a parent.
        ignored: bool,
    },
    Note {
        /// The element key, `file-note-{dir}`.
        key: String,
        /// The directory the note is about.
        dir: String,
        depth: u32,
        note: Note,
    },
}

impl Row {
    fn key(&self) -> &str {
        match self {
            Row::Entry { key, .. } | Row::Note { key, .. } => key,
        }
    }

    fn entry(&self) -> Option<(&str, bool)> {
        match self {
            Row::Entry {
                path, directory, ..
            } => Some((path, *directory)),
            Row::Note { .. } => None,
        }
    }
}

/// The element key of `path`'s row.
pub(super) fn row_key(path: &str) -> String {
    format!("file-row-{path}")
}

impl FilePane {
    /// Rebuilds the visible rows from the listings and the expanded directories.
    pub(super) fn rebuild(&mut self) {
        let mut rows = Vec::with_capacity(self.rows.len());
        self.push_rows("", 0, false, &mut rows);
        self.rows = rows;
        self.rows_rev += 1;
    }

    fn push_rows(&self, dir: &str, depth: u32, ignored: bool, rows: &mut Vec<Row>) {
        let Some(listing) = self.listings.get(dir) else {
            return;
        };
        for entry in &listing.entries {
            let directory = entry.kind == EntryKind::Dir;
            let ignored = ignored || entry.ignored == Some(true);
            rows.push(Row::Entry {
                key: row_key(&entry.path),
                path: entry.path.clone(),
                depth,
                directory,
                ignored,
            });
            if directory && self.expanded.contains(&entry.path) {
                self.push_rows(&entry.path, depth + 1, ignored, rows);
            }
        }
        let note = if let Some(error) = &listing.error {
            Some(Note::Failed(error.clone()))
        } else if listing.entries.is_empty() {
            Some(if listing.loading {
                Note::Loading
            } else {
                Note::Empty
            })
        } else {
            listing
                .truncated
                .then_some(Note::Truncated(listing.entries.len()))
        };
        if let Some(note) = note {
            rows.push(Row::Note {
                key: format!("file-note-{dir}"),
                dir: dir.to_owned(),
                depth,
                note,
            });
        }
    }

    fn row_index(&self, path: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| row.entry().is_some_and(|(p, _)| p == path))
    }

    fn select_index(&mut self, ix: usize) {
        if let Some((path, _)) = self.rows.get(ix).and_then(Row::entry) {
            self.selected = Some(path.to_owned());
        }
    }

    /// Moves the selection `delta` entries, skipping notes.
    pub(super) fn move_selection(&mut self, delta: isize) {
        let entries: Vec<usize> = (0..self.rows.len())
            .filter(|ix| self.rows[*ix].entry().is_some())
            .collect();
        if entries.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_deref()
            .and_then(|path| self.row_index(path))
            .and_then(|ix| entries.iter().position(|entry| *entry == ix));
        let next = match current {
            Some(at) => (at as isize + delta).clamp(0, entries.len() as isize - 1) as usize,
            None if delta < 0 => entries.len() - 1,
            None => 0,
        };
        self.select_index(entries[next]);
    }

    /// The selected row's index, path and whether it is a directory.
    fn selected_entry(&self) -> Option<(usize, String, bool)> {
        let ix = self.row_index(self.selected.as_deref()?)?;
        let (path, directory) = self.rows[ix].entry()?;
        Some((ix, path.to_owned(), directory))
    }

    /// Expands or collapses directory `path`; a directory never listed starts loading.
    pub(super) fn toggle_dir(&mut self, path: &str) -> Option<Effect> {
        let mut effect = None;
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_owned());
            if !self.listings.contains_key(path) {
                effect = Some(self.begin_load(path));
            }
        }
        self.rebuild();
        effect
    }

    /// →: expands the selected directory, or enters an expanded one.
    fn expand_selected(&mut self) -> Option<Effect> {
        let (ix, path, directory) = self.selected_entry()?;
        if !directory {
            return None;
        }
        if self.expanded.contains(&path) {
            if matches!(self.rows.get(ix + 1), Some(Row::Entry { depth, .. }) if *depth > 0) {
                self.select_index(ix + 1);
            }
            None
        } else {
            self.toggle_dir(&path)
        }
    }

    /// ←: collapses the selected directory, or selects the parent.
    fn collapse_selected(&mut self) -> Option<Effect> {
        let (_, path, directory) = self.selected_entry()?;
        if directory && self.expanded.contains(&path) {
            return self.toggle_dir(&path);
        }
        if let Some((parent, _)) = path.rsplit_once('/')
            && let Some(ix) = self.row_index(parent)
        {
            self.select_index(ix);
        }
        None
    }

    /// A key while the tree has focus: whether it was taken and what it needs fetched.
    /// Unmodified typing is taken too, so nothing reaches the tree's focus field.
    pub(super) fn key(&mut self, ev: &KeyEvent) -> (bool, Option<Effect>) {
        let mods = ev.mods;
        if mods.cmd || mods.ctrl || mods.alt || mods.logo {
            return (false, None);
        }
        let effect = match &ev.key {
            Key::Up => {
                self.move_selection(-1);
                None
            }
            Key::Down => {
                self.move_selection(1);
                None
            }
            Key::Right => self.expand_selected(),
            Key::Left => self.collapse_selected(),
            Key::Enter => match self.selected_entry() {
                Some((_, path, directory)) => self.activate(&path, directory),
                None => {
                    self.move_selection(1);
                    None
                }
            },
            Key::Text(_) | Key::Backspace | Key::Delete | Key::Home | Key::End => None,
            _ => return (false, None),
        };
        (true, effect)
    }
}

/// The icon of a file named `name`.
pub(super) fn file_icon(name: &str) -> &'static Icon {
    let name = name.to_ascii_lowercase();
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match (name.as_str(), extension) {
        ("cargo.lock" | "flake.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock", _)
        | (_, "lock") => &icons::FILE_LOCK,
        (
            ".gitignore" | ".gitattributes" | ".editorconfig" | ".envrc" | "dockerfile"
            | "makefile",
            _,
        )
        | (_, "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" | "nix") => &icons::FILE_COG,
        (_, "json" | "jsonc") => &icons::FILE_BRACES,
        (_, "md" | "mdx" | "markdown" | "txt" | "rst") => &icons::FILE_TEXT,
        (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "avif") => {
            &icons::FILE_IMAGE
        }
        (_, "sh" | "bash" | "zsh" | "fish") => &icons::FILE_TERMINAL,
        (_, "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "7z") => &icons::FILE_ARCHIVE,
        (_, "pem" | "key" | "crt") => &icons::FILE_KEY,
        (
            _,
            "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "c" | "h" | "cc"
            | "cpp" | "hpp" | "java" | "kt" | "swift" | "rb" | "php" | "cs" | "zig" | "lua"
            | "html" | "css" | "scss" | "vue" | "svelte" | "sql",
        ) => &icons::FILE_CODE,
        _ => &icons::FILE,
    }
}

/// How the rows draw, from the theme, with the selection's fill showing whether the tree has
/// focus.
fn style(k: &Kit, focused: bool) -> TreeStyle {
    let mut style = TreeStyle::from_kit(k).label(k.ty.text_small, k.c.text_base, k.c.text_base);
    if !focused {
        style.selected = k.c.surface_hover;
    }
    style
}

/// The tree: an invisible focus field (the tree's keyboard) over the virtualised rows.
pub(super) fn view(s: &J3, pane: &FilePane, ui: &Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let style = style(k, pane.keys.focused);
    let row_key = key.clone();
    let build_key = key.clone();
    let kit = crate::theme::kit(k.dark, s.prefs.ui());
    let muted = Status::of(k).muted;
    let danger = Status::of(k).danger;
    let rows = col()
        .key("file-tree-rows")
        .w_full()
        .py(4.0)
        .growth(GrowthSpec::default())
        .virtual_rows(
            pane.rows.len(),
            style.row_height,
            move |s: &J3, i| {
                let row = s.files.get(&row_key).and_then(|pane| pane.rows.get(i));
                Cow::Borrowed(row.map_or("", Row::key))
            },
            move |s: &J3, i| {
                let Some(pane) = s.files.get(&build_key) else {
                    return el();
                };
                match pane.rows.get(i) {
                    Some(Row::Entry {
                        key,
                        path,
                        depth,
                        directory,
                        ignored,
                    }) => entry_row(
                        &kit,
                        &style,
                        &build_key,
                        pane,
                        (key, path, *depth, *directory, *ignored),
                    ),
                    Some(Row::Note {
                        key, depth, note, ..
                    }) => note_row(&kit, &style, key, *depth, note, muted, danger),
                    None => el(),
                }
            },
        )
        .virtual_rows_version(pane.rows_rev);
    let focus_key = key.clone();
    let press_key = key.clone();
    let focus = text_input(&pane.keys, TextStyle::new(1.0, 400.0))
        .key("file-tree-focus")
        .single_line()
        .label("Files")
        .absolute(0.0, 0.0)
        .size(1.0, 1.0)
        .color(Color::TRANSPARENT)
        .caret(Color::TRANSPARENT, Color::TRANSPARENT)
        .on_edit(move |s: &mut J3| &mut s.files.entry(focus_key.clone()).or_default().keys)
        .on_input_paste(|_: &mut J3, _, _| true)
        .on_input_key({
            let key = key.clone();
            move |s: &mut J3, ev: &KeyEvent, tx: &mut Tx| s.file_tree_key(&key, ev, tx)
        });
    col()
        .key("file-tree")
        .role("tree")
        .label("Files")
        .w_full()
        .min_h(0.0)
        .px(4.0)
        .on_press(move |s: &mut J3| {
            if let Some(pane) = s.files.get_mut(&press_key) {
                pane.keys.focused = true;
            }
        })
        .child(focus)
        .child(
            col()
                .key("file-tree-scroll")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(rows),
        )
}

/// One file or directory: indent guides, chevron, icon and name; aria = its path.
fn entry_row(
    k: &Kit,
    style: &TreeStyle,
    key: &ThreadKey,
    pane: &FilePane,
    (row_key, path, depth, directory, ignored): (&str, &str, u32, bool, bool),
) -> El {
    let name = path.rsplit('/').next().unwrap_or(path);
    let selected = pane.selected.as_deref() == Some(path);
    let expanded = directory && pane.expanded.contains(path);
    let item_icon: &'static Icon = match (directory, expanded) {
        (true, true) => &icons::FOLDER_OPEN,
        (true, false) => &icons::FOLDER,
        (false, _) => file_icon(name),
    };
    let mut item = TreeItem::new(name).icon(item_icon);
    if ignored {
        item = item.label_color(k.c.text_muted).icon_color(k.c.icon_faint);
    }
    let state = TreeRow::new(path, depth)
        .expanded(directory.then_some(expanded))
        .selected(selected);
    let (key, path) = (key.clone(), path.to_owned());
    item.build(k, &state, style)
        .key(row_key)
        .label(path.clone())
        .on_press(move |s: &mut J3| s.click_file(&key, &path, directory))
}

/// A directory's note: loading (with a spinner), empty, truncated, or its error.
fn note_row(
    k: &Kit,
    style: &TreeStyle,
    row_key: &str,
    depth: u32,
    note: &Note,
    muted: Color,
    danger: Color,
) -> El {
    let label = note.text().into_owned();
    let color = if matches!(note, Note::Failed(_)) {
        danger
    } else {
        muted
    };
    let mut line = row()
        .key(row_key)
        .label(label.clone())
        .w_full()
        .h(style.row_height)
        .pl(style.padding + depth as f32 * style.indent + 16.0 + 4.0)
        .pr(style.padding)
        .gap(6.0)
        .center_y();
    for level in 0..depth {
        let x = style.padding + level as f32 * style.indent + 7.5;
        line = line.child(
            el().key(format!("guide{level}"))
                .pin(Some(x), Some(0.0), None, Some(0.0))
                .w(1.0)
                .fill(style.guide)
                .a11y_hidden(),
        );
    }
    if *note == Note::Loading {
        line = line.child(Spinner::new().size(12.0).color(muted).build(k));
    }
    line.child(
        text(label)
            .font(k.ty.text_xsmall)
            .color(color)
            .min_w(0.0)
            .truncate()
            .a11y_hidden(),
    )
}
