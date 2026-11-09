//! `server.usage`: the usage limits of every account omp is logged into on
//! this host, from `omp usage --json` (wire.md §5.1.2).

use j3_protocol::v2::{
    envelope::{Error, ErrorCode},
    methods::{AccountInfo, Usage, UsageAccount, UsageLimit, UsageWindow},
};
use serde::Deserialize;
use time::{
    OffsetDateTime, PrimitiveDateTime, format_description::FormatItem, macros::format_description,
};

use crate::omp::OmpLauncher;

/// `omp usage --json` output; only the fields clients show are read.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    /// Unix ms.
    generated_at: i64,
    #[serde(default)]
    reports: Vec<Report>,
}

/// One account's limits.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    provider: String,
    /// When omp last fetched this report (Unix ms); omp caches reports.
    #[serde(default)]
    fetched_at: Option<i64>,
    #[serde(default)]
    limits: Vec<Limit>,
    #[serde(default)]
    metadata: Account,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Account {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    plan_type: Option<String>,
}

#[derive(Deserialize)]
struct Limit {
    label: String,
    #[serde(default)]
    window: Option<Window>,
    #[serde(default)]
    amount: Amount,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    #[serde(default)]
    label: Option<String>,
    /// Unix ms.
    #[serde(default)]
    resets_at: Option<i64>,
}

/// How much of a limit is used.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Amount {
    /// 0..=1.
    #[serde(default)]
    used_fraction: Option<f64>,
    #[serde(default)]
    used: Option<f64>,
    #[serde(default)]
    limit: Option<f64>,
    #[serde(default)]
    unit: Option<String>,
}

const ISO_MILLIS: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

/// Unix ms as a wire timestamp; out-of-range values clamp.
fn iso(ms: i64) -> String {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .unwrap_or_else(|_| {
            if ms < 0 {
                PrimitiveDateTime::MIN.assume_utc()
            } else {
                PrimitiveDateTime::MAX.assume_utc()
            }
        })
        .format(ISO_MILLIS)
        .expect("UTC datetimes always format with a fixed description")
}

/// `server.usage`: `omp usage --json` on this host (wire.md §5.1.2);
/// `unavailable` when omp fails or prints something else.
pub async fn usage(omp: &OmpLauncher) -> Result<Usage, Error> {
    let stdout = omp
        .usage()
        .await
        .map_err(|error| Error::new(ErrorCode::Unavailable, error.to_string()))?;
    parse(&stdout).map_err(|message| Error::new(ErrorCode::Unavailable, message))
}

/// The usage report of `omp usage --json` output. A limit without
/// `used`/`limit` is reported in percent from its used fraction.
pub fn parse(stdout: &[u8]) -> Result<Usage, String> {
    let snapshot: Snapshot = serde_json::from_slice(stdout)
        .map_err(|error| format!("unexpected omp usage output: {error}"))?;
    Ok(Usage {
        generated_at: iso(snapshot.generated_at),
        accounts: snapshot
            .reports
            .into_iter()
            .map(|report| UsageAccount {
                provider: report.provider,
                account: (report.metadata.email.is_some() || report.metadata.plan_type.is_some())
                    .then_some(AccountInfo {
                        email: report.metadata.email,
                        plan: report.metadata.plan_type,
                    }),
                fetched_at: report.fetched_at.map(iso),
                limits: report.limits.into_iter().map(limit).collect(),
            })
            .collect(),
    })
}

fn limit(limit: Limit) -> UsageLimit {
    let amount = limit.amount;
    let (used, max, unit) = match (amount.used, amount.limit) {
        (Some(used), Some(max)) => (used, max, amount.unit.unwrap_or_default()),
        _ => (
            amount.used_fraction.unwrap_or_default() * 100.0,
            100.0,
            "percent".to_owned(),
        ),
    };
    UsageLimit {
        window: limit.window.map(|window| UsageWindow {
            label: window.label.unwrap_or_default(),
            resets_at: window.resets_at.map(iso),
        }),
        label: limit.label,
        used,
        limit: max,
        unit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_accounts_limits_and_percent_fallback() {
        let usage = parse(
            br#"{"generatedAt": 1762686000000, "reports": [
                {"provider": "anthropic", "fetchedAt": 1762685940000,
                 "metadata": {"email": "a@b.c", "planType": "max"},
                 "limits": [
                    {"id": "5h", "label": "5 hours", "status": "ok",
                     "window": {"label": "5h", "resetsAt": 1762690000000},
                     "amount": {"used": 42, "limit": 100, "unit": "percent"}},
                    {"id": "week", "label": "Week", "amount": {"usedFraction": 0.25}}]},
                {"provider": "openai", "limits": []}]}"#,
        )
        .unwrap();
        assert_eq!(usage.generated_at, "2025-11-09T11:00:00.000Z");
        let anthropic = &usage.accounts[0];
        assert_eq!(
            anthropic.fetched_at.as_deref(),
            Some("2025-11-09T10:59:00.000Z")
        );
        assert_eq!(
            anthropic.account,
            Some(AccountInfo {
                email: Some("a@b.c".into()),
                plan: Some("max".into())
            })
        );
        let five = &anthropic.limits[0];
        assert_eq!(
            (five.used, five.limit, five.unit.as_str()),
            (42.0, 100.0, "percent")
        );
        assert_eq!(five.window.as_ref().unwrap().label, "5h");
        let week = &anthropic.limits[1];
        assert_eq!(
            (week.used, week.limit, week.unit.as_str()),
            (25.0, 100.0, "percent")
        );
        assert_eq!(week.window, None);
        assert_eq!(usage.accounts[1].account, None);
    }

    #[test]
    fn rejects_other_output() {
        assert!(
            parse(b"not json")
                .unwrap_err()
                .starts_with("unexpected omp usage output")
        );
    }
}
