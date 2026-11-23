//! Inspect and retune one session without changing its configured defaults.

mod budget;

use std::io::Write;

use anyhow::Result;
use clap::{Args, ValueEnum};
use serde::Serialize;

use super::{Ctx, GlobalFlags};
use rimz::agents::state::TuneSetting;
use rimz::harness::tune::{SettingReport, target_handle};
use rimz::store::snapshot::SidebarSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
enum Setting {
    KeepWarm,
    IdleCompact,
    SmartCompact,
    Budget,
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

    let change = args.setting.zip(args.value.as_deref());
    let tuned = rimz::harness::tune::tune(
        &ctx.workspace,
        &ctx.store,
        agent,
        change.map(|(setting, raw)| (setting.into(), raw)),
        by,
    )?;
    let agent = &tuned.agent;
    let reports = &tuned.reports;
    let handle = target_handle(agent, &peers);
    if args.setting.is_none() && args.json {
        let mut budget = serde_json::to_value(rimz::harness::tune::budget(
            ctx.store.runtime_paths(),
            agent,
            &peers,
        )?)?;
        // AgentBudgetReport is a struct with derived serialization, so it is always an object.
        budget
            .as_object_mut()
            .expect("a derived budget report serializes as an object")
            .remove("agent");
        return crate::cli::render::json(&serde_json::json!({
            "agent": handle, "settings": reports, "budget": budget,
        }));
    }
    for report in reports {
        if args
            .setting
            .is_some_and(|setting| TuneSetting::from(setting) != report.setting)
        {
            continue;
        }
        if args.json {
            let mut value = serde_json::to_value(report)?;
            value["agent"] = serde_json::json!(handle);
            return crate::cli::render::json(&value);
        }
        let condition = (report.setting == TuneSetting::KeepWarm)
            .then_some(tuned.keep_warm_condition.as_deref())
            .flatten();
        render_setting(
            &rimz::address::handle_with_name(&handle, agent.name.as_deref()),
            report,
            condition,
        )?;
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

fn render_setting(handle: &str, report: &SettingReport, condition: Option<&str>) -> Result<()> {
    let mut out = crate::cli::render::out();
    write!(
        out,
        "{handle} {}: {} · {}",
        report.setting, report.value, report.source
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
