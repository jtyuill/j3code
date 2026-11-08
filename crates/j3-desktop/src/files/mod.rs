//! The project file pane (spec §5.4, §8.5): the thread's project as it is on the thread's
//! server host, beside the chat. A tree loaded one directory at a time
//! (`connection.list_entries`) over a read-only, syntax-coloured preview of the opened file
//! (`connection.read_file`). ⌘⇧E toggles it; with the tree focused ↑/↓ move, → expands (or
//! enters) a directory, ← collapses it (or goes to the parent) and Enter opens.
//!
//! The pane's tree and preview logic lives on [`FilePane`] and returns [`Effect`]s (a listing
//! or a file to fetch) that the `J3` commands here run on the thread's connection, so the
//! keyboard and expansion rules are testable without a server.

pub mod fixtures;
mod preview;
#[cfg(test)]
mod tests;
mod tree;

use std::collections::{HashMap, HashSet};

use j3_client::{
    Error,
    t3::project::{ProjectEntry, ProjectListEntriesResult},
};
use rune::{Cursor, Cx, Drag, El, Phase, TextEdit, Transition, Tx, col, el, row, text};
use rune_kit::{IconButton, IconButtonVariant, Spinner};

use crate::{
    actions::{Action, shortcut_label},
    app::{J3, Ui},
    icons,
    sidebar::ThreadKey,
};

/// The pane's default width, its range (rem) and the tree's height above an open preview.
const WIDTH_REM: f32 = 22.5;
const WIDTH_RANGE_REM: (f32, f32) = (15.0, 60.0);
const TREE_REM: f32 = 14.0;
const TREE_RANGE_REM: (f32, f32) = (4.5, 60.0);

/// One directory's listing.
#[derive(Default)]
struct Listing {
    entries: Vec<ProjectEntry>,
    truncated: bool,
    /// A request is in flight; `entries` are the previous answer's.
    loading: bool,
    error: Option<String>,
    /// The request whose answer counts; older answers are dropped.
    request: u64,
}

/// Something the pane needs from the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Effect {
    /// List directory `dir` (`""` is the project root) as request `request`.
    Load { dir: String, request: u64 },
    /// Read file `path` as request `request`.
    Read { path: String, request: u64 },
}

/// One thread's file pane.
pub struct FilePane {
    pub open: bool,
    /// The pane's width in points (22.5 rem by default, 15–60 rem).
    pub width: f32,
    /// The tree's height in points while a preview shows under it (14 rem, 4.5–60 rem).
    tree_height: f32,
    /// The pane was opened once: its project is resolved and the root listed.
    started: bool,
    /// The project's directory on the server host.
    cwd: String,
    project: String,
    /// Listings by directory path, `""` for the project root.
    listings: HashMap<String, Listing>,
    expanded: HashSet<String>,
    /// The visible tree, rebuilt when listings or expansion change.
    rows: Vec<tree::Row>,
    /// Bumped with every rebuild (the virtual rows' key version).
    rows_rev: u64,
    selected: Option<String>,
    preview: Option<preview::Preview>,
    /// The tree's keyboard focus: an invisible field, focused by setting `focused`.
    keys: TextEdit,
    next_request: u64,
}

impl Default for FilePane {
    fn default() -> Self {
        Self {
            open: false,
            width: WIDTH_REM * 16.0,
            tree_height: TREE_REM * 16.0,
            started: false,
            cwd: String::new(),
            project: String::new(),
            listings: HashMap::new(),
            expanded: HashSet::new(),
            rows: Vec::new(),
            rows_rev: 0,
            selected: None,
            preview: None,
            keys: TextEdit::new(""),
            next_request: 0,
        }
    }
}

impl FilePane {
    /// A pane over `cwd` (titled `project`) that has not listed anything yet, sized for an
    /// interface font of `rem` points.
    fn new(cwd: String, project: String, rem: f32) -> Self {
        Self {
            width: WIDTH_REM * rem,
            tree_height: TREE_REM * rem,
            started: true,
            cwd,
            project,
            ..Self::default()
        }
    }

    fn request(&mut self) -> u64 {
        self.next_request += 1;
        self.next_request
    }

    /// Marks `dir` as loading and returns the request that lists it.
    fn begin_load(&mut self, dir: &str) -> Effect {
        let request = self.request();
        let listing = self.listings.entry(dir.to_owned()).or_default();
        listing.loading = true;
        listing.request = request;
        self.rebuild();
        Effect::Load {
            dir: dir.to_owned(),
            request,
        }
    }

    /// The answer to listing `dir` as `request`.
    fn listed(&mut self, dir: &str, request: u64, result: Result<ProjectListEntriesResult, Error>) {
        let Some(listing) = self
            .listings
            .get_mut(dir)
            .filter(|listing| listing.request == request)
        else {
            return;
        };
        listing.loading = false;
        match result {
            Ok(result) => {
                listing.entries = result.entries;
                listing.truncated = result.truncated;
                listing.error = None;
            }
            Err(error) => listing.error = Some(error.to_string()),
        }
        self.rebuild();
    }

