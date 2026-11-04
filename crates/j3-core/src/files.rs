//! `projects.listEntries` and `projects.readFile`: one directory level of a
//! project and the text of one file, read on the server host. The desktop's
//! file pane browses with them; the iOS app reads an optional `t3.json`.
//!
//! Both are blocking filesystem work; callers run them on `spawn_blocking`.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read as _, Write as _},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

use j3_protocol::t3::project::{
    ProjectEntriesFailure, ProjectEntry, ProjectEntryKind, ProjectFileFailure,
    ProjectFileOperation, ProjectListEntriesError, ProjectListEntriesInput,
    ProjectListEntriesResult, ProjectReadFileError, ProjectReadFileInput, ProjectReadFileResult,
};

/// Most entries one listing returns.
pub const MAX_ENTRIES: usize = 2000;
/// Most bytes of a file one read returns.
pub const MAX_READ_BYTES: u64 = 1 << 20;
/// A NUL byte this early marks a file as binary.
const BINARY_SNIFF_BYTES: usize = 8 << 10;

/// The entries of `directoryPath` (relative to `cwd`): directories first,
/// then by case-insensitive name, without `.git`, at most [`MAX_ENTRIES`].
/// Entries git ignores are flagged when `cwd` is in a git work tree.
pub fn list_entries(
    input: &ProjectListEntriesInput,
) -> Result<ProjectListEntriesResult, Box<ProjectListEntriesError>> {
    let fail = |failure, message: String| {
        let mut error = Box::new(ProjectListEntriesError::new(message));
        error.cwd = Some(input.cwd.clone());
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
    let requested = input.directory_path.as_deref().unwrap_or_default().trim();
    let list_failed = |message: String| fail(ProjectEntriesFailure::DirectoryListFailed, message);
    let prefix = workspace_relative(requested)
        .ok_or_else(|| list_failed(format!("{requested} is outside the workspace")))?;
    let dir = if prefix.is_empty() {
        root.clone()
    } else {
        fs::canonicalize(root.join(&prefix))
            .map_err(|error| list_failed(format!("{prefix}: {error}")))?
    };
    if !dir.starts_with(&root) {
        return Err(list_failed(format!(
            "{prefix} resolves outside the workspace: {}",
            dir.display()
        )));
    }
    let read = fs::read_dir(&dir).map_err(|error| list_failed(format!("{prefix}: {error}")))?;

    let mut listed: Vec<(String, bool)> = read
        .filter_map(Result::ok)
        .filter_map(|entry| {
            // Names that are not UTF-8 cannot be addressed over the wire.
            let name = entry.file_name().into_string().ok()?;
            if name == ".git" {
                return None;
            }
            let directory = match entry.file_type() {
                // A symlink lists as what it points at (a file when broken).
                Ok(kind) if kind.is_symlink() => entry.path().is_dir(),
                Ok(kind) => kind.is_dir(),
                Err(_) => false,
            };
            Some((name, directory))
        })
        .collect();
    listed.sort_by_cached_key(|(name, directory)| (!directory, name.to_lowercase(), name.clone()));
    let truncated = listed.len() > MAX_ENTRIES;
    listed.truncate(MAX_ENTRIES);

    let ignored = git_ignored(&dir, listed.iter().map(|(name, _)| name.as_str()));
    let entries = listed
        .into_iter()
        .map(|(name, directory)| ProjectEntry {
            ignored: ignored.contains(&name).then_some(true),
            path: if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            },
            kind: if directory {
                ProjectEntryKind::Directory
            } else {
                ProjectEntryKind::File
            },
        })
        .collect();
    Ok(ProjectListEntriesResult { entries, truncated })
}

