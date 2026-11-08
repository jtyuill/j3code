//! "Add project" (spec §8.4): a device (any live environment; a chosen one that went
//! offline stays listed), a folder typed or browsed on it with `connection.browse` (stale
//! replies dropped by revision), and a name filled in from the folder until the user edits
//! it. Creating the project closes the dialog and, from the main window, starts a thread in
//! it.
//!
//! While a folder name is typed, the first folder beside it that starts with what was typed
//! shows after the caret as a suggestion; Tab or → takes it.

use j3_client::t3::filesystem::FilesystemBrowseEntry;
use rune::{Cx, El, Key, KeyEvent, Mods, ScrollSpec, TextEdit, Tx, col, el, icon, row, text};
use rune_kit::{Kit, Spinner, TextInput};

use super::{Dialog, Field};
use crate::{app::J3, icons, settings::controls::segmented, theme::Status};

/// The folder listing under the path field.
pub enum Listing {
    Loading,
    Loaded {
        parent: String,
        entries: Vec<FilesystemBrowseEntry>,
    },
    Failed(String),
}

/// The folders of the directory the path's last segment is typed in: what its suggestion
/// is picked from.
pub struct Siblings {
    /// The path up to and including its last `/` (empty: the home directory).
    pub dir: String,
    /// `None` until listed (or when listing failed).
    pub entries: Option<Vec<FilesystemBrowseEntry>>,
}

/// The Add project dialog's form.
pub struct ProjectPicker {
    /// The chosen device (environment key).
    pub env: String,
    /// Start a thread in the new project once it exists.
    pub then_new_thread: bool,
    pub path: TextEdit,
    pub title: TextEdit,
    /// The user typed a name: the folder no longer names the project.
    pub title_edited: bool,
    pub listing: Listing,
    /// The folders the path's suggestion comes from, listed once per directory.
    pub siblings: Option<Siblings>,
    /// Which opening of the dialog this is (from `DialogState::next_serial`): a create that
    /// finishes after this picker closed must not act on another one.
    pub id: u64,
    /// The browse whose reply the listing waits for (from `DialogState::next_serial`, so a
    /// closed picker's reply never matches a later one); older replies are dropped.
    pub revision: u64,
    pub creating: bool,
    pub error: Option<String>,
}

impl ProjectPicker {
    pub fn new(env: String, then_new_thread: bool) -> Self {
        let mut path = TextEdit::new("~/");
        path.focused = true;
        path.set_caret(path.text.len());
        Self {
            env,
            then_new_thread,
            path,
            title: TextEdit::new(""),
            title_edited: false,
            listing: Listing::Loading,
            siblings: None,
            id: 0,
            revision: 0,
            creating: false,
            error: None,
        }
    }

    /// What the path's suggestion adds after the caret: the rest of the first folder (in
    /// listing order) whose name starts with the segment being typed, and a `/`. Only with
    /// the caret at the end and nothing selected or being composed.
    pub fn suggestion(&self) -> Option<String> {
        let edit = &self.path;
        let end = edit.text.len();
        if edit.preedit.is_some() || edit.anchor != end || edit.focus != end {
            return None;
        }
        let dir = dir_part(&edit.text);
        let typed = &edit.text[dir.len()..];
        let siblings = self.siblings.as_ref().filter(|s| s.dir == dir)?;
        if typed.is_empty() {
            return None;
        }
        siblings
            .entries
            .as_ref()?
            .iter()
            .find(|entry| entry.name.starts_with(typed))
            .map(|entry| format!("{}/", &entry.name[typed.len()..]))
    }
}

/// `path` up to and including its last `/`; empty without one.
fn dir_part(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..=i])
}

/// A project's default title: the last segment of its path. The home directory (`~`) and
/// the root name nothing.
pub fn title_from_path(path: &str) -> String {
    let last = path
        .trim()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if last == "~" { "" } else { last }.to_owned()
}

impl J3 {
    fn picker(&mut self) -> Option<&mut ProjectPicker> {
        match &mut self.dialogs.open {
            Some(Dialog::AddProject(picker)) => Some(picker),
            _ => None,
        }
    }

