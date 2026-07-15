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

/// One local worktree available to lane resume resolution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneWorktree {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub from_pr: Option<u64>,
}

/// User-facing way to select lane resume work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaneResumeSelector {
    List,
    Scope(String),
    PullRequest(u64),
    Current,
}

/// Effective launch configuration used only when every lane member is closed.
#[derive(Clone, Debug)]
pub struct LaneRestoreConfig {
    pub teams: TeamsConfig,
    pub profiles: ProfilesConfig,
    pub commands: CommandsConfig,
}

/// Pure facts needed to decide one lane resume request.
#[derive(Clone, Debug)]
pub struct LaneResumeRequest<'a> {
    pub selector: LaneResumeSelector,
    pub agents: &'a [AgentState],
    pub worktrees: &'a [LaneWorktree],
    pub current_root: &'a Path,
    pub project_root: &'a Path,
    pub max: usize,
    pub rimz_bin: &'a Path,
}

/// One row in the root-level lane resume listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneSummary {
    pub path: PathBuf,
    pub label: String,
    pub members: usize,
    pub live: usize,
    pub freshest: Timestamp,
}

/// Why lane qualification or planning cannot proceed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LaneResumeError {
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
    #[error("live lane has no focus candidate")]
    LiveNoFocus,
    #[error("live lane agent has no bound pane")]
    LiveNoPane,
    #[error("{message}")]
    RestoreConfig { message: String },
}

/// All-closed lane plan awaiting durable identity allocation.
#[derive(Clone, Debug)]
pub struct LaneRestorePlan {
    teams: TeamsConfig,
    team: Vec<PlannedTeamTab>,
    flat: ResumePlan,
    discovery_skipped: Vec<LocalSessionObservation>,
    preflight_kinds: Vec<AgentKind>,
}

impl LaneRestorePlan {
    pub fn skipped(&self) -> &[ResumeSkip] {
        &self.flat.skipped
    }

    pub fn discovery_skipped(&self) -> &[LocalSessionObservation] {
        &self.discovery_skipped
    }
}

/// Boundary work chosen by lane resume policy.
#[derive(Clone, Debug)]
pub enum LaneResumeAction {
    List {
        lanes: Vec<LaneSummary>,
    },
    Focus {
        lane_label: String,
        pane_id: PaneId,
    },
    SplitClosed {
        lane_label: String,
        cwd: PathBuf,
        channel: Option<String>,
        target_pane_id: PaneId,
        commands: Vec<Vec<String>>,
        skipped: Vec<ResumeSkip>,
        live_labels: Vec<String>,
        preflight_kinds: Vec<AgentKind>,
    },
    RestoreClosed {
        lane_label: String,
        cwd: PathBuf,
        plan: LaneRestorePlan,
    },
}

