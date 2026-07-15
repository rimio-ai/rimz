//! Launch and rebirth resume planning: turn the durable agent rollup into flat
//! or team tabs a session re-seeds.
//!
//! When the CLI admits agent recovery for a reborn room — a machine reboot or
//! mux crash — the agents' processes are gone, but the store remembers them.
//! The caller scopes the audit rollup to the producer's persisted live roster,
//! then this module plans one `#channel` tab per worktree, with one resume pane
//! per prior root agent, so the next birth can recover where the user left off
//! instead of empty.
//!
//! Planning stays pure over supplied rollups and filesystem predicates. Team
//! materialization is the shared durable launch boundary used by room rebirth
//! and explicit lane resume; the mux still receives only compiled tabs.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;

use crate::Store;
use crate::agents::find_adapter;
use crate::agents::{AgentState, LocalSessionObservation};
use crate::config::{CommandsConfig, ProfilesConfig, TeamsConfig};
use crate::harness::plan::{
    LayoutPaneParams, cohort_cells, fresh_resume_launch_requests, layout_panes_with_names,
};
use crate::harness::spec::LayoutSpec;
use crate::ids::{AgentKind, AgentSessionId, PaneId};
use crate::mux::ResumeTab;
use crate::store::AgentLaunchScope;
use crate::store::runtime::AgentLiveness;

/// The default ceiling on agents auto-resumed into one reborn session, so a
/// long-lived workspace cannot fork-bomb a fleet of agent processes on birth.
/// Anything past it is reported, never silently dropped.
pub const DEFAULT_RESUME_MAX: usize = 128;

/// Why a candidate agent was not resumed — surfaced in the start report so a
/// skipped agent stays visible rather than silently lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeSkipReason {
    /// The agent's kind has no resume CLI ([`crate::agents::AgentAdapter::resume_command`]).
    NoResumeSupport,
    /// The session id names a conversation the provider never persisted, so
    /// there is nothing to resume.
    NoConversation,
    /// Dropped to stay within the resume cap.
    OverCap,
}

impl ResumeSkipReason {
    pub const fn label(self) -> &'static str {
        match self {
            Self::NoResumeSupport => "no resume CLI",
            Self::NoConversation => "no saved conversation",
            Self::OverCap => "over the resume cap",
        }
    }
}

/// A candidate that the planner deliberately did not resume, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeSkip {
    pub label: String,
    pub reason: ResumeSkipReason,
}

/// What a reborn session should re-seed, and what it deliberately left out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumePlan {
    /// The tabs to seed, ordered by their freshest pane activity (the lead is
    /// the focus target). Panes inside each tab are freshest-first.
    pub tabs: Vec<ResumeTab>,
    /// Candidates not resumed, each with its reason — the start report names them.
    pub skipped: Vec<ResumeSkip>,
    /// Candidates whose worktree disappeared; the caller records these as
    /// durable end traces so they leave the next resume candidate set.
    pub tombstone: Vec<(AgentKind, AgentSessionId)>,
}

/// One cell in an explicit cohort resume spec, reduced to the matching fields
/// the planner needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CohortCell {
    pub kind: AgentKind,
    pub role: Option<String>,
}

/// One agent cell's explicit-resume seed.
#[derive(Clone, Debug, PartialEq)]
pub enum CohortSeed {
    /// Resume a prior provider-native session, replaying its launch identity.
    Resume(Box<AgentState>),
    /// Launch a fresh pane for a cell missing resumable history.
    Fresh,
}

/// Explicit `rimz agents <spec> --resume` plan, parallel to the layout's agent
/// cells.
#[derive(Clone, Debug, PartialEq)]
pub struct CohortResumePlan {
    pub seeds: Vec<CohortSeed>,
    pub cwd: Option<PathBuf>,
    pub channel: Option<String>,
    pub fresh: Vec<String>,
    pub launch_group: Option<String>,
}

