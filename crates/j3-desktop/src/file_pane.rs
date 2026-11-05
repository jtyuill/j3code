//! The file pane (t3's right-panel Files): the thread's project as it is on
//! the thread's server host. A tree loaded one directory at a time
//! (`projects.listEntries`) over a read-only preview of the opened file
//! (`projects.readFile`). With the tree focused, ↑/↓ move, → expands (or
//! enters) a directory, ← collapses it (or goes to the parent) and Enter
//! opens.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Editor, EditorState},
    resizable::{resizable_panel, v_resizable},
    spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    TestSupportExt as _, UniformListScrollHandle, Window, div, px, rems, uniform_list,
};
use j3_client::{
    Error,
    t3::project::{
        ProjectEntry, ProjectEntryKind, ProjectListEntriesResult, ProjectReadFileResult,
    },
};

use crate::{environment::Environment, titlebar};

/// The pane asks to be closed.
pub struct CloseFiles;

/// Width of one tree level.
const INDENT: f32 = 14.;

/// One directory's listing.
#[derive(Default)]
struct Listing {
    entries: Vec<ProjectEntry>,
    truncated: bool,
    /// A request is in flight; `entries` are the previous answer's.
    loading: bool,
    error: Option<SharedString>,
    _request: Option<Task<()>>,
}

/// A status line under a directory's entries.
#[derive(Clone)]
enum Note {
    Loading,
    Empty,
    /// The listing stopped at this many entries.
    Truncated(usize),
    Failed(SharedString),
}

#[derive(Clone)]
enum Row {
    Entry {
        path: SharedString,
        name: SharedString,
        depth: usize,
        directory: bool,
        /// Ignored by git, itself or through a parent.
        ignored: bool,
    },
    Note {
        /// The directory the note is about.
        dir: SharedString,
        depth: usize,
        note: Note,
    },
}

enum PreviewBody {
    Loading,
    Text {
        editor: Entity<EditorState>,
        /// Shown above a truncated file.
        truncated: Option<String>,
    },
    Binary,
    Failed(SharedString),
}

struct Preview {
    path: SharedString,
    body: PreviewBody,
    _request: Task<()>,
}

pub struct FilePane {
    environment: Entity<Environment>,
    /// The project's directory on the server host.
    cwd: String,
    project: SharedString,
    /// Listings by directory path, `""` for the project root.
    listings: HashMap<String, Listing>,
    expanded: HashSet<String>,
    /// The visible tree, rebuilt when listings or expansion change.
    rows: Rc<Vec<Row>>,
    selected: Option<SharedString>,
    preview: Option<Preview>,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
}

impl EventEmitter<CloseFiles> for FilePane {}

