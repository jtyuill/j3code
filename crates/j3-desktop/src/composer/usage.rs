//! The usage gauge beside the model (spec §7.5): the model provider's cached usage on the
//! composer's environment, and a popover listing its limits, refreshed when opened. The data
//! is the app's `UsageStore`; watches follow the shown composer (`J3::poll_composer`).

use j3_protocol::j3::{UsageLimit, UsageReport};
use rune::{El, ScrollSpec, col, el, icon, row, text};
use rune_kit::{Placement, Popover, Spinner};

use super::ComposerKey;
use crate::{
    app::{J3, Ui},
    feed::age_since,
    icons, logos,
    theme::Status,
    usage::{self as store, limit_title, remaining, unix_ms, used_fraction},
};

impl J3 {
    fn toggle_usage(&mut self, key: &ComposerKey) {
        let composer = self.composer(key);
        composer.usage_open = !composer.usage_open;
        if composer.usage_open {
            self.refresh_usage(key.env());
        }
    }
}

/// The button (with the popover in the layer while open).
pub(super) fn view(s: &J3, ui: &mut Ui, key: &ComposerKey) -> El {
    let k = ui.k;
    let env = s.envs.get(key.env());
    let provider = s.composer_choice(key).map(|c| c.provider.clone());
    let label = provider.as_deref().and_then(|provider| {
        let snapshot = s.usage.snapshot(key.env())?;
        if s.prefs.ui().shows_usage(provider)
            && let Some(breakdown) = store::breakdown(snapshot, provider)
        {
            return Some(breakdown);
        }
        store::peak(snapshot, provider).map(|peak| format!("{:.0}%", peak * 100.))
    });
    let help = match (&provider, env) {
        (Some(provider), Some(env)) => format!(
            "{} usage limits on {}",
            logos::provider_name(provider),
            env.label()
        ),
        _ => "Usage limits".to_owned(),
    };
    let open = s.composers.get(key).is_some_and(|c| c.usage_open);
    let toggle = key.clone();
    let mut button = row()
        .key("usage")
        .role("button")
        .label(label.clone().unwrap_or_else(|| "Usage".to_owned()))
        .help(help)
        .h(28.0)
        .px(6.0)
        .gap(4.0)
        .center_y()
        .radius(k.radius.md)
        .hover(k.c.hover)
        .child(icon(&icons::GAUGE, 14.0).color(k.c.icon_muted));
    if let Some(label) = label {
        button = button.child(text(label).font(k.ty.text_xsmall).color(k.c.text_muted));
    }
    let button = button.on_press(move |s: &mut J3| s.toggle_usage(&toggle));
    let close = key.clone();
    let mut popover = Popover::new("usage-popover")
        .trigger(button)
        .label("Usage")
        .placement(Placement::TopStart)
        .padding(0.0)
        .open(open)
        .on_close(move |s: &mut J3| s.composer(&close).usage_open = false);
    if open {
        popover = popover.child(panel(s, ui, key, provider.as_deref().unwrap_or_default()));
    }
    popover.build(k, ui.layer)
}