/// One named-team tab selected for restore and awaiting launch materialization.
#[derive(Clone, Debug)]
pub struct PlannedTeamTab {
    pub label: String,
    pub cwd: PathBuf,
    pub channel: Option<String>,
    pub team: String,
    pub layout: LayoutSpec,
    pub cohort: CohortResumePlan,
    pub freshest: Timestamp,
}

/// Why explicit cohort resume cannot proceed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CohortResumeErr {
    NothingToResume { spec: String },
    MembersStillLive { labels: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ResumeTabIdentity {
    Channel(String),
    Cwd(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlannedResumeTab {
    identity: ResumeTabIdentity,
    tab: ResumeTab,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ResumeCandidateKey {
    Pane(PaneId),
    Session(AgentKind, AgentSessionId),
}

impl ResumePlan {
    /// Whether there is nothing to seed — the birth is exactly the bare working
    /// room.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

/// Select the transitive overlap cluster containing the newest native session.
///
/// Provider sessions are intervals from creation through last activity. The
/// newest merged cluster is the last concurrent working set; older disjoint
/// clusters are returned separately so callers can report what stayed closed.
pub fn concurrent_session_set(
    mut observations: Vec<LocalSessionObservation>,
) -> (Vec<LocalSessionObservation>, Vec<LocalSessionObservation>) {
    observations.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    let mut clusters: Vec<(Timestamp, Vec<LocalSessionObservation>)> = Vec::new();
    for observation in observations {
        let end = observation.last_activity.max(observation.created_at);
        if let Some((cluster_end, members)) = clusters.last_mut()
            && observation.created_at <= *cluster_end
        {
            *cluster_end = (*cluster_end).max(end);
            members.push(observation);
        } else {
            clusters.push((end, vec![observation]));
        }
    }
    let Some(selected) = clusters
        .iter()
        .enumerate()
        .max_by_key(|(_, (end, _))| *end)
        .map(|(index, _)| index)
    else {
        return (Vec::new(), Vec::new());
    };
    let mut resume = clusters.remove(selected).1;
    let mut skipped = clusters
        .into_iter()
        .flat_map(|(_, members)| members)
        .collect::<Vec<_>>();
    let newest_first = |left: &LocalSessionObservation, right: &LocalSessionObservation| {
        right
            .last_activity
            .cmp(&left.last_activity)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.session_id.cmp(&right.session_id))
    };
    resume.sort_by(newest_first);
    skipped.sort_by(newest_first);
    (resume, skipped)
}

/// Synthesize the minimal durable shape the existing flat resume planner reads.
pub fn discovered_agent_state(
    observation: &LocalSessionObservation,
    channel: Option<&str>,
) -> AgentState {
    AgentState {
        agent_id: observation.session_id.clone(),
        kind: observation.kind.clone(),
        name: None,
        name_explicit: false,
        kind_ordinal: None,
        profile: None,
        mode: None,
        role: None,
        team: None,
        launch_group: None,
        launch_ordinal: None,
        channel: channel
            .filter(|channel| !channel.is_empty())
            .map(ToOwned::to_owned),
        status: observation.status,
        phase: observation.phase,
        pane: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: Some(observation.workspace.to_string_lossy().into_owned()),
        worktree_branch: None,
        task: None,
        prompt: None,
        description: None,
        transcript_path: Some(observation.transcript_path.to_string_lossy().into_owned()),
        origin: None,
        recent_prompts: Vec::new(),
        model: None,
        effort: None,
        budget: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        cache_write_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        context: None,
        budget_park: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        waiting_since: None,
        open_ask: None,
        compacting_since: None,
        compaction_count: 0,
        last_compact_command_tokens: None,
        last_seen: observation.last_activity,
        last_activity: observation.last_activity,
        registered_at: Some(observation.created_at),
    }
}

/// Allocate fresh team members and compile one planned team restore tab.
pub fn materialize_team_restore_tab(
    store: &Store,
    session_name: &str,
    teams: &TeamsConfig,
    planned: &PlannedTeamTab,
) -> anyhow::Result<ResumeTab> {
    use anyhow::Context;

    let team_roles = teams.0.get(&planned.team).map(|team| team.roles.as_slice());
    let launch_requests = fresh_resume_launch_requests(
        &planned.layout,
        &planned.cohort,
        Some(&planned.team),
        team_roles,
        planned.channel.as_deref(),
    )?;
    let batch = if launch_requests.is_empty() {
        None
    } else {
        Some(store.begin_agent_launch_batch(
            &launch_requests,
            AgentLaunchScope {
                session_name: session_name.to_owned(),
                cwd: planned.cwd.clone(),
                worktree_name: None,
                channel: planned.channel.clone(),
                description: None,
            },
        )?)
    };
    let layout = layout_panes_with_names(
        &planned.layout,
        LayoutPaneParams {
            cwd: &planned.cwd,
            prompt: None,
            prompt_agent_index: None,
            cleanup_worktree: false,
            in_place: false,
            team: Some(&planned.team),
            channel: planned.channel.as_deref(),
            resume_seeds: Some(&planned.cohort.seeds),
        },
        batch.as_ref().map_or(&[], |batch| batch.identities()),
    )
    .context("building team restore layout")?;
    Ok(ResumeTab {
        label: planned.label.clone(),
        cwd: planned.cwd.clone(),
        layout,
    })
}

/// Plan restorable named-team tabs from prior root agents.
pub fn plan_team_restore_tabs(
    agents: &[AgentState],
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    project_root: Option<&Path>,
    worktree_exists: impl Fn(&Path) -> bool,
    session_backed: impl Fn(&AgentState) -> bool,
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
        let Ok(layout) = crate::harness::spec::resolve_team(&team, teams, profiles, commands)
        else {
            continue;
        };
        let cells = cohort_cells(&layout);
        let group_agents = group.iter().copied().cloned().collect::<Vec<_>>();
        let Ok(mut cohort) = plan_cohort_resume(
            &group_agents,
            |_| AgentLiveness::Dead,
            &cells,
            Some(&team),
            |path| worktree_exists(path),
            &session_backed,
        ) else {
            continue;
        };
        let Some(newest) = newest_agent(&group) else {
            continue;
        };
        let channel = project_root
            .and_then(|project_root| {
                crate::harness::target::resolve_room_channel(
                    project_root,
                    &cwd,
                    Some(&team),
                    cohort.channel.as_deref(),
                )
            })
            .or_else(|| cohort.channel.clone());
        cohort.channel = channel.clone();
        let label = channel_label(channel.as_deref(), &cwd);
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

/// Partition agents into planned named-team tabs and flat resume candidates.
pub fn split_team_and_flat(
    agents: &[AgentState],
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    project_root: Option<&Path>,
    worktree_exists: impl Fn(&Path) -> bool,
    session_backed: impl Fn(&AgentState) -> bool,
) -> (Vec<PlannedTeamTab>, Vec<AgentState>) {
    let team = plan_team_restore_tabs(
        agents,
        teams,
        profiles,
        commands,
        project_root,
        worktree_exists,
        session_backed,
    );
    let flat = agents
        .iter()
        .filter(|agent| {
            !team
                .iter()
                .any(|planned| planned_team_matches_agent(planned, agent))
        })
        .cloned()
        .collect();
    (team, flat)
}

pub fn planned_team_matches_agent(planned: &PlannedTeamTab, agent: &AgentState) -> bool {
    agent.team.as_deref() == Some(planned.team.as_str())
        && normalized_agent_worktree(agent).as_deref() == Some(planned.cwd.as_path())
}

fn normalized_agent_worktree(agent: &AgentState) -> Option<PathBuf> {
    agent
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(Path::new)
        .map(crate::worktree::normalize_path_lexical)
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

/// Plan the resume seeds for one reborn session from the durable agent rollup.
///
/// `agents` is the audit rollup (non-tombstoned durable agents; the persisted
/// live roster protects crash candidates from the write-path reap until
/// planning completes); `ended` is the `(kind, agent_id)` set the user closed
/// cleanly from [`crate::RuntimeProjection::ended`]; `max` caps the auto-launched panes;
/// `worktree_exists` decides whether a candidate's worktree
/// is still on disk (production passes `|p| p.is_dir()`); `rimz_bin` is the
/// `rimz` executable each pane's wrapper argv names (production passes
/// `std::env::current_exe()`).
///
/// A candidate qualifies when it is in the caller-supplied roster scope, is a
/// root agent (subagents ride their parent), still carries a session id and a
/// worktree, and was not cleanly ended. A pane stamp identifies the incarnation
/// being replaced when present; a `session.rebirth` boundary retires old stamps,
/// so an unstamped durable candidate remains resumable and dedupes by provider
/// session identity. One pane hosts one agent: a relaunch that re-used a pane id
/// collapses to its newest stamp — the same rule the live sidebar binds by
/// (`stamped_agent_for_pane`, in `store::snapshot::panes`) — while distinct
/// sessions without stamps each remain candidates.
pub fn plan_resume(
    agents: &[AgentState],
    ended: &BTreeSet<(AgentKind, AgentSessionId)>,
    max: usize,
    project_root: Option<&Path>,
    worktree_exists: impl Fn(&Path) -> bool,
    session_backed: impl Fn(&AgentState) -> bool,
    rimz_bin: &Path,
) -> ResumePlan {
    // Root agents that are still identified and not cleanly ended. Subagents
    // ride their parent, so their lack of a pane never makes them standalone
    // resume candidates.
    let mut candidates: Vec<&AgentState> = agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| {
            agent
                .worktree_path
                .as_deref()
                .is_some_and(|path| !path.is_empty())
        })
        .filter(|agent| !ended.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .collect();

    // Most-recently-active first (deterministic on ties), so the newest session
    // wins supersession, the lead pane is the focus target, and the cap keeps
    // the freshest agents.
    candidates.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.agent_id.cmp(&b.agent_id))
    });

    let mut seen: HashSet<ResumeCandidateKey> = HashSet::new();
    let mut plan = ResumePlan::default();
    let mut tabs: Vec<PlannedResumeTab> = Vec::new();
    for agent in candidates {
        // An older relaunch that re-used a pane is superseded by the newest
        // stamp. Rebirth-retired stamps fall back to provider session identity.
        let key = agent.pane.as_ref().map_or_else(
            || ResumeCandidateKey::Session(agent.kind.clone(), agent.agent_id.clone()),
            |pane| ResumeCandidateKey::Pane(pane.pane_id.clone()),
        );
        if !seen.insert(key) {
            continue;
        }
        let worktree = agent.worktree_path.clone().unwrap_or_default();
        let cwd = PathBuf::from(&worktree);
        let channel = agent_room_channel(project_root, agent, &cwd);
        let label = build_label(&agent.kind, channel.as_deref(), &cwd);
        if !worktree_exists(&cwd) {
            plan.tombstone
                .push((agent.kind.clone(), agent.agent_id.clone()));
            continue;
        }
        if !supports_agent_resume(agent) {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::NoResumeSupport,
            });
            continue;
        }
        if !session_backed(agent) {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::NoConversation,
            });
            continue;
        }
        let seeded = tabs.iter().map(|tab| tab.tab.pane_count()).sum::<usize>();
        if seeded >= max {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::OverCap,
            });
            continue;
        }
        // The pane runs the supervised exec wrapper, not the agent CLI
        // directly: every agent launch funnels through `rimz agents exec`,
        // which replays the durable launch identity, applies trusted
        // `[[agents]]` env and the adapter's launch pins before spawning the
        // resume argv.
        let command = resume_command(rimz_bin, agent, channel.as_deref());
        let tab_label = channel_label(channel.as_deref(), &cwd);
        let identity = resume_tab_identity(channel.as_deref(), &cwd);
        if let Some(tab) = tabs.iter_mut().find(|tab| tab.identity == identity) {
            if let Some(column) = tab.tab.layout.columns.first_mut() {
                column.panes.push(crate::mux::PaneCmd { argv: command });
            }
        } else {
            tabs.push(PlannedResumeTab {
                identity,
                tab: ResumeTab::flat(tab_label, cwd, vec![command]),
            });
        }
    }
    disambiguate_resume_tab_labels(&mut tabs);
    plan.tabs = tabs.into_iter().map(|planned| planned.tab).collect();
    plan
}