impl FilePane {
    pub fn new(
        environment: Entity<Environment>,
        cwd: String,
        project: SharedString,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut pane = Self {
            environment,
            cwd,
            project,
            listings: HashMap::new(),
            expanded: HashSet::new(),
            rows: Rc::default(),
            selected: None,
            preview: None,
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
        };
        pane.load(String::new(), cx);
        pane
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus_handle, cx);
    }

    /// Lists the expanded directories again (dropping collapsed ones'
    /// listings) and rereads the open file.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let expanded = &self.expanded;
        self.listings
            .retain(|dir, _| dir.is_empty() || expanded.contains(dir));
        let dirs: Vec<String> = self.listings.keys().cloned().collect();
        for dir in dirs {
            self.load(dir, cx);
        }
        if let Some(path) = self.preview.as_ref().map(|preview| preview.path.clone()) {
            self.read(path, window, cx);
        }
    }

    fn load(&mut self, dir: String, cx: &mut Context<Self>) {
        let connection = self.environment.read(cx).connection().clone();
        let cwd = self.cwd.clone();
        let key = dir.clone();
        let request = cx.spawn(async move |this, cx| {
            let result = connection.list_entries(&cwd, &dir).await;
            let _ = this.update(cx, |this, cx| this.listed(&dir, result, cx));
        });
        let listing = self.listings.entry(key).or_default();
        listing.loading = true;
        listing._request = Some(request);
        self.rebuild(cx);
    }

    fn listed(
        &mut self,
        dir: &str,
        result: Result<ProjectListEntriesResult, Error>,
        cx: &mut Context<Self>,
    ) {
        let Some(listing) = self.listings.get_mut(dir) else {
            return;
        };
        listing.loading = false;
        match result {
            Ok(result) => {
                listing.entries = result.entries;
                listing.truncated = result.truncated;
                listing.error = None;
            }
            Err(error) => listing.error = Some(error.to_string().into()),
        }
        self.rebuild(cx);
    }

    fn rebuild(&mut self, cx: &mut Context<Self>) {
        let mut rows = Vec::new();
        self.push_rows("", 0, false, &mut rows);
        self.rows = Rc::new(rows);
        cx.notify();
    }

    fn push_rows(&self, dir: &str, depth: usize, ignored: bool, rows: &mut Vec<Row>) {
        let Some(listing) = self.listings.get(dir) else {
            return;
        };
        for entry in &listing.entries {
            let directory = entry.kind == ProjectEntryKind::Directory;
            let ignored = ignored || entry.ignored == Some(true);
            let name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
            rows.push(Row::Entry {
                path: entry.path.clone().into(),
                name: name.to_owned().into(),
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
                dir: dir.to_owned().into(),
                depth,
                note,
            });
        }
    }

    fn row_index(&self, path: &str) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| matches!(row, Row::Entry { path: p, .. } if p == path))
    }

    fn select_index(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(Row::Entry { path, .. }) = self.rows.get(ix) {
            self.selected = Some(path.clone());
            self.scroll.scroll_to_item(ix, ScrollStrategy::Top);
            cx.notify();
        }
    }

    /// Moves the selection `delta` entries, skipping notes.
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let entries: Vec<usize> = (0..self.rows.len())
            .filter(|ix| matches!(self.rows[*ix], Row::Entry { .. }))
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
        self.select_index(entries[next], cx);
    }

    fn selected_row(&self) -> Option<(usize, Row)> {
        let ix = self.row_index(self.selected.as_deref()?)?;
        Some((ix, self.rows[ix].clone()))
    }

    fn toggle_dir(&mut self, path: &str, cx: &mut Context<Self>) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_owned());
            if !self.listings.contains_key(path) {
                self.load(path.to_owned(), cx);
            }
        }
        self.rebuild(cx);
    }

    /// →: expands the selected directory, or enters an expanded one.
    fn expand_selected(&mut self, cx: &mut Context<Self>) {
        let Some((
            ix,
            Row::Entry {
                path,
                directory: true,
                ..
            },
        )) = self.selected_row()
        else {
            return;
        };
        if self.expanded.contains(path.as_ref()) {
            if matches!(self.rows.get(ix + 1), Some(Row::Entry { depth, .. }) if *depth > 0) {
                self.select_index(ix + 1, cx);
            }
        } else {
            self.toggle_dir(&path, cx);
        }
    }

    /// ←: collapses the selected directory, or selects the parent.
    fn collapse_selected(&mut self, cx: &mut Context<Self>) {
        let Some((
            _,
            Row::Entry {
                path, directory, ..
            },
        )) = self.selected_row()
        else {
            return;
        };
        if directory && self.expanded.contains(path.as_ref()) {
            self.toggle_dir(&path, cx);
        } else if let Some((parent, _)) = path.rsplit_once('/')
            && let Some(ix) = self.row_index(parent)
        {
            self.select_index(ix, cx);
        }
    }

    /// Enter or a click: toggles a directory, opens a file.
    fn activate(
        &mut self,
        path: SharedString,
        directory: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(path.clone());
        if directory {
            self.toggle_dir(&path, cx);
        } else if self
            .preview
            .as_ref()
            .is_none_or(|preview| preview.path != path)
        {
            self.read(path, window, cx);
        }
        cx.notify();
    }

    fn read(&mut self, path: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let connection = self.environment.read(cx).connection().clone();
        let cwd = self.cwd.clone();
        let requested = path.clone();
        let request = cx.spawn_in(window, async move |this, cx| {
            let result = connection.read_file(&cwd, &requested).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.file_read(&requested, result, window, cx)
            });
        });
        self.preview = Some(Preview {
            path,
            body: PreviewBody::Loading,
            _request: request,
        });
        cx.notify();
    }

    fn file_read(
        &mut self,
        path: &str,
        result: Result<ProjectReadFileResult, Error>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(preview) = self.preview.as_mut().filter(|preview| preview.path == path) else {
            return;
        };
        preview.body = match result {
            Ok(file) => {
                let language = crate::syntax::language(path);
                let truncated = file.truncated.then(|| {
                    format!(
                        "Showing the first {} of {}.",
                        format_size(file.contents.len() as u64),
                        format_size(file.byte_length)
                    )
                });
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .soft_wrap(false)
                        .default_value(file.contents)
                });
                PreviewBody::Text { editor, truncated }
            }
            Err(Error::Rejected { error, .. }) if error["failure"] == "binary_file" => {
                PreviewBody::Binary
            }
            Err(error) => PreviewBody::Failed(error.to_string().into()),
        };
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.modifiers.modified() {
            return;
        }
        match event.keystroke.key.as_str() {
            "up" => self.move_selection(-1, cx),
            "down" => self.move_selection(1, cx),
            "right" => self.expand_selected(cx),
            "left" => self.collapse_selected(cx),
            "enter" => match self.selected_row() {
                Some((
                    _,
                    Row::Entry {
                        path, directory, ..
                    },
                )) => self.activate(path, directory, window, cx),
                _ => self.move_selection(1, cx),
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let loading = self.listings.values().any(|listing| listing.loading)
            || self
                .preview
                .as_ref()
                .is_some_and(|preview| matches!(preview.body, PreviewBody::Loading));
        h_flex()
            .flex_shrink_0()
            .h(rems(titlebar::ROW_HEIGHT))
            .px_3()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Icon::new(Lucide::FolderTree)
                    .small()
                    .text_color(theme.muted_foreground),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_medium()
                    .child(self.project.clone()),
            )
            .child(if loading {
                div()
                    .size_6()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Spinner::new().xsmall())
                    .into_any_element()
            } else {
                Button::new("refresh-files")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::RefreshCw))
                    .tooltip("Refresh files")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx)))
                    .into_any_element()
            })
            .child(
                Button::new("close-files")
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(Lucide::X))
                    .tooltip(format!(
                        "Close files ({})",
                        crate::actions::shortcut("secondary-shift-e", cx)
                    ))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CloseFiles))),
            )
            .into_any_element()
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let rows = Rc::clone(&self.rows);
        let focused = self.focus_handle.is_focused(window);
        let theme = cx.theme();
        let guides = |depth: usize| {
            (0..depth).map(|_| {
                div().flex_shrink_0().w(px(INDENT)).h_full().child(
                    div()
                        .ml(px(INDENT / 2.))
                        .h_full()
                        .border_l_1()
                        .border_color(theme.border),
                )
            })
        };
        let mut elements = Vec::with_capacity(range.len());
        for row in &rows[range] {
            let element = match row {
                Row::Entry {
                    path,
                    name,
                    depth,
                    directory,
                    ignored,
                } => {
                    let selected = self.selected.as_ref() == Some(path);
                    let expanded = *directory && self.expanded.contains(path.as_ref());
                    let chevron = directory.then(|| {
                        if expanded {
                            Lucide::ChevronDown
                        } else {
                            Lucide::ChevronRight
                        }
                    });
                    let icon = match (*directory, expanded) {
                        (true, true) => Lucide::FolderOpen,
                        (true, false) => Lucide::Folder,
                        (false, _) => file_icon(name),
                    };
                    let color = if selected && focused {
                        theme.accent_foreground
                    } else if *ignored {
                        theme.muted_foreground
                    } else {
                        theme.foreground
                    };
                    let (path, directory) = (path.clone(), *directory);
                    h_flex()
                        .id(SharedString::from(format!("file-row-{path}")))
                        .test_support()
                        .aria_label(path.clone())
                        .h_6()
                        .px_2()
                        .rounded(theme.radius)
                        .text_sm()
                        .text_color(color)
                        .when(selected && focused, |this| this.bg(theme.accent))
                        .when(selected && !focused, |this| this.bg(theme.list_active))
                        .when(!selected, |this| {
                            this.hover(|this| this.bg(theme.list_hover))
                        })
                        .children(guides(*depth))
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(px(INDENT))
                                .flex()
                                .items_center()
                                .when_some(chevron, |this, chevron| {
                                    this.child(
                                        Icon::new(chevron)
                                            .xsmall()
                                            .text_color(theme.muted_foreground),
                                    )
                                }),
                        )
                        .child(
                            Icon::new(icon)
                                .small()
                                .flex_shrink_0()
                                .mr_1p5()
                                .when(*ignored || !selected || !focused, |this| {
                                    this.text_color(theme.muted_foreground)
                                }),
                        )
                        .child(div().min_w_0().truncate().child(name.clone()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.focus(window, cx);
                            this.activate(path.clone(), directory, window, cx);
                        }))
                        .into_any_element()
                }
                Row::Note { dir, depth, note } => {
                    let (text, color): (SharedString, _) = match note {
                        Note::Loading => ("Loading…".into(), theme.muted_foreground),
                        Note::Empty => ("Empty".into(), theme.muted_foreground),
                        Note::Truncated(count) => (
                            format!("Too many entries; showing the first {count}").into(),
                            theme.muted_foreground,
                        ),
                        Note::Failed(error) => (error.clone(), theme.danger),
                    };
                    h_flex()
                        .id(SharedString::from(format!("file-note-{dir}")))
                        .test_support()
                        .aria_label(text.clone())
                        .h_6()
                        .px_2()
                        .gap_1p5()
                        .text_xs()
                        .text_color(color)
                        .children(guides(*depth))
                        .child(div().flex_shrink_0().w(px(INDENT)))
                        .when(matches!(note, Note::Loading), |this| {
                            this.child(Spinner::new().xsmall())
                        })
                        .child(div().min_w_0().truncate().child(text))
                        .into_any_element()
                }
            };
            elements.push(element);
        }
        elements
    }

    fn render_tree(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("file-tree")
            .test_support()
            .key_context("FileTree")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .py_1()
            .px_1()
            .child(
                uniform_list(
                    "file-tree-rows",
                    self.rows.len(),
                    cx.processor(Self::render_rows),
                )
                .size_full()
                .track_scroll(&self.scroll),
            )
            .into_any_element()
    }

    fn render_preview(&self, preview: &Preview, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let segments: Vec<&str> = preview.path.split('/').collect();
        let last = segments.len() - 1;
        let breadcrumb = segments.into_iter().enumerate().flat_map(|(ix, segment)| {
            let separator = (ix > 0).then(|| {
                Icon::new(Lucide::ChevronRight)
                    .xsmall()
                    .flex_shrink_0()
                    .text_color(theme.muted_foreground)
                    .into_any_element()
            });
            let segment = div()
                .when(ix < last, |this| this.min_w_0().truncate())
                .when(ix == last, |this| {
                    this.flex_shrink_0().text_color(theme.foreground)
                })
                .child(segment.to_owned())
                .into_any_element();
            separator.into_iter().chain([segment])
        });
        let notice = |text: String, color| {
            div()
                .id("file-preview-notice")
                .test_support()
                .aria_label(text.clone())
                .flex_shrink_0()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(color)
                .border_b_1()
                .border_color(theme.border)
                .child(text)
        };
        let path = preview.path.clone();
        let body = match &preview.body {
            PreviewBody::Loading => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(Spinner::new())
                .into_any_element(),
            PreviewBody::Text { editor, truncated } => v_flex()
                .size_full()
                .when_some(truncated.clone(), |this, truncated| {
                    this.child(notice(truncated, theme.muted_foreground))
                })
                .child(
                    div().flex_1().min_h_0().child(
                        Editor::new(editor)
                            .readonly(true)
                            .bordered(false)
                            .h_full()
                            .aria_label(format!("Contents of {path}")),
                    ),
                )
                .into_any_element(),
            PreviewBody::Binary => notice(
                "Binary file; there is no preview.".to_owned(),
                theme.muted_foreground,
            )
            .into_any_element(),
            PreviewBody::Failed(error) => {
                notice(error.to_string(), theme.danger).into_any_element()
            }
        };
        let copied = preview.path.clone();
        v_flex()
            .id("file-preview")
            .test_support()
            .size_full()
            .child(
                h_flex()
                    .flex_shrink_0()
                    .h_8()
                    .px_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_0p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .children(breadcrumb),
                    )
                    .child(
                        Button::new("copy-file-path")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Lucide::Copy))
                            .tooltip("Copy path")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copied.to_string()))
                            }),
                    )
                    .child(
                        Button::new("close-file-preview")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(Lucide::X))
                            .tooltip("Close preview")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.preview = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

