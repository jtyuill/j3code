//! Provider usage limits (`omp usage --json` on an environment's host,
//! through `j3.getUsage`), shown from the composer's usage button for the
//! composer's environment and the chosen model's provider.
//!
//! [`UsageStore`] is app-wide: the last snapshot of each environment is
//! cached in `usage.json` next to `desktop.json` and shown as it is. While a
//! composer shows an environment's usage button, that environment is asked
//! again once a minute, and opening the button asks at once; until an answer
//! arrives (and when one fails) the cached snapshot stays on screen.

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, progress::Progress, spinner::Spinner,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Global, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, TestSupportExt as _, WeakEntity, Window, div, px,
};
use j3_protocol::j3::{UsageLimit, UsageReport, UsageSnapshot};
use time::OffsetDateTime;

use crate::{environment::Environment, feed::age_since, logos, services::Services};

const CACHE_FILE: &str = "usage.json";
/// How often the shown environments' usage is asked for again.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Every environment's last usage snapshot, shared by the usage buttons.
pub struct UsageStore {
    /// Latest snapshot per environment key.
    cache: BTreeMap<String, UsageSnapshot>,
    /// Environments with a request in flight.
    pending: HashMap<SharedString, Task<()>>,
    /// The last request's failure, per environment.
    errors: HashMap<SharedString, SharedString>,
    /// `None` without a config directory: nothing is persisted.
    path: Option<PathBuf>,
    /// Live usage buttons and their environments; polled while any lives.
    watched: Vec<(WeakEntity<UsagePanel>, WeakEntity<Environment>)>,
    /// The polling loop, while anything is watched.
    poll: Option<Task<()>>,
}

struct GlobalUsageStore(Entity<UsageStore>);

impl Global for GlobalUsageStore {}

impl UsageStore {
    /// The app's store, created (and its cache loaded) on first use.
    pub fn global(cx: &mut App) -> Entity<UsageStore> {
        if let Some(store) = cx.try_global::<GlobalUsageStore>() {
            return store.0.clone();
        }
        let path = Services::global(cx)
            .config_path()
            .map(|config| config.with_file_name(CACHE_FILE));
        let cache = path.as_deref().map(load).unwrap_or_default();
        let store = cx.new(|_| Self {
            cache,
            pending: HashMap::new(),
            errors: HashMap::new(),
            path,
            watched: Vec::new(),
            poll: None,
        });
        cx.set_global(GlobalUsageStore(store.clone()));
        store
    }

    /// `env`'s cached snapshot.
    pub fn snapshot(&self, env: &str) -> Option<&UsageSnapshot> {
        self.cache.get(env)
    }

