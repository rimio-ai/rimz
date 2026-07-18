//! `rimz providers` — account plans, auth state, limits, credits, and spend.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::{Timestamp, tz::TimeZone};
use serde::Serialize;

use super::GlobalFlags;
use super::render::{self, KeyVals, cell};
use super::spinner::Spinner;
use rimz::agents::spending::{ProviderSpendingCache, read_provider_spending_cache};
use rimz::agents::{ExtraCredits, ProviderAccountScope, RateLimitWindow, ResetCredits, SpendTally};
use rimz::config::MachineConfig;
use rimz::sidebar::enrich::provider_panels_from_caches;
use rimz::sidebar::refresh::{
    AccountsCache, ProviderRecord, query_provider_accounts, refresh_account_usage_if_due,
    refresh_account_usage_now,
};
use rimz::{DailyBudgetView, RuntimePaths, SidebarProviderPanel};

const SPINNER_MIN_AGE: Duration = Duration::from_millis(150);

#[derive(Debug, Args)]
pub struct ProvidersArgs {
    /// Show only one provider kind.
    #[arg(value_name = "KIND")]
    kind: Option<String>,
    /// Emit the report as JSON instead of human-readable blocks.
    #[arg(long)]
    json: bool,
    /// Bypass account and usage refresh TTLs.
    #[arg(long)]
    refresh: bool,
    /// Include logged-out and empty provider kinds.
    #[arg(long)]
    all: bool,
}

pub fn run(args: ProvidersArgs, _globals: &GlobalFlags) -> Result<()> {
    validate_kind(args.kind.as_deref())?;
    let runtime = RuntimePaths::shared();
    runtime
        .ensure_shared_dirs()
        .context("preparing shared provider cache paths")?;
    let config = MachineConfig::load_lenient();
    let spinner = (!args.json && std::io::stdout().is_terminal())
        .then(|| Spinner::delayed("Querying provider accounts", SPINNER_MIN_AGE));
    let accounts = query_provider_accounts(&runtime, args.refresh);
    if let Some(spinner) = &spinner {
        spinner.set("Refreshing provider usage");
    }
    for (kind, record) in &accounts.providers {
        if args.kind.as_deref().is_some_and(|filter| filter != kind)
            || !record.ok
            || record.account.is_none()
        {
            continue;
        }
        if args.refresh {
            refresh_account_usage_now(&runtime, kind);
        } else {
            refresh_account_usage_if_due(&runtime, kind);
        }
    }
    drop(spinner);

    let provider_spending = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    let account_facts: BTreeMap<_, _> = accounts
        .providers
        .iter()
        .filter_map(|(kind, record)| {
            record
                .account
                .clone()
                .map(|account| (kind.clone(), account))
        })
        .collect();
    let panels = provider_panels_from_caches(&runtime, &config, account_facts, &provider_spending);
    let reports = assemble_reports(
        &accounts,
        panels,
        &provider_spending,
        args.kind.as_deref(),
        args.all,
    );
    if args.json {
        return render::json_pretty(&reports);
    }
    let mut out = render::out();
    render::finish(write_pretty(
        &mut out,
        &reports,
        Timestamp::now(),
        &config.time_zone(),
    ))
}

