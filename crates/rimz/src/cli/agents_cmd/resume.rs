//! Place-first lane recovery for a live room.
//!
//! Lane resolution is local and durable-store-backed. Whole-lane recovery
//! reuses the room-rebirth team and flat planners; partial recovery splits only
//! closed sessions beside a surviving live member.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use jiff::Timestamp;
use rimz::agents::{AgentState, LocalSessionObservation};
use rimz::harness::resume::{
    concurrent_session_set, discovered_agent_state,
    materialize_team_restore_tab as build_team_restore_tab, resume_session_present,
    split_team_and_flat,
};
use rimz::ids::{AgentKind, AgentSessionId, PaneId};
use rimz::mux::{ResumeTab, SplitPaneOptions, TabOptions};
use rimz::store::runtime::AgentLiveness;

use super::{GlobalFlags, RoomContext, RoomSizing};

#[derive(Clone, Debug)]
struct LocalWorktree {
    name: String,
    path: PathBuf,
    branch: Option<String>,
    from_pr: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedLane {
    display: String,
    path: PathBuf,
    channel: Option<String>,
    worktree_name: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
enum ResumeLaneErr {
    #[error("no lane '{scope}' in this workspace")]
    Unknown { scope: String },
    #[error(
        "worktree for '{scope}' was removed; recreate it with rimz agents <spec> -w {worktree}"
    )]
    Removed { scope: String, worktree: String },
    #[error(
        "PR {number} has no local worktree; start one with rimz agents <spec> --from-pr {number}"
    )]
    PrNotLocal { number: u64 },
    #[error("nothing to resume in '{scope}'")]
    Nothing { scope: String },
}

#[derive(Debug)]
struct LaneLiveness {
    live: Vec<AgentState>,
    closed: Vec<AgentState>,
}

