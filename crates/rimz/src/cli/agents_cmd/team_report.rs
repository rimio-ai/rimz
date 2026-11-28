//! The `TEAM_REPORT` an agent-launched team owes its launcher when its cohort dies before
//! `Done`; the flip into `Done` reports through `rimz::harness::team_stage::flip_cohort`.

use rimz::harness::run;
use rimz::harness::run::report::ReportErr;
use rimz::ids::AgentSessionId;
use rimz::workspace::ResolvedWorkspace;
use rimz::{RuntimeScope, Store};

pub(super) fn settle_ended_teams(
    workspace: &ResolvedWorkspace,
    store: &Store,
    launcher_id: &AgentSessionId,
) -> Result<(), ReportErr> {
    let projection = store.runtime_projection(RuntimeScope::Audit)?;
    let Some(launcher) =
        super::subagent_report::report_parent(&projection.agents, launcher_id, None)
            .filter(|launcher| launcher.ended_at.is_none())
    else {
        return Ok(());
    };
    // A parked run asks this on every strand poll: skip the runs read unless
    // this launcher launched a team.
    if !projection
        .agents
        .iter()
        .any(|agent| agent.is_team_seat() && rimz::harness::fleet::reports_to(agent, launcher))
    {
        return Ok(());
    }
    let runs = run::list(store.paths())?;
    for record in rimz::harness::fleet::ended_team_runs(&projection.agents, &runs, launcher) {
        let stage = rimz::harness::scratch::board_stage(&record.worktree_path);
        let board = rimz::harness::scratch::durable_board_path(&record.worktree_path);
        if let Err(error) = run::report_team_run(
            workspace,
            store,
            record,
            &board,
            stage.as_ref().map(|stage| stage.name.as_str()),
        ) {
            tracing::debug!(run_id = %record.run_id, %error, "could not settle an ended team run");
        }
    }
    Ok(())
}