    /// Asks `env` for a new snapshot, unless a request is in flight; the
    /// cached snapshot stays until it is replaced.
    pub fn refresh(&mut self, env: &Entity<Environment>, cx: &mut Context<Self>) {
        let env = env.read(cx);
        let key = env.key().clone();
        if self.pending.contains_key(&key) {
            return;
        }
        let connection = env.connection().clone();
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                let result = connection.usage().await;
                let _ = this.update(cx, |this, cx| {
                    this.pending.remove(&key);
                    match result {
                        Ok(snapshot) => {
                            this.errors.remove(&key);
                            this.cache.insert(key.to_string(), snapshot);
                            this.save();
                        }
                        Err(error) => {
                            this.errors.insert(key, error.to_string().into());
                        }
                    }
                    cx.notify();
                });
            }
        });
        self.pending.insert(key, task);
        cx.notify();
    }

    /// Keeps `env`'s snapshot fresh while `panel` lives: refreshed now if
    /// stale, then every [`POLL_INTERVAL`].
    pub fn watch(
        &mut self,
        panel: WeakEntity<UsagePanel>,
        env: &Entity<Environment>,
        cx: &mut Context<Self>,
    ) {
        self.watched.push((panel, env.downgrade()));
        if self.is_stale(env.read(cx).key(), OffsetDateTime::now_utc()) {
            self.refresh(env, cx);
        }
        if self.poll.is_none() {
            self.poll = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(POLL_INTERVAL).await;
                    let polling = this.update(cx, |this, cx| this.poll_watched(cx));
                    if !polling.unwrap_or(false) {
                        break;
                    }
                }
            }));
        }
    }

    /// One poll: drops the closed panels and refreshes the environments
    /// still shown whose snapshot is stale. `false` stops polling (nothing
    /// is watched any more).
    fn poll_watched(&mut self, cx: &mut Context<Self>) -> bool {
        self.watched
            .retain(|(panel, env)| panel.upgrade().is_some() && env.upgrade().is_some());
        if self.watched.is_empty() {
            self.poll = None;
            return false;
        }
        let now = OffsetDateTime::now_utc();
        let envs: Vec<Entity<Environment>> = self
            .watched
            .iter()
            .filter_map(|(_, env)| env.upgrade())
            .collect();
        for env in envs {
            // `refresh` skips an environment already asked this tick.
            if self.is_stale(env.read(cx).key(), now) {
                self.refresh(&env, cx);
            }
        }
        true
    }

    /// Whether `env`'s snapshot is missing or due for a poll. A little
    /// slack keeps a snapshot fetched just before a tick from skipping it.
    fn is_stale(&self, env: &str, now: OffsetDateTime) -> bool {
        self.cache
            .get(env)
            .and_then(|snapshot| unix_ms(snapshot.generated_at))
            .is_none_or(|at| now - at >= POLL_INTERVAL - Duration::from_secs(5))
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

/// The highest used fraction among `provider`'s limits in `snapshot`.
pub fn peak(snapshot: &UsageSnapshot, provider: &str) -> Option<f64> {
    snapshot
        .reports
        .iter()
        .filter(|report| report.provider == provider)
        .flat_map(|report| &report.limits)
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
            .and_then(|window| window.label.as_deref())
            .unwrap_or(&limit.label)
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
pub fn windows(snapshot: &UsageSnapshot, provider: &str) -> Vec<(Span, f64)> {
    let mut found: BTreeMap<Span, f64> = BTreeMap::new();
    for report in snapshot
        .reports
        .iter()
        .filter(|report| report.provider == provider)
    {
        let mut seen = Vec::new();
        for limit in &report.limits {
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
pub fn breakdown(snapshot: &UsageSnapshot, provider: &str) -> Option<String> {
    let windows = windows(snapshot, provider);
    (!windows.is_empty()).then(|| {
        windows
            .iter()
            .map(|(span, fraction)| format!("{} {:.0}%", span.label(), fraction * 100.))
            .collect::<Vec<_>>()
            .join(" · ")
    })
}

/// One environment's usage for one provider: the usage button's popover.
pub struct UsagePanel {
    store: Entity<UsageStore>,
    environment: Entity<Environment>,
    /// omp provider id of the composer's model.
    provider: Option<String>,
    _store: Subscription,
}

impl UsagePanel {
    pub fn new(environment: Entity<Environment>, cx: &mut Context<Self>) -> Self {
        let store = UsageStore::global(cx);
        let panel = cx.weak_entity();
        store.update(cx, |store, cx| store.watch(panel, &environment, cx));
        Self {
            _store: cx.observe(&store, |_, _, cx| cx.notify()),
            store,
            environment,
            provider: None,
        }
    }

    pub fn set_provider(&mut self, provider: Option<String>, cx: &mut Context<Self>) {
        if self.provider != provider {
            self.provider = provider;
            cx.notify();
        }
    }

    /// Asks the environment for a new snapshot.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let environment = self.environment.clone();
        self.store
            .update(cx, |store, cx| store.refresh(&environment, cx));
    }

    fn render_report(
        &self,
        env_key: &SharedString,
        ix: usize,
        report: &UsageReport,
        accounts: usize,
        now: OffsetDateTime,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let account = &report.metadata;
        let who = [
            account.email.as_deref().or(account.org_name.as_deref()),
            account.plan_type.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        v_flex()
            .gap_1()
            // One account needs no heading; several are told apart by it.
            .when(accounts > 1 && !who.is_empty(), |this| {
                this.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(who),
                )
            })
            .children(report.limits.iter().enumerate().map(|(lix, limit)| {
                let id = format!("usage-{env_key}-{ix}-{lix}");
                render_limit(id, limit, now, cx)
            }))
            .into_any_element()
    }
}

fn render_limit(id: String, limit: &UsageLimit, now: OffsetDateTime, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let fraction = used_fraction(limit);
    let color = match fraction {
        Some(f) if f >= 0.9 => theme.danger,
        Some(f) if f >= 0.7 => theme.warning,
        _ => theme.progress_bar,
    };
    let resets = limit
        .window
        .as_ref()
        .and_then(|window| window.resets_at)
        .and_then(unix_ms)
        .map(|at| format!("resets in {}", remaining(at - now)));
    v_flex()
        .id(SharedString::from(id.clone()))
        .test_support()
        .gap_0p5()
        .child(
            h_flex()
                .gap_2()
                .text_xs()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(limit_title(limit)),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_color(theme.muted_foreground)
                        .child(match fraction {
                            Some(f) => format!("{:.0}%", f * 100.),
                            None => "—".to_owned(),
                        }),
                ),
        )
        .child(
            Progress::new(SharedString::from(format!("{id}-bar")))
                .xsmall()
                .color(color)
                .value(fraction.unwrap_or(0.) as f32 * 100.),
        )
        .when_some(resets, |this, resets| {
            this.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(resets),
            )
        })
        .into_any_element()
}

/// The limit's label, with its window when the label doesn't name it
/// ("Gemini · Weekly", but just "Claude 5 Hour").
fn limit_title(limit: &UsageLimit) -> String {
    match limit.window.as_ref().and_then(|w| w.label.as_deref()) {
        Some(window) if !limit.label.contains(window) => format!("{} · {window}", limit.label),
        _ => limit.label.clone(),
    }
}

fn used_fraction(limit: &UsageLimit) -> Option<f64> {
    let amount = &limit.amount;
    amount
        .used_fraction
        .or_else(|| match (amount.used, amount.limit) {
            (Some(used), Some(max)) if max > 0. => Some(used / max),
            _ => None,
        })
}

fn unix_ms(ms: i64) -> Option<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()
}

/// A countdown such as `45m`, `3h 12m` or `2d 4h`.
fn remaining(duration: time::Duration) -> String {
    let minutes = duration.whole_minutes().max(0);
    match minutes {
        0..60 => format!("{minutes}m"),
        60..1_440 => format!("{}h {}m", minutes / 60, minutes % 60),
        _ => format!("{}d {}h", minutes / 1_440, minutes % 1_440 / 60),
    }
}

/// The cache at `path`; empty when missing or unreadable.
fn load(path: &Path) -> BTreeMap<String, UsageSnapshot> {
    let Ok(bytes) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, path = %path.display(), "ignoring the usage cache");
        BTreeMap::new()
    })
}

