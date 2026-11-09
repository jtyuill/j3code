//! `projects.searchEntries`: the composer's `@path` completion, matching omp's
//! editor. A query inside the workspace is a fuzzy match over every file and
//! directory git doesn't ignore; `dir/rest` narrows it to `dir`; an empty query
//! or `dir/` lists that one directory. Queries leaving the workspace (`/…`,
//! `~/…`, `../…`) list the directory they name, by name prefix, since a
//! recursive walk there can be enormous.
//!
//! Walks are blocking; callers run [`EntryIndex::search`] on `spawn_blocking`.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ignore::WalkBuilder;
use j3_protocol::v2::envelope::Error;
use j3_protocol::v2::methods::{EntriesResult, Entry, EntryKind, SearchEntriesParams};

use crate::files::workspace_relative;

/// Most entries one workspace walk keeps.
pub const MAX_INDEXED: usize = 25_000;
/// Most entries one search returns.
pub const MAX_LIMIT: usize = 200;
/// How long a walk answers searches before the workspace is walked again, so
/// typing a query walks once while files the agent writes show up soon.
const FRESH: Duration = Duration::from_secs(10);

/// Recent workspace walks, by canonical root.
#[derive(Default)]
pub struct EntryIndex {
    walks: Mutex<HashMap<PathBuf, Arc<Walk>>>,
}

struct Walk {
    at: Instant,
    entries: Vec<Indexed>,
    truncated: bool,
}

struct Indexed {
    /// `/`-joined, relative to the root.
    path: String,
    lower: String,
    directory: bool,
}

/// One candidate before the limit.
struct Found {
    path: String,
    directory: bool,
}

impl EntryIndex {
    /// The entries `params.query` names under the project directory `root`
    /// (`~` is the server user's home): best matches first, at most
    /// `params.limit` (default and cap [`MAX_LIMIT`]).
    pub fn search(&self, root: &str, params: &SearchEntriesParams) -> Result<EntriesResult, Error> {
        let canonical_root = fs::canonicalize(root).map_err(|error| {
            Error::rejected(format!("The project folder {root} can't be read: {error}"))
        })?;
        if !canonical_root.is_dir() {
            return Err(Error::rejected(format!(
                "The project folder {root} is not a directory."
            )));
        }
        let root = canonical_root.as_path();
        let limit = params
            .limit
            .map_or(MAX_LIMIT, |limit| (limit as usize).clamp(1, MAX_LIMIT));
        // `~` and `..` alone name a directory to list, as `~/` and `../` do.
        let query = match params.query.trim() {
            bare @ ("~" | "..") => format!("{bare}/"),
            query => query.to_owned(),
        };
        let query = query.as_str();
        let wanted = |found: &Found| match params.kind {
            Some(EntryKind::File) if found.directory => false,
            Some(EntryKind::Dir) if !found.directory => false,
            _ => params.images_only != Some(true) || (!found.directory && is_image(&found.path)),
        };

        let (dir_part, rest) = query.rsplit_once('/').unwrap_or(("", query));
        let (found, walk_truncated) = if leaves_workspace(query) {
            let shown = &query[..query.len() - rest.len()];
            (list_dir(&expand(shown, root), shown, rest), false)
        } else if query.is_empty() {
            (list_dir(root, "", ""), false)
        } else if let Some(scope) = workspace_relative(dir_part)
            .filter(|scope| !dir_part.is_empty() && !scope.is_empty() && root.join(scope).is_dir())
        {
            if rest.is_empty() {
                (
                    list_dir(&root.join(&scope), &format!("{scope}/"), ""),
                    false,
                )
            } else {
                let walk = self.walk(root);
                let under = format!("{scope}/");
                (fuzzy(&walk, &under, &rest.to_lowercase()), walk.truncated)
            }
        } else {
            let walk = self.walk(root);
            (fuzzy(&walk, "", &query.to_lowercase()), walk.truncated)
        };

        let mut entries = Vec::with_capacity(limit);
        let mut matched = 0;
        for found in found.into_iter().filter(wanted) {
            matched += 1;
            if entries.len() < limit {
                entries.push(Entry {
                    path: found.path,
                    kind: if found.directory {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    },
                    ignored: None,
                });
            }
        }
        Ok(EntriesResult {
            truncated: walk_truncated || matched > limit,
            entries,
        })
    }