impl Focusable for FilePane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FilePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let tree = self.render_tree(cx);
        let body = match &self.preview {
            None => tree.into_any_element(),
            Some(preview) => {
                let rem = window.rem_size();
                v_resizable("file-pane-split")
                    .child(
                        resizable_panel()
                            .size(rem * 14.)
                            .size_range(rem * 4.5..rem * 60.)
                            .child(tree),
                    )
                    .child(resizable_panel().child(self.render_preview(preview, cx)))
                    .into_any_element()
            }
        };
        v_flex()
            .id("file-pane")
            .test_support()
            .size_full()
            .border_l_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// The icon of a file named `name`.
fn file_icon(name: &str) -> Lucide {
    let name = name.to_ascii_lowercase();
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match (name.as_str(), extension) {
        ("cargo.lock" | "flake.lock" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock", _)
        | (_, "lock") => Lucide::FileLock,
        (
            ".gitignore" | ".gitattributes" | ".editorconfig" | ".envrc" | "dockerfile"
            | "makefile",
            _,
        )
        | (_, "toml" | "yaml" | "yml" | "ini" | "cfg" | "conf" | "env" | "nix") => Lucide::FileCog,
        (_, "json" | "jsonc") => Lucide::FileBraces,
        (_, "md" | "mdx" | "markdown" | "txt" | "rst") => Lucide::FileText,
        (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "avif") => {
            Lucide::FileImage
        }
        (_, "sh" | "bash" | "zsh" | "fish") => Lucide::FileTerminal,
        (_, "zip" | "tar" | "gz" | "tgz" | "xz" | "zst" | "7z") => Lucide::FileArchive,
        (_, "pem" | "key" | "crt") => Lucide::FileKey,
        (
            _,
            "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "py" | "go" | "c" | "h" | "cc"
            | "cpp" | "hpp" | "java" | "kt" | "swift" | "rb" | "php" | "cs" | "zig" | "lua"
            | "html" | "css" | "scss" | "vue" | "svelte" | "sql",
        ) => Lucide::FileCode,
        _ => Lucide::File,
    }
}

/// `bytes` for people: `512 B`, `3.4 KB`, `1.0 MB`.
fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.;
    let value = bytes as f64;
    if value < KB {
        format!("{bytes} B")
    } else if value < KB * KB {
        format!("{:.1} KB", value / KB)
    } else {
        format!("{:.1} MB", value / (KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_and_sizes() {
        assert_eq!(file_icon("Cargo.lock"), Lucide::FileLock);
        assert_eq!(file_icon("Cargo.toml"), Lucide::FileCog);
        assert_eq!(file_icon("lib.rs"), Lucide::FileCode);
        assert_eq!(file_icon("logo.PNG"), Lucide::FileImage);
        assert_eq!(file_icon("LICENSE"), Lucide::File);
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(3 * 1024 + 410), "3.4 KB");
        assert_eq!(format_size(1 << 20), "1.0 MB");
    }
}