impl Render for UsagePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = OffsetDateTime::now_utc();
        let env = self.environment.read(cx);
        let key = env.key().clone();
        let env_label = env.label().clone();
        let store = self.store.read(cx);
        let pending = store.pending.contains_key(&key);
        let error = store.errors.get(&key).cloned();
        let snapshot = store.cache.get(key.as_ref());
        let theme = cx.theme();
        let provider = self.provider.as_deref().unwrap_or_default();
        let provider_name = logos::provider_name(provider).to_owned();
        let status: SharedString = if pending {
            "Updating…".into()
        } else if let Some(at) = snapshot.and_then(|s| unix_ms(s.generated_at)) {
            match age_since(at, now).as_str() {
                "now" => "updated now".into(),
                age => format!("updated {age} ago").into(),
            }
        } else {
            SharedString::default()
        };
        // Report indices stay those of the whole snapshot, so ids are stable.
        let reports: Vec<(usize, &UsageReport)> = snapshot
            .iter()
            .flat_map(|snapshot| snapshot.reports.iter().enumerate())
            .filter(|(_, report)| report.provider == provider)
            .collect();
        let body = if !reports.is_empty() {
            v_flex()
                .gap_2()
                .children(reports.iter().map(|(ix, report)| {
                    self.render_report(&key, *ix, report, reports.len(), now, cx)
                }))
                .into_any_element()
        } else if snapshot.is_some() {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("No usage limits reported for {provider_name}"))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        v_flex()
            .id("usage-panel")
            .test_support()
            .w(px(300.))
            .max_h(px(480.))
            .overflow_y_scroll()
            .gap_2()
            .child(
                h_flex()
                    .gap_1p5()
                    .child(logos::provider_logo(provider, px(14.), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_semibold()
                            .child(provider_name),
                    )
                    .when(pending, |this| this.child(Spinner::new().xsmall())),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_1().min_w_0().truncate().child(env_label))
                    .child(div().flex_shrink_0().child(status)),
            )
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .id(SharedString::from(format!("usage-error-{key}")))
                        .test_support()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(error),
                )
            })
            .child(body)
    }
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
                "id": label, "label": label, "window": {"label": window},
                "amount": {"usedFraction": used},
            })
        };
        let snapshot: UsageSnapshot = serde_json::from_value(serde_json::json!({
            "generatedAt": 0,
            "reports": [
                {"provider": "anthropic", "limits": [
                    limit("Claude 5 Hour", "5 Hour", 0.12),
                    limit("Claude 7 Day", "7 Day", 0.2),
                    limit("Claude 7 Day (Fable)", "7 Day", 0.9),
                ]},
                {"provider": "opencode-go", "limits": [
                    limit("5 Hour limit", "5 Hour", 0.),
                    limit("Weekly limit", "Weekly", 0.04),
                    limit("Monthly limit", "Monthly", 0.5),
                ]},
                {"provider": "openai-codex", "limits": [limit("30 days", "30 days", 0.01)]},
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
