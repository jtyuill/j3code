//! `projects.listEntries`, `projects.readFile` and `projects.browse`
//! (wire.md §5.3): one directory level of a project, the text of one file,
//! and the directories under the user's home matching a typed path, read on
//! the server host.
//!
//! All are blocking filesystem work; callers run them on `spawn_blocking`.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read as _, Write as _},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use j3_protocol::v2::envelope::Error;
use j3_protocol::v2::methods::{
    BrowseEntry, BrowseResult, EntriesResult, Entry, EntryKind, ReadFileResult,
};

/// Most entries one listing returns.
pub const MAX_ENTRIES: usize = 2000;
/// Most bytes of a file one read returns.
pub const MAX_READ_BYTES: u64 = 1 << 20;
/// A NUL byte this early marks a file as binary.
const BINARY_SNIFF_BYTES: usize = 8 << 10;

/// The entries of `dir` (relative to the project directory `root`):
/// directories first, then by case-insensitive name, without `.git`, at most
/// [`MAX_ENTRIES`]. Entries git ignores are flagged when `root` is in a git
/// work tree.
pub fn list_entries(root: &str, dir: Option<&str>) -> Result<EntriesResult, Error> {
    let canonical_root = fs::canonicalize(root).map_err(|error| {
        Error::rejected(format!("The project folder {root} can't be read: {error}"))
    })?;
    if !canonical_root.is_dir() {
        return Err(Error::rejected(format!(
            "The project folder {root} is not a directory."
        )));
    }
    let requested = dir.unwrap_or_default().trim();
    let prefix = workspace_relative(requested)
        .ok_or_else(|| Error::rejected(format!("{requested} is outside the project folder.")))?;
    let listed_dir = if prefix.is_empty() {
        canonical_root.clone()
    } else {
        fs::canonicalize(canonical_root.join(&prefix))
            .map_err(|error| Error::rejected(format!("{prefix} can't be listed: {error}")))?
    };
    if !listed_dir.starts_with(&canonical_root) {
        return Err(Error::rejected(format!(
            "{prefix} resolves outside the project folder: {}",
            listed_dir.display()
        )));
    }
    let read = fs::read_dir(&listed_dir)
        .map_err(|error| Error::rejected(format!("{prefix} can't be listed: {error}")))?;

    let mut listed: Vec<(String, bool)> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            // Names that are not UTF-8 cannot be addressed over the wire.
            let name = entry.file_name().into_string().ok()?;
            if name == ".git" {
                return None;
            }
            Some((name, is_dir(&entry)))
        })
        .collect();
    listed.sort_by_cached_key(|(name, directory)| (!directory, name.to_lowercase(), name.clone()));
    let truncated = listed.len() > MAX_ENTRIES;
    listed.truncate(MAX_ENTRIES);

    let ignored = git_ignored(&listed_dir, listed.iter().map(|(name, _)| name.as_str()));
    let entries = listed
        .into_iter()
        .map(|(name, directory)| Entry {
            ignored: ignored.contains(&name).then_some(true),
            path: if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            },
            kind: if directory {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
        })
        .collect();
    Ok(EntriesResult { entries, truncated })
}

/// Whether a directory entry is a directory; a symlink counts as what it
/// points at (a file when broken).
fn is_dir(entry: &fs::DirEntry) -> bool {
    match entry.file_type() {
        Ok(kind) if kind.is_symlink() => entry.path().is_dir(),
        Ok(kind) => kind.is_dir(),
        Err(_) => false,
    }
}

