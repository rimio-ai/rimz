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

#[test]
fn concurrent_session_set_selects_the_newest_overlap_cluster() {
    // `second` overlaps neither `first` nor `third` on its own, but both
    // bracket it, so the three merge transitively into one working set.
    let overlapping = vec![
        local_session("claude", "first", 9, 17),
        local_session("codex", "second", 13, 16),
        local_session("claude", "third", 10, 18),
    ];
    // A day apart: two clusters that never touch.
    let disjoint = vec![
        local_session("claude", "yesterday", 9, 10),
        local_session("codex", "today", 33, 34),
    ];

    for (label, observations, expect_resume, expect_skipped) in [
        (
            "transitive overlap merges, newest first",
            overlapping,
            vec!["third", "first", "second"],
            vec![],
        ),
        (
            "only the newest disjoint run resumes",
            disjoint,
            vec!["today"],
            vec!["yesterday"],
        ),
        ("nothing observed", Vec::new(), vec![], vec![]),
    ] {
        let (resume, skipped) = concurrent_session_set(observations);
        assert_eq!(session_ids(&resume), expect_resume, "{label}");
        assert_eq!(session_ids(&skipped), expect_skipped, "{label}");
    }
}

#[test]
fn discovered_candidate_requires_session_and_workspace() {
    let mut observation = local_session("claude", "only", 9, 10);
    observation.session_id = AgentSessionId::from("");
    assert!(ResumeCandidate::from_observation(&observation).is_none());

    observation.session_id = AgentSessionId::from("only");
    observation.workspace = PathBuf::new();
    assert!(ResumeCandidate::from_observation(&observation).is_none());
}

#[test]
fn cohort_resume_selects_newest_team_member_per_role() {
    let old_planner = team_agent("claude", "old-planner", "planner", "/code/forge", 30);
    let planner = AgentState {
        channel: Some("design".to_owned()),
        ..team_agent("claude", "planner", "planner", "/code/forge", 2)
    };
    let coder = team_agent("codex", "coder", "coder", "/code/forge", 4);
    let cells = vec![
        cohort_cell("claude", Some("planner")),
        cohort_cell("codex", Some("coder")),
    ];

    let plan = cohort(&[old_planner, planner, coder], &cells, Some("forge")).expect("cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("planner"), Some("coder")]
    );
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/forge")));
    assert_eq!(plan.channel.as_deref(), Some("design"));
    assert!(plan.fresh.is_empty());
}

#[test]
fn cohort_resume_uses_filtered_worktree_even_when_older_than_same_team_elsewhere() {
    let agents = vec![
        team_agent("claude", "newest-planner", "planner", "/code/newer", 1),
        team_agent("codex", "newest-coder", "coder", "/code/newer", 2),
        team_agent("claude", "target-planner", "planner", "/code/restore", 50),
        team_agent("codex", "target-coder", "coder", "/code/restore", 60),
    ];
    let scoped = agents
        .into_iter()
        .filter(|agent| agent.worktree_path.as_deref() == Some("/code/restore"))
        .collect::<Vec<_>>();
    let cells = vec![
        cohort_cell("claude", Some("planner")),
        cohort_cell("codex", Some("coder")),
    ];

    let plan = cohort(&scoped, &cells, Some("forge")).expect("filtered cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("target-planner"), Some("target-coder")]
    );
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/restore")));
}

#[test]
fn cohort_resume_includes_every_ended_session_backed_member() {
    let planner = team_agent("claude", "planner", "planner", "/code/forge", 1);
    let planner = AgentState {
        ended_at: Some(planner.last_seen),
        ..planner
    };
    let coder = team_agent("codex", "coder", "coder", "/code/forge", 2);
    let coder = AgentState {
        ended_at: Some(coder.last_seen),
        ..coder
    };

    let plan = cohort(
        &[planner, coder],
        &[
            cohort_cell("claude", Some("planner")),
            cohort_cell("codex", Some("coder")),
        ],
        Some("forge"),
    )
    .expect("closed team member remains a cohort candidate");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("planner"), Some("coder")]
    );
}

#[test]
fn cohort_refuses_live_and_unmatched_specs() {
    // The pet name is what proves the live-member label format.
    let live_planner = AgentState {
        name: Some("swift-otter".to_owned()),
        ..team_agent("claude", "planner", "planner", "/code/forge", 1)
    };

    for (label, agents, cells, team, liveness, on_disk, expected) in [
        (
            "a still-live member blocks the relaunch",
            vec![live_planner],
            vec![cohort_cell("claude", Some("planner"))],
            Some("forge"),
            live as fn(&AgentState) -> AgentLiveness,
            true,
            CohortResumeErr::MembersStillLive {
                labels: vec!["claude:swift-otter (planner)".to_owned()],
            },
        ),
        (
            "no prior session of the requested kind",
            vec![agent("codex", "c1", "/code/query-engine", 1)],
            vec![cohort_cell("claude", None)],
            None,
            dead,
            true,
            CohortResumeErr::NothingToResume {
                spec: "claude".to_owned(),
            },
        ),
        (
            "a vanished worktree drops its members",
            vec![agent("claude", "a1", "/code/gone", 1)],
            vec![cohort_cell("claude", None)],
            None,
            dead,
            false,
            CohortResumeErr::NothingToResume {
                spec: "claude".to_owned(),
            },
        ),
    ] {
        let err =
            cohort_with(&agents, &cells, team, liveness, |_| on_disk, |_| true).expect_err(label);
        assert_eq!(err, expected, "{label}");
    }
}

