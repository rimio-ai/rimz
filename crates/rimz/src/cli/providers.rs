//! `rimz providers` — account plans, auth state, limits, credits, and spend.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Serialize;

use super::GlobalFlags;
use super::render::{self, KeyVals, cell};
use super::spinner::Spinner;
use rimz::RuntimePaths;
use rimz::agents::account::{AccountsCache, ProviderRecord, ProviderStatus};
use rimz::agents::spending::{ProviderSpendingCache, read_provider_spending_cache};
use rimz::agents::{ExtraCredits, ProviderAccountScope, RateLimitWindow, ResetCredits, SpendTally};
use rimz::agents::{LoginCatalog, ProviderLogin, RoomLoginSet};
use rimz::config::MachineConfig;
use rimz::ids::{AgentKind, LoginKey, LoginName};
use rimz::room::{AccountStanding, Deciding, Scope, Scopes};
use rimz::sidebar::enrich::{provider_panel_for_login, provider_panels_from_caches};
use rimz::sidebar::refresh::{query_provider_accounts, refresh_provider_usage};
use rimz::store::snapshot::{DailyBudgetView, SidebarProviderPanel};

const SPINNER_MIN_AGE: Duration = Duration::from_millis(150);

#[derive(Debug, Args)]
pub struct ProvidersArgs {
    /// Show only one provider kind.
    #[arg(value_name = "KIND")]
    kind: Option<String>,
    /// Emit the report as JSON instead of human-readable text.
    #[arg(long)]
    json: bool,
    /// Bypass account and usage refresh TTLs.
    #[arg(long)]
    refresh: bool,
    /// Include logged-out and empty provider kinds.
    #[arg(long)]
    all: bool,
}

