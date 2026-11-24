//! `agent` and `team`: the jobs of `rimz agents <profile> --bg --new-tab` and
//! `rimz teams launch`, through the one layout launch the commands go through. The room's
//! project root and an absolute `cwd` reach the launch as values, since the server's process
//! directory can lie outside the room.

use std::path::PathBuf;
use std::sync::Arc;

use super::Tools;
use super::notices::{CollectedNotices, with_warnings};
use super::room::Room;
use super::start::{Start, TaskKind};
use crate::config::MachineConfig;
use crate::config::effective::LaunchAgents;
use crate::launcher::context::resolve_caller;
use crate::launcher::{
    CohortRequest, LaunchOutcome, LaunchOverrides, LayoutEntry, LayoutLaunch, LayoutRoom,
    OpenedLayout,
};
use crate::mcp::catalog::{AgentArgs, TeamArgs};
use crate::mcp::{Answer, Failure};
use crate::store::run::ReportTo;

/// What the two tools launch, as values the command would have parsed from its flags.
struct Launch {
    spec: String,
    prompt: String,
    description: Option<String>,
    name: Option<String>,
    agent: Option<String>,
    worktree: Option<String>,
    from_pr: Option<crate::forge::PrTarget>,
    cwd: Option<PathBuf>,
    fresh: bool,
    teams_scene: bool,
}

/// Launch one persistent peer in a new background tab, started by the caller.
pub(super) fn launch_agent(tools: &Tools, args: &AgentArgs) -> Result<Answer, Failure> {
    let cwd = args
        .cwd
        .as_deref()
        .map(|cwd| tools.dir(Some(cwd)))
        .transpose()?;
    let from_pr = pr_target(args.from_pr.as_deref())?;
    let (room, machine, effective) = caller_room(tools, "an agent")?;
    let spec = args.profile.trim();
    if effective.teams.0.contains_key(spec) {
        return Err(Failure::new(format!(
            "`{spec}` is a team; launch it with `team`"
        )));
    }
    if let Some(name) = args.name.as_deref() {
        crate::harness::plan::validate_agent_name(name)?;
    }
    let agent = crate::harness::plan::normalized_preset_value(args.agent.as_deref());
    let (opened, warnings) = open(
        &room,
        machine,
        Launch {
            spec: spec.to_owned(),
            prompt: args.prompt.clone(),
            description: args.description.clone(),
            name: args.name.clone(),
            agent,
            worktree: args.worktree.clone(),
            from_pr,
            cwd,
            fresh: args.fresh,
            teams_scene: false,
        },
    )?;
    let seat = opened
        .identities
        .get(opened.leader_index.unwrap_or(0))
        .ok_or_else(|| Failure::new("the launch opened no agent"))?;
    let report = match &opened.team {
        Some((team, _)) => seat_report(&team_id(team, &opened)?),
        None => report(TaskKind::Agent, opened.peer.is_some()).to_owned(),
    };
    Ok(Answer::data(&Start {
        warnings,
        ..Start::running(TaskKind::Agent, &seat.name, report)
    }))
}

/// Launch a configured team's cohort in its worktree, started by the caller.
pub(super) fn launch_team(tools: &Tools, args: &TeamArgs) -> Result<Answer, Failure> {
    let from_pr = pr_target(args.from_pr.as_deref())?;
    let (room, machine, effective) = caller_room(tools, "a team")?;
    let team = args.team.trim();
    require_team(&effective, &machine, team)?;
    let (opened, warnings) = open(
        &room,
        machine,
        Launch {
            spec: team.to_owned(),
            prompt: args.prompt.clone(),
            description: args.description.clone(),
            name: None,
            agent: None,
            worktree: args.worktree.clone(),
            from_pr,
            cwd: None,
            fresh: false,
            teams_scene: true,
        },
    )?;
    Ok(Answer::data(&Start {
        warnings,
        ..Start::running(
            TaskKind::Team,
            &team_id(team, &opened)?,
            report(TaskKind::Team, opened.peer.is_some()),
        )
    }))
}

/// The name under which `tasks` lists the opened lane of `team`: the team instance.
fn team_id(team: &str, opened: &OpenedLayout) -> Result<String, Failure> {
    let instance = crate::ids::TeamInstanceId::new(team, Some(opened.lane.routing_name()))
        .map_err(|err| Failure::new(err.to_string()))?;
    Ok(format!("{}#{}", instance.team(), instance.lane_text()))
}