#[test]
fn cohort_resume_starts_fresh_for_kind_without_resume_cli() {
    let agents = vec![agent("ghost", "g1", "/code/query-engine", 1)];
    let plan = cohort(&agents, &[cohort_cell("ghost", None)], None)
        .expect("unsupported kind still matched prior cohort");

    assert_eq!(plan.seeds, vec![CohortSeed::Fresh]);
    assert_eq!(plan.fresh, vec!["ghost:query-engine".to_owned()]);
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/query-engine")));
}

#[test]
fn cohort_resume_starts_fresh_for_provisional_launch_placeholder() {
    let coder = team_agent(
        "codex",
        "launch_019f2cecea067320b667c5946d266e64",
        "coder",
        "/code/pets-l",
        4,
    );

    let plan = cohort(
        &[coder],
        &[cohort_cell("codex", Some("coder"))],
        Some("forge"),
    )
    .expect("provisional placeholder still matched the cohort");

    assert_eq!(plan.seeds, vec![CohortSeed::Fresh]);
    assert_eq!(plan.fresh, vec!["codex:pets-l".to_owned()]);
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/pets-l")));
}

#[test]
fn cohort_resume_relaunches_empty_session_fresh() {
    let agents = vec![agent("claude", "a1", "/code/query-engine", 1)];
    let plan = cohort_with(
        &agents,
        &[cohort_cell("claude", None)],
        None,
        dead,
        |_| true,
        |_| false,
    )
    .expect("empty session still relaunches the matched cohort cell");

    assert_eq!(plan.seeds, vec![CohortSeed::Fresh]);
    assert_eq!(plan.fresh, vec!["claude:query-engine".to_owned()]);
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/query-engine")));
}

#[test]
fn cohort_resume_matches_inline_group_by_launch_ordinal() {
    let old = inline_agent("claude", "old", "launch_old", 0, "/code/old", 50);
    let first = inline_agent("codex", "first", "launch_new", 0, "/code/new", 2);
    let second = inline_agent("claude", "second", "launch_new", 1, "/code/new", 3);
    let cells = vec![cohort_cell("claude", None), cohort_cell("codex", None)];

    let plan = cohort(&[old, first, second], &cells, None).expect("inline cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("first"), Some("second")]
    );
    assert_eq!(plan.launch_group.as_deref(), Some("launch_new"));
}

#[test]
fn inline_cohort_without_ordinals_matches_same_kind_members_by_role() {
    let planner = AgentState {
        launch_group: Some("launch_new".to_owned()),
        role: Some("planner".to_owned()),
        ..agent("claude", "planner", "/code/new", 2)
    };
    let coder = AgentState {
        launch_group: Some("launch_new".to_owned()),
        role: Some("coder".to_owned()),
        ..agent("claude", "coder", "/code/new", 3)
    };
    let cells = vec![
        cohort_cell("claude", Some("coder")),
        cohort_cell("claude", Some("planner")),
    ];

    let matches = match_cohort(&[&planner, &coder], &cells, None);

    assert_eq!(
        matches
            .into_iter()
            .map(|agent| agent.map(|agent| agent.agent_id.as_str()))
            .collect::<Vec<_>>(),
        [Some("coder"), Some("planner")]
    );
}

#[test]
fn inline_cohort_without_roles_falls_back_to_kind() {
    let claude = AgentState {
        launch_group: Some("launch_new".to_owned()),
        ..agent("claude", "claude", "/code/new", 2)
    };
    let codex = AgentState {
        launch_group: Some("launch_new".to_owned()),
        ..agent("codex", "codex", "/code/new", 3)
    };
    let cells = vec![cohort_cell("codex", None), cohort_cell("claude", None)];

    let matches = match_cohort(&[&claude, &codex], &cells, None);

    assert_eq!(
        matches
            .into_iter()
            .map(|agent| agent.map(|agent| agent.agent_id.as_str()))
            .collect::<Vec<_>>(),
        [Some("codex"), Some("claude")]
    );
}

#[test]
fn match_cohort_dispatches_team_and_inline_membership() {
    let team_planner = team_agent("claude", "team", "planner", "/code/forge", 3);
    let team_coder = team_agent("codex", "team-coder", "coder", "/code/forge", 4);
    let inline_planner = inline_agent("claude", "inline", "launch_inline", 0, "/code/forge", 2);
    let inline_coder = inline_agent(
        "codex",
        "inline-coder",
        "launch_inline",
        1,
        "/code/forge",
        1,
    );
    let cells = vec![
        cohort_cell("claude", Some("planner")),
        cohort_cell("codex", Some("coder")),
    ];
    let candidates = [&team_planner, &team_coder, &inline_planner, &inline_coder];

    let team = match_cohort(&candidates, &cells, Some("forge"));
    let inline = match_cohort(&candidates, &cells, None);

    assert_eq!(team[0].map(|agent| agent.agent_id.as_str()), Some("team"));
    assert_eq!(
        inline[0].map(|agent| agent.agent_id.as_str()),
        Some("inline")
    );
}

#[test]
fn cohort_relaunch_normalizes_worktrees_and_keeps_named_team_siblings() {
    let planner = AgentState {
        ended_at: Some(Timestamp::UNIX_EPOCH),
        ..team_agent("claude", "planner", "planner", "/code/feature", 3)
    };
    let reviewer = team_agent("codex", "reviewer", "reviewer", "/code/feature", 1);

    assert_eq!(
        inspect_cohort_relaunch(
            &[planner, reviewer],
            Path::new("/code/topic/../feature"),
            &[cohort_cell("claude", Some("planner"))],
            Some("forge"),
        ),
        CohortRelaunchState::Present {
            focus_pane: Some(pane_id("terminal_reviewer")),
        }
    );
}