fn validate_kind(kind: Option<&str>) -> Result<()> {
    let Some(kind) = kind else {
        return Ok(());
    };
    let known: Vec<_> = rimz::agents::known_kinds().collect();
    if known.contains(&kind) {
        return Ok(());
    }
    bail!(
        "unknown provider kind `{kind}`; known kinds: {}",
        known.join(", ")
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProviderStatus {
    LoggedIn,
    LoggedOut,
    Unavailable,
}

impl ProviderStatus {
    fn from_record(record: Option<&ProviderRecord>) -> Self {
        match record {
            Some(record) if record.ok && record.account.is_some() => Self::LoggedIn,
            Some(record) if record.ok => Self::LoggedOut,
            Some(_) | None => Self::Unavailable,
        }
    }

    fn human(self) -> &'static str {
        match self {
            Self::LoggedIn => "logged in",
            Self::LoggedOut => "logged out",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ProviderReport {
    kind: String,
    product_name: String,
    status: ProviderStatus,
    probed_at: Option<Timestamp>,
    plan: Option<String>,
    plan_label: Option<String>,
    account_id: Option<String>,
    sub_provider: Option<String>,
    account_scope: Option<ProviderAccountScope>,
    metered: Option<bool>,
    version: Option<String>,
    windows: Vec<RateLimitWindow>,
    extra_credits: Option<ExtraCredits>,
    reset_credits: Option<ResetCredits>,
    spending: Option<SpendTally>,
    day_budget: Option<DailyBudgetView>,
    active_sessions: u32,
}

fn assemble_reports(
    accounts: &AccountsCache,
    panels: Vec<SidebarProviderPanel>,
    provider_spending: &ProviderSpendingCache,
    filter: Option<&str>,
    all: bool,
) -> Vec<ProviderReport> {
    let known: BTreeSet<_> = rimz::agents::known_kinds().collect();
    let mut reports = Vec::new();
    let mut emitted = BTreeSet::new();
    for panel in panels {
        let kind = panel.kind.as_str();
        if !known.contains(kind)
            || filter.is_some_and(|filter| filter != kind)
            || !include_kind(kind, accounts, provider_spending, all)
        {
            continue;
        }
        emitted.insert(panel.kind.clone());
        reports.push(build_report(
            kind,
            accounts.providers.get(kind),
            Some(&panel),
            provider_spending,
        ));
    }
    for kind in rimz::agents::known_kinds() {
        if emitted.contains(kind)
            || filter.is_some_and(|filter| filter != kind)
            || !include_kind(kind, accounts, provider_spending, all)
        {
            continue;
        }
        reports.push(build_report(
            kind,
            accounts.providers.get(kind),
            None,
            provider_spending,
        ));
    }
    reports
}

fn include_kind(
    kind: &str,
    accounts: &AccountsCache,
    provider_spending: &ProviderSpendingCache,
    all: bool,
) -> bool {
    all || accounts
        .providers
        .get(kind)
        .and_then(|record| record.account.as_ref())
        .is_some()
        || provider_spending
            .spending
            .by_provider
            .get(kind)
            .is_some_and(|tally| !tally.is_zero() || tally.year.sessions > 0)
}

fn build_report(
    kind: &str,
    record: Option<&ProviderRecord>,
    panel: Option<&SidebarProviderPanel>,
    provider_spending: &ProviderSpendingCache,
) -> ProviderReport {
    let descriptor = rimz::agents::descriptor_by_kind(kind)
        .expect("reports are assembled only for registered provider kinds");
    let account = record.and_then(|record| record.account.as_ref());
    let raw_plan = account.and_then(|account| account.plan.clone());
    ProviderReport {
        kind: kind.to_owned(),
        product_name: descriptor.display_name.to_owned(),
        status: ProviderStatus::from_record(record),
        probed_at: record.and_then(|record| timestamp_from_millis(record.probed_at_ms)),
        plan_label: panel.and_then(|panel| panel.plan.clone()).or_else(|| {
            raw_plan
                .as_deref()
                .map(|plan| descriptor.plan_label.format(plan))
        }),
        plan: raw_plan,
        account_id: account.and_then(|account| account.account_id.clone()),
        sub_provider: account.and_then(|account| account.sub_provider.clone()),
        account_scope: account
            .map(|account| account.scope.clone())
            .or_else(|| panel.map(|panel| panel.account_scope.clone())),
        metered: panel
            .map(|panel| panel.metered)
            .or_else(|| account.and_then(|account| account.metered)),
        version: panel
            .and_then(|panel| panel.version.clone())
            .or_else(|| account.and_then(|account| account.version.clone())),
        windows: panel.map_or_else(Vec::new, |panel| panel.windows.clone()),
        extra_credits: panel.and_then(|panel| panel.extra_credits.clone()),
        reset_credits: panel.and_then(|panel| panel.reset_credits.clone()),
        spending: panel
            .and_then(|panel| panel.spending.clone())
            .or_else(|| provider_spending.spending.by_provider.get(kind).cloned()),
        day_budget: panel.and_then(|panel| panel.day_budget),
        active_sessions: panel.map_or(0, |panel| panel.active_sessions),
    }
}

fn timestamp_from_millis(millis: u64) -> Option<Timestamp> {
    i64::try_from(millis)
        .ok()
        .and_then(|millis| Timestamp::from_millisecond(millis).ok())
}

fn write_pretty(
    out: &mut impl Write,
    reports: &[ProviderReport],
    now: Timestamp,
    time_zone: &TimeZone,
) -> std::io::Result<()> {
    for (index, report) in reports.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        write!(
            out,
            "{} — ",
            render::paint(
                render::palette::identity(&report.kind).bold(),
                &render::palette::identity_name(&report.kind),
            )
        )?;
        write_optional(out, report.plan_label.as_deref(), "")?;
        writeln!(
            out,
            " · {}",
            render::paint(
                render::status::provider(report.status),
                report.status.human()
            )
        )?;

        let mut rows = KeyVals::new().indent(2);
        rows.push(
            "version",
            report
                .version
                .as_deref()
                .map_or_else(unknown_cell, |version| {
                    cell(format!("v{}", render::one_line(version)))
                }),
        );
        if let Some(account_id) = &report.account_id {
            rows.push("account", cell(render::one_line(account_id)));
        }
        if let Some(sub_provider) = &report.sub_provider {
            rows.push("provider", cell(render::one_line(sub_provider)));
        }
        if let Some(scope) = &report.account_scope
            && !scope.is_kind_wide()
        {
            rows.push("scope", cell(scope_label(scope)));
        }
        for window in &report.windows {
            rows.push(
                rimz::theme::fmt::window_label(window),
                value_cell(window_value(window, now)),
            );
        }
        if report.metered == Some(true) && report.windows.is_empty() {
            rows.push("usage", unknown_cell());
        } else if report.metered == Some(false) {
            rows.push("usage", cell("∞"));
        }
        if let Some(extra) = &report.extra_credits {
            match extra_credit_spans(extra) {
                Some(spans) => rows.push_spans("extra", spans),
                None => rows.push("extra", unknown_cell()),
            }
        }
        if let Some(reset) = &report.reset_credits {
            rows.push_lines("resets", reset_credit_lines(reset, now, time_zone));
        }
        if let Some(spending) = &report.spending {
            rows.push_spans(
                "spend",
                [
                    cell("7d "),
                    money_cell(spending.week.usd),
                    cell(" · 30d "),
                    money_cell(spending.month.usd),
                ],
            );
        } else {
            rows.push("spend", unknown_cell());
        }
        if let Some(budget) = report.day_budget {
            let mut spans = vec![
                money_cell(budget.spend_usd),
                cell(" of "),
                money_cell(budget.cap_usd),
                cell("/day"),
            ];
            if budget.parked {
                spans.push(cell(" · parked"));
            }
            rows.push_spans("budget", spans);
        }
        if report.active_sessions > 0 {
            rows.push("sessions", cell(report.active_sessions.to_string()));
        }
        rows.render(out)?;
    }
    Ok(())
}

fn write_optional(out: &mut impl Write, value: Option<&str>, prefix: &str) -> std::io::Result<()> {
    match value {
        Some(value) => write!(out, "{prefix}{}", render::one_line(value)),
        None => write!(out, "{}", render::paint(render::palette::faint(), "–")),
    }
}

fn value_cell(value: Option<String>) -> render::Cell {
    value.map_or_else(unknown_cell, cell)
}

fn unknown_cell() -> render::Cell {
    cell("–").fg(render::palette::faint())
}

fn scope_label(scope: &ProviderAccountScope) -> String {
    match scope {
        ProviderAccountScope::KindWide => "kind-wide".to_owned(),
        ProviderAccountScope::SubProvider { provider, variant } => {
            render::one_line(&format!("{provider}/{variant}"))
        }
    }
}

fn window_value(window: &RateLimitWindow, now: Timestamp) -> Option<String> {
    if window.lifted {
        return Some("∞".to_owned());
    }
    let used = window.used_percentage?;
    if window.not_started(now) {
        return Some(format!("{used}% used · ready"));
    }
    let reset = window
        .resets_at
        .map(|deadline| {
            format!(
                "resets in {}",
                rimz::theme::fmt::reset_countdown(deadline, now)
            )
        })
        .unwrap_or_else(|| "resets –".to_owned());
    Some(format!("{used}% used · {reset}"))
}

fn money_cell(value: f64) -> render::Cell {
    cell(rimz::theme::fmt::dollars2(value)).fg(render::palette::money())
}

fn extra_credit_spans(extra: &ExtraCredits) -> Option<Vec<render::Cell>> {
    match extra {
        ExtraCredits::Disabled => Some(vec![cell("disabled")]),
        ExtraCredits::Known {
            used_usd,
            remaining_usd,
            limit_usd,
        } => {
            let mut spans = Vec::new();
            for (value, label) in [
                (*used_usd, " used"),
                (*remaining_usd, " remaining"),
                (*limit_usd, " limit"),
            ] {
                let Some(value) = value else {
                    continue;
                };
                if !spans.is_empty() {
                    spans.push(cell(" · "));
                }
                spans.push(money_cell(value));
                spans.push(cell(label));
            }
            (!spans.is_empty()).then_some(spans)
        }
    }
}

fn reset_credit_lines(
    reset: &ResetCredits,
    now: Timestamp,
    time_zone: &TimeZone,
) -> Vec<Vec<render::Cell>> {
    let noun = if reset.count == 1 {
        "credit"
    } else {
        "credits"
    };
    let mut lines = vec![vec![cell(format!("{} {noun}", reset.count))]];
    let mut expiries = if reset.expiries.is_empty() {
        reset.soonest_expiry.into_iter().collect::<Vec<_>>()
    } else {
        reset.expiries.clone()
    };
    expiries.sort_unstable();
    for deadline in expiries
        .into_iter()
        .take(usize::try_from(reset.count).unwrap_or(usize::MAX).min(3))
    {
        let local = deadline.to_zoned(time_zone.clone());
        let relative = if deadline <= now {
            "due".to_owned()
        } else {
            format!("in {}", rimz::theme::fmt::reset_countdown(deadline, now))
        };
        lines.push(vec![cell(format!(
            "- {} · {relative}",
            local.strftime("%Y-%m-%d %H:%M:%S %:z")
        ))]);
    }
    lines
}

#[cfg(test)]
mod tests;