#[derive(Debug)]
struct LaneSummary {
    path: PathBuf,
    label: String,
    members: usize,
    live: usize,
    freshest: Timestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum LaneCandidateKey {
    Pane(PaneId),
    Session(AgentKind, AgentSessionId),
}

pub(super) fn resume_lane(
    scope: Option<String>,
    from_pr: Option<rimz::forge::PrTarget>,
    bg: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace =
        rimz::workspace::WorkspaceResolver::resolve_participant(".", globals.root.clone())
            .context("resolving current workspace")?;
    let mux = rimz::mux::auto_detect_backend(globals.mux).map_err(|_| {
        anyhow::anyhow!(crate::cli::agents_launch::live_session_guidance(
            &workspace.session_name
        ))
    })?;
    let machine_config = crate::cli::machine_config();
    let room = RoomContext::from_resolved(
        &workspace,
        machine_config.clone(),
        mux,
        RoomSizing::OrdinaryTab,
    )?;
    let backend = room.backend();
    crate::cli::agents_launch::ensure_live_session(backend, &workspace.session_name)?;
    crate::cli::record_workspace(&workspace)?;

    let store = crate::cli::open_store(&workspace)?;
    let projection = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?;

    let worktrees = local_worktrees(&workspace)?;
    if scope.is_none() && from_pr.is_none() && workspace.worktree_root == workspace.project_root {
        return list_resumable_lanes(&projection.agents, &workspace.project_root, &worktrees);
    }

    let lane = match (scope.as_deref(), from_pr) {
        (_, Some(pr)) => resolve_pr_lane(pr.number, &worktrees)?,
        (Some(scope), None) => resolve_scope_lane(scope, &projection.agents, &worktrees)?,
        (None, None) => resolve_cwd_lane(
            &workspace.worktree_root,
            &workspace.project_root,
            &projection.agents,
            &worktrees,
        )?,
    };
    ensure_lane_exists(&lane, Path::is_dir)?;

    let lane_agents = current_lane_candidates(&projection.agents, &lane);
    let liveness = split_lane_liveness(&lane_agents, rimz::store::runtime::agent_liveness);
    let machine_config = crate::cli::machine_config();
    if should_discover_lane_sessions(&lane_agents, &liveness) {
        return resume_discovered_lane(&workspace, &room, &lane, machine_config.as_ref(), bg);
    }
    if liveness.closed.is_empty() {
        let agent = liveness
            .live
            .first()
            .context("live lane has no focus candidate")?;
        let pane = agent
            .pane
            .as_ref()
            .context("live lane agent has no bound pane")?;
        backend.focus_pane(&pane.pane_id, Some(&workspace.session_name))?;
        writeln!(
            std::io::stdout().lock(),
            "lane '{}' is already live — focused",
            lane.display
        )?;
        return Ok(());
    }
    if !liveness.closed.iter().any(resume_session_present) {
        return Err(ResumeLaneErr::Nothing {
            scope: lane.display,
        }
        .into());
    }

    if !liveness.live.is_empty() {
        return resume_closed_into_live_lane(
            &workspace,
            backend,
            &lane,
            &liveness,
            machine_config.resume.max,
            bg,
        );
    }

    resume_closed_lane(
        &workspace,
        &room,
        &store,
        &lane,
        &liveness.closed,
        machine_config.as_ref(),
        bg,
    )
}

fn should_discover_lane_sessions(lane_agents: &[AgentState], liveness: &LaneLiveness) -> bool {
    lane_agents.is_empty()
        || (liveness.live.is_empty() && !liveness.closed.iter().any(resume_session_present))
}

fn discover_lane_sessions(path: &Path) -> Vec<LocalSessionObservation> {
    rimz::agents::ADAPTERS
        .iter()
        .copied()
        .filter(|adapter| adapter.descriptor().capabilities.local_session_discovery)
        .flat_map(|adapter| adapter.discover_local_sessions(path))
        .filter(|observation| {
            std::fs::metadata(&observation.transcript_path)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        })
        .collect()
}

fn resume_discovered_lane(
    workspace: &rimz::ResolvedWorkspace,
    room: &RoomContext,
    lane: &ResolvedLane,
    machine_config: &rimz::config::MachineConfig,
    bg: bool,
) -> Result<()> {
    let observations = discover_lane_sessions(&lane.path);
    if observations.is_empty() {
        return Err(ResumeLaneErr::Nothing {
            scope: lane.display.clone(),
        }
        .into());
    }
    let (resume, skipped) = concurrent_session_set(observations);
    let states = resume
        .iter()
        .map(|observation| discovered_agent_state(observation, lane.channel.as_deref()))
        .collect::<Vec<_>>();
    for kind in states.iter().map(|agent| agent.kind.as_str()) {
        super::launch::agent_launch_env(&workspace.project_root, kind)?;
    }
    let plan = flat_resume_plan(
        &states,
        machine_config.resume.max,
        Some(&workspace.project_root),
    );
    report_discovery_skips(&skipped)?;
    report_resume_skips(&plan.skipped)?;
    if plan.tabs.is_empty() {
        return Err(ResumeLaneErr::Nothing {
            scope: lane.display.clone(),
        }
        .into());
    }
    let count = plan.tabs.iter().map(ResumeTab::pane_count).sum::<usize>();
    for tab in plan.tabs {
        open_resume_tab(workspace, room, tab, bg)?;
    }
    writeln!(
        std::io::stdout().lock(),
        "resumed {count} agent{} in '{}'",
        if count == 1 { "" } else { "s" },
        lane.display
    )?;
    Ok(())
}

fn local_worktrees(workspace: &rimz::ResolvedWorkspace) -> Result<Vec<LocalWorktree>> {
    if workspace.root_class != rimz::workspace::RootClass::Repo {
        return Ok(Vec::new());
    }
    rimz::worktree::list(&workspace.project_root)?
        .into_iter()
        .map(|entry| {
            let marker = rimz::worktree::read_marker_for_worktree(&entry.path)?
                .with_context(|| format!("reading worktree marker for {}", entry.path.display()))?;
            Ok(LocalWorktree {
                name: entry.name,
                path: rimz::worktree::normalize_path_lexical(&entry.path),
                branch: entry.branch,
                from_pr: marker.from_pr,
            })
        })
        .collect()
}

fn resolve_pr_lane(number: u64, worktrees: &[LocalWorktree]) -> Result<ResolvedLane> {
    let fallback = format!("pr-{number}");
    let worktree = worktrees
        .iter()
        .find(|worktree| worktree.from_pr == Some(number))
        .or_else(|| worktrees.iter().find(|worktree| worktree.name == fallback))
        .ok_or(ResumeLaneErr::PrNotLocal { number })?;
    Ok(lane_from_worktree(worktree))
}

fn resolve_scope_lane(
    raw_scope: &str,
    agents: &[AgentState],
    worktrees: &[LocalWorktree],
) -> Result<ResolvedLane> {
    let scope = raw_scope.strip_prefix('#').unwrap_or(raw_scope);
    if let Some(agent) = newest_matching_agent(agents, scope) {
        let path = agent
            .worktree_path
            .as_deref()
            .map(Path::new)
            .map(rimz::worktree::normalize_path_lexical)
            .context("lane agent has no working directory")?;
        let channel = rimz::harness::target::agent_channel(agent);
        let worktree_name = worktrees
            .iter()
            .find(|worktree| worktree.path == path)
            .map(|worktree| worktree.name.clone())
            .unwrap_or_else(|| path_label(&path));
        return Ok(ResolvedLane {
            display: raw_scope.to_owned(),
            path,
            channel,
            worktree_name,
        });
    }
    if let Some(worktree) = worktrees
        .iter()
        .find(|worktree| worktree_matches_scope(worktree, scope))
    {
        let mut lane = lane_from_worktree(worktree);
        lane.display = raw_scope.to_owned();
        return Ok(lane);
    }
    Err(ResumeLaneErr::Unknown {
        scope: raw_scope.to_owned(),
    }
    .into())
}

fn resolve_cwd_lane(
    cwd: &Path,
    project_root: &Path,
    agents: &[AgentState],
    worktrees: &[LocalWorktree],
) -> Result<ResolvedLane> {
    let cwd = rimz::worktree::normalize_path_lexical(cwd);
    if cwd == rimz::worktree::normalize_path_lexical(project_root) {
        return Err(ResumeLaneErr::Unknown {
            scope: path_label(&cwd),
        }
        .into());
    }
    if let Some(worktree) = worktrees.iter().find(|worktree| worktree.path == cwd) {
        return Ok(lane_from_worktree(worktree));
    }
    let agent = newest_agent_at_path(agents, &cwd).ok_or_else(|| ResumeLaneErr::Unknown {
        scope: path_label(&cwd),
    })?;
    let channel = rimz::harness::target::agent_channel(agent);
    Ok(ResolvedLane {
        display: channel
            .as_deref()
            .map_or_else(|| path_label(&cwd), |channel| format!("#{channel}")),
        path: cwd.clone(),
        channel,
        worktree_name: path_label(&cwd),
    })
}

fn lane_from_worktree(worktree: &LocalWorktree) -> ResolvedLane {
    ResolvedLane {
        display: worktree.name.clone(),
        path: worktree.path.clone(),
        channel: None,
        worktree_name: worktree.name.clone(),
    }
}

fn worktree_matches_scope(worktree: &LocalWorktree, scope: &str) -> bool {
    worktree.name == scope
        || worktree.branch.as_deref() == Some(scope)
        || worktree.path == Path::new(scope)
        || worktree.path.file_name().is_some_and(|name| name == scope)
}

fn newest_matching_agent<'a>(agents: &'a [AgentState], scope: &str) -> Option<&'a AgentState> {
    agents
        .iter()
        .filter(|agent| root_session(agent))
        .filter(|agent| rimz::harness::target::agent_in_worktree(agent, scope))
        .min_by(newest_agent_cmp)
}