#[test]
fn cohort_relaunch_presence_table() {
    let one_cell = vec![cohort_cell("codex", None)];
    let two_cells = vec![cohort_cell("claude", None), cohort_cell("codex", None)];
    let closed = |agent: AgentState| AgentState {
        ended_at: Some(Timestamp::UNIX_EPOCH),
        ..agent
    };
    let paneless = |agent: AgentState| AgentState {
        pane: None,
        ..agent
    };
    let member = |kind, id, group, ordinal, secs| {
        inline_agent(kind, id, group, ordinal, "/code/feature", secs)
    };

    let with_pane = agent("codex", "pane", "/code/feature", 3);
    let live_without_pane = {
        let base = agent("codex", "live", "/code/feature", 1);
        AgentState {
            runtime_owner: Some(crate::store::runtime::current_process_owner(
                crate::pane::RuntimeOwnerKind::Agent,
                base.agent_id.to_string(),
            )),
            ..paneless(base)
        }
    };
    let closed_newest_group = vec![
        member("claude", "old-planner", "launch_old", 0, 10),
        member("codex", "old-coder", "launch_old", 1, 9),
        closed(member("claude", "new-planner", "launch_new", 0, 2)),
        closed(member("codex", "new-coder", "launch_new", 1, 1)),
    ];
    let present_group = vec![
        member("claude", "older", "launch_group", 0, 5),
        member("codex", "fresher", "launch_group", 1, 1),
    ];

    for (label, agents, cells, expected) in [
        ("absent", Vec::new(), &one_cell, CohortRelaunchState::Absent),
        (
            "ended",
            vec![closed(agent("codex", "ended", "/code/feature", 4))],
            &one_cell,
            CohortRelaunchState::Closed,
        ),
        (
            "unknown with pane",
            vec![with_pane],
            &one_cell,
            CohortRelaunchState::Present {
                focus_pane: Some(pane_id("terminal_pane")),
            },
        ),
        (
            "unknown without pane",
            vec![paneless(agent("codex", "paneless", "/code/feature", 2))],
            &one_cell,
            CohortRelaunchState::Closed,
        ),
        (
            "live without pane",
            vec![live_without_pane],
            &one_cell,
            CohortRelaunchState::Present { focus_pane: None },
        ),
        (
            "newest inline group decides, and it is closed",
            closed_newest_group,
            &two_cells,
            CohortRelaunchState::Closed,
        ),
        (
            "focus lands on the freshest present pane",
            present_group,
            &two_cells,
            CohortRelaunchState::Present {
                focus_pane: Some(pane_id("terminal_fresher")),
            },
        ),
    ] {
        assert_eq!(
            inspect_cohort_relaunch(&agents, Path::new("/code/feature"), cells, None),
            expected,
            "{label}"
        );
    }
}

#[test]
fn cohort_resume_drops_members_whose_worktree_is_gone() {
    let agents = vec![agent("claude", "a1", "/code/gone", 1)];
    let err = cohort_with(
        &agents,
        &[cohort_cell("claude", None)],
        None,
        dead,
        |_| false,
        |_| true,
    )
    .expect_err("missing worktree drops candidate");

    assert_eq!(
        err,
        CohortResumeErr::NothingToResume {
            spec: "claude".to_owned()
        }
    );
}

#[test]
fn resumes_root_agents_most_recent_first() {
    let agents = vec![
        agent("codex", "c1", "/code/query-engine", 30),
        agent("claude", "a1", "/code/qe-feature", 5),
    ];
    let plan = plan(&agents);
    assert!(plan.skipped.is_empty());
    assert_eq!(plan.tabs.len(), 2);
    // Most-recently-active leads (the focus target).
    assert_eq!(plan.tabs[0].label, "#qe-feature");
    // Wrapper argv: the pane funnels through `rimz agents exec`, which
    // injects launch env before spawning the adapter's resume argv.
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![exec_resume("claude", "a1")]
    );
    assert_eq!(plan.tabs[0].cwd, PathBuf::from("/code/qe-feature"));
    assert_eq!(plan.tabs[1].label, "#query-engine");
    assert_eq!(
        single_column(&plan.tabs[1]),
        vec![exec_resume("codex", "c1")]
    );
    assert_eq!(
        plan.resumed,
        [
            (AgentKind::new_unchecked("claude"), "a1".into()),
            (AgentKind::new_unchecked("codex"), "c1".into()),
        ]
        .into_iter()
        .collect()
    );
}

#[test]
fn rebirth_resume_skips_provisional_launch_placeholder() {
    let agents = vec![agent(
        "codex",
        "launch_019f2cecea067320b667c5946d266e64",
        "/code/pets-l",
        4,
    )];
    let plan = plan(&agents);

    assert!(plan.tabs.is_empty());
    assert_eq!(
        plan.skipped,
        vec![ResumeSkip {
            label: "codex:pets-l".to_owned(),
            reason: ResumeSkipReason::NoResumeSupport,
        }]
    );
    assert!(plan.agents_to_end.is_empty());
}

#[test]
fn plan_resume_skips_agent_without_conversation() {
    let agents = vec![agent("claude", "a1", "/code/query-engine", 1)];
    let plan = plan_with(&agents, DEFAULT_RESUME_MAX, None, |_| true, |_| false);

    assert!(plan.tabs.is_empty());
    assert_eq!(
        plan.skipped,
        vec![ResumeSkip {
            label: "claude:query-engine".to_owned(),
            reason: ResumeSkipReason::NoConversation,
        }]
    );
    assert!(plan.agents_to_end.is_empty());
}

#[test]
fn disambiguates_reborn_tabs_with_the_same_basename() {
    let agents = vec![
        agent("claude", "a1", "/work/repoA/main", 5),
        agent("codex", "c1", "/work/repoB/main", 9),
    ];
    let plan = plan(&agents);

    assert_eq!(plan.tabs.len(), 2);
    assert_eq!(plan.tabs[0].cwd, PathBuf::from("/work/repoA/main"));
    assert_eq!(plan.tabs[0].label, "#repoA/main");
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![exec_resume("claude", "a1")]
    );
    assert_eq!(plan.tabs[1].cwd, PathBuf::from("/work/repoB/main"));
    assert_eq!(plan.tabs[1].label, "#repoB/main");
    assert_eq!(
        single_column(&plan.tabs[1]),
        vec![exec_resume("codex", "c1")]
    );
}

