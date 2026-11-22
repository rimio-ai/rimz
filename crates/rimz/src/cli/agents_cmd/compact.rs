//! Operator-triggered context compaction through the native command queue, with the continue
//! a busy agent is owed afterward.

use std::io::Write;

use anyhow::Result;
use rimz::address::{addressable_agents, agent_handle};
use rimz::message::compact::{ReceiptOutcome, compact_agent as compact};

use crate::cli::{GlobalFlags, ctx::Ctx, render, send};

pub(super) fn compact_agent(
    reference: String,
    instruction: Option<String>,
    no_continue: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.resolution_snapshot_with_context()?;
    let agent = crate::cli::resolve_agent_one(
        &ctx.store,
        &snapshot,
        &reference,
        None,
        &ctx.address_context(),
    )?;
    let handle = agent_handle(agent, &addressable_agents(&snapshot), false);
    let caller = send::resolve_caller(&ctx.store)?;
    let receipt = compact(
        &ctx.workspace,
        &ctx.store,
        agent,
        &handle,
        instruction.as_deref(),
        no_continue,
        send::sender_for(caller.as_ref(), ctx.channel(), false),
    )?;
    let message_id = receipt.message_id;
    match receipt.outcome {
        ReceiptOutcome::Sent => writeln!(render::out(), "compacting {handle} ({message_id})")?,
        ReceiptOutcome::Queued { continues } => writeln!(
            render::out(),
            "queued compaction for {handle} ({message_id}) — {handle} is {}; delivers at its next turn boundary, {}",
            agent.effective_status(jiff::Timestamp::now()).as_str(),
            if continues {
                "then continues it"
            } else {
                "with no continue after"
            },
        )?,
        ReceiptOutcome::Joined { continues } => writeln!(
            render::out(),
            "compaction {message_id} is already queued for {handle}; delivers at its next turn boundary, {}",
            if continues {
                "then continues it"
            } else {
                "with no continue after"
            },
        )?,
    }
    Ok(())
}