    /// Lists the expanded directories again (dropping collapsed ones' listings) and rereads
    /// the open file.
    fn refresh(&mut self) -> Vec<Effect> {
        let expanded = &self.expanded;
        self.listings
            .retain(|dir, _| dir.is_empty() || expanded.contains(dir));
        let mut dirs: Vec<String> = self.listings.keys().cloned().collect();
        dirs.sort();
        let mut effects: Vec<Effect> = dirs.iter().map(|dir| self.begin_load(dir)).collect();
        if let Some(path) = self.preview.as_ref().map(|preview| preview.path.clone()) {
            effects.push(self.begin_read(path));
        }
        effects
    }

    /// Something is in flight: the header shows a spinner instead of the refresh button.
    fn loading(&self) -> bool {
        self.listings.values().any(|listing| listing.loading)
            || self.preview.as_ref().is_some_and(preview::Preview::loading)
    }

    /// Starts reading `path` into the preview.
    fn begin_read(&mut self, path: String) -> Effect {
        let request = self.request();
        self.preview = Some(preview::Preview::loading_for(path.clone(), request));
        Effect::Read { path, request }
    }

    /// Enter or a click: toggles a directory, opens a file not already open.
    fn activate(&mut self, path: &str, directory: bool) -> Option<Effect> {
        self.selected = Some(path.to_owned());
        if directory {
            self.toggle_dir(path)
        } else if self
            .preview
            .as_ref()
            .is_none_or(|preview| preview.path != path)
        {
            Some(self.begin_read(path.to_owned()))
        } else {
            None
        }
    }
}

impl J3 {
    /// Shows or hides `key`'s file pane. The first open resolves the thread's project and
    /// lists its root; opening focuses the tree, hiding refocuses the composer.
    /// Lands at once (see `crate::motion`).
    pub fn toggle_files(&mut self, key: ThreadKey, tx: &mut Tx) {
        tx.instant();
        if self.files.get(&key).is_some_and(|pane| pane.open) {
            if let Some(pane) = self.files.get_mut(&key) {
                pane.open = false;
            }
            self.focus_composer(tx);
            return;
        }
        if !self.files.get(&key).is_some_and(|pane| pane.started) {
            let (cwd, project) = self
                .envs
                .get(&key.env)
                .and_then(|env| {
                    let shell = &env.state().shell;
                    let thread = shell.thread(&key.thread_id)?;
                    let project = shell.project(&thread.project_id)?;
                    Some((project.workspace_root.clone(), project.title.clone()))
                })
                .unwrap_or_default();
            let mut pane = FilePane::new(cwd, project, self.prefs.ui().interface_font_size());
            let effect = pane.begin_load("");
            self.files.insert(key.clone(), pane);
            self.run_file_effects(&key, [effect]);
        }
        if let Some(pane) = self.files.get_mut(&key) {
            pane.open = true;
            pane.keys.focused = true;
        }
    }

    /// Fetches what `effects` ask for on `key`'s connection; each answer lands in its pane.
    fn run_file_effects(&mut self, key: &ThreadKey, effects: impl IntoIterator<Item = Effect>) {
        let (Some(connection), Some(pane)) = (
            self.envs.get(&key.env).map(|env| env.connection().clone()),
            self.files.get(key),
        ) else {
            return;
        };
        let cwd = pane.cwd.clone();
        for effect in effects {
            let (connection, cwd, key) = (connection.clone(), cwd.clone(), key.clone());
            match effect {
                Effect::Load { dir, request } => self.spawn(async move {
                    let result = connection.list_entries(&cwd, &dir).await;
                    move |s: &mut J3, _: &mut Tx| {
                        if let Some(pane) = s.files.get_mut(&key) {
                            pane.listed(&dir, request, result);
                        }
                    }
                }),
                Effect::Read { path, request } => self.spawn(async move {
                    let result = connection.read_file(&cwd, &path).await;
                    let body = preview::Body::read(&path, result);
                    move |s: &mut J3, _: &mut Tx| {
                        if let Some(pane) = s.files.get_mut(&key) {
                            pane.file_read(&path, request, body);
                        }
                    }
                }),
            }
        }
    }

    /// The header's refresh button.
    fn refresh_files(&mut self, key: &ThreadKey) {
        if let Some(pane) = self.files.get_mut(key) {
            let effects = pane.refresh();
            self.run_file_effects(key, effects);
        }
    }

    /// A click on a tree row: focuses the tree and activates the row.
    fn click_file(&mut self, key: &ThreadKey, path: &str, directory: bool) {
        let Some(pane) = self.files.get_mut(key) else {
            return;
        };
        pane.keys.focused = true;
        let effect = pane.activate(path, directory);
        self.run_file_effects(key, effect);
    }

    /// A key while the tree has focus; true when the tree took it.
    fn file_tree_key(&mut self, key: &ThreadKey, ev: &rune::KeyEvent, tx: &mut Tx) -> bool {
        let Some(pane) = self.files.get_mut(key) else {
            return false;
        };
        let (taken, effect) = pane.key(ev);
        if let Some(path) = pane.selected.as_deref().filter(|_| taken) {
            tx.scroll_into_view_in("file-tree", tree::row_key(path));
        }
        self.run_file_effects(key, effect);
        taken
    }
}