#[test]
fn resume_command_replays_launch_identity() {
    // A reborn agent re-stamps its durable launch identity, so it answers
    // to `@<profile>` and `@<role>` again after a mux rebirth.
    let agent = AgentState {
        name: Some("swift-otter".to_owned()),
        profile: Some("claude-planner".to_owned()),
        role: Some("planner".to_owned()),
        team: Some("forge".to_owned()),
        launch_group: Some("launch_group_1".to_owned()),
        launch_ordinal: Some(2),
        ..agent("claude", "a1", "/code/qe", 1)
    };
    let argv = resume_command(
        Path::new(RIMZ_BIN),
        &agent,
        agent.channel.as_deref(),
        &ResumePosture::default(),
    );
    assert_eq!(&argv[..4], [RIMZ_BIN, "agents", "exec", "claude"]);
    let request = decode_exec_request(&argv);
    assert_eq!(
        request.action,
        crate::harness::launch::ExecAction::Resume {
            session_id: "a1".to_owned(),
            extra_args: Vec::new(),
        }
    );
    assert!(request.close_pane_on_exit);
    assert_eq!(request.identity.name.as_deref(), Some("swift-otter"));
    assert_eq!(
        request.identity.params.profile.as_deref(),
        Some("claude-planner")
    );
    assert_eq!(request.identity.params.role.as_deref(), Some("planner"));
    assert_eq!(request.identity.params.team.as_deref(), Some("forge"));
    assert_eq!(
        request.identity.params.launch_group.as_deref(),
        Some("launch_group_1")
    );
    assert_eq!(request.identity.params.launch_ordinal, Some(2));
}

#[test]
fn resume_replays_the_profile_declared_posture() {
    // A session that launched as `@planner` comes back as a planner: the
    // profile's model, effort, and appended system prompt ride the resume argv,
    // not just the `@planner` handle.
    let prompt = tempfile::NamedTempFile::new().expect("temp prompt file");
    let profiles = profiles(
        "planner",
        Profile {
            model: Some("opus".to_owned()),
            effort: Some("high".to_owned()),
            append_system_prompt_file: Some(prompt.path().to_path_buf()),
            ..profile("claude")
        },
    );
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &profiles);

    assert!(plan.warnings.is_empty());
    let request = decode_exec_request(&single_pane_argv(&plan));
    let expected = crate::harness::spec::profile_cell("planner", &profiles)
        .expect("planner profile resolves")
        .args;
    assert!(
        expected.iter().any(|arg| arg == "opus"),
        "profile argv should carry the model: {expected:?}"
    );
    assert_eq!(
        request.action,
        crate::harness::launch::ExecAction::Resume {
            session_id: "a1".to_owned(),
            extra_args: expected,
        }
    );
    assert_eq!(request.identity.params.model.as_deref(), Some("opus"));
    assert_eq!(request.identity.params.effort.as_deref(), Some("high"));
}

#[test]
fn resume_leaves_one_off_launch_values_out_of_the_posture() {
    // `model` on the rollup is observed, not declared — the user may have
    // switched it mid-session with `/model`. Only the profile speaks here.
    let agent = AgentState {
        profile: Some("planner".to_owned()),
        model: Some("some-one-off-model".to_owned()),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &profiles("planner", profile("claude")));

    let argv = single_pane_argv(&plan);
    assert!(
        !argv.iter().any(|arg| arg == "some-one-off-model"),
        "one-off model leaked into the resume argv: {argv:?}"
    );
    assert_eq!(decode_exec_request(&argv).identity.params.model, None);
}

#[test]
fn resume_replays_the_stamped_mode_when_the_profile_declares_none() {
    // The launch event records the permission posture the user granted, so a
    // profile-less agent still comes back with it.
    let agent = AgentState {
        mode: Some(PermissionMode::Yolo),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &no_profiles());

    let request = decode_exec_request(&single_pane_argv(&plan));
    assert_eq!(request.action.extra_args(), yolo_argv("claude"));
    assert_eq!(request.identity.params.mode, Some(PermissionMode::Yolo));
}

#[test]
fn resume_degrades_to_bare_when_the_profile_is_gone() {
    // Rebirth runs unattended, so a profile dropped from config warns and
    // recovers rather than refusing to bring the session back.
    let agent = AgentState {
        profile: Some("retired".to_owned()),
        ..agent("claude", "a1", "/code/qe", 1)
    };

    let plan = plan_profiled(agent, &no_profiles());

    assert_eq!(plan.tabs.len(), 1, "the session still comes back");
    assert_eq!(plan.warnings.len(), 1);
    assert!(
        plan.warnings[0].contains("retired"),
        "warning should name the profile: {}",
        plan.warnings[0]
    );
    assert_eq!(
        decode_exec_request(&single_pane_argv(&plan))
            .action
            .extra_args(),
        &[] as &[String]
    );
}

#[test]
fn profile_mode_wins_over_the_stamped_mode() {
    // The profile is the standing decision; the stamp only fills a gap.
    let profiles = profiles(
        "planner",
        Profile {
            mode: Some(PermissionMode::Auto),
            ..profile("claude")
        },
    );

    let posture = posture_for(
        "claude",
        Some("planner"),
        Some(PermissionMode::Yolo),
        &profiles,
    );

    assert_eq!(posture.mode, Some(PermissionMode::Auto));
    assert!(
        !yolo_argv("claude")
            .iter()
            .any(|arg| posture.args.contains(arg)),
        "stamped yolo argv leaked past the profile's mode: {:?}",
        posture.args
    );
}

