//! Hidden helper that stops a launch past its call's `timeout`: it claims the deadline once,
//! settles the open run timed out so the launcher's report names the timeout, and stops every
//! live seat of that launch.

use anyhow::{Context, Result};
use jiff::Timestamp;

use rimz::agents::AgentState;
use rimz::harness::call_timeout::CallTimeoutRequest;

use super::Ctx;

pub fn call_timeout(request: CallTimeoutRequest, globals: &super::GlobalFlags) -> Result<()> {
    let ctx = Ctx::for_workspace(request.workspace_id.clone(), globals.mux)?;
    let paths = ctx.store.paths();
    let Some(call) =
        rimz::harness::call::claim_timeout(paths, &request.launch_id, Timestamp::now())
            .context("claiming the call's timeout")?
    else {
        return Ok(());
    };
    let failure = rimz::harness::call::timeout_failure(&call);
    // A launch already ended at its deadline is owed nothing, read from the store's rows so the
    // report never waits on a pane being seen; only the stop needs the room's live view.
    let rows = ctx.store.snapshot_cached()?;
    if rimz::harness::call::live_seats(&call, rows.agents.iter()).is_empty() {
        return Ok(());
    }
    let snapshot = ctx.alive_snapshot()?;
    let seats: Vec<&AgentState> = rimz::harness::call::live_seats(&call, snapshot.agents.iter());
    // The leader is read from the audit rows, so a team whose leader has ended while a worker
    // runs on still gets its timeout report.
    let audit = ctx.store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let leader = call.seats.first().and_then(|seat| {
        rimz::address::launch_row(audit.agents.iter(), &seat.kind, &seat.launch_id)
            .filter(|row| row.launch_key() == &seat.launch_id)
    });
    let Some(leader) = leader else {
        return stop(&ctx, globals, &snapshot, &seats);
    };
    if leader.is_team_seat() {
        if let Some(record) = rimz::harness::run::open_team_run(paths, leader)? {
            let board = rimz::harness::scratch::durable_board_path(&record.worktree_path);
            rimz::harness::run::report_team_timeout(
                &ctx.workspace,
                &ctx.store,
                &record,
                &board,
                &failure,
            )?;
        }
        return stop(&ctx, globals, &snapshot, &seats);
    }
    let open = rimz::launcher::stop::newest_run_for_agent(&ctx.store, leader)?
        .filter(|run| !run.status.is_terminal());
    match open {
        Some(run) => {
            rimz::harness::run::time_out_run(&ctx.store, &run.run_id, &failure)?;
        }
        // A staying peer at rest has no open turn, so its stop gets a record of its own.
        None if call.stay => {
            let reader = leader
                .launcher()
                .and_then(|(kind, id)| rimz::address::launch_row(audit.agents.iter(), kind, id))
                .and_then(|launcher| launcher.name.as_deref());
            rimz::harness::run::time_out_idle_peer(paths, leader, reader, &failure)?;
        }
        // A one-task run that completed after the claim has reported its own end.
        None => return stop(&ctx, globals, &snapshot, &seats),
    }
    stop(&ctx, globals, &snapshot, &seats)?;
    if let Some((_, launcher)) = leader.launcher() {
        rimz::harness::orphan_sweep::spawn_digest_helper(
            ctx.store.runtime_paths(),
            launcher.clone(),
        );
    }
    Ok(())
}

fn stop(
    ctx: &Ctx,
    globals: &super::GlobalFlags,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    seats: &[&AgentState],
) -> Result<()> {
    if seats.is_empty() {
        return Ok(());
    }
    for stop in super::stop_resolved(ctx, globals, snapshot, seats)? {
        if let Some(error) = stop.error() {
            tracing::warn!(%error, "call timeout: could not stop a seat");
        }
    }
    Ok(())
}