fn newest_agent_at_path<'a>(agents: &'a [AgentState], path: &Path) -> Option<&'a AgentState> {
    agents
        .iter()
        .filter(|agent| root_session(agent))
        .filter(|agent| normalized_agent_path(agent).as_deref() == Some(path))
        .min_by(newest_agent_cmp)
}

fn newest_agent_cmp(left: &&AgentState, right: &&AgentState) -> std::cmp::Ordering {
    right
        .last_activity
        .cmp(&left.last_activity)
        .then_with(|| left.agent_id.cmp(&right.agent_id))
}

fn ensure_lane_exists(lane: &ResolvedLane, exists: impl Fn(&Path) -> bool) -> Result<()> {
    if exists(&lane.path) {
        return Ok(());
    }
    Err(ResumeLaneErr::Removed {
        scope: lane.display.clone(),
        worktree: lane.worktree_name.clone(),
    }
    .into())
}

fn current_lane_candidates(agents: &[AgentState], lane: &ResolvedLane) -> Vec<AgentState> {
    let candidates = agents
        .iter()
        .filter(|agent| root_session(agent))
        .filter(|agent| normalized_agent_path(agent).as_deref() == Some(lane.path.as_path()))
        .filter(|agent| {
            lane.channel.as_deref().is_none_or(|channel| {
                rimz::harness::target::agent_channel(agent).as_deref() == Some(channel)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    dedupe_current_candidates(candidates)
}

fn dedupe_current_candidates(mut candidates: Vec<AgentState>) -> Vec<AgentState> {
    candidates.sort_by(|left, right| {
        right
            .last_activity
            .cmp(&left.last_activity)
            .then_with(|| left.agent_id.cmp(&right.agent_id))
    });
    let mut seen = HashSet::<LaneCandidateKey>::new();
    candidates.retain(|agent| {
        let key = agent.pane.as_ref().map_or_else(
            || LaneCandidateKey::Session(agent.kind.clone(), agent.agent_id.clone()),
            |pane| LaneCandidateKey::Pane(pane.pane_id.clone()),
        );
        seen.insert(key)
    });
    candidates
}

fn root_session(agent: &AgentState) -> bool {
    agent.parent_agent_id.is_none()
        && !agent.agent_id.is_empty()
        && agent
            .worktree_path
            .as_deref()
            .is_some_and(|path| !path.is_empty())
}

fn normalized_agent_path(agent: &AgentState) -> Option<PathBuf> {
    agent
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .map(rimz::worktree::normalize_path_lexical)
}

fn split_lane_liveness(
    agents: &[AgentState],
    liveness: impl Fn(&AgentState) -> AgentLiveness,
) -> LaneLiveness {
    let (live, closed) = agents
        .iter()
        .cloned()
        .partition(|agent| matches!(liveness(agent), AgentLiveness::Live { .. }));
    LaneLiveness { live, closed }
}

fn resume_closed_into_live_lane(
    workspace: &rimz::ResolvedWorkspace,
    backend: &dyn rimz::mux::MuxBackend,
    lane: &ResolvedLane,
    liveness: &LaneLiveness,
    max: usize,
    bg: bool,
) -> Result<()> {
    for agent in &liveness.closed {
        if rimz::agents::find_adapter(agent.kind.as_str()).is_some_and(|adapter| {
            adapter
                .resume_command(agent.agent_id.as_str(), &lane.path)
                .is_some()
        }) && resume_session_present(agent)
        {
            super::launch::agent_launch_env(&workspace.project_root, agent.kind.as_str())?;
        }
    }
    let plan = flat_resume_plan(&liveness.closed, max, Some(&workspace.project_root));
    report_resume_skips(&plan.skipped)?;
    let commands = plan
        .tabs
        .iter()
        .flat_map(|tab| &tab.layout.columns)
        .flat_map(|column| &column.panes)
        .map(|pane| pane.argv.clone())
        .collect::<Vec<_>>();
    if commands.is_empty() {
        return Err(ResumeLaneErr::Nothing {
            scope: lane.display.clone(),
        }
        .into());
    }
    let resumed = commands.len();
    let target = liveness
        .live
        .first()
        .and_then(|agent| agent.pane.as_ref())
        .context("live lane has no pane target")?;
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let mut lane_workspace = workspace.clone();
    lane_workspace.worktree_root = lane.path.clone();
    let channel = lane_channel(lane, &liveness.closed);
    for command in commands {
        backend.split_pane(SplitPaneOptions {
            session_name: None,
            target_view_id: None,
            target_pane_id: Some(target.pane_id.clone()),
            cwd: Some(lane.path.to_string_lossy().into_owned()),
            command: Some(command),
            env: crate::cli::agents_launch::launch_identity_env(
                &lane_workspace,
                channel.as_deref(),
                false,
            ),
            title: None,
            stacked: false,
            direction,
            focus: !bg,
        })?;
    }
    report_live_skips(&liveness.live, &liveness.closed)?;
    writeln!(
        std::io::stdout().lock(),
        "resumed {} closed agent{} in '{}'",
        resumed,
        if resumed == 1 { "" } else { "s" },
        lane.display
    )?;
    Ok(())
}

fn resume_closed_lane(
    workspace: &rimz::ResolvedWorkspace,
    room: &RoomContext,
    store: &rimz::Store,
    lane: &ResolvedLane,
    agents: &[AgentState],
    machine_config: &rimz::config::MachineConfig,
    bg: bool,
) -> Result<()> {
    let launch = super::launch::effective_launch_agents(machine_config, workspace)?;
    let (team, flat_agents) = split_team_and_flat(
        agents,
        &launch.teams,
        &launch.profiles,
        &machine_config.agents.commands,
        Some(&workspace.project_root),
        Path::is_dir,
        resume_session_present,
    );
    for kind in team.iter().flat_map(|planned| planned.layout.agent_kinds()) {
        super::launch::agent_launch_env(&workspace.project_root, kind)?;
    }
    let team_panes = team
        .iter()
        .map(|planned| planned.cohort.seeds.len())
        .sum::<usize>();
    let flat = flat_resume_plan(
        &flat_agents,
        machine_config.resume.max.saturating_sub(team_panes),
        Some(&workspace.project_root),
    );
    for agent in &flat_agents {
        if rimz::agents::find_adapter(agent.kind.as_str()).is_some_and(|adapter| {
            adapter
                .resume_command(agent.agent_id.as_str(), &lane.path)
                .is_some()
        }) && resume_session_present(agent)
        {
            super::launch::agent_launch_env(&workspace.project_root, agent.kind.as_str())?;
        }
    }
    report_resume_skips(&flat.skipped)?;

    let mut tabs = Vec::new();
    for planned in &team {
        tabs.push(build_team_restore_tab(
            store,
            &workspace.workspace_id,
            &workspace.session_name,
            &launch.teams,
            planned,
        )?);
    }
    tabs.extend(flat.tabs);
    if tabs.is_empty() {
        return Err(ResumeLaneErr::Nothing {
            scope: lane.display.clone(),
        }
        .into());
    }
    let count = tabs.iter().map(ResumeTab::pane_count).sum::<usize>();
    for tab in tabs {
        open_resume_tab(workspace, room, tab, bg)?;
    }
    writeln!(
        std::io::stdout().lock(),
        "resumed {count} agent{} in '{}'",
        if count == 1 { "" } else { "s" },
        lane.display
    )?;
    Ok(())
}

fn flat_resume_plan(
    agents: &[AgentState],
    max: usize,
    project_root: Option<&Path>,
) -> rimz::harness::resume::ResumePlan {
    rimz::harness::resume::plan_resume(
        agents,
        &BTreeSet::<(AgentKind, AgentSessionId)>::new(),
        max,
        project_root,
        Path::is_dir,
        resume_session_present,
        &rimz::proc::rimz_exe(),
    )
}

fn open_resume_tab(
    workspace: &rimz::ResolvedWorkspace,
    room: &RoomContext,
    tab: ResumeTab,
    bg: bool,
) -> Result<()> {
    let sidebar = room.sidebar_options(&tab.cwd, Vec::new(), None);
    room.backend()
        .open_tab(&TabOptions {
            session_name: workspace.session_name.clone(),
            title: tab.label,
            cwd: tab.cwd,
            panes: tab.layout,
            focus: !bg,
            dock_sidebar: true,
            sidebar,
        })
        .map_err(Into::into)
}

fn lane_channel(lane: &ResolvedLane, agents: &[AgentState]) -> Option<String> {
    lane.channel.clone().or_else(|| {
        agents
            .first()
            .and_then(rimz::harness::target::agent_channel)
    })
}

fn report_live_skips(live: &[AgentState], closed: &[AgentState]) -> Result<()> {
    let peers = live.iter().chain(closed).collect::<Vec<_>>();
    let mut out = std::io::stdout().lock();
    for agent in live {
        writeln!(
            out,
            "skipped live @{}",
            rimz::harness::target::agent_handle(agent, &peers, true)
        )?;
    }
    Ok(())
}

fn report_resume_skips(skips: &[rimz::harness::resume::ResumeSkip]) -> Result<()> {
    let mut out = std::io::stderr().lock();
    for skip in skips {
        writeln!(
            out,
            "rimz: not resumed: {} ({})",
            skip.label,
            resume_skip_reason(skip.reason)
        )?;
    }
    Ok(())
}

fn report_discovery_skips(skips: &[LocalSessionObservation]) -> Result<()> {
    let mut out = std::io::stderr().lock();
    for observation in skips {
        writeln!(
            out,
            "rimz: not resumed: {} {} (older run)",
            observation.kind, observation.session_id
        )?;
    }
    Ok(())
}

fn resume_skip_reason(reason: rimz::harness::resume::ResumeSkipReason) -> &'static str {
    match reason {
        rimz::harness::resume::ResumeSkipReason::NoResumeSupport => "no resume CLI",
        rimz::harness::resume::ResumeSkipReason::NoConversation => "no saved conversation",
        rimz::harness::resume::ResumeSkipReason::OverCap => "over the resume cap",
    }
}

fn list_resumable_lanes(
    agents: &[AgentState],
    project_root: &Path,
    worktrees: &[LocalWorktree],
) -> Result<()> {
    let summaries = lane_summaries(agents, project_root, rimz::store::runtime::agent_liveness);
    let store_paths = summaries
        .iter()
        .map(|summary| summary.path.as_path())
        .collect::<HashSet<_>>();
    let discovered = worktrees
        .iter()
        .filter(|worktree| !store_paths.contains(worktree.path.as_path()))
        .cloned()
        .map(|worktree| {
            let observations = discover_lane_sessions(&worktree.path);
            (worktree, observations)
        })
        .collect();
    let summaries = merge_native_lane_summaries(summaries, discovered);
    let mut table = crate::cli::render::Table::new(["LANE", "MEMBERS", "LIVE", "CLOSED", "AGE"])
        .right(&[1, 2, 3, 4]);
    let now = Timestamp::now();
    for lane in summaries {
        table.row([
            crate::cli::render::cell(lane.label),
            crate::cli::render::cell(lane.members.to_string()),
            crate::cli::render::cell(lane.live.to_string()),
            crate::cli::render::cell((lane.members - lane.live).to_string()),
            crate::cli::render::cell(crate::cli::render::rel_age(lane.freshest, now)),
        ]);
    }
    table.render(&mut crate::cli::render::out())?;
    Ok(())
}

fn lane_summaries(
    agents: &[AgentState],
    project_root: &Path,
    liveness: impl Fn(&AgentState) -> AgentLiveness,
) -> Vec<LaneSummary> {
    let mut groups = BTreeMap::<(PathBuf, Option<String>), Vec<AgentState>>::new();
    let root = rimz::worktree::normalize_path_lexical(project_root);
    for agent in agents.iter().filter(|agent| root_session(agent)) {
        let Some(path) = normalized_agent_path(agent) else {
            continue;
        };
        let channel = rimz::harness::target::agent_channel(agent).filter(|_| {
            path != root
                || agent
                    .channel
                    .as_deref()
                    .is_some_and(|value| !value.is_empty())
        });
        groups
            .entry((path, channel))
            .or_default()
            .push(agent.clone());
    }
    let mut summaries = groups
        .into_iter()
        .filter_map(|((path, channel), agents)| {
            let lane = ResolvedLane {
                display: String::new(),
                worktree_name: path_label(&path),
                path,
                channel,
            };
            let candidates = dedupe_current_candidates(agents);
            let freshest = candidates.first()?.last_activity;
            let live = candidates
                .iter()
                .filter(|agent| matches!(liveness(agent), AgentLiveness::Live { .. }))
                .count();
            Some(LaneSummary {
                path: lane.path.clone(),
                label: lane_channel(&lane, &candidates).map_or_else(
                    || format!("#{}", path_label(&lane.path)),
                    |value| format!("#{value}"),
                ),
                members: candidates.len(),
                live,
                freshest,
            })
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        right
            .freshest
            .cmp(&left.freshest)
            .then_with(|| left.label.cmp(&right.label))
    });
    summaries
}

fn merge_native_lane_summaries(
    mut summaries: Vec<LaneSummary>,
    discovered: Vec<(LocalWorktree, Vec<LocalSessionObservation>)>,
) -> Vec<LaneSummary> {
    for (worktree, observations) in discovered {
        if summaries
            .iter()
            .any(|summary| summary.path == worktree.path)
        {
            continue;
        }
        let (resume, _) = concurrent_session_set(observations);
        let Some(freshest) = resume
            .iter()
            .map(|observation| observation.last_activity)
            .max()
        else {
            continue;
        };
        summaries.push(LaneSummary {
            path: worktree.path,
            label: format!("#{}", worktree.name),
            members: resume.len(),
            live: 0,
            freshest,
        });
    }
    summaries.sort_by(|left, right| {
        right
            .freshest
            .cmp(&left.freshest)
            .then_with(|| left.label.cmp(&right.label))
    });
    summaries
}

fn path_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::ids::MuxName;
    use rimz::pane::{PaneRef, RuntimeOwner, RuntimeOwnerKind};

    fn agent(kind: &str, id: &str, path: &str, channel: Option<&str>) -> AgentState {
        let mut agent = rimz::testkit::agent_state(kind, id, Timestamp::now());
        agent.worktree_path = Some(path.to_owned());
        agent.channel = channel.map(ToOwned::to_owned);
        agent.pane = Some(PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, format!("%{id}")),
            session_name: "rimz".to_owned(),
            view_id: None,
            view_kind: None,
            view_name: None,
            title: None,
            is_focused: false,
            is_floating: false,
            command: None,
            foreground_cmdline: None,
            spawn_command: None,
            cwd: None,
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        });
        agent
    }

    fn worktree(name: &str, branch: &str, from_pr: Option<u64>) -> LocalWorktree {
        LocalWorktree {
            name: name.to_owned(),
            path: PathBuf::from(format!("/repo-worktrees/{name}")),
            branch: Some(branch.to_owned()),
            from_pr,
        }
    }

    fn local_observation(
        kind: &str,
        id: &str,
        path: &str,
        created: &str,
        last: &str,
    ) -> LocalSessionObservation {
        LocalSessionObservation {
            kind: AgentKind::new_unchecked(kind),
            session_id: AgentSessionId::from(id),
            workspace: PathBuf::from(path),
            transcript_path: PathBuf::from(format!("/provider/{id}.jsonl")),
            created_at: created.parse().unwrap(),
            fresh_binding_at: None,
            first_event_at: None,
            last_activity: last.parse().unwrap(),
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            latest_prompt: None,
            native_prompt_detail: None,
            waiting_since: None,
            context_pct: None,
        }
    }

    #[test]
    fn lane_resolution_accepts_channel_worktree_branch_and_pr_provenance() {
        let agents = vec![agent(
            "codex",
            "docs",
            "/repo-worktrees/review-docs",
            Some("docs"),
        )];
        let worktrees = vec![worktree("review-docs", "feat/docs", Some(69))];

        assert_eq!(
            resolve_scope_lane("#docs", &agents, &worktrees)
                .unwrap()
                .path,
            PathBuf::from("/repo-worktrees/review-docs")
        );
        assert_eq!(
            resolve_scope_lane("review-docs", &[], &worktrees)
                .unwrap()
                .path,
            PathBuf::from("/repo-worktrees/review-docs")
        );
        assert_eq!(
            resolve_scope_lane("feat/docs", &[], &worktrees)
                .unwrap()
                .path,
            PathBuf::from("/repo-worktrees/review-docs")
        );
        assert_eq!(
            resolve_pr_lane(69, &worktrees).unwrap().worktree_name,
            "review-docs"
        );
    }

    #[test]
    fn pr_resolution_falls_back_to_conventional_name() {
        let worktrees = vec![worktree("pr-42", "review", None)];

        assert_eq!(
            resolve_pr_lane(42, &worktrees).unwrap().worktree_name,
            "pr-42"
        );
    }

    #[test]
    fn bare_resume_inside_a_worktree_resolves_that_lane() {
        let worktrees = vec![worktree("docs", "feat/docs", None)];

        let lane = resolve_cwd_lane(
            Path::new("/repo-worktrees/docs"),
            Path::new("/repo"),
            &[],
            &worktrees,
        )
        .expect("cwd lane");

        assert_eq!(lane.path, PathBuf::from("/repo-worktrees/docs"));
        assert_eq!(lane.worktree_name, "docs");
    }

    #[test]
    fn lane_errors_include_the_fix() {
        let unknown = resolve_scope_lane("#nope", &[], &[]).unwrap_err();
        assert_eq!(unknown.to_string(), "no lane '#nope' in this workspace");

        let lane = ResolvedLane {
            display: "#docs".to_owned(),
            path: PathBuf::from("/gone/docs"),
            channel: Some("docs".to_owned()),
            worktree_name: "docs".to_owned(),
        };
        assert_eq!(
            ensure_lane_exists(&lane, |_| false)
                .unwrap_err()
                .to_string(),
            "worktree for '#docs' was removed; recreate it with rimz agents <spec> -w docs"
        );
        assert_eq!(
            resolve_pr_lane(69, &[]).unwrap_err().to_string(),
            "PR 69 has no local worktree; start one with rimz agents <spec> --from-pr 69"
        );
        assert_eq!(
            ResumeLaneErr::Nothing {
                scope: "#docs".to_owned()
            }
            .to_string(),
            "nothing to resume in '#docs'"
        );
    }

    #[test]
    fn liveness_split_keeps_only_closed_members_for_partial_resume() {
        let agents = vec![
            agent("claude", "live", "/repo-worktrees/docs", Some("docs")),
            agent("codex", "closed", "/repo-worktrees/docs", Some("docs")),
        ];
        let split = split_lane_liveness(&agents, |agent| {
            if agent.agent_id.as_str() == "live" {
                AgentLiveness::Live { pid: 7 }
            } else {
                AgentLiveness::Dead
            }
        });

        assert_eq!(split.live.len(), 1);
        assert_eq!(split.closed.len(), 1);
        assert_eq!(split.closed[0].agent_id.as_str(), "closed");
    }

    #[test]
    fn paneless_closed_agent_is_a_lane_candidate() {
        let mut closed = agent("claude", "closed", "/repo-worktrees/docs", Some("docs"));
        closed.pane = None;
        let lane = ResolvedLane {
            display: "#docs".to_owned(),
            path: PathBuf::from("/repo-worktrees/docs"),
            channel: Some("docs".to_owned()),
            worktree_name: "docs".to_owned(),
        };

        let candidates = current_lane_candidates(&[closed], &lane);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].agent_id.as_str(), "closed");
    }

    #[test]
    fn native_discovery_fallback_requires_missing_durable_conversations() {
        let empty = LaneLiveness {
            live: Vec::new(),
            closed: Vec::new(),
        };
        assert!(should_discover_lane_sessions(&[], &empty));

        let mut missing = agent("claude", "missing", "/repo-worktrees/docs", Some("docs"));
        missing.transcript_path = Some("/missing/transcript.jsonl".to_owned());
        let closed_missing = LaneLiveness {
            live: Vec::new(),
            closed: vec![missing.clone()],
        };
        assert!(should_discover_lane_sessions(
            std::slice::from_ref(&missing),
            &closed_missing
        ));

        let present = agent("claude", "present", "/repo-worktrees/docs", Some("docs"));
        let closed_present = LaneLiveness {
            live: Vec::new(),
            closed: vec![present.clone()],
        };
        assert!(!should_discover_lane_sessions(
            std::slice::from_ref(&present),
            &closed_present
        ));

        let with_live = LaneLiveness {
            live: vec![present],
            closed: vec![missing.clone()],
        };
        assert!(!should_discover_lane_sessions(&[missing], &with_live));
    }

    #[test]
    fn all_closed_team_and_stray_plan_as_team_tab_plus_flat_pane() {
        let mut planner = agent("claude", "planner", "/repo-worktrees/docs", Some("docs"));
        planner.team = Some("forge".to_owned());
        planner.role = Some("planner".to_owned());
        let mut coder = agent("codex", "coder", "/repo-worktrees/docs", Some("docs"));
        coder.team = Some("forge".to_owned());
        coder.role = Some("coder".to_owned());
        let stray = agent("codex", "stray", "/repo-worktrees/docs", Some("docs"));
        let agents = vec![planner, coder, stray];
        let mut profiles = rimz::config::ProfilesConfig::default();
        profiles.0.insert(
            "claude-plan".to_owned(),
            rimz::config::Profile {
                agent: "claude".to_owned(),
                mode: None,
                model: None,
                effort: None,
                budget: None,
                system_prompt_file: None,
                append_system_prompt_file: None,
                args: None,
            },
        );
        profiles.0.insert(
            "codex-code".to_owned(),
            rimz::config::Profile {
                agent: "codex".to_owned(),
                mode: None,
                model: None,
                effort: None,
                budget: None,
                system_prompt_file: None,
                append_system_prompt_file: None,
                args: None,
            },
        );
        let mut teams = rimz::config::TeamsConfig::default();
        teams.0.insert(
            "forge".to_owned(),
            rimz::config::Team {
                roles: vec![
                    rimz::config::RoleBinding {
                        role: "planner".to_owned(),
                        profile: "claude-plan".to_owned(),
                        mode: None,
                        model: None,
                        effort: None,
                        budget: None,
                        system_prompt_file: None,
                        append_system_prompt_file: None,
                        args: None,
                    },
                    rimz::config::RoleBinding {
                        role: "coder".to_owned(),
                        profile: "codex-code".to_owned(),
                        mode: None,
                        model: None,
                        effort: None,
                        budget: None,
                        system_prompt_file: None,
                        append_system_prompt_file: None,
                        args: None,
                    },
                ],
                leader: None,
                layout: Some("planner,coder".to_owned()),
            },
        );
        let commands = rimz::config::CommandsConfig::default();
        let (team, flat_agents) = split_team_and_flat(
            &agents,
            &teams,
            &profiles,
            &commands,
            Some(Path::new("/repo")),
            |_| true,
            |_| true,
        );
        let flat = rimz::harness::resume::plan_resume(
            &flat_agents,
            &BTreeSet::new(),
            128,
            Some(Path::new("/repo")),
            |_| true,
            |_| true,
            Path::new("/bin/rimz"),
        );

        assert_eq!(team.len(), 1);
        assert_eq!(flat.tabs.len(), 1);
        assert_eq!(flat.tabs[0].pane_count(), 1);
    }

    #[test]
    fn summary_reports_live_and_closed_counts() {
        let mut live = agent("claude", "live", "/repo-worktrees/docs", Some("docs"));
        live.runtime_owner = Some(RuntimeOwner::new(RuntimeOwnerKind::Agent, "live", 7, None));
        let closed = agent("codex", "closed", "/repo-worktrees/docs", Some("docs"));
        let summaries = lane_summaries(&[live, closed], Path::new("/repo"), |agent| {
            if agent.agent_id.as_str() == "live" {
                AgentLiveness::Live { pid: 7 }
            } else {
                AgentLiveness::Dead
            }
        });

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].label, "#docs");
        assert_eq!(summaries[0].members, 2);
        assert_eq!(summaries[0].live, 1);
    }

    #[test]
    fn native_discovery_adds_only_storeless_worktree_lanes() {
        let store_summary = LaneSummary {
            path: PathBuf::from("/repo-worktrees/docs"),
            label: "#docs".to_owned(),
            members: 1,
            live: 0,
            freshest: "2025-01-03T00:00:00Z".parse().unwrap(),
        };
        let docs = worktree("docs", "feat/docs", None);
        let native = worktree("native", "feat/native", None);
        let discovered = vec![
            (
                docs,
                vec![local_observation(
                    "claude",
                    "ignored",
                    "/repo-worktrees/docs",
                    "2025-01-04T09:00:00Z",
                    "2025-01-04T10:00:00Z",
                )],
            ),
            (
                native,
                vec![
                    local_observation(
                        "claude",
                        "older-run",
                        "/repo-worktrees/native",
                        "2025-01-01T09:00:00Z",
                        "2025-01-01T10:00:00Z",
                    ),
                    local_observation(
                        "claude",
                        "planner",
                        "/repo-worktrees/native",
                        "2025-01-04T09:00:00Z",
                        "2025-01-04T11:00:00Z",
                    ),
                    local_observation(
                        "codex",
                        "coder",
                        "/repo-worktrees/native",
                        "2025-01-04T10:00:00Z",
                        "2025-01-04T12:00:00Z",
                    ),
                ],
            ),
        ];

        let summaries = merge_native_lane_summaries(vec![store_summary], discovered);

        assert_eq!(summaries.len(), 2);
        let native = summaries
            .iter()
            .find(|summary| summary.label == "#native")
            .unwrap();
        assert_eq!(native.members, 2);
        assert_eq!(native.live, 0);
    }
}
