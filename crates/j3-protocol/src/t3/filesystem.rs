//! Host filesystem RPCs (`C/filesystem.ts`): `filesystem.browse`
//! (ios-surface M13) and `filesystem.getMetadata` (M21).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `FilesystemGetMetadataInput` (`C/filesystem.ts:8-14`): 1..=64 absolute
/// paths, each at most 512 chars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemGetMetadataInput {
    pub paths: Vec<String>,
}

/// `FilesystemEntryMetadata.kind` (`C/filesystem.ts:17`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilesystemEntryKind {
    #[serde(rename = "file")]
    File,
    #[serde(rename = "directory")]
    Directory,
    #[serde(rename = "other")]
    Other,
}

/// `FilesystemEntryMetadata` (`C/filesystem.ts:16-20`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemEntryMetadata {
    pub kind: FilesystemEntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// `FilesystemGetMetadataResult` (`C/filesystem.ts:24-26`): one entry per
/// requested path, in order; `null` for missing/unreadable/unsupported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemGetMetadataResult {
    pub entries: Vec<Option<FilesystemEntryMetadata>>,
}

/// `FilesystemBrowseInput` (`C/filesystem.ts:29-32`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemBrowseInput {
    pub partial_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `FilesystemBrowseEntry` (`C/filesystem.ts:35-38`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemBrowseEntry {
    pub name: String,
    pub full_path: String,
}

/// `FilesystemBrowseResult` (`C/filesystem.ts:41-44`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemBrowseResult {
    pub parent_path: String,
    pub entries: Vec<FilesystemBrowseEntry>,
}

/// `FilesystemBrowseFailure` (`C/filesystem.ts:47-51`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilesystemBrowseFailure {
    #[serde(rename = "windows_path_unsupported")]
    WindowsPathUnsupported,
    #[serde(rename = "current_project_required")]
    CurrentProjectRequired,
    #[serde(rename = "read_directory_failed")]
    ReadDirectoryFailed,
}

json_literal! {
    /// `_tag` of [`FilesystemBrowseError`].
    pub struct FilesystemBrowseErrorTag = str("FilesystemBrowseError");
}

/// `FilesystemBrowseError` (`C/filesystem.ts:58-88`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesystemBrowseError {
    #[serde(rename = "_tag")]
    pub tag: FilesystemBrowseErrorTag,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FilesystemBrowseFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

impl FilesystemBrowseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            tag: FilesystemBrowseErrorTag,
            partial_path: None,
            cwd: None,
            failure: None,
            parent_path: None,
            platform: None,
            message: message.into(),
            cause: None,
        }
    }
}
