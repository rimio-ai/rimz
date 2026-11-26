//! Operator- or self-triggered context compaction through the native command queue, with the
//! continue a busy agent's own request owes it afterward.

use std::io::Write;

use anyhow::Result;
use rimz::address::{addressable_agents, named_handle};
use rimz::message::compact::{CompactAsk, ReceiptOutcome, SoftBecause, compact_agent as compact};
use rimz::store::message::MessageStatus;

use crate::cli::{GlobalFlags, ctx::Ctx, render, send};

pub(super) fn compact_agent(
    reference: String,
    instruction: Option<String>,
    no_continue: bool,
    hard: bool,
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
    let handle = named_handle(agent, &addressable_agents(&snapshot), false);
    let caller = send::resolve_caller(&ctx.store)?;
    let self_request = caller
        .as_ref()
        .and_then(|caller| {
            rimz::harness::ancestry::resolve_launch_caller(&snapshot.agents, caller).ok()
        })
        .is_some_and(|caller| caller.launch_key() == agent.launch_key());
    let receipt = compact(
        &ctx.workspace,
        &ctx.store,
        agent,
        &handle,
        instruction.as_deref(),
        CompactAsk {
            own: self_request,
            no_continue,
            hard,
        },
        send::sender_for(caller.as_ref(), ctx.channel(), false),
    )?;
    let message_id = receipt.message_id;
    match receipt.outcome {
        ReceiptOutcome::Sent { continues: false } => {
            writeln!(render::out(), "compacting {handle} ({message_id})")?;
        }
        // A kind whose command takes the continue as its next prompt sent it on the same line.
        ReceiptOutcome::Sent { continues: true }
            if ctx.store.list_messages()?.iter().any(|record| {
                record.follows_command.as_ref() == Some(&message_id)
                    && record.status == MessageStatus::Sent
            }) =>
        {
            writeln!(
                render::out(),
                "compacting {handle} now ({message_id}) with its continue on the same line"
            )?;
        }
        ReceiptOutcome::Sent { continues: true } => writeln!(
            render::out(),
            "compacting {handle} now ({message_id}), then continues it if nothing else is waiting"
        )?,
        ReceiptOutcome::Queued { continues } => writeln!(
            render::out(),
            "queued compaction for {handle} ({message_id}) — {handle} is {}; delivers at its next turn boundary, {}",
            agent.effective_status(jiff::Timestamp::now()).as_str(),
            if continues {
                "then continues it if nothing else is waiting"
            } else {
                "with no continue after"
            },
        )?,
        ReceiptOutcome::Joined { continues } => writeln!(
            render::out(),
            "compaction {message_id} is already queued for {handle}; delivers at its next turn boundary, {}",
            if continues {
                "then continues it if nothing else is waiting"
            } else {
                "with no continue after"
            },
        )?,
    }
    if let Some(because) = receipt.soft_because {
        writeln!(
            render::out(),
            "--hard did not end {handle}'s turn: {}",
            match because {
                SoftBecause::OwnRequest => "an agent's own compaction waits for its turn to end",
                SoftBecause::AwaitingAnswer => "it is waiting on an answer",
                SoftBecause::NoContinueText =>
                    "resume.auto_continue_text is empty, and ending a turn for a compaction owes a continue",
                SoftBecause::PremisedCommand =>
                    "the compaction already queued for it can be withdrawn once its turn ends",
            }
        )?;
    }
    Ok(())
}