fn pr_target(raw: Option<&str>) -> Result<Option<crate::forge::PrTarget>, Failure> {
    raw.map(|raw| crate::forge::parse(raw).map_err(Failure::new))
        .transpose()
}

/// The caller's room, refused for a caller RimZ cannot identify, with the configuration the
/// spec resolves against.
fn caller_room(
    tools: &Tools,
    thing: &str,
) -> Result<(Room, Arc<MachineConfig>, LaunchAgents), Failure> {
    let room = Room::open(&tools.dir(None)?)?;
    if resolve_caller(&room.store)?.is_none() {
        return Err(Failure::new(format!(
            "launching {thing} is only available to an agent RimZ can identify"
        ))
        .with_hint("serve this tool to an agent RimZ launched in this room"));
    }
    let machine = MachineConfig::load_lenient();
    let effective = crate::config::effective::load(&machine, &room.workspace.project_root)
        .map_err(|err| Failure::from(anyhow::Error::from(err)))?;
    Ok((room, machine, effective))
}

/// `team` launches only a configured team, and names the valid ones when it is not.
fn require_team(
    effective: &LaunchAgents,
    machine: &MachineConfig,
    team: &str,
) -> Result<(), Failure> {
    let teams = &effective.teams;
    if teams.0.contains_key(team) {
        return Ok(());
    }
    if let Some(detail) = machine.definition_failure_for(team) {
        return Err(Failure::new(detail));
    }
    if teams.role_spec(team).is_some() {
        return Err(Failure::new(format!(
            "`{team}` names one role; launch it with `agent`"
        )));
    }
    if teams.0.is_empty() {
        return Err(
            Failure::new(format!("unknown team `{team}`; no teams are configured"))
                .with_hint("`rimz packs install crew` installs the default teams"),
        );
    }
    let valid: Vec<&str> = teams.0.keys().map(String::as_str).collect();
    Err(Failure::new(format!(
        "unknown team `{team}`; configured teams: {}",
        valid.join(", ")
    )))
}

/// Run the command's launch, as `--bg --new-tab`, reporting to the caller. A launch that opens
/// nothing the caller owns is a failure, which says so when the brief still reached a cohort.
fn open(
    room: &Room,
    machine: Arc<MachineConfig>,
    launch: Launch,
) -> Result<(Box<OpenedLayout>, Vec<String>), Failure> {
    let mut notices = CollectedNotices::default();
    let tool = if launch.teams_scene {
        TaskKind::Team
    } else {
        TaskKind::Agent
    };
    let from_pr = launch.from_pr.is_some();
    let outcome = enter_and_launch(room, machine, launch, &mut notices);
    let closed = notices.closed_cohort().map(str::to_owned);
    let warnings = notices.into_lines();
    let failure = match outcome {
        Ok(LaunchOutcome::Opened(opened)) => return Ok((opened, warnings)),
        Ok(LaunchOutcome::BudgetExceeded { reason }) => Failure::new(reason),
        Ok(LaunchOutcome::Declined) => Failure::new("the launch was declined; nothing launched")
            .with_hint("call `tasks` to see what you have running"),
        Ok(LaunchOutcome::Existing {
            prompt_delivered: true,
        }) => Failure::new(
            "the cohort already live in this worktree took your brief: it went to its leader, \
             and nothing new started",
        )
        .with_hint("do not send the brief again; its leader answers in its own turn"),
        Ok(LaunchOutcome::Existing {
            prompt_delivered: false,
        }) => Failure::new("a cohort is already live in this worktree; nothing launched")
            .with_hint("call `tasks` to see what you have running"),
        Err(err) => with_closed_cohort_hint(Failure::from(err), closed.as_deref(), tool, from_pr),
    };
    Err(with_warnings(failure, warnings))
}

/// The way past a refused closed cohort, in the called tool's terms: the library's text names
/// the command's `--fresh`, which `team` has no argument for, and its resume command when one
/// exists. `agent` refuses `fresh` beside `from_pr`, so after a `from_pr` call the hint names
/// the closed worktree, a marker name that `worktree` takes as it is.
fn with_closed_cohort_hint(
    failure: Failure,
    closed: Option<&str>,
    tool: TaskKind,
    from_pr: bool,
) -> Failure {
    let Some(name) = closed else {
        return failure;
    };
    let hint = match (tool, from_pr) {
        (TaskKind::Team, false) => "name another `worktree`".to_owned(),
        (TaskKind::Team, true) => "`team` cannot start a new session in this worktree; name a \
                                   `worktree` in place of `from_pr` to launch outside the pull \
                                   request"
            .to_owned(),
        (_, false) => "set `fresh` to start a new session in this worktree".to_owned(),
        (_, true) => format!(
            "to start a new session in this worktree, call again with `worktree` {name:?} and \
             `fresh` in place of `from_pr`"
        ),
    };
    failure.with_hint(hint)
}

