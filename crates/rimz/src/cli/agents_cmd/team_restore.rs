use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::agents::AgentState;
use rimz::config::{CommandsConfig, ProfilesConfig, TeamsConfig};
use rimz::harness::spec::LayoutSpec;
use rimz::mux::ResumeTab;
use rimz::store::AgentLaunchAppend;
use rimz::store::event::AgentLaunchState;
use rimz::store::runtime::AgentLiveness;

use super::launch::{
    LayoutPaneParams, cohort_cells, fresh_resume_launch_requests, layout_panes_with_names,
};

#[derive(Clone, Debug)]
pub(crate) struct PlannedTeamTab {
    pub(crate) label: String,
    pub(crate) cwd: PathBuf,
    pub(crate) channel: Option<String>,
    pub(crate) team: String,
    pub(crate) layout: LayoutSpec,
    pub(crate) cohort: rimz::harness::resume::CohortResumePlan,
    pub(crate) freshest: Timestamp,
}

pub(crate) fn plan_team_restore_tabs(
    agents: &[AgentState],
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    project_root: Option<&Path>,
    worktree_exists: impl Fn(&Path) -> bool,
) -> Vec<PlannedTeamTab> {
    let mut groups: BTreeMap<(String, PathBuf), Vec<&AgentState>> = BTreeMap::new();
    for agent in agents {
        if agent.parent_agent_id.is_some() || agent.agent_id.is_empty() {
            continue;
        }
        let Some(team) = agent.team.as_deref().filter(|team| !team.is_empty()) else {
            continue;
        };
        if !teams.0.contains_key(team) {
            continue;
        }
        let Some(worktree) = normalized_agent_worktree(agent) else {
            continue;
        };
        groups
            .entry((team.to_owned(), worktree))
            .or_default()
            .push(agent);
    }

    let mut tabs = Vec::new();
    for ((team, cwd), group) in groups {
        let Ok(layout) = rimz::harness::spec::resolve_team(&team, teams, profiles, commands) else {
            continue;
        };
        let cells = cohort_cells(&layout);
        let group_agents = group.iter().copied().cloned().collect::<Vec<_>>();
        let Ok(mut cohort) = rimz::harness::resume::plan_cohort_resume(
            &group_agents,
            |_| AgentLiveness::Dead,
            &cells,
            Some(&team),
            |path| worktree_exists(path),
        ) else {
            continue;
        };
        let Some(newest) = newest_agent(&group) else {
            continue;
        };
        let channel = project_root
            .and_then(|project_root| {
                rimz::harness::target::resolve_room_channel(
                    project_root,
                    &cwd,
                    Some(&team),
                    cohort.channel.as_deref(),
                )
            })
            .or_else(|| cohort.channel.clone());
        cohort.channel = channel.clone();
        let label = rimz::harness::resume::channel_label(channel.as_deref(), &cwd);
        tabs.push(PlannedTeamTab {
            label,
            cwd,
            channel,
            team,
            layout,
            cohort,
            freshest: newest.last_activity,
        });
    }
    tabs.sort_by(|a, b| newest_cmp(a.freshest, &a.team, b.freshest, &b.team));
    tabs
}

pub(crate) fn materialize_team_restore_tab(
    store: &rimz::Store,
    workspace_id: &rimz::WorkspaceId,
    session_name: &str,
    teams: &TeamsConfig,
    planned: &PlannedTeamTab,
) -> Result<ResumeTab> {
    let team_roles = teams.0.get(&planned.team).map(|team| team.roles.as_slice());
    let launch_requests = fresh_resume_launch_requests(
        &planned.layout,
        &planned.cohort,
        Some(&planned.team),
        team_roles,
        planned.channel.as_deref(),
    )?;
    let identities = if launch_requests.is_empty() {
        Vec::new()
    } else {
        store.append_agent_launches_allocating(
            &launch_requests,
            &AgentLaunchAppend {
                workspace_id: workspace_id.clone(),
                session_name: session_name.to_owned(),
                cwd: planned.cwd.clone(),
                worktree_name: None,
                channel: planned.channel.clone(),
                prompt: None,
                description: None,
                state: AgentLaunchState::Starting,
                pane_id: None,
            },
        )?
    };
    let layout = layout_panes_with_names(
        &planned.layout,
        LayoutPaneParams {
            cwd: &planned.cwd,
            prompt: None,
            cleanup_worktree: false,
            in_place: false,
            team: Some(&planned.team),
            channel: planned.channel.as_deref(),
            resume_seeds: Some(&planned.cohort.seeds),
        },
        &identities,
    )
    .context("building team restore layout")?;
    Ok(ResumeTab {
        label: planned.label.clone(),
        cwd: planned.cwd.clone(),
        layout,
    })
}

pub(crate) fn planned_team_matches_agent(planned: &PlannedTeamTab, agent: &AgentState) -> bool {
    agent.team.as_deref() == Some(planned.team.as_str())
        && normalized_agent_worktree(agent).as_deref() == Some(planned.cwd.as_path())
}

