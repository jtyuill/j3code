//! Provider usage limits (`omp usage --json` on an environment's host,
//! through `server.usage`), shown from the composer's usage button for the
//! composer's environment and the chosen model's provider.
//!
//! [`UsageStore`] is app-wide (`J3::usage`): the last snapshot of each
//! environment is cached in `usage.json` next to `desktop.json` and shown as
//! it is. While a composer shows an environment's usage button, that
//! environment is asked again once a minute, and opening the button asks at
//! once; until an answer arrives (and when one fails) the cached snapshot
//! stays on screen.
//!
//! How a composer uses it:
//! - While it shows an environment's usage button it holds a watch:
//!   `J3::watch_usage(env, true, tx)` when the button appears (which also
//!   asks at once when the cached snapshot is stale) and
//!   `J3::watch_usage(env, false, tx)` when it goes away; every watch is
//!   paired with an unwatch.
//! - Opening the button calls `J3::refresh_usage(env)`.
//! - The button and its popover read [`UsageStore::snapshot`] (with
//!   [`peak`], [`breakdown`] and [`windows`] for the provider),
//!   [`UsageStore::is_pending`] for a spinner, [`UsageStore::error`] for the
//!   last failure and [`UsageStore::updated_at`] for the snapshot's age.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

use j3_client::methods::{Usage, UsageLimit};
use time::OffsetDateTime;

use crate::{app::J3, sidebar::parse_time};

const CACHE_FILE: &str = "usage.json";
/// How often the shown environments' usage is asked for again.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Every environment's last usage snapshot, shared by the usage buttons.
#[derive(Default)]
pub struct UsageStore {
    /// Latest snapshot per environment key.
    cache: BTreeMap<String, Usage>,
    /// Environments with a request in flight.
    pending: HashSet<String>,
    /// The last request's failure, per environment.
    errors: HashMap<String, String>,
    /// `None` without a config directory: nothing is persisted.
    path: Option<PathBuf>,
    /// Watched environments and how many usage buttons watch each.
    watched: HashMap<String, usize>,
    /// When (`Clock::t`) the watched environments are next polled, while
    /// anything is watched.
    poll_at: Option<f64>,
}

impl UsageStore {
    /// The store with its cache loaded from `usage.json` beside
    /// `config_path` (`desktop.json`, `Services::config_path`); `None` keeps
    /// nothing on disk.
    pub fn load(config_path: Option<PathBuf>) -> Self {
        let path = config_path.map(|config| config.with_file_name(CACHE_FILE));
        let cache = path.as_deref().map(load).unwrap_or_default();
        Self {
            cache,
            path,
            ..Self::default()
        }
    }

    /// `env`'s cached snapshot.
    pub fn snapshot(&self, env: &str) -> Option<&Usage> {
        self.cache.get(env)
    }

    /// Whether a request for `env` is in flight.
    pub fn is_pending(&self, env: &str) -> bool {
        self.pending.contains(env)
    }

    /// Why `env`'s last request failed; cleared by the next answer.
    pub fn error(&self, env: &str) -> Option<&str> {
        self.errors.get(env).map(String::as_str)
    }

    /// When `env`'s cached snapshot was taken.
    pub fn updated_at(&self, env: &str) -> Option<OffsetDateTime> {
        self.cache
            .get(env)
            .and_then(|snapshot| parse_time(&snapshot.generated_at))
    }

    /// Whether `env`'s snapshot is missing or due for a poll. A little
    /// slack keeps a snapshot fetched just before a tick from skipping it.
    fn is_stale(&self, env: &str, now: OffsetDateTime) -> bool {
        self.cache
            .get(env)
            .and_then(|snapshot| parse_time(&snapshot.generated_at))
            .is_none_or(|at| now - at >= POLL_INTERVAL - Duration::from_secs(5))
    }