    /// The Add project dialog just opened: start on its environment when that is live, else
    /// on the first live device, and list the home folder.
    pub(super) fn project_picker_opened(&mut self) {
        let live: Vec<String> = self
            .envs
            .distinct()
            .into_iter()
            .filter(|env| env.is_live())
            .map(|env| env.key().to_owned())
            .collect();
        let id = self.dialogs.next_serial();
        let Some(picker) = self.picker() else {
            return;
        };
        picker.id = id;
        if !live.contains(&picker.env)
            && let Some(first) = live.into_iter().next()
        {
            picker.env = first;
        }
        self.browse_folders();
    }

    /// Switches device: its folders start again from home.
    pub fn choose_project_device(&mut self, env: String) {
        let Some(picker) = self.picker() else {
            return;
        };
        if picker.env == env {
            return;
        }
        picker.env = env;
        picker.error = None;
        picker.siblings = None;
        picker.path.replace_text("~/".to_owned());
        picker.path.set_caret(picker.path.text.len());
        picker.path.focused = true;
        self.project_path_changed();
    }

    /// The path changed: the name follows it (until edited) and the listing reloads.
    pub fn project_path_changed(&mut self) {
        let Some(picker) = self.picker() else {
            return;
        };
        if !picker.title_edited {
            picker
                .title
                .replace_text(title_from_path(&picker.path.text));
        }
        self.browse_folders();
    }

    /// Opens folder `full_path` of the listing: the path becomes it (with a trailing `/`).
    pub fn open_project_folder(&mut self, full_path: &str) {
        let Some(picker) = self.picker() else {
            return;
        };
        let path = format!("{}/", full_path.trim_end_matches('/'));
        picker.path.replace_text(path);
        picker.path.set_caret(picker.path.text.len());
        picker.path.focused = true;
        self.project_path_changed();
    }

    /// The path field's keys: Tab or → at the end takes the suggestion. True when taken.
    fn project_path_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        if ev.mods != Mods::default() || !matches!(ev.key, Key::Tab | Key::Right) {
            return false;
        }
        let Some(picker) = self.picker() else {
            return false;
        };
        let Some(suffix) = picker.suggestion() else {
            return false;
        };
        let path = format!("{}{suffix}", picker.path.text);
        picker.path.replace_text(path);
        picker.path.set_caret(picker.path.text.len());
        self.project_path_changed();
        tx.instant();
        true
    }

    /// Lists the folders matching the path on the chosen device, and the folders beside the
    /// segment being typed when that is a directory not listed yet.
    fn browse_folders(&mut self) {
        let Some(env) = self.picker().map(|picker| picker.env.clone()) else {
            return;
        };
        let connection = self.envs.get(&env).map(|e| e.connection().clone());
        let revision = self.dialogs.next_serial();
        let Some(picker) = self.picker() else {
            return;
        };
        picker.revision = revision;
        let Some(connection) = connection else {
            picker.listing = Listing::Failed("That device isn’t configured any more.".into());
            return;
        };
        picker.listing = Listing::Loading;
        let partial = picker.path.text.clone();
        let id = picker.id;
        let dir = dir_part(&partial).to_owned();
        let new_dir = picker.siblings.as_ref().is_none_or(|s| s.dir != dir);
        if new_dir {
            picker.siblings = Some(Siblings {
                dir: dir.clone(),
                entries: None,
            });
            // A path ending in `/` lists that directory itself, below.
            if partial != dir {
                let connection = connection.clone();
                self.spawn(async move {
                    let result = connection.browse(&dir).await;
                    move |s: &mut J3, _: &mut Tx| {
                        if let Ok(result) = result {
                            s.set_siblings(id, &dir, result.entries);
                        }
                    }
                });
            }
        }
        self.spawn(async move {
            let result = connection.browse(&partial).await;
            move |s: &mut J3, _: &mut Tx| {
                if let Ok(result) = &result
                    && partial.ends_with('/')
                {
                    s.set_siblings(id, &partial, result.entries.clone());
                }
                let Some(picker) = s.picker() else {
                    return;
                };
                if picker.revision != revision {
                    return;
                }
                picker.listing = match result {
                    Ok(result) => Listing::Loaded {
                        parent: result.parent_path,
                        entries: result.entries,
                    },
                    Err(e) => Listing::Failed(e.to_string()),
                };
            }
        });
    }

    /// The folders of `dir` arrived for picker `id`: they back the suggestion while the path
    /// is still typed in `dir`.
    fn set_siblings(&mut self, id: u64, dir: &str, entries: Vec<FilesystemBrowseEntry>) {
        if let Some(siblings) = self
            .picker()
            .filter(|picker| picker.id == id)
            .and_then(|picker| picker.siblings.as_mut())
            .filter(|siblings| siblings.dir == dir)
        {
            siblings.entries = Some(entries);
        }
    }

    /// Creates the project; on success the dialog closes and, when asked, a new thread opens
    /// in it.
    pub(super) fn create_project(&mut self) {
        let Some(picker) = self.picker() else {
            return;
        };
        if picker.creating {
            return;
        }
        let path = picker.path.text.trim().to_owned();
        if path.is_empty() {
            picker.error = Some("Choose a folder.".into());
            return;
        }
        let title = match picker.title.text.trim() {
            "" => title_from_path(&path),
            title => title.to_owned(),
        };
        let env = picker.env.clone();
        let then_new_thread = picker.then_new_thread;
        let id = picker.id;
        let Some(connection) = self.envs.get(&env).map(|e| e.connection().clone()) else {
            return;
        };
        let Some(picker) = self.picker() else {
            return;
        };
        picker.creating = true;
        picker.error = None;
        self.spawn(async move {
            let result = connection.create_project(&title, &path).await;
            move |s: &mut J3, tx: &mut Tx| {
                // Cancelled, or another dialog took its place: the project exists, but
                // nothing on screen asked for what comes next any more.
                let Some(picker) = s.picker().filter(|picker| picker.id == id) else {
                    return;
                };
                match result {
                    Ok(project) => {
                        s.close_dialog();
                        if then_new_thread {
                            s.new_thread(env, Some(project.id), tx);
                        }
                    }
                    Err(e) => {
                        picker.creating = false;
                        picker.error = Some(e.to_string());
                    }
                }
            }
        });
    }
}