fn agent_room_channel(
    project_root: Option<&Path>,
    agent: &AgentState,
    cwd: &Path,
) -> Option<String> {
    match project_root {
        Some(project_root) => crate::harness::target::resolve_room_channel(
            project_root,
            cwd,
            agent.team.as_deref(),
            agent.channel.as_deref(),
        ),
        None => agent
            .channel
            .as_deref()
            .filter(|channel| !channel.is_empty())
            .map(ToOwned::to_owned),
    }
}

/// Plan one explicit cohort relaunch from the durable agent rollup.
///
/// `cells` is the launch layout's agent cells in display order. `team` is the
/// named-team spec, when the launch resolved one. The caller supplies liveness
/// and worktree existence so the matching rules stay pure and testable.
pub fn plan_cohort_resume(
    agents: &[AgentState],
    liveness: impl Fn(&AgentState) -> AgentLiveness,
    cells: &[CohortCell],
    team: Option<&str>,
    worktree_exists: impl Fn(&Path) -> bool,
    session_backed: impl Fn(&AgentState) -> bool,
) -> Result<CohortResumePlan, CohortResumeErr> {
    let spec = cohort_spec_label(cells, team);
    let candidates = cohort_candidates(agents, worktree_exists);
    let matches = match_cohort(&candidates, cells, team);

    let matched_any = matches.iter().any(Option::is_some);
    if !matched_any {
        return Err(CohortResumeErr::NothingToResume { spec });
    }

    let live = matches
        .iter()
        .flatten()
        .copied()
        .filter(|agent| matches!(liveness(agent), AgentLiveness::Live { .. }))
        .map(cohort_agent_label)
        .collect::<Vec<_>>();
    if !live.is_empty() {
        return Err(CohortResumeErr::MembersStillLive { labels: live });
    }

    let newest = matches
        .iter()
        .flatten()
        .copied()
        .min_by(cohort_newest_cmp)
        .expect("matched_any guarantees one matched member");
    let cwd = agent_worktree(newest);
    let channel = newest
        .channel
        .as_deref()
        .filter(|channel| !channel.is_empty())
        .map(ToOwned::to_owned);
    let launch_group = newest
        .launch_group
        .as_deref()
        .filter(|group| !group.is_empty())
        .map(ToOwned::to_owned);

    let mut seeds = Vec::with_capacity(cells.len());
    let mut fresh = Vec::new();
    for (index, cell) in cells.iter().enumerate() {
        let matched = matches[index];
        let seed = match matched {
            Some(agent) if supports_agent_resume(agent) && session_backed(agent) => {
                CohortSeed::Resume(Box::new(agent.clone()))
            }
            Some(agent) => {
                fresh.push(cohort_fresh_label_for_agent(agent));
                CohortSeed::Fresh
            }
            None => {
                let label = cwd
                    .as_deref()
                    .map(|cwd| build_label(cell.kind.as_str(), channel.as_deref(), cwd))
                    .unwrap_or_else(|| cell.kind.as_str().to_owned());
                fresh.push(label);
                CohortSeed::Fresh
            }
        };
        seeds.push(seed);
    }

    Ok(CohortResumePlan {
        seeds,
        cwd,
        channel,
        fresh,
        launch_group,
    })
}

