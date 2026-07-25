use std::io::Write;

use anyhow::{Result, bail};

use super::super::{Ctx, GlobalFlags, agents_cmd, render};
use rimz::agents::AgentState;
use rimz::harness::target::TeamCohort;

fn select<'a>(
    team: &str,
    worktree: Option<&str>,
    current_channel: Option<&str>,
    agents: &'a [AgentState],
) -> Result<TeamCohort<'a>> {
    let mut cohorts = rimz::harness::target::team_cohorts(agents)
        .into_iter()
        .filter(|cohort| cohort.team == team)
        .filter(|cohort| {
            worktree.is_none_or(|worktree| {
                cohort.channel == worktree
                    || cohort
                        .members
                        .iter()
                        .any(|agent| rimz::harness::target::agent_in_worktree(agent, worktree))
            })
        })
        .collect::<Vec<_>>();
    if cohorts.is_empty() {
        bail!("no live cohort for team `{team}`; resume one with `rimz teams resume {team}`");
    }
    if worktree.is_none()
        && let Some(index) = current_channel
            .and_then(|channel| cohorts.iter().position(|cohort| cohort.channel == channel))
    {
        return Ok(cohorts.swap_remove(index));
    }
    if cohorts.len() == 1 {
        return Ok(cohorts.remove(0));
    }
    let lanes = cohorts
        .iter()
        .map(|cohort| format!("#{}", cohort.channel))
        .collect::<Vec<_>>()
        .join(", ");
    bail!("team `{team}` has live cohorts in {lanes}; select one with `-w <worktree>`")
}

pub(super) fn stop(team: &str, worktree: Option<&str>, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let cohort = select(team, worktree, ctx.channel(), &snapshot.agents)?;
    let mut failed = false;
    let mut out = render::out();
    for agent in cohort.members.iter().copied() {
        let label = rimz::harness::target::agent_handle(agent, &cohort.members, true);
        match agents_cmd::stop_resolved(&ctx, globals, agent) {
            Ok(()) => writeln!(out, "stopped {label}")?,
            Err(err) => {
                failed = true;
                writeln!(out, "error {label}: {err:#}")?;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

pub(super) fn focus(team: &str, worktree: Option<&str>, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let cohort = select(team, worktree, ctx.channel(), &snapshot.agents)?;
    let leader = super::list::effective_teams(globals)?
        .0
        .get(team)
        .and_then(|team| team.leader.as_deref())
        .map(ToOwned::to_owned);
    let agent = cohort
        .members
        .iter()
        .copied()
        .find(|agent| agent.effective_status().is_actionable())
        .or_else(|| {
            leader.as_deref().and_then(|leader| {
                cohort
                    .members
                    .iter()
                    .copied()
                    .find(|agent| agent.role.as_deref() == Some(leader))
            })
        })
        .or_else(|| cohort.members.first().copied())
        .ok_or_else(|| anyhow::anyhow!("team `{team}` has an empty live cohort"))?;
    agents_cmd::focus_resolved(&ctx, agent)
}

pub(super) fn restart(team: &str, worktree: Option<&str>, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let mut cohort = select(team, worktree, ctx.channel(), &snapshot.agents)?;
    cohort
        .members
        .sort_by_key(|agent| agent.launch_ordinal.unwrap_or(u32::MAX));
    let mut failed = false;
    let mut out = render::out();
    for agent in cohort.members {
        let label = rimz::harness::target::agent_handle(agent, &[agent], true);
        match agents_cmd::restart_resolved(&ctx, agent, &[agent]) {
            Ok(message) => writeln!(out, "{message}")?,
            Err(err) => {
                failed = true;
                writeln!(out, "error {label}: {err:#}")?;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, team: &str, channel: &str) -> AgentState {
        let mut agent = rimz::testkit::agent_state("codex", id, jiff::Timestamp::UNIX_EPOCH);
        agent.team = Some(team.to_owned());
        agent.channel = Some(channel.to_owned());
        agent
    }

    #[test]
    fn selection_prefers_the_current_lane() {
        let agents = vec![
            agent("auth", "forge", "auth"),
            agent("docs", "forge", "docs"),
        ];

        let selected = select("forge", None, Some("docs"), &agents).expect("current cohort");

        assert_eq!(selected.channel, "docs");
    }

    #[test]
    fn selection_lists_ambiguous_lanes_and_the_fix() {
        let agents = vec![
            agent("auth", "forge", "auth"),
            agent("docs", "forge", "docs"),
        ];

        let error = select("forge", None, None, &agents).expect_err("ambiguous cohorts");

        assert!(error.to_string().contains("#auth, #docs"));
        assert!(error.to_string().contains("-w <worktree>"));
    }

    #[test]
    fn selection_points_to_resume_when_nothing_is_live() {
        let error = select("forge", None, None, &[]).expect_err("missing cohort");

        assert!(error.to_string().contains("rimz teams resume forge"));
    }
}