pub fn run(args: ProvidersArgs, globals: &GlobalFlags) -> Result<()> {
    validate_kind(args.kind.as_deref())?;
    let runtime = RuntimePaths::shared();
    runtime
        .ensure_shared_dirs()
        .context("preparing shared provider cache paths")?;
    let config = MachineConfig::load_lenient();
    let (standing, in_room) = match super::position_standing(globals, &config) {
        Ok(standing) => standing,
        Err(error) => {
            writeln!(
                std::io::stderr().lock(),
                "rimz: warning: cannot read the account selection here, so no account is marked active: {error:#}"
            )?;
            (AccountStanding::unread(&config), false)
        }
    };
    if let Some(blocked) = standing.blocked() {
        writeln!(std::io::stderr().lock(), "rimz: warning: {blocked}")?;
    }
    let spinner = (!args.json && std::io::stdout().is_terminal())
        .then(|| Spinner::delayed("Querying provider accounts", SPINNER_MIN_AGE));
    let catalog = LoginCatalog::from_config(&config.accounts).ok();
    let logins: Vec<_> = rimz::agents::known_kinds()
        .flat_map(|kind| {
            let mut logins = vec![ProviderLogin::default_for(AgentKind::new_unchecked(kind))];
            if let Some(catalog) = &catalog {
                logins.extend(
                    catalog
                        .all()
                        .filter(|login| login.kind().as_str() == kind && !login.name().is_default())
                        .cloned(),
                );
            }
            logins
        })
        .collect();
    let accounts = query_provider_accounts(&runtime, &logins, args.refresh);
    if let Some(spinner) = &spinner {
        spinner.set("Refreshing provider usage");
    }
    for login in &logins {
        let Some(record) = accounts.logins.get(&login.key()) else {
            continue;
        };
        if args
            .kind
            .as_deref()
            .is_some_and(|filter| filter != login.kind().as_str())
            || ProviderStatus::from_record(Some(record)) != ProviderStatus::LoggedIn
        {
            continue;
        }
        refresh_provider_usage(&runtime, login, args.refresh);
    }
    drop(spinner);

    let provider_spending = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    let account_facts: BTreeMap<_, _> = logins
        .iter()
        .filter(|login| login.name().is_default())
        .filter_map(|login| {
            accounts
                .logins
                .get(&login.key())?
                .account
                .clone()
                .map(|account| (login.key(), account))
        })
        .collect();
    let native_panels = provider_panels_from_caches(
        &runtime,
        &RoomLoginSet::native(),
        &config,
        account_facts,
        &provider_spending,
    );
    // Kinds with a panel keep the dashboard's usage ranking; the rest follow
    // registry order, each kind's default account ahead of its named ones.
    let mut logins = logins;
    logins.sort_by_key(|login| {
        native_panels
            .iter()
            .position(|panel| panel.kind == login.kind().as_str())
            .unwrap_or(usize::MAX)
    });
    let mut panels: BTreeMap<_, _> = native_panels
        .into_iter()
        .map(|panel| {
            (
                LoginKey::default_for(AgentKind::new_unchecked(&panel.kind)),
                panel,
            )
        })
        .collect();
    for login in logins.iter().filter(|login| !login.name().is_default()) {
        let account = accounts
            .logins
            .get(&login.key())
            .and_then(|record| record.account.clone());
        if let Some(panel) = provider_panel_for_login(
            &runtime,
            login,
            catalog.as_ref(),
            &config,
            account,
            &provider_spending,
        ) {
            panels.insert(login.key(), panel);
        }
    }
    let mut reports = assemble_reports(
        &logins,
        &accounts,
        panels,
        &provider_spending,
        args.kind.as_deref(),
        args.all,
    );
    mark_standing(&mut reports, &standing);
    if args.json {
        return render::json_pretty(&reports);
    }
    let mut out = render::out();
    let (now, time_zone) = (Timestamp::now(), config.time_zone());
    if args.kind.is_some() {
        return render::finish(write_pretty(&mut out, &reports, now, &time_zone));
    }
    let deciding = reports
        .iter()
        .filter_map(|report| {
            let deciding = standing.deciding(&AgentKind::new_unchecked(&report.kind))?;
            Some((report.kind.clone(), deciding))
        })
        .collect();
    render::finish(write_overview(&mut out, &reports, now, &deciding, in_room))
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

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ProviderReport {
    kind: String,
    account: LoginName,
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
    active: bool,
    default_for: Scopes,
}

fn assemble_reports(
    logins: &[ProviderLogin],
    accounts: &AccountsCache,
    panels: BTreeMap<LoginKey, SidebarProviderPanel>,
    provider_spending: &ProviderSpendingCache,
    filter: Option<&str>,
    all: bool,
) -> Vec<ProviderReport> {
    let named_kinds: BTreeSet<&AgentKind> = logins
        .iter()
        .filter(|login| !login.name().is_default())
        .map(ProviderLogin::kind)
        .collect();
    let mut reports = Vec::new();
    for login in logins {
        let kind = login.kind().as_str();
        if filter.is_some_and(|filter| filter != kind)
            || (login.name().is_default()
                && !named_kinds.contains(login.kind())
                && !include_kind(kind, accounts, provider_spending, all))
        {
            continue;
        }
        reports.push(build_report(
            login,
            accounts.logins.get(&login.key()),
            panels.get(&login.key()),
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
        .logins
        .get(&rimz::ids::LoginKey::default_for(
            rimz::ids::AgentKind::new_unchecked(kind),
        ))
        .and_then(|record| record.account.as_ref())
        .is_some()
        || provider_spending
            .spending
            .by_provider
            .get(kind)
            .is_some_and(|tally| !tally.is_zero() || tally.year.sessions > 0)
}

fn build_report(
    login: &ProviderLogin,
    record: Option<&ProviderRecord>,
    panel: Option<&SidebarProviderPanel>,
    provider_spending: &ProviderSpendingCache,
) -> ProviderReport {
    let kind = login.kind().as_str();
    let definition = rimz::agents::spec_by_kind(kind)
        .expect("reports are assembled only for registered provider kinds");
    let account = record.and_then(|record| record.account.as_ref());
    let raw_plan = account.and_then(|account| account.plan.clone());
    ProviderReport {
        kind: kind.to_owned(),
        account: login.name().clone(),
        product_name: panel
            .map(|panel| panel.product_name.clone())
            .unwrap_or_else(|| {
                if login.name().is_default() {
                    definition.display_name.to_owned()
                } else {
                    format!("{} · {}", definition.display_name, login.name())
                }
            }),
        status: ProviderStatus::from_record(record),
        probed_at: record.and_then(|record| timestamp_from_millis(record.probed_at_ms)),
        plan_label: panel.and_then(|panel| panel.plan.clone()).or_else(|| {
            raw_plan
                .as_deref()
                .map(|plan| definition.plan_label.format(plan))
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
        spending: panel.and_then(|panel| panel.spending.clone()).or_else(|| {
            provider_spending
                .spending
                .by_login
                .get(&login.pool())
                .cloned()
        }),
        day_budget: panel.and_then(|panel| panel.day_budget),
        active_sessions: panel.map_or(0, |panel| panel.active_sessions),
        active: false,
        default_for: Scopes::default(),
    }
}

/// Mark each report with the account a launch at the position uses and the
/// layers it is the default of.
fn mark_standing(reports: &mut [ProviderReport], standing: &AccountStanding) {
    for report in reports {
        let kind = AgentKind::new_unchecked(&report.kind);
        report.active = standing.active(&kind).as_ref() == Some(&report.account);
        report.default_for = standing.scopes(&kind, &report.account);
    }
}

/// Every kind in turn, each as one table with a row per account.
fn write_overview(
    out: &mut impl Write,
    reports: &[ProviderReport],
    now: Timestamp,
    deciding: &BTreeMap<String, Deciding>,
    in_room: bool,
) -> std::io::Result<()> {
    for (index, group) in reports.chunk_by(|a, b| a.kind == b.kind).enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        // A room's selection moves only from inside that room.
        let deciding = deciding
            .get(&group[0].kind)
            .copied()
            .filter(|deciding| in_room || *deciding != Deciding::Room);
        write_comparison(out, group, now, deciding)?;
    }
    Ok(())
}

/// A rate-limit lane's identity across accounts: its scope id, else its
/// duration.
type WindowKey<'a> = (Option<&'a str>, Option<u32>);

fn window_key(window: &RateLimitWindow) -> WindowKey<'_> {
    match &window.scope {
        Some(scope) => (Some(scope.id.as_str()), None),
        None => (None, window.duration_mins),
    }
}

fn write_comparison(
    out: &mut impl Write,
    reports: &[ProviderReport],
    now: Timestamp,
    deciding: Option<Deciding>,
) -> std::io::Result<()> {
    let kind = reports[0].kind.as_str();
    write!(
        out,
        "{}",
        render::paint(
            render::palette::identity(kind).bold(),
            &render::palette::identity_name(kind)
        )
    )?;
    match reports.iter().find_map(|report| report.version.as_deref()) {
        Some(version) => writeln!(out, " · v{}", render::one_line(version))?,
        None => writeln!(out)?,
    }
    let mut windows: Vec<(WindowKey<'_>, String)> = Vec::new();
    for window in reports.iter().flat_map(|report| &report.windows) {
        let key = window_key(window);
        if !windows.iter().any(|(seen, _)| *seen == key) {
            windows.push((
                key,
                format!("{} LEFT", rimz::theme::fmt::window_label(window)),
            ));
        }
    }
    let extra = reports.iter().any(|report| report.extra_credits.is_some());
    let credits = reports.iter().any(|report| report.reset_credits.is_some());
    let mut headers = vec!["".to_owned(), "ACCOUNT".to_owned(), "PLAN".to_owned()];
    headers.extend(windows.iter().map(|(_, label)| label.clone()));
    headers.extend(extra.then(|| "EXTRA".to_owned()));
    headers.extend(credits.then(|| "RESETS".to_owned()));
    headers.push("SPEND 7d".to_owned());
    let mut table = render::Table::new(headers);
    for report in reports {
        let mut cells = vec![
            if report.active {
                cell("●").fg(render::palette::accent())
            } else if report.default_for.contains(Scope::NewRooms) {
                cell("○").fg(render::palette::muted())
            } else {
                cell("")
            },
            if report.account.is_default() {
                cell(report.account.as_str()).fg(render::palette::muted())
            } else {
                cell(report.account.as_str())
            },
            if report.status == ProviderStatus::LoggedIn {
                value_cell(report.plan_label.as_deref().map(render::one_line))
            } else {
                cell(provider_status_label(report.status))
                    .fg(render::status::provider(report.status))
            },
        ];
        cells.extend(windows.iter().map(|(key, _)| {
            if report.metered == Some(false) {
                return cell("∞");
            }
            report
                .windows
                .iter()
                .find(|window| window_key(window) == *key)
                .and_then(|window| render::window_cell(window, now))
                .unwrap_or_else(unknown_cell)
        }));
        if extra {
            cells.push(extra_cell(report.extra_credits.as_ref()));
        }
        if credits {
            cells.push(credits_cell(report.reset_credits.as_ref(), now));
        }
        cells.push(report.spending.as_ref().map_or_else(
            || cell("-").dash(),
            |spending| money_cell(spending.week.usd),
        ));
        table.row(cells);
    }
    table.render(out)?;
    if let Some((reason, command)) = switch_hint(reports, now, deciding) {
        writeln!(out)?;
        writeln!(out, "  {reason}")?;
        writeln!(out, "    {command}")?;
    }
    Ok(())
}

fn extra_cell(extra: Option<&ExtraCredits>) -> render::Cell {
    match extra {
        Some(ExtraCredits::Disabled) => cell("off"),
        Some(ExtraCredits::Known {
            remaining_usd: Some(remaining),
            ..
        }) => money_cell(*remaining).suffix("left", render::palette::body()),
        Some(ExtraCredits::Known {
            used_usd: Some(used),
            ..
        }) => money_cell(*used).suffix("used", render::palette::body()),
        Some(ExtraCredits::Known { .. }) | None => unknown_cell(),
    }
}

fn credits_cell(reset: Option<&ResetCredits>, now: Timestamp) -> render::Cell {
    let Some(reset) = reset else {
        return unknown_cell();
    };
    if reset.count == 0 {
        return cell("-").dash();
    }
    let soonest = reset
        .expiries
        .iter()
        .min()
        .copied()
        .or(reset.soonest_expiry);
    match soonest {
        Some(deadline) if deadline <= now => cell(format!("{} · due", reset.count)),
        Some(deadline) => cell(format!(
            "{} · exp {}",
            reset.count,
            rimz::theme::fmt::reset_countdown(deadline, now)
        )),
        None => cell(reset.count.to_string()),
    }
}

/// The sentence and command that move the marker to the logged-in sibling
/// with the most room, once the active account has 20% or less left of a
/// window that sibling has strictly more of. A project-decided kind gets
/// none: no `accounts use` form moves it.
fn switch_hint(
    reports: &[ProviderReport],
    now: Timestamp,
    deciding: Option<Deciding>,
) -> Option<(String, String)> {
    let active = reports.iter().find(|report| report.active)?;
    let siblings: Vec<_> = reports
        .iter()
        .filter(|report| !report.active && report.status == ProviderStatus::LoggedIn)
        .map(|report| (&report.account, report.windows.as_slice()))
        .collect();
    let hint = rimz::agents::account::switch_hint(&active.windows, &siblings, now)?;
    let command = super::accounts_use_command(deciding, &active.kind, hint.sibling)?;
    Some((
        format!(
            "{} {} has {}% left of its {} window; {} has the most room:",
            active.kind,
            active.account,
            hint.left,
            rimz::theme::fmt::window_label(hint.window),
            hint.sibling
        ),
        command,
    ))
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
    let named_kinds: BTreeSet<&str> = reports
        .iter()
        .filter(|report| !report.account.is_default())
        .map(|report| report.kind.as_str())
        .collect();
    for (index, report) in reports.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        let named = named_kinds.contains(report.kind.as_str());
        let mut name = render::palette::identity_name(&report.kind);
        if named {
            if report.active {
                write!(out, "{} ", render::paint(render::palette::accent(), "●"))?;
            } else {
                write!(out, "  ")?;
            }
            name = format!("{name} · {}", report.account);
        }
        write!(
            out,
            "{} — ",
            render::paint(render::palette::identity(&report.kind).bold(), &name)
        )?;
        write_optional(out, report.plan_label.as_deref(), "")?;
        write!(
            out,
            " · {}",
            render::paint(
                render::status::provider(report.status),
                provider_status_label(report.status)
            )
        )?;
        let scopes = report.default_for.label();
        if named && scopes != "-" {
            write!(out, " · {scopes}")?;
        }
        writeln!(out)?;

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
            let label = rimz::theme::fmt::window_label(window);
            match window_spans(window, now) {
                Some(spans) => rows.push_spans(label, spans),
                None => rows.push(label, unknown_cell()),
            }
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
        if let Some(reset) = &report.reset_credits
            && reset.count > 0
        {
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
        } else if report.account.is_default() {
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

fn provider_status_label(status: ProviderStatus) -> &'static str {
    match status {
        ProviderStatus::LoggedIn => "logged in",
        ProviderStatus::LoggedOut => "logged out",
        ProviderStatus::Unavailable => "unavailable",
    }
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

fn window_spans(window: &RateLimitWindow, now: Timestamp) -> Option<Vec<render::Cell>> {
    if window.lifted {
        return Some(vec![cell("∞")]);
    }
    let left = window.remaining_percentage(now)?;
    let tail = if window.not_started(now) {
        "ready".to_owned()
    } else {
        window.resets_at.map_or_else(
            || "resets –".to_owned(),
            |deadline| {
                format!(
                    "resets in {}",
                    rimz::theme::fmt::reset_countdown(deadline, now)
                )
            },
        )
    };
    let mut spans = vec![
        render::percent_left_cell(left),
        cell(format!(" left · {tail}")),
    ];
    spans.extend(render::window_pace_cell(window, now));
    Some(spans)
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