/// The popover's panel: provider, environment and freshness, the last error, then the
/// provider's limits per account.
fn panel(s: &J3, ui: &Ui, key: &ComposerKey, provider: &str) -> El {
    let k = ui.k;
    let env_key = key.env();
    let env_label = s.envs.get(env_key).map_or("", |env| env.label());
    let now = s.clock.now;
    let pending = s.usage.is_pending(env_key);
    let snapshot = s.usage.snapshot(env_key);
    let provider_name = logos::provider_name(provider);
    let status = if pending {
        "Updating…".to_owned()
    } else if let Some(at) = s.usage.updated_at(env_key) {
        match age_since(at, now).as_str() {
            "now" => "updated now".to_owned(),
            age => format!("updated {age} ago"),
        }
    } else {
        String::new()
    };
    let mut title = row()
        .gap(6.0)
        .center_y()
        .child(logos::provider_logo(provider, 14.0, k))
        .child(
            text(provider_name)
                .font(k.ty.text_small_medium)
                .color(k.c.text_strong)
                .grow(1.0)
                .shrink()
                .truncate(),
        );
    if pending {
        title = title.child(Spinner::new().size(12.0).build(k));
    }
    let mut body = col()
        .key("usage-panel")
        .w(300.0)
        .max_h(480.0)
        .padding(12.0)
        .gap(8.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(title)
        .child(
            row()
                .gap(8.0)
                .child(
                    text(env_label)
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_muted)
                        .grow(1.0)
                        .shrink()
                        .truncate(),
                )
                .child(text(status).font(k.ty.text_xsmall).color(k.c.text_muted)),
        );
    if let Some(error) = s.usage.error(env_key) {
        body = body.child(
            text(error)
                .key(format!("usage-error-{env_key}"))
                .font(k.ty.text_xsmall)
                .color(Status::of(k).danger)
                .wrap(),
        );
    }
    // Report indices stay those of the whole snapshot, so keys are stable.
    let reports: Vec<(usize, &UsageReport)> = snapshot
        .iter()
        .flat_map(|snapshot| snapshot.reports.iter().enumerate())
        .filter(|(_, report)| report.provider == provider)
        .collect();
    if reports.is_empty() {
        if snapshot.is_some() {
            body = body.child(
                text(format!("No usage limits reported for {provider_name}"))
                    .font(k.ty.text_xsmall)
                    .color(k.c.text_muted),
            );
        }
        return body;
    }
    for (ix, report) in &reports {
        body = body.child(report_view(ui, env_key, *ix, report, reports.len(), now));
    }
    body
}

/// One account's limits; several accounts are told apart by email or org and plan.
fn report_view(
    ui: &Ui,
    env: &str,
    ix: usize,
    report: &UsageReport,
    accounts: usize,
    now: time::OffsetDateTime,
) -> El {
    let k = ui.k;
    let account = &report.metadata;
    let who = [
        account.email.as_deref().or(account.org_name.as_deref()),
        account.plan_type.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let mut column = col().gap(4.0);
    if accounts > 1 && !who.is_empty() {
        column = column.child(
            text(who)
                .font(k.ty.text_xsmall)
                .color(k.c.text_muted)
                .truncate(),
        );
    }
    for (lix, limit) in report.limits.iter().enumerate() {
        column = column.child(limit_view(
            ui,
            format!("usage-{env}-{ix}-{lix}"),
            limit,
            now,
        ));
    }
    column
}

/// A limit: title and percentage, a bar (danger from 90%, warning from 70%) and when it
/// resets.
fn limit_view(ui: &Ui, key: String, limit: &UsageLimit, now: time::OffsetDateTime) -> El {
    let k = ui.k;
    let status = Status::of(k);
    let fraction = used_fraction(limit);
    let color = match fraction {
        Some(f) if f >= 0.9 => status.danger,
        Some(f) if f >= 0.7 => status.warning,
        _ => k.c.border_active,
    };
    let resets = limit
        .window
        .as_ref()
        .and_then(|window| window.resets_at)
        .and_then(unix_ms)
        .map(|at| format!("resets in {}", remaining(at - now)));
    let title = limit_title(limit);
    let bar = row()
        .key("bar")
        .role("progressbar")
        .label(title.clone())
        .w_full()
        .h(6.0)
        .radius(999.0)
        .fill(k.c.border_weak)
        .clip_soft(0.0)
        .child(
            el().h_full()
                .radius(999.0)
                .fill(color)
                .w_frac(fraction.unwrap_or(0.0).clamp(0.0, 1.0) as f32),
        );
    let mut column = col()
        .key(key)
        .gap(2.0)
        .child(
            row()
                .gap(8.0)
                .child(
                    text(title)
                        .font(k.ty.text_xsmall)
                        .color(k.c.text_base)
                        .grow(1.0)
                        .shrink()
                        .truncate(),
                )
                .child(
                    text(match fraction {
                        Some(f) => format!("{:.0}%", f * 100.),
                        None => "—".to_owned(),
                    })
                    .font(k.ty.text_xsmall)
                    .color(k.c.text_muted),
                ),
        )
        .child(bar);
    if let Some(resets) = resets {
        column = column.child(text(resets).font(k.ty.text_xsmall).color(k.c.text_muted));
    }
    column
}
