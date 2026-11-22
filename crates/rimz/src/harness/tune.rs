//! Retune one session's keep-warm, idle-compaction, and smart-compaction overrides without
//! changing its configured defaults, and report each setting against its configured value
//! (`rimz agents tune`).

use anyhow::{Context, Result};
use serde::Serialize;

use crate::agents::AgentState;
use crate::agents::state::{TuneOverride, TuneSetting};
use crate::config::{FlipCompact, IdleCompact, KeepWarm, MachineConfig};
use crate::harness::budget::{BudgetLedger, BudgetSpec, read_ledger, total_cost_usd};
use crate::harness::cache_keepalive::SeatPolicy;
use crate::store::event::AgentTunePayload;
use crate::store::message::AutoCompact;
use crate::{ResolvedWorkspace, RuntimePaths, Store};

/// One setting's value, where it comes from, and the configured value an override replaces.
#[derive(Debug, Serialize)]
pub struct SettingReport {
    pub setting: TuneSetting,
    pub value: String,
    /// `override` when the session was tuned, else `config`.
    pub source: &'static str,
    pub configured: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_at: Option<jiff::Timestamp>,
}

/// A session's settings after a tune: keep-warm, idle-compact, and smart-compact, in that order.
#[derive(Debug)]
pub struct Tuned {
    /// The session as the store holds it after the change.
    pub agent: AgentState,
    pub reports: [SettingReport; 3],
    /// The condition the configured keep-warm holds while, when it has one.
    pub keep_warm_condition: Option<String>,
}

/// Set `change`'s setting on `agent` to its value, or back to its configured value for
/// `reset`, recorded as set `by`; with no change, read the settings as they stand.
pub fn tune(
    workspace: &ResolvedWorkspace,
    store: &Store,
    agent: &AgentState,
    change: Option<(TuneSetting, &str)>,
    by: Option<String>,
) -> Result<Tuned> {
    let agent = match change {
        Some((setting, raw)) => {
            let value = if raw.trim() == "reset" {
                None
            } else {
                Some(canonical_value(setting, raw)?)
            };
            store.tune_agent(
                &agent.kind,
                &AgentTunePayload {
                    agent_id: agent.agent_id.clone(),
                    setting,
                    value,
                    by,
                },
                &workspace.session_name,
            )?;
            store
                .snapshot()?
                .agents
                .iter()
                .find(|updated| updated.kind == agent.kind && updated.agent_id == agent.agent_id)
                .cloned()
                .context("tuned session is no longer in the rollup")?
        }
        None => agent.clone(),
    };
    let machine = MachineConfig::load_lenient();
    let policy = SeatPolicy::load(&machine, Some(&workspace.project_root));
    let configured = policy.configured_values(&agent, &machine.harness);
    let reports = [
        setting_report(
            TuneSetting::KeepWarm,
            agent.tune.keep_warm.as_ref(),
            configured
                .keep_warm
                .as_ref()
                .map(|(max, _)| KeepWarm::For(*max).to_string()),
            ToString::to_string,
        ),
        setting_report(
            TuneSetting::IdleCompact,
            agent.tune.idle_compact.as_ref(),
            configured
                .idle_compact
                .map(|floor| IdleCompact::Tokens(floor).to_string()),
            ToString::to_string,
        ),
        setting_report(
            TuneSetting::SmartCompact,
            agent.tune.smart_compact.as_ref(),
            configured
                .smart_compact
                .map(|line| smart_value(&FlipCompact::Threshold(line))),
            smart_value,
        ),
    ];
    let keep_warm_condition = configured.keep_warm.and_then(|(_, condition)| condition);
    Ok(Tuned {
        agent,
        reports,
        keep_warm_condition,
    })
}

fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    Ok(serde_json::from_value(serde_json::json!(raw))?)
}

