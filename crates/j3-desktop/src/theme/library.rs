//! The themes on offer: the built-in ones (`crates/j3-desktop/themes/*.json`, compiled in)
//! and the user's (`<config dir>/j3/themes/*.json`), each known by its id, the file's name
//! without `.json`. `main` names the user's directory ([`install`]); Settings re-reads it each
//! time it opens ([`reload`]). `J3_THEME` (an id or a file) shows one theme whatever the
//! setting says ([`force`]), for previewing a theme being written.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, OnceLock, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use super::file::Theme;

/// The id of the built-in theme `ui.theme: None` shows for each system appearance.
pub const LIGHT: &str = "light";
pub const DARK: &str = "dark";

const BUILTIN: [(&str, &str); 6] = [
    (LIGHT, include_str!("../../themes/light.json")),
    (DARK, include_str!("../../themes/dark.json")),
    ("tokyo-night", include_str!("../../themes/tokyo-night.json")),
    (
        "tokyo-night-storm",
        include_str!("../../themes/tokyo-night-storm.json"),
    ),
    (
        "tokyo-night-moon",
        include_str!("../../themes/tokyo-night-moon.json"),
    ),
    (
        "tokyo-night-day",
        include_str!("../../themes/tokyo-night-day.json"),
    ),
];

pub struct Entry {
    pub id: String,
    pub theme: Theme,
}

struct Library {
    /// Built-in themes in [`BUILTIN`]'s order, then the user's by id.
    entries: Vec<Arc<Entry>>,
    /// Why each user theme that didn't load didn't: `file.json: reason`.
    errors: Vec<String>,
    dir: Option<PathBuf>,
}

static LIBRARY: LazyLock<RwLock<Library>> = LazyLock::new(|| {
    let entries = BUILTIN
        .iter()
        .map(|(id, json)| {
            let theme = Theme::parse(json).unwrap_or_else(|e| panic!("themes/{id}.json: {e}"));
            Arc::new(Entry {
                id: (*id).to_owned(),
                theme,
            })
        })
        .collect();
    RwLock::new(Library {
        entries,
        errors: Vec::new(),
        dir: None,
    })
});

/// Bumped whenever the entries may have changed, so kits built from them are rebuilt.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static FORCED: OnceLock<String> = OnceLock::new();

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Relaxed)
}

/// The theme shown instead of the setting's, if `J3_THEME` named one.
pub fn forced() -> Option<&'static str> {
    FORCED.get().map(String::as_str)
}

/// The id a theme file has: its name without the extension.
fn id_of(path: &Path) -> Result<String, String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "the file's name is not text".to_owned())
}

/// Reads and checks the theme file at `path`.
pub fn read(path: &Path) -> Result<Theme, String> {
    let json = fs::read_to_string(path).map_err(|e| e.to_string())?;
    Theme::parse(&json)
}

/// Where the user's themes are, when `main` named a directory.
pub fn dir() -> Option<PathBuf> {
    LIBRARY.read().unwrap().dir.clone()
}

/// Names the user's theme directory and loads it.
pub fn install(dir: PathBuf) {
    LIBRARY.write().unwrap().dir = Some(dir);
    reload();
}

/// Reads the user's theme directory again. A missing directory holds no themes.
pub fn reload() {
    let mut library = LIBRARY.write().unwrap();
    let Some(dir) = library.dir.clone() else {
        return;
    };
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    library.entries.truncate(BUILTIN.len());
    library.errors.clear();
    for path in paths {
        let loaded = id_of(&path).and_then(|id| {
            if BUILTIN.iter().any(|(builtin, _)| *builtin == id) {
                return Err(format!(
                    "`{id}` is a built-in theme; give the file another name"
                ));
            }
            Ok(Entry {
                id,
                theme: read(&path)?,
            })
        });
        match loaded {
            Ok(entry) => library.entries.push(Arc::new(entry)),
            Err(e) => {
                let file = path.file_name().unwrap_or_default().to_string_lossy();
                tracing::warn!("theme {}: {e}", path.display());
                library.errors.push(format!("{file}: {e}"));
            }
        }
    }
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Shows `theme` (an id, or the path of a theme file) whatever the setting says. Call once,
/// after [`install`].
pub fn force(theme: &str) -> Result<(), String> {
    let path = Path::new(theme);
    let id = if path.is_file() {
        let entry = Arc::new(Entry {
            id: id_of(path)?,
            theme: read(path)?,
        });
        let id = entry.id.clone();
        let mut library = LIBRARY.write().unwrap();
        library.entries.retain(|e| e.id != id);
        library.entries.push(entry);
        GENERATION.fetch_add(1, Ordering::Relaxed);
        id
    } else if get(theme).is_some() {
        theme.to_owned()
    } else {
        let ids: Vec<String> = list().into_iter().map(|(id, _)| id).collect();
        return Err(format!(
            "`{theme}` is neither a theme file nor one of: {}",
            ids.join(", ")
        ));
    };
    FORCED
        .set(id)
        .map_err(|_| "a theme is forced already".to_owned())
}

pub fn get(id: &str) -> Option<Arc<Entry>> {
    let library = LIBRARY.read().unwrap();
    library.entries.iter().find(|e| e.id == id).cloned()
}

/// Every theme's `(id, name)`.
pub fn list() -> Vec<(String, String)> {
    let library = LIBRARY.read().unwrap();
    library
        .entries
        .iter()
        .map(|e| (e.id.clone(), e.theme.name.clone()))
        .collect()
}

/// The user theme files that didn't load, and why.
pub fn errors() -> Vec<String> {
    LIBRARY.read().unwrap().errors.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_themes_load_with_their_terminal_colours() {
        let themes = list();
        let names: Vec<&str> = themes.iter().map(|(_, name)| name.as_str()).collect();
        assert_eq!(
            names[..6],
            [
                "Light",
                "Dark",
                "Tokyo Night",
                "Tokyo Night Storm",
                "Tokyo Night Moon",
                "Tokyo Night Day"
            ]
        );
        for (id, _) in &themes[..6] {
            let entry = get(id).unwrap();
            assert!(entry.theme.ansi.is_some(), "{id}");
            assert_eq!(entry.theme.kit().dark, entry.theme.dark, "{id}");
        }
        assert!(!get(LIGHT).unwrap().theme.dark && get(DARK).unwrap().theme.dark);
        assert!(!get("tokyo-night-day").unwrap().theme.dark);
    }
}