/// The form's body.
pub(super) fn body(s: &J3, cx: &mut Cx<J3>, k: &Kit, picker: &ProjectPicker) -> El {
    let status = Status::of(k);
    let label = |title: &str| {
        text(title.to_owned())
            .font(k.ty.text_small_medium)
            .color(k.c.text_base)
    };
    let mut form = col()
        .key("project-picker")
        .w_full()
        .gap(12.0)
        .child(
            col()
                .gap(6.0)
                .child(label("Device"))
                .child(row().child(devices(s, k, picker))),
        )
        .child(
            col()
                .gap(6.0)
                .child(label("Folder"))
                .child(path_field(cx, k, picker))
                .child(listing(k, &picker.listing)),
        )
        .child(
            col().gap(6.0).child(label("Name")).child(
                TextInput::new(&picker.title)
                    .width(None)
                    .placeholder("Name")
                    .label("Name")
                    .on_edit(|s: &mut J3| s.dialogs.field(Field::ProjectTitle))
                    .on_change(|s: &mut J3, _: &mut Tx| {
                        if let Some(picker) = s.picker() {
                            picker.title_edited = true;
                        }
                    })
                    .on_submit(|s: &mut J3, tx: &mut Tx| s.confirm_dialog(tx))
                    .build(k)
                    .key("project-title"),
            ),
        );
    if picker.creating {
        form = form.child(
            row()
                .gap(8.0)
                .center_y()
                .child(Spinner::new().size(12.0).build(k))
                .child(
                    text("Adding project…")
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_muted),
                ),
        );
    }
    if let Some(error) = &picker.error {
        form = form.child(
            text(error.clone())
                .key("dialog-error")
                .font(k.ty.text_xsmall)
                .color(status.danger)
                .wrap(),
        );
    }
    form
}

/// The folder field, with its suggestion drawn after the caret while it has focus.
fn path_field(cx: &mut Cx<J3>, k: &Kit, picker: &ProjectPicker) -> El {
    let style = k.ty.ui_compact;
    let input = TextInput::new(&picker.path)
        .width(None)
        .text_style(style)
        .placeholder("~/Developer/project")
        .label("Folder")
        .on_edit(|s: &mut J3| s.dialogs.field(Field::Path))
        .on_change(|s: &mut J3, _: &mut Tx| s.project_path_changed())
        .on_key(|s: &mut J3, ev: &KeyEvent, tx: &mut Tx| s.project_path_key(ev, tx))
        .on_submit(|s: &mut J3, tx: &mut Tx| s.confirm_dialog(tx))
        .build(k)
        .key("project-path");
    let mut field = el().key("project-path-field").w_full().child(input);
    let suggestion = picker.suggestion().filter(|_| picker.path.focused);
    if let Some(suffix) = suggestion
        && let (Some(caret), Some(area)) =
            (cx.caret("project-path"), cx.layout("project-path-field"))
    {
        // Set like the path, from the caret's own geometry, clipped to the field.
        let x = caret[0] + caret[2] - area[0];
        let width = area[2] - 8.0 - x;
        if width > 0.0 {
            field = field.child(
                el().key("project-path-suggestion")
                    .label(suffix.clone())
                    .absolute(x, caret[1] - area[1])
                    .w(width)
                    .h(caret[3])
                    .clip_soft(0.0)
                    .child(
                        text(suffix)
                            .font(style)
                            .color(k.alpha(k.c.text_muted, 0.6))
                            .a11y_hidden(),
                    ),
            );
        }
    }
    field
}

