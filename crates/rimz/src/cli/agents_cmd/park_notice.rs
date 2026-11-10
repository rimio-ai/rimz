//! Detached helper for a child stopped on a provider limit: holds its run until the reset and
//! queues the parent's one notice for the park, or fails the run and reclaims it as a timeout does.

use std::time::Duration;

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::harness::park_notice::{
    ParkAction, ParkNoticeRequest, failure_reason, park_action, reset_reading, text,
};
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageSender};

use super::Ctx;

pub(super) fn run(request: ParkNoticeRequest, globals: &super::GlobalFlags) -> Result<()> {
    let ctx = Ctx::for_workspace(request.workspace_id, None)
        .context("resolving park notice workspace")?;
    let mut audit = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading agent history")?;
    rimz::store::agent_context::attach_rest_certificates(ctx.runtime(), audit.agents.iter_mut());
    // Fold the per-tool heartbeat as the producer's enrich does, so both sides
    // name the park by the same `last_activity` and the claim satisfies the detector.
    let heartbeats = rimz::agent_activity::read_for_keys(
        ctx.runtime(),
        audit
            .agents
            .iter()
            .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
    );
    let agents = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        ctx.workspace.workspace_id.clone(),
        audit.agents,
        Timestamp::now(),
    )
    .with_agent_activity(&heartbeats)
    .agents;
    let paths = ctx.store.paths();
    let run = rimz::harness::run::load(paths, &request.run_id).context("loading parked run")?;
    let now = Timestamp::now();
    let action = park_action(
        &run,
        &agents,
        now,
        Duration::from_secs(request.horizon_secs),
        request.continuation,
        |child| reset_reading(ctx.runtime(), child, now),
    );
    let (child, parent, hold) = match action {
        None => return Ok(()),
        Some(ParkAction::Fail {
            child,
            activity,
            cause,
        }) => {
            let reason = failure_reason(child, cause, now);
            let Some(record) =
                rimz::harness::run::fail_limit_park(paths, &run.run_id, activity, &reason)?
            else {
                return Ok(());
            };
            return super::run_timeout::reclaim_settled_run(&ctx, globals, &run, &record)
                .context("reclaiming limit-failed run pane");
        }
        Some(ParkAction::Notice {
            child,
            parent,
            hold,
        }) => (child, parent, hold),
    };
    let activity = hold.activity;
    if !rimz::harness::run::claim_park_notice(paths, &run.run_id, hold)? {
        return Ok(());
    }
    let message = SyntheticMessage {
        agent: parent,
        text: text(child, &hold, request.continuation, Timestamp::now()),
        sender: MessageSender::Harness {
            notice: HarnessNotice::SubagentPaused,
        },
        gate: DeliveryGate::Done,
        pane_id: parent.pane.as_ref().map(|pane| pane.pane_id.clone()),
    }
    .record(&ctx.workspace)
    .with_about_run(Some(run.run_id.clone()));
    if let Err(err) = ctx
        .store
        .queue_message(&message, &ctx.workspace.session_name)
    {
        let _ = rimz::harness::run::release_park_notice(paths, &run.run_id, activity);
        return Err(err).context("queueing park notice");
    }
    // A closed gate leaves the record queued for the delivery sweep.
    if let Some(pane_id) = &message.pane_id {
        deliver_one(
            &ctx.workspace,
            &ctx.store,
            &message.message_id,
            Some(pane_id.mux()),
            DeliveryPolicy::Boundary,
        )
        .context("delivering park notice")?;
    }
    Ok(())
}