/// The text of `path`: relative to the project directory `root` (and
/// resolving inside it), or an absolute host path. At most
/// [`MAX_READ_BYTES`]; `size` is the whole file's. A missing file is
/// `exists: false` with `allow_missing`, else `not_found`.
pub fn read_file(root: &str, path: &str, allow_missing: bool) -> Result<ReadFileResult, Error> {
    let requested = path.trim();
    let missing = |path: String| {
        if allow_missing {
            Ok(ReadFileResult {
                path,
                text: String::new(),
                size: 0,
                truncated: false,
                exists: false,
            })
        } else {
            Err(Error::not_found("file", &path))
        }
    };
    let unreadable =
        |cause: io::Error| Error::rejected(format!("{requested} can't be read: {cause}"));

    // The lexical target, the path answered with, and the root it must stay in.
    let (target, relative_path, canonical_root) = if Path::new(requested).is_absolute() {
        (PathBuf::from(requested), requested.to_owned(), None)
    } else {
        let canonical_root = match fs::canonicalize(root) {
            Ok(canonical_root) => canonical_root,
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => {
                return missing(requested.to_owned());
            }
            Err(cause) => {
                return Err(Error::rejected(format!(
                    "The project folder {root} can't be read: {cause}"
                )));
            }
        };
        let relative = workspace_relative(requested)
            .filter(|relative| !relative.is_empty())
            .ok_or_else(|| {
                Error::rejected(format!("{requested} is outside the project folder."))
            })?;
        (
            canonical_root.join(&relative),
            relative,
            Some(canonical_root),
        )
    };
    let resolved = match fs::canonicalize(&target) {
        Ok(resolved) => resolved,
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => return missing(relative_path),
        Err(cause) => return Err(unreadable(cause)),
    };
    if let Some(canonical_root) = &canonical_root
        && !resolved.starts_with(canonical_root)
    {
        return Err(Error::rejected(format!(
            "{requested} resolves outside the project folder: {}",
            resolved.display()
        )));
    }

    let file = File::open(&resolved).map_err(unreadable)?;
    let metadata = file.metadata().map_err(unreadable)?;
    if !metadata.is_file() {
        return Err(Error::rejected(format!("{requested} is not a file.")));
    }
    let size = metadata.len();
    let mut bytes = Vec::with_capacity(size.min(MAX_READ_BYTES) as usize);
    file.take(MAX_READ_BYTES)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return Err(Error::rejected(format!(
            "{requested} is binary and can't be shown as text."
        )));
    }
    let text = String::from_utf8(bytes)
        .unwrap_or_else(|invalid| String::from_utf8_lossy(invalid.as_bytes()).into_owned());
    Ok(ReadFileResult {
        path: relative_path,
        text,
        size,
        truncated: size > MAX_READ_BYTES,
        exists: true,
    })
}

/// `projects.browse`: the directories in the folder `partial_path` names
/// (`~` is `home`, relative paths are under `home`), sorted by
/// case-insensitive name. A path not ending in `/` that is not a directory
/// lists its parent, so the client filters by the typed name.
pub fn browse(home: &str, partial_path: &str) -> Result<BrowseResult, Error> {
    let partial = partial_path.trim();
    let mut dir = if partial.is_empty() {
        PathBuf::from(home)
    } else {
        Path::new(home).join(expand_home(partial, home))
    };
    if !partial.ends_with('/')
        && !dir.is_dir()
        && let Some(parent) = dir.parent()
    {
        dir = parent.to_owned();
    }
    let read = fs::read_dir(&dir)
        .map_err(|error| Error::rejected(format!("{} can't be listed: {error}", dir.display())))?;
    let mut entries: Vec<BrowseEntry> = read
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .map(|entry| BrowseEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry.path().to_string_lossy().into_owned(),
        })
        .collect();
    entries.sort_by_cached_key(|entry| entry.name.to_lowercase());
    Ok(BrowseResult {
        parent_path: dir.to_string_lossy().into_owned(),
        entries,
    })
}

/// `path` with a leading `~` replaced by `home`.
fn expand_home(path: &str, home: &str) -> PathBuf {
    if path == "~" {
        PathBuf::from(home)
    } else if let Some(rest) = path.strip_prefix("~/") {
        Path::new(home).join(rest)
    } else {
        PathBuf::from(path)
    }
}

/// Why [`image_file`] refused a path.
#[derive(Debug, PartialEq, Eq)]
pub enum ImageFileError {
    /// A relative path that climbs out of the workspace root.
    OutsideRoot,
    /// Not an image the clients render, or larger than an upload.
    NotImage,
    /// Nothing (or no regular file) at the resolved path.
    NotFound(PathBuf),
}

