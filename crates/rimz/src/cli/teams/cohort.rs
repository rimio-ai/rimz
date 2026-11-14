use std::collections::HashMap;
use std::io::Write;

use anyhow::{Result, bail};

use super::super::{Ctx, GlobalFlags, agents_cmd, render};
use rimz::address::TeamCohort;
use rimz::agents::AgentState;

/// What a drive verb names: one team, optionally in one lane, or a bare
/// `#lane` that selects whichever team's cohort is live there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Target<'a> {
    Team {
        team: &'a str,
        lane: Option<&'a str>,
    },
    Lane(&'a str),
}

/// Resolve `target` to one live cohort, and load the team catalog that
/// defines it. A named team was checked by the caller before the room opened;
/// a lane target learns its team only here, so it is checked now.
fn resolve<'a>(
    target: Target<'_>,
    teams: Option<rimz::config::TeamsConfig>,
    current_channel: Option<&rimz::ids::LaneName>,
    agents: &'a [AgentState],
    globals: &GlobalFlags,
) -> Result<(TeamCohort<'a>, rimz::config::TeamsConfig)> {
    let cohort = select(target, current_channel, agents)?;
    let teams = match teams {
        Some(teams) => teams,
        None => super::ensure_defined(cohort.team(), globals)?,
    };
    Ok((cohort, teams))
}

pub(super) fn select<'a>(
    target: Target<'_>,
    current_channel: Option<&rimz::ids::LaneName>,
    agents: &'a [AgentState],
) -> Result<TeamCohort<'a>> {
    let (team, worktree) = match target {
        Target::Team { team, lane } => (team, lane),
        Target::Lane(lane) => return select_in_lane(lane, agents),
    };
    let team_cohorts = rimz::address::team_cohorts(agents)
        .into_iter()
        .filter(|cohort| cohort.team() == team)
        .collect::<Vec<_>>();
    let mut cohorts = team_cohorts
        .iter()
        .filter(|cohort| worktree.is_none_or(|worktree| matches_worktree(cohort, worktree)))
        .cloned()
        .collect::<Vec<_>>();
    if cohorts.is_empty() {
        if let Some(worktree) = worktree {
            let lanes = lane_list(&team_cohorts, false);
            if !lanes.is_empty() {
                bail!("no live cohort matching `{team}#{worktree}`; live cohorts are in {lanes}");
            }
            bail!("no live cohort matching `{team}#{worktree}`");
        }
        bail!("no live cohort for team `{team}`; resume one with `rimz teams resume {team}`");
    }
    if worktree.is_none()
        && let Some(index) = current_channel.and_then(|channel| {
            cohorts
                .iter()
                .position(|cohort| cohort.instance.lane() == Some(channel))
        })
    {
        return Ok(cohorts.swap_remove(index));
    }
    if cohorts.len() == 1 {
        return Ok(cohorts.remove(0));
    }
    let lanes = lane_list(&cohorts, false);
    if let Some(worktree) = worktree {
        bail!(
            "`{team}#{worktree}` matches live cohorts in {lanes}; select an exact lane with `{team}#<lane>`"
        );
    }
    bail!("team `{team}` has live cohorts in {lanes}; select one with `{team}#<lane>`")
}

/// The one team cohort live in `lane`, whatever its team.
fn select_in_lane<'a>(lane: &str, agents: &'a [AgentState]) -> Result<TeamCohort<'a>> {
    let all = rimz::address::team_cohorts(agents);
    let mut cohorts = all
        .iter()
        .filter(|cohort| matches_worktree(cohort, lane))
        .cloned()
        .collect::<Vec<_>>();
    match cohorts.len() {
        1 => Ok(cohorts.remove(0)),
        0 if all.is_empty() => bail!("no live team cohort in #{lane}"),
        0 => bail!(
            "no live team cohort in #{lane}; live cohorts: {}",
            lane_list(&all, true)
        ),
        _ => bail!(
            "#{lane} holds live cohorts of several teams: {}; select one with `<team>#{lane}`",
            lane_list(&cohorts, true)
        ),
    }
}