    /// Lands the answer to `env`'s request: a snapshot replaces the cached
    /// one (and is saved), an error keeps it.
    fn answered(&mut self, env: String, result: Result<Usage, String>) {
        self.pending.remove(&env);
        match result {
            Ok(snapshot) => {
                self.errors.remove(&env);
                self.cache.insert(env, snapshot);
                self.save();
            }
            Err(error) => {
                self.errors.insert(env, error);
            }
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let json = serde_json::to_vec_pretty(&self.cache).expect("usage snapshots encode");
        if let Err(error) = std::fs::write(path, json) {
            tracing::warn!(%error, path = %path.display(), "saving the usage cache");
        }
    }
}

impl J3 {
    /// Asks `env` for a new snapshot, unless a request is in flight; the
    /// cached snapshot stays until it is replaced.
    pub fn refresh_usage(&mut self, env: &str) {
        if self.usage.pending.contains(env) {
            return;
        }
        let Some(environment) = self.envs.get(env) else {
            return;
        };
        let connection = environment.connection().clone();
        let key = env.to_owned();
        self.usage.pending.insert(key.clone());
        self.spawn(async move {
            let result = connection
                .server_usage()
                .await
                .map_err(|error| error.to_string());
            move |s: &mut J3, _: &mut rune::Tx| s.usage.answered(key, result)
        });
    }

    /// Starts (`watching`) or ends one usage button's watch of `env`. While
    /// any watch holds, `env`'s snapshot is kept fresh: refreshed now if
    /// stale, then every [`POLL_INTERVAL`].
    pub fn watch_usage(&mut self, env: &str, watching: bool, tx: &mut rune::Tx) {
        if !watching {
            if let Some(count) = self.usage.watched.get_mut(env) {
                *count -= 1;
                if *count == 0 {
                    self.usage.watched.remove(env);
                }
            }
            return;
        }
        *self.usage.watched.entry(env.to_owned()).or_default() += 1;
        if self.usage.is_stale(env, self.clock.now) {
            self.refresh_usage(env);
        }
        if self.usage.poll_at.is_none() {
            self.usage.poll_at = Some(self.clock.t + POLL_INTERVAL.as_secs_f64());
            tx.wake_after(POLL_INTERVAL);
        }
    }

    /// One poll when it is due: forgets watched environments that are gone
    /// and refreshes the watched ones whose snapshot is stale; polling stops
    /// once nothing is watched. True when a refresh started.
    pub(crate) fn poll_usage(&mut self, tx: &mut rune::Tx) -> bool {
        if self.usage.poll_at.is_none_or(|at| self.clock.t < at) {
            return false;
        }
        self.usage
            .watched
            .retain(|env, _| self.envs.get(env).is_some());
        if self.usage.watched.is_empty() {
            self.usage.poll_at = None;
            return false;
        }
        self.usage.poll_at = Some(self.clock.t + POLL_INTERVAL.as_secs_f64());
        tx.wake_after(POLL_INTERVAL);
        let now = self.clock.now;
        let stale: Vec<String> = self
            .usage
            .watched
            .keys()
            .filter(|env| !self.usage.pending.contains(*env) && self.usage.is_stale(env, now))
            .cloned()
            .collect();
        for env in &stale {
            self.refresh_usage(env);
        }
        !stale.is_empty()
    }
}

/// The highest used fraction among `provider`'s limits in `snapshot`.
pub fn peak(snapshot: &Usage, provider: &str) -> Option<f64> {
    snapshot
        .accounts
        .iter()
        .filter(|account| account.provider == provider)
        .flat_map(|account| &account.limits)
        .filter_map(used_fraction)
        .max_by(f64::total_cmp)
}

/// The windows [`windows`] reports, shortest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Span {
    Session,
    Weekly,
    Monthly,
}

impl Span {
    pub fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Weekly => "Weekly",
            Self::Monthly => "Monthly",
        }
    }

    /// The window a limit counts in, from its window label (`5 Hour`,
    /// `7 Day`, `Weekly`, `Monthly`). Longer windows such as 30 days are
    /// none of them.
    fn of(limit: &UsageLimit) -> Option<Self> {
        let label = limit
            .window
            .as_ref()
            .map_or(&limit.label, |window| &window.label)
            .to_lowercase();
        if label.contains("5 hour") || label.contains("5h") || label.contains("session") {
            Some(Self::Session)
        } else if label.contains("week") || label.contains("7 day") || label.contains("7d") {
            Some(Self::Weekly)
        } else if label.contains("month") {
            Some(Self::Monthly)
        } else {
            None
        }
    }
}

