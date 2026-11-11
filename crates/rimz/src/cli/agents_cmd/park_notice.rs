//! Detached helper for a child stopped on a provider limit: holds its run until the refill, or
//! fails the run with its typed cause and reclaims it as a timeout does.

use std::time::Duration;

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::harness::park_notice::{ParkAction, ParkNoticeRequest, limit_reading, park_action};

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
        |child, since| limit_reading(ctx.runtime(), child, now, since),
    );
    match action {
        None => Ok(()),
        Some(ParkAction::Hold { child, hold }) => {
            if !rimz::harness::run::claim_limit_hold(paths, &run.run_id, hold)? {
                return Ok(());
            }
            // A held child no longer holds back its siblings' answers.
            let Some((kind, launcher)) = child.launcher() else {
                return Ok(());
            };
            super::subagent_report::report_fleet_with_kind(
                &ctx.workspace,
                &ctx.store,
                launcher,
                Some(kind),
            )
            .context("reporting the held child's fleet")?;
            Ok(())
        }
        Some(ParkAction::Fail {
            activity, failure, ..
        }) => {
            let Some(record) =
                rimz::harness::run::fail_limit_park(paths, &run.run_id, activity, failure)?
            else {
                return Ok(());
            };
            super::run_timeout::reclaim_settled_run(&ctx, globals, &run, &record)
                .context("reclaiming limit-failed run pane")
        }
    }
}