/// One segment per live device, the chosen one selected (and listed even when offline).
fn devices(s: &J3, k: &Kit, picker: &ProjectPicker) -> El {
    let devices: Vec<(String, String)> = s
        .envs
        .distinct()
        .into_iter()
        .filter(|env| env.is_live() || env.key() == picker.env)
        .map(|env| {
            let label = match env.status_label() {
                Some(status) => format!("{} ({status})", env.label()),
                None => env.label().to_owned(),
            };
            (env.key().to_owned(), label)
        })
        .collect();
    let selected = devices.iter().position(|(key, _)| *key == picker.env);
    let keys: Vec<String> = devices.iter().map(|(key, _)| key.clone()).collect();
    let segments: Vec<(String, String)> = devices
        .into_iter()
        .map(|(key, label)| (format!("project-device-{key}"), label))
        .collect();
    segmented(
        k,
        "project-device",
        "Device",
        &segments,
        selected,
        move |s, ix, _| {
            if let Some(env) = keys.get(ix) {
                s.choose_project_device(env.clone());
            }
        },
    )
}

/// The folders under the path: a spinner, the error, or the parent and its folders.
fn listing(k: &Kit, listing: &Listing) -> El {
    let muted = |line: String| {
        row()
            .px(8.0)
            .py(4.0)
            .child(text(line).font(k.ty.text_xsmall).color(k.c.text_muted))
    };
    let mut rows = col().key("folder-rows").w_full().padding(4.0);
    rows = match listing {
        Listing::Loading => {
            rows.child(row().padding(8.0).child(Spinner::new().size(12.0).build(k)))
        }
        Listing::Failed(error) => rows.child(
            row().padding(8.0).child(
                text(error.clone())
                    .font(k.ty.text_xsmall)
                    .color(Status::of(k).danger)
                    .wrap(),
            ),
        ),
        Listing::Loaded { parent, entries } => {
            rows = rows.child(muted(parent.clone()));
            if entries.is_empty() {
                rows = rows.child(muted("No folders".into()));
            }
            for entry in entries {
                let full_path = entry.full_path.clone();
                rows = rows.child(
                    row()
                        .key(format!("browse-{}", entry.full_path))
                        .role("button")
                        .label(entry.full_path.clone())
                        .w_full()
                        .gap(8.0)
                        .px(8.0)
                        .py(3.0)
                        .center_y()
                        .radius(6.0)
                        .hover(k.c.hover)
                        .child(icon(&icons::FOLDER, 14.0).color(k.c.icon_muted))
                        .child(
                            text(entry.name.clone())
                                .font(k.ty.text_small)
                                .color(k.c.text_base)
                                .truncate(),
                        )
                        .on_press(move |s: &mut J3| s.open_project_folder(&full_path)),
                );
            }
            rows
        }
    };
    col()
        .key("folder-listing")
        .w_full()
        .h(208.0)
        .radius(6.0)
        .border(1.0, k.c.border_base)
        .child(
            rows.scroll(ScrollSpec {
                stick_bottom: false,
            })
            .grow(1.0)
            .min_h(0.0),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_titles_come_from_the_last_path_segment() {
        assert_eq!(title_from_path("~/Developer/j3code/"), "j3code");
        assert_eq!(title_from_path("/tmp/j3-desktop-smoke"), "j3-desktop-smoke");
        assert_eq!(title_from_path("  /srv/app// "), "app");
        assert_eq!(title_from_path("/"), "");
        assert_eq!(title_from_path("~/"), "");
        assert_eq!(title_from_path("~"), "");
    }
}