    /// `root`'s walk, reused while fresh.
    fn walk(&self, root: &Path) -> Arc<Walk> {
        let now = Instant::now();
        let fresh = |walk: &Arc<Walk>| now.duration_since(walk.at) < FRESH;
        if let Some(walk) = self.walks.lock().unwrap().get(root).filter(|w| fresh(w)) {
            return walk.clone();
        }
        let walk = Arc::new(walk(root));
        let mut walks = self.walks.lock().unwrap();
        walks.retain(|_, walk| fresh(walk));
        walks.insert(root.to_owned(), walk.clone());
        walk
    }
}

/// Every file and directory under `root` git doesn't ignore, hidden ones
/// included, `.git` excluded, at most [`MAX_INDEXED`].
fn walk(root: &Path) -> Walk {
    let at = Instant::now();
    let mut entries = Vec::new();
    let mut truncated = false;
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walker.filter_map(Result::ok) {
        if entry.depth() == 0 {
            continue;
        }
        if entries.len() == MAX_INDEXED {
            truncated = true;
            break;
        }
        // Paths that are not UTF-8 cannot be addressed over the wire.
        let Some(path) = entry.path().strip_prefix(root).ok().and_then(Path::to_str) else {
            continue;
        };
        entries.push(Indexed {
            path: path.to_owned(),
            lower: path.to_lowercase(),
            directory: entry.file_type().is_some_and(|kind| kind.is_dir()),
        });
    }
    Walk {
        at,
        entries,
        truncated,
    }
}

/// The walk's entries under `under` (empty for all) whose remaining path
/// contains `query` as a subsequence, best first: the name equal to, starting
/// with or containing the query, the path containing it, then the tightest
/// subsequences; shorter paths first among equals.
fn fuzzy(walk: &Walk, under: &str, query: &str) -> Vec<Found> {
    let mut scored: Vec<(u32, &Indexed)> = walk
        .entries
        .iter()
        .filter_map(|entry| {
            let lower = entry.lower.strip_prefix(under)?;
            Some((score(query, lower)?, entry))
        })
        .collect();
    scored.sort_by(|(a, x), (b, y)| {
        b.cmp(a)
            .then(x.path.len().cmp(&y.path.len()))
            .then_with(|| x.path.cmp(&y.path))
    });
    scored
        .into_iter()
        .map(|(_, entry)| Found {
            path: entry.path.clone(),
            directory: entry.directory,
        })
        .collect()
}

/// How well lowercase `query` matches lowercase `path`; `None` when it isn't
/// a subsequence.
fn score(query: &str, path: &str) -> Option<u32> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == query {
        return Some(100);
    }
    if name.starts_with(query) {
        return Some(80);
    }
    if name.contains(query) {
        return Some(60);
    }
    if path.contains(query) {
        return Some(50);
    }
    let mut wanted = query.chars().peekable();
    let mut gaps = 0u32;
    let mut last = None;
    for (at, ch) in path.chars().enumerate() {
        if wanted.peek() == Some(&ch) {
            wanted.next();
            if last.is_some_and(|last| at > last + 1) {
                gaps += 1;
            }
            last = Some(at);
        }
    }
    wanted
        .peek()
        .is_none()
        .then(|| 40u32.saturating_sub(gaps * 5).max(1))
}

/// The entries of `dir` whose names start with `name_prefix` (ignoring case),
/// as `shown` + name: directories first, then by name, without `.git`.
fn list_dir(dir: &Path, shown: &str, name_prefix: &str) -> Vec<Found> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let name_prefix = name_prefix.to_lowercase();
    let mut listed: Vec<(String, bool)> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            if name == ".git" || !name.to_lowercase().starts_with(&name_prefix) {
                return None;
            }
            // A symlink lists as what it points at (a file when broken).
            let directory = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => entry.path().is_dir(),
                Ok(kind) => kind.is_dir(),
                Err(_) => false,
            };
            Some((name, directory))
        })
        .collect();
    listed.sort_by_cached_key(|(name, directory)| (!directory, name.to_lowercase(), name.clone()));
    listed
        .into_iter()
        .map(|(name, directory)| Found {
            path: format!("{shown}{name}"),
            directory,
        })
        .collect()
}