/// The host image file `requested` names, as `workspace-file` assets see
/// it: relative to `root` (and resolving inside it), or absolute like
/// `projects.readFile`. Only GIF, JPEG, PNG, WebP and SVG files up to the
/// image upload limit qualify; returns the path and its MIME type.
pub fn image_file(root: &str, requested: &str) -> Result<(PathBuf, &'static str), ImageFileError> {
    let requested = requested.trim();
    let extension = Path::new(requested)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let mime_type = match extension.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        _ => return Err(ImageFileError::NotImage),
    };
    let (target, root) = if Path::new(requested).is_absolute() {
        (PathBuf::from(requested), None)
    } else {
        let relative = workspace_relative(requested)
            .filter(|relative| !relative.is_empty())
            .ok_or(ImageFileError::OutsideRoot)?;
        let root = fs::canonicalize(root).map_err(|_| ImageFileError::NotFound(root.into()))?;
        (root.join(relative), Some(root))
    };
    let resolved = fs::canonicalize(&target).map_err(|_| ImageFileError::NotFound(target))?;
    if root.is_some_and(|root| !resolved.starts_with(root)) {
        return Err(ImageFileError::OutsideRoot);
    }
    match fs::metadata(&resolved) {
        Ok(metadata) if metadata.is_file() => {
            if metadata.len() > crate::attachments::MAX_IMAGE_BYTES {
                return Err(ImageFileError::NotImage);
            }
            Ok((resolved, mime_type))
        }
        _ => Err(ImageFileError::NotFound(resolved)),
    }
}