fn cohort_spec_label(cells: &[CohortCell], team: Option<&str>) -> String {
    if let Some(team) = team {
        return team.to_owned();
    }
    let kinds = cells
        .iter()
        .map(|cell| cell.kind.as_str())
        .collect::<Vec<_>>()
        .join(",");
    if kinds.is_empty() {
        "<empty>".to_owned()
    } else {
        kinds
    }
}

fn cohort_candidates(
    agents: &[AgentState],
    worktree_exists: impl Fn(&Path) -> bool,
) -> Vec<&AgentState> {
    agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| !agent.agent_id.is_empty())
        .filter(|agent| agent_worktree(agent).is_some_and(|path| worktree_exists(&path)))
        .collect()
}

/// Match one launch layout to its newest prior cohort.
///
/// Named teams match by team and role, single-agent inline specs match by kind,
/// and multi-agent inline specs match by launch group and cell identity.
pub fn match_cohort<'a>(
    candidates: &[&'a AgentState],
    cells: &[CohortCell],
    team: Option<&str>,
) -> Vec<Option<&'a AgentState>> {
    let mut candidates = candidates.to_vec();
    candidates.sort_by(cohort_newest_cmp);
    match (team, cells.len()) {
        (Some(team), _) => match_team_cohort(&candidates, cells, team),
        (None, 1) => match_single_cohort(&candidates, &cells[0]),
        (None, _) => match_inline_cohort(&candidates, cells),
    }
}

