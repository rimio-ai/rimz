//! Shared fixtures for the resume suite; the tests live in the concern
//! modules beside this one.

use super::*;
use crate::config::Profile;
use crate::ids::{MuxName, PaneId};
use crate::pane::PaneRef;
use jiff::Timestamp;

const RIMZ_BIN: &str = "/bin/rimz";

fn pane_id(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

/// A root agent bound to its own pane, active `secs_ago` seconds back.
fn agent(kind: &str, id: &str, worktree: &str, secs_ago: i64) -> AgentState {
    let when = Timestamp::now() - std::time::Duration::from_secs(secs_ago.max(0) as u64);
    AgentState {
        pane: Some(PaneRef::from_id(pane_id(&format!("terminal_{id}")))),
        worktree_path: Some(worktree.to_owned()),
        ..crate::testkit::agent_state(kind, id, when)
    }
}

/// As [`agent`], but stamped on an explicit pane id so a test can model two
/// sessions sharing one pane (a relaunch in place) rather than the default
/// one-pane-per-id.
fn agent_on_pane(kind: &str, id: &str, worktree: &str, secs_ago: i64, raw: &str) -> AgentState {
    AgentState {
        pane: Some(PaneRef::from_id(pane_id(raw))),
        ..agent(kind, id, worktree, secs_ago)
    }
}

/// A named-team member holding a role.
fn team_agent(kind: &str, id: &str, role: &str, worktree: &str, secs_ago: i64) -> AgentState {
    AgentState {
        team: Some("forge".to_owned()),
        role: Some(role.to_owned()),
        ..agent(kind, id, worktree, secs_ago)
    }
}

/// An inline-spec member carrying its launch group and cell ordinal.
fn inline_agent(
    kind: &str,
    id: &str,
    group: &str,
    ordinal: u32,
    worktree: &str,
    secs_ago: i64,
) -> AgentState {
    AgentState {
        launch_group: Some(group.to_owned()),
        launch_ordinal: Some(ordinal),
        ..agent(kind, id, worktree, secs_ago)
    }
}

fn exec_resume(kind: &str, id: &str) -> Vec<String> {
    crate::harness::launch::exec_argv(
        Path::new(RIMZ_BIN),
        &crate::harness::launch::ExecRequest {
            kind: AgentKind::new_unchecked(kind),
            action: crate::harness::launch::ExecAction::Resume {
                session_id: id.to_owned(),
                extra_args: Vec::new(),
            },
            system_prompt_file: None,
            provider_account: crate::harness::launch::ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            identity: crate::harness::launch::ExecIdentity::default(),
        },
    )
    .expect("exec argv")
}

fn decode_exec_request(argv: &[String]) -> crate::harness::launch::ExecRequest {
    let payload = argv
        .windows(2)
        .find_map(|pair| (pair[0] == "--request").then_some(pair[1].as_str()))
        .expect("exec request payload");
    crate::harness::launch::decode_exec_request(&argv[3], None, payload)
        .expect("decode exec request")
}

fn single_column(tab: &ResumeTab) -> Vec<Vec<String>> {
    let column = tab
        .layout
        .columns
        .first()
        .expect("resume tab has one column");
    column.panes.iter().map(|pane| pane.argv.clone()).collect()
}

fn first_argv(tab: &ResumeTab) -> Vec<String> {
    single_column(tab)
        .into_iter()
        .next()
        .expect("resume tab has a pane")
}

fn cohort_cell(kind: &str, role: Option<&str>) -> CohortCell {
    CohortCell {
        kind: AgentKind::new_unchecked(kind),
        role: role.map(ToOwned::to_owned),
    }
}

fn dead(_: &AgentState) -> AgentLiveness {
    AgentLiveness::Dead
}

fn live(_: &AgentState) -> AgentLiveness {
    AgentLiveness::Live { pid: 7 }
}

fn resume_id(seed: &CohortSeed) -> Option<&str> {
    match seed {
        CohortSeed::Resume(agent) => Some(agent.agent_id.as_str()),
        CohortSeed::Fresh => None,
    }
}

fn session_ids(observations: &[LocalSessionObservation]) -> Vec<&str> {
    observations
        .iter()
        .map(|observation| observation.session_id.as_str())
        .collect()
}

/// A native session observation running from hour `created` to hour `last`,
/// counted from a fixed base. Clustering compares interval endpoints only, so
/// plain hours show the overlap structure that ISO stamps bury.
fn local_session(kind: &str, id: &str, created: i64, last: i64) -> LocalSessionObservation {
    let at = |hour: i64| {
        "2025-01-01T00:00:00Z".parse::<Timestamp>().unwrap()
            + std::time::Duration::from_secs(hour.max(0) as u64 * 3600)
    };
    LocalSessionObservation {
        kind: AgentKind::new_unchecked(kind),
        session_id: AgentSessionId::from(id),
        workspace: PathBuf::from("/code/query-engine"),
        transcript_path: PathBuf::from(format!("/provider/{id}.jsonl")),
        created_at: at(created),
        fresh_binding_at: None,
        first_event_at: None,
        last_activity: at(last),
        projection: crate::agents::LocalSessionProjection::IdentityOnly,
    }
}

/// The planning environment, so a call site names only what it varies.
fn ctx<'a>(
    max: usize,
    project_root: Option<&'a Path>,
    profiles: &'a ProfilesConfig,
) -> ResumeContext<'a> {
    ResumeContext {
        project_root,
        rimz_bin: Path::new(RIMZ_BIN),
        profiles,
        max,
    }
}

