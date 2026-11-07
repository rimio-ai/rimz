//! Inspect and retune one session without changing its configured defaults.

mod budget;

use std::io::Write;

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use serde::Serialize;

use super::{Ctx, GlobalFlags};
use rimz::agents::state::{TuneOverride, TuneSetting};
use rimz::config::{FlipCompact, IdleCompact, KeepWarm, MachineConfig};
use rimz::harness::budget::{BudgetSpec, read_ledger};
use rimz::harness::cache_keepalive::SeatPolicy;
use rimz::store::event::AgentTunePayload;
use rimz::store::message::AutoCompact;
use rimz::store::snapshot::SidebarSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum Setting {
    KeepWarm,
    IdleCompact,
    SmartCompact,
    Budget,
}

impl Setting {
    fn name(self) -> String {
        TuneSetting::from(self).to_string()
    }
}

impl From<Setting> for TuneSetting {
    fn from(setting: Setting) -> Self {
        match setting {
            Setting::KeepWarm => Self::KeepWarm,
            Setting::IdleCompact => Self::IdleCompact,
            Setting::SmartCompact => Self::SmartCompact,
            Setting::Budget => Self::Budget,
        }
    }
}

#[derive(Debug, Args)]
pub(super) struct TuneArgs {
    /// Agent address (`@coder`, kind, name, or exact session id).
    #[arg(value_name = "AGENT")]
    reference: String,
    /// Setting to inspect or change; omit to report all four.
    #[arg(value_enum)]
    setting: Option<Setting>,
    /// New value, `off`, or `reset`; omit to inspect.
    value: Option<String>,
    /// Lift a budget park without queueing the configured continue prompt.
    #[arg(long)]
    no_continue: bool,
    /// Print structured tuning facts.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
pub(super) struct BudgetArgs {
    /// Agent address (`@coder`, kind, name, or exact session id).
    reference: String,
    /// New cap, `+AMOUNT`, `off`/`clear`, or `reset`; omit to inspect.
    value: Option<String>,
    /// Lift a park without queueing the configured continue prompt.
    #[arg(long)]
    no_continue: bool,
    /// Print structured budget facts.
    #[arg(long)]
    json: bool,
}

impl From<BudgetArgs> for TuneArgs {
    fn from(args: BudgetArgs) -> Self {
        Self {
            reference: args.reference,
            setting: Some(Setting::Budget),
            value: args.value,
            no_continue: args.no_continue,
            json: args.json,
        }
    }
}

pub(super) fn run_tune(args: TuneArgs, globals: &GlobalFlags) -> Result<()> {
    if args.no_continue && args.setting != Some(Setting::Budget) {
        return Err(crate::cli::usage::UsageError::new(
            "--no-continue is only valid with setting budget".into(),
        )
        .into());
    }
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.resolution_snapshot_with_context()?;
    let peers = rimz::address::addressable_agents(&snapshot);
    let resolved_agent = resolve_target(&ctx, &snapshot, &args.reference)?;
    let agent = &resolved_agent;
    let by = if args.value.is_some() {
        crate::cli::send::resolve_caller(&ctx.store)?
            .and_then(|caller| {
                rimz::harness::ancestry::resolve_launch_caller(&snapshot.agents, &caller).ok()
            })
            .map(|caller| rimz::address::agent_handle(caller, &peers, true))
    } else {
        None
    };
    if args.setting == Some(Setting::Budget) {
        return budget::run_budget(&ctx, agent, &peers, &args, by);
    }

    let updated_snapshot;
    let agent = if let (Some(setting), Some(raw)) = (args.setting, args.value.as_deref()) {
        let value = if raw.trim() == "reset" {
            None
        } else {
            Some(canonical_value(setting, raw)?)
        };
        ctx.store.tune_agent(
            &agent.kind,
            &AgentTunePayload {
                agent_id: agent.agent_id.clone(),
                setting: setting.into(),
                value,
                by,
            },
            &ctx.workspace.session_name,
        )?;
        updated_snapshot = ctx.store.snapshot()?;
        updated_snapshot
            .agents
            .iter()
            .find(|updated| updated.kind == agent.kind && updated.agent_id == agent.agent_id)
            .context("tuned session is no longer in the rollup")?
    } else {
        agent
    };
    let machine = MachineConfig::load_lenient();
    let policy = SeatPolicy::load(&machine, Some(&ctx.workspace.project_root));
    let configured = policy.configured_values(agent, &machine.harness);
    let reports = [
        setting_report(
            Setting::KeepWarm,
            agent.tune.keep_warm.as_ref(),
            configured
                .keep_warm
                .as_ref()
                .map(|(max, _)| KeepWarm::For(*max).to_string()),
            ToString::to_string,
        ),
        setting_report(
            Setting::IdleCompact,
            agent.tune.idle_compact.as_ref(),
            configured
                .idle_compact
                .map(|floor| IdleCompact::Tokens(floor).to_string()),
            ToString::to_string,
        ),
        setting_report(
            Setting::SmartCompact,
            agent.tune.smart_compact.as_ref(),
            configured
                .smart_compact
                .map(|line| smart_value(&FlipCompact::Threshold(line))),
            smart_value,
        ),
    ];
    let handle = target_handle(agent, &peers);
    if args.setting.is_none() && args.json {
        let ledger = read_ledger(ctx.store.runtime_paths(), &agent.kind, &agent.agent_id);
        let launched = agent
            .budget
            .as_deref()
            .map(str::parse::<BudgetSpec>)
            .transpose()?;
        let mut budget =
            serde_json::to_value(budget::report(agent, &peers, ledger.as_ref(), launched))?;
        // AgentBudgetReport is a struct with derived serialization, so it is always an object.
        budget
            .as_object_mut()
            .expect("a derived budget report serializes as an object")
            .remove("agent");
        return crate::cli::render::json(&serde_json::json!({
            "agent": handle, "settings": reports, "budget": budget,
        }));
    }
    for report in &reports {
        if args
            .setting
            .is_some_and(|setting| setting != report.setting)
        {
            continue;
        }
        if args.json {
            let mut value = serde_json::to_value(report)?;
            value["agent"] = serde_json::json!(handle);
            return crate::cli::render::json(&value);
        }
        let condition = (report.setting == Setting::KeepWarm)
            .then(|| {
                configured
                    .keep_warm
                    .as_ref()
                    .and_then(|(_, condition)| condition.as_deref())
            })
            .flatten();
        render_setting(&handle, report, condition)?;
    }
    if args.setting.is_none() {
        return budget::run_budget(&ctx, agent, &peers, &args, None);
    }
    Ok(())
}

fn resolve_target(
    ctx: &Ctx,
    snapshot: &SidebarSnapshot,
    reference: &str,
) -> Result<rimz::agents::AgentState> {
    let error = match super::resolve_live_or_audit(
        &ctx.store,
        &ctx.workspace,
        ctx.store.runtime_paths(),
        snapshot,
        reference,
        &ctx.address_context(),
    ) {
        Ok((agent, _)) => return Ok(agent),
        Err(error) if crate::cli::resolution_missed(&error) => error,
        Err(error) => return Err(error),
    };
    let audit = ctx.store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let candidates = audit
        .agents
        .iter()
        .filter(|agent| agent.is_provider_subagent())
        .collect::<Vec<_>>();
    match rimz::address::resolve_agent(reference, None, &ctx.address_context(), &candidates) {
        Ok(agent) => Ok(agent.clone()),
        Err(
            rimz::address::TargetErr::NoMatch { .. }
            | rimz::address::TargetErr::NoMatchInChannel { .. },
        ) => Err(error),
        Err(error) => Err(error.into()),
    }
}

fn target_handle(agent: &rimz::agents::AgentState, peers: &[&rimz::agents::AgentState]) -> String {
    if agent.ended_at.is_some() || agent.is_provider_subagent() {
        return format!("@{}", agent.agent_id);
    }
    rimz::address::agent_handle(agent, peers, true)
}

fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    Ok(serde_json::from_value(serde_json::json!(raw))?)
}

fn canonical_value(setting: Setting, raw: &str) -> Result<String> {
    Ok(match setting {
        Setting::KeepWarm => parse::<KeepWarm>(raw)?.to_string(),
        Setting::IdleCompact => parse::<IdleCompact>(raw)?.to_string(),
        Setting::SmartCompact => smart_value(&parse(raw)?),
        Setting::Budget => raw.to_owned(),
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

#[derive(Serialize)]
struct SettingReport {
    setting: Setting,
    value: String,
    source: &'static str,
    configured: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    set_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    set_at: Option<jiff::Timestamp>,
}

fn setting_report<T>(
    setting: Setting,
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

fn render_setting(handle: &str, report: &SettingReport, condition: Option<&str>) -> Result<()> {
    let mut out = crate::cli::render::out();
    write!(
        out,
        "{handle} {}: {} · {}",
        report.setting.name(),
        report.value,
        report.source
    )?;
    if report.source == "override" {
        if let Some(by) = &report.set_by {
            write!(out, " by {by}")?;
        }
        write!(
            out,
            " · config {}",
            report.configured.as_deref().unwrap_or("off")
        )?;
    }
    if let Some(condition) = condition {
        write!(out, " while {condition}")?;
    }
    writeln!(out)?;
    Ok(())
}