/// `provider`'s used fraction per window, shortest first. Each account
/// counts its first limit of a window (the provider's overall one, before
/// per-model limits); of several accounts the highest wins.
pub fn windows(snapshot: &Usage, provider: &str) -> Vec<(Span, f64)> {
    let mut found: BTreeMap<Span, f64> = BTreeMap::new();
    for account in snapshot
        .accounts
        .iter()
        .filter(|account| account.provider == provider)
    {
        let mut seen = Vec::new();
        for limit in &account.limits {
            let Some(window) = Span::of(limit) else {
                continue;
            };
            if seen.contains(&window) {
                continue;
            }
            seen.push(window);
            if let Some(fraction) = used_fraction(limit) {
                let slot = found.entry(window).or_insert(fraction);
                *slot = slot.max(fraction);
            }
        }
    }
    found.into_iter().collect()
}

/// [`windows`] as `Session 12% · Weekly 20%`; `None` when `provider`
/// reports none.
pub fn breakdown(snapshot: &Usage, provider: &str) -> Option<String> {
    let windows = windows(snapshot, provider);
    (!windows.is_empty()).then(|| {
        windows
            .iter()
            .map(|(span, fraction)| format!("{} {:.0}%", span.label(), fraction * 100.))
            .collect::<Vec<_>>()
            .join(" · ")
    })
}

/// The limit's label, with its window when the label doesn't name it
/// ("Gemini · Weekly", but just "Claude 5 Hour").
pub fn limit_title(limit: &UsageLimit) -> String {
    match limit.window.as_ref().map(|w| w.label.as_str()) {
        Some(window) if !limit.label.contains(window) => format!("{} · {window}", limit.label),
        _ => limit.label.clone(),
    }
}

/// How much of `limit` is used, 0 to 1, when it says.
pub fn used_fraction(limit: &UsageLimit) -> Option<f64> {
    (limit.limit > 0.).then(|| limit.used / limit.limit)
}

/// A countdown such as `45m`, `3h 12m` or `2d 4h`.
pub fn remaining(duration: time::Duration) -> String {
    let minutes = duration.whole_minutes().max(0);
    match minutes {
        0..60 => format!("{minutes}m"),
        60..1_440 => format!("{}h {}m", minutes / 60, minutes % 60),
        _ => format!("{}d {}h", minutes / 1_440, minutes % 1_440 / 60),
    }
}

/// The cache at `path`; empty when missing or unreadable.
fn load(path: &Path) -> BTreeMap<String, Usage> {
    let Ok(bytes) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, path = %path.display(), "ignoring the usage cache");
        BTreeMap::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdowns_carry_the_next_unit() {
        assert_eq!(remaining(time::Duration::seconds(-5)), "0m");
        assert_eq!(remaining(time::Duration::minutes(59)), "59m");
        assert_eq!(remaining(time::Duration::minutes(3 * 60 + 12)), "3h 12m");
        assert_eq!(
            remaining(time::Duration::minutes(2 * 1_440 + 4 * 60 + 30)),
            "2d 4h"
        );
    }

    #[test]
    fn windows_are_the_providers_overall_limits_by_length() {
        let limit = |label: &str, window: &str, used: f64| {
            serde_json::json!({
                "label": label, "window": {"label": window},
                "used": used, "limit": 100., "unit": "percent",
            })
        };
        let snapshot: Usage = serde_json::from_value(serde_json::json!({
            "generatedAt": "2026-11-09T11:18:00.000Z",
            "accounts": [
                {"provider": "anthropic", "limits": [
                    limit("Claude 5 Hour", "5 Hour", 12.),
                    limit("Claude 7 Day", "7 Day", 20.),
                    limit("Claude 7 Day (Fable)", "7 Day", 90.),
                ]},
                {"provider": "opencode-go", "limits": [
                    limit("5 Hour limit", "5 Hour", 0.),
                    limit("Weekly limit", "Weekly", 4.),
                    limit("Monthly limit", "Monthly", 50.),
                ]},
                {"provider": "openai-codex", "limits": [limit("30 days", "30 days", 1.)]},
            ],
        }))
        .unwrap();
        assert_eq!(
            windows(&snapshot, "anthropic"),
            [(Span::Session, 0.12), (Span::Weekly, 0.2)]
        );
        assert_eq!(
            windows(&snapshot, "opencode-go"),
            [
                (Span::Session, 0.),
                (Span::Weekly, 0.04),
                (Span::Monthly, 0.5)
            ]
        );
        assert!(windows(&snapshot, "openai-codex").is_empty());
    }
}
