//! Operator-triggered context compaction through the native command queue.

use std::io::Write;

use anyhow::{Context, Result, bail};
use rimz::agents::definition::CompactInstruction;
use rimz::harness::target::{addressable_agents, agent_handle};
use rimz::ids::MessageId;
use rimz::message::compact::{CompactOutcome, CompactRequest, send_compact};

use crate::cli::{GlobalFlags, ctx::Ctx, render, send};

pub(super) fn compact_agent(
    reference: String,
    instruction: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.resolution_snapshot_with_context()?;
    let agent = crate::cli::resolve_agent_one(&snapshot, &reference, None, ctx.channel())?;
    let handle = agent_handle(agent, &addressable_agents(&snapshot), false);
    let pane_id = agent
        .pane
        .as_ref()
        .context("agent has no bound pane; nothing to compact")?
        .pane_id
        .clone();
    let spec = rimz::agents::spec_by_kind(agent.kind.as_str())
        .with_context(|| format!("{} has no native compaction command", agent.kind))?;
    let compact = spec
        .launch
        .compact_command
        .with_context(|| format!("{} has no native compaction command", agent.kind))?;
    if !spec.lifecycle_hooks.turn_started.is_native() {
        bail!(
            "{} reports no durable turn starts, so RimZ cannot guarantee a compaction never follows a compaction; compact it in its own pane",
            agent.kind
        );
    }
    if instruction.is_some() && compact.instruction == CompactInstruction::Unsupported {
        bail!(
            "{} does not accept a compaction instruction; it receives `{}` bare — rerun without the instruction",
            agent.kind,
            compact.command,
        );
    }
    let config = crate::cli::machine_config();
    let command = spec
        .launch
        .compact_command(
            instruction
                .as_deref()
                .unwrap_or(config.harness.compact_instruction()),
        )
        .context("adapter has no native compaction command")?;
    let caller = send::resolve_caller(&ctx.store)?;
    let message_id = MessageId::new();
    let outcome = send_compact(
        &ctx.workspace,
        &ctx.store,
        CompactRequest {
            message_id: message_id.clone(),
            agent,
            pane_id,
            command,
            sender: send::sender_for(caller.as_ref(), ctx.channel(), false),
            automated: false,
        },
    )
    .map_err(|err| anyhow::anyhow!("{handle}: {err}"))?;
    match outcome {
        CompactOutcome::Sent => writeln!(render::out(), "compacting {handle} ({message_id})")?,
        CompactOutcome::Queued => writeln!(
            render::out(),
            "queued compaction for {handle} ({message_id}) — {handle} is {}; delivers at its next turn boundary",
            agent.effective_status().as_str(),
        )?,
    }
    Ok(())
}
