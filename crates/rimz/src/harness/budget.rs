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
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus};
use crate::config::MachineConfig;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::message::{DeliveryGate, MessageRecord, MessageSender, MessageStatus};
use crate::store::SidebarSnapshot;
use crate::store::atomic::write_temp_then_rename_cache;

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
        format!(
            "{prefix}: ${:.2} of ${:.2}{}",
            self.spend_usd,
            self.cap_usd,
            if self.window == BudgetWindow::Day {
                "/day"
            } else {
                ""
            }
        )
    }
}

/// Runtime overrides and park state for one workspace's daily fleet cap.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetBudgetLedger {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_spec: Option<BudgetSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raised_cap_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<BudgetParkStamp>,
}

impl FleetBudgetLedger {
    pub fn effective_cap_usd(&self, config: &MachineConfig) -> Option<f64> {
        if self.disabled {
            return None;
        }
        self.raised_cap_usd
            .or_else(|| self.override_spec.map(|spec| spec.cap_usd))
            .or_else(|| config.harness.budget.map(|cap| cap.as_usd()))
    }

    pub fn cap_source(&self, config: &MachineConfig) -> BudgetCapSource {
        if self.disabled {
            BudgetCapSource::Cleared
        } else if self.raised_cap_usd.is_some() {
            BudgetCapSource::Raised
        } else if self.override_spec.is_some() {
            BudgetCapSource::Override
        } else if config.harness.budget.is_some() {
            BudgetCapSource::Config
        } else {
            BudgetCapSource::None
        }
    }
}

/// Machine-shared runtime state for one provider login's daily cap.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountBudgetLedger {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raised_cap_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<BudgetParkStamp>,
}

impl AccountBudgetLedger {
    pub fn effective_cap_usd(&self, kind: &AgentKind, config: &MachineConfig) -> Option<f64> {
        if self.disabled {
            return None;
        }
        self.raised_cap_usd.or_else(|| {
            config
                .accounts
                .budget(kind.as_str())
                .map(|cap| cap.as_usd())
        })
    }

