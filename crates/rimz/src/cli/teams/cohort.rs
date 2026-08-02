use std::collections::HashMap;
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
    let team_cohorts = rimz::harness::target::team_cohorts(agents)
        .into_iter()
        .filter(|cohort| cohort.team == team)
        .collect::<Vec<_>>();
    let mut cohorts = team_cohorts
        .iter()
        .filter(|cohort| {
            worktree.is_none_or(|worktree| {
                cohort.channel == worktree
                    || cohort
                        .members
                        .iter()
                        .any(|agent| rimz::harness::target::agent_in_worktree(agent, worktree))
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    if cohorts.is_empty() {
        if let Some(worktree) = worktree {
            let lanes = team_cohorts
                .iter()
                .map(|cohort| format!("#{}", cohort.channel))
                .collect::<Vec<_>>()
                .join(", ");
            if !lanes.is_empty() {
                bail!(
                    "team `{team}` has no live cohort matching `-w {worktree}`; live cohorts are in {lanes}"
                );
            }
            bail!("team `{team}` has no live cohort matching `-w {worktree}`");
        }
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
    if let Some(worktree) = worktree {
        bail!(
            "team `{team}` filter `-w {worktree}` matches live cohorts in {lanes}; select an exact lane or worktree"
        );
    }
    bail!("team `{team}` has live cohorts in {lanes}; select one with `-w <worktree>`")
}

pub(super) fn stop(team: &str, worktree: Option<&str>, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let cohort = select(team, worktree, ctx.channel(), &snapshot.agents)?;
    agents_cmd::stop_many(&ctx, globals, &snapshot, &cohort.members)
}

fn focus_member<'a>(
    cohort: &TeamCohort<'a>,
    leader: Option<&str>,
    attention_scores: &HashMap<&str, u32>,
) -> &'a AgentState {
    let score = |agent: &AgentState| {
        attention_scores
            .get(agent.agent_id.as_str())
            .copied()
            .unwrap_or_default()
    };
    let actionable = cohort
        .members
        .iter()
        .copied()
        .filter(|agent| agent.effective_status().is_actionable())
        .fold(None, |best, candidate| match best {
            Some(best) if score(best) >= score(candidate) => Some(best),
            _ => Some(candidate),
        });
    actionable
        .or_else(|| {
            leader.and_then(|leader| {
                cohort
                    .members
                    .iter()
                    .copied()
                    .find(|agent| agent.role.as_deref() == Some(leader))
            })
        })
        // `team_cohorts` only creates cohorts after inserting a live member.
        .unwrap_or_else(|| {
            cohort
                .members
                .first()
                .copied()
                .expect("team_cohorts emits non-empty cohorts")
        })
}

pub(super) fn focus(
    team: &str,
    worktree: Option<&str>,
    leader: Option<&str>,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let cohort = select(team, worktree, ctx.channel(), &snapshot.agents)?;
    let attention_scores = snapshot
        .rows()
        .map(|row| (row.id.as_str(), row.attention_score))
        .collect::<HashMap<_, _>>();
    agents_cmd::focus_resolved(&ctx, focus_member(&cohort, leader, &attention_scores))
}

pub(super) fn restart(team: &str, worktree: Option<&str>, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let mut cohort = select(team, worktree, ctx.channel(), &snapshot.agents)?;
    let peers = rimz::harness::target::addressable_agents(&snapshot);
    cohort
        .members
        .sort_by_key(|agent| agent.launch_ordinal.unwrap_or(u32::MAX));
    let mut failed = false;
    let mut out = render::out();
    for agent in cohort.members.iter().copied() {
        let label = rimz::harness::target::agent_handle(agent, &peers, true);
        match agents_cmd::restart_resolved(&ctx, agent, &peers) {
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
    fn selection_names_an_unmatched_worktree_and_live_lanes() {
        let agents = vec![
            agent("auth", "forge", "auth"),
            agent("docs", "forge", "docs"),
        ];

        let error = select("forge", Some("typo"), None, &agents).expect_err("missing cohort");

        assert!(error.to_string().contains("-w typo"));
        assert!(error.to_string().contains("#auth, #docs"));
    }

    #[test]
    fn selection_points_to_resume_when_nothing_is_live() {
        let error = select("forge", None, None, &[]).expect_err("missing cohort");

        assert!(error.to_string().contains("rimz teams resume forge"));
    }

    #[test]
    fn focus_picks_the_actionable_member_with_the_highest_attention_score() {
        let mut roster_first = agent("first", "forge", "docs");
        roster_first.status = rimz::agents::AgentStatus::Waiting;
        let mut urgent = agent("urgent", "forge", "docs");
        urgent.status = rimz::agents::AgentStatus::Failed;
        let agents = vec![roster_first, urgent];
        let cohort = rimz::harness::target::team_cohorts(&agents)
            .into_iter()
            .next()
            .expect("cohort");
        let scores = HashMap::from([("first", 10), ("urgent", 50)]);

        assert_eq!(
            focus_member(&cohort, None, &scores).agent_id.as_str(),
            "urgent"
        );
    }

    #[test]
    fn room_peers_keep_teamless_role_collisions_out_of_rendered_handles() {
        let mut team_member = agent("team-reviewer", "forge", "feat-x");
        team_member.role = Some("reviewer".to_owned());
        team_member.kind_ordinal = Some(1);
        let mut ad_hoc = agent("ad-hoc-reviewer", "", "feat-x");
        ad_hoc.team = None;
        ad_hoc.role = Some("reviewer".to_owned());
        ad_hoc.kind_ordinal = Some(2);
        let snapshot = rimz::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            vec![team_member, ad_hoc],
            jiff::Timestamp::UNIX_EPOCH,
        );
        let peers = rimz::harness::target::addressable_agents(&snapshot);

        assert_eq!(
            rimz::harness::target::agent_handle(peers[0], &peers, true),
            "@codex-1#feat-x"
        );
    }
}