fn match_team_cohort<'a>(
    candidates: &[&'a AgentState],
    cells: &[CohortCell],
    team: &str,
) -> Vec<Option<&'a AgentState>> {
    let pool = candidates
        .iter()
        .copied()
        .filter(|agent| agent.team.as_deref() == Some(team))
        .collect::<Vec<_>>();
    let mut claimed = BTreeSet::new();
    let mut matches = Vec::with_capacity(cells.len());
    for cell in cells {
        let agent = if let Some(role) = cell.role.as_deref() {
            pool.iter().copied().find(|agent| {
                !claimed.contains(&agent.agent_id) && agent.role.as_deref() == Some(role)
            })
        } else {
            pool.iter()
                .copied()
                .find(|agent| !claimed.contains(&agent.agent_id) && agent.kind == cell.kind)
        };
        if let Some(agent) = agent {
            claimed.insert(agent.agent_id.clone());
        }
        matches.push(agent);
    }
    matches
}

fn match_single_cohort<'a>(
    candidates: &[&'a AgentState],
    cell: &CohortCell,
) -> Vec<Option<&'a AgentState>> {
    vec![
        candidates
            .iter()
            .copied()
            .find(|agent| agent.kind == cell.kind),
    ]
}

fn match_inline_cohort<'a>(
    candidates: &[&'a AgentState],
    cells: &[CohortCell],
) -> Vec<Option<&'a AgentState>> {
    let mut groups: BTreeMap<&str, Vec<&'a AgentState>> = BTreeMap::new();
    for agent in candidates {
        if let Some(group) = agent
            .launch_group
            .as_deref()
            .filter(|group| !group.is_empty())
        {
            groups.entry(group).or_default().push(*agent);
        }
    }
    let mut groups = groups.into_values().collect::<Vec<_>>();
    groups.sort_by(|a, b| {
        let newest_a = a
            .iter()
            .copied()
            .min_by(cohort_newest_cmp)
            .expect("group is non-empty");
        let newest_b = b
            .iter()
            .copied()
            .min_by(cohort_newest_cmp)
            .expect("group is non-empty");
        cohort_newest_cmp(&newest_a, &newest_b)
    });

    for group in groups {
        let matches = map_inline_group_to_cells(&group, cells);
        if matches.iter().any(Option::is_some) {
            return matches;
        }
    }
    vec![None; cells.len()]
}