/// `path` normalized to `/`-joined components under the workspace root
/// (empty for the root itself); `None` when it is absolute, climbs out of
/// the root, or is not UTF-8.
pub(crate) fn workspace_relative(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

/// Which of `names` (entries of `dir`) git ignores, from one
/// `git check-ignore`. Empty outside a git work tree or without git.
fn git_ignored<'a>(dir: &Path, names: impl Iterator<Item = &'a str>) -> HashSet<String> {
    let mut stdin = Vec::new();
    for name in names {
        stdin.extend_from_slice(name.as_bytes());
        stdin.push(0);
    }
    if stdin.is_empty() {
        return HashSet::new();
    }
    let child = Command::new("git")
        .args(["check-ignore", "--stdin", "-z"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return HashSet::new();
    };
    // Write from another thread: git answers while it reads, so a large
    // listing would otherwise fill both pipes.
    let writer = child.stdin.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let _ = pipe.write_all(&stdin);
        })
    });
    let output = child.wait_with_output();
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    // Exit 0: some paths are ignored; 1: none; 128: not a work tree.
    match output {
        Ok(output) if output.status.success() => output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .filter_map(|name| std::str::from_utf8(name).ok())
            .map(str::to_owned)
            .collect(),
        _ => HashSet::new(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use j3_protocol::v2::envelope::ErrorCode;

    #[test]
    fn workspace_relative_normalizes_and_refuses_escapes() {
        assert_eq!(workspace_relative("").as_deref(), Some(""));
        assert_eq!(
            workspace_relative("./src/../crates//a/").as_deref(),
            Some("crates/a")
        );
        assert_eq!(workspace_relative("a/.."), Some(String::new()));
        assert_eq!(workspace_relative(".."), None);
        assert_eq!(workspace_relative("a/../../b"), None);
        assert_eq!(workspace_relative("/etc"), None);
    }

    /// A fresh directory, removed on drop.
    pub(crate) struct TempDir(PathBuf);

    impl TempDir {
        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) fn project() -> TempDir {
        use std::sync::atomic::{AtomicU32, Ordering};
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("j3-files-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(fs::canonicalize(dir).unwrap())
    }

    fn write(root: &Path, path: &str, contents: &[u8]) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn code(error: &Error) -> ErrorCode {
        error.code
    }

    #[test]
    fn list_entries_sorts_dirs_first_and_flags_ignored() {
        let dir = project();
        let root = dir.path();
        let git = Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(root)
            .status();
        assert!(git.unwrap().success());
        write(root, ".gitignore", b"build/\n*.log\n");
        write(root, "b.txt", b"b");
        write(root, "A.txt", b"a");
        write(root, "debug.log", b"");
        write(root, "build/out", b"");
        write(root, "src/main.rs", b"");
        let listed = list_entries(root.to_str().unwrap(), None).unwrap();
        let paths: Vec<_> = listed.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            ["build", "src", ".gitignore", "A.txt", "b.txt", "debug.log"]
        );
        assert!(!listed.truncated);
        let ignored: Vec<_> = listed
            .entries
            .iter()
            .filter(|e| e.ignored == Some(true))
            .map(|e| e.path.as_str())
            .collect();
        assert_eq!(ignored, ["build", "debug.log"]);
        assert_eq!(listed.entries[0].kind, EntryKind::Dir);
        assert_eq!(listed.entries[2].kind, EntryKind::File);

        let nested = list_entries(root.to_str().unwrap(), Some("src")).unwrap();
        assert_eq!(nested.entries[0].path, "src/main.rs");
        let escape = list_entries(root.to_str().unwrap(), Some("../")).unwrap_err();
        assert_eq!(code(&escape), ErrorCode::Rejected);
        let missing = list_entries(root.to_str().unwrap(), Some("nope")).unwrap_err();
        assert_eq!(code(&missing), ErrorCode::Rejected);
    }

    #[test]
    fn list_entries_truncates() {
        let dir = project();
        for n in 0..=MAX_ENTRIES {
            write(dir.path(), &format!("f{n:05}"), b"");
        }
        let listed = list_entries(dir.path().to_str().unwrap(), None).unwrap();
        assert!(listed.truncated);
        assert_eq!(listed.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn read_file_reads_truncates_and_refuses() {
        let dir = project();
        let root = dir.path();
        let root_str = root.to_str().unwrap();
        write(root, "src/a.txt", b"hello");
        let read = read_file(root_str, "./src/a.txt", false).unwrap();
        assert_eq!(
            read,
            ReadFileResult {
                path: "src/a.txt".into(),
                text: "hello".into(),
                size: 5,
                truncated: false,
                exists: true,
            }
        );
        // An absolute host path is read as is.
        let absolute = root.join("src/a.txt");
        let read = read_file(root_str, absolute.to_str().unwrap(), false).unwrap();
        assert_eq!(
            (read.path.as_str(), read.text.as_str()),
            (absolute.to_str().unwrap(), "hello")
        );

        write(root, "big.txt", &vec![b'x'; MAX_READ_BYTES as usize + 10]);
        let big = read_file(root_str, "big.txt", false).unwrap();
        assert!(big.truncated);
        assert_eq!(big.size, MAX_READ_BYTES + 10);
        assert_eq!(big.text.len(), MAX_READ_BYTES as usize);

        write(root, "bin", b"ab\0cd");
        assert_eq!(
            code(&read_file(root_str, "bin", false).unwrap_err()),
            ErrorCode::Rejected
        );

        let missing = read_file(root_str, "gone.json", false).unwrap_err();
        assert_eq!(code(&missing), ErrorCode::NotFound);
        let allowed = read_file(root_str, "gone.json", true).unwrap();
        assert!(!allowed.exists);
        assert_eq!(allowed.path, "gone.json");

        let escape = read_file(root_str, "../etc/passwd", false).unwrap_err();
        assert_eq!(code(&escape), ErrorCode::Rejected);
        let outside = project();
        write(outside.path(), "secret", b"s");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path().join("secret"), root.join("link")).unwrap();
            let escape = read_file(root_str, "link", false).unwrap_err();
            assert_eq!(code(&escape), ErrorCode::Rejected);
        }
        assert_eq!(
            code(&read_file(root_str, "src", false).unwrap_err()),
            ErrorCode::Rejected
        );
    }

    #[test]
    fn browse_lists_directories_of_the_typed_path() {
        let dir = project();
        let home = dir.path().to_str().unwrap();
        for path in ["Code/b", "Code/a", "code2", "Docs"] {
            fs::create_dir_all(dir.path().join(path)).unwrap();
        }
        write(dir.path(), "Code/file", b"");
        let names = |result: BrowseResult| -> Vec<String> {
            result.entries.into_iter().map(|e| e.name).collect()
        };
        // A partial name lists its parent; the client filters by prefix.
        let partial = browse(home, "~/Cod").unwrap();
        assert_eq!(partial.parent_path, home);
        assert_eq!(names(partial), ["Code", "code2", "Docs"]);
        let inside = browse(home, "Code/").unwrap();
        assert_eq!(inside.parent_path, format!("{home}/Code/"));
        assert_eq!(
            inside.entries[0].path,
            dir.path().join("Code/a").to_str().unwrap()
        );
        assert_eq!(names(inside), ["a", "b"]);
        assert_eq!(names(browse(home, "").unwrap()), ["Code", "code2", "Docs"]);
        let gone = browse(home, "~/missing/deeper/").unwrap_err();
        assert_eq!(code(&gone), ErrorCode::Rejected);
    }
}
