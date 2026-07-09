//! Inspect and change one agent's runtime dollar cap.

use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::Args;

use super::GlobalFlags;
use rimz::harness::budget::{
    BudgetLedger, BudgetSpec, BudgetWindow, DayBaseline, read_ledger, total_cost_usd, write_ledger,
};
use rimz::message::{DeliveryGate, MessageRecord, MessageSender};
use rimz::workspace::WorkspaceResolver;

#[derive(Debug, Args)]
pub struct BudgetArgs {
    /// Agent address (`@coder`, kind, name, or exact session id).
    reference: String,
    /// New cap, `+AMOUNT`, or `clear`; omit to inspect.
    value: Option<String>,
    /// Clear a park without queueing the configured continue prompt.
    #[arg(long)]
    no_continue: bool,
}

pub fn run_budget(args: BudgetArgs, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let mut snapshot =
        rimz::sidebar::produce::resolution_snapshot(&workspace, &store, globals.mux)?;
    snapshot =
        snapshot.with_agent_context(rimz::store::agent_context::read_all(store.runtime_paths()));
    let current_channel = crate::cli::current_channel(&workspace);
    let agent = crate::cli::resolve_agent_one(
        &snapshot,
        &args.reference,
        None,
        current_channel.as_deref(),
    )?;
    let launched = agent
        .budget
        .as_deref()
        .map(BudgetSpec::from_str)
        .transpose()
        .context("parsing the agent's launch budget")?;
    let existing = read_ledger(store.runtime_paths(), &agent.kind, &agent.agent_id);

    if args.value.is_none() {
        return render_budget(agent, existing.as_ref(), launched);
    }

    let mut ledger = existing
        .or_else(|| launched.map(BudgetLedger::new))
        .unwrap_or_else(|| {
            BudgetLedger::new(BudgetSpec {
                cap_usd: 0.0,
                window: BudgetWindow::Session,
            })
        });
    let was_parked = ledger.parked.is_some();
    match args.value.as_deref().map(str::trim) {
        Some("clear") => {
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
            ledger.raised_cap_usd = Some(current + delta.cap_usd);
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
    rimz::harness::budget::clear_resume_park(store.runtime_paths(), &agent.kind, &agent.agent_id);

    if was_parked && !args.no_continue {
        let text = rimz::config::MachineConfig::load_lenient()
            .resume
            .auto_continue_text
            .clone();
        if !text.trim().is_empty() {
            let message = MessageRecord::new(
                workspace.workspace_id,
                agent,
                text,
                true,
                DeliveryGate::Done,
            )
            .with_channel(rimz::harness::target::agent_channel(agent))
            .with_sender(MessageSender::Human);
            store
                .queue_message(&message, &workspace.session_name)
                .context("queueing budget continue prompt")?;
        }
    }
    render_budget(agent, Some(&ledger), launched)
}

fn render_budget(
    agent: &rimz::agents::AgentState,
    ledger: Option<&BudgetLedger>,
    launched: Option<BudgetSpec>,
) -> Result<()> {
    let mut kv = crate::cli::render::KeyVals::new();
    kv.push(
        "agent",
        crate::cli::render::cell(format!("@{}", agent.agent_id)),
    );
    kv.push(
        "spend",
        crate::cli::render::cell(
            total_cost_usd(agent)
                .map(|cost| format!("${cost:.2}"))
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash(),
    );
    let spec = ledger.map(|ledger| ledger.spec).or(launched);
    let cap = match ledger {
        Some(ledger) => ledger.effective_cap_usd(),
        None => launched.map(|spec| spec.cap_usd),
    };
    kv.push(
        "cap",
        crate::cli::render::cell(
            cap.map(|cap| format!("${cap:.2}"))
                .unwrap_or_else(|| "none".to_owned()),
        ),
    );
    kv.push(
        "window",
        crate::cli::render::cell(spec.map_or("-", |spec| spec.window.label())).dash(),
    );
    kv.push(
        "parked",
        crate::cli::render::cell(if ledger.is_some_and(|ledger| ledger.parked.is_some()) {
            "yes"
        } else {
            "no"
        }),
    );
    crate::cli::render::finish(kv.render(&mut crate::cli::render::out()))?;
    Ok(())
}