fn map_inline_group_to_cells<'a>(
    group: &[&'a AgentState],
    cells: &[CohortCell],
) -> Vec<Option<&'a AgentState>> {
    let mut matches = vec![None; cells.len()];
    let mut claimed = BTreeSet::new();

    for agent in group {
        let Some(ordinal) = agent
            .launch_ordinal
            .and_then(|ordinal| usize::try_from(ordinal).ok())
        else {
            continue;
        };
        if ordinal >= cells.len() || matches[ordinal].is_some() {
            continue;
        }
        matches[ordinal] = Some(*agent);
        claimed.insert(agent.agent_id.clone());
    }

    for agent in group {
        if claimed.contains(&agent.agent_id) {
            continue;
        }
        let Some(role) = agent.role.as_deref() else {
            continue;
        };
        let Some(index) = cells
            .iter()
            .enumerate()
            .find(|(index, cell)| {
                matches[*index].is_none()
                    && cell.kind == agent.kind
                    && cell.role.as_deref() == Some(role)
            })
            .map(|(index, _)| index)
        else {
            continue;
        };
        matches[index] = Some(*agent);
        claimed.insert(agent.agent_id.clone());
    }

    for agent in group {
        if claimed.contains(&agent.agent_id) {
            continue;
        }
        let Some(index) = cells
            .iter()
            .enumerate()
            .find(|(index, cell)| matches[*index].is_none() && cell.kind == agent.kind)
            .map(|(index, _)| index)
        else {
            continue;
        };
        matches[index] = Some(*agent);
        claimed.insert(agent.agent_id.clone());
    }

    matches
}