    pub fn cap_source(&self, kind: &AgentKind, config: &MachineConfig) -> BudgetCapSource {
        if self.disabled {
            BudgetCapSource::Cleared
        } else if self.raised_cap_usd.is_some() {
            BudgetCapSource::Raised
        } else if config.accounts.budget(kind.as_str()).is_some() {
            BudgetCapSource::Config
        } else {
            BudgetCapSource::None
        }
    }
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

pub fn total_cost_usd(agent: &AgentState) -> Option<f64> {
    agent
        .context
        .as_ref()?
        .cost
        .as_ref()?
        .total_cost_usd
        .filter(|cost| cost.is_finite() && *cost >= 0.0)
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

    if let (Some(parked), Some(delivered)) = (ledger.parked.as_ref(), latest_human_delivery)
        && delivered > parked.at
        && ledger
            .waived_delivery_at
            .is_none_or(|waived| delivered > waived)
    {
        ledger.waived_delivery_at = Some(delivered);
    }
    if let Some(waived) = ledger.waived_delivery_at
        && agent
            .turn_started_at
            .is_some_and(|started| started >= waived && agent.status == AgentStatus::Running)
    {
        return BudgetVerdict::Waived { spend_usd, cap_usd };
    }
    if ledger.waived_delivery_at.is_some()
        && agent.status != AgentStatus::Running
        && agent.turn_started_at.is_some_and(|started| {
            ledger
                .waived_delivery_at
                .is_some_and(|waived| started >= waived)
        })
    {
        // Move the park past the consumed delivery so that message cannot
        // waive a second turn.
        ledger.parked = Some(BudgetParkStamp {
            at_cost: total,
            at: now,
        });
        ledger.waived_delivery_at = None;
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
    let fleet = read_fleet_ledger(runtime);
    let fleet_cap = fleet.effective_cap_usd(config);
    let scope_state = read_scope_state(runtime);
    let provider = crate::agents::spending::read_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
    );
    let day_cutoff = local_day_start(now, &zone).map(|stamp| stamp.as_second().max(0) as u64);
    for agent in &mut snapshot.agents {
        agent.budget_park = None;
        if let Some(ledger) = ledger_for_agent(runtime, agent)
            && let (Some(parked), Some(cap_usd)) =
                (ledger.parked.as_ref(), ledger.effective_cap_usd())
            && !active_waiver(agent, ledger.waived_delivery_at)
        {
            let total = total_cost_usd(agent).unwrap_or(parked.at_cost);
            agent.budget_park = Some(BudgetPark {
                cap_usd,
                spend_usd: ledger.spend_usd(total),
                window: ledger.spec.window,
                at: parked.at,
                scope: BudgetScope::Agent,
                account_kind: None,
                resets_at: (ledger.spec.window == BudgetWindow::Day)
                    .then(|| ledger_day_reset(&ledger, now, &zone))
                    .flatten(),
            });
        }
        if agent.budget_park.is_some() || active_scope_waiver(agent, &scope_state) {
            continue;
        }
        if let (Some(parked), Some(cap_usd)) = (fleet.parked.as_ref(), fleet_cap) {
            let spend_usd = current_workspace_day(runtime, day_cutoff)
                .map_or(parked.at_cost, |spend| spend.max(parked.at_cost));
            agent.budget_park = Some(BudgetPark {
                cap_usd,
                spend_usd,
                window: BudgetWindow::Day,
                at: parked.at,
                scope: BudgetScope::Fleet,
                account_kind: None,
                resets_at: next_day_start(now, &zone),
            });
            continue;
        }
        let account = read_account_ledger(runtime, &agent.kind);
        let Some(parked) = account.parked.as_ref() else {
            continue;
        };
        let Some(cap_usd) = account.effective_cap_usd(&agent.kind, config) else {
            continue;
        };
        let spend_usd = (provider.day_cutoff_secs == day_cutoff.unwrap_or_default())
            .then(|| {
                provider
                    .day_by_provider
                    .get(agent.kind.as_str())
                    .map(|day| day.usd)
            })
            .flatten()
            .map_or(parked.at_cost, |spend| spend.max(parked.at_cost));
        agent.budget_park = Some(BudgetPark {
            cap_usd,
            spend_usd,
            window: BudgetWindow::Day,
            at: parked.at,
            scope: BudgetScope::Account,
            account_kind: Some(agent.kind.clone()),
            resets_at: next_day_start(now, &zone),
        });
    }
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
    let day_cutoff = local_day_start(now, &zone).map(|stamp| stamp.as_second().max(0) as u64);
    let fleet_spend = if snapshot.fleet_day_spend_epoch_secs == day_cutoff {
        snapshot.fleet_day_spend_usd.unwrap_or_default()
    } else {
        current_workspace_day(runtime, day_cutoff).unwrap_or_default()
    };
    let mut fleet = read_fleet_ledger(runtime);
    let fleet_before = fleet.clone();
    let fleet_cap = fleet.effective_cap_usd(config);
    let fleet_verdict = evaluate_daily_scope(&mut fleet.parked, fleet_cap, fleet_spend, now);

    let provider = crate::agents::spending::read_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
    );
    let root_kinds = snapshot
        .root_agents()
        .map(|agent| agent.kind.clone())
        .collect::<BTreeSet<_>>();
    let mut accounts = BTreeMap::new();
    for kind in root_kinds {
        let mut ledger = read_account_ledger(runtime, &kind);
        let before = ledger.clone();
        let cap = ledger.effective_cap_usd(&kind, config);
        let spend = (provider.day_cutoff_secs == day_cutoff.unwrap_or_default())
            .then(|| {
                provider
                    .day_by_provider
                    .get(kind.as_str())
                    .map(|day| day.usd)
            })
            .flatten()
            .unwrap_or_default();
        let verdict = evaluate_daily_scope(&mut ledger.parked, cap, spend, now);
        accounts.insert(kind, (ledger, before, verdict));
    }

