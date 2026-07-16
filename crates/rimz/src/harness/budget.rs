//! Agent, room-fleet, and provider-account dollar caps.
//!
//! Launch identity carries agent [`BudgetSpec`]s; machine config supplies the
//! local-day fleet and account caps. The sidebar producer evaluates durable
//! transcript spend plus live card costs, writes cache-class scope ledgers, and
//! spawns the hidden `agents budget-park` helper when a running turn crosses a
//! cap. The helper owns the pane interrupt and supervised-run transition; this
//! module stays safe in the sidebar's read-only store import graph.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus};
use crate::config::MachineConfig;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::message::{DeliveryGate, MessageRecord, MessageSender, MessageStatus};
use crate::store::SidebarSnapshot;
use crate::store::atomic::write_temp_then_rename_cache;

pub use crate::harness::auto_continue::clear_budget_park;

const INTERRUPT_RETRY_SECS: i64 = 120;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetWindow {
    Session,
    Day,
}

impl BudgetWindow {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Day => "day",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetSpec {
    pub cap_usd: f64,
    pub window: BudgetWindow,
}

impl FromStr for BudgetSpec {
    type Err = BudgetParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(BudgetParseError::Empty);
        }
        if raw.starts_with('+') {
            return Err(BudgetParseError::InvalidAmount(raw.to_owned()));
        }
        let (amount, window) = if let Some(amount) = raw.strip_suffix("/day") {
            (amount.trim(), BudgetWindow::Day)
        } else {
            (raw, BudgetWindow::Session)
        };
        let amount = amount.strip_prefix('$').unwrap_or(amount).trim();
        let cap_usd = amount
            .parse::<f64>()
            .map_err(|_| BudgetParseError::InvalidAmount(raw.to_owned()))?;
        if !cap_usd.is_finite() || cap_usd < 0.0 {
            return Err(BudgetParseError::InvalidAmount(raw.to_owned()));
        }
        Ok(Self { cap_usd, window })
    }
}

impl fmt::Display for BudgetSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${:.2}", self.cap_usd)?;
        if self.window == BudgetWindow::Day {
            f.write_str("/day")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BudgetParseError {
    #[error("budget is empty; use an amount such as `5`, `$4.50`, or `20/day`")]
    Empty,
    #[error(
        "invalid budget `{0}`; use a non-negative dollar amount such as `5`, `$4.50`, or `20/day`"
    )]
    InvalidAmount(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DayBaseline {
    pub date: Date,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetParkStamp {
    pub at_cost: f64,
    pub at: Timestamp,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    #[default]
    Agent,
    Fleet,
    Account,
}

/// Read-side park projection carried in snapshots and agent inspection JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetPark {
    pub cap_usd: f64,
    pub spend_usd: f64,
    pub window: BudgetWindow,
    pub at: Timestamp,
    #[serde(default)]
    pub scope: BudgetScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_kind: Option<AgentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
}

impl BudgetPark {
    pub fn summary(&self) -> String {
        fmt_spend(self.spend_usd, self.cap_usd, self.window)
    }

    pub fn label(&self) -> String {
        let prefix = match self.scope {
            BudgetScope::Agent => "budget".to_owned(),
            BudgetScope::Fleet => "fleet budget".to_owned(),
            BudgetScope::Account => format!(
                "{} account budget",
                self.account_kind
                    .as_ref()
                    .map_or("provider", AgentKind::as_str)
            ),
        };
        format!("{prefix}: {}", self.summary())
    }
}

/// One workspace-fleet or provider-login daily budget scope.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DailyBudgetScope {
    Fleet,
    Account(AgentKind),
}

/// Runtime override and park state shared by daily budget scopes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DailyBudgetLedger {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_spec: Option<BudgetSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raised_cap_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<BudgetParkStamp>,
}

impl DailyBudgetScope {
    pub fn effective_cap_usd(
        &self,
        ledger: &DailyBudgetLedger,
        config: &MachineConfig,
    ) -> Option<f64> {
        let configured = self.configured_cap_usd(config)?;
        if ledger.disabled {
            return None;
        }
        ledger.raised_cap_usd.or(match self {
            Self::Fleet => ledger
                .override_spec
                .map(|spec| spec.cap_usd)
                .or(Some(configured)),
            Self::Account(_) => Some(configured),
        })
    }

    pub fn cap_source(
        &self,
        ledger: &DailyBudgetLedger,
        config: &MachineConfig,
    ) -> BudgetCapSource {
        if self.configured_cap_usd(config).is_none() {
            BudgetCapSource::None
        } else if ledger.disabled {
            BudgetCapSource::Cleared
        } else if ledger.raised_cap_usd.is_some() {
            BudgetCapSource::Raised
        } else if matches!(self, Self::Fleet) && ledger.override_spec.is_some() {
            BudgetCapSource::Override
        } else {
            BudgetCapSource::Config
        }
    }

    fn configured_cap_usd(&self, config: &MachineConfig) -> Option<f64> {
        match self {
            Self::Fleet => config.harness.budget.map(|cap| cap.as_usd()),
            Self::Account(kind) => {
                crate::agents::descriptor_by_kind(kind.as_str())?
                    .has_authoritative_account_spend()
                    .then_some(())?;
                config
                    .accounts
                    .budget(kind.as_str())
                    .map(|cap| cap.as_usd())
            }
        }
    }

    pub fn apply_absolute(&self, ledger: &mut DailyBudgetLedger, cap: BudgetSpec) {
        match self {
            Self::Fleet => {
                ledger.override_spec = Some(cap);
                ledger.raised_cap_usd = None;
            }
            Self::Account(_) => {
                ledger.override_spec = None;
                ledger.raised_cap_usd = Some(cap.cap_usd);
            }
        }
        ledger.disabled = false;
    }

