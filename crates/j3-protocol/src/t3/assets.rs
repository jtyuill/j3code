//! Asset URLs and attachment uploads (`C/assets.ts`):
//! `attachments.createUploadUrl` (ios-surface M10), `assets.createUrl` (M11)
//! and `attachments.delete` (M15).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ids::{ThreadId, TurnItemId};

/// `ToolActivityNativeAppReference` (`C/providerRuntime.ts:83-93`), tagged by `_tag`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum ToolActivityNativeAppReference {
    #[serde(rename = "app-id", rename_all = "camelCase")]
    AppId { app_id: String },
    #[serde(rename = "display-name", rename_all = "camelCase")]
    DisplayName { display_name: String },
}

/// `disposition` of the `attachment` resource (`C/assets.ts:39`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetDisposition {
    #[serde(rename = "inline")]
    Inline,
    #[serde(rename = "attachment")]
    Attachment,
}

json_literal! {
    /// `Schema.Literal("attachment")` (`media-file` disposition, `C/assets.ts:28`).
    pub struct AttachmentDisposition = str("attachment");
}

json_literal! {
    /// `Schema.Literal("mcp-app")` (`C/assets.ts:40`).
    pub struct McpAppRenderIntent = str("mcp-app");
}

/// `AssetResource` (`C/assets.ts:20-62`), tagged by `_tag`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum AssetResource {
    #[serde(rename = "workspace-file", rename_all = "camelCase")]
    WorkspaceFile { thread_id: ThreadId, path: String },
    #[serde(rename = "media-file", rename_all = "camelCase")]
    MediaFile {
        thread_id: ThreadId,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disposition: Option<AttachmentDisposition>,
    },
    #[serde(rename = "draft-workspace-file", rename_all = "camelCase")]
    DraftWorkspaceFile { cwd: String, path: String },
    #[serde(rename = "attachment", rename_all = "camelCase")]
    Attachment {
        /// At most 256 chars.
        attachment_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        disposition: Option<AssetDisposition>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        render_intent: Option<McpAppRenderIntent>,
    },
    #[serde(rename = "tool-output-image", rename_all = "camelCase")]
    ToolOutputImage {
        thread_id: ThreadId,
        item_id: TurnItemId,
        index: u64,
    },
    #[serde(rename = "project-favicon", rename_all = "camelCase")]
    ProjectFavicon {
        cwd: String,
        /// `ProjectFaviconPath`; a cache-key hint only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    #[serde(rename = "native-app-icon", rename_all = "camelCase")]
    NativeAppIcon { app: ToolActivityNativeAppReference },
    /// `SOURCE_CONTROL_MEDIA_ASSET` (`C/assets.ts:18`).
    #[serde(rename = "github-media", rename_all = "camelCase")]
    GithubMedia { cwd: String, url: String },
}

/// `AssetCreateUrlInput` (`C/assets.ts:65-67`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetCreateUrlInput {
    pub resource: AssetResource,
}

/// `AssetImageDimensions` (`C/assets.ts:70-73`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetImageDimensions {
    pub width: u64,
    pub height: u64,
}

/// `AssetCreateUrlResult` (`C/assets.ts:76-85`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetCreateUrlResult {
    pub relative_url: String,
    /// Epoch ms; `Schema.Number` kept as a JSON number so integral values re-encode unchanged.
    pub expires_at: serde_json::Number,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_dimensions: Option<AssetImageDimensions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_revision: Option<String>,
}

json_literal! {
    /// `Schema.Literal("image")`.
    pub struct ImageType = str("image");
}

json_literal! {
    /// `Schema.Literal("file")`.
    pub struct FileType = str("file");
}

/// `ImageAttachmentCreateUploadUrlInput` (`C/assets.ts:90-98`). `type` is
/// `optionalKey`: its presence is kept so re-encoding is faithful.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageAttachmentCreateUploadUrlInput {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ImageType>,
    pub name: String,
    /// One of `PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES`.
    pub mime_type: String,
    pub size_bytes: u64,
}