#[test]
fn a_profile_prompt_file_that_vanished_degrades_instead_of_refusing() {
    // Rebirth is unattended: a deleted prompt file must not strand the session.
    let dir = tempfile::tempdir().expect("temp dir");
    let profiles = profiles(
        "planner",
        Profile {
            system_prompt_file: Some(dir.path().join("missing.md")),
            ..profile("codex")
        },
    );

    let posture = posture_for("codex", Some("planner"), None, &profiles);

    assert!(posture.args.is_empty());
    assert!(matches!(
        posture.degraded,
        Some(PostureDegrade::PromptFileMissing { .. })
    ));
}

#[test]
fn posture_reports_a_provider_switch_rather_than_refusing() {
    // Restart escalates this; unattended resume degrades on it. Either way the
    // resolver reports rather than fails.
    let profiles = profiles("planner", profile("codex"));

    let posture = posture_for("claude", Some("planner"), None, &profiles);

    assert!(posture.args.is_empty());
    assert!(matches!(
        posture.degraded,
        Some(PostureDegrade::KindChanged { .. })
    ));
}

#[test]
fn filters_subagents_and_ended_candidates_but_resumes_paneless_roots() {
    let child = AgentState {
        parent_agent_id: Some("parent".into()),
        ..agent("claude", "kid", "/code/query-engine", 1)
    };
    // A rebirth boundary retires the pane stamp, but durable provider identity
    // keeps the root session resumable.
    let paneless = AgentState {
        pane: None,
        ..agent("claude", "paneless", "/code/query-engine", 1)
    };
    let ended: BTreeSet<(AgentKind, AgentSessionId)> =
        [(AgentKind::new_unchecked("claude"), "ended".into())]
            .into_iter()
            .collect();
    let plan = plan_excluding(
        &[
            child,
            paneless,
            agent("claude", "ended", "/code/query-engine", 1),
        ],
        &ended,
    );
    assert_eq!(plan.tabs.len(), 1);
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![exec_resume("claude", "paneless")]
    );
    assert!(plan.skipped.is_empty());
}

#[test]
fn dedups_paneless_records_by_provider_session_identity() {
    let older = AgentState {
        pane: None,
        ..agent("claude", "same", "/code/query-engine", 60)
    };
    let newer = AgentState {
        pane: None,
        ..agent("claude", "same", "/code/query-engine", 2)
    };

    let plan = plan(&[older, newer]);

    assert_eq!(plan.tabs.len(), 1);
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![exec_resume("claude", "same")]
    );
}

#[test]
fn stamps_a_missing_worktree_session_ended() {
    let agents = vec![agent("claude", "a1", "/code/gone", 1)];
    let plan = plan_with(&agents, DEFAULT_RESUME_MAX, None, |_| false, |_| true);
    assert!(plan.tabs.is_empty());
    assert!(plan.skipped.is_empty());
    assert_eq!(
        plan.agents_to_end,
        vec![(AgentKind::new_unchecked("claude"), "a1".into())]
    );
}

#[test]
fn dedups_a_relaunched_agent_keeping_the_newest() {
    // A relaunch in place re-uses the same pane id; the older stamp is
    // superseded by the newest, exactly as the live sidebar binds the pane.
    let agents = vec![
        agent_on_pane("claude", "old", "/code/query-engine", 60, "terminal_4"),
        agent_on_pane("claude", "new", "/code/query-engine", 2, "terminal_4"),
    ];
    let plan = plan(&agents);
    assert_eq!(plan.tabs.len(), 1);
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![exec_resume("claude", "new")]
    );
    // The superseded relaunch is dropped silently, not reported as a skip.
    assert!(plan.skipped.is_empty());
}

#[test]
fn collapses_a_relaunch_that_changed_branch_on_one_pane() {
    // Same pane, a branch checkout between the two sessions. The pane is the
    // identity, so the differing branch must not leak a second resume pane —
    // the `(kind, worktree, branch)` key used to double this.
    let agents = vec![
        AgentState {
            worktree_branch: Some("main".to_owned()),
            ..agent_on_pane("claude", "old", "/code/query-engine", 60, "terminal_4")
        },
        AgentState {
            worktree_branch: Some("feature".to_owned()),
            ..agent_on_pane("claude", "new", "/code/query-engine", 2, "terminal_4")
        },
    ];
    let plan = plan(&agents);
    assert_eq!(plan.tabs.len(), 1);
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![exec_resume("claude", "new")]
    );
}

#[test]
fn keeps_two_same_kind_agents_in_one_worktree() {
    // Two Claude sessions running side by side in one worktree — distinct
    // panes, so each is its own live agent. The `(kind, worktree, branch)`
    // key used to collapse them to one; pane identity keeps both.
    let agents = vec![
        agent_on_pane("claude", "a1", "/code/query-engine", 5, "terminal_4"),
        agent_on_pane("claude", "a2", "/code/query-engine", 9, "terminal_5"),
        agent("codex", "c1", "/code/query-engine", 12),
    ];
    let plan = plan(&agents);
    assert_eq!(plan.tabs.len(), 1);
    assert_eq!(plan.tabs[0].label, "#query-engine");
    // Freshest leads within the tab; all same-worktree sessions are resumed.
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![
            exec_resume("claude", "a1"),
            exec_resume("claude", "a2"),
            exec_resume("codex", "c1")
        ]
    );
}