    pub fn label(&self) -> String {
        match self {
            Self::Fleet => "fleet".to_owned(),
            Self::Account(kind) => format!("{kind} account"),
        }
    }

    pub fn account_kind(&self) -> Option<&AgentKind> {
        match self {
            Self::Fleet => None,
            Self::Account(kind) => Some(kind),
        }
    }

    pub fn ledger_label(&self) -> &'static str {
        match self {
            Self::Fleet => "fleet budget ledger",
            Self::Account(_) => "account budget ledger",
        }
    }

    pub fn require_configured(&self, config: &MachineConfig) -> Result<(), String> {
        if self.configured_cap_usd(config).is_some() {
            return Ok(());
        }
        Err(match self {
            Self::Fleet => {
                "no fleet budget is configured; turn it on with `rimz config set harness.budget 50/day`"
                    .to_owned()
            }
            Self::Account(kind) => format!(
                "no {kind} account budget is configured; turn it on with `rimz config set accounts.budget.{kind} 100/day`"
            ),
        })
    }

    pub fn raise_unavailable_message(&self) -> String {
        match self {
            Self::Fleet => {
                "cannot raise a cleared or unset fleet budget; set an absolute `/day` cap first"
                    .to_owned()
            }
            Self::Account(kind) => format!(
                "cannot raise a cleared or unset {kind} account budget; set an absolute `/day` cap first"
            ),
        }
    }

    fn exhausted_reason(&self, spend_usd: f64, cap_usd: f64) -> String {
        let spend = fmt_spend(spend_usd, cap_usd, BudgetWindow::Day);
        match self {
            Self::Fleet => {
                format!("fleet budget exhausted ({spend}); use `rimz budget` to raise or clear it")
            }
            Self::Account(kind) => format!(
                "{kind} account budget exhausted ({spend}); use `rimz budget --account {kind}` to raise or clear it"
            ),
        }
    }

    pub fn ledger_path(&self, runtime: &RuntimePaths) -> PathBuf {
        self.file(runtime).path
    }

    pub fn day_spend_usd(
        &self,
        runtime: &RuntimePaths,
        config: &MachineConfig,
        now: Timestamp,
        provider: Option<&crate::agents::spending::ProviderSpendingCache>,
    ) -> f64 {
        let cutoff = local_day_cutoff_secs(now, &config.time_zone());
        match self {
            Self::Fleet => current_workspace_day(runtime, cutoff).unwrap_or_default(),
            Self::Account(kind) => provider
                .and_then(|provider| provider_day_usd(provider, cutoff, kind.as_str()))
                .unwrap_or_default(),
        }
    }

    pub fn read_ledger(&self, runtime: &RuntimePaths) -> DailyBudgetLedger {
        let mut ledger: DailyBudgetLedger = self.file(runtime).read();
        if matches!(self, Self::Account(_)) {
            ledger.override_spec = None;
        }
        ledger
    }

    pub fn write_ledger(
        &self,
        runtime: &RuntimePaths,
        ledger: &DailyBudgetLedger,
    ) -> Result<(), ScopeLedgerWriteError> {
        let file = self.file(runtime);
        match self {
            Self::Fleet => file.write(ledger),
            Self::Account(_) => {
                let mut account = ledger.clone();
                account.override_spec = None;
                file.write(&account)
            }
        }
    }

    fn merge_park(
        &self,
        runtime: &RuntimePaths,
        parked: Option<BudgetParkStamp>,
    ) -> Result<(), ScopeLedgerWriteError> {
        self.file(runtime).merge_park(self, parked)
    }

    fn file(&self, runtime: &RuntimePaths) -> ScopeLedgerFile {
        match self {
            Self::Fleet => ScopeLedgerFile {
                path: runtime.root.join("budget.fleet.json"),
                lock_path: runtime.root.join("budget.fleet.lock"),
            },
            Self::Account(kind) => {
                let component = account_ledger_component(kind);
                ScopeLedgerFile {
                    path: runtime
                        .persistent_shared_root
                        .join(format!("budget.account.{component}.json")),
                    lock_path: runtime
                        .shared_root
                        .join(format!("budget.account.{component}.lock")),
                }
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScopeLedgerWriteError {
    #[error(transparent)]
    Lock(#[from] crate::store::lock::LockErr),
    #[error(transparent)]
    Write(#[from] crate::store::atomic::AtomicErr),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BudgetCapSource {
    Config,
    Override,
    Raised,
    Cleared,
    None,
}

impl fmt::Display for BudgetCapSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Config => "config",
            Self::Override => "override",
            Self::Raised => "raised",
            Self::Cleared => "cleared",
            Self::None => "none",
        })
    }
}

/// Per-agent waiver and interrupt state shared by fleet and account parks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetScopeState {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub waivers: BTreeMap<String, Timestamp>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parked_at: BTreeMap<String, Timestamp>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub last_interrupt_at: BTreeMap<String, Timestamp>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetLedger {
    pub spec: BudgetSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raised_cap_usd: Option<f64>,
    /// `clear` disables a launch-carried cap without erasing its audit shape.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_baseline: Option<DayBaseline>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<BudgetParkStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_interrupt_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waived_delivery_at: Option<Timestamp>,
}

impl BudgetLedger {
    pub fn new(spec: BudgetSpec) -> Self {
        Self {
            spec,
            raised_cap_usd: None,
            disabled: false,
            day_baseline: None,
            parked: None,
            last_interrupt_at: None,
            waived_delivery_at: None,
        }
    }