/// Cohorts as a reader types them back: `#lane`, or `team#lane` with
/// `with_team`. A cohort whose members carry no lane is spelled `#external`,
/// which `matches_worktree` selects, and says why.
fn lane_list(cohorts: &[TeamCohort<'_>], with_team: bool) -> String {
    cohorts
        .iter()
        .map(|cohort| {
            let team = if with_team { cohort.team() } else { "" };
            let note = if cohort.instance.lane().is_none() {
                " (members with no recorded lane)"
            } else {
                ""
            };
            format!("{team}#{}{note}", cohort.channel())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `worktree` (the text after `#`, or `-w`) selects `cohort`. The
/// lane-less cohort prints as `team#external`, so that text selects it back.
pub(super) fn matches_worktree(cohort: &TeamCohort<'_>, worktree: &str) -> bool {
    if cohort.instance.lane().is_none() && cohort.instance.lane_text() == worktree {
        return true;
    }
    let lane = rimz::address::LaneSelector::parse(Some(worktree));
    lane.name()
        .is_some_and(|name| cohort.instance.lane() == Some(name))
        || cohort
            .members
            .iter()
            .any(|agent| rimz::address::agent_in_lane(agent, &lane))
}

pub(super) fn stop(
    target: Target<'_>,
    teams: Option<rimz::config::TeamsConfig>,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let (cohort, _) = resolve(target, teams, ctx.channel(), &snapshot.agents, globals)?;
    let peers = rimz::address::addressable_agents(&snapshot);
    let stops = agents_cmd::stop_resolved(&ctx, globals, &snapshot, &cohort.members)?;
    let mut out = render::out();
    let members = cohort.members.len();
    let plural = if members == 1 { "" } else { "s" };
    let stopped = stops.iter().filter(|stop| stop.root_stopped()).count();
    if stops.iter().all(|stop| stop.error().is_none()) {
        writeln!(
            out,
            "stopped {} ({members} member{plural})",
            cohort.instance
        )?;
    } else {
        writeln!(
            out,
            "{}: {stopped} of {members} member{plural} stopped",
            cohort.instance
        )?;
    }
    let mut failed = false;
    for (agent, stop) in cohort.members.iter().copied().zip(stops) {
        let label = rimz::address::agent_handle(agent, &peers, true);
        failed |= stop.report(&mut out, &label)?;
    }
    if failed {
        std::process::exit(1);
    }
    if let Some(checkout) = cohort
        .members
        .first()
        .and_then(|agent| agent.worktree_path.as_deref())
        && super::board_context::in_worktree(
            &cohort,
            &rimz::utils::path::normalize_path_lexical(std::path::Path::new(checkout)),
        )
        && let Some(stage) = rimz::harness::scratch::board_stage(std::path::Path::new(checkout))
        && stage.name != rimz::config::DONE_STAGE
    {
        let target = cohort.instance.to_string();
        let command = super::super::usage::shell_command(["rimz", "teams", "resume", &target]);
        let resume = format!(" Resume: {command} ·");
        let release = super::super::usage::shell_command([
            "rimz", "teams", "flip", &target, "Done", "<note>",
        ]);
        let stage = stage.name;
        writeln!(
            out,
            "board at {stage}; the checkout stays held.{resume} release: {release}"
        )?;
    }
    Ok(())
}

fn focus_member<'a>(
    cohort: &TeamCohort<'a>,
    leader: Option<&str>,
    attention_scores: &HashMap<&str, u32>,
    now: jiff::Timestamp,
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
        .filter(|agent| agent.effective_status(now).is_actionable())
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
    target: Target<'_>,
    teams: Option<rimz::config::TeamsConfig>,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let (cohort, teams) = resolve(target, teams, ctx.channel(), &snapshot.agents, globals)?;
    let leader = teams
        .0
        .get(cohort.team())
        .and_then(|team| team.leader.as_deref());
    let attention_scores = snapshot
        .rows()
        .map(|row| (row.id.as_str(), row.attention_score))
        .collect::<HashMap<_, _>>();
    agents_cmd::focus_resolved(
        &ctx,
        focus_member(&cohort, leader, &attention_scores, snapshot.now),
    )
}

pub(super) fn restart(
    target: Target<'_>,
    teams: Option<rimz::config::TeamsConfig>,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.alive_snapshot()?;
    let focus = rimz::harness::ancestry::LaunchFocus::resolve(
        false,
        rimz::harness::ancestry::resolve_caller(&snapshot.agents).as_ref(),
    );
    let (mut cohort, _) = resolve(target, teams, ctx.channel(), &snapshot.agents, globals)?;
    let peers = rimz::address::addressable_agents(&snapshot);
    cohort
        .members
        .sort_by_key(|agent| agent.launch_ordinal.unwrap_or(u32::MAX));
    let mut failed = false;
    let mut out = render::out();
    writeln!(
        out,
        "restarted {} ({} member{})",
        cohort.instance,
        cohort.members.len(),
        if cohort.members.len() == 1 { "" } else { "s" }
    )?;
    for agent in cohort.members.iter().copied() {
        let label = rimz::address::agent_handle(agent, &peers, true);
        match agents_cmd::restart_resolved(
            &ctx,
            agent,
            &peers,
            &snapshot.agents,
            focus,
            Some(rimz::launcher::trust::Unattended::Team),
            || Some(rimz::launcher::trust::Unattended::Team),
            None,
        ) {
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
        rimz::testkit::set_lane(&mut agent, channel);
        agent
    }

    /// A seat whose row carries no lane, as one recorded before lanes were
    /// typed reads now.
    fn laneless(id: &str, team: &str) -> AgentState {
        let mut agent = rimz::testkit::agent_state("codex", id, jiff::Timestamp::UNIX_EPOCH);
        agent.team = Some(team.to_owned());
        agent.lane = None;
        agent.channel = None;
        agent
    }

    fn team<'a>(team: &'a str, lane: Option<&'a str>) -> Target<'a> {
        Target::Team { team, lane }
    }

    #[test]
    fn a_laneless_cohort_is_named_external_and_selected_by_that_name() {
        let agents = vec![laneless("coder", "recon"), agent("docs", "recon", "docs")];

        let error = select(team("recon", Some("agent-swap")), None, &agents)
            .expect_err("no cohort in the lane");
        assert_eq!(
            error.to_string(),
            "no live cohort matching `recon#agent-swap`; live cohorts are in \
             #external (members with no recorded lane), #docs"
        );

        let selected =
            select(team("recon", Some("external")), None, &agents).expect("lane-less cohort");
        assert_eq!(selected.members[0].agent_id.as_str(), "coder");
        let selected = select(Target::Lane("external"), None, &agents).expect("lane-less cohort");
        assert_eq!(selected.members[0].agent_id.as_str(), "coder");
    }

    #[test]
    fn a_lane_target_selects_the_one_team_live_there() {
        let agents = vec![
            agent("forge-docs", "forge", "docs"),
            agent("recon-swap", "recon", "agent-swap"),
        ];

        let selected = select(Target::Lane("agent-swap"), None, &agents).expect("one cohort");

        assert_eq!(selected.team(), "recon");
        assert_eq!(selected.members[0].agent_id.as_str(), "recon-swap");
    }

    #[test]
    fn a_lane_target_ignores_the_current_lane() {
        let agents = vec![
            agent("forge-docs", "forge", "docs"),
            agent("recon-swap", "recon", "agent-swap"),
        ];
        let docs = rimz::ids::LaneName::dashed("docs");

        let selected = select(Target::Lane("agent-swap"), Some(&docs), &agents).expect("cohort");

        assert_eq!(selected.team(), "recon");
    }

    #[test]
    fn a_lane_target_refuses_several_teams_and_names_each() {
        let agents = vec![
            agent("forge-swap", "forge", "agent-swap"),
            agent("recon-swap", "recon", "agent-swap"),
        ];

        let error = select(Target::Lane("agent-swap"), None, &agents).expect_err("ambiguous");

        assert_eq!(
            error.to_string(),
            "#agent-swap holds live cohorts of several teams: forge#agent-swap, \
             recon#agent-swap; select one with `<team>#agent-swap`"
        );
    }

    #[test]
    fn a_lane_target_with_no_cohort_lists_the_live_ones() {
        let agents = vec![agent("forge-docs", "forge", "docs")];

        let error = select(Target::Lane("typo"), None, &agents).expect_err("no cohort");
        assert_eq!(
            error.to_string(),
            "no live team cohort in #typo; live cohorts: forge#docs"
        );

        let error = select(Target::Lane("typo"), None, &[]).expect_err("no cohort");
        assert_eq!(error.to_string(), "no live team cohort in #typo");
    }

    #[test]
    fn selection_prefers_the_current_lane() {
        let agents = vec![
            agent("auth", "forge", "auth"),
            agent("docs", "forge", "docs"),
        ];

        let docs = rimz::ids::LaneName::dashed("docs");
        let selected = select(team("forge", None), Some(&docs), &agents).expect("current cohort");

        assert_eq!(selected.channel(), "docs");
    }

    #[test]
    fn selection_lists_ambiguous_lanes_and_the_fix() {
        let agents = vec![
            agent("auth", "forge", "auth"),
            agent("docs", "forge", "docs"),
        ];

        let error = select(team("forge", None), None, &agents).expect_err("ambiguous cohorts");

        assert!(error.to_string().contains("#auth, #docs"));
        assert!(error.to_string().contains("forge#<lane>"));
    }

    #[test]
    fn selection_names_an_unmatched_worktree_and_live_lanes() {
        let agents = vec![
            agent("auth", "forge", "auth"),
            agent("docs", "forge", "docs"),
        ];

        let error = select(team("forge", Some("typo")), None, &agents).expect_err("missing cohort");

        assert!(error.to_string().contains("forge#typo"));
        assert!(error.to_string().contains("#auth, #docs"));
    }

    #[test]
    fn main_selects_only_the_trunk_cohort() {
        let external = vec![agent("ext", "forge", "external")];
        let error =
            select(team("forge", Some("main")), None, &external).expect_err("no trunk cohort");
        assert!(
            error
                .to_string()
                .contains("no live cohort matching `forge#main`"),
            "{error}"
        );

        let mut trunk = agent("trunk", "forge", "forge");
        rimz::testkit::set_trunk(&mut trunk);
        let agents = vec![trunk, agent("ext", "forge", "external")];
        let selected = select(team("forge", Some("main")), None, &agents).expect("trunk cohort");
        assert_eq!(selected.members[0].agent_id.as_str(), "trunk");
    }

    #[test]
    fn selection_points_to_resume_when_nothing_is_live() {
        let error = select(team("forge", None), None, &[]).expect_err("missing cohort");

        assert!(error.to_string().contains("rimz teams resume forge"));
    }

    #[test]
    fn focus_picks_the_actionable_member_with_the_highest_attention_score() {
        let mut roster_first = agent("first", "forge", "docs");
        roster_first.status = rimz::agents::AgentStatus::Waiting;
        let mut urgent = agent("urgent", "forge", "docs");
        urgent.status = rimz::agents::AgentStatus::Failed;
        let agents = vec![roster_first, urgent];
        let cohort = rimz::address::team_cohorts(&agents)
            .into_iter()
            .next()
            .expect("cohort");
        let scores = HashMap::from([("first", 10), ("urgent", 50)]);

        assert_eq!(
            focus_member(&cohort, None, &scores, jiff::Timestamp::UNIX_EPOCH)
                .agent_id
                .as_str(),
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
        let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            vec![team_member, ad_hoc],
            jiff::Timestamp::UNIX_EPOCH,
        );
        let peers = rimz::address::addressable_agents(&snapshot);

        assert_eq!(
            rimz::address::agent_handle(peers[0], &peers, true),
            "@codex-1#feat-x"
        );
    }
}
