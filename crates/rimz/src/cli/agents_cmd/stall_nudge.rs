//! Detached helper for a stalled team's once-per-visit nudge.

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::config::MachineConfig;
use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::stall_nudge::StallNudgeRequest;
use rimz::message::deliver::{DeliveryPolicy, deliver_one};

use super::Ctx;

pub(super) fn run(request: StallNudgeRequest) -> Result<()> {
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), None)
        .context("resolving stall nudge workspace")?;
    let machine = MachineConfig::load_lenient();
    let Some(message) = rimz::harness::stall_nudge::helper::queue(
        &request,
        &ctx.workspace,
        &ctx.store,
        &machine.time_zone(),
        || super::read_heartbeat_agents(&ctx),
    )
    .context("queueing team stall nudge")?
    else {
        return Ok(());
    };
    let outcome = match &message.pane_id {
        Some(pane) => deliver_one(
            &ctx.workspace,
            &ctx.store,
            &message.message_id,
            Some(pane.mux()),
            DeliveryPolicy::Boundary,
        )
        .context("delivering team stall nudge"),
        None => Ok(false),
    };
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::StallNudge {
            kind: message.kind.clone(),
            agent_id: message.agent_id.clone(),
            label: message.agent_name.as_ref().map(|name| format!("@{name}")),
            team: request.team,
            stage: request.stage,
            quiet_secs: message
                .enqueued_at
                .duration_since(request.stalled_since)
                .as_secs()
                .max(0) as u64,
            message_id: message.message_id.to_string(),
            delivered: matches!(outcome, Ok(true)),
            error: outcome.as_ref().err().map(ToString::to_string),
        },
    });
    outcome?;
    Ok(())
}