    pub fn effective_cap_usd(&self) -> Option<f64> {
        (!self.disabled).then_some(self.raised_cap_usd.unwrap_or(self.spec.cap_usd))
    }

    pub fn spend_usd(&self, total_cost_usd: f64) -> f64 {
        match self.spec.window {
            BudgetWindow::Session => total_cost_usd,
            BudgetWindow::Day => (total_cost_usd
                - self
                    .day_baseline
                    .as_ref()
                    .map_or(total_cost_usd, |baseline| baseline.cost_usd))
            .max(0.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BudgetVerdict {
    Under { spend_usd: f64, cap_usd: f64 },
    Park { spend_usd: f64, cap_usd: f64 },
    Waived { spend_usd: f64, cap_usd: f64 },
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaiverOutcome {
    /// A running turn started after the waiver — honor it.
    Waived,
    /// The waived turn finished — the caller consumes the waiver and re-parks at `now`.
    Consumed,
    /// No waiver applies — the park stands.
    Parked,
}

/// Record a fresh human delivery after a park, then classify the current turn.
fn waiver_step(
    agent: &AgentState,
    parked_at: Option<Timestamp>,
    waived: &mut Option<Timestamp>,
    latest_delivery: Option<Timestamp>,
) -> WaiverOutcome {
    if let (Some(parked), Some(delivered)) = (parked_at, latest_delivery)
        && delivered > parked
        && waived.is_none_or(|prior| delivered > prior)
    {
        *waived = Some(delivered);
    }
    if let Some(waived_at) = *waived
        && agent
            .turn_started_at
            .is_some_and(|started| started >= waived_at)
    {
        if agent.status == AgentStatus::Running {
            return WaiverOutcome::Waived;
        }
        *waived = None;
        return WaiverOutcome::Consumed;
    }
    WaiverOutcome::Parked
}

pub fn total_cost_usd(agent: &AgentState) -> Option<f64> {
    let cost = agent.context.as_ref()?.cost.as_ref()?;
    if !cost.coverage.contributes_to_live_spend() {
        return None;
    }
    cost.total_cost_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
}

/// Current spend and effective cap for one agent's inspection surface.
pub fn spend_summary(
    runtime: &RuntimePaths,
    agent: &AgentState,
    session_cost: Option<f64>,
) -> Option<String> {
    if let Some(park) = agent.budget_park.as_ref() {
        return Some(park.summary());
    }
    let ledger = read_ledger(runtime, &agent.kind, &agent.agent_id);
    let launched = agent
        .budget
        .as_deref()
        .and_then(|raw| raw.parse::<BudgetSpec>().ok());
    let spec = ledger.as_ref().map(|ledger| ledger.spec).or(launched)?;
    let cap = ledger
        .as_ref()
        .map_or(Some(spec.cap_usd), BudgetLedger::effective_cap_usd)?;
    let total = total_cost_usd(agent).or(session_cost).unwrap_or(0.0);
    let spend = ledger.as_ref().map_or_else(
        || BudgetLedger::new(spec).spend_usd(total),
        |ledger| ledger.spend_usd(total),
    );
    Some(fmt_spend(spend, cap, spec.window))
}

fn fmt_spend(spend_usd: f64, cap_usd: f64, window: BudgetWindow) -> String {
    format!(
        "${spend_usd:.2} of ${cap_usd:.2}{}",
        if window == BudgetWindow::Day {
            "/day"
        } else {
            ""
        }
    )
}

/// Pure budget decision plus day/waiver bookkeeping. The caller owns IO.
pub fn evaluate(
    agent: &AgentState,
    ledger: &mut BudgetLedger,
    now: Timestamp,
    zone: &TimeZone,
    latest_human_delivery: Option<Timestamp>,
) -> BudgetVerdict {
    let Some(cap_usd) = ledger.effective_cap_usd() else {
        ledger.parked = None;
        return BudgetVerdict::Disabled;
    };
    let total = total_cost_usd(agent).unwrap_or(0.0);
    let spend_usd = match ledger.spec.window {
        BudgetWindow::Session => total,
        BudgetWindow::Day => {
            let date = now.to_zoned(zone.clone()).date();
            match ledger.day_baseline.as_mut() {
                Some(baseline) if baseline.date != date => {
                    baseline.date = date;
                    baseline.cost_usd = total;
                    ledger.parked = None;
                    ledger.last_interrupt_at = None;
                    ledger.waived_delivery_at = None;
                }
                None => {
                    ledger.day_baseline = Some(DayBaseline {
                        date,
                        cost_usd: total,
                    });
                }
                Some(_) => {}
            }
            ledger.spend_usd(total)
        }
    };
    if spend_usd < cap_usd {
        ledger.parked = None;
        ledger.last_interrupt_at = None;
        return BudgetVerdict::Under { spend_usd, cap_usd };
    }

    match waiver_step(
        agent,
        ledger.parked.as_ref().map(|park| park.at),
        &mut ledger.waived_delivery_at,
        latest_human_delivery,
    ) {
        WaiverOutcome::Waived => return BudgetVerdict::Waived { spend_usd, cap_usd },
        WaiverOutcome::Consumed => {
            // Move the park past the consumed delivery so that message cannot
            // waive a second turn.
            ledger.parked = Some(BudgetParkStamp {
                at_cost: total,
                at: now,
            });
        }
        WaiverOutcome::Parked => {}
    }

    ledger.parked.get_or_insert(BudgetParkStamp {
        at_cost: total,
        at: now,
    });
    BudgetVerdict::Park { spend_usd, cap_usd }
}

/// Stamp ledger park projections onto agent state for producer and consumer
/// folds. This reads runtime cache files only.
pub fn project_parks(
    snapshot: &mut SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
) {
    let now = snapshot.now;
    let zone = config.time_zone();
    let fleet_scope = DailyBudgetScope::Fleet;
    let fleet = fleet_scope.read_ledger(runtime);
    let scope_state = read_scope_state(runtime);
    let provider = crate::agents::spending::read_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
    );
    let day_cutoff = local_day_cutoff_secs(now, &zone);
    let mut accounts = BTreeMap::new();
    for agent in &mut snapshot.agents {
        agent.budget_park = ledger_for_agent(runtime, agent)
            .and_then(|ledger| agent_scope_park(agent, &ledger, now, &zone));
        if agent.budget_park.is_some() || active_scope_waiver(agent, &scope_state) {
            continue;
        }
        if !pause_applies(agent, scope_interrupted(&scope_state, agent)) {
            continue;
        }
        if let Some(park) = daily_scope_park(
            &fleet_scope,
            &fleet,
            runtime,
            config,
            &provider,
            day_cutoff,
            now,
            &zone,
        ) {
            agent.budget_park = Some(park);
            continue;
        }
        let account = accounts
            .entry(agent.kind.clone())
            .or_insert_with(|| DailyBudgetScope::Account(agent.kind.clone()).read_ledger(runtime));
        agent.budget_park = daily_scope_park(
            &DailyBudgetScope::Account(agent.kind.clone()),
            account,
            runtime,
            config,
            &provider,
            day_cutoff,
            now,
            &zone,
        );
    }
}

fn agent_scope_park(
    agent: &AgentState,
    ledger: &BudgetLedger,
    now: Timestamp,
    zone: &TimeZone,
) -> Option<BudgetPark> {
    let parked = ledger.parked.as_ref()?;
    let cap_usd = ledger.effective_cap_usd()?;
    if active_waiver(agent, ledger.waived_delivery_at)
        || !pause_applies(agent, ledger.last_interrupt_at.is_some())
    {
        return None;
    }
    let total = total_cost_usd(agent).unwrap_or(parked.at_cost);
    Some(BudgetPark {
        cap_usd,
        spend_usd: ledger.spend_usd(total),
        window: ledger.spec.window,
        at: parked.at,
        scope: BudgetScope::Agent,
        account_kind: None,
        resets_at: (ledger.spec.window == BudgetWindow::Day)
            .then(|| ledger_day_reset(ledger, now, zone))
            .flatten(),
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "one daily-scope projection boundary"
)]
fn daily_scope_park(
    scope: &DailyBudgetScope,
    ledger: &DailyBudgetLedger,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    provider: &crate::agents::spending::ProviderSpendingCache,
    day_cutoff: Option<u64>,
    now: Timestamp,
    zone: &TimeZone,
) -> Option<BudgetPark> {
    let parked = ledger.parked.as_ref()?;
    let cap_usd = scope.effective_cap_usd(ledger, config)?;
    let spend = match scope {
        DailyBudgetScope::Fleet => current_workspace_day(runtime, day_cutoff),
        DailyBudgetScope::Account(kind) => provider_day_usd(provider, day_cutoff, kind.as_str()),
    };
    let spend_usd = spend.map_or(parked.at_cost, |spend| spend.max(parked.at_cost));
    let (park_scope, account_kind) = match scope {
        DailyBudgetScope::Fleet => (BudgetScope::Fleet, None),
        DailyBudgetScope::Account(kind) => (BudgetScope::Account, Some(kind.clone())),
    };
    Some(BudgetPark {
        cap_usd,
        spend_usd,
        window: BudgetWindow::Day,
        at: parked.at,
        scope: park_scope,
        account_kind,
        resets_at: next_day_start(now, zone),
    })
}

/// Producer-side enforcement. Ledger files are cache-class durability; pane
/// interruption and run-store writes stay in the hidden CLI helper.
pub fn enforce(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    messages_dir: &Path,
    config: &MachineConfig,
) {
    let now = snapshot.now;
    let zone = config.time_zone();
    let human_deliveries = delivered_human_messages(messages_dir);
    let day_cutoff = local_day_cutoff_secs(now, &zone);
    let scopes = evaluate_scopes(snapshot, runtime, config, now, day_cutoff);
    let mut scope_state = read_scope_state(runtime);
    let scope_before = scope_state.clone();
    {
        let mut ctx = EnforceCtx {
            runtime,
            snapshot,
            now,
            zone: &zone,
            human_deliveries: &human_deliveries,
            scopes: &scopes,
            scope_state: &mut scope_state,
        };
        for agent in snapshot.root_agents() {
            enforce_agent(agent, &mut ctx);
        }
    }

    for daily in scopes.daily {
        if daily.ledger.parked != daily.parked_before
            && let Err(err) = daily.scope.merge_park(runtime, daily.ledger.parked.clone())
        {
            warn_write(&format!("{} budget ledger", daily.scope.label()), err);
        }
    }
    if scope_state != scope_before
        && let Err(err) = write_scope_state(runtime, &scope_state)
    {
        warn_write("budget scope state", err);
    }
}

struct DailyScopeVerdict {
    scope: DailyBudgetScope,
    ledger: DailyBudgetLedger,
    parked_before: Option<BudgetParkStamp>,
    verdict: BudgetVerdict,
}

struct ScopeVerdicts {
    daily: Vec<DailyScopeVerdict>,
}

fn evaluate_scopes(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    now: Timestamp,
    day_cutoff: Option<u64>,
) -> ScopeVerdicts {
    let fleet_spend = if snapshot.fleet_day_spend_epoch_secs == day_cutoff {
        snapshot.fleet_day_spend_usd.unwrap_or_default()
    } else {
        current_workspace_day(runtime, day_cutoff).unwrap_or_default()
    };
    let fleet_scope = DailyBudgetScope::Fleet;
    let mut fleet = fleet_scope.read_ledger(runtime);
    let fleet_parked_before = fleet.parked.clone();
    let fleet_cap = fleet_scope.effective_cap_usd(&fleet, config);
    let fleet_verdict = evaluate_daily_scope(&mut fleet.parked, fleet_cap, fleet_spend, now);

    let provider = crate::agents::spending::read_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
    );
    let root_kinds = snapshot
        .root_agents()
        .map(|agent| agent.kind.clone())
        .collect::<BTreeSet<_>>();
    let mut daily = vec![DailyScopeVerdict {
        scope: fleet_scope,
        ledger: fleet,
        parked_before: fleet_parked_before,
        verdict: fleet_verdict,
    }];
    for kind in root_kinds {
        let scope = DailyBudgetScope::Account(kind.clone());
        let mut ledger = scope.read_ledger(runtime);
        let parked_before = ledger.parked.clone();
        let cap = scope.effective_cap_usd(&ledger, config);
        let spend = provider_day_usd(&provider, day_cutoff, kind.as_str()).unwrap_or_default();
        let verdict = evaluate_daily_scope(&mut ledger.parked, cap, spend, now);
        daily.push(DailyScopeVerdict {
            scope,
            ledger,
            parked_before,
            verdict,
        });
    }

    ScopeVerdicts { daily }
}

struct EnforceCtx<'a> {
    runtime: &'a RuntimePaths,
    snapshot: &'a SidebarSnapshot,
    now: Timestamp,
    zone: &'a TimeZone,
    human_deliveries: &'a [MessageRecord],
    scopes: &'a ScopeVerdicts,
    scope_state: &'a mut BudgetScopeState,
}

fn enforce_agent(agent: &AgentState, ctx: &mut EnforceCtx<'_>) {
    if agent.agent_id.is_empty() || agent.agent_id.is_provisional() {
        return;
    }
    let mut ledger = ledger_for_agent(ctx.runtime, agent);
    let before = ledger.clone();
    let latest_delivery = ctx
        .human_deliveries
        .iter()
        .filter(|message| message.same_agent_card(agent))
        .filter_map(|message| message.delivered_at)
        .max();
    let verdict = ledger.as_mut().map_or(BudgetVerdict::Disabled, |ledger| {
        evaluate(agent, ledger, ctx.now, ctx.zone, latest_delivery)
    });
    let scope_verdict = evaluate_scope_waiver(
        agent,
        binding_scope_park(ctx.scopes, &agent.kind),
        latest_delivery,
        ctx.scope_state,
        ctx.now,
    );
    let classification = classify_parks(&verdict, scope_verdict);
    apply_enforcement_effects(agent, &mut ledger, classification, ctx);
    if ledger != before
        && let Some(ledger) = ledger.as_ref()
        && let Err(err) = write_ledger(ctx.runtime, &agent.kind, &agent.agent_id, ledger)
    {
        warn_write("agent budget ledger", err);
    }
}

#[derive(Clone, Copy)]
struct ParkClassification {
    agent_parked: bool,
    scope_parked: bool,
    all_under: bool,
}

fn classify_parks(verdict: &BudgetVerdict, scope_verdict: ScopeAgentVerdict) -> ParkClassification {
    ParkClassification {
        agent_parked: matches!(verdict, BudgetVerdict::Park { .. }),
        scope_parked: scope_verdict == ScopeAgentVerdict::Park,
        all_under: matches!(
            verdict,
            BudgetVerdict::Under { .. } | BudgetVerdict::Disabled
        ) && scope_verdict == ScopeAgentVerdict::Under,
    }
}

fn apply_enforcement_effects(
    agent: &AgentState,
    ledger: &mut Option<BudgetLedger>,
    classification: ParkClassification,
    ctx: &mut EnforceCtx<'_>,
) {
    if classification.all_under {
        crate::harness::auto_continue::clear_budget_park(ctx.runtime, &agent.kind, &agent.agent_id);
        return;
    }
    if !classification.agent_parked && !classification.scope_parked {
        return;
    }
    update_auto_continue(agent, ledger.as_ref(), classification, ctx);
    interrupt_parked_agent(agent, ledger, classification, ctx);
}

fn update_auto_continue(
    agent: &AgentState,
    ledger: Option<&BudgetLedger>,
    classification: ParkClassification,
    ctx: &EnforceCtx<'_>,
) {
    let daily_park = classification.scope_parked
        || (classification.agent_parked
            && ledger.is_some_and(|ledger| ledger.spec.window == BudgetWindow::Day));
    let interrupted = (classification.agent_parked
        && ledger.is_some_and(|ledger| ledger.last_interrupt_at.is_some()))
        || (classification.scope_parked && scope_interrupted(ctx.scope_state, agent));
    if daily_park && pause_applies(agent, interrupted) {
        if let Some(deadline) = next_day_start(ctx.now, ctx.zone) {
            crate::harness::auto_continue::arm_budget_park(
                ctx.runtime,
                &agent.kind,
                &agent.agent_id,
                deadline,
                agent.last_activity,
            );
        }
    } else {
        crate::harness::auto_continue::clear_budget_park(ctx.runtime, &agent.kind, &agent.agent_id);
    }
}

fn interrupt_parked_agent(
    agent: &AgentState,
    ledger: &mut Option<BudgetLedger>,
    classification: ParkClassification,
    ctx: &mut EnforceCtx<'_>,
) {
    let key = scope_agent_key(agent);
    let agent_interrupt = classification
        .agent_parked
        .then(|| ledger.as_ref().and_then(|ledger| ledger.last_interrupt_at))
        .flatten();
    let scope_interrupt = classification
        .scope_parked
        .then(|| ctx.scope_state.last_interrupt_at.get(&key).copied())
        .flatten();
    let last_interrupt = [agent_interrupt, scope_interrupt]
        .into_iter()
        .flatten()
        .max();
    if agent.status != AgentStatus::Running || !interrupt_due(last_interrupt, ctx.now) {
        return;
    }
    let Some(pane_id) = live_pane(ctx.snapshot, agent) else {
        return;
    };

    // A supervised run records this agent's own cost. The broader
    // fleet/account figure only decides why the pane is stopped.
    let at_cost = total_cost_usd(agent);
    if let Some(ledger) = ledger.as_ref()
        && let Err(err) = write_ledger(ctx.runtime, &agent.kind, &agent.agent_id, ledger)
    {
        warn_write("agent budget ledger", err);
    }
    if spawn_budget_park(ctx.runtime, agent, &pane_id, at_cost) {
        if classification.agent_parked
            && let Some(ledger) = ledger.as_mut()
        {
            ledger.last_interrupt_at = Some(ctx.now);
        }
        if classification.scope_parked {
            ctx.scope_state.last_interrupt_at.insert(key, ctx.now);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScopeAgentVerdict {
    Under,
    Park,
    Waived,
}

fn evaluate_daily_scope(
    parked: &mut Option<BudgetParkStamp>,
    cap_usd: Option<f64>,
    spend_usd: f64,
    now: Timestamp,
) -> BudgetVerdict {
    let Some(cap_usd) = cap_usd else {
        *parked = None;
        return BudgetVerdict::Disabled;
    };
    if spend_usd < cap_usd {
        *parked = None;
        return BudgetVerdict::Under { spend_usd, cap_usd };
    }
    parked.get_or_insert(BudgetParkStamp {
        at_cost: spend_usd,
        at: now,
    });
    BudgetVerdict::Park { spend_usd, cap_usd }
}

fn evaluate_scope_waiver(
    agent: &AgentState,
    parked_at: Option<Timestamp>,
    latest_delivery: Option<Timestamp>,
    state: &mut BudgetScopeState,
    now: Timestamp,
) -> ScopeAgentVerdict {
    let key = scope_agent_key(agent);
    let Some(parked_at) = parked_at else {
        state.waivers.remove(&key);
        state.parked_at.remove(&key);
        state.last_interrupt_at.remove(&key);
        return ScopeAgentVerdict::Under;
    };
    let threshold = *state
        .parked_at
        .entry(key.clone())
        .and_modify(|threshold| *threshold = (*threshold).max(parked_at))
        .or_insert(parked_at);
    let mut waived = state.waivers.get(&key).copied();
    let outcome = waiver_step(agent, Some(threshold), &mut waived, latest_delivery);
    match waived {
        Some(waived) => {
            state.waivers.insert(key.clone(), waived);
        }
        None => {
            state.waivers.remove(&key);
        }
    }
    match outcome {
        WaiverOutcome::Waived => ScopeAgentVerdict::Waived,
        WaiverOutcome::Consumed => {
            state.parked_at.insert(key, now);
            ScopeAgentVerdict::Park
        }
        WaiverOutcome::Parked => ScopeAgentVerdict::Park,
    }
}

fn binding_scope_park(scopes: &ScopeVerdicts, kind: &AgentKind) -> Option<Timestamp> {
    scopes.daily.iter().find_map(|daily| {
        let applies = matches!(daily.scope, DailyBudgetScope::Fleet)
            || matches!(&daily.scope, DailyBudgetScope::Account(account) if account == kind);
        (applies && matches!(daily.verdict, BudgetVerdict::Park { .. }))
            .then(|| daily.ledger.parked.as_ref().map(|park| park.at))
            .flatten()
    })
}

fn active_waiver(agent: &AgentState, waived: Option<Timestamp>) -> bool {
    waived.is_some_and(|waived| {
        agent.status == AgentStatus::Running
            && agent
                .turn_started_at
                .is_some_and(|started| started >= waived)
    })
}

/// A binding budget park affects a live turn or one this park interrupted.
/// Waiting agents keep their ask visible until its answered turn runs again.
fn pause_applies(agent: &AgentState, interrupted: bool) -> bool {
    agent.status == AgentStatus::Running || (interrupted && agent.status != AgentStatus::Waiting)
}

/// Whether this room interrupted the agent for the binding fleet/account park.
pub fn scope_interrupted(state: &BudgetScopeState, agent: &AgentState) -> bool {
    state
        .last_interrupt_at
        .contains_key(&scope_agent_key(agent))
}

fn active_scope_waiver(agent: &AgentState, state: &BudgetScopeState) -> bool {
    active_waiver(agent, state.waivers.get(&scope_agent_key(agent)).copied())
}

fn scope_agent_key(agent: &AgentState) -> String {
    format!("{}:{}", agent.kind, agent.agent_id)
}

fn warn_write(label: &str, err: impl std::error::Error + 'static) {
    tracing::warn!(
        tags.operation = "budget.write_ledger",
        error = &err as &dyn std::error::Error,
        "failed to write {label}",
    );
}

fn ledger_for_agent(runtime: &RuntimePaths, agent: &AgentState) -> Option<BudgetLedger> {
    if let Some(mut ledger) = read_ledger(runtime, &agent.kind, &agent.agent_id) {
        if let Some(spec) = agent.budget.as_deref().and_then(|raw| raw.parse().ok())
            && ledger.spec != spec
            && ledger.raised_cap_usd.is_none()
            && !ledger.disabled
        {
            ledger.spec = spec;
        }
        return Some(ledger);
    }
    let spec = agent.budget.as_deref()?.parse().ok()?;
    Some(BudgetLedger::new(spec))
}

fn delivered_human_messages(messages_dir: &Path) -> Vec<MessageRecord> {
    let mut messages = crate::store::message_store::list(messages_dir).unwrap_or_default();
    messages.extend(crate::store::message_store::list_history(messages_dir).unwrap_or_default());
    messages
        .into_iter()
        .filter(is_budget_waiving_delivery)
        .collect()
}

fn is_budget_waiving_delivery(message: &MessageRecord) -> bool {
    // Auto-continue records render as human-authored for transcript
    // attribution, but their resume gate must not waive a dollar cap.
    message.status == MessageStatus::Delivered
        && matches!(message.sender, MessageSender::Human)
        && !message.automated
        && message.gate != DeliveryGate::Resume
}

fn live_pane(snapshot: &SidebarSnapshot, agent: &AgentState) -> Option<PaneId> {
    snapshot
        .agent_panes
        .iter()
        .find(|pane| pane.kind == agent.kind && pane.agent_id.as_ref() == Some(&agent.agent_id))
        .map(|pane| pane.pane_id.clone())
}

fn interrupt_due(last: Option<Timestamp>, now: Timestamp) -> bool {
    last.is_none_or(|last| now.as_second() - last.as_second() >= INTERRUPT_RETRY_SECS)
}

fn next_day_start(now: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
    now.to_zoned(zone.clone())
        .tomorrow()
        .ok()?
        .start_of_day()
        .ok()
        .map(|zoned| zoned.timestamp())
}

fn local_day_start(now: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
    now.to_zoned(zone.clone())
        .start_of_day()
        .ok()
        .map(|zoned| zoned.timestamp())
}

fn local_day_cutoff_secs(now: Timestamp, zone: &TimeZone) -> Option<u64> {
    local_day_start(now, zone).map(|stamp| stamp.as_second().max(0) as u64)
}

fn provider_day_usd(
    provider: &crate::agents::spending::ProviderSpendingCache,
    cutoff_secs: Option<u64>,
    kind: &str,
) -> Option<f64> {
    (Some(provider.day_cutoff_secs) == cutoff_secs)
        .then(|| provider.day_by_provider.get(kind).map(|day| day.usd))
        .flatten()
}

fn current_workspace_day(runtime: &RuntimePaths, cutoff: Option<u64>) -> Option<f64> {
    let cutoff = cutoff?;
    current_workspace_day_cache(runtime, cutoff).map(|cache| cache.day.usd)
}

fn current_workspace_day_cache(
    runtime: &RuntimePaths,
    cutoff: u64,
) -> Option<crate::agents::spending::WorkspaceSpendingCache> {
    std::fs::read_dir(&runtime.root)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            (name.starts_with("workspace-spending.") && name.ends_with(".json"))
                .then(|| crate::agents::spending::read_workspace_spending_cache(&path))
        })
        .filter(|cache| {
            cache.version == crate::agents::spending::WORKSPACE_SPENDING_VERSION
                && cache.day_cutoff_secs == cutoff
        })
        .max_by_key(|cache| cache.refreshed_at_ms)
}

pub fn workspace_day_cache(
    runtime: &RuntimePaths,
    config: &MachineConfig,
    now: Timestamp,
) -> crate::agents::spending::WorkspaceSpendingCache {
    let cutoff = local_day_cutoff_secs(now, &config.time_zone()).unwrap_or_default();
    current_workspace_day_cache(runtime, cutoff).unwrap_or_default()
}

/// Attach daily-cap summaries after the spending overlay has stamped the
/// room's live local-day figure.
pub fn project_budget_views(
    snapshot: &mut SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &MachineConfig,
    provider: &crate::agents::spending::ProviderSpendingCache,
) {
    let now = snapshot.now;
    let cutoff = local_day_cutoff_secs(now, &config.time_zone());
    let fleet_scope = DailyBudgetScope::Fleet;
    let fleet = fleet_scope.read_ledger(runtime);
    snapshot.fleet_budget = fleet_scope
        .effective_cap_usd(&fleet, config)
        .map(|cap_usd| {
            let spend_usd = snapshot.fleet_day_spend_usd.unwrap_or_default();
            crate::DailyBudgetView {
                cap_usd,
                spend_usd,
                parked: fleet.parked.is_some() && spend_usd >= cap_usd,
            }
        });
    for panel in &mut snapshot.providers {
        let kind = AgentKind::new_unchecked(panel.kind.clone());
        let scope = DailyBudgetScope::Account(kind);
        let ledger = scope.read_ledger(runtime);
        panel.day_budget = scope.effective_cap_usd(&ledger, config).map(|cap_usd| {
            let spend_usd = provider_day_usd(provider, cutoff, &panel.kind).unwrap_or_default();
            crate::DailyBudgetView {
                cap_usd,
                spend_usd,
                parked: ledger.parked.is_some() && spend_usd >= cap_usd,
            }
        });
    }
}

/// Fail-fast guard for programmatic work. Interactive launches remain allowed;
/// a delivered human message can waive their next parked turn.
pub fn scope_gate(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    config: &MachineConfig,
    now: Timestamp,
) -> Option<String> {
    let zone = config.time_zone();
    let cutoff = local_day_start(now, &zone)?;
    let cutoff_secs = local_day_cutoff_secs(now, &zone)?;

    let mut provider = None;
    for scope in [
        DailyBudgetScope::Fleet,
        DailyBudgetScope::Account(kind.clone()),
    ] {
        let ledger = scope.read_ledger(runtime);
        let Some(cap) = scope.effective_cap_usd(&ledger, config) else {
            continue;
        };
        let mut spend = match &scope {
            DailyBudgetScope::Fleet => {
                current_workspace_day(runtime, Some(cutoff_secs)).unwrap_or_default()
            }
            DailyBudgetScope::Account(kind) => {
                let provider = provider.get_or_insert_with(|| {
                    crate::agents::spending::read_provider_spending_cache(
                        &runtime.shared_provider_spending_path(),
                    )
                });
                provider_day_usd(provider, Some(cutoff_secs), kind.as_str()).unwrap_or_default()
            }
        };
        if let Some(parked) = ledger.parked.as_ref().filter(|parked| parked.at >= cutoff) {
            spend = spend.max(parked.at_cost);
        }
        if spend >= cap {
            return Some(scope.exhausted_reason(spend, cap));
        }
    }
    None
}

fn ledger_day_reset(ledger: &BudgetLedger, now: Timestamp, zone: &TimeZone) -> Option<Timestamp> {
    let date = ledger
        .day_baseline
        .as_ref()
        .map(|baseline| baseline.date)
        .unwrap_or_else(|| now.to_zoned(zone.clone()).date());
    date.tomorrow()
        .ok()?
        .at(0, 0, 0, 0)
        .to_zoned(zone.clone())
        .ok()
        .map(|zoned| zoned.timestamp())
}

#[cfg(not(test))]
fn spawn_budget_park(
    runtime: &RuntimePaths,
    agent: &AgentState,
    pane_id: &PaneId,
    at_cost: Option<f64>,
) -> bool {
    let mut cmd = crate::child_process::detached_rimz_command(crate::proc::rimz_exe(), runtime);
    cmd.args([
        "agents",
        "budget-park",
        "--workspace-id",
        runtime.workspace_id.as_str(),
        "--kind",
        agent.kind.as_str(),
        "--agent-id",
        agent.agent_id.as_str(),
        "--pane",
        &pane_id.to_string(),
    ]);
    if let Some(at_cost) = at_cost {
        cmd.arg("--at-cost").arg(at_cost.to_string());
    }
    if let Err(err) = crate::child_process::spawn_detached_reaped(&mut cmd, "agent-budget-park") {
        tracing::debug!(
            workspace = %runtime.workspace_id,
            kind = %agent.kind,
            agent_id = %agent.agent_id,
            tags.operation = "budget.spawn_park",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn agent budget park",
        );
        return false;
    }
    true
}

#[cfg(test)]
fn spawn_budget_park(
    _runtime: &RuntimePaths,
    _agent: &AgentState,
    _pane_id: &PaneId,
    _at_cost: Option<f64>,
) -> bool {
    true
}

pub fn budget_ledger_path(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> PathBuf {
    runtime
        .root
        .join(format!("budget.{}.json", agent_digest(kind, agent_id)))
}

pub(crate) fn agent_digest(kind: &AgentKind, agent_id: &AgentSessionId) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(agent_id.as_str().as_bytes());
    let digest = hex::encode(hasher.finalize());
    digest[..32].to_owned()
}

pub fn read_ledger(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> Option<BudgetLedger> {
    let bytes = std::fs::read(budget_ledger_path(runtime, kind, agent_id)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn write_ledger(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    ledger: &BudgetLedger,
) -> crate::store::atomic::Result<()> {
    let path = budget_ledger_path(runtime, kind, agent_id);
    write_temp_then_rename_cache(&path, ledger)
}

fn account_ledger_component(kind: &AgentKind) -> String {
    if kind
        .as_str()
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        kind.as_str().to_owned()
    } else {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_str().as_bytes());
        format!("kind-{}", &hex::encode(hasher.finalize())[..16])
    }
}

struct ScopeLedgerFile {
    path: PathBuf,
    lock_path: PathBuf,
}

impl ScopeLedgerFile {
    fn read<T: DeserializeOwned + Default>(&self) -> T {
        std::fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write<T: Serialize>(&self, value: &T) -> Result<(), ScopeLedgerWriteError> {
        let _guard = crate::store::lock::WorkspaceLock::acquire(&self.lock_path)?;
        self.write_unlocked(value)?;
        Ok(())
    }

    fn merge_park(
        &self,
        scope: &DailyBudgetScope,
        parked: Option<BudgetParkStamp>,
    ) -> Result<(), ScopeLedgerWriteError> {
        let _guard = crate::store::lock::WorkspaceLock::acquire(&self.lock_path)?;
        let mut current: DailyBudgetLedger = self.read();
        if matches!(scope, DailyBudgetScope::Account(_)) {
            current.override_spec = None;
        }
        current.parked = parked;
        self.write_unlocked(&current)?;
        Ok(())
    }

    fn write_unlocked<T: Serialize>(&self, value: &T) -> crate::store::atomic::Result<()> {
        write_temp_then_rename_cache(&self.path, value)
    }
}

pub fn scope_state_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("budget.scopes.json")
}

pub fn read_scope_state(runtime: &RuntimePaths) -> BudgetScopeState {
    std::fs::read(scope_state_path(runtime))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn write_scope_state(
    runtime: &RuntimePaths,
    state: &BudgetScopeState,
) -> crate::store::atomic::Result<()> {
    write_temp_then_rename_cache(&scope_state_path(runtime), state)
}

#[cfg(test)]
mod tests;