#[test]
fn caps_and_reports_the_overflow() {
    let agents = vec![
        agent("claude", "a1", "/code/wt-1", 5),
        agent("claude", "a2", "/code/wt-2", 10),
    ];
    let plan = plan_capped(&agents, 1);
    assert_eq!(plan.tabs.len(), 1);
    // The freshest survives the cap; the older overflows.
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![exec_resume("claude", "a1")]
    );
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].reason, ResumeSkipReason::OverCap);
    assert_eq!(
        plan.resumed,
        [(AgentKind::new_unchecked("claude"), "a1".into())]
            .into_iter()
            .collect()
    );
}

#[test]
fn build_label_prefers_channel_over_worktree_dir() {
    let cwd = Path::new("/code/query-engine");
    assert_eq!(build_label("codex", None, cwd), "codex:query-engine");
    assert_eq!(build_label("codex", Some("design"), cwd), "codex:design");
}

#[test]
fn resume_tab_labels_and_replayed_channel() {
    for (label, agent, project_root, tab_label, channel, team) in [
        (
            "no channel falls back to the worktree directory",
            agent("codex", "c1", "/code/query-engine", 1),
            None,
            "#query-engine",
            None,
            None,
        ),
        (
            "an explicit channel names the tab and is replayed",
            AgentState {
                channel: Some("design".to_owned()),
                ..agent("codex", "c1", "/code/query-engine", 1)
            },
            None,
            "#design",
            Some("design"),
            None,
        ),
        (
            "a team worktree under the project root resolves a room channel",
            team_agent("claude", "planner", "planner", "/code/project-wt/auth", 1),
            Some(Path::new("/code/project")),
            "#auth",
            Some("auth"),
            Some("forge"),
        ),
    ] {
        let plan = plan_with(
            &[agent],
            DEFAULT_RESUME_MAX,
            project_root,
            |_| true,
            |_| true,
        );

        assert_eq!(plan.tabs[0].label, tab_label, "{label}");
        let request = decode_exec_request(&first_argv(&plan.tabs[0]));
        assert_eq!(
            request.identity.params.channel.as_deref(),
            channel,
            "{label}"
        );
        assert_eq!(request.identity.params.team.as_deref(), team, "{label}");
    }
}

/// A recorded transcript is the provider's own answer: require it to exist and
/// carry content. A record without one defers to the adapter, which reads the
/// store the resume would open; the store probe itself is covered in the
/// adapter that owns it. Adapters that keep no inspectable store abstain, and
/// an agent with no worktree leaves nothing for one to resolve — both stay
/// resumable.
#[test]
fn resume_session_present_requires_a_redeemable_conversation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let present = dir.path().join("present.jsonl");
    std::fs::write(&present, "{}\n").expect("write transcript");
    let empty = dir.path().join("empty.jsonl");
    std::fs::write(&empty, "").expect("write empty transcript");
    let missing = dir.path().join("missing.jsonl");

    let recorded = |path: &Path| AgentState {
        transcript_path: Some(path.to_string_lossy().into_owned()),
        ..agent("claude", "a1", "/repo", 0)
    };

    for (label, agent, expected) in [
        (
            "a written transcript is redeemable",
            recorded(&present),
            true,
        ),
        ("a missing transcript is not", recorded(&missing), false),
        ("an empty transcript is not", recorded(&empty), false),
        (
            "a directory is not a transcript",
            recorded(dir.path()),
            false,
        ),
        (
            "no transcript and no worktree leaves nothing to probe",
            agent("claude", "06e78f43-ecc1-486b-b50d-3c1f7770a5ae", "", 0),
            true,
        ),
        (
            "an adapter without an inspectable store abstains",
            agent("opencode", "ses_abc", "/repo", 0),
            true,
        ),
    ] {
        assert_eq!(resume_session_present(&agent), expected, "{label}");
    }
}

#[test]
fn plans_team_restore_in_declared_layout_order() {
    let (teams, profiles, commands) = team_configs();
    let planner = team_agent("claude", "planner", "planner", "/repo/forge", 3);
    let coder = team_agent("codex", "coder", "coder", "/repo/forge", 5);

    let tabs = plan_team_restore_tabs(
        &[coder, planner],
        &teams,
        &profiles,
        &commands,
        Some(Path::new("/repo")),
        |_| true,
        |_| true,
    );

    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].label, "#forge");
    assert_eq!(tabs[0].cohort.seeds.len(), 2);
    assert!(matches!(
        &tabs[0].cohort.seeds[0],
        CohortSeed::Resume(agent) if agent.agent_id.as_str() == "planner"
    ));
    assert!(matches!(
        &tabs[0].cohort.seeds[1],
        CohortSeed::Resume(agent) if agent.agent_id.as_str() == "coder"
    ));
}

#[test]
fn plans_fresh_seed_for_missing_team_member() {
    let (teams, profiles, commands) = team_configs();
    let planner = team_agent("claude", "planner", "planner", "/repo/forge", 3);

    let tabs = plan_team_restore_tabs(
        &[planner],
        &teams,
        &profiles,
        &commands,
        Some(Path::new("/repo")),
        |_| true,
        |_| true,
    );

    assert_eq!(tabs.len(), 1);
    assert!(matches!(tabs[0].cohort.seeds[0], CohortSeed::Resume(_)));
    assert_eq!(tabs[0].cohort.seeds[1], CohortSeed::Fresh);
}

#[test]
fn split_team_and_flat_keeps_unmatched_agents_for_flat_resume() {
    let (teams, profiles, commands) = team_configs();
    let planner = team_agent("claude", "planner", "planner", "/repo/forge", 3);
    let flat = agent("codex", "flat", "/repo/other", 5);

    let (tabs, flat_agents) = split_team_and_flat(
        &[planner, flat],
        &teams,
        &profiles,
        &commands,
        Some(Path::new("/repo")),
        |_| true,
        |_| true,
    );

    assert_eq!(tabs.len(), 1);
    assert_eq!(flat_agents.len(), 1);
    assert_eq!(flat_agents[0].agent_id.as_str(), "flat");
}