fn enter_and_launch(
    room: &Room,
    machine: Arc<MachineConfig>,
    launch: Launch,
    notices: &mut CollectedNotices,
) -> anyhow::Result<LaunchOutcome> {
    let entered = LayoutRoom::enter(LayoutEntry {
        root: Some(room.workspace.project_root.clone()),
        mux: None,
        machine,
        worktree: launch.worktree.as_deref(),
        from_pr: launch.from_pr.is_some(),
        fresh: launch.fresh,
        cwd: launch.cwd.as_deref(),
        under_fire: false,
        caller_repo: true,
    })?;
    let layout = LayoutLaunch {
        spec: Some(launch.spec),
        prompt: Some(launch.prompt),
        name: launch.name,
        cwd: launch.cwd,
        cohort: CohortRequest {
            worktree: launch.worktree,
            from_pr: launch.from_pr,
            description: launch.description,
            fresh: launch.fresh,
            bg: true,
            new_tab: true,
            ..CohortRequest::default()
        },
        overrides: LaunchOverrides {
            agent: launch.agent,
            ..LaunchOverrides::default()
        },
        teams_scene: launch.teams_scene,
        one_seat: !launch.teams_scene,
        report_to: ReportTo::Launcher,
        ..LayoutLaunch::default()
    };
    crate::launcher::launch_layout(&entered, layout, false, notices)
}

/// How a seat of `team`, launched through `agent`, answers: its team reports, not the seat.
fn seat_report(team: &str) -> String {
    format!(
        "no AGENT_REPORT comes, since this agent is a seat of team `{}`: `join` on the id \
         is the way to its answer, and `tasks` lists it under `team:{team}`",
        team.split_once('#').map_or(team, |(name, _)| name)
    )
}

/// How the launch's result reaches the caller; a launch that opened no reporting run says so.
fn report(kind: TaskKind, reports: bool) -> &'static str {
    match (kind, reports) {
        (TaskKind::Team, true) => {
            "a TEAM_REPORT message to you each time the team's board reaches Done"
        }
        (TaskKind::Team, false) => {
            "no report comes, since the leader's provider cannot report; `join` on the id is the \
             way to the answer"
        }
        (_, true) => "an AGENT_REPORT message to you once none of your agents is mid-turn",
        (_, false) => {
            "no report comes, since this agent's provider cannot report; `join` on the id is \
             the way to its answers"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_refused_closed_cohort_gets_a_hint_in_the_called_tools_terms() {
        let hint = |error: &str, closed, tool, from_pr| {
            with_closed_cohort_hint(Failure::new(error), closed, tool, from_pr).hint
        };
        let closed = "worktree `pr-1` is closed";
        let fresh = Some("set `fresh` to start a new session in this worktree".to_owned());
        assert_eq!(hint(closed, Some("pr-1"), TaskKind::Agent, false), fresh);
        assert_eq!(
            hint(closed, Some("pr-1"), TaskKind::Agent, true),
            Some(
                "to start a new session in this worktree, call again with `worktree` \"pr-1\" \
                 and `fresh` in place of `from_pr`"
                    .to_owned()
            ),
        );
        assert_eq!(
            hint(closed, Some("pr-1"), TaskKind::Team, false),
            Some("name another `worktree`".to_owned()),
        );
        assert_eq!(
            hint(closed, Some("pr-1"), TaskKind::Team, true),
            Some(
                "`team` cannot start a new session in this worktree; name a `worktree` in \
                 place of `from_pr` to launch outside the pull request"
                    .to_owned()
            ),
        );
        let names_the_flag = "cannot relaunch 'demo' fresh; or resume without --fresh";
        for (tool, from_pr) in [(TaskKind::Agent, true), (TaskKind::Team, false)] {
            assert_eq!(hint(names_the_flag, None, tool, from_pr), None);
        }
    }

    #[test]
    fn the_report_sentence_says_whether_a_report_comes() {
        assert!(report(TaskKind::Agent, true).contains("AGENT_REPORT"));
        assert!(report(TaskKind::Team, true).contains("TEAM_REPORT"));
        for kind in [TaskKind::Agent, TaskKind::Team] {
            let none = report(kind, false);
            assert!(
                none.contains("no report") && none.contains("`join`"),
                "{none}"
            );
        }
    }
}
