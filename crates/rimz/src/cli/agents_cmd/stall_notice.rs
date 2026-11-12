//! Detached helper for the once-per-run silent-child notice and its assist.

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::stall_notice::{StallNoticeRequest, text, unnoticed_stall};
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageSender};

use super::Ctx;

pub(super) fn run(request: StallNoticeRequest) -> Result<()> {
    let ctx = Ctx::for_workspace(request.workspace_id, None)
        .context("resolving stall notice workspace")?;
    let (agents, now) = super::read_heartbeat_agents(&ctx).context("reading agent history")?;
    let paths = ctx.store.paths();
    let run =
        rimz::harness::run::load(paths, &request.run_id).context("loading silent child run")?;
    let Some(stall) = unnoticed_stall(&run, &agents, now, request.stalled_after_secs) else {
        return Ok(());
    };
    if !rimz::harness::run::claim_stall_notice(paths, &run.run_id, now)? {
        return Ok(());
    }
    let message = SyntheticMessage {
        agent: stall.parent,
        text: text(&stall, &run, now),
        sender: MessageSender::Harness {
            notice: HarnessNotice::SubagentStalled,
        },
        gate: DeliveryGate::Done,
        premises: rimz::store::message::Premise::NONE,
    }
    .record(&ctx.workspace)
    .with_about_run(Some(run.run_id.clone()));
    if let Err(err) = ctx
        .store
        .queue_message(&message, &ctx.workspace.session_name)
    {
        let _ = rimz::harness::run::release_stall_notice(paths, &run.run_id, now);
        return Err(err).context("queueing stall notice");
    }
    let outcome = match &stall.parent.pane {
        Some(pane) => deliver_one(
            &ctx.workspace,
            &ctx.store,
            &message.message_id,
            Some(pane.pane_id.mux()),
            DeliveryPolicy::Boundary,
        )
        .context("delivering stall notice"),
        None => Ok(false),
    };
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::StallNotice {
            kind: stall.child.kind.clone(),
            agent_id: stall.child.agent_id.clone(),
            label: stall.child.name.as_ref().map(|name| format!("@{name}")),
            parent: format!(
                "@{}",
                stall
                    .parent
                    .name
                    .clone()
                    .unwrap_or_else(|| stall.parent.agent_id.to_string())
            ),
            silent_secs: stall.silent_secs,
            message_id: message.message_id.to_string(),
            delivered: matches!(outcome, Ok(true)),
            error: outcome.as_ref().err().map(ToString::to_string),
        },
    });
    outcome?;
    Ok(())
}
