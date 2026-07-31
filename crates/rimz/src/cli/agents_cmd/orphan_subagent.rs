//! Hidden helper that records and repairs a producer-detected subagent orphan.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::harness::orphan_sweep::OrphanSubagentRequest;

use super::Ctx;

pub fn repair_orphan(request: OrphanSubagentRequest, globals: &super::GlobalFlags) -> Result<()> {
    let paths = rimz::StatePaths::for_workspace(request.workspace_id.clone())
        .context("preparing orphaned subagent store paths")?;
    let initial = rimz::harness::orphan_sweep::resolve(&paths, &request, Timestamp::now())
        .context("checking orphaned subagent")?;
    let Some(initial) = initial else {
        return Ok(());
    };
    let mux_hint = initial.child.pane.as_ref().map(|pane| pane.pane_id.mux());
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), mux_hint)?;
    let Some(orphan) =
        rimz::harness::orphan_sweep::resolve(ctx.store.paths(), &request, Timestamp::now())
            .context("rechecking orphaned subagent")?
    else {
        return Ok(());
    };

    let diag = rimz::diag::DiagSink::for_workspace(
        ctx.workspace.workspace_id.clone(),
        ctx.workspace.session_name.clone(),
        None,
    );
    let repair = (|| -> Result<()> {
        if let Some(run) = orphan.run.as_ref() {
            super::supervised::stop_supervised_run(&ctx.workspace, &ctx.store, globals, run)
                .context("reclaiming orphaned subagent run pane")
        } else {
            let pane = orphan
                .child
                .pane
                .as_ref()
                .context("orphaned subagent has no pane or run record")?;
            rimz::mux::backend_for(pane.pane_id.mux())
                .close_pane(&ctx.workspace.session_name, &pane.pane_id)
                .context("reclaiming orphaned subagent pane")
        }
    })();
    if let Err(err) = repair {
        diag.emit(rimz::diag::record::DiagEvent::SubagentOrphanRepairFailed {
            agent_kind: orphan.child.kind,
            agent_id: orphan.child.agent_id,
            parent_agent_id: request.parent_agent_id,
            orphaned_at_ms: orphan.orphaned_at.as_millisecond().max(0) as u64,
            error: format!("{err:#}"),
        });
        return Err(err);
    }

    let observation = rimz::agents::AgentLifecycleObservation::new(
        Some(orphan.child.agent_id.clone()),
        rimz::agents::LifecycleSignal::Ended,
    );
    let ended = rimz::EventEnvelope::agent_lifecycle(
        ctx.workspace.workspace_id.clone(),
        &ctx.workspace.session_name,
        orphan.child.kind.as_str(),
        "rimz.subagent-orphan-reaped",
        &observation,
    );
    ctx.store
        .append_event(&ended)
        .context("recording orphaned subagent end")?;
    diag.emit(rimz::diag::record::DiagEvent::SubagentOrphanReaped {
        agent_kind: orphan.child.kind,
        agent_id: orphan.child.agent_id,
        parent_agent_id: request.parent_agent_id,
        orphaned_at_ms: orphan.orphaned_at.as_millisecond().max(0) as u64,
    });
    Ok(())
}
