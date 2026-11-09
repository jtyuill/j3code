//! Wall-clock time as the wire carries it: RFC 3339 UTC strings with
//! millisecond precision (`docs/protocol/wire.md`), and epoch milliseconds
//! for comparisons.

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Now, in epoch milliseconds.
pub fn now_ms() -> i64 {
    (OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

/// Now, as a wire timestamp.
pub fn now_iso() -> String {
    iso(now_ms())
}

/// Epoch milliseconds as a wire timestamp (`2026-11-09T11:18:00.000Z`).
pub fn iso(ms: i64) -> String {
    let at = OffsetDateTime::UNIX_EPOCH + time::Duration::milliseconds(ms);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
        at.millisecond()
    )
}

/// An RFC 3339 timestamp (any offset) in epoch milliseconds; `None` when it
/// does not parse.
pub fn parse_ms(text: &str) -> Option<i64> {
    let at = OffsetDateTime::parse(text.trim(), &Rfc3339).ok()?;
    Some((at.unix_timestamp_nanos() / 1_000_000) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_normalizes() {
        let ms = parse_ms("2026-11-09T11:18:00.123Z").unwrap();
        assert_eq!(iso(ms), "2026-11-09T11:18:00.123Z");
        assert_eq!(
            iso(parse_ms("2026-11-09T12:18:00+01:00").unwrap()),
            "2026-11-09T11:18:00.000Z"
        );
        assert_eq!(parse_ms("yesterday"), None);
        assert_eq!(iso(0), "1970-01-01T00:00:00.000Z");
    }
}
