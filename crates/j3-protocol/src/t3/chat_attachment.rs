//! Chat attachments (`C/chatAttachment.ts:148-205`). Commands and messages
//! carry references only; bytes are uploaded over HTTP first.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::ids::ChatAttachmentId;

/// `ChatImageAttachment`: `mimeType` matches `/^image\//i`, `sizeBytes` ≤ 10 MiB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatImageAttachment {
    pub id: ChatAttachmentId,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// `SnapShotSource` (desktop screen capture metadata); opaque here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}

/// `ChatFileAttachment`: `sizeBytes` 1..=50 MiB.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatFileAttachment {
    pub id: ChatAttachmentId,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    /// `PastedTextAttachmentSource` (`{"_tag":"pasted-text"}`); opaque here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}

/// `ChatUnknownAttachment`: any other `type` (not `image`/`file`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatUnknownAttachment {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: ChatAttachmentId,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

/// `ChatAttachment` union, discriminated by `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatAttachment {
    Image(ChatImageAttachment),
    File(ChatFileAttachment),
    Unknown(ChatUnknownAttachment),
}

impl ChatAttachment {
    pub fn id(&self) -> &str {
        match self {
            Self::Image(a) => &a.id,
            Self::File(a) => &a.id,
            Self::Unknown(a) => &a.id,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum Known<I, F> {
    #[serde(rename = "image")]
    Image(I),
    #[serde(rename = "file")]
    File(F),
}

impl Serialize for ChatAttachment {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Image(a) => Known::<_, &ChatFileAttachment>::Image(a).serialize(serializer),
            Self::File(a) => Known::<&ChatImageAttachment, _>::File(a).serialize(serializer),
            Self::Unknown(a) => a.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ChatAttachment {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let result = match value.get("type").and_then(Value::as_str) {
            Some("image" | "file") => {
                Known::<ChatImageAttachment, ChatFileAttachment>::deserialize(value).map(|known| {
                    match known {
                        Known::Image(a) => Self::Image(a),
                        Known::File(a) => Self::File(a),
                    }
                })
            }
            _ => ChatUnknownAttachment::deserialize(value).map(Self::Unknown),
        };
        result.map_err(serde::de::Error::custom)
    }
}