impl LaneResumeAction {
    /// Provider kinds callers validate before any mux or durable allocation.
    pub fn agent_kinds_needing_preflight(&self) -> &[AgentKind] {
        match self {
            Self::SplitClosed {
                preflight_kinds, ..
            } => preflight_kinds,
            Self::RestoreClosed { plan, .. } => &plan.preflight_kinds,
            Self::List { .. } | Self::Focus { .. } => &[],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedLane {
    display: String,
    path: PathBuf,
    channel: Option<String>,
    worktree_name: String,
}

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

#[derive(Clone, Debug)]
struct ResumeCandidate {
    kind: AgentKind,
    session_id: AgentSessionId,
    name: Option<String>,
    name_explicit: bool,
    profile: Option<String>,
    role: Option<String>,
    team: Option<String>,
    launch_group: Option<String>,
    launch_ordinal: Option<u32>,
    channel: Option<String>,
    cwd: PathBuf,
    pane_id: Option<PaneId>,
    last_activity: Timestamp,
    conversation_present: bool,
}

impl ResumeCandidate {
    fn from_agent(agent: &AgentState, conversation_present: impl FnOnce() -> bool) -> Option<Self> {
        if !root_session(agent) {
            return None;
        }
        Some(Self::from_agent_identity(agent, conversation_present()))
    }

    fn from_agent_identity(agent: &AgentState, conversation_present: bool) -> Self {
        Self {
            kind: agent.kind.clone(),
            session_id: agent.agent_id.clone(),
            name: agent.name.clone(),
            name_explicit: agent.name_explicit,
            profile: agent.profile.clone(),
            role: agent.role.clone(),
            team: agent.team.clone(),
            launch_group: agent.launch_group.clone(),
            launch_ordinal: agent.launch_ordinal,
            channel: agent.channel.clone(),
            cwd: agent_worktree(agent).unwrap_or_default(),
            pane_id: agent.pane.as_ref().map(|pane| pane.pane_id.clone()),
            last_activity: agent.last_activity,
            conversation_present,
        }
    }

    fn from_observation(observation: &LocalSessionObservation) -> Option<Self> {
        if observation.session_id.is_empty() || observation.workspace.as_os_str().is_empty() {
            return None;
        }
        Some(Self {
            kind: observation.kind.clone(),
            session_id: observation.session_id.clone(),
            name: None,
            name_explicit: false,
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            cwd: observation.workspace.clone(),
            pane_id: None,
            last_activity: observation.last_activity,
            conversation_present: true,
        })
    }

    fn key(&self) -> ResumeCandidateKey {
        resume_candidate_key(&self.kind, &self.session_id, self.pane_id.as_ref())
    }
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

/// Resolve, qualify, and plan one place-first lane resume request.
pub fn plan_lane_resume(
    request: LaneResumeRequest<'_>,
    path_exists: impl Fn(&Path) -> bool,
    session_backed: impl Fn(&AgentState) -> bool,
    liveness: impl Fn(&AgentState) -> AgentLiveness,
    mut discover_sessions: impl FnMut(&Path) -> Vec<LocalSessionObservation>,
    restore_config: impl FnOnce() -> Result<LaneRestoreConfig, LaneResumeError>,
) -> Result<LaneResumeAction, LaneResumeError> {
    if matches!(request.selector, LaneResumeSelector::List) {
        let mut lanes = lane_summaries(request.agents, request.project_root, &liveness);
        let durable_paths = lanes
            .iter()
            .map(|summary| summary.path.clone())
            .collect::<HashSet<_>>();
        for worktree in request
            .worktrees
            .iter()
            .filter(|worktree| !durable_paths.contains(&worktree.path))
        {
            let (resume, _) = concurrent_session_set(discover_sessions(&worktree.path));
            let Some(freshest) = resume.iter().map(|session| session.last_activity).max() else {
                continue;
            };
            lanes.push(LaneSummary {
                path: worktree.path.clone(),
                label: format!("#{}", worktree.name),
                members: resume.len(),
                live: 0,
                freshest,
            });
        }
        sort_lane_summaries(&mut lanes);
        return Ok(LaneResumeAction::List { lanes });
    }

    let lane = resolve_lane(&request)?;
    if !path_exists(&lane.path) {
        return Err(LaneResumeError::Removed {
            scope: lane.display,
            worktree: lane.worktree_name,
        });
    }
    let candidates = current_lane_candidates(request.agents, &lane);
    let (live, closed): (Vec<_>, Vec<_>) = candidates
        .iter()
        .cloned()
        .partition(|agent| matches!(liveness(agent), AgentLiveness::Live { .. }));

    if candidates.is_empty() || (live.is_empty() && !closed.iter().any(&session_backed)) {
        return plan_discovered_lane(&request, &lane, discover_sessions(&lane.path), path_exists);
    }
    if closed.is_empty() {
        let agent = live.first().ok_or(LaneResumeError::LiveNoFocus)?;
        let pane = agent.pane.as_ref().ok_or(LaneResumeError::LiveNoPane)?;
        return Ok(LaneResumeAction::Focus {
            lane_label: lane.display,
            pane_id: pane.pane_id.clone(),
        });
    }
    if !closed.iter().any(&session_backed) {
        return Err(LaneResumeError::Nothing {
            scope: lane.display,
        });
    }
    if !live.is_empty() {
        return plan_live_lane_split(
            &request,
            lane,
            candidates,
            live,
            closed,
            path_exists,
            session_backed,
        );
    }
    plan_closed_lane(
        &request,
        lane,
        closed,
        path_exists,
        session_backed,
        restore_config()?,
    )
}

fn resolve_lane(request: &LaneResumeRequest<'_>) -> Result<ResolvedLane, LaneResumeError> {
    match &request.selector {
        LaneResumeSelector::List => unreachable!("list returns before lane resolution"),
        LaneResumeSelector::PullRequest(number) => resolve_pr_lane(*number, request.worktrees),
        LaneResumeSelector::Scope(scope) => {
            resolve_scope_lane(scope, request.agents, request.worktrees)
        }
        LaneResumeSelector::Current => resolve_current_lane(
            request.current_root,
            request.project_root,
            request.agents,
            request.worktrees,
        ),
    }
}

fn resolve_pr_lane(
    number: u64,
    worktrees: &[LaneWorktree],
) -> Result<ResolvedLane, LaneResumeError> {
    let fallback = format!("pr-{number}");
    let worktree = worktrees
        .iter()
        .find(|worktree| worktree.from_pr == Some(number))
        .or_else(|| worktrees.iter().find(|worktree| worktree.name == fallback))
        .ok_or(LaneResumeError::PrNotLocal { number })?;
    Ok(lane_from_worktree(worktree))
}

fn resolve_scope_lane(
    raw_scope: &str,
    agents: &[AgentState],
    worktrees: &[LaneWorktree],
) -> Result<ResolvedLane, LaneResumeError> {
    let scope = raw_scope.strip_prefix('#').unwrap_or(raw_scope);
    if let Some(agent) = agents
        .iter()
        .filter(|agent| root_session(agent))
        .filter(|agent| crate::harness::target::agent_in_worktree(agent, scope))
        .min_by(|left, right| {
            newest_cmp(
                left.last_activity,
                left.agent_id.as_str(),
                right.last_activity,
                right.agent_id.as_str(),
            )
        })
    {
        let path = normalized_agent_worktree(agent).ok_or_else(|| LaneResumeError::Unknown {
            scope: raw_scope.to_owned(),
        })?;
        let channel = crate::harness::target::agent_channel(agent);
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
    Err(LaneResumeError::Unknown {
        scope: raw_scope.to_owned(),
    })
}

fn resolve_current_lane(
    current_root: &Path,
    project_root: &Path,
    agents: &[AgentState],
    worktrees: &[LaneWorktree],
) -> Result<ResolvedLane, LaneResumeError> {
    let current = crate::worktree::normalize_path_lexical(current_root);
    if current == crate::worktree::normalize_path_lexical(project_root) {
        return Err(LaneResumeError::Unknown {
            scope: path_label(&current),
        });
    }
    if let Some(worktree) = worktrees.iter().find(|worktree| worktree.path == current) {
        return Ok(lane_from_worktree(worktree));
    }
    let agent = agents
        .iter()
        .filter(|agent| root_session(agent))
        .filter(|agent| normalized_agent_worktree(agent).as_deref() == Some(current.as_path()))
        .min_by(|left, right| {
            newest_cmp(
                left.last_activity,
                left.agent_id.as_str(),
                right.last_activity,
                right.agent_id.as_str(),
            )
        })
        .ok_or_else(|| LaneResumeError::Unknown {
            scope: path_label(&current),
        })?;
    let channel = crate::harness::target::agent_channel(agent);
    Ok(ResolvedLane {
        display: channel
            .as_deref()
            .map_or_else(|| path_label(&current), |channel| format!("#{channel}")),
        path: current.clone(),
        channel,
        worktree_name: path_label(&current),
    })
}

fn lane_from_worktree(worktree: &LaneWorktree) -> ResolvedLane {
    ResolvedLane {
        display: worktree.name.clone(),
        path: worktree.path.clone(),
        channel: None,
        worktree_name: worktree.name.clone(),
    }
}

fn worktree_matches_scope(worktree: &LaneWorktree, scope: &str) -> bool {
    worktree.name == scope
        || worktree.branch.as_deref() == Some(scope)
        || worktree.path == Path::new(scope)
        || worktree.path.file_name().is_some_and(|name| name == scope)
}

fn current_lane_candidates(agents: &[AgentState], lane: &ResolvedLane) -> Vec<AgentState> {
    let mut candidates = agents
        .iter()
        .filter(|agent| root_session(agent))
        .filter(|agent| normalized_agent_worktree(agent).as_deref() == Some(lane.path.as_path()))
        .filter(|agent| {
            lane.channel.as_deref().is_none_or(|channel| {
                crate::harness::target::agent_channel(agent).as_deref() == Some(channel)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        newest_cmp(
            left.last_activity,
            left.agent_id.as_str(),
            right.last_activity,
            right.agent_id.as_str(),
        )
    });
    let mut seen = HashSet::<ResumeCandidateKey>::new();
    candidates.retain(|agent| {
        let key = resume_candidate_key(
            &agent.kind,
            &agent.agent_id,
            agent.pane.as_ref().map(|pane| &pane.pane_id),
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

fn resume_candidate_key(
    kind: &AgentKind,
    session_id: &AgentSessionId,
    pane_id: Option<&PaneId>,
) -> ResumeCandidateKey {
    pane_id.map_or_else(
        || ResumeCandidateKey::Session(kind.clone(), session_id.clone()),
        |pane_id| ResumeCandidateKey::Pane(pane_id.clone()),
    )
}

fn plan_discovered_lane(
    request: &LaneResumeRequest<'_>,
    lane: &ResolvedLane,
    observations: Vec<LocalSessionObservation>,
    path_exists: impl Fn(&Path) -> bool,
) -> Result<LaneResumeAction, LaneResumeError> {
    if observations.is_empty() {
        return Err(LaneResumeError::Nothing {
            scope: lane.display.clone(),
        });
    }
    let (resume, discovery_skipped) = concurrent_session_set(observations);
    let candidates = resume
        .iter()
        .filter_map(ResumeCandidate::from_observation)
        .collect::<Vec<_>>();
    let preflight_kinds = candidates
        .iter()
        .map(|candidate| candidate.kind.clone())
        .collect();
    let flat = plan_resume_candidates(
        candidates,
        request.max,
        Some(request.project_root),
        path_exists,
        request.rimz_bin,
    );
    if flat.tabs.is_empty() {
        return Err(LaneResumeError::Nothing {
            scope: lane.display.clone(),
        });
    }
    Ok(LaneResumeAction::RestoreClosed {
        lane_label: lane.display.clone(),
        cwd: lane.path.clone(),
        plan: LaneRestorePlan {
            teams: TeamsConfig::default(),
            team: Vec::new(),
            flat,
            discovery_skipped,
            preflight_kinds,
        },
    })
}

fn plan_live_lane_split(
    request: &LaneResumeRequest<'_>,
    lane: ResolvedLane,
    candidates: Vec<AgentState>,
    live: Vec<AgentState>,
    closed: Vec<AgentState>,
    path_exists: impl Fn(&Path) -> bool,
    session_backed: impl Fn(&AgentState) -> bool,
) -> Result<LaneResumeAction, LaneResumeError> {
    let flat = plan_resume(
        &closed,
        &BTreeSet::new(),
        request.max,
        Some(request.project_root),
        path_exists,
        &session_backed,
        request.rimz_bin,
    );
    let commands = flat
        .tabs
        .iter()
        .flat_map(|tab| &tab.layout.columns)
        .flat_map(|column| &column.panes)
        .map(|pane| pane.argv.clone())
        .collect::<Vec<_>>();
    if commands.is_empty() {
        return Err(LaneResumeError::Nothing {
            scope: lane.display,
        });
    }
    let target_pane_id = live
        .first()
        .ok_or(LaneResumeError::LiveNoFocus)?
        .pane
        .as_ref()
        .ok_or(LaneResumeError::LiveNoPane)?
        .pane_id
        .clone();
    let peers = candidates.iter().collect::<Vec<_>>();
    let live_labels = live
        .iter()
        .map(|agent| crate::harness::target::agent_handle(agent, &peers, true))
        .collect();
    let preflight_kinds = closed
        .iter()
        .filter(|agent| supports_agent_resume(agent) && session_backed(agent))
        .map(|agent| agent.kind.clone())
        .collect();
    let channel = lane.channel.clone().or_else(|| {
        closed
            .first()
            .and_then(crate::harness::target::agent_channel)
    });
    Ok(LaneResumeAction::SplitClosed {
        lane_label: lane.display,
        cwd: lane.path,
        channel,
        target_pane_id,
        commands,
        skipped: flat.skipped,
        live_labels,
        preflight_kinds,
    })
}

fn plan_closed_lane(
    request: &LaneResumeRequest<'_>,
    lane: ResolvedLane,
    closed: Vec<AgentState>,
    path_exists: impl Fn(&Path) -> bool,
    session_backed: impl Fn(&AgentState) -> bool,
    restore: LaneRestoreConfig,
) -> Result<LaneResumeAction, LaneResumeError> {
    let (team, flat_agents) = split_team_and_flat(
        &closed,
        &restore.teams,
        &restore.profiles,
        &restore.commands,
        Some(request.project_root),
        &path_exists,
        &session_backed,
    );
    let team_panes = team
        .iter()
        .map(|planned| planned.cohort.seeds.len())
        .sum::<usize>();
    let flat = plan_resume(
        &flat_agents,
        &BTreeSet::new(),
        request.max.saturating_sub(team_panes),
        Some(request.project_root),
        path_exists,
        &session_backed,
        request.rimz_bin,
    );
    if team.is_empty() && flat.tabs.is_empty() {
        return Err(LaneResumeError::Nothing {
            scope: lane.display,
        });
    }
    let mut preflight_kinds = team
        .iter()
        .flat_map(|planned| planned.layout.agent_kinds())
        .map(AgentKind::new_unchecked)
        .collect::<Vec<_>>();
    preflight_kinds.extend(
        flat_agents
            .iter()
            .filter(|agent| supports_agent_resume(agent) && session_backed(agent))
            .map(|agent| agent.kind.clone()),
    );
    Ok(LaneResumeAction::RestoreClosed {
        lane_label: lane.display,
        cwd: lane.path,
        plan: LaneRestorePlan {
            teams: restore.teams,
            team,
            flat,
            discovery_skipped: Vec::new(),
            preflight_kinds,
        },
    })
}

fn lane_summaries(
    agents: &[AgentState],
    project_root: &Path,
    liveness: impl Fn(&AgentState) -> AgentLiveness,
) -> Vec<LaneSummary> {
    let mut groups = BTreeMap::<(PathBuf, Option<String>), Vec<AgentState>>::new();
    let root = crate::worktree::normalize_path_lexical(project_root);
    for agent in agents.iter().filter(|agent| root_session(agent)) {
        let Some(path) = normalized_agent_worktree(agent) else {
            continue;
        };
        let channel = crate::harness::target::agent_channel(agent).filter(|_| {
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
            let candidates = current_lane_candidates(&agents, &lane);
            let freshest = candidates.first()?.last_activity;
            let live = candidates
                .iter()
                .filter(|agent| matches!(liveness(agent), AgentLiveness::Live { .. }))
                .count();
            Some(LaneSummary {
                path: lane.path.clone(),
                label: lane
                    .channel
                    .clone()
                    .or_else(|| {
                        candidates
                            .first()
                            .and_then(crate::harness::target::agent_channel)
                    })
                    .map_or_else(
                        || format!("#{}", path_label(&lane.path)),
                        |value| format!("#{value}"),
                    ),
                members: candidates.len(),
                live,
                freshest,
            })
        })
        .collect::<Vec<_>>();
    sort_lane_summaries(&mut summaries);
    summaries
}

fn sort_lane_summaries(summaries: &mut [LaneSummary]) {
    summaries.sort_by(|left, right| {
        right
            .freshest
            .cmp(&left.freshest)
            .then_with(|| left.label.cmp(&right.label))
    });
}

fn path_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

/// Allocate fresh team members and return complete tabs for one lane restore.
pub fn materialize_lane_restore(
    store: &Store,
    session_name: &str,
    plan: LaneRestorePlan,
) -> anyhow::Result<Vec<ResumeTab>> {
    let mut tabs = Vec::with_capacity(plan.team.len() + plan.flat.tabs.len());
    for planned in &plan.team {
        tabs.push(materialize_team_restore_tab(
            store,
            session_name,
            &plan.teams,
            planned,
        )?);
    }
    tabs.extend(plan.flat.tabs);
    Ok(tabs)
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
    let candidates = agents
        .iter()
        .filter(|agent| !ended.contains(&(agent.kind.clone(), agent.agent_id.clone())))
        .filter_map(|agent| ResumeCandidate::from_agent(agent, || session_backed(agent)))
        .collect();
    plan_resume_candidates(candidates, max, project_root, worktree_exists, rimz_bin)
}

fn plan_resume_candidates(
    mut candidates: Vec<ResumeCandidate>,
    max: usize,
    project_root: Option<&Path>,
    worktree_exists: impl Fn(&Path) -> bool,
    rimz_bin: &Path,
) -> ResumePlan {
    candidates.sort_by(|left, right| {
        newest_cmp(
            left.last_activity,
            left.session_id.as_str(),
            right.last_activity,
            right.session_id.as_str(),
        )
    });

    let mut seen: HashSet<ResumeCandidateKey> = HashSet::new();
    let mut plan = ResumePlan::default();
    let mut tabs: Vec<PlannedResumeTab> = Vec::new();
    for candidate in candidates {
        // An older relaunch that re-used a pane is superseded by the newest
        // stamp. Rebirth-retired stamps fall back to provider session identity.
        if !seen.insert(candidate.key()) {
            continue;
        }
        let channel = candidate_room_channel(project_root, &candidate);
        let label = build_label(&candidate.kind, channel.as_deref(), &candidate.cwd);
        if !worktree_exists(&candidate.cwd) {
            plan.tombstone
                .push((candidate.kind.clone(), candidate.session_id.clone()));
            continue;
        }
        if !supports_candidate_resume(&candidate) {
            plan.skipped.push(ResumeSkip {
                label,
                reason: ResumeSkipReason::NoResumeSupport,
            });
            continue;
        }
        if !candidate.conversation_present {
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
        let command = candidate_resume_command(rimz_bin, &candidate, channel.as_deref());
        let tab_label = channel_label(channel.as_deref(), &candidate.cwd);
        let identity = resume_tab_identity(channel.as_deref(), &candidate.cwd);
        if let Some(tab) = tabs.iter_mut().find(|tab| tab.identity == identity) {
            if let Some(column) = tab.tab.layout.columns.first_mut() {
                column.panes.push(crate::mux::PaneCmd { argv: command });
            }
        } else {
            tabs.push(PlannedResumeTab {
                identity,
                tab: ResumeTab::flat(tab_label, candidate.cwd, vec![command]),
            });
        }
    }
    disambiguate_resume_tab_labels(&mut tabs);
    plan.tabs = tabs.into_iter().map(|planned| planned.tab).collect();
    plan
}

fn candidate_room_channel(
    project_root: Option<&Path>,
    candidate: &ResumeCandidate,
) -> Option<String> {
    match project_root {
        Some(project_root) => crate::harness::target::resolve_room_channel(
            project_root,
            &candidate.cwd,
            candidate.team.as_deref(),
            candidate.channel.as_deref(),
        ),
        None => candidate
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
        .min_by(|left, right| {
            newest_cmp(
                left.last_activity,
                left.agent_id.as_str(),
                right.last_activity,
                right.agent_id.as_str(),
            )
        })
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
    candidates.sort_by(|left, right| {
        newest_cmp(
            left.last_activity,
            left.agent_id.as_str(),
            right.last_activity,
            right.agent_id.as_str(),
        )
    });
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
            .min_by(|left, right| {
                newest_cmp(
                    left.last_activity,
                    left.agent_id.as_str(),
                    right.last_activity,
                    right.agent_id.as_str(),
                )
            })
            .expect("group is non-empty");
        let newest_b = b
            .iter()
            .copied()
            .min_by(|left, right| {
                newest_cmp(
                    left.last_activity,
                    left.agent_id.as_str(),
                    right.last_activity,
                    right.agent_id.as_str(),
                )
            })
            .expect("group is non-empty");
        newest_cmp(
            newest_a.last_activity,
            newest_a.agent_id.as_str(),
            newest_b.last_activity,
            newest_b.agent_id.as_str(),
        )
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

fn supports_candidate_resume(candidate: &ResumeCandidate) -> bool {
    if candidate.session_id.is_provisional() {
        return false;
    }
    find_adapter(&candidate.kind).is_some_and(|adapter| {
        adapter
            .resume_command(&candidate.session_id, &candidate.cwd)
            .is_some()
    })
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
    candidate_resume_command(
        rimz_bin,
        &ResumeCandidate::from_agent_identity(agent, true),
        channel,
    )
}

fn candidate_resume_command(
    rimz_bin: &Path,
    candidate: &ResumeCandidate,
    channel: Option<&str>,
) -> Vec<String> {
    let channel = candidate
        .channel
        .as_deref()
        .filter(|channel| !channel.is_empty())
        .or_else(|| channel.filter(|channel| !channel.is_empty()));
    crate::harness::launch::exec_argv(
        rimz_bin,
        &crate::harness::launch::ExecInvocation {
            kind: candidate.kind.as_str(),
            action: crate::harness::launch::ExecAction::Resume {
                session_id: candidate.session_id.as_str(),
                extra_args: &[],
            },
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            identity: crate::harness::launch::ExecIdentity {
                name: candidate.name.as_deref(),
                name_explicit: candidate.name_explicit,
                profile: candidate.profile.as_deref(),
                mode: None,
                role: candidate.role.as_deref(),
                team: candidate.team.as_deref(),
                launch_group: candidate.launch_group.as_deref(),
                launch_ordinal: candidate.launch_ordinal,
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