/// The text of `relativePath`: relative to `cwd` (and resolving inside it),
/// or an absolute host path. At most [`MAX_READ_BYTES`]; `byteLength` is the
/// whole file's size. A missing file with `allowMissing` is
/// `exists: false`.
pub fn read_file(
    input: &ProjectReadFileInput,
) -> Result<ProjectReadFileResult, Box<ProjectReadFileError>> {
    let requested = input.relative_path.trim();
    let cwd = &input.cwd;
    let allow_missing = input.allow_missing == Some(true);
    let error = |failure, message: String| {
        let mut error = Box::new(ProjectReadFileError::new(message));
        error.cwd = Some(cwd.clone());
        error.relative_path = Some(requested.to_owned());
        error.failure = Some(failure);
        error
    };
    let operation_failed = |operation, path: &Path, cause: io::Error| {
        let mut error = error(
            ProjectFileFailure::OperationFailed,
            format!("Failed to read workspace file '{requested}' in '{cwd}': {cause}"),
        );
        error.operation = Some(operation);
        error.operation_path = Some(path.to_string_lossy().into_owned());
        error
    };
    let missing = |relative_path: String| ProjectReadFileResult {
        relative_path,
        contents: String::new(),
        byte_length: 0,
        truncated: false,
        exists: Some(false),
    };

    // The lexical target, the path answered with, and the root it must stay in.
    let (target, relative_path, root) = if Path::new(requested).is_absolute() {
        (PathBuf::from(requested), requested.to_owned(), None)
    } else {
        let root = match fs::canonicalize(cwd) {
            Ok(root) => root,
            Err(cause) if allow_missing && cause.kind() == io::ErrorKind::NotFound => {
                return Ok(missing(requested.to_owned()));
            }
            Err(cause) => {
                return Err(operation_failed(
                    ProjectFileOperation::RealpathWorkspaceRoot,
                    Path::new(cwd),
                    cause,
                ));
            }
        };
        let relative = workspace_relative(requested)
            .filter(|relative| !relative.is_empty())
            .ok_or_else(|| {
                error(
                    ProjectFileFailure::WorkspacePathOutsideRoot,
                    format!("Workspace file '{requested}' is outside workspace root '{cwd}'."),
                )
            })?;
        (root.join(&relative), relative, Some(root))
    };
    let resolved = match fs::canonicalize(&target) {
        Ok(resolved) => resolved,
        Err(cause) if allow_missing && cause.kind() == io::ErrorKind::NotFound => {
            return Ok(missing(relative_path));
        }
        Err(cause) => {
            return Err(operation_failed(
                ProjectFileOperation::RealpathTarget,
                &target,
                cause,
            ));
        }
    };
    if let Some(root) = &root
        && !resolved.starts_with(root)
    {
        let mut error = error(
            ProjectFileFailure::ResolvedPathOutsideRoot,
            format!(
                "Workspace file '{requested}' resolves outside workspace root '{cwd}': {}",
                resolved.display()
            ),
        );
        error.resolved_path = Some(resolved.to_string_lossy().into_owned());
        error.resolved_workspace_root = Some(root.to_string_lossy().into_owned());
        return Err(error);
    }
    let with_resolved = |mut error: Box<ProjectReadFileError>| {
        error.resolved_path = Some(resolved.to_string_lossy().into_owned());
        error
    };

    let file = File::open(&resolved)
        .map_err(|cause| operation_failed(ProjectFileOperation::Open, &resolved, cause))?;
    let metadata = file
        .metadata()
        .map_err(|cause| operation_failed(ProjectFileOperation::Stat, &resolved, cause))?;
    if !metadata.is_file() {
        return Err(with_resolved(error(
            ProjectFileFailure::PathNotFile,
            format!("Workspace path '{requested}' in '{cwd}' is not a file."),
        )));
    }
    let byte_length = metadata.len();
    let mut bytes = Vec::with_capacity(byte_length.min(MAX_READ_BYTES) as usize);
    file.take(MAX_READ_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|cause| operation_failed(ProjectFileOperation::Read, &resolved, cause))?;
    if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return Err(with_resolved(error(
            ProjectFileFailure::BinaryFile,
            format!(
                "Workspace file '{requested}' in '{cwd}' is binary and cannot be previewed as text."
            ),
        )));
    }
    let contents = String::from_utf8(bytes)
        .unwrap_or_else(|invalid| String::from_utf8_lossy(invalid.as_bytes()).into_owned());
    Ok(ProjectReadFileResult {
        relative_path,
        contents,
        byte_length,
        truncated: byte_length > MAX_READ_BYTES,
        exists: Some(true),
    })
}

/// Why [`image_file`] refused a path.
#[derive(Debug, PartialEq, Eq)]
pub enum ImageFileError {
    /// A relative path that climbs out of the workspace root.
    OutsideRoot,
    /// Not a raster image the clients render, or larger than an upload.
    NotImage,
    /// Nothing (or no regular file) at the resolved path.
    NotFound(PathBuf),
}

/// The host image file `requested` names, as `workspace-file` assets see
/// it: relative to `root` (and resolving inside it), or absolute like
/// `projects.readFile`. Only GIF, JPEG, PNG and WebP files up to the image
/// upload limit qualify; returns the path and its MIME type.
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
fn workspace_relative(path: &str) -> Option<String> {
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
mod tests {
    use super::*;

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
}
