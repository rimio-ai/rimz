//! Unified account readings and provider tables.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Serialize;

use super::super::GlobalFlags;
use super::super::render::{self, KeyVals, cell};
use super::super::spinner::Spinner;
use super::{ListArgs, login_command};
use rimz::RuntimePaths;
use rimz::agents::account::{AccountsCache, ProviderRecord, ProviderStatus};
use rimz::agents::spending::{ProviderSpendingCache, read_provider_spending_cache};
use rimz::agents::{
    AccountStatus, BirthLoginErr, Entitlement, ExtraCredits, ProviderAccountScope, RateLimitWindow,
    ResetCredits, SpendWindow, SpendWindowMode,
};
use rimz::agents::{LoginCatalog, ProviderLogin, RoomLoginSet};
use rimz::config::{AccountHistory, AccountsConfig, MachineConfig};
use rimz::harness::spend_ladder::{SpendFacts, SpendStep};
use rimz::ids::{AgentKind, LoginKey, LoginName};
use rimz::room::{AccountStanding, Deciding, Scope, Scopes};
use rimz::sidebar::enrich::{provider_panel_for_login, provider_panels_from_caches};
use rimz::sidebar::refresh::{query_provider_accounts, refresh_provider_usage};
use rimz::store::snapshot::{DailyBudgetView, SidebarProviderPanel};

const SPINNER_MIN_AGE: Duration = Duration::from_millis(150);