    let mut scope_state = read_scope_state(runtime);
    let scope_before = scope_state.clone();
    for agent in snapshot.root_agents() {
        if agent.agent_id.is_empty() || agent.agent_id.is_provisional() {
            continue;
        }
        let mut ledger = ledger_for_agent(runtime, agent);
        let before = ledger.clone();
        let latest_delivery = human_deliveries
            .iter()
            .filter(|message| {
                message.same_card(&agent.kind, &agent.agent_id, agent.name.as_deref())
            })
            .filter_map(|message| message.delivered_at)
            .max();
        let verdict = ledger.as_mut().map_or(BudgetVerdict::Disabled, |ledger| {
            evaluate(agent, ledger, now, &zone, latest_delivery)
        });
        let scope_park = binding_scope_park(&fleet, &fleet_verdict, &accounts, &agent.kind);
        let scope_verdict = evaluate_scope_waiver(
            agent,
            scope_park.as_ref().map(|(_, parked)| parked.at),
            latest_delivery,
            &mut scope_state,
            now,
        );
        let agent_parked = matches!(verdict, BudgetVerdict::Park { .. });
        let scope_parked = scope_verdict == ScopeAgentVerdict::Park;
        let all_under = matches!(
            verdict,
            BudgetVerdict::Under { .. } | BudgetVerdict::Disabled
        ) && scope_verdict == ScopeAgentVerdict::Under;

        if all_under {
            crate::harness::auto_continue::clear_budget_park(runtime, &agent.kind, &agent.agent_id);
        } else if agent_parked || scope_parked {
            let daily_park = scope_parked
                || (agent_parked
                    && ledger
                        .as_ref()
                        .is_some_and(|ledger| ledger.spec.window == BudgetWindow::Day));
            if daily_park {
                if let Some(deadline) = next_day_start(now, &zone) {
                    crate::harness::auto_continue::arm_budget_park(
                        runtime,
                        &agent.kind,
                        &agent.agent_id,
                        deadline,
                        agent.last_activity,
                    );
                }
            } else {
                crate::harness::auto_continue::clear_budget_park(
                    runtime,
                    &agent.kind,
                    &agent.agent_id,
                );
            }

            let key = scope_agent_key(agent);
            let last_interrupt = [
                agent_parked
                    .then(|| ledger.as_ref().and_then(|ledger| ledger.last_interrupt_at))
                    .flatten(),
                scope_parked
                    .then(|| scope_state.last_interrupt_at.get(&key).copied())
                    .flatten(),
            ]
            .into_iter()
            .flatten()
            .max();
            if agent.status == AgentStatus::Running
                && interrupt_due(last_interrupt, now)
                && let Some(pane_id) = live_pane(snapshot, agent)
            {
                // A supervised run records this agent's own cost. The broader
                // fleet/account figure only decides why the pane is stopped.
                let at_cost = total_cost_usd(agent);
                if let Some(ledger) = ledger.as_ref()
                    && let Err(err) = write_ledger(runtime, &agent.kind, &agent.agent_id, ledger)
                {
                    warn_write("agent budget ledger", err);
                }
                if spawn_budget_park(runtime, agent, &pane_id, at_cost) {
                    if agent_parked && let Some(ledger) = ledger.as_mut() {
                        ledger.last_interrupt_at = Some(now);
                    }
                    if scope_parked {
                        scope_state.last_interrupt_at.insert(key, now);
                    }
                }
            }
        }
        if ledger != before
            && let Some(ledger) = ledger.as_ref()
            && let Err(err) = write_ledger(runtime, &agent.kind, &agent.agent_id, ledger)
        {
            warn_write("agent budget ledger", err);
        }
    }

    if fleet != fleet_before
        && let Err(err) = write_fleet_ledger(runtime, &fleet)
    {
        warn_write("fleet budget ledger", err);
    }
    for (kind, (ledger, before, _)) in accounts {
        if ledger != before
            && let Err(err) = write_account_ledger(runtime, &kind, &ledger)
        {
            warn_write("account budget ledger", err);
        }
    }
    if scope_state != scope_before
        && let Err(err) = write_scope_state(runtime, &scope_state)
    {
        warn_write("budget scope state", err);
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
    if let Some(delivered) = latest_delivery
        && delivered > threshold
        && state
            .waivers
            .get(&key)
            .is_none_or(|waived| delivered > *waived)
    {
        state.waivers.insert(key.clone(), delivered);
    }
    if let Some(waived) = state.waivers.get(&key).copied() {
        if agent.status == AgentStatus::Running
            && agent
                .turn_started_at
                .is_some_and(|started| started >= waived)
        {
            return ScopeAgentVerdict::Waived;
        }
        if agent.status != AgentStatus::Running
            && agent
                .turn_started_at
                .is_some_and(|started| started >= waived)
        {
            state.waivers.remove(&key);
            state.parked_at.insert(key, now);
        }
    }
    ScopeAgentVerdict::Park
}

fn binding_scope_park<'a>(
    fleet: &'a FleetBudgetLedger,
    fleet_verdict: &BudgetVerdict,
    accounts: &'a BTreeMap<AgentKind, (AccountBudgetLedger, AccountBudgetLedger, BudgetVerdict)>,
    kind: &AgentKind,
) -> Option<(BudgetScope, &'a BudgetParkStamp)> {
    let fleet = matches!(fleet_verdict, BudgetVerdict::Park { .. })
        .then(|| fleet.parked.as_ref().map(|park| (BudgetScope::Fleet, park)))
        .flatten();
    let account = accounts.get(kind).and_then(|(ledger, _, verdict)| {
        matches!(verdict, BudgetVerdict::Park { .. })
            .then(|| {
                ledger
                    .parked
                    .as_ref()
                    .map(|park| (BudgetScope::Account, park))
            })
            .flatten()
    });
    fleet.or(account)
}

