//! Detached helper that tells a parent, once per park, that a child stopped on a provider limit.

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::harness::park_notice::{ParkNoticeRequest, text, unnoticed_park};
use rimz::message::deliver::{DeliveryPolicy, deliver_one};
use rimz::message::synthetic::SyntheticMessage;
use rimz::store::message::{DeliveryGate, HarnessNotice, MessageSender};

use super::Ctx;

pub(super) fn run(request: ParkNoticeRequest) -> Result<()> {
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
    let Some(park) = unnoticed_park(&run, &agents) else {
        return Ok(());
    };
    let activity = park.child.last_activity;
    if !rimz::harness::run::claim_park_notice(paths, &run.run_id, activity)? {
        return Ok(());
    }
    let message = SyntheticMessage {
        agent: park.parent,
        text: text(park.child, &run, Timestamp::now()),
        sender: MessageSender::Harness {
            notice: HarnessNotice::SubagentPaused,
        },
        gate: DeliveryGate::Done,
        pane_id: park.parent.pane.as_ref().map(|pane| pane.pane_id.clone()),
    }
    .record(&ctx.workspace);
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
