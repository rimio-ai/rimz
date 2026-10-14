//! Inspect and change one agent's runtime dollar cap.

use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Args;
use serde::Serialize;

use super::{Ctx, GlobalFlags};
use rimz::agents::BudgetWindow;
use rimz::harness::budget::{
    BudgetLedger, BudgetSpec, DayBaseline, read_ledger, total_cost_usd, write_ledger,
};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, MessageSender};

#[derive(Debug, Args)]
pub struct BudgetArgs {
    /// Agent address (`@coder`, kind, name, or exact session id).
    reference: String,
    /// New cap, `+AMOUNT`, or `off`/`clear`; omit to inspect.
    value: Option<String>,
    /// Lift a park without queueing the configured continue prompt.
    #[arg(long)]
    no_continue: bool,
    /// Print structured budget facts.
    #[arg(long)]
    json: bool,
}

pub fn run_budget(args: BudgetArgs, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let snapshot = ctx.resolution_snapshot_with_context()?;
    let current_channel = &ctx.address_context();
    let agent =
        crate::cli::resolve_agent_one(store, &snapshot, &args.reference, None, current_channel)?;
    let launched = agent
        .budget
        .as_deref()
        .map(BudgetSpec::from_str)
        .transpose()
        .context("parsing the agent's launch budget")?;
    let existing = read_ledger(store.runtime_paths(), &agent.kind, &agent.agent_id);
    let peers = rimz::address::addressable_agents(&snapshot);

    if args.value.is_none() {
        return render_budget(agent, &peers, existing.as_ref(), launched, args.json);
    }

    let mut ledger = existing
        .or_else(|| launched.map(BudgetLedger::new))
        .unwrap_or_else(|| {
            BudgetLedger::new(BudgetSpec {
                cap_usd: 0.0,
                window: BudgetWindow::Session,
            })
        });
    let was_paused = ledger.parked.is_some() && ledger.last_interrupt_at.is_some();
    match args.value.as_deref().map(str::trim) {
        Some("clear" | "off") => {
            ledger.disabled = true;
            ledger.raised_cap_usd = None;
        }
        Some(raw) if raw.starts_with('+') => {
            let delta = raw[1..]
                .parse::<BudgetSpec>()
                .map_err(|err| anyhow::anyhow!("invalid budget raise `{raw}`: {err}"))?;
            if delta.window != BudgetWindow::Session {
                bail!("a relative budget raise uses `+AMOUNT`, without `/day`");
            }
            let current = ledger
                .effective_cap_usd()
                .context("cannot raise a cleared budget; set an absolute cap first")?;
            let raised = current + delta.cap_usd;
            if !raised.is_finite() {
                bail!("budget raise is too large");
            }
            ledger.raised_cap_usd = Some(raised);
            ledger.disabled = false;
        }
        Some(raw) => {
            let next = raw.parse::<BudgetSpec>()?;
            let previous_window = ledger.spec.window;
            ledger.spec.window = next.window;
            if previous_window != BudgetWindow::Day && next.window == BudgetWindow::Day {
                let zone = rimz::config::MachineConfig::load_lenient().time_zone();
                ledger.day_baseline = Some(DayBaseline {
                    date: jiff::Timestamp::now().to_zoned(zone).date(),
                    cost_usd: total_cost_usd(agent).unwrap_or(0.0),
                });
            } else if next.window == BudgetWindow::Session {
                ledger.day_baseline = None;
            }
            ledger.raised_cap_usd = Some(next.cap_usd);
            ledger.disabled = false;
        }
        None => unreachable!("read-only returned above"),
    }
    ledger.parked = None;
    ledger.last_interrupt_at = None;
    ledger.waived_delivery_at = None;
    write_ledger(store.runtime_paths(), &agent.kind, &agent.agent_id, &ledger)
        .context("writing the agent budget ledger")?;
    rimz::harness::budget::clear_budget_park(store.runtime_paths(), &agent.kind, &agent.agent_id);

    if was_paused && !args.no_continue {
        let text = rimz::config::MachineConfig::load_lenient()
            .resume
            .auto_continue_text
            .clone();
        if !text.trim().is_empty() {
            store
                .queue_message(
                    &SyntheticMessage {
                        agent,
                        text,
                        sender: MessageSender::System,
                        gate: DeliveryGate::Done,
                        pane_id: None,
                    }
                    .record(workspace),
                    &workspace.session_name,
                )
                .context("queueing budget continue prompt")?;
        }
    }
    render_budget(agent, &peers, Some(&ledger), launched, args.json)
}

#[derive(Serialize)]
struct AgentBudgetReport {
    agent: String,
    spend_usd: Option<f64>,
    cap_usd: Option<f64>,
    window: Option<&'static str>,
    source: &'static str,
    parked: bool,
}

fn render_budget(
    agent: &rimz::agents::AgentState,
    peers: &[&rimz::agents::AgentState],
    ledger: Option<&BudgetLedger>,
    launched: Option<BudgetSpec>,
    json: bool,
) -> Result<()> {
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
    let report = AgentBudgetReport {
        agent: rimz::address::agent_handle(agent, peers, true),
        spend_usd,
        cap_usd,
        window: spec.map(|spec| spec.window.label()),
        source,
        parked: ledger.is_some_and(|ledger| ledger.parked.is_some()),
    };
    if json {
        return crate::cli::render::json(&report);
    }
    let mut out = crate::cli::render::out();
    let command = format!("rimz agents budget {}", report.agent);
    let Some(cap) = report.cap_usd else {
        writeln!(out, "{} · no cap · set one: {command} 5", report.agent)?;
        return Ok(());
    };
    let spend = report.spend_usd.map_or_else(
        || "unknown spend".to_owned(),
        |spend| format!("${spend:.2}"),
    );
    let window = if report.window == Some("day") {
        "day"
    } else {
        "session"
    };
    let park = if report.parked {
        "parked"
    } else {
        "not parked"
    };
    writeln!(
        out,
        "{} · {spend} of ${cap:.2} {window} · {park} · source {source} · raise: {command} +10 · lift: {command} off",
        report.agent
    )?;
    Ok(())
}