/// `FileAttachmentCreateUploadUrlInput` (`C/assets.ts:100-108`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachmentCreateUploadUrlInput {
    #[serde(rename = "type")]
    pub kind: FileType,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// `AttachmentCreateUploadUrlInput` (`C/assets.ts:110-113`). A payload
/// without `type` is an image; the `type` literals pick the member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttachmentCreateUploadUrlInput {
    File(FileAttachmentCreateUploadUrlInput),
    Image(ImageAttachmentCreateUploadUrlInput),
}

/// `AttachmentCreateUploadUrlResult` (`C/assets.ts:116-120`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentCreateUploadUrlResult {
    pub attachment_id: String,
    pub relative_url: String,
    /// Epoch ms; `Schema.Number` kept as a JSON number so integral values re-encode unchanged.
    pub expires_at: serde_json::Number,
}

/// `AttachmentDeleteInput` (`C/assets.ts:123-125`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDeleteInput {
    pub attachment_id: String,
}

/// A `Schema.TaggedError` struct `{"_tag": $tag, ...fields}` with a `new`
/// constructor over its required fields; `optional` fields are omitted when `None`.
macro_rules! tagged_error {
    (
        $(#[$meta:meta])*
        $name:ident / $tag_ty:ident = $tag:literal
        { $($field:ident: $fty:ty),* $(,)? }
        $(optional { $($ofield:ident: $oty:ty),* $(,)? })?
    ) => {
        json_literal! {
            #[doc = concat!("`_tag` of [`", stringify!($name), "`].")]
            pub struct $tag_ty = str($tag);
        }

        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            #[serde(rename = "_tag")]
            pub tag: $tag_ty,
            $(pub $field: $fty,)*
            $($(
                #[serde(default, skip_serializing_if = "Option::is_none")]
                pub $ofield: Option<$oty>,
            )*)?
        }

        impl $name {
            #[allow(clippy::new_without_default)]
            pub fn new($($field: $fty),*) -> Self {
                Self {
                    tag: $tag_ty,
                    $($field,)*
                    $($($ofield: None,)*)?
                }
            }
        }
    };
}

tagged_error! {
    /// `AttachmentUploadSigningKeyError` (`C/assets.ts:128-137`); `cause` is a required defect.
    AttachmentUploadSigningKeyError / AttachmentUploadSigningKeyErrorTag = "AttachmentUploadSigningKeyError" {
        cause: Value,
    }
}

tagged_error! {
    /// `AssetWorkspaceContextNotFoundError` (`C/assets.ts:139-148`).
    AssetWorkspaceContextNotFoundError / AssetWorkspaceContextNotFoundErrorTag = "AssetWorkspaceContextNotFoundError" {
        resource: AssetResource,
    }
}

tagged_error! {
    /// `AssetWorkspaceContextResolutionError` (`C/assets.ts:150-160`).
    AssetWorkspaceContextResolutionError / AssetWorkspaceContextResolutionErrorTag = "AssetWorkspaceContextResolutionError" {
        resource: AssetResource,
        cause: Value,
    }
}

tagged_error! {
    /// `AssetWorkspaceRootNormalizationError` (`C/assets.ts:162-172`).
    AssetWorkspaceRootNormalizationError / AssetWorkspaceRootNormalizationErrorTag = "AssetWorkspaceRootNormalizationError" {
        resource: AssetResource,
        cause: Value,
    }
}

tagged_error! {
    /// `AssetWorkspacePathValidationError` (`C/assets.ts:174-184`).
    AssetWorkspacePathValidationError / AssetWorkspacePathValidationErrorTag = "AssetWorkspacePathValidationError" {
        resource: AssetResource,
        cause: Value,
    }
}

tagged_error! {
    /// `AssetPreviewTypeValidationError` (`C/assets.ts:186-197`).
    AssetPreviewTypeValidationError / AssetPreviewTypeValidationErrorTag = "AssetPreviewTypeValidationError" {
        resource: AssetResource,
    }
}

