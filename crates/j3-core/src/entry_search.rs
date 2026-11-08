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
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use ignore::WalkBuilder;
use j3_protocol::t3::project::{
    ProjectEntriesFailure, ProjectEntry, ProjectEntryKind, ProjectSearchEntriesError,
    ProjectSearchEntriesInput, ProjectSearchEntriesResult,
};

use crate::files::workspace_relative;

/// Most entries one workspace walk keeps.
pub const MAX_INDEXED: usize = 25_000;
/// Most entries one search returns (`PROJECT_SEARCH_ENTRIES_MAX_LIMIT`).
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
    /// The entries `input.query` names under `input.cwd` (`home` expands `~`):
    /// best matches first, at most `input.limit`.
    pub fn search(
        &self,
        input: &ProjectSearchEntriesInput,
        home: &str,
    ) -> Result<ProjectSearchEntriesResult, Box<ProjectSearchEntriesError>> {
        let fail = |failure, message: String| {
            let mut error = Box::new(ProjectSearchEntriesError::new(message));
            error.cwd = Some(input.cwd.clone());
            error.limit = Some(input.limit);
            error.failure = Some(failure);
            error
        };
        let root = fs::canonicalize(&input.cwd).map_err(|error| {
            let failure = if error.kind() == io::ErrorKind::NotFound {
                ProjectEntriesFailure::WorkspaceRootNotFound
            } else {
                ProjectEntriesFailure::WorkspaceRootStatFailed
            };
            fail(failure, format!("{}: {error}", input.cwd))
        })?;
        if !root.is_dir() {
            return Err(fail(
                ProjectEntriesFailure::WorkspaceRootNotDirectory,
                format!("{} is not a directory", input.cwd),
            ));
        }
        let limit = (input.limit as usize).clamp(1, MAX_LIMIT);
        // `~` and `..` alone name a directory to list, as `~/` and `../` do.
        let query = match input.query.trim() {
            bare @ ("~" | "..") => format!("{bare}/"),
            query => query.to_owned(),
        };
        let query = query.as_str();
        let wanted = |found: &Found| match input.kind {
            Some(ProjectEntryKind::File) if found.directory => false,
            Some(ProjectEntryKind::Directory) if !found.directory => false,
            _ => input.image_only != Some(true) || (!found.directory && is_image(&found.path)),
        };

        let (dir_part, rest) = query.rsplit_once('/').unwrap_or(("", query));
        let (found, walk_truncated) = if leaves_workspace(query) {
            let shown = &query[..query.len() - rest.len()];
            (list_dir(&expand(shown, &root, home), shown, rest), false)
        } else if query.is_empty() {
            (list_dir(&root, "", ""), false)
        } else if let Some(scope) = workspace_relative(dir_part)
            .filter(|scope| !dir_part.is_empty() && !scope.is_empty() && root.join(scope).is_dir())
        {
            if rest.is_empty() {
                (
                    list_dir(&root.join(&scope), &format!("{scope}/"), ""),
                    false,
                )
            } else {
                let walk = self.walk(&root);
                let under = format!("{scope}/");
                (fuzzy(&walk, &under, &rest.to_lowercase()), walk.truncated)
            }
        } else {
            let walk = self.walk(&root);
            (fuzzy(&walk, "", &query.to_lowercase()), walk.truncated)
        };

        let mut entries = Vec::with_capacity(limit);
        let mut matched = 0;
        for found in found.into_iter().filter(wanted) {
            matched += 1;
            if entries.len() < limit {
                entries.push(ProjectEntry {
                    path: found.path,
                    kind: if found.directory {
                        ProjectEntryKind::Directory
                    } else {
                        ProjectEntryKind::File
                    },
                    ignored: None,
                });
            }
        }
        Ok(ProjectSearchEntriesResult {
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
/// `/`) names.
fn expand(shown: &str, root: &Path, home: &str) -> PathBuf {
    match shown.strip_prefix("~/") {
        Some(rest) => Path::new(home).join(rest),
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