/// `key`'s file pane while it is open: header, tree, and the preview under the tree.
pub fn view(s: &J3, _cx: &mut Cx<J3>, ui: &mut Ui, key: &ThreadKey) -> El {
    let Some(pane) = s.files.get(key).filter(|pane| pane.open) else {
        return col();
    };
    let k = ui.k;
    let rem = ui.m.rem;
    let tree = tree::view(s, pane, ui, key);
    let body = match &pane.preview {
        None => tree.grow(1.0),
        Some(preview) => col()
            .key("file-pane-split")
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .child(tree.h(pane.tree_height).shrink())
            .child(
                preview::view(s, preview, ui, key)
                    .grow(1.0)
                    .min_h(0.0)
                    .border_sides([1.0, 0.0, 0.0, 0.0], k.c.border_muted),
            )
            .child(
                grip(
                    key,
                    "file-pane-split-grip",
                    Cursor::ResizeRow,
                    rem,
                    TREE_RANGE_REM,
                    |pane| &mut pane.tree_height,
                    |d| d.dy,
                )
                .pin(
                    Some(0.0),
                    Some(pane.tree_height - GRIP / 2.0),
                    Some(0.0),
                    None,
                )
                .h(GRIP),
            ),
    };
    // Browsing files is frequent: the pane comes and goes, and its tree and preview change,
    // in the frame they happen (`crate::motion`).
    col()
        .key("file-pane")
        .w(pane.width)
        .h_full()
        .min_h(0.0)
        .instant_changes()
        .transition(Transition::none())
        .fill(k.c.bg_base)
        .border_sides([0.0, 0.0, 0.0, 1.0], k.c.border_muted)
        .child(header(pane, ui, key))
        .child(body)
        .child(
            grip(
                key,
                "thread-files-split",
                Cursor::ResizeColumn,
                rem,
                WIDTH_RANGE_REM,
                |pane| &mut pane.width,
                |d| -d.dx,
            )
            .pin(Some(-GRIP / 2.0), Some(0.0), None, Some(0.0))
            .w(GRIP),
        )
}

/// The grab width of a resize grip, centred on the hairline it moves.
const GRIP: f32 = 6.0;

/// An invisible strip that resizes a part of the pane by dragging: `size` picks the length,
/// `delta` the drag's travel that grows it; it stays within `range` rem.
fn grip(
    key: &ThreadKey,
    name: &str,
    cursor: Cursor,
    rem: f32,
    range: (f32, f32),
    size: fn(&mut FilePane) -> &mut f32,
    delta: fn(&Drag) -> f32,
) -> El {
    let key = key.clone();
    let line = el()
        .key(name)
        .role("separator")
        .label(if cursor == Cursor::ResizeRow {
            "Resize the file tree"
        } else {
            "Resize the file pane"
        })
        .cursor(cursor);
    line.on_drag(move |s: &mut J3, d: &mut Drag, tx: &mut Tx| {
        let Some(pane) = s.files.get_mut(&key) else {
            return;
        };
        let value = size(pane);
        match d.phase {
            Phase::Down => d.data = *value,
            Phase::Move => {
                tx.instant();
                *value = (d.data + delta(d)).clamp(range.0 * rem, range.1 * rem);
            }
            Phase::Up if d.cancelled() => *value = d.data,
            Phase::Up => {}
        }
    })
}

/// FolderTree icon, the project's title, refresh (or a spinner while loading) and close.
fn header(pane: &FilePane, ui: &Ui, key: &ThreadKey) -> El {
    let k = ui.k;
    let refresh = if pane.loading() {
        el().key("files-loading")
            .size(24.0, 24.0)
            .center()
            .child(Spinner::new().size(14.0).build(k))
    } else {
        let key = key.clone();
        IconButton::new(&icons::REFRESH_CW)
            .variant(IconButtonVariant::GhostMuted)
            .control_size(rune_kit::ControlSize::Small)
            .label("Refresh files")
            .on_press(move |s: &mut J3| s.refresh_files(&key))
            .build(k)
            .key("refresh-files")
    };
    let close = {
        let key = key.clone();
        IconButton::new(&icons::X)
            .variant(IconButtonVariant::GhostMuted)
            .control_size(rune_kit::ControlSize::Small)
            .label("Close files")
            .help(format!(
                "Close files ({})",
                shortcut_label(Action::ToggleFiles)
            ))
            .on_press_tx(move |s: &mut J3, tx: &mut Tx| s.toggle_files(key.clone(), tx))
            .build(k)
            .key("close-files")
    };
    row()
        .key("file-pane-header")
        .w_full()
        .h(ui.m.row())
        .px(12.0)
        .gap(4.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(rune::icon(&icons::FOLDER_TREE, 16.0).color(k.c.icon_muted))
        .child(
            text(pane.project.clone())
                .key("file-pane-title")
                .font(k.ty.text_small_medium)
                .color(k.c.text_base)
                .grow(1.0)
                .min_w(0.0)
                .truncate(),
        )
        .child(refresh)
        .child(close)
}