fn supports_agent_resume(agent: &AgentState) -> bool {
    // A provisional `launch_...` id only names Rimz's pre-adoption placeholder.
    // Keep the matched cohort cell and relaunch it fresh instead of asking the
    // adapter to resume an id outside the provider session store.
    if agent.agent_id.is_provisional() {
        return false;
    }
    let Some(cwd) = agent_worktree(agent) else {
        return false;
    };
    find_adapter(&agent.kind)
        .is_some_and(|adapter| adapter.resume_command(&agent.agent_id, &cwd).is_some())
}

/// A resumed agent must have a conversation on disk. Claude and Codex stamp a
/// `transcript_path` on their first `SessionStart` hook, before any prompt, so a
/// never-answered session carries an id and a path to a file that was never
/// written. Require a recorded transcript to exist and be non-empty; treat an
/// unreported path (Pi, OpenCode) as present so their resume is unchanged.
pub fn resume_session_present(agent: &AgentState) -> bool {
    match agent
        .transcript_path
        .as_deref()
        .filter(|path| !path.is_empty())
    {
        Some(path) => std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() > 0),
        None => true,
    }
}

fn cohort_newest_cmp(a: &&AgentState, b: &&AgentState) -> std::cmp::Ordering {
    b.last_activity
        .cmp(&a.last_activity)
        .then_with(|| a.agent_id.cmp(&b.agent_id))
}

