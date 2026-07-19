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

/// A cell that matched a prior member but cannot resume it keeps the match and
/// relaunches fresh, rather than asking an adapter to reopen a session that is
/// not there.
#[test]
fn cohort_relaunches_an_unresumable_match_fresh() {
    let provisional = "launch_019f2cecea067320b667c5946d266e64";

    for (label, agents, cells, team, redeemable, fresh, cwd) in [
        (
            "a kind with no resume CLI",
            vec![agent("ghost", "g1", "/code/query-engine", 1)],
            vec![cohort_cell("ghost", None)],
            None,
            true,
            "ghost:query-engine",
            "/code/query-engine",
        ),
        (
            "a provisional launch placeholder",
            vec![team_agent("codex", provisional, "coder", "/code/pets-l", 4)],
            vec![cohort_cell("codex", Some("coder"))],
            Some("forge"),
            true,
            "codex:pets-l",
            "/code/pets-l",
        ),
        (
            "a session the provider never persisted",
            vec![agent("claude", "a1", "/code/query-engine", 1)],
            vec![cohort_cell("claude", None)],
            None,
            false,
            "claude:query-engine",
            "/code/query-engine",
        ),
    ] {
        let plan = cohort_with(&agents, &cells, team, dead, |_| true, |_| redeemable).expect(label);

        assert_eq!(plan.seeds, vec![CohortSeed::Fresh], "{label}");
        assert_eq!(plan.fresh, vec![fresh.to_owned()], "{label}");
        assert_eq!(plan.cwd.as_deref(), Some(Path::new(cwd)), "{label}");
    }
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

/// `match_cohort` walks a ladder: a named team claims by role, an inline group
/// claims by launch ordinal, then by role, then by bare kind. Legacy members
/// carrying roles but no ordinals must still land on their own cell.
#[test]
fn match_cohort_resolves_cells_by_team_then_ordinal_then_role_then_kind() {
    let roled = |kind, id, role: &str| AgentState {
        launch_group: Some("launch_new".to_owned()),
        role: Some(role.to_owned()),
        ..agent(kind, id, "/code/new", 2)
    };
    let grouped = |kind, id| AgentState {
        launch_group: Some("launch_new".to_owned()),
        ..agent(kind, id, "/code/new", 2)
    };

    let by_role = [
        roled("claude", "planner", "planner"),
        roled("claude", "coder", "coder"),
    ];
    let by_kind = [grouped("claude", "claude"), grouped("codex", "codex")];
    let mixed = [
        team_agent("claude", "team", "planner", "/code/forge", 3),
        team_agent("codex", "team-coder", "coder", "/code/forge", 4),
        inline_agent("claude", "inline", "launch_inline", 0, "/code/forge", 2),
        inline_agent(
            "codex",
            "inline-coder",
            "launch_inline",
            1,
            "/code/forge",
            1,
        ),
    ];

    for (label, candidates, cells, team, expected) in [
        (
            "same-kind inline members without ordinals claim by role",
            by_role.iter().collect::<Vec<_>>(),
            vec![
                cohort_cell("claude", Some("coder")),
                cohort_cell("claude", Some("planner")),
            ],
            None,
            vec![Some("coder"), Some("planner")],
        ),
        (
            "inline members without roles fall back to bare kind",
            by_kind.iter().collect::<Vec<_>>(),
            vec![cohort_cell("codex", None), cohort_cell("claude", None)],
            None,
            vec![Some("codex"), Some("claude")],
        ),
        (
            "one pool, team membership claims the cell",
            mixed.iter().collect::<Vec<_>>(),
            vec![
                cohort_cell("claude", Some("planner")),
                cohort_cell("codex", Some("coder")),
            ],
            Some("forge"),
            vec![Some("team"), Some("team-coder")],
        ),
        (
            "the same pool without a team falls to the inline group",
            mixed.iter().collect::<Vec<_>>(),
            vec![
                cohort_cell("claude", Some("planner")),
                cohort_cell("codex", Some("coder")),
            ],
            None,
            vec![Some("inline"), Some("inline-coder")],
        ),
    ] {
        let matched = match_cohort(&candidates, &cells, team)
            .into_iter()
            .map(|agent| agent.map(|agent| agent.agent_id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(matched, expected, "{label}");
    }
}

/// A `..` segment in the requested worktree normalizes before matching, and a
/// named team keeps every sibling in the cohort — so a closed planner still
/// focuses its live reviewer.
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

/// A candidate that cannot be seeded is reported, never silently dropped: a
/// skip carries its reason, and a vanished worktree instead yields a durable
/// end trace so the agent leaves the next candidate set.
#[test]
fn resume_reports_why_a_candidate_was_not_seeded() {
    let provisional = "launch_019f2cecea067320b667c5946d266e64";

    for (label, agent, on_disk, redeemable, skipped, to_end) in [
        (
            "a provisional launch placeholder is not a provider session",
            agent("codex", provisional, "/code/pets-l", 4),
            true,
            true,
            vec![ResumeSkip {
                label: "codex:pets-l".to_owned(),
                reason: ResumeSkipReason::NoResumeSupport,
            }],
            vec![],
        ),
        (
            "a session the provider cannot reopen plans fresh instead",
            agent("claude", "a1", "/code/query-engine", 1),
            true,
            false,
            vec![ResumeSkip {
                label: "claude:query-engine".to_owned(),
                reason: ResumeSkipReason::NoConversation,
            }],
            vec![],
        ),
        (
            "a vanished worktree ends the session rather than skipping it",
            agent("claude", "a1", "/code/gone", 1),
            false,
            true,
            vec![],
            vec![(AgentKind::new_unchecked("claude"), "a1".into())],
        ),
    ] {
        let plan = plan_with(
            &[agent],
            DEFAULT_RESUME_MAX,
            None,
            |_| on_disk,
            |_| redeemable,
        );

        assert!(plan.tabs.is_empty(), "{label}");
        assert_eq!(plan.skipped, skipped, "{label}");
        assert_eq!(plan.agents_to_end, to_end, "{label}");
    }
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
fn resume_dedupes_by_pane_then_session_identity() {
    let paneless = |agent: AgentState| AgentState {
        pane: None,
        ..agent
    };
    let on_branch = |branch: &str, agent: AgentState| AgentState {
        worktree_branch: Some(branch.to_owned()),
        ..agent
    };
    let qe = "/code/query-engine";

    for (label, agents, expected) in [
        (
            "a relaunch on one pane keeps the newest stamp",
            vec![
                agent_on_pane("claude", "old", qe, 60, "terminal_4"),
                agent_on_pane("claude", "new", qe, 2, "terminal_4"),
            ],
            vec![exec_resume("claude", "new")],
        ),
        (
            "a branch checkout between the two does not leak a second pane",
            vec![
                on_branch("main", agent_on_pane("claude", "old", qe, 60, "terminal_4")),
                on_branch(
                    "feature",
                    agent_on_pane("claude", "new", qe, 2, "terminal_4"),
                ),
            ],
            vec![exec_resume("claude", "new")],
        ),
        (
            "two same-kind agents on distinct panes both survive",
            vec![
                agent_on_pane("claude", "a1", qe, 5, "terminal_4"),
                agent_on_pane("claude", "a2", qe, 9, "terminal_5"),
                agent("codex", "c1", qe, 12),
            ],
            vec![
                exec_resume("claude", "a1"),
                exec_resume("claude", "a2"),
                exec_resume("codex", "c1"),
            ],
        ),
        (
            "paneless records dedupe on provider session identity",
            vec![
                paneless(agent("claude", "same", qe, 60)),
                paneless(agent("claude", "same", qe, 2)),
            ],
            vec![exec_resume("claude", "same")],
        ),
    ] {
        let plan = plan(&agents);
        assert_eq!(plan.tabs.len(), 1, "{label}");
        assert_eq!(plan.tabs[0].label, "#query-engine", "{label}");
        // Freshest leads within the tab.
        assert_eq!(single_column(&plan.tabs[0]), expected, "{label}");
        // A superseded relaunch is dropped silently, never reported as a skip.
        assert!(plan.skipped.is_empty(), "{label}");
    }
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

/// A restorable team tab carries one seed per declared role, in the team's own
/// layout order: a prior member resumes, a missing one launches fresh, and a
/// group whose team no longer resolves is left alone entirely.
#[test]
fn team_restore_tabs_seed_every_declared_role() {
    let (teams, profiles, commands) = team_configs();
    let planner = team_agent("claude", "planner", "planner", "/repo/forge", 3);
    let coder = team_agent("codex", "coder", "coder", "/repo/forge", 5);

    for (label, agents, teams, expected) in [
        (
            "declared layout order, not arrival order",
            vec![coder, planner.clone()],
            teams.clone(),
            Some(vec![Some("planner"), Some("coder")]),
        ),
        (
            "a missing member launches fresh beside the resumed one",
            vec![planner.clone()],
            teams,
            Some(vec![Some("planner"), None]),
        ),
        (
            "a team that no longer resolves plans no tab",
            vec![planner],
            TeamsConfig::default(),
            None,
        ),
    ] {
        let tabs = plan_team_restore_tabs(
            &agents,
            &teams,
            &profiles,
            &commands,
            Some(Path::new("/repo")),
            |_| true,
            |_| true,
        );

        let Some(expected) = expected else {
            assert!(tabs.is_empty(), "{label}");
            continue;
        };
        assert_eq!(tabs.len(), 1, "{label}");
        assert_eq!(tabs[0].label, "#forge", "{label}");
        let seeds = tabs[0]
            .cohort
            .seeds
            .iter()
            .map(resume_id)
            .collect::<Vec<_>>();
        assert_eq!(seeds, expected, "{label}");
    }
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

/// Lane selectors resolve to a place before anything is planned. A durable
/// agent outranks a same-named worktree, a scope matches a worktree by name,
/// branch, full path, or directory name, and a PR marker outranks the legacy
/// `pr-<n>` naming. Each case reports the resolved worktree through `Removed`.
#[test]
fn lane_selectors_resolve_to_a_place() {
    let durable = AgentState {
        channel: Some("docs".to_owned()),
        ..agent("codex", "durable", "/other/agent-lane", 1)
    };
    let agent_lane = [durable];
    let review = [LaneWorktree {
        name: "review".to_owned(),
        path: PathBuf::from("/repo-worktrees/docs"),
        branch: Some("feat/docs".to_owned()),
        from_pr: None,
    }];
    let pr_lanes = [
        lane_worktree("pr-42", "legacy", None),
        lane_worktree("review", "pull/42", Some(42)),
    ];
    let scope = |name: &str| LaneResumeSelector::Scope(name.to_owned());

    for (label, selector, agents, worktrees, resolved) in [
        (
            "a durable agent outranks a colliding worktree name",
            scope("docs"),
            agent_lane.as_slice(),
            [lane_worktree("docs", "feat/docs", None)].as_slice(),
            "agent-lane",
        ),
        (
            "scope by worktree name",
            scope("review"),
            &[],
            &review,
            "review",
        ),
        (
            "scope by branch",
            scope("feat/docs"),
            &[],
            &review,
            "review",
        ),
        (
            "scope by full path",
            scope("/repo-worktrees/docs"),
            &[],
            &review,
            "review",
        ),
        (
            "scope by directory name",
            scope("docs"),
            &[],
            &review,
            "review",
        ),
        (
            "a PR marker outranks the legacy pr-<n> name",
            LaneResumeSelector::PullRequest(42),
            &[],
            &pr_lanes,
            "review",
        ),
    ] {
        let error = LaneCase::new(selector, agents)
            .worktrees(worktrees)
            .path_exists(|_| false)
            .liveness(live)
            .run()
            .unwrap_err();
        assert!(
            matches!(error, LaneResumeError::Removed { ref worktree, .. } if worktree == resolved),
            "{label}: got {error:?}"
        );
    }
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
