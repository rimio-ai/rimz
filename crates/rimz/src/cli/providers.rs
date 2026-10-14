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
use rimz::agents::{
    Entitlement, ExtraCredits, ProviderAccountScope, RateLimitWindow, ResetCredits, SpendWindow,
    SpendWindowMode,
};
use rimz::agents::{LoginCatalog, ProviderLogin, RoomLoginSet};
use rimz::config::MachineConfig;
use rimz::ids::{AgentKind, LoginKey};
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
    /// Expand logged-out and empty provider kinds into tables.
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
    let catalog = Some(LoginCatalog::room_view(&config.accounts).0);
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
    let now = Timestamp::now();
    let agents = rimz::room::live_agents_by_login().ok();
    let installed = rimz::agents::known_kinds()
        .filter(|kind| {
            rimz::agents::spec_by_kind(kind)
                .is_some_and(|spec| rimz::agents::locate_binary(spec).is_some())
        })
        .map(str::to_owned)
        .collect();
    let mut reports = assemble_reports(
        &logins,
        &accounts,
        panels,
        &provider_spending,
        args.kind.as_deref(),
        args.all,
        &ReportContext {
            day_cutoff_secs: now
                .to_zoned(config.time_zone())
                .start_of_day()
                .ok()
                .map(|day| day.timestamp().as_second().max(0) as u64),
            config: &config,
            agents: agents.as_ref(),
            installed: &installed,
        },
    );
    mark_standing(&mut reports, &standing);
    if !in_room {
        for report in &mut reports {
            report.active = false;
        }
    }
    if args.json {
        return render::json_pretty(&reports);
    }
    let mut out = render::out();
    let time_zone = config.time_zone();
    let deciding = reports
        .iter()
        .filter_map(|report| {
            let deciding = standing.deciding(&AgentKind::new_unchecked(&report.kind))?;
            if !in_room && deciding == Deciding::Room {
                return None;
            }
            Some((report.kind.clone(), deciding))
        })
        .collect();
    if args.kind.is_some() {
        return render::finish(write_pretty(&mut out, &reports, now, &time_zone, &deciding));
    }
    render::finish(write_overview(
        &mut out, &reports, now, &time_zone, &deciding, in_room,
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

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ProviderReport {
    kind: String,
    account: LoginKey,
    pool: LoginKey,
    product_name: String,
    status: ProviderStatus,
    entitlement: Entitlement,
    probed_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    probe_error: Option<String>,
    #[serde(skip)]
    installed: bool,
    #[serde(skip)]
    expanded: bool,
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
    spending: Option<ProviderSpend>,
    day_budget: Option<DailyBudgetView>,
    agents: Option<usize>,
    active: bool,
    default_for: Scopes,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct ProviderSpend {
    today: Option<SpendWindow>,
    headline: SpendWindow,
    window: SpendWindowMode,
    week: SpendWindow,
    month: SpendWindow,
    year: SpendWindow,
}

struct ReportContext<'a> {
    day_cutoff_secs: Option<u64>,
    config: &'a MachineConfig,
    agents: Option<&'a BTreeMap<LoginKey, usize>>,
    installed: &'a BTreeSet<String>,
}

fn assemble_reports(
    logins: &[ProviderLogin],
    accounts: &AccountsCache,
    panels: BTreeMap<LoginKey, SidebarProviderPanel>,
    provider_spending: &ProviderSpendingCache,
    filter: Option<&str>,
    all: bool,
    context: &ReportContext<'_>,
) -> Vec<ProviderReport> {
    let mut reports = Vec::new();
    for login in logins {
        let kind = login.kind().as_str();
        if filter.is_some_and(|filter| filter != kind) {
            continue;
        }
        let mut report = build_report(
            login,
            accounts.logins.get(&login.key()),
            panels.get(&login.key()),
            provider_spending,
            context,
        );
        report.expanded = all || filter.is_some();
        reports.push(report);
    }
    reports
}

fn build_report(
    login: &ProviderLogin,
    record: Option<&ProviderRecord>,
    panel: Option<&SidebarProviderPanel>,
    provider_spending: &ProviderSpendingCache,
    context: &ReportContext<'_>,
) -> ProviderReport {
    let kind = login.kind().as_str();
    let definition = rimz::agents::spec_by_kind(kind)
        .expect("reports are assembled only for registered provider kinds");
    let account = record.and_then(|record| record.account.as_ref());
    let entitlement = panel.map_or(Entitlement::Ok, |panel| panel.entitlement);
    let raw_plan = if matches!(entitlement, Entitlement::Lapsed { .. }) {
        None
    } else {
        account.and_then(|account| account.plan.clone())
    };
    ProviderReport {
        kind: kind.to_owned(),
        account: login.key(),
        pool: login.pool(),
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
        entitlement,
        probed_at: record.and_then(|record| timestamp_from_millis(record.probed_at_ms)),
        probe_error: record.and_then(|record| record.probe_error.as_ref().map(ToString::to_string)),
        installed: context.installed.contains(kind),
        expanded: false,
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
        spending: panel
            .and_then(|panel| panel.spending.as_deref())
            .or_else(|| provider_spending.spending.by_login.get(&login.pool()))
            .map(|tally| ProviderSpend {
                today: (context.day_cutoff_secs == Some(provider_spending.day_cutoff_secs)).then(
                    || {
                        provider_spending
                            .day_by_login
                            .get(&login.pool())
                            .cloned()
                            .unwrap_or_default()
                    },
                ),
                headline: tally.headline.clone(),
                window: context.config.sidebar.spend_window,
                week: tally.week.clone(),
                month: tally.month.clone(),
                year: tally.year.clone(),
            }),
        day_budget: panel.and_then(|panel| panel.day_budget),
        agents: context
            .agents
            .map(|counts| counts.get(&login.key()).copied().unwrap_or(0)),
        active: false,
        default_for: Scopes::default(),
    }
}

/// Mark each report with the account a launch at the position uses and the
/// layers it is the default of.
fn mark_standing(reports: &mut [ProviderReport], standing: &AccountStanding) {
    for report in reports {
        let kind = AgentKind::new_unchecked(&report.kind);
        report.active = standing.active(&kind).as_ref() == Some(&report.account.name);
        report.default_for = standing.scopes(&kind, &report.account.name);
    }
}

/// Account tables followed by the kinds with no login or named accounts.
fn write_overview(
    out: &mut impl Write,
    reports: &[ProviderReport],
    now: Timestamp,
    time_zone: &TimeZone,
    deciding: &BTreeMap<String, Deciding>,
    in_room: bool,
) -> std::io::Result<()> {
    let mut folded = Vec::new();
    let mut displayed = Vec::new();
    let mut printed = false;
    for group in reports.chunk_by(|a, b| a.kind == b.kind) {
        if !group.iter().any(|report| {
            report.expanded
                || !report.account.name.is_default()
                || report.status == ProviderStatus::LoggedIn
        }) {
            let report = &group[0];
            let version = report.version.as_ref().map_or_else(String::new, |version| {
                format!(" v{}", render::one_line(version))
            });
            folded.push(format!(
                "{}{version} {}",
                report.kind,
                status_label(report, false)
            ));
            continue;
        }
        if printed {
            writeln!(out)?;
        }
        write_comparison(out, group, now, time_zone)?;
        displayed.extend(group);
        printed = true;
    }
    let sources = deciding
        .iter()
        .filter(|(_, source)| in_room || **source != Deciding::Room)
        .map(|(kind, source)| (AgentKind::new_unchecked(kind), *source))
        .collect();
    write_marker_legend(out, displayed.iter().copied(), &sources)?;
    if !folded.is_empty() {
        if printed {
            writeln!(out)?;
        }
        writeln!(out, "Also: {}", folded.join(" · "))?;
    }
    Ok(())
}

fn write_marker_legend<'a>(
    out: &mut impl Write,
    reports: impl Iterator<Item = &'a ProviderReport> + Clone,
    deciding: &BTreeMap<AgentKind, Deciding>,
) -> std::io::Result<()> {
    let mut deciding = deciding.clone();
    for report in reports.clone().filter(|report| report.active) {
        deciding
            .entry(report.account.kind.clone())
            .or_insert(Deciding::Room);
    }
    let displayed = reports
        .clone()
        .filter(|report| report.active)
        .map(|report| report.kind.as_str())
        .collect();
    let machine_marker = reports
        .clone()
        .any(|report| !report.active && report.default_for.contains(Scope::NewRooms));
    if let Some(legend) = render::account_marker_legend(&displayed, &deciding, machine_marker) {
        writeln!(out, "{legend}")?;
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
    time_zone: &TimeZone,
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
    let version = reports
        .iter()
        .find(|report| report.active)
        .and_then(|report| report.version.as_deref())
        .or_else(|| {
            reports
                .iter()
                .find(|report| report.account.name.is_default())
                .and_then(|report| report.version.as_deref())
        })
        .or_else(|| reports.iter().find_map(|report| report.version.as_deref()));
    if let Some(version) = version {
        write!(out, " · v{}", render::one_line(version))?;
    }
    if let Some(spending) = reports
        .iter()
        .find(|report| report.account.name.is_default())
        .and_then(|report| report.spending.as_ref())
    {
        write!(
            out,
            " · {} spent in 7d",
            render::paint(
                render::palette::money(),
                &rimz::theme::fmt::dollars2(spending.week.usd)
            )
        )?;
    }
    let members: Vec<_> = reports
        .iter()
        .filter(|report| report.pool.name.is_default())
        .map(|report| report.account.name.as_str())
        .collect();
    if members.len() > 1 {
        write!(out, " (shared by {})", members.join(", "))?;
    }
    writeln!(out)?;
    let mut windows: Vec<(WindowKey<'_>, String)> = Vec::new();
    for window in reports.iter().flat_map(|report| &report.windows) {
        let key = window_key(window);
        if !windows.iter().any(|(seen, _)| *seen == key) {
            windows.push((key, format!("{} LEFT", window_label(window))));
        }
    }
    let extra = reports.iter().any(|report| report.extra_credits.is_some());
    let credits = reports.iter().any(|report| report.reset_credits.is_some());
    let mut headers = vec!["".to_owned(), "ACCOUNT".to_owned(), "PLAN".to_owned()];
    headers.extend(windows.iter().map(|(_, label)| label.clone()));
    headers.extend(extra.then(|| "EXTRA LEFT".to_owned()));
    headers.extend(credits.then(|| "RESET CREDITS".to_owned()));
    let standalone = reports.iter().any(|report| !report.pool.name.is_default());
    headers.extend(standalone.then(|| "SPEND 7d".to_owned()));
    let missing = reports
        .iter()
        .any(|report| missing_reading(report, now, time_zone).is_some());
    headers.extend(missing.then(|| "READING".to_owned()));
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
            if report.account.name.is_default() {
                cell(report.account.to_string()).fg(render::palette::muted())
            } else {
                cell(report.account.to_string())
            },
            if matches!(report.entitlement, Entitlement::Lapsed { .. }) {
                cell("lapsed").fg(render::status::role(render::status::StateRole::Failed))
            } else if report.status == ProviderStatus::LoggedIn {
                value_cell(report.plan_label.as_deref().map(render::one_line))
            } else {
                cell(status_label(report, true)).fg(render::status::provider(report.status))
            },
        ];
        cells.extend(windows.iter().map(|(key, _)| {
            if matches!(report.entitlement, Entitlement::Lapsed { .. }) {
                return unknown_cell();
            }
            if report.metered == Some(false) {
                return cell("no limit");
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
        if standalone {
            cells.push(if report.pool.name.is_default() {
                cell("")
            } else {
                report.spending.as_ref().map_or_else(
                    || cell("-").dash(),
                    |spending| money_cell(spending.week.usd),
                )
            });
        }
        if missing {
            cells.push(cell(
                missing_reading(report, now, time_zone).unwrap_or_default(),
            ));
        }
        table.row(cells);
    }
    table.render(out)?;
    if let Some(legend) = render::pace_legend(
        reports
            .iter()
            .filter(|report| {
                !matches!(report.entitlement, Entitlement::Lapsed { .. })
                    && report.metered != Some(false)
            })
            .flat_map(|report| &report.windows),
        now,
    ) {
        writeln!(out, "{legend}")?;
    }
    Ok(())
}

fn extra_cell(extra: Option<&ExtraCredits>) -> render::Cell {
    match extra {
        Some(ExtraCredits::Disabled) => cell("off"),
        Some(ExtraCredits::Known {
            remaining_usd: Some(remaining),
            ..
        }) => money_cell(*remaining),
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
    deciding: &BTreeMap<String, Deciding>,
) -> std::io::Result<()> {
    let named_kinds: BTreeSet<&str> = reports
        .iter()
        .filter(|report| !report.account.name.is_default())
        .map(|report| report.kind.as_str())
        .collect();
    for (index, report) in reports.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        let named = named_kinds.contains(report.kind.as_str());
        let mut name = render::palette::identity_name(&report.kind);
        if report.active {
            write!(out, "{} ", render::paint(render::palette::accent(), "●"))?;
        } else if report.default_for.contains(Scope::NewRooms) {
            write!(out, "{} ", render::paint(render::palette::muted(), "○"))?;
        } else if named {
            write!(out, "  ")?;
        }
        if named {
            name = format!("{name} · {}", report.account);
        }
        write!(
            out,
            "{} — ",
            render::paint(render::palette::identity(&report.kind).bold(), &name)
        )?;
        if matches!(report.entitlement, Entitlement::Lapsed { .. }) {
            write!(
                out,
                "{}",
                render::paint(
                    render::status::role(render::status::StateRole::Failed),
                    "lapsed"
                )
            )?;
        } else {
            write_optional(out, report.plan_label.as_deref(), "")?;
        }
        write!(
            out,
            " · {}",
            render::paint(
                render::status::provider(report.status),
                &status_label(report, true)
            )
        )?;
        let scopes = report.default_for.label();
        if named && scopes != "-" {
            write!(out, " · {scopes}")?;
        }
        writeln!(out)?;

        let mut rows = KeyVals::new().indent(2);
        rows.push(
            "probed",
            report
                .probed_at
                .map_or_else(|| cell("never"), |stamp| cell(render::rel_age(stamp, now))),
        );
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
            let label = window_label(window);
            let mut spans = window_spans(window, now).unwrap_or_else(|| vec![unknown_cell()]);
            if let Some(stamp) = window.observed_at {
                spans.push(cell(format!(" · read {}", render::rel_age(stamp, now))));
            }
            rows.push_spans(label, spans);
        }
        if let Some(reason) = missing_reading(report, now, time_zone) {
            rows.push("usage", cell(reason));
        } else if report.metered == Some(false) {
            rows.push("usage", cell("no limit"));
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
            rows.push_lines("reset credits", reset_credit_lines(reset, now, time_zone));
        }
        if let Some(spending) = &report.spending
            && (report.account.name.is_default() || !report.pool.name.is_default())
        {
            rows.push_spans(
                "spend",
                [
                    cell("7d "),
                    money_cell(spending.week.usd),
                    cell(" · 30d "),
                    money_cell(spending.month.usd),
                ],
            );
        } else if report.account.name.is_default() {
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
        rows.push(
            "agents",
            report
                .agents
                .map_or_else(unknown_cell, |count| cell(count.to_string())),
        );
        rows.render(out)?;
    }
    let sources = deciding
        .iter()
        .map(|(kind, source)| (AgentKind::new_unchecked(kind), *source))
        .collect();
    write_marker_legend(out, reports.iter(), &sources)?;
    if let Some(legend) =
        render::pace_legend(reports.iter().flat_map(|report| &report.windows), now)
    {
        writeln!(out, "{legend}")?;
    }
    Ok(())
}

fn status_label(report: &ProviderReport, cause: bool) -> String {
    if !report.installed {
        return "not installed".to_owned();
    }
    if rimz::agents::spec_by_kind(&report.kind)
        .is_some_and(|spec| spec.user_coverage.account.is_unsupported())
    {
        return "no login check".to_owned();
    }
    if report.probed_at.is_none() {
        return "not probed".to_owned();
    }
    match report.status {
        ProviderStatus::LoggedIn => "logged in".to_owned(),
        ProviderStatus::LoggedOut => "logged out".to_owned(),
        ProviderStatus::Unavailable => match report.probe_error.as_deref().filter(|_| cause) {
            Some(error) => format!("probe failed: {error}"),
            None => "probe failed".to_owned(),
        },
    }
}

fn missing_reading(
    report: &ProviderReport,
    now: Timestamp,
    time_zone: &TimeZone,
) -> Option<String> {
    if report.metered == Some(false)
        || (report.windows.is_empty()
            && rimz::agents::spec_by_kind(&report.kind)
                .is_none_or(|spec| spec.expected_windows.is_empty()))
        || report
            .windows
            .iter()
            .any(|window| window.lifted || window.remaining_percentage(now).is_some())
    {
        return None;
    }
    Some(
        report
            .windows
            .iter()
            .filter_map(|window| window.observed_at)
            .max()
            .map_or_else(
                || "never read".to_owned(),
                |stamp| {
                    format!(
                        "no reading since {}",
                        stamp.to_zoned(time_zone.clone()).strftime("%Y-%m-%d")
                    )
                },
            ),
    )
}

fn window_label(window: &RateLimitWindow) -> String {
    let label = rimz::theme::fmt::window_label(window);
    if label == "cr" {
        "AI credits".to_owned()
    } else {
        label
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
        return Some(vec![cell("no limit")]);
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
        ExtraCredits::Disabled => Some(vec![cell("off")]),
        ExtraCredits::Known {
            used_usd,
            remaining_usd,
            limit_usd,
        } => {
            let mut spans = Vec::new();
            for (value, label) in [
                (*used_usd, " used"),
                (*remaining_usd, " left"),
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