fn agent_worktree(agent: &AgentState) -> Option<PathBuf> {
    agent
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

fn cohort_agent_label(agent: &AgentState) -> String {
    match agent.name.as_deref().filter(|name| !name.is_empty()) {
        Some(name) => format!("{}:{} ({})", agent.kind.as_str(), name, agent.agent_id),
        None => format!("{}:{}", agent.kind.as_str(), agent.agent_id),
    }
}

fn cohort_fresh_label_for_agent(agent: &AgentState) -> String {
    let cwd = agent_worktree(agent).unwrap_or_default();
    build_label(agent.kind.as_str(), agent.channel.as_deref(), &cwd)
}

fn resume_tab_identity(channel: Option<&str>, cwd: &Path) -> ResumeTabIdentity {
    match channel.filter(|channel| !channel.is_empty()) {
        Some(channel) => ResumeTabIdentity::Channel(channel.to_owned()),
        None => ResumeTabIdentity::Cwd(cwd.to_path_buf()),
    }
}

fn disambiguate_resume_tab_labels(tabs: &mut [PlannedResumeTab]) {
    let mut label_counts = BTreeMap::new();
    for planned in tabs.iter() {
        *label_counts.entry(planned.tab.label.clone()).or_insert(0) += 1;
    }
    let relabel: BTreeSet<usize> = tabs
        .iter()
        .enumerate()
        .filter(|(_, planned)| label_counts[&planned.tab.label] > 1)
        .filter(|(_, planned)| matches!(planned.identity, ResumeTabIdentity::Cwd(_)))
        .map(|(index, _)| index)
        .collect();
    if relabel.is_empty() {
        return;
    }

    let mut used: HashSet<String> = tabs
        .iter()
        .enumerate()
        .filter(|(index, _)| !relabel.contains(index))
        .map(|(_, planned)| planned.tab.label.clone())
        .collect();
    for index in relabel {
        let base = parent_prefixed_label(&tabs[index].tab.cwd)
            .unwrap_or_else(|| tabs[index].tab.label.clone());
        tabs[index].tab.label = unique_label(&base, &mut used);
    }
}

fn parent_prefixed_label(cwd: &Path) -> Option<String> {
    let child = cwd.file_name()?.to_string_lossy();
    let parent = cwd.parent()?.file_name()?.to_string_lossy();
    Some(format!("#{parent}/{child}"))
}

fn unique_label(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_owned()) {
        return base.to_owned();
    }
    for ordinal in 2.. {
        let candidate = format!("{base}-{ordinal}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("unbounded ordinal search always yields a fresh label")
}

/// Wrapper argv for resuming one prior provider-native session with its durable
/// Rimz launch identity and rebirth channel.
pub fn resume_command(rimz_bin: &Path, agent: &AgentState, channel: Option<&str>) -> Vec<String> {
    let channel = agent
        .channel
        .as_deref()
        .filter(|channel| !channel.is_empty())
        .or_else(|| channel.filter(|channel| !channel.is_empty()));
    crate::harness::launch::exec_argv(
        rimz_bin,
        &crate::harness::launch::ExecInvocation {
            kind: agent.kind.as_str(),
            action: crate::harness::launch::ExecAction::Resume {
                session_id: agent.agent_id.as_str(),
                extra_args: &[],
            },
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            identity: crate::harness::launch::ExecIdentity {
                name: agent.name.as_deref(),
                name_explicit: agent.name_explicit,
                profile: agent.profile.as_deref(),
                mode: None,
                role: agent.role.as_deref(),
                team: agent.team.as_deref(),
                launch_group: agent.launch_group.as_deref(),
                launch_ordinal: agent.launch_ordinal,
                channel,
                // Resume did not replay model/effort before the wrapper grammar
                // moved here; keep argv behavior stable.
                model: None,
                effort: None,
                ..crate::harness::launch::ExecIdentity::default()
            },
        },
    )
}

/// A short, view-safe label for a resumed agent: `kind:<channel>`, falling back
/// to the worktree directory name, then `kind:agent`. Used in skip reports and
/// legacy per-agent tab title fallbacks.
pub fn build_label(kind: &str, channel: Option<&str>, worktree: &Path) -> String {
    format!("{kind}:{}", channel_short(channel, worktree))
}

/// A short, view-safe channel name: explicit channel, then worktree directory,
/// then `agent`.
pub fn channel_short(channel: Option<&str>, worktree: &Path) -> String {
    channel
        .filter(|channel| !channel.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            worktree
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "agent".to_owned())
}

/// A channel tab label from the worktree directory name, matching live
/// worktree-launch tabs. A main-repo non-worktree agent falls back to
/// `#<repo-name>` rather than the live `kind:repo` title because resume groups by
/// cwd.
pub fn channel_label(channel: Option<&str>, worktree: &Path) -> String {
    format!("#{}", channel_short(channel, worktree))
}

#[cfg(test)]
mod tests;
