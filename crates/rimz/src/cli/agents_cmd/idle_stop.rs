//! `rimz agents idle-stop` — the hidden helper the sidebar producer spawns to
//! stop an agent whose `stop --when-idle` request is due.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::harness::assist_log::{Assist, AssistRecord};
use rimz::harness::idle_stop::{IdleStopHelperRequest, Verdict, decide};
use rimz::store::snapshot::find_agent;

use super::{Ctx, GlobalFlags, StopTracker, stop_resolved};

pub fn run_idle_stop(request: IdleStopHelperRequest, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), Some(request.pane_id.mux()))?;
    let snapshot = ctx
        .published_snapshot()
        .context("reading idle-stop snapshot")?;
    let Some(agent) = find_agent(&snapshot.agents, &request.kind, &request.agent_id) else {
        return Ok(());
    };
    let Some(stop) = rimz::store::idle_stop::read(ctx.store.paths())
        .into_iter()
        .find(|pending| pending.kind == request.kind && pending.agent_id == request.agent_id)
        .map(|pending| pending.stop)
    else {
        return Ok(());
    };
    if snapshot.live_agent_pane(&request.kind, &request.agent_id) != Some(request.pane_id) {
        return Ok(());
    }
    let verdict = decide(&ctx.store, agent, &stop, Timestamp::now())
        .context("deciding whether the idle stop is due")?;
    let Verdict::Stop { idle_secs } = verdict else {
        tracing::debug!(agent = %request.label, ?verdict, "idle stop held");
        return Ok(());
    };
    // A stop that goes through retires the session, and with it the request;
    // a failed one leaves the request armed for the producer's next ask.
    let outcome = stop_resolved(&ctx, globals, &snapshot, agent, &mut StopTracker::default());
    rimz::harness::assist_log::append(&AssistRecord {
        at: Timestamp::now(),
        assist: Assist::IdleStop {
            kind: request.kind,
            agent_id: request.agent_id,
            label: request.label,
            idle_secs,
            idle_after_secs: stop.after_secs,
            requested_by: stop.requested_by,
            stopped: outcome.is_ok(),
            error: outcome.as_ref().err().map(|err| format!("{err:#}")),
        },
    });
    outcome.context("stopping the idle agent")?;
    Ok(())
}
