//! An offline file pane for the `files` scene and the pane's tests: the `j3code` project's
//! tree with nested expanded directories, a git-ignored `target/`, an empty directory and
//! `src/syntax.rs` previewed.

use j3_client::methods::{EntriesResult, Entry, EntryKind, ReadFileResult};

use super::{Effect, FilePane, preview::Body};
use crate::{app::J3, sidebar::ThreadKey};

/// The previewed file.
pub const PREVIEW_PATH: &str = "crates/j3-desktop/src/syntax.rs";

const PREVIEW: &str = r#"//! The extension → language map shared by the file pane and the feed's tool
//! output. The names are fence language names, as
//! `rune_kit::syntax::highlight` takes them.

/// Fence languages by the file extensions that select them.
const LANGUAGES: &[(&[&str], &str)] = &[
	(&["rs"], "rust"),
	(&["ts", "mts", "cts"], "typescript"),
	(&["json", "jsonc"], "json"),
	(&["toml"], "toml"),
];

/// The fence language of `path`, from its extension; `"text"` when there is
/// none for it.
pub fn language(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "text";
    };
    LANGUAGES
        .iter()
        .find(|(extensions, _)| extensions.iter().any(|known| known.eq_ignore_ascii_case(extension)))
        .map_or("text", |(_, language)| *language)
}
"#;

fn dir(path: &str) -> Entry {
    Entry {
        path: path.to_owned(),
        kind: EntryKind::Dir,
        ignored: None,
    }
}

fn file(path: &str) -> Entry {
    Entry {
        path: path.to_owned(),
        kind: EntryKind::File,
        ignored: None,
    }
}

fn ignored(mut entry: Entry) -> Entry {
    entry.ignored = Some(true);
    entry
}

/// The listings, by directory, in the server's order (directories first).
pub fn listings() -> Vec<(&'static str, Vec<Entry>)> {
    vec![
        (
            "",
            vec![
                dir(".github"),
                dir("crates"),
                dir("docs"),
                ignored(dir("target")),
                file(".gitignore"),
                file("Cargo.lock"),
                file("Cargo.toml"),
                file("README.md"),
                file("flake.nix"),
                file("logo.png"),
            ],
        ),
        (
            "crates",
            vec![
                dir("crates/j3-client"),
                dir("crates/j3-desktop"),
                dir("crates/j3-protocol"),
            ],
        ),
        (
            "crates/j3-desktop",
            vec![
                dir("crates/j3-desktop/src"),
                file("crates/j3-desktop/Cargo.toml"),
                file("crates/j3-desktop/build.rs"),
            ],
        ),
        (
            "crates/j3-desktop/src",
            vec![
                dir("crates/j3-desktop/src/app"),
                file("crates/j3-desktop/src/lib.rs"),
                file("crates/j3-desktop/src/main.rs"),
                file(PREVIEW_PATH),
            ],
        ),
        ("docs", Vec::new()),
        (
            "target",
            vec![dir("target/debug"), file("target/.rustc_info.json")],
        ),
    ]
}

/// A pane over project `project_id` titled `project` with the fixture listings loaded and
/// `expanded` open; `loading` are expanded and still waiting for their listing.
pub fn pane(project_id: &str, project: &str, expanded: &[&str], loading: &[&str]) -> FilePane {
    let mut pane = FilePane::new(project_id.to_owned(), project.to_owned(), 16.0);
    for (dir, entries) in listings() {
        if !dir.is_empty() && !expanded.contains(&dir) {
            continue;
        }
        if !dir.is_empty() {
            pane.expanded.insert(dir.to_owned());
        }
        let Effect::Load { request, .. } = pane.begin_load(dir) else {
            unreachable!("begin_load lists");
        };
        let result = EntriesResult {
            entries,
            truncated: false,
        };
        pane.listed(dir, request, Ok(result));
    }
    for dir in loading {
        pane.expanded.insert((*dir).to_owned());
        pane.begin_load(dir);
    }
    pane
}

/// Opens `key`'s file pane on the fixture tree with [`PREVIEW_PATH`] selected and previewed,
/// the tree focused.
pub fn install(s: &mut J3, key: &ThreadKey) {
    let mut pane = pane(
        "j3code",
        "j3code",
        &[
            "crates",
            "crates/j3-desktop",
            "crates/j3-desktop/src",
            "docs",
        ],
        &[],
    );
    let Effect::Read { request, .. } = pane.begin_read(PREVIEW_PATH.to_owned()) else {
        unreachable!("begin_read reads");
    };
    let read = ReadFileResult {
        path: PREVIEW_PATH.to_owned(),
        text: PREVIEW.to_owned(),
        size: PREVIEW.len() as u64,
        truncated: false,
        exists: true,
    };
    pane.file_read(PREVIEW_PATH, request, Body::read(PREVIEW_PATH, Ok(read)));
    pane.selected = Some(PREVIEW_PATH.to_owned());
    pane.open = true;
    pane.keys.focused = true;
    s.files.insert(key.clone(), pane);
}