fn normalized_agent_worktree(agent: &AgentState) -> Option<PathBuf> {
    agent
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .map(rimz::worktree::normalize_path_lexical)
}

fn newest_agent<'a>(agents: &[&'a AgentState]) -> Option<&'a AgentState> {
    agents.iter().copied().min_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    })
}

fn newest_cmp(
    left: Timestamp,
    left_tie: &str,
    right: Timestamp,
    right_tie: &str,
) -> std::cmp::Ordering {
    right.cmp(&left).then_with(|| left_tie.cmp(right_tie))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::AgentStatus;
    use rimz::config::{Profile, RoleBinding, Team};
    use rimz::ids::{MuxName, PaneId};
    use rimz::pane::PaneRef;

    fn pane(raw: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, raw),
            session_name: String::new(),
            view_id: None,
            view_kind: None,
            view_name: None,
            is_focused: false,
            is_floating: false,
            command: None,
            spawn_command: None,
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }

    fn agent(kind: &str, id: &str, role: &str, worktree: &str, secs_ago: i64) -> AgentState {
        let when = Timestamp::now() - std::time::Duration::from_secs(secs_ago.max(0) as u64);
        let mut agent = root_agent(kind, id, when);
        agent.status = AgentStatus::Idle;
        agent.pane = Some(pane(&format!("%{id}")));
        agent.worktree_path = Some(worktree.to_owned());
        agent.team = Some("pcr".to_owned());
        agent.role = Some(role.to_owned());
        agent
    }

    fn root_agent(kind: &str, id: &str, now: Timestamp) -> AgentState {
        AgentState {
            status: AgentStatus::Running,
            ..rimz::testkit::agent_state(kind, id, now)
        }
    }

    fn configs() -> (TeamsConfig, ProfilesConfig, CommandsConfig) {
        let mut profiles = ProfilesConfig::default();
        profiles
            .0
            .insert("claude-plan".to_owned(), profile("claude"));
        profiles.0.insert("codex-code".to_owned(), profile("codex"));
        let mut teams = TeamsConfig::default();
        teams.0.insert(
            "pcr".to_owned(),
            Team {
                roles: vec![
                    RoleBinding {
                        role: "planner".to_owned(),
                        profile: "claude-plan".to_owned(),
                        mode: None,
                        model: None,
                        effort: None,
                        system_prompt_file: None,
                        append_system_prompt_file: None,
                        args: None,
                    },
                    RoleBinding {
                        role: "coder".to_owned(),
                        profile: "codex-code".to_owned(),
                        mode: None,
                        model: None,
                        effort: None,
                        system_prompt_file: None,
                        append_system_prompt_file: None,
                        args: None,
                    },
                ],
                layout: Some("planner,coder".to_owned()),
            },
        );
        (teams, profiles, CommandsConfig::default())
    }

    fn profile(agent: &str) -> Profile {
        Profile {
            agent: agent.to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            args: None,
        }
    }

    #[test]
    fn plans_team_restore_in_declared_layout_order() {
        let (teams, profiles, commands) = configs();
        let planner = agent("claude", "planner", "planner", "/repo/pcr", 3);
        let coder = agent("codex", "coder", "coder", "/repo/pcr", 5);

        let tabs = plan_team_restore_tabs(
            &[coder, planner],
            &teams,
            &profiles,
            &commands,
            Some(Path::new("/repo")),
            |_| true,
        );

        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].label, "#pcr");
        assert_eq!(tabs[0].cohort.seeds.len(), 2);
        assert!(matches!(
            &tabs[0].cohort.seeds[0],
            rimz::harness::resume::CohortSeed::Resume(agent) if agent.agent_id.as_str() == "planner"
        ));
        assert!(matches!(
            &tabs[0].cohort.seeds[1],
            rimz::harness::resume::CohortSeed::Resume(agent) if agent.agent_id.as_str() == "coder"
        ));
    }

    #[test]
    fn plans_fresh_seed_for_missing_team_member() {
        let (teams, profiles, commands) = configs();
        let planner = agent("claude", "planner", "planner", "/repo/pcr", 3);

        let tabs = plan_team_restore_tabs(
            &[planner],
            &teams,
            &profiles,
            &commands,
            Some(Path::new("/repo")),
            |_| true,
        );

        assert_eq!(tabs.len(), 1);
        assert!(matches!(
            tabs[0].cohort.seeds[0],
            rimz::harness::resume::CohortSeed::Resume(_)
        ));
        assert_eq!(
            tabs[0].cohort.seeds[1],
            rimz::harness::resume::CohortSeed::Fresh
        );
    }

    #[test]
    fn ignores_group_whose_team_no_longer_resolves() {
        let (_teams, profiles, commands) = configs();
        let planner = agent("claude", "planner", "planner", "/repo/pcr", 3);

        let tabs = plan_team_restore_tabs(
            &[planner],
            &TeamsConfig::default(),
            &profiles,
            &commands,
            Some(Path::new("/repo")),
            |_| true,
        );

        assert!(tabs.is_empty());
    }
}