#[test]
fn team_restore_ignores_group_whose_team_no_longer_resolves() {
    let (_teams, profiles, commands) = team_configs();
    let planner = team_agent("claude", "planner", "planner", "/repo/forge", 3);

    let tabs = plan_team_restore_tabs(
        &[planner],
        &TeamsConfig::default(),
        &profiles,
        &commands,
        Some(Path::new("/repo")),
        |_| true,
        |_| true,
    );

    assert!(tabs.is_empty());
}

#[test]
fn lane_scope_prefers_agent_over_colliding_worktree_name() {
    let durable = AgentState {
        channel: Some("docs".to_owned()),
        ..agent("codex", "durable", "/other/agent-lane", 1)
    };
    let agents = [durable];
    let worktrees = [lane_worktree("docs", "feat/docs", None)];
    let error = LaneCase::new(LaneResumeSelector::Scope("docs".to_owned()), &agents)
        .worktrees(&worktrees)
        .path_exists(|_| false)
        .liveness(live)
        .run()
        .unwrap_err();

    assert_eq!(
        error,
        LaneResumeError::Removed {
            scope: "docs".to_owned(),
            worktree: "agent-lane".to_owned(),
        }
    );
}

#[test]
fn lane_scope_matches_branch_full_path_and_file_name() {
    let worktrees = [LaneWorktree {
        name: "review".to_owned(),
        path: PathBuf::from("/repo-worktrees/docs"),
        branch: Some("feat/docs".to_owned()),
        from_pr: None,
    }];
    for scope in ["review", "feat/docs", "/repo-worktrees/docs", "docs"] {
        let error = LaneCase::new(LaneResumeSelector::Scope(scope.to_owned()), &[])
            .worktrees(&worktrees)
            .path_exists(|_| false)
            .run()
            .unwrap_err();
        assert!(matches!(
            error,
            LaneResumeError::Removed { ref worktree, .. } if worktree == "review"
        ));
    }
}

#[test]
fn lane_pr_prefers_marker_before_legacy_name() {
    let worktrees = [
        lane_worktree("pr-42", "legacy", None),
        lane_worktree("review", "pull/42", Some(42)),
    ];
    let error = LaneCase::new(LaneResumeSelector::PullRequest(42), &[])
        .worktrees(&worktrees)
        .path_exists(|_| false)
        .run()
        .unwrap_err();

    assert!(matches!(
        error,
        LaneResumeError::Removed { worktree, .. } if worktree == "review"
    ));
}

#[test]
fn lane_focus_uses_freshest_live_member_after_pane_dedupe() {
    let agents = [
        agent_on_pane("codex", "older", "/lane", 20, "shared"),
        agent_on_pane("claude", "other", "/lane", 5, "other"),
        agent_on_pane("codex", "newer", "/lane", 2, "shared"),
    ];
    let action = LaneCase::new(LaneResumeSelector::Current, &agents)
        .current_root("/lane")
        .liveness(live)
        .restore(|| panic!("focus must not load restore config"))
        .run()
        .unwrap();

    assert!(matches!(
        action,
        LaneResumeAction::Focus { pane_id: id, .. } if id == pane_id("shared")
    ));
}

#[test]
fn lane_partial_resume_targets_live_pane_and_only_seeds_closed_members() {
    let agents = [
        agent_on_pane("claude", "live", "/lane", 1, "live-pane"),
        agent_on_pane("codex", "closed", "/lane", 2, "closed-pane"),
    ];
    let action = LaneCase::new(LaneResumeSelector::Current, &agents)
        .current_root("/lane")
        .liveness(|agent| {
            if agent.agent_id.as_str() == "live" {
                AgentLiveness::Live { pid: 7 }
            } else {
                AgentLiveness::Dead
            }
        })
        .run()
        .unwrap();

    let LaneResumeAction::SplitClosed {
        target_pane_id,
        commands,
        live_labels,
        ..
    } = action
    else {
        panic!("expected partial split");
    };
    assert_eq!(target_pane_id, pane_id("live-pane"));
    assert_eq!(commands.len(), 1);
    assert!(matches!(
        decode_exec_request(&commands[0]).action,
        crate::harness::launch::ExecAction::Resume { ref session_id, .. }
            if session_id == "closed"
    ));
    assert_eq!(live_labels.len(), 1);
}

#[test]
fn lane_rejects_provisional_or_unbacked_conversations() {
    let provisional = [agent("codex", "launch_pending", "/lane", 1)];
    let provisional_error = LaneCase::new(LaneResumeSelector::Current, &provisional)
        .current_root("/lane")
        .run()
        .unwrap_err();
    assert_eq!(
        provisional_error,
        LaneResumeError::Nothing {
            scope: "#lane".to_owned()
        }
    );

    let durable = [agent("codex", "durable", "/lane", 1)];
    let no_conversation = LaneCase::new(LaneResumeSelector::Current, &durable)
        .current_root("/lane")
        .session_backed(|_| false)
        .run()
        .unwrap_err();
    assert_eq!(
        no_conversation,
        LaneResumeError::Nothing {
            scope: "#lane".to_owned()
        }
    );
}

#[test]
fn lane_listing_groups_deduped_members_and_sorts_freshest_first() {
    let agents = [
        agent_on_pane("codex", "old", "/docs", 30, "docs"),
        agent_on_pane("codex", "new", "/docs", 10, "docs"),
        agent_on_pane("claude", "api", "/api", 2, "api"),
    ];
    let action = LaneCase::new(LaneResumeSelector::List, &agents)
        .liveness(|agent| {
            if agent.agent_id.as_str() == "api" {
                AgentLiveness::Live { pid: 7 }
            } else {
                AgentLiveness::Dead
            }
        })
        .restore(|| panic!("listing must not load restore config"))
        .run()
        .unwrap();

    let LaneResumeAction::List { lanes } = action else {
        panic!("expected listing");
    };
    assert_eq!(lanes.len(), 2);
    assert_eq!(lanes[0].label, "#api");
    assert_eq!(lanes[0].live, 1);
    assert_eq!(lanes[1].members, 1);
}

