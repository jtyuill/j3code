//! t3code wire shapes (`packages/contracts/src` @ 8760d063) used by the iOS
//! thread surface (`docs/protocol/ios-surface.md`).
//!
//! Conventions, matching the contracts' Effect schemas:
//! - `Schema.optional` / `optionalKey` → `Option` that is omitted when `None`.
//! - `Schema.NullOr` → `Option` that is always serialized (`null` when `None`).
//! - `withDecodingDefault` → `#[serde(default)]`, always serialized.
//! - Literal unions → enums with the exact strings; tagged unions use their
//!   real discriminator. Forward-compatible unions keep unknown variants as
//!   `Unknown(serde_json::Value)` and re-encode them unchanged.
//! - Branded ids are plain strings ([`ids`]); timestamps are ISO-8601 UTC
//!   strings with milliseconds ([`now_iso`], [`iso_from_unix_ms`]).
//! - Fields of features j3 cuts (pull requests, guest share, subagent
//!   workflows, ...) keep their exact key but are typed as opaque JSON.

#[macro_use]
mod macros;

pub mod assets;
pub mod auth;
pub mod background;
pub mod chat_attachment;
pub mod commands;
pub mod environment;
pub mod environment_http;
pub mod filesystem;
pub mod ids;
pub mod methods;
pub mod model;
pub mod orchestration;
pub mod project;
pub mod server;
pub mod thread_search;

use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, PrimitiveDateTime};

const ISO_MILLIS: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

fn format_iso(at: OffsetDateTime) -> String {
    at.format(ISO_MILLIS)
        .expect("UTC datetimes always format with a fixed description")
}

/// Current time as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn now_iso() -> String {
    format_iso(OffsetDateTime::now_utc())
}

/// Epoch milliseconds as `YYYY-MM-DDTHH:MM:SS.mmmZ`. Values outside the
/// supported range (years ±9999) clamp to its ends.
pub fn iso_from_unix_ms(ms: i64) -> String {
    let at = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).unwrap_or_else(
        |_| {
            if ms < 0 {
                PrimitiveDateTime::MIN.assume_utc()
            } else {
                PrimitiveDateTime::MAX.assume_utc()
            }
        },
    );
    format_iso(at)
}

macros::json_literal! {
    /// `Schema.Literal(1)`, e.g. `version: 1`.
    pub struct One = int(1);
}

macros::json_literal! {
    /// `Schema.Literal(true)`, e.g. `detailInTurnItem: true`.
    pub struct True = bool(true);
}

/// `Schema.optional(Schema.NullOr(X))`: absent (`None`), `null`
/// (`Some(None)`) and a value (`Some(Some(x))`) all round-trip. Use with
/// `#[serde(default, skip_serializing_if = "Option::is_none", with = "double_option")]`.
pub mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<Option<T>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(deserializer).map(Some)
    }
}
