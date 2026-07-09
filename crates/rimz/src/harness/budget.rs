//! Per-agent dollar caps and their runtime ledger.
//!
//! Launch identity carries the configured [`BudgetSpec`]. The sidebar producer
//! reads live session cost, updates one cache-class ledger per session, and
//! spawns the hidden `agents budget-park` helper when a running turn crosses
//! its cap. The helper owns the pane interrupt and supervised-run transition;
//! this module stays safe in the sidebar's read-only store import graph.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use jiff::{Timestamp, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::RuntimePaths;
use crate::agents::{AgentState, AgentStatus};
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

/// Read-side park projection carried in snapshots and agent inspection JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetPark {
    pub cap_usd: f64,
    pub spend_usd: f64,
    pub window: BudgetWindow,
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
}

impl BudgetPark {
    pub fn label(&self) -> String {
        format!(
            "budget: ${:.2} of ${:.2}{}",
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
                        cost_usd: 0.0,
                    });
                }
                Some(_) => {}
            }
            (total
                - ledger
                    .day_baseline
                    .as_ref()
                    .map_or(0.0, |baseline| baseline.cost_usd))
            .max(0.0)
        }
    };
    if spend_usd < cap_usd {
        ledger.parked = None;
        ledger.last_interrupt_at = None;
        return BudgetVerdict::Under { spend_usd, cap_usd };
    }

    if ledger.spec.window == BudgetWindow::Session {
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
    }

    ledger.parked.get_or_insert(BudgetParkStamp {
        at_cost: total,
        at: now,
    });
    BudgetVerdict::Park { spend_usd, cap_usd }
}

/// Stamp ledger park projections onto agent state for producer and consumer
/// folds. This reads runtime cache files only.
pub fn project_parks(snapshot: &mut SidebarSnapshot, runtime: &RuntimePaths, zone: &TimeZone) {
    let now = snapshot.now;
    for agent in &mut snapshot.agents {
        agent.budget_park = None;
        let Some(ledger) = ledger_for_agent(runtime, agent) else {
            continue;
        };
        let (Some(parked), Some(cap_usd)) = (ledger.parked.as_ref(), ledger.effective_cap_usd())
        else {
            continue;
        };
        if ledger.spec.window == BudgetWindow::Session
            && let Some(waived) = ledger.waived_delivery_at
            && agent.status == AgentStatus::Running
            && agent
                .turn_started_at
                .is_some_and(|started| started >= waived)
        {
            continue;
        }
        let total = total_cost_usd(agent).unwrap_or(parked.at_cost);
        let spend_usd = match ledger.spec.window {
            BudgetWindow::Session => total,
            BudgetWindow::Day => (total
                - ledger
                    .day_baseline
                    .as_ref()
                    .map_or(0.0, |baseline| baseline.cost_usd))
            .max(0.0),
        };
        agent.budget_park = Some(BudgetPark {
            cap_usd,
            spend_usd,
            window: ledger.spec.window,
            at: parked.at,
            resets_at: (ledger.spec.window == BudgetWindow::Day)
                .then(|| ledger_day_reset(&ledger, now, zone))
                .flatten(),
        });
    }
}

/// Producer-side enforcement. Ledger files are cache-class durability; pane
/// interruption and run-store writes stay in the hidden CLI helper.
pub fn enforce(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    messages_dir: &Path,
    zone: &TimeZone,
) {
    let now = snapshot.now;
    let human_deliveries = delivered_human_messages(messages_dir);
    for agent in snapshot.root_agents() {
        if agent.agent_id.is_empty() || agent.agent_id.is_provisional() {
            continue;
        }
        let Some(mut ledger) = ledger_for_agent(runtime, agent) else {
            continue;
        };
        let before = ledger.clone();
        let latest_delivery = human_deliveries
            .iter()
            .filter(|message| {
                message.same_card(&agent.kind, &agent.agent_id, agent.name.as_deref())
            })
            .filter_map(|message| message.delivered_at)
            .max();
        let verdict = evaluate(agent, &mut ledger, now, zone, latest_delivery);
        match verdict {
            BudgetVerdict::Under { .. } | BudgetVerdict::Disabled => {
                crate::harness::auto_continue::clear_budget_park(
                    runtime,
                    &agent.kind,
                    &agent.agent_id,
                );
            }
            BudgetVerdict::Waived { .. } => {}
            BudgetVerdict::Park { .. } => {
                if ledger.spec.window == BudgetWindow::Day
                    && let Some(deadline) = next_day_start(now, zone)
                {
                    crate::harness::auto_continue::arm_budget_park(
                        runtime,
                        &agent.kind,
                        &agent.agent_id,
                        deadline,
                        agent.last_activity,
                    );
                }
                if agent.status == AgentStatus::Running
                    && interrupt_due(ledger.last_interrupt_at, now)
                    && let Some(pane_id) = live_pane(snapshot, agent)
                {
                    // Publish the park before the helper can race its read.
                    if let Err(err) = write_ledger(runtime, &agent.kind, &agent.agent_id, &ledger) {
                        tracing::warn!(
                            tags.operation = "budget.write_ledger",
                            error = &err as &dyn std::error::Error,
                            "failed to write agent budget ledger",
                        );
                    }
                    if spawn_budget_park(runtime, agent, &pane_id) {
                        ledger.last_interrupt_at = Some(now);
                    }
                }
            }
        }
        if ledger != before
            && let Err(err) = write_ledger(runtime, &agent.kind, &agent.agent_id, &ledger)
        {
            tracing::warn!(
                tags.operation = "budget.write_ledger",
                error = &err as &dyn std::error::Error,
                "failed to write agent budget ledger",
            );
        }
    }
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
        .filter(|message| {
            // Auto-continue records render as human-authored for transcript
            // attribution, but their resume gate must not waive a dollar cap.
            message.status == MessageStatus::Delivered
                && matches!(message.sender, MessageSender::Human)
                && message.gate != DeliveryGate::Resume
        })
        .collect()
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
fn spawn_budget_park(runtime: &RuntimePaths, agent: &AgentState, pane_id: &PaneId) -> bool {
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
fn spawn_budget_park(_runtime: &RuntimePaths, _agent: &AgentState, _pane_id: &PaneId) -> bool {
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
    fn day_budget_rebases_when_local_date_advances() {
        let zone = TimeZone::UTC;
        let first = "2026-06-01T23:59:00Z".parse().expect("timestamp");
        let next = "2026-06-02T00:01:00Z".parse().expect("timestamp");
        let mut ledger = BudgetLedger::new("5/day".parse().expect("spec"));
        let over = agent(6.0, AgentStatus::Running, Some(first));
        assert!(matches!(
            evaluate(&over, &mut ledger, first, &zone, None),
            BudgetVerdict::Park { .. }
        ));
        let reset = agent(6.5, AgentStatus::Idle, Some(next));
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
        project_parks(&mut snapshot, &runtime, &TimeZone::UTC);
        assert!(snapshot.agents[0].budget_park.is_none());

        running.status = AgentStatus::Idle;
        let mut snapshot = SidebarSnapshot::build_with_agents(
            runtime.workspace_id.clone(),
            vec![running],
            Timestamp::from_second(203).expect("timestamp"),
        );
        project_parks(&mut snapshot, &runtime, &TimeZone::UTC);
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
}