/// Whether `query` names a place outside the workspace: absolute, under the
/// home directory, or climbing out with `..`.
fn leaves_workspace(query: &str) -> bool {
    query.starts_with('/')
        || query.starts_with("~/")
        || (!query.is_empty() && workspace_relative(query).is_none())
}

/// The host directory `shown` (a query's directory part: empty, or ending in
/// `/`) names; `~/` is the server user's home.
fn expand(shown: &str, root: &Path) -> PathBuf {
    match shown.strip_prefix("~/") {
        Some(rest) => {
            Path::new(&std::env::var("HOME").unwrap_or_else(|_| "/".to_owned())).join(rest)
        }
        None => root.join(shown),
    }
}

/// Whether `path` is an image the clients render.
fn is_image(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::tests::project;

    fn search(index: &EntryIndex, root: &Path, query: &str) -> Vec<String> {
        let params = SearchEntriesParams {
            project_id: "p".into(),
            query: query.into(),
            limit: None,
            kind: None,
            images_only: None,
        };
        let result = index.search(root.to_str().unwrap(), &params).unwrap();
        result.entries.into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn search_ranks_names_over_paths_and_subsequences() {
        let dir = project();
        let root = dir.path();
        for path in [
            "src/app.rs",
            "src/apps/main.rs",
            "docs/mapping.md",
            "a/p/p.txt",
            "app.rs",
        ] {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        fs::write(root.join(".gitignore"), "docs/\n").unwrap();
        let index = EntryIndex::default();
        let found = search(&index, root, "app");
        // Name prefixes (shorter paths first), then paths containing it,
        // then subsequences.
        assert_eq!(found[..3], ["app.rs", "src/apps", "src/app.rs"]);
        assert_eq!(found[3..], ["src/apps/main.rs", "a/p/p.txt"]);
        assert!(found.iter().all(|path| !path.starts_with("docs")));

        // `dir/rest` narrows to `dir`; `dir/` lists it.
        assert_eq!(search(&index, root, "src/main"), ["src/apps/main.rs"]);
        assert_eq!(search(&index, root, "src/"), ["src/apps", "src/app.rs"]);
        // An empty query lists the root, directories first.
        assert_eq!(search(&index, root, "")[..2], ["a", "docs"]);

        let params = SearchEntriesParams {
            project_id: "p".into(),
            query: "app".into(),
            limit: Some(1),
            kind: Some(EntryKind::File),
            images_only: None,
        };
        let limited = index.search(root.to_str().unwrap(), &params).unwrap();
        assert_eq!(limited.entries.len(), 1);
        assert_eq!(limited.entries[0].path, "app.rs");
        assert_eq!(limited.entries[0].kind, EntryKind::File);
        assert!(limited.truncated);
    }

    #[test]
    fn search_lists_directories_outside_the_project_by_prefix() {
        let dir = project();
        let root = dir.path().join("proj");
        fs::create_dir_all(root.join("x")).unwrap();
        fs::create_dir_all(dir.path().join("sibling")).unwrap();
        fs::write(dir.path().join("shot.PNG"), "").unwrap();
        let index = EntryIndex::default();
        assert_eq!(search(&index, &root, "../si"), ["../sibling"]);
        let params = SearchEntriesParams {
            project_id: "p".into(),
            query: "../".into(),
            limit: None,
            kind: None,
            images_only: Some(true),
        };
        let images = index.search(root.to_str().unwrap(), &params).unwrap();
        let paths: Vec<_> = images.entries.into_iter().map(|e| e.path).collect();
        assert_eq!(paths, ["../shot.PNG"]);
    }
}