fn no_profiles() -> ProfilesConfig {
    ProfilesConfig::default()
}

/// A bare machine profile; each test sets the posture fields it exercises.
fn profile(agent: &str) -> Profile {
    toml::from_str(&format!("agent = {agent:?}")).expect("profile fixture")
}

/// One machine profile, so a resume candidate has posture to replay.
fn profiles(name: &str, profile: Profile) -> ProfilesConfig {
    ProfilesConfig(BTreeMap::from([(name.to_owned(), profile)]))
}

/// The argv of the one pane a single-candidate plan seeds.
fn single_pane_argv(plan: &ResumePlan) -> Vec<String> {
    let [tab] = plan.tabs.as_slice() else {
        panic!("expected exactly one resume tab, got {}", plan.tabs.len());
    };
    let [column] = tab.layout.columns.as_slice() else {
        panic!("expected exactly one column");
    };
    let [pane] = column.panes.as_slice() else {
        panic!("expected exactly one pane");
    };
    pane.argv.clone()
}

/// [`plan_resume`] with the permissive defaults every rebirth test shares:
/// every worktree on disk, every session redeemable, nothing cleanly ended.
fn plan(agents: &[AgentState]) -> ResumePlan {
    plan_capped(agents, DEFAULT_RESUME_MAX)
}

fn plan_capped(agents: &[AgentState], max: usize) -> ResumePlan {
    plan_with(agents, max, None, |_| true, |_| true)
}

fn plan_excluding(
    agents: &[AgentState],
    ended: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> ResumePlan {
    plan_resume(
        agents,
        ended,
        ctx(DEFAULT_RESUME_MAX, None, &no_profiles()),
        |_| true,
        |_| true,
    )
}

fn plan_with(
    agents: &[AgentState],
    max: usize,
    project_root: Option<&Path>,
    worktree_exists: impl Fn(&Path) -> bool,
    session_backed: impl Fn(&AgentState) -> bool,
) -> ResumePlan {
    plan_resume(
        agents,
        &BTreeSet::new(),
        ctx(max, project_root, &no_profiles()),
        worktree_exists,
        session_backed,
    )
}

/// The argv `Yolo` compiles to for an adapter, for asserting the stamped mode
/// reached (or never reached) the resume argv.
fn yolo_argv(kind: &str) -> Vec<String> {
    crate::agents::find_definition(kind)
        .expect("registered adapter")
        .spec()
        .launch
        .permission_args(PermissionMode::Yolo)
}

/// [`resolve_posture`] for one profile against one agent kind.
fn posture_for(
    kind: &str,
    profile: Option<&str>,
    stamped_mode: Option<PermissionMode>,
    profiles: &ProfilesConfig,
) -> ResumePosture {
    let kind = AgentKind::new_unchecked(kind);
    resolve_posture(
        PostureRequest {
            profile,
            kind: &kind,
            stamped_mode,
        },
        profiles,
    )
}

/// [`plan_resume`] for one candidate whose profile posture rides the argv.
fn plan_profiled(agent: AgentState, profiles: &ProfilesConfig) -> ResumePlan {
    plan_resume(
        &[agent],
        &BTreeSet::new(),
        ctx(DEFAULT_RESUME_MAX, None, profiles),
        |_| true,
        |_| true,
    )
}

/// [`plan_cohort_resume`] over a closed cohort whose worktrees are all on disk
/// and whose sessions are all redeemable.
fn cohort(
    agents: &[AgentState],
    cells: &[CohortCell],
    team: Option<&str>,
) -> Result<CohortResumePlan, CohortResumeErr> {
    cohort_with(agents, cells, team, dead, |_| true, |_| true)
}

fn cohort_with(
    agents: &[AgentState],
    cells: &[CohortCell],
    team: Option<&str>,
    liveness: impl Fn(&AgentState) -> AgentLiveness,
    worktree_exists: impl Fn(&Path) -> bool,
    session_backed: impl Fn(&AgentState) -> bool,
) -> Result<CohortResumePlan, CohortResumeErr> {
    plan_cohort_resume(
        agents,
        liveness,
        cells,
        team,
        worktree_exists,
        session_backed,
    )
}

fn team_configs() -> (TeamsConfig, ProfilesConfig, CommandsConfig) {
    let profiles = toml::from_str(
        r#"
        claude-plan = { agent = "claude" }
        codex-code = { agent = "codex" }
        "#,
    )
    .expect("profiles fixture");
    let teams = toml::from_str(
        r#"
        [forge]
        layout = "planner,coder"
        roles = [
            { role = "planner", profile = "claude-plan" },
            { role = "coder", profile = "codex-code" },
        ]
        "#,
    )
    .expect("teams fixture");
    (teams, profiles, CommandsConfig::default())
}

fn lane_worktree(name: &str, branch: &str, from_pr: Option<u64>) -> LaneWorktree {
    LaneWorktree {
        name: name.to_owned(),
        path: PathBuf::from(format!("/repo-worktrees/{name}")),
        branch: Some(branch.to_owned()),
        from_pr,
    }
}

fn empty_lane_restore() -> Result<LaneRestoreConfig, LaneResumeError> {
    Ok(LaneRestoreConfig {
        teams: TeamsConfig::default(),
        profiles: ProfilesConfig::default(),
        commands: CommandsConfig::default(),
    })
}

type PathPredicate<'a> = Box<dyn Fn(&Path) -> bool + 'a>;
type AgentPredicate<'a> = Box<dyn Fn(&AgentState) -> bool + 'a>;
type LivenessFn<'a> = Box<dyn Fn(&AgentState) -> AgentLiveness + 'a>;
type DiscoverFn<'a> = Box<dyn FnMut(&Path) -> Vec<LocalSessionObservation> + 'a>;
type RestoreFn<'a> = Box<dyn FnOnce() -> Result<LaneRestoreConfig, LaneResumeError> + 'a>;

/// One [`plan_lane_resume`] call. Defaults are the permissive lane: the
/// worktree is on disk, every session is redeemable, every member is closed,
/// nothing is discoverable natively, and the restore config is empty. Each
/// test names only what it varies.
struct LaneCase<'a> {
    selector: LaneResumeSelector,
    agents: &'a [AgentState],
    worktrees: &'a [LaneWorktree],
    current_root: &'a Path,
    max: usize,
    path_exists: PathPredicate<'a>,
    session_backed: AgentPredicate<'a>,
    liveness: LivenessFn<'a>,
    discover: DiscoverFn<'a>,
    restore: RestoreFn<'a>,
}