/// The team tab is planned first and its panes count against the cap, so a
/// flat member only rides along when the cap leaves room for it.
#[test]
fn lane_all_closed_restores_team_first_within_the_cap() {
    for (label, max, entries, panes, over_cap) in [
        (
            "the two team panes consume a cap of two",
            2,
            ["team"].as_slice(),
            [2].as_slice(),
            true,
        ),
        (
            "a cap of three leaves room for the flat member",
            3,
            ["team", "flat"].as_slice(),
            [2, 1].as_slice(),
            false,
        ),
    ] {
        let (teams, profiles, commands) = team_configs();
        let agents = [
            team_agent("claude", "planner", "planner", "/lane", 1),
            team_agent("codex", "coder", "coder", "/lane", 2),
            agent("codex", "flat", "/lane", 3),
        ];
        let action = LaneCase::new(LaneResumeSelector::Current, &agents)
            .current_root("/lane")
            .max(max)
            .restore(|| {
                Ok(LaneRestoreConfig {
                    teams,
                    profiles,
                    commands,
                })
            })
            .run()
            .unwrap();

        let LaneResumeAction::RestoreClosed { plan, .. } = action else {
            panic!("expected closed restore: {label}");
        };
        let kinds = plan
            .recovery
            .entries
            .iter()
            .map(|entry| match entry {
                RecoveryEntry::Team(_) => "team",
                RecoveryEntry::Flat(_) => "flat",
            })
            .collect::<Vec<_>>();
        let pane_counts = plan
            .recovery
            .entries
            .iter()
            .map(RecoveryEntry::pane_count)
            .collect::<Vec<_>>();
        assert_eq!(kinds, entries, "{label}");
        assert_eq!(pane_counts, panes, "{label}");
        assert_eq!(
            plan.recovery
                .skipped
                .iter()
                .any(|skip| skip.label == "codex:lane" && skip.reason == ResumeSkipReason::OverCap),
            over_cap,
            "{label}"
        );
    }
}

#[test]
fn lane_recovery_materializes_team_first_and_fails_strictly() {
    let (teams, profiles, commands) = team_configs();
    let agents = [
        team_agent("claude", "planner", "planner", "/lane", 1),
        team_agent("codex", "coder", "coder", "/lane", 2),
        agent("codex", "flat", "/lane", 3),
    ];
    let action = LaneCase::new(LaneResumeSelector::Current, &agents)
        .current_root("/lane")
        .max(3)
        .restore(|| {
            Ok(LaneRestoreConfig {
                teams,
                profiles,
                commands,
            })
        })
        .run()
        .unwrap();
    let LaneResumeAction::RestoreClosed { plan, .. } = action else {
        panic!("expected closed restore");
    };
    let dir = tempfile::tempdir().expect("store root");
    let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
    let paths =
        crate::store::paths::StatePaths::under(workspace.clone(), &dir.path().join("state"))
            .expect("state paths");
    let runtime = crate::store::paths::RuntimePaths::under(workspace, &dir.path().join("runtime"))
        .expect("runtime paths");
    let store = Store::open(paths, runtime).expect("store");

    let tabs = plan
        .clone()
        .materialize(&store, "rimz-test")
        .expect("strict lane materialization");
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0].pane_count(), 2);
    assert_eq!(tabs[1].pane_count(), 1);

    let mut broken = plan;
    let team = broken
        .recovery
        .entries
        .iter_mut()
        .find_map(|entry| match entry {
            RecoveryEntry::Team(team) => Some(team),
            RecoveryEntry::Flat(_) => None,
        })
        .expect("team entry");
    team.cohort.seeds.truncate(1);
    assert!(broken.materialize(&store, "rimz-test").is_err());
}

#[test]
fn recovery_plan_sorts_equal_freshness_by_label() {
    let when: Timestamp = "2025-01-01T00:00:00Z".parse().unwrap();
    let zed = AgentState {
        last_activity: when,
        ..agent("codex", "zed", "/work/zed", 1)
    };
    let alpha = AgentState {
        last_activity: when,
        ..agent("codex", "alpha", "/work/alpha", 1)
    };
    let flat = plan_resume_detailed(
        &[zed, alpha],
        &BTreeSet::new(),
        ctx(2, None, &no_profiles()),
        |_| true,
        |_| true,
    );
    let mut recovery = RecoveryPlan::new(TeamsConfig::default(), Vec::new(), flat);
    recovery.sort_by_freshness();

    assert_eq!(recovery.labels(), ["#alpha", "#zed"]);
}

#[test]
fn lane_listing_discovers_only_worktrees_without_durable_members() {
    let durable = [agent("codex", "docs", "/repo-worktrees/docs", 5)];
    let worktrees = [
        lane_worktree("docs", "feat/docs", None),
        lane_worktree("native", "feat/native", None),
    ];
    let action = LaneCase::new(LaneResumeSelector::List, &durable)
        .worktrees(&worktrees)
        .discover(|path| {
            assert_eq!(path, Path::new("/repo-worktrees/native"));
            vec![local_session("claude", "native", 9, 10)]
        })
        .restore(|| panic!("listing must not load restore config"))
        .run()
        .unwrap();

    let LaneResumeAction::List { lanes } = action else {
        panic!("expected listing");
    };
    assert_eq!(lanes.len(), 2);
    assert!(lanes.iter().any(|lane| lane.label == "#native"));
}