tagged_error! {
    /// `AssetWorkspaceAssetInspectionError` (`C/assets.ts:199-211`).
    AssetWorkspaceAssetInspectionError / AssetWorkspaceAssetInspectionErrorTag = "AssetWorkspaceAssetInspectionError" {
        resource: AssetResource,
        cause: Value,
    }
}

tagged_error! {
    /// `AssetWorkspaceAssetNotFoundError` (`C/assets.ts:213-228`).
    AssetWorkspaceAssetNotFoundError / AssetWorkspaceAssetNotFoundErrorTag = "AssetWorkspaceAssetNotFoundError" {
        resource: AssetResource,
    } optional {
        checked_path: String,
    }
}

tagged_error! {
    /// `AssetWorkspaceResolutionError` (`C/assets.ts:230-240`).
    AssetWorkspaceResolutionError / AssetWorkspaceResolutionErrorTag = "AssetWorkspaceResolutionError" {
        resource: AssetResource,
        cause: Value,
    }
}

tagged_error! {
    /// `AssetAttachmentNotFoundError` (`C/assets.ts:242-251`).
    AssetAttachmentNotFoundError / AssetAttachmentNotFoundErrorTag = "AssetAttachmentNotFoundError" {
        resource: AssetResource,
    }
}

tagged_error! {
    /// `AssetProjectFaviconResolutionError` (`C/assets.ts:253-263`).
    AssetProjectFaviconResolutionError / AssetProjectFaviconResolutionErrorTag = "AssetProjectFaviconResolutionError" {
        resource: AssetResource,
        cause: Value,
    }
}

tagged_error! {
    /// `AssetProjectFaviconInspectionError` (`C/assets.ts:265-275`).
    AssetProjectFaviconInspectionError / AssetProjectFaviconInspectionErrorTag = "AssetProjectFaviconInspectionError" {
        resource: AssetResource,
        cause: Value,
    }
}

tagged_error! {
    /// `AssetProjectFaviconNotFoundError` (`C/assets.ts:277-286`).
    AssetProjectFaviconNotFoundError / AssetProjectFaviconNotFoundErrorTag = "AssetProjectFaviconNotFoundError" {
        resource: AssetResource,
    }
}

tagged_error! {
    /// `AssetSigningKeyLoadError` (`C/assets.ts:288-298`).
    AssetSigningKeyLoadError / AssetSigningKeyLoadErrorTag = "AssetSigningKeyLoadError" {
        resource: AssetResource,
        cause: Value,
    }
}

tagged_error! {
    /// `AssetSourceControlMediaUrlValidationError` (`C/assets.ts:300-308`); its
    /// wire tag predates the rename.
    AssetSourceControlMediaUrlValidationError / AssetSourceControlMediaUrlValidationErrorTag = "AssetGitHubMediaUrlValidationError" {}
}

/// `AssetAccessError` (`C/assets.ts:310-325`); each member's `_tag` literal
/// selects it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssetAccessError {
    WorkspaceContextNotFound(AssetWorkspaceContextNotFoundError),
    WorkspaceContextResolution(AssetWorkspaceContextResolutionError),
    WorkspaceRootNormalization(AssetWorkspaceRootNormalizationError),
    WorkspacePathValidation(AssetWorkspacePathValidationError),
    PreviewTypeValidation(AssetPreviewTypeValidationError),
    WorkspaceAssetInspection(AssetWorkspaceAssetInspectionError),
    WorkspaceAssetNotFound(AssetWorkspaceAssetNotFoundError),
    WorkspaceResolution(AssetWorkspaceResolutionError),
    AttachmentNotFound(AssetAttachmentNotFoundError),
    ProjectFaviconResolution(AssetProjectFaviconResolutionError),
    ProjectFaviconInspection(AssetProjectFaviconInspectionError),
    ProjectFaviconNotFound(AssetProjectFaviconNotFoundError),
    SourceControlMediaUrlValidation(AssetSourceControlMediaUrlValidationError),
    SigningKeyLoad(AssetSigningKeyLoadError),
}