fn active_waiver(agent: &AgentState, waived: Option<Timestamp>) -> bool {
    waived.is_some_and(|waived| {
        agent.status == AgentStatus::Running
            && agent
                .turn_started_at
                .is_some_and(|started| started >= waived)
    })
}

fn active_scope_waiver(agent: &AgentState, state: &BudgetScopeState) -> bool {
    active_waiver(agent, state.waivers.get(&scope_agent_key(agent)).copied())
}

fn scope_agent_key(agent: &AgentState) -> String {
    format!("{}:{}", agent.kind, agent.agent_id)
}

fn warn_write(label: &str, err: crate::store::atomic::AtomicErr) {
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
    let cutoff = local_day_start(now, &config.time_zone())
        .map(|stamp| stamp.as_second().max(0) as u64)
        .unwrap_or_default();
    current_workspace_day_cache(runtime, cutoff).unwrap_or_default()
}

pub fn fleet_day_spend_usd(runtime: &RuntimePaths, config: &MachineConfig, now: Timestamp) -> f64 {
    let cutoff =
        local_day_start(now, &config.time_zone()).map(|stamp| stamp.as_second().max(0) as u64);
    current_workspace_day(runtime, cutoff).unwrap_or_default()
}

pub fn account_day_spend_usd(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    config: &MachineConfig,
    now: Timestamp,
) -> f64 {
    let Some(cutoff) =
        local_day_start(now, &config.time_zone()).map(|stamp| stamp.as_second().max(0) as u64)
    else {
        return 0.0;
    };
    let provider = crate::agents::spending::read_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
    );
    if provider.day_cutoff_secs != cutoff {
        return 0.0;
    }
    provider
        .day_by_provider
        .get(kind.as_str())
        .map_or(0.0, |day| day.usd)
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
    let cutoff = local_day_start(now, &config.time_zone())
        .map(|stamp| stamp.as_second().max(0) as u64)
        .unwrap_or_default();
    let fleet = read_fleet_ledger(runtime);
    snapshot.fleet_budget = fleet.effective_cap_usd(config).map(|cap_usd| {
        let spend_usd = snapshot.fleet_day_spend_usd.unwrap_or_default();
        crate::DailyBudgetView {
            cap_usd,
            spend_usd,
            parked: fleet.parked.is_some() && spend_usd >= cap_usd,
        }
    });
    for panel in &mut snapshot.providers {
        let kind = AgentKind::new_unchecked(panel.kind.clone());
        let ledger = read_account_ledger(runtime, &kind);
        panel.day_budget = ledger.effective_cap_usd(&kind, config).map(|cap_usd| {
            let spend_usd = (provider.day_cutoff_secs == cutoff)
                .then(|| provider.day_by_provider.get(&panel.kind).map(|day| day.usd))
                .flatten()
                .unwrap_or_default();
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
    let cutoff_secs = cutoff.as_second().max(0) as u64;

    let fleet = read_fleet_ledger(runtime);
    if let Some(cap) = fleet.effective_cap_usd(config) {
        let mut spend = current_workspace_day(runtime, Some(cutoff_secs)).unwrap_or_default();
        if let Some(parked) = fleet.parked.as_ref().filter(|parked| parked.at >= cutoff) {
            spend = spend.max(parked.at_cost);
        }
        if spend >= cap {
            return Some(format!(
                "fleet budget exhausted (${spend:.2} of ${cap:.2}/day); use `rimz budget` to raise or clear it"
            ));
        }
    }

    let account = read_account_ledger(runtime, kind);
    if let Some(cap) = account.effective_cap_usd(kind, config) {
        let provider = crate::agents::spending::read_provider_spending_cache(
            &runtime.shared_provider_spending_path(),
        );
        let mut spend = (provider.day_cutoff_secs == cutoff_secs)
            .then(|| {
                provider
                    .day_by_provider
                    .get(kind.as_str())
                    .map(|day| day.usd)
            })
            .flatten()
            .unwrap_or_default();
        if let Some(parked) = account.parked.as_ref().filter(|parked| parked.at >= cutoff) {
            spend = spend.max(parked.at_cost);
        }
        if spend >= cap {
            return Some(format!(
                "{} account budget exhausted (${spend:.2} of ${cap:.2}/day); use `rimz budget --account {}` to raise or clear it",
                kind, kind
            ));
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
    let mut hasher = Sha256::new();
    hasher.update(kind.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(agent_id.as_str().as_bytes());
    let digest = hex::encode(hasher.finalize());
    runtime.root.join(format!("budget.{}.json", &digest[..32]))
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

pub fn fleet_ledger_path(runtime: &RuntimePaths) -> PathBuf {
    runtime.root.join("budget.fleet.json")
}

pub fn read_fleet_ledger(runtime: &RuntimePaths) -> FleetBudgetLedger {
    std::fs::read(fleet_ledger_path(runtime))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn write_fleet_ledger(
    runtime: &RuntimePaths,
    ledger: &FleetBudgetLedger,
) -> crate::store::atomic::Result<()> {
    write_temp_then_rename_cache(&fleet_ledger_path(runtime), ledger)
}

pub fn account_ledger_path(runtime: &RuntimePaths, kind: &AgentKind) -> PathBuf {
    let component = if kind
        .as_str()
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        kind.as_str().to_owned()
    } else {
        let mut hasher = Sha256::new();
        hasher.update(kind.as_str().as_bytes());
        format!("kind-{}", &hex::encode(hasher.finalize())[..16])
    };
    runtime
        .persistent_shared_root
        .join(format!("budget.account.{component}.json"))
}

pub fn read_account_ledger(runtime: &RuntimePaths, kind: &AgentKind) -> AccountBudgetLedger {
    std::fs::read(account_ledger_path(runtime, kind))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn write_account_ledger(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    ledger: &AccountBudgetLedger,
) -> crate::store::atomic::Result<()> {
    write_temp_then_rename_cache(&account_ledger_path(runtime, kind), ledger)
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

pub fn clear_resume_park(runtime: &RuntimePaths, kind: &AgentKind, agent_id: &AgentSessionId) {
    crate::harness::auto_continue::clear_budget_park(runtime, kind, agent_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentContext, AgentCost};

    fn agent(cost: f64, status: AgentStatus, turn_started_at: Option<Timestamp>) -> AgentState {
        let now = Timestamp::from_second(100).expect("timestamp");
        let mut agent = AgentState::stub("claude", "sess", status);
        agent.turn_started_at = turn_started_at;
        agent.context = Some(AgentContext {
            source: "test".to_owned(),
            cost: Some(AgentCost {
                total_cost_usd: Some(cost),
                ..AgentCost::default()
            }),
            observed_at: now,
            ..crate::store::agent_context::empty_context("test", now)
        });
        agent
    }

    #[test]
    fn budget_spec_accepts_canonical_forms_and_rejects_bad_values() {
        for (raw, cap, window, display) in [
            ("5", 5.0, BudgetWindow::Session, "$5.00"),
            ("$4.50", 4.5, BudgetWindow::Session, "$4.50"),
            ("20/day", 20.0, BudgetWindow::Day, "$20.00/day"),
        ] {
            let spec: BudgetSpec = raw.parse().expect(raw);
            assert_eq!(spec.cap_usd, cap);
            assert_eq!(spec.window, window);
            assert_eq!(spec.to_string(), display);
        }
        for raw in ["", "$", "+5", "-1", "NaN", "5/week", "1/day/nope"] {
            assert!(raw.parse::<BudgetSpec>().is_err(), "{raw}");
        }
    }

    #[test]
    fn absolute_budget_parks_and_one_human_delivery_waives_one_turn() {
        let zone = TimeZone::UTC;
        let now = Timestamp::from_second(200).expect("timestamp");
        let mut ledger = BudgetLedger::new("5".parse().expect("spec"));
        let idle = agent(6.0, AgentStatus::Idle, Some(now));
        assert!(matches!(
            evaluate(&idle, &mut ledger, now, &zone, None),
            BudgetVerdict::Park { .. }
        ));
        let delivered = Timestamp::from_second(201).expect("timestamp");
        let running = agent(6.0, AgentStatus::Running, Some(delivered));
        assert!(matches!(
            evaluate(
                &running,
                &mut ledger,
                Timestamp::from_second(202).expect("timestamp"),
                &zone,
                Some(delivered)
            ),
            BudgetVerdict::Waived { .. }
        ));
        let idle = agent(6.0, AgentStatus::Idle, Some(delivered));
        assert!(matches!(
            evaluate(
                &idle,
                &mut ledger,
                Timestamp::from_second(203).expect("timestamp"),
                &zone,
                Some(delivered)
            ),
            BudgetVerdict::Park { .. }
        ));
        assert!(
            ledger
                .parked
                .as_ref()
                .is_some_and(|park| park.at.as_second() == 203)
        );
    }

    #[test]
    fn day_budget_rebases_on_first_sight_and_when_local_date_advances() {
        let zone = TimeZone::UTC;
        let first = "2026-06-01T23:59:00Z".parse().expect("timestamp");
        let next = "2026-06-02T00:01:00Z".parse().expect("timestamp");
        let mut ledger = BudgetLedger::new("5/day".parse().expect("spec"));
        let resumed = agent(6.0, AgentStatus::Running, Some(first));
        assert!(matches!(
            evaluate(&resumed, &mut ledger, first, &zone, None),
            BudgetVerdict::Under { spend_usd, .. } if spend_usd == 0.0
        ));
        let over = agent(12.0, AgentStatus::Running, Some(first));
        assert!(matches!(
            evaluate(&over, &mut ledger, first, &zone, None),
            BudgetVerdict::Park { .. }
        ));
        let reset = agent(12.5, AgentStatus::Idle, Some(next));
        assert!(matches!(
            evaluate(&reset, &mut ledger, next, &zone, None),
            BudgetVerdict::Under { spend_usd, .. } if spend_usd == 0.0
        ));
        assert!(ledger.parked.is_none());
    }

    #[test]
    fn active_absolute_waiver_hides_the_paused_projection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");
        let delivered = Timestamp::from_second(201).expect("timestamp");
        let mut running = agent(6.0, AgentStatus::Running, Some(delivered));
        let mut ledger = BudgetLedger::new("5".parse().expect("spec"));
        ledger.parked = Some(BudgetParkStamp {
            at_cost: 6.0,
            at: Timestamp::from_second(200).expect("timestamp"),
        });
        ledger.waived_delivery_at = Some(delivered);
        write_ledger(&runtime, &running.kind, &running.agent_id, &ledger).expect("write ledger");

        let mut snapshot = SidebarSnapshot::build_with_agents(
            workspace_id,
            vec![running.clone()],
            Timestamp::from_second(202).expect("timestamp"),
        );
        project_parks(&mut snapshot, &runtime, &MachineConfig::default());
        assert!(snapshot.agents[0].budget_park.is_none());

        running.status = AgentStatus::Idle;
        let mut snapshot = SidebarSnapshot::build_with_agents(
            runtime.workspace_id.clone(),
            vec![running],
            Timestamp::from_second(203).expect("timestamp"),
        );
        project_parks(&mut snapshot, &runtime, &MachineConfig::default());
        assert!(snapshot.agents[0].budget_park.is_some());
    }

    #[test]
    fn interrupt_retry_is_throttled_for_two_minutes() {
        let at = Timestamp::from_second(1_000).expect("timestamp");
        assert!(!interrupt_due(
            Some(at),
            Timestamp::from_second(1_119).expect("timestamp")
        ));
        assert!(interrupt_due(
            Some(at),
            Timestamp::from_second(1_120).expect("timestamp")
        ));
    }

    #[test]
    fn only_interactive_human_delivery_waives_a_budget() {
        let state = agent(0.0, AgentStatus::Idle, None);
        let mut message = MessageRecord::new(
            crate::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/budget")),
            &state,
            "continue".to_owned(),
            true,
            DeliveryGate::Done,
        );
        message.status = MessageStatus::Delivered;
        assert!(is_budget_waiving_delivery(&message));

        message.automated = true;
        assert!(!is_budget_waiving_delivery(&message));
        message.automated = false;
        message.gate = DeliveryGate::Resume;
        assert!(!is_budget_waiving_delivery(&message));
        message.gate = DeliveryGate::Done;
        message.sender = MessageSender::Agent {
            kind: state.kind,
            name: None,
            profile: None,
            role: None,
            channel: None,
        };
        assert!(!is_budget_waiving_delivery(&message));
    }

    #[test]
    fn daily_scopes_park_and_reopen_when_spend_resets() {
        let now = Timestamp::from_second(1_000).expect("timestamp");
        let mut parked = None;
        assert!(matches!(
            evaluate_daily_scope(&mut parked, Some(5.0), 4.99, now),
            BudgetVerdict::Under { .. }
        ));
        assert!(matches!(
            evaluate_daily_scope(&mut parked, Some(5.0), 5.0, now),
            BudgetVerdict::Park { .. }
        ));
        assert_eq!(parked.as_ref().map(|park| park.at_cost), Some(5.0));
        assert!(matches!(
            evaluate_daily_scope(
                &mut parked,
                Some(5.0),
                0.25,
                Timestamp::from_second(2_000).expect("timestamp")
            ),
            BudgetVerdict::Under { .. }
        ));
        assert!(parked.is_none());
    }

    #[test]
    fn scope_waiver_is_consumed_after_exactly_one_turn() {
        let parked = Timestamp::from_second(200).expect("parked");
        let delivered = Timestamp::from_second(201).expect("delivered");
        let mut state = BudgetScopeState::default();
        let idle = agent(0.0, AgentStatus::Idle, Some(parked));
        assert_eq!(
            evaluate_scope_waiver(&idle, Some(parked), Some(delivered), &mut state, delivered),
            ScopeAgentVerdict::Park
        );
        let running = agent(0.0, AgentStatus::Running, Some(delivered));
        assert_eq!(
            evaluate_scope_waiver(
                &running,
                Some(parked),
                Some(delivered),
                &mut state,
                Timestamp::from_second(202).expect("timestamp")
            ),
            ScopeAgentVerdict::Waived
        );
        let finished = agent(0.0, AgentStatus::Idle, Some(delivered));
        assert_eq!(
            evaluate_scope_waiver(
                &finished,
                Some(parked),
                Some(delivered),
                &mut state,
                Timestamp::from_second(203).expect("timestamp")
            ),
            ScopeAgentVerdict::Park
        );
        let next = agent(
            0.0,
            AgentStatus::Running,
            Some(Timestamp::from_second(204).expect("timestamp")),
        );
        assert_eq!(
            evaluate_scope_waiver(
                &next,
                Some(parked),
                Some(delivered),
                &mut state,
                Timestamp::from_second(204).expect("timestamp")
            ),
            ScopeAgentVerdict::Park
        );
        let later_park = Timestamp::from_second(205).expect("later park");
        assert_eq!(
            evaluate_scope_waiver(
                &next,
                Some(later_park),
                Some(Timestamp::from_second(204).expect("old delivery")),
                &mut state,
                later_park,
            ),
            ScopeAgentVerdict::Park
        );
        assert_eq!(state.parked_at.get("claude:sess"), Some(&later_park));
    }

    #[test]
    fn scope_ledgers_round_trip_and_labels_name_the_binding_scope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .expect("runtime");
        runtime.ensure_dirs().expect("dirs");
        let fleet = FleetBudgetLedger {
            override_spec: Some("20/day".parse().expect("spec")),
            raised_cap_usd: Some(25.0),
            disabled: false,
            parked: Some(BudgetParkStamp {
                at_cost: 25.5,
                at: Timestamp::from_second(100).expect("timestamp"),
            }),
        };
        write_fleet_ledger(&runtime, &fleet).expect("fleet write");
        assert_eq!(read_fleet_ledger(&runtime), fleet);

        let kind = AgentKind::new_unchecked("claude");
        let account = AccountBudgetLedger {
            raised_cap_usd: Some(100.0),
            disabled: false,
            parked: fleet.parked.clone(),
        };
        write_account_ledger(&runtime, &kind, &account).expect("account write");
        assert_eq!(read_account_ledger(&runtime, &kind), account);

        let fleet_label = BudgetPark {
            cap_usd: 25.0,
            spend_usd: 25.5,
            window: BudgetWindow::Day,
            at: Timestamp::from_second(100).expect("timestamp"),
            scope: BudgetScope::Fleet,
            account_kind: None,
            resets_at: None,
        };
        assert_eq!(fleet_label.label(), "fleet budget: $25.50 of $25.00/day");
        assert_eq!(
            BudgetPark {
                scope: BudgetScope::Account,
                account_kind: Some(kind),
                ..fleet_label
            }
            .label(),
            "claude account budget: $25.50 of $25.00/day"
        );
    }

    #[test]
    fn park_projection_uses_agent_then_fleet_then_account_precedence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_id = crate::ids::WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
        runtime.ensure_dirs().expect("dirs");
        let config: MachineConfig = toml::from_str(
            "timezone = \"UTC\"\n[harness]\nbudget = \"10/day\"\n[accounts.budget]\nclaude = \"20/day\"\n",
        )
        .expect("config");
        let now = Timestamp::from_second(200).expect("timestamp");
        let state = agent(6.0, AgentStatus::Idle, Some(now));
        let parked = BudgetParkStamp {
            at_cost: 30.0,
            at: now,
        };

        let mut agent_ledger = BudgetLedger::new("5".parse().expect("spec"));
        agent_ledger.parked = Some(parked.clone());
        write_ledger(&runtime, &state.kind, &state.agent_id, &agent_ledger).expect("agent ledger");
        write_fleet_ledger(
            &runtime,
            &FleetBudgetLedger {
                parked: Some(parked.clone()),
                ..Default::default()
            },
        )
        .expect("fleet ledger");
        write_account_ledger(
            &runtime,
            &state.kind,
            &AccountBudgetLedger {
                parked: Some(parked),
                ..Default::default()
            },
        )
        .expect("account ledger");

        let projected_scope = |state: &AgentState| {
            let mut snapshot =
                SidebarSnapshot::build_with_agents(workspace_id.clone(), vec![state.clone()], now);
            project_parks(&mut snapshot, &runtime, &config);
            snapshot.agents[0]
                .budget_park
                .as_ref()
                .map(|park| park.scope)
        };
        assert_eq!(projected_scope(&state), Some(BudgetScope::Agent));

        std::fs::remove_file(budget_ledger_path(&runtime, &state.kind, &state.agent_id))
            .expect("remove agent ledger");
        assert_eq!(projected_scope(&state), Some(BudgetScope::Fleet));

        let mut fleet = read_fleet_ledger(&runtime);
        fleet.disabled = true;
        write_fleet_ledger(&runtime, &fleet).expect("disable fleet");
        assert_eq!(projected_scope(&state), Some(BudgetScope::Account));
    }

    #[test]
    fn scope_gate_reads_room_and_account_local_day_caches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimePaths::under(
            crate::ids::WorkspaceId::from_project_root(dir.path()),
            dir.path(),
        )
        .expect("runtime");
        runtime.ensure_dirs().expect("dirs");
        let config: MachineConfig = toml::from_str(
            "timezone = \"UTC\"\n[harness]\nbudget = \"5/day\"\n[accounts.budget]\nclaude = \"10/day\"\n",
        )
        .expect("config");
        let now: Timestamp = "2026-06-02T12:00:00Z".parse().expect("now");
        let cutoff = local_day_start(now, &TimeZone::UTC)
            .expect("cutoff")
            .as_second() as u64;
        crate::agents::spending::write_workspace_spending_cache(
            &runtime.workspace_spending_path("scope"),
            &crate::agents::spending::WorkspaceSpendingCache {
                scope_hash: "scope".to_owned(),
                day: crate::agents::spending::SpendWindow {
                    usd: 5.25,
                    ..Default::default()
                },
                day_cutoff_secs: cutoff,
                ..Default::default()
            },
        );
        let kind = AgentKind::new_unchecked("claude");
        assert!(
            scope_gate(&runtime, &kind, &config, now)
                .is_some_and(|reason| reason.contains("fleet budget exhausted"))
        );

        let mut fleet = read_fleet_ledger(&runtime);
        fleet.disabled = true;
        write_fleet_ledger(&runtime, &fleet).expect("disable fleet");
        let spending = crate::agents::spending::Spending::default();
        let provider_day = BTreeMap::from([(
            "claude".to_owned(),
            crate::agents::spending::SpendWindow {
                usd: 10.5,
                ..Default::default()
            },
        )]);
        crate::agents::spending::write_provider_spending_cache_with_day(
            &runtime.shared_provider_spending_path(),
            now.as_millisecond() as u64,
            &spending,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &provider_day,
            cutoff,
        );
        assert!(
            scope_gate(&runtime, &kind, &config, now)
                .is_some_and(|reason| reason.contains("claude account budget exhausted"))
        );
    }
}