fn canonical_value(setting: TuneSetting, raw: &str) -> Result<String> {
    Ok(match setting {
        TuneSetting::KeepWarm => parse::<KeepWarm>(raw)?.to_string(),
        TuneSetting::IdleCompact => parse::<IdleCompact>(raw)?.to_string(),
        TuneSetting::SmartCompact => smart_value(&parse(raw)?),
        TuneSetting::Budget => raw.to_owned(),
    })
}

fn smart_value(value: &FlipCompact) -> String {
    match value {
        FlipCompact::Off => "off".into(),
        FlipCompact::Threshold(AutoCompact::Percent(percent)) => format!("{percent}%"),
        FlipCompact::Threshold(AutoCompact::Tokens(tokens)) => {
            IdleCompact::Tokens(*tokens).to_string()
        }
    }
}

fn setting_report<T>(
    setting: TuneSetting,
    tuned: Option<&TuneOverride<T>>,
    configured: Option<String>,
    format: impl FnOnce(&T) -> String,
) -> SettingReport {
    SettingReport {
        setting,
        value: tuned
            .map(|tuned| format(&tuned.value))
            .or_else(|| configured.clone())
            .unwrap_or_else(|| "off".into()),
        source: if tuned.is_some() {
            "override"
        } else {
            "config"
        },
        configured,
        set_by: tuned.and_then(|tuned| tuned.by.clone()),
        set_at: tuned.map(|tuned| tuned.at),
    }
}

/// One session's dollar cap: what it has spent in the cap's window, the cap, and where the cap
/// comes from (`launch`, `override`, `cleared`, or `none`).
#[derive(Debug, Serialize)]
pub struct AgentBudgetReport {
    pub agent: String,
    pub spend_usd: Option<f64>,
    pub cap_usd: Option<f64>,
    pub window: Option<&'static str>,
    pub source: &'static str,
    pub parked: bool,
}

/// `agent`'s cap as its ledger stands, or as it launched when it has none.
pub fn budget(
    runtime: &RuntimePaths,
    agent: &AgentState,
    peers: &[&AgentState],
) -> Result<AgentBudgetReport> {
    let ledger = read_ledger(runtime, &agent.kind, &agent.agent_id);
    let launched = agent
        .budget
        .as_deref()
        .map(str::parse::<BudgetSpec>)
        .transpose()?;
    Ok(budget_report(agent, peers, ledger.as_ref(), launched))
}

pub fn budget_report(
    agent: &AgentState,
    peers: &[&AgentState],
    ledger: Option<&BudgetLedger>,
    launched: Option<BudgetSpec>,
) -> AgentBudgetReport {
    let spec = ledger.map(|ledger| ledger.spec).or(launched);
    let spend_usd = total_cost_usd(agent).map(|total| {
        ledger
            .cloned()
            .or_else(|| spec.map(BudgetLedger::new))
            .map_or(total, |ledger| ledger.spend_usd(total))
    });
    let cap_usd = match ledger {
        Some(ledger) => ledger.effective_cap_usd(),
        None => launched.map(|spec| spec.cap_usd),
    };
    let source = match ledger {
        Some(ledger) if ledger.disabled => "cleared",
        Some(ledger) if ledger.raised_cap_usd.is_some() => "override",
        _ if spec.is_some() => "launch",
        _ => "none",
    };
    AgentBudgetReport {
        agent: target_handle(agent, peers),
        spend_usd,
        cap_usd,
        window: spec.map(|spec| spec.window.label()),
        source,
        parked: ledger.is_some_and(|ledger| ledger.parked.is_some()),
    }
}

/// The handle a tune report names `agent` by: its session id once it has ended or when it is
/// a provider subagent, which no handle reaches.
pub fn target_handle(agent: &AgentState, peers: &[&AgentState]) -> String {
    if agent.ended_at.is_some() || agent.is_provider_subagent() {
        return format!("@{}", agent.agent_id);
    }
    crate::address::agent_handle(agent, peers, true)
}