pub(super) fn list(args: ListArgs, target: Option<&str>, globals: &GlobalFlags) -> Result<()> {
    let (filter, name) = match target {
        Some(target) => {
            let (kind, name) = target
                .split_once('@')
                .map_or((target, None), |(kind, name)| (kind, Some(name)));
            validate_kind(kind)?;
            (Some(kind), name.map(str::parse::<LoginName>).transpose()?)
        }
        None => (None, None),
    };
    let runtime = RuntimePaths::shared();
    runtime
        .ensure_shared_dirs()
        .context("preparing shared provider cache paths")?;
    let config = MachineConfig::load_lenient();
    for (_, warning) in config.accounts.label_warnings() {
        writeln!(std::io::stderr().lock(), "rimz: warning: {warning}")?;
    }
    let (standing, _) = match super::super::position_standing(globals, &config) {
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
    let spinner = Spinner::delayed("Querying provider accounts", SPINNER_MIN_AGE);
    let (catalog, errors) = LoginCatalog::room_view(&config.accounts);
    for error in errors.values() {
        writeln!(std::io::stderr().lock(), "rimz: warning: {error}")?;
    }
    let logins: Vec<_> = rimz::agents::known_kinds()
        .flat_map(|kind| {
            let mut logins = vec![ProviderLogin::default_for(AgentKind::new_unchecked(kind))];
            logins.extend(
                catalog
                    .all()
                    .filter(|login| login.kind().as_str() == kind && !login.name().is_default())
                    .cloned(),
            );
            logins
        })
        .collect();
    if let (Some(kind), Some(name)) = (filter, name.as_ref())
        && !logins
            .iter()
            .any(|login| login.kind().as_str() == kind && login.name() == name)
    {
        let configured = logins
            .iter()
            .filter(|login| login.kind().as_str() == kind)
            .map(|login| login.name().as_str())
            .collect::<Vec<_>>()
            .join(", ");
        bail!("no {kind} account `{name}`; configured: {configured}");
    }
    let started_ms = rimz::utils::time::unix_now_ms();
    let probe_logins = RoomLoginSet::new(None, Some(catalog.clone()), rimz::agents::ambient_env());
    let mut accounts = query_provider_accounts(&runtime, &logins, &probe_logins, args.refresh);
    let stale: Vec<_> = logins
        .iter()
        .filter(|login| {
            config.accounts.named(login.kind()).is_some()
                && accounts.logins.get(&login.key()).is_some_and(|record| {
                    record.probed_at_ms < started_ms
                        && (record.login_is_ambiguous()
                            || ProviderStatus::from_record(Some(record))
                                == ProviderStatus::LoggedOut)
                })
        })
        .cloned()
        .collect();
    if !stale.is_empty() && !args.refresh {
        accounts = query_provider_accounts(&runtime, &stale, &probe_logins, true);
    }
    spinner.set("Refreshing provider usage");
    for login in &logins {
        let Some(record) = accounts.logins.get(&login.key()) else {
            continue;
        };
        if filter.is_some_and(|filter| filter != login.kind().as_str())
            || ProviderStatus::from_record(Some(record)) != ProviderStatus::LoggedIn
        {
            continue;
        }
        refresh_provider_usage(&runtime, login, &probe_logins, args.refresh);
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
    // Configured kinds lead, then cached 30-day use across all accounts. Stable
    // ties keep registry order and each kind's default-then-named accounts.
    let mut logins = logins;
    logins.sort_by_key(|login| {
        provider_order(
            login.kind(),
            &config.theme.display.provider_list,
            &provider_spending,
        )
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
            Some(&catalog),
            &config,
            account,
            &provider_spending,
        ) {
            panels.insert(login.key(), panel);
        }
    }
    let now = Timestamp::now();
    let agents = match rimz::room::live_agents_by_login() {
        Ok(agents) => Some(agents),
        Err(error) => {
            writeln!(
                std::io::stderr().lock(),
                "rimz: warning: cannot count live agents per account: {error}"
            )?;
            None
        }
    };
    let installed = rimz::agents::known_kinds()
        .filter(|kind| {
            rimz::agents::spec_by_kind(kind)
                .is_some_and(|spec| rimz::agents::locate_binary(spec).is_some())
        })
        .map(str::to_owned)
        .collect();
    let reports = assemble_reports(
        &logins,
        &accounts,
        panels,
        &provider_spending,
        filter,
        &ReportContext {
            day_cutoff_secs: now
                .to_zoned(config.time_zone())
                .start_of_day()
                .ok()
                .map(|day| day.timestamp().as_second().max(0) as u64),
            config: &config,
            installed: &installed,
            spend_step: &|key| SpendFacts::read(&runtime, key, now).step(&config.resume, None, now),
        },
    );
    let readings = reports
        .iter()
        .filter(|report| accounts.logins.contains_key(&report.account))
        .map(|report| {
            (
                report.account.clone(),
                LoginReading {
                    status: report.status,
                    entitlement: report.entitlement,
                    metered: report.metered,
                    windows: report.windows.clone(),
                    extra_credits: report.extra_credits.clone(),
                    reset_credits: report.reset_credits.clone(),
                    spend_step: report.spend_step,
                },
            )
        })
        .collect();
    let mut rows = account_rows(
        &config.accounts,
        &catalog,
        &standing,
        &rimz::agents::ambient_env(),
        agents.as_ref(),
        &readings,
        &errors,
    );
    for row in &mut rows {
        if let Some(report) = reports.iter().find(|report| report.account == row.account) {
            row.apply(report);
        } else {
            row.installed = installed.contains(row.kind.as_str());
        }
    }
    let multiple = multiple_account_kinds(&rows);
    rows.retain(|row| {
        filter.is_none_or(|kind| row.kind.as_str() == kind)
            && name.as_ref().is_none_or(|name| row.account.name == *name)
    });
    rows.sort_by_key(|row| {
        logins
            .iter()
            .position(|login| login.kind() == &row.kind)
            .unwrap_or(usize::MAX)
    });
    if args.json {
        return render::json_pretty(&rows);
    }
    let deciding = rows
        .iter()
        .filter_map(|row| Some((row.kind.clone(), standing.deciding(&row.kind)?)))
        .collect();
    let mut out = render::out();
    if target.is_some() {
        return render::finish(write_pretty(
            &mut out,
            &rows,
            now,
            &config.time_zone(),
            &deciding,
            &multiple,
        ));
    }
    render::finish(write_overview(
        &mut out,
        &rows,
        now,
        &config.time_zone(),
        &deciding,
        args.all,
    ))
}

fn provider_order(
    kind: &AgentKind,
    provider_list: &[String],
    spending: &ProviderSpendingCache,
) -> (usize, Reverse<u32>) {
    let configured = provider_list
        .iter()
        .position(|entry| entry == kind.as_str())
        .unwrap_or(usize::MAX);
    let sessions = spending
        .spending
        .by_provider
        .get(kind.as_str())
        .map_or(0, |tally| tally.month.sessions);
    (configured, Reverse(sessions))
}

fn validate_kind(kind: &str) -> Result<()> {
    let known: Vec<_> = rimz::agents::known_kinds().collect();
    if known.contains(&kind) {
        return Ok(());
    }
    bail!(
        "unknown provider kind `{kind}`; known kinds: {}",
        known.join(", ")
    )
}

struct ProviderReport {
    account: LoginKey,
    pool: LoginKey,
    product_name: String,
    status: ProviderStatus,
    entitlement: Entitlement,
    probed_at: Option<Timestamp>,
    probe_error: Option<String>,
    installed: bool,
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
    spend_step: SpendStep,
    spending: Option<ProviderSpend>,
    day_budget: Option<DailyBudgetView>,
    month_budget: Option<DailyBudgetView>,
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
    installed: &'a BTreeSet<String>,
    spend_step: &'a dyn Fn(&LoginKey) -> SpendStep,
}

fn assemble_reports(
    logins: &[ProviderLogin],
    accounts: &AccountsCache,
    panels: BTreeMap<LoginKey, SidebarProviderPanel>,
    provider_spending: &ProviderSpendingCache,
    filter: Option<&str>,
    context: &ReportContext<'_>,
) -> Vec<ProviderReport> {
    let mut reports = Vec::new();
    for login in logins {
        let kind = login.kind().as_str();
        if filter.is_some_and(|filter| filter != kind) {
            continue;
        }
        let report = build_report(
            login,
            accounts.logins.get(&login.key()),
            panels.get(&login.key()),
            provider_spending,
            context,
        );
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
    let undecided = panel.is_some_and(|panel| panel.undecided)
        || account.is_some_and(|account| account.is_undecided());
    let entitlement = panel.map_or(Entitlement::Ok, |panel| panel.entitlement);
    let raw_plan = if undecided || matches!(entitlement, Entitlement::Lapsed { .. }) {
        None
    } else {
        account.and_then(|account| account.plan.clone())
    };
    ProviderReport {
        account: login.key(),
        pool: login.pool(),
        product_name: panel
            .map(|panel| panel.product_name.clone())
            .unwrap_or_else(|| {
                let identity =
                    rimz::theme::resolve_provider_identity(kind, &context.config.theme.providers);
                context
                    .config
                    .accounts
                    .provider_title(&identity.product_name, &login.key())
            }),
        status: ProviderStatus::from_record(record),
        entitlement,
        probed_at: record.and_then(|record| timestamp_from_millis(record.probed_at_ms)),
        probe_error: record.and_then(|record| record.probe_error.as_ref().map(ToString::to_string)),
        installed: context.installed.contains(kind),
        plan_label: panel
            .filter(|_| !undecided)
            .and_then(|panel| panel.plan.clone())
            .or_else(|| {
                raw_plan.as_deref().map(|plan| {
                    definition.plan_label.format(
                        plan,
                        account.and_then(|account| account.plan_name.as_deref()),
                    )
                })
            }),
        plan: raw_plan,
        account_id: account.and_then(|account| account.account_id.clone()),
        sub_provider: account.and_then(|account| account.sub_provider.clone()),
        account_scope: account
            .map(|account| account.scope.clone())
            .or_else(|| panel.map(|panel| panel.account_scope.clone())),
        metered: panel
            .map(|panel| panel.metered)
            .or_else(|| account.and_then(|account| account.metered))
            .filter(|_| !undecided),
        version: panel
            .and_then(|panel| panel.version.clone())
            .or_else(|| account.and_then(|account| account.version.clone())),
        windows: panel.map_or_else(Vec::new, |panel| panel.windows.clone()),
        extra_credits: panel.and_then(|panel| panel.extra_credits.clone()),
        reset_credits: panel.and_then(|panel| panel.reset_credits.clone()),
        spend_step: (context.spend_step)(&login.key()),
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
        month_budget: panel.and_then(|panel| panel.month_budget),
    }
}

#[derive(Serialize)]
struct AccountRow {
    kind: AgentKind,
    account: LoginKey,
    home: Option<PathBuf>,
    /// `None` for `default` and for a selected account nothing declares.
    history: Option<AccountHistory>,
    label: Option<String>,
    machine_default: bool,
    /// A setup or subscription-access problem, with its fix when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    problem: Option<String>,
    #[serde(skip)]
    setup_error: Option<BirthLoginErr>,
    #[serde(skip)]
    login_command: Option<String>,
    status: AccountStatus,
    entitlement: Entitlement,
    /// A launch from here uses this account for its kind.
    active: bool,
    default_for: Scopes,
    /// Live agents on this account across every live room; `None` when the
    /// rooms could not be inventoried.
    agents: Option<usize>,
    /// Every reported provider window, including model-scoped limits.
    windows: Vec<RateLimitWindow>,
    /// Paid usage beyond the subscription.
    #[serde(skip_serializing_if = "Option::is_none")]
    extra_credits: Option<ExtraCredits>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_credits: Option<ResetCredits>,
    /// Where the account's spend ladder stands; `None` without a reading.
    spend_step: Option<SpendStep>,
    /// `false` for an account without subscription windows.
    metered: Option<bool>,
    /// This run's probe outcome; `None` when no record is available.
    login: Option<ProviderStatus>,
    pool: LoginKey,
    product_name: String,
    probed_at: Option<Timestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    probe_error: Option<String>,
    plan: Option<String>,
    plan_label: Option<String>,
    account_id: Option<String>,
    sub_provider: Option<String>,
    account_scope: Option<ProviderAccountScope>,
    version: Option<String>,
    spending: Option<ProviderSpend>,
    day_budget: Option<DailyBudgetView>,
    month_budget: Option<DailyBudgetView>,
    #[serde(skip)]
    installed: bool,
}

impl AccountRow {
    fn apply(&mut self, report: &ProviderReport) {
        self.entitlement = report.entitlement;
        self.metered = report.metered;
        self.windows.clone_from(&report.windows);
        self.extra_credits.clone_from(&report.extra_credits);
        self.reset_credits.clone_from(&report.reset_credits);
        self.pool = report.pool.clone();
        self.product_name = report.product_name.clone();
        self.probed_at = report.probed_at;
        self.probe_error = report.probe_error.clone();
        self.plan = report.plan.clone();
        self.plan_label = report.plan_label.clone();
        self.account_id = report.account_id.clone();
        self.sub_provider = report.sub_provider.clone();
        self.account_scope = report.account_scope.clone();
        self.version = report.version.clone();
        self.spending = report.spending.clone();
        self.day_budget = report.day_budget;
        self.month_budget = report.month_budget;
        self.installed = report.installed;
    }

    /// Fold login knowledge into healthy setup; setup problems stand because
    /// they must be fixed first. Only a confident login leaves it ready.
    fn read(&mut self, reading: Option<&LoginReading>, login_command: impl FnOnce() -> String) {
        self.login = reading.map(|reading| reading.status);
        if self.status == AccountStatus::Ready
            && !matches!(
                self.login,
                Some(ProviderStatus::LoggedIn | ProviderStatus::LoggedOut)
            )
        {
            self.status = AccountStatus::LoginUnknown;
        }
        let Some(reading) = reading else {
            return;
        };
        self.entitlement = reading.entitlement;
        self.metered = reading.metered;
        self.windows.clone_from(&reading.windows);
        self.extra_credits.clone_from(&reading.extra_credits);
        self.reset_credits.clone_from(&reading.reset_credits);
        self.spend_step = Some(reading.spend_step);
        if !setup_problem(self) && matches!(self.entitlement, Entitlement::Lapsed { .. }) {
            self.problem = Some(format!(
                "plan lapsed: no {} access",
                render::palette::identity_name(self.kind.as_str())
            ));
            return;
        }
        if self.status == AccountStatus::Ready
            && reading.status == ProviderStatus::LoggedOut
            && rimz::agents::spec_by_kind(self.kind.as_str())
                .is_some_and(|spec| !spec.user_coverage.account.is_unsupported())
        {
            self.status = AccountStatus::LoggedOut;
            let command = login_command();
            self.problem = Some(format!(
                "{} account `{}` is logged out; log in once: {}",
                self.kind, self.account.name, command
            ));
            self.login_command = Some(command);
        }
    }

    /// Whether this run's record says the account has no login, or the row
    /// has no login at all: its windows are not its quota then.
    fn without_login(&self) -> bool {
        matches!(self.login, None | Some(ProviderStatus::LoggedOut))
    }

    fn state(&self) -> RowState {
        if !self.installed {
            return RowState::NotInstalled;
        }
        if setup_problem(self) {
            return RowState::SetupProblem;
        }
        if matches!(self.entitlement, Entitlement::Lapsed { .. }) {
            return RowState::Lapsed;
        }
        if rimz::agents::spec_by_kind(self.kind.as_str())
            .is_some_and(|spec| spec.user_coverage.account.is_unsupported())
        {
            return RowState::NoLoginCheck;
        }
        if self.login == Some(ProviderStatus::LoggedOut) {
            return RowState::LoggedOut;
        }
        if self.probed_at.is_none() {
            return RowState::NotProbed;
        }
        match self.login {
            Some(ProviderStatus::LoggedIn) => RowState::LoggedIn,
            _ => RowState::ProbeFailed,
        }
    }
}

enum RowState {
    NotInstalled,
    SetupProblem,
    Lapsed,
    NoLoginCheck,
    LoggedOut,
    NotProbed,
    LoggedIn,
    ProbeFailed,
}

/// What one run read about a login: its probe record and its panel.
struct LoginReading {
    status: ProviderStatus,
    entitlement: Entitlement,
    metered: Option<bool>,
    windows: Vec<RateLimitWindow>,
    extra_credits: Option<ExtraCredits>,
    reset_credits: Option<ResetCredits>,
    spend_step: SpendStep,
}

/// Every known provider account, `default` first,
/// plus a row for each account some layer selects but nothing declares.
fn account_rows(
    accounts: &AccountsConfig,
    catalog: &LoginCatalog,
    standing: &AccountStanding,
    ambient: &BTreeMap<String, String>,
    agents: Option<&BTreeMap<LoginKey, usize>>,
    readings: &BTreeMap<LoginKey, LoginReading>,
    errors: &BTreeMap<AgentKind, rimz::agents::LoginConfigErr>,
) -> Vec<AccountRow> {
    let row = |kind: &AgentKind, name: &LoginName, home, problem: Option<BirthLoginErr>| {
        let key = LoginKey {
            kind: kind.clone(),
            name: name.clone(),
        };
        AccountRow {
            kind: kind.clone(),
            account: key.clone(),
            home,
            history: accounts
                .named(kind)
                .and_then(|declared| declared.get(name))
                .map(|account| account.history),
            label: accounts.label(&key).map(str::to_owned),
            machine_default: accounts.use_accounts.get(kind).cloned().unwrap_or_default() == *name,
            status: AccountStatus::of(problem.as_ref()),
            entitlement: Entitlement::Ok,
            problem: problem.as_ref().map(ToString::to_string),
            setup_error: problem,
            login_command: None,
            active: standing.active(kind).as_ref() == Some(name),
            default_for: standing.scopes(kind, name),
            agents: agents.map(|agents| agents.get(&key).copied().unwrap_or_default()),
            windows: Vec::new(),
            extra_credits: None,
            reset_credits: None,
            spend_step: None,
            metered: None,
            login: None,
            pool: catalog.pool(&key),
            product_name: accounts
                .provider_title(&render::palette::identity_name(kind.as_str()), &key),
            probed_at: None,
            probe_error: None,
            plan: None,
            plan_label: None,
            account_id: None,
            sub_provider: None,
            account_scope: None,
            version: None,
            spending: None,
            day_budget: None,
            month_budget: None,
            installed: false,
        }
    };
    let mut rows: Vec<AccountRow> = catalog
        .all()
        .map(|login| {
            let mut row = row(
                login.kind(),
                login.name(),
                login.home_dir(ambient),
                login.health(ambient).err(),
            );
            row.read(readings.get(&login.key()), || login_command(login, ambient));
            row
        })
        .collect();
    for key in standing.selected() {
        let problem = if standing.machine_selects(&key.kind, &key.name) {
            catalog.select_machine(&key.kind, &key.name).err()
        } else {
            catalog
                .select(&key.kind, &key.name)
                .err()
                .map(BirthLoginErr::from)
        };
        if let Some(problem) = problem {
            rows.push(row(&key.kind, &key.name, None, Some(problem)));
        }
    }
    for (kind, error) in errors {
        rows.retain(|row| row.kind != *kind || !row.account.name.is_default());
        for row in rows.iter_mut().filter(|row| row.kind == *kind) {
            row.active = false;
        }
        let login = ProviderLogin::default_for(kind.clone());
        let mut failed = row(kind, login.name(), login.home_dir(ambient), None);
        failed.active = false;
        failed.machine_default = false;
        failed.default_for = Scopes::default();
        failed.status = AccountStatus::Unavailable;
        failed.login = readings.get(&login.key()).map(|reading| reading.status);
        failed.problem = Some(error.to_string());
        rows.push(failed);
    }
    rows.sort_by(|a, b| {
        (&a.kind, !a.account.name.is_default(), &a.account.name).cmp(&(
            &b.kind,
            !b.account.name.is_default(),
            &b.account.name,
        ))
    });
    rows
}

fn multiple_account_kinds(rows: &[AccountRow]) -> BTreeSet<AgentKind> {
    rows.chunk_by(|a, b| a.kind == b.kind)
        .filter(|group| group.len() > 1)
        .map(|group| group[0].kind.clone())
        .collect()
}

/// Account choices and readings, followed by other logins and their repairs.
fn write_overview(
    out: &mut impl Write,
    reports: &[AccountRow],
    now: Timestamp,
    time_zone: &TimeZone,
    deciding: &BTreeMap<AgentKind, Deciding>,
    all: bool,
) -> std::io::Result<()> {
    let mut single_logins = Vec::new();
    let mut marker_rows = Vec::new();
    let mut printed = false;
    for group in reports.chunk_by(|a, b| a.kind == b.kind) {
        let reading = group.iter().any(|row| {
            !row.without_login()
                && (!row.windows.is_empty()
                    || row.extra_credits.is_some()
                    || row.reset_credits.is_some())
        });
        if group.len() == 1 && !reading {
            single_logins.push(&group[0]);
            continue;
        }
        if printed {
            writeln!(out)?;
        }
        write_comparison(out, group, now, time_zone)?;
        if group.len() > 1 {
            marker_rows.extend(group);
        }
        printed = true;
    }
    if let Some(legend) = marker_legend(marker_rows.iter().copied(), deciding) {
        if printed {
            writeln!(out)?;
        }
        writeln!(out, "{legend}")?;
        printed = true;
    }
    let other: Vec<_> = single_logins
        .iter()
        .copied()
        .filter(|row| all || matches!(row.state(), RowState::LoggedIn))
        .collect();
    if !other.is_empty() {
        if printed {
            writeln!(out)?;
        }
        write_other_providers(out, &other, now, time_zone)?;
        printed = true;
    }
    if !all && let Some(unchecked) = unchecked_logins(&single_logins) {
        if printed && other.is_empty() {
            writeln!(out)?;
        }
        writeln!(
            out,
            "{}",
            render::paint(render::palette::faint(), &unchecked)
        )?;
        printed = true;
    }
    let mut needs: Vec<_> = reports
        .iter()
        .filter(|row| row.installed)
        .filter_map(|row| row.problem.as_deref().map(|problem| (row, problem)))
        .collect();
    needs.sort_by_key(|(row, _)| {
        if setup_problem(row) {
            0
        } else if matches!(row.entitlement, Entitlement::Lapsed { .. }) {
            1
        } else {
            2
        }
    });
    if !needs.is_empty() {
        if printed {
            writeln!(out)?;
        }
        writeln!(out, "Needs you")?;
        let mut table = render::Table::new(["ACCOUNT", "STATE", "FIX"])
            .without_headers()
            .indent(3);
        for (row, problem) in needs {
            let account = if row.account.name.is_default() {
                row.kind.to_string()
            } else {
                row.account.to_string()
            };
            let fix = if setup_problem(row) {
                row.setup_error
                    .as_ref()
                    .map_or_else(|| problem.to_owned(), BirthLoginErr::repair)
            } else if matches!(row.entitlement, Entitlement::Lapsed { .. }) {
                format!(
                    "no {} access",
                    render::palette::identity_name(row.kind.as_str())
                )
            } else {
                row.login_command.as_ref().map_or_else(
                    || problem.to_owned(),
                    |command| format!("log in once: {command}"),
                )
            };
            table.row([
                cell(account),
                cell(status_label(row, false)).fg(plan_tone(row)),
                cell(fix),
            ]);
        }
        table.render(out)?;
        printed = true;
    }
    if !printed {
        writeln!(
            out,
            "No provider is set up here; `rimz accounts --all` lists every supported one."
        )?;
    }
    Ok(())
}

fn write_other_providers(
    out: &mut impl Write,
    reports: &[&AccountRow],
    now: Timestamp,
    time_zone: &TimeZone,
) -> std::io::Result<()> {
    writeln!(out, "Other providers")?;
    let missing = reports
        .iter()
        .any(|row| missing_reading(row, now, time_zone).is_some());
    let spend = reports.iter().any(|row| {
        row.spending
            .as_ref()
            .is_some_and(|spending| rimz::theme::fmt::dollars2(spending.week.usd) != "$0.00")
    });
    let mut headers = vec!["PROVIDER", "LOGIN"];
    headers.extend(missing.then_some("READING"));
    headers.push("AGENTS");
    headers.extend(spend.then_some("SPEND 7d"));
    headers.push("VERSION");
    let mut table = render::Table::new(headers).indent(3);
    for row in reports {
        let login = if matches!(row.state(), RowState::LoggedIn) && row.plan_label.is_some() {
            plan_cell(row)
        } else {
            cell(status_label(row, false)).fg(plan_tone(row))
        };
        let mut cells = vec![
            cell(render::palette::identity_name(row.kind.as_str()))
                .fg(render::palette::identity(row.kind.as_str())),
            login,
        ];
        if missing {
            cells.push(cell(
                missing_reading(row, now, time_zone).unwrap_or_default(),
            ));
        }
        cells.push(match row.agents {
            None => unknown_cell(),
            Some(0) => cell("-").dash(),
            Some(count) => cell(count.to_string()),
        });
        if spend {
            cells.push(row.spending.as_ref().map_or_else(
                || cell("-").dash(),
                |spending| money_cell(spending.week.usd),
            ));
        }
        cells.push(row.version.as_deref().map_or_else(unknown_cell, |version| {
            cell(format!("v{}", render::one_line(version)))
        }));
        table.row(cells);
    }
    table.render(out)
}

fn unchecked_logins(reports: &[&AccountRow]) -> Option<String> {
    let mut failures: Vec<(Option<&str>, Vec<&str>)> = Vec::new();
    let mut unprobed = Vec::new();
    let mut unsupported = Vec::new();
    for row in reports.iter().filter(|row| row.installed) {
        let kind = row.kind.as_str();
        match row.state() {
            RowState::ProbeFailed => {
                let cause = row.probe_error.as_deref();
                if let Some((_, kinds)) = failures.iter_mut().find(|(seen, _)| *seen == cause) {
                    kinds.push(kind);
                } else {
                    failures.push((cause, vec![kind]));
                }
            }
            RowState::NotProbed => unprobed.push(kind),
            RowState::NoLoginCheck => unsupported.push(kind),
            _ => {}
        }
    }
    let mut groups = Vec::new();
    for (cause, mut kinds) in failures {
        kinds.sort_unstable();
        let reason = cause.map_or_else(
            || "login read failed".to_owned(),
            |cause| format!("login read failed: {}", render::one_line(cause)),
        );
        groups.push(format!("{} ({reason})", kinds.join(", ")));
    }
    for (mut kinds, reason) in [
        (unprobed, "not probed yet"),
        (unsupported, "no login check"),
    ] {
        if !kinds.is_empty() {
            kinds.sort_unstable();
            groups.push(format!("{} ({reason})", kinds.join(", ")));
        }
    }
    (!groups.is_empty()).then(|| format!("Not checked, launches proceed: {}", groups.join(" · ")))
}

fn write_problems<'a>(
    out: &mut impl Write,
    rows: impl Iterator<Item = &'a AccountRow>,
) -> std::io::Result<()> {
    for row in rows.filter(|row| row.installed) {
        let Some(problem) = &row.problem else {
            continue;
        };
        let style = if matches!(row.entitlement, Entitlement::Lapsed { .. }) {
            render::status::role(render::status::StateRole::Failed)
        } else {
            render::palette::warn()
        };
        writeln!(out, "{}", render::paint(style, problem))?;
    }
    Ok(())
}

fn marker_legend<'a>(
    reports: impl Iterator<Item = &'a AccountRow> + Clone,
    deciding: &BTreeMap<AgentKind, Deciding>,
) -> Option<String> {
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
    render::account_marker_legend(&displayed, &deciding, machine_marker)
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
    reports: &[AccountRow],
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
    let standalone = reports.iter().any(|report| !report.pool.name.is_default());
    if let Some(spending) = reports
        .iter()
        .find(|report| report.account.name.is_default())
        .and_then(|report| report.spending.as_ref())
        .filter(|spending| rimz::theme::fmt::dollars2(spending.week.usd) != "$0.00")
    {
        write!(
            out,
            " · {}{} spent in 7d",
            if standalone { "shared pool " } else { "" },
            render::paint(
                render::palette::money(),
                &rimz::theme::fmt::dollars2(spending.week.usd)
            )
        )?;
        let members: Vec<_> = reports
            .iter()
            .filter(|report| report.pool.name.is_default())
            .map(|report| report.account.name.as_str())
            .collect();
        if !standalone && members.len() > 1 {
            write!(out, " (shared by {})", members.join(", "))?;
        }
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
    let labelled = reports.iter().any(|report| report.label.is_some());
    let mut headers = vec!["".to_owned(), "NAME".to_owned()];
    headers.extend(labelled.then(|| "LABEL".to_owned()));
    headers.push("PLAN".to_owned());
    headers.extend(windows.iter().map(|(_, label)| label.clone()));
    headers.extend(extra.then(|| "EXTRA LEFT".to_owned()));
    headers.extend(credits.then(|| "RESET CREDITS".to_owned()));
    headers.push("AGENTS".to_owned());
    headers.extend(standalone.then(|| "SPEND 7d".to_owned()));
    let missing = reports
        .iter()
        .any(|report| missing_reading(report, now, time_zone).is_some());
    headers.extend(missing.then(|| "READING".to_owned()));
    let multiple = reports.len() > 1;
    // A marker column without markers has no width; one space keeps the 3-column indent.
    let mut table = render::Table::new(headers).indent(usize::from(!multiple));
    let mut columns: Vec<_> = windows
        .iter()
        .map(|(key, _)| {
            render::window_cells(
                reports.iter().map(|report| {
                    if report.without_login()
                        || matches!(report.entitlement, Entitlement::Lapsed { .. })
                    {
                        return None;
                    }
                    report
                        .windows
                        .iter()
                        .find(|window| window_key(window) == *key)
                }),
                now,
            )
            .into_iter()
        })
        .collect();
    for report in reports {
        let mut cells = vec![
            if multiple && report.active {
                cell("●").fg(render::palette::accent())
            } else if multiple && report.default_for.contains(Scope::NewRooms) {
                cell("○").fg(render::palette::muted())
            } else {
                cell("")
            },
            if report.account.name.is_default() {
                cell(report.account.name.as_str()).fg(render::palette::muted())
            } else {
                cell(report.account.name.as_str())
            },
        ];
        if labelled {
            cells.push(cell(report.label.as_deref().unwrap_or_default()));
        }
        cells.push(plan_cell(report));
        cells.extend(columns.iter_mut().map(|column| {
            let value = column.next().flatten();
            if report.without_login() || matches!(report.entitlement, Entitlement::Lapsed { .. }) {
                unknown_cell()
            } else if report.metered == Some(false) {
                cell("no limit")
            } else {
                value.unwrap_or_else(unknown_cell)
            }
        }));
        if extra {
            cells.push(if report.without_login() {
                unknown_cell()
            } else {
                render::extra_credits_cell(
                    report.extra_credits.as_ref(),
                    report.spend_step == Some(SpendStep::PaidCredit),
                )
            });
        }
        if credits {
            cells.push(if report.without_login() {
                unknown_cell()
            } else {
                credits_cell(report.reset_credits.as_ref(), now)
            });
        }
        cells.push(match report.agents {
            None => unknown_cell(),
            Some(0) => cell("-").dash(),
            Some(count) => cell(count.to_string()),
        });
        if standalone {
            cells.push(if report.pool.name.is_default() {
                cell("shared")
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
    table.render(out)
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
    reports: &[AccountRow],
    now: Timestamp,
    time_zone: &TimeZone,
    deciding: &BTreeMap<AgentKind, Deciding>,
    multiple: &BTreeSet<AgentKind>,
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
        let markers = multiple.contains(&report.kind);
        let mut name = render::palette::identity_name(report.kind.as_str());
        if markers && report.active {
            write!(out, "{} ", render::paint(render::palette::accent(), "●"))?;
        } else if markers && report.default_for.contains(Scope::NewRooms) {
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
            render::paint(
                render::palette::identity(report.kind.as_str()).bold(),
                &name
            )
        )?;
        if report.installed
            && !setup_problem(report)
            && matches!(report.entitlement, Entitlement::Lapsed { .. })
        {
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
        if !report.installed
            || !matches!(report.entitlement, Entitlement::Lapsed { .. })
            || setup_problem(report)
        {
            write!(
                out,
                " · {}",
                render::paint(plan_tone(report), &status_label(report, true))
            )?;
        }
        let scopes = report.default_for.label();
        if named && scopes != "-" {
            write!(out, " · {scopes}")?;
        }
        writeln!(out)?;

        let mut rows = KeyVals::new().indent(2);
        if let Some(label) = &report.label {
            rows.push("label", cell(label));
        }
        rows.push(
            "probed",
            report
                .probed_at
                .map_or_else(|| cell("never"), |stamp| cell(render::rel_age(stamp, now))),
        );
        rows.push(
            "home",
            report.home.as_ref().map_or_else(unknown_cell, |home| {
                cell(render::home_relative(&home.display().to_string()))
            }),
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
            let mut spans = extra_credit_spans(extra).unwrap_or_else(|| vec![unknown_cell()]);
            if report.spend_step == Some(SpendStep::PaidCredit) {
                spans.push(cell(" · paying"));
            }
            rows.push_spans("extra", spans);
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
        if let Some((budget, window)) = report
            .day_budget
            .map(|budget| (budget, "day"))
            .or_else(|| report.month_budget.map(|budget| (budget, "month")))
        {
            let mut spans = vec![
                money_cell(budget.spend_usd),
                cell(" of "),
                money_cell(budget.cap_usd),
                cell(format!("/{window}")),
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
    if !reports.is_empty() {
        writeln!(out)?;
    }
    if let Some(legend) = marker_legend(
        reports.iter().filter(|row| multiple.contains(&row.kind)),
        deciding,
    ) {
        writeln!(out, "{legend}")?;
    }
    write_problems(out, reports.iter())
}

fn setup_problem(report: &AccountRow) -> bool {
    !matches!(
        report.status,
        AccountStatus::Ready | AccountStatus::LoggedOut | AccountStatus::LoginUnknown
    )
}

fn plan_tone(report: &AccountRow) -> anstyle::Style {
    if !report.installed {
        return render::status::provider(ProviderStatus::Unavailable);
    }
    if setup_problem(report) {
        return render::status::account(report.status);
    }
    if matches!(report.entitlement, Entitlement::Lapsed { .. }) {
        return render::status::role(render::status::StateRole::Failed);
    }
    render::status::provider(report.login.unwrap_or(ProviderStatus::Unavailable))
}

fn plan_cell(report: &AccountRow) -> render::Cell {
    if report.installed
        && !setup_problem(report)
        && !matches!(report.entitlement, Entitlement::Lapsed { .. })
        && report.login == Some(ProviderStatus::LoggedIn)
        && report.probed_at.is_some()
        && rimz::agents::spec_by_kind(report.kind.as_str())
            .is_some_and(|spec| !spec.user_coverage.account.is_unsupported())
    {
        return value_cell(report.plan_label.as_deref().map(render::one_line));
    }
    cell(status_label(report, false)).fg(plan_tone(report))
}

fn status_label(report: &AccountRow, cause: bool) -> String {
    match report.state() {
        RowState::NotInstalled => "not installed".to_owned(),
        RowState::SetupProblem => report.status.as_str().to_owned(),
        RowState::Lapsed => "lapsed".to_owned(),
        RowState::NoLoginCheck => "no login check".to_owned(),
        RowState::LoggedOut => "logged out".to_owned(),
        RowState::NotProbed => "not probed".to_owned(),
        RowState::LoggedIn => "logged in".to_owned(),
        RowState::ProbeFailed => match report.probe_error.as_deref().filter(|_| cause) {
            Some(error) => format!("probe failed: {error}"),
            None => "probe failed".to_owned(),
        },
    }
}

fn missing_reading(report: &AccountRow, now: Timestamp, time_zone: &TimeZone) -> Option<String> {
    if report.metered == Some(false)
        || (report.windows.is_empty()
            && !rimz::agents::expects_usage_reading(
                &report.kind,
                report.metered,
                report.sub_provider.as_deref(),
            ))
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
        ExtraCredits::Unlimited => Some(vec![cell("unlimited")]),
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
#[path = "tests.rs"]
mod tests;
