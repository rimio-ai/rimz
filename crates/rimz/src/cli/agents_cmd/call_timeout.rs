//! Hidden helper that stops a launch past its call's `timeout`: it claims the deadline once,
//! settles the open run timed out (or records a staying peer at rest timed out) so the launcher's
//! report names the timeout, and stops every live seat of that launch. A one-task run that ended
//! before the deadline is left as it is, a pane `--keep` holds included.

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
    // A launch already ended at its deadline is owed nothing, read from a fresh store projection
    // so a seat whose owner died since the last publish drops out and the report never waits on a
    // pane being seen; only the stop needs the room's live view.
    let rows = ctx.store.snapshot()?;
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
    // A team seat run alone, as `rimz agents <team.role> --stay false` runs it, has no team run
    // and times out as a solo run does.
    if leader.is_team_seat()
        && let Some(record) = rimz::harness::run::open_team_run(paths, leader)?
    {
        let board = rimz::harness::scratch::durable_board_path(&record.worktree_path);
        rimz::harness::run::report_team_timeout(
            &ctx.workspace,
            &ctx.store,
            &record,
            &board,
            &failure,
        )?;
        return stop(&ctx, globals, &snapshot, &seats);
    }
    let newest = rimz::launcher::stop::newest_run_for_agent(&ctx.store, leader)?;
    match &newest {
        Some(run) if !run.status.is_terminal() => {
            rimz::harness::run::time_out_run(&ctx.store, &run.run_id, &failure)?;
        }
        // A staying peer at rest has no open turn, so its stop gets a record of its own, which
        // reports where its launch's runs report.
        _ if call.stay => {
            let reader = leader
                .launcher()
                .and_then(|(kind, id)| rimz::address::launch_row(audit.agents.iter(), kind, id))
                .and_then(|launcher| launcher.name.as_deref());
            let report_to = newest
                .as_ref()
                .map_or(rimz::store::run::ReportTo::Launcher, |run| run.report_to);
            rimz::harness::run::time_out_idle_peer(paths, leader, reader, report_to, &failure)?;
        }
        // A one-task run that ended before the deadline has reported its own end, and a pane
        // `--keep` holds open past that end stays open.
        Some(_) => return Ok(()),
        None => return stop(&ctx, globals, &snapshot, &seats),
    }
    let stop_result = stop(&ctx, globals, &snapshot, &seats);
    if let Some((_, launcher)) = leader.launcher() {
        rimz::harness::orphan_sweep::spawn_digest_helper(
            ctx.store.runtime_paths(),
            launcher.clone(),
        );
    }
    stop_result
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
    let errors: Vec<String> = super::stop_resolved(ctx, globals, snapshot, seats)?
        .into_iter()
        .filter_map(|stop| stop.error())
        .collect();
    if errors.is_empty() {
        return Ok(());
    }
    anyhow::bail!("call timeout: could not stop a seat: {}", errors.join("; "))
}