impl<'a> LaneCase<'a> {
    fn new(selector: LaneResumeSelector, agents: &'a [AgentState]) -> Self {
        Self {
            selector,
            agents,
            worktrees: &[],
            current_root: Path::new("/repo"),
            max: 128,
            path_exists: Box::new(|_| true),
            session_backed: Box::new(|_| true),
            liveness: Box::new(dead),
            discover: Box::new(|_| Vec::new()),
            restore: Box::new(empty_lane_restore),
        }
    }

    fn worktrees(mut self, worktrees: &'a [LaneWorktree]) -> Self {
        self.worktrees = worktrees;
        self
    }

    fn current_root(mut self, root: &'a str) -> Self {
        self.current_root = Path::new(root);
        self
    }

    fn max(mut self, max: usize) -> Self {
        self.max = max;
        self
    }

    fn path_exists(mut self, f: impl Fn(&Path) -> bool + 'a) -> Self {
        self.path_exists = Box::new(f);
        self
    }

    fn session_backed(mut self, f: impl Fn(&AgentState) -> bool + 'a) -> Self {
        self.session_backed = Box::new(f);
        self
    }

    fn liveness(mut self, f: impl Fn(&AgentState) -> AgentLiveness + 'a) -> Self {
        self.liveness = Box::new(f);
        self
    }

    fn discover(mut self, f: impl FnMut(&Path) -> Vec<LocalSessionObservation> + 'a) -> Self {
        self.discover = Box::new(f);
        self
    }

    fn restore(
        mut self,
        f: impl FnOnce() -> Result<LaneRestoreConfig, LaneResumeError> + 'a,
    ) -> Self {
        self.restore = Box::new(f);
        self
    }

    fn run(self) -> Result<LaneResumeAction, LaneResumeError> {
        plan_lane_resume(
            LaneResumeRequest {
                selector: self.selector,
                agents: self.agents,
                worktrees: self.worktrees,
                current_root: self.current_root,
                project_root: Path::new("/repo"),
                max: self.max,
                rimz_bin: Path::new(RIMZ_BIN),
            },
            self.path_exists,
            self.session_backed,
            self.liveness,
            self.discover,
            self.restore,
        )
    }
}

mod cohort;
mod flat;
mod lane;
mod posture;
