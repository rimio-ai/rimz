//! Inspect and change one agent's runtime dollar cap.

use std::io::Write;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::{Ctx, TuneArgs};
use rimz::agents::BudgetWindow;
use rimz::harness::budget::{
    BudgetLedger, BudgetSpec, DayBaseline, read_ledger, remove_ledger, total_cost_usd, write_ledger,
};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::event::AgentTunePayload;
use rimz::store::message::{DeliveryGate, MessageSender};

pub(super) fn run_budget(
    ctx: &Ctx,
    agent: &rimz::agents::AgentState,
    peers: &[&rimz::agents::AgentState],
    args: &TuneArgs,
    by: Option<String>,
) -> Result<()> {
    if args.value.is_some() {
        if agent.ended_at.is_some() {
            bail!("session has ended");
        }
        if agent.is_provider_subagent() {
            bail!("session is a provider subagent");
        }
    }
    let (workspace, store) = (&ctx.workspace, &ctx.store);
    let launched = agent
        .budget
        .as_deref()
        .map(BudgetSpec::from_str)
        .transpose()
        .context("parsing the agent's launch budget")?;
    let existing = read_ledger(store.runtime_paths(), &agent.kind, &agent.agent_id);
    let resetting = args
        .value
        .as_deref()
        .is_some_and(|value| value.trim() == "reset");
    let has_override = existing.as_ref().is_some_and(|ledger| {
        ledger.raised_cap_usd.is_some() || ledger.disabled || launched.is_none()
    });
    if args.value.is_none() || (resetting && !has_override) {
        return render_budget(agent, peers, existing.as_ref(), launched, args.json);
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
            let next = if resetting {
                launched
            } else {
                Some(raw.parse::<BudgetSpec>()?)
            };
            if let Some(next) = next {
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
                ledger.raised_cap_usd = (!resetting).then_some(next.cap_usd);
                if resetting {
                    ledger.spec = next;
                }
                ledger.disabled = false;
            }
        }
        None => unreachable!("read-only returned above"),
    }
    ledger.parked = None;
    ledger.last_interrupt_at = None;
    ledger.waived_delivery_at = None;
    let removed = resetting && launched.is_none();
    if removed {
        remove_ledger(store.runtime_paths(), &agent.kind, &agent.agent_id)
            .context("removing the agent budget ledger")?;
    } else {
        write_ledger(store.runtime_paths(), &agent.kind, &agent.agent_id, &ledger)
            .context("writing the agent budget ledger")?;
    }
    rimz::harness::budget::clear_budget_park(store.runtime_paths(), &agent.kind, &agent.agent_id);
    store
        .tune_agent(
            &agent.kind,
            &AgentTunePayload {
                agent_id: agent.agent_id.clone(),
                setting: super::TuneSetting::Budget,
                value: if resetting { None } else { args.value.clone() },
                by,
            },
            &workspace.session_name,
        )
        .context("the cap change took effect, but its audit record was not written")?;

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
                    }
                    .record(workspace),
                    &workspace.session_name,
                )
                .context("queueing budget continue prompt")?;
        }
    }
    render_budget(
        agent,
        peers,
        (!removed).then_some(&ledger),
        launched,
        args.json,
    )
}

#[derive(Serialize)]
pub(super) struct AgentBudgetReport {
    agent: String,
    spend_usd: Option<f64>,
    cap_usd: Option<f64>,
    window: Option<&'static str>,
    source: &'static str,
    parked: bool,
}

pub(super) fn report(
    agent: &rimz::agents::AgentState,
    peers: &[&rimz::agents::AgentState],
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
        agent: super::target_handle(agent, peers),
        spend_usd,
        cap_usd,
        window: spec.map(|spec| spec.window.label()),
        source,
        parked: ledger.is_some_and(|ledger| ledger.parked.is_some()),
    }
}

fn render_budget(
    agent: &rimz::agents::AgentState,
    peers: &[&rimz::agents::AgentState],
    ledger: Option<&BudgetLedger>,
    launched: Option<BudgetSpec>,
    json: bool,
) -> Result<()> {
    let report = report(agent, peers, ledger, launched);
    if json {
        return crate::cli::render::json(&report);
    }
    let mut out = crate::cli::render::out();
    let can_tune = agent.ended_at.is_none() && !agent.is_provider_subagent();
    let command = format!("rimz agents tune {} budget", report.agent);
    let Some(cap) = report.cap_usd else {
        write!(out, "{} · no cap", report.agent)?;
        if can_tune {
            write!(out, " · set one: {command} 5")?;
        }
        writeln!(out)?;
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
    write!(
        out,
        "{} · {spend} of ${cap:.2} {window} · {park} · source {}",
        report.agent, report.source
    )?;
    if can_tune {
        write!(out, " · raise: {command} +10 · lift: {command} off")?;
    }
    writeln!(out)?;
    Ok(())
}
