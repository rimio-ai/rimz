use super::*;
use crate::agents::AgentStatus;
use crate::config::{Profile, RoleBinding, Team};
use crate::ids::{MuxName, PaneId};
use crate::pane::PaneRef;
use jiff::Timestamp;

fn pane(raw: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: String::new(),
        view_id: None,
        view_kind: None,
        view_name: None,
        title: None,
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
    }
}

/// A root agent bound to a pane, active `secs_ago` seconds back.
fn agent(kind: &str, id: &str, worktree: &str, branch: Option<&str>, secs_ago: i64) -> AgentState {
    let when = Timestamp::now() - std::time::Duration::from_secs(secs_ago.max(0) as u64);
    let mut agent = crate::sidebar::test_support::root_agent(kind, id, None);
    agent.name = None;
    agent.kind_ordinal = None;
    agent.status = AgentStatus::Idle;
    agent.pane = Some(pane(&format!("terminal_{id}")));
    agent.worktree_path = Some(worktree.to_owned());
    agent.worktree_branch = branch.map(ToOwned::to_owned);
    agent.last_seen = when;
    agent.last_activity = when;
    agent.registered_at = Some(when);
    agent
}

/// As [`agent`], but stamped on an explicit pane id so a test can model two
/// sessions sharing one pane (a relaunch in place) rather than the default
/// one-pane-per-id.
fn agent_on_pane(
    kind: &str,
    id: &str,
    worktree: &str,
    branch: Option<&str>,
    secs_ago: i64,
    pane_raw: &str,
) -> AgentState {
    let mut agent = agent(kind, id, worktree, branch, secs_ago);
    agent.pane = Some(pane(pane_raw));
    agent
}

fn no_ended() -> BTreeSet<(AgentKind, AgentSessionId)> {
    BTreeSet::new()
}

fn exec_resume(kind: &str, id: &str) -> Vec<String> {
    crate::harness::launch::exec_argv(
        Path::new("/bin/rimz"),
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

fn resume_id(seed: &CohortSeed) -> Option<&str> {
    match seed {
        CohortSeed::Resume(agent) => Some(agent.agent_id.as_str()),
        CohortSeed::Fresh => None,
    }
}

fn local_session(kind: &str, id: &str, created: &str, last: &str) -> LocalSessionObservation {
    LocalSessionObservation {
        kind: AgentKind::new_unchecked(kind),
        session_id: AgentSessionId::from(id),
        workspace: PathBuf::from("/code/query-engine"),
        transcript_path: PathBuf::from(format!("/provider/{id}.jsonl")),
        created_at: created.parse().unwrap(),
        fresh_binding_at: None,
        first_event_at: None,
        last_activity: last.parse().unwrap(),
        projection: crate::agents::LocalSessionProjection::IdentityOnly,
    }
}

#[test]
fn concurrent_session_set_merges_transitive_overlap() {
    let observations = vec![
        local_session(
            "claude",
            "first",
            "2025-01-01T09:00:00Z",
            "2025-01-01T17:00:00Z",
        ),
        local_session(
            "codex",
            "second",
            "2025-01-01T13:00:00Z",
            "2025-01-01T16:00:00Z",
        ),
        local_session(
            "claude",
            "third",
            "2025-01-01T10:00:00Z",
            "2025-01-01T18:00:00Z",
        ),
    ];

    let (resume, skipped) = concurrent_session_set(observations);

    assert_eq!(resume.len(), 3);
    assert_eq!(resume[0].session_id.as_str(), "third");
    assert!(skipped.is_empty());
}

#[test]
fn concurrent_session_set_keeps_only_the_newest_disjoint_run() {
    let observations = vec![
        local_session(
            "claude",
            "yesterday",
            "2025-01-01T09:00:00Z",
            "2025-01-01T10:00:00Z",
        ),
        local_session(
            "codex",
            "today",
            "2025-01-02T09:00:00Z",
            "2025-01-02T10:00:00Z",
        ),
    ];

    let (resume, skipped) = concurrent_session_set(observations);

    assert_eq!(resume[0].session_id.as_str(), "today");
    assert_eq!(skipped[0].session_id.as_str(), "yesterday");
}

#[test]
fn concurrent_session_set_handles_single_and_empty_inputs() {
    let single = local_session(
        "claude",
        "only",
        "2025-01-01T09:00:00Z",
        "2025-01-01T10:00:00Z",
    );
    assert_eq!(
        concurrent_session_set(vec![single]).0[0]
            .session_id
            .as_str(),
        "only"
    );
    assert_eq!(concurrent_session_set(Vec::new()), (Vec::new(), Vec::new()));
}

#[test]
fn discovered_candidate_is_paneless_and_keeps_native_facts() {
    let observation = local_session(
        "claude",
        "only",
        "2025-01-01T09:00:00Z",
        "2025-01-01T10:00:00Z",
    );

    let candidate = ResumeCandidate::from_observation(&observation).expect("native candidate");

    assert_eq!(candidate.session_id.as_str(), "only");
    assert_eq!(candidate.cwd, PathBuf::from("/code/query-engine"));
    assert!(candidate.pane_id.is_none());
    assert!(candidate.channel.is_none());
    assert!(candidate.team.is_none());
    assert!(candidate.role.is_none());
    assert!(candidate.conversation_present);
}

#[test]
fn discovered_candidate_requires_session_and_workspace() {
    let mut observation = local_session(
        "claude",
        "only",
        "2025-01-01T09:00:00Z",
        "2025-01-01T10:00:00Z",
    );
    observation.session_id = AgentSessionId::from("");
    assert!(ResumeCandidate::from_observation(&observation).is_none());

    observation.session_id = AgentSessionId::from("only");
    observation.workspace = PathBuf::new();
    assert!(ResumeCandidate::from_observation(&observation).is_none());
}

#[test]
fn cohort_resume_selects_newest_team_member_per_role() {
    let mut old_planner = agent("claude", "old-planner", "/code/forge", Some("forge"), 30);
    old_planner.team = Some("forge".to_owned());
    old_planner.role = Some("planner".to_owned());
    let mut planner = agent("claude", "planner", "/code/forge", Some("forge"), 2);
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    planner.channel = Some("design".to_owned());
    let mut coder = agent("codex", "coder", "/code/forge", Some("forge"), 4);
    coder.team = Some("forge".to_owned());
    coder.role = Some("coder".to_owned());
    let cells = vec![
        cohort_cell("claude", Some("planner")),
        cohort_cell("codex", Some("coder")),
    ];

    let plan = plan_cohort_resume(
        &[old_planner, planner, coder],
        dead,
        &cells,
        Some("forge"),
        |_| true,
        |_| true,
    )
    .expect("cohort plan");

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
    let mut newest_planner = agent("claude", "newest-planner", "/code/newer", Some("newer"), 1);
    newest_planner.team = Some("forge".to_owned());
    newest_planner.role = Some("planner".to_owned());
    let mut newest_coder = agent("codex", "newest-coder", "/code/newer", Some("newer"), 2);
    newest_coder.team = Some("forge".to_owned());
    newest_coder.role = Some("coder".to_owned());
    let mut target_planner = agent(
        "claude",
        "target-planner",
        "/code/restore",
        Some("restore"),
        50,
    );
    target_planner.team = Some("forge".to_owned());
    target_planner.role = Some("planner".to_owned());
    let mut target_coder = agent(
        "codex",
        "target-coder",
        "/code/restore",
        Some("restore"),
        60,
    );
    target_coder.team = Some("forge".to_owned());
    target_coder.role = Some("coder".to_owned());
    let agents = vec![newest_planner, newest_coder, target_planner, target_coder];
    let scoped = agents
        .into_iter()
        .filter(|agent| agent.worktree_path.as_deref() == Some("/code/restore"))
        .collect::<Vec<_>>();
    let cells = vec![
        cohort_cell("claude", Some("planner")),
        cohort_cell("codex", Some("coder")),
    ];

    let plan = plan_cohort_resume(&scoped, dead, &cells, Some("forge"), |_| true, |_| true)
        .expect("filtered cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("target-planner"), Some("target-coder")]
    );
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/restore")));
}

#[test]
fn cohort_resume_includes_every_ended_session_backed_member() {
    let mut planner = agent("claude", "planner", "/code/forge", Some("forge"), 1);
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    planner.ended_at = Some(planner.last_seen);
    let mut coder = agent("codex", "coder", "/code/forge", Some("forge"), 2);
    coder.team = Some("forge".to_owned());
    coder.role = Some("coder".to_owned());
    coder.ended_at = Some(coder.last_seen);

    let plan = plan_cohort_resume(
        &[planner, coder],
        dead,
        &[
            cohort_cell("claude", Some("planner")),
            cohort_cell("codex", Some("coder")),
        ],
        Some("forge"),
        |_| true,
        |_| true,
    )
    .expect("closed team member remains a cohort candidate");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("planner"), Some("coder")]
    );
}

#[test]
fn cohort_resume_refuses_a_still_live_member() {
    let mut planner = agent("claude", "planner", "/code/forge", Some("forge"), 1);
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    planner.name = Some("swift-otter".to_owned());
    let err = plan_cohort_resume(
        &[planner],
        |_| AgentLiveness::Live { pid: 42 },
        &[cohort_cell("claude", Some("planner"))],
        Some("forge"),
        |_| true,
        |_| true,
    )
    .expect_err("live member refuses resume");

    assert_eq!(
        err,
        CohortResumeErr::MembersStillLive {
            labels: vec!["claude:swift-otter (planner)".to_owned()]
        }
    );
}

#[test]
fn cohort_resume_refuses_when_nothing_matches() {
    let agents = vec![agent("codex", "c1", "/code/query-engine", Some("main"), 1)];
    let err = plan_cohort_resume(
        &agents,
        dead,
        &[cohort_cell("claude", None)],
        None,
        |_| true,
        |_| true,
    )
    .expect_err("no matching kind");

    assert_eq!(
        err,
        CohortResumeErr::NothingToResume {
            spec: "claude".to_owned()
        }
    );
}

#[test]
fn cohort_resume_starts_fresh_for_kind_without_resume_cli() {
    let agents = vec![agent("ghost", "g1", "/code/query-engine", None, 1)];
    let plan = plan_cohort_resume(
        &agents,
        dead,
        &[cohort_cell("ghost", None)],
        None,
        |_| true,
        |_| true,
    )
    .expect("unsupported kind still matched prior cohort");

    assert_eq!(plan.seeds, vec![CohortSeed::Fresh]);
    assert_eq!(plan.fresh, vec!["ghost:query-engine".to_owned()]);
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/query-engine")));
}

#[test]
fn cohort_resume_starts_fresh_for_provisional_launch_placeholder() {
    let mut coder = agent(
        "codex",
        "launch_019f2cecea067320b667c5946d266e64",
        "/code/pets-l",
        Some("pets-l"),
        4,
    );
    coder.team = Some("forge".to_owned());
    coder.role = Some("coder".to_owned());

    let plan = plan_cohort_resume(
        &[coder],
        dead,
        &[cohort_cell("codex", Some("coder"))],
        Some("forge"),
        |_| true,
        |_| true,
    )
    .expect("provisional placeholder still matched the cohort");

    assert_eq!(plan.seeds, vec![CohortSeed::Fresh]);
    assert_eq!(plan.fresh, vec!["codex:pets-l".to_owned()]);
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/pets-l")));
}

#[test]
fn cohort_resume_relaunches_empty_session_fresh() {
    let agents = vec![agent("claude", "a1", "/code/query-engine", None, 1)];
    let plan = plan_cohort_resume(
        &agents,
        dead,
        &[cohort_cell("claude", None)],
        None,
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
    let mut old = agent("claude", "old", "/code/old", None, 50);
    old.launch_group = Some("launch_old".to_owned());
    old.launch_ordinal = Some(0);
    let mut first = agent("codex", "first", "/code/new", None, 2);
    first.launch_group = Some("launch_new".to_owned());
    first.launch_ordinal = Some(0);
    let mut second = agent("claude", "second", "/code/new", None, 3);
    second.launch_group = Some("launch_new".to_owned());
    second.launch_ordinal = Some(1);
    let cells = vec![cohort_cell("claude", None), cohort_cell("codex", None)];

    let plan = plan_cohort_resume(
        &[old, first, second],
        dead,
        &cells,
        None,
        |_| true,
        |_| true,
    )
    .expect("inline cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("first"), Some("second")]
    );
    assert_eq!(plan.launch_group.as_deref(), Some("launch_new"));
}

#[test]
fn inline_cohort_without_ordinals_matches_same_kind_members_by_role() {
    let mut planner = agent("claude", "planner", "/code/new", None, 2);
    planner.launch_group = Some("launch_new".to_owned());
    planner.role = Some("planner".to_owned());
    let mut coder = agent("claude", "coder", "/code/new", None, 3);
    coder.launch_group = Some("launch_new".to_owned());
    coder.role = Some("coder".to_owned());
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
    let mut claude = agent("claude", "claude", "/code/new", None, 2);
    claude.launch_group = Some("launch_new".to_owned());
    let mut codex = agent("codex", "codex", "/code/new", None, 3);
    codex.launch_group = Some("launch_new".to_owned());
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
    let mut team_planner = agent("claude", "team", "/code/forge", None, 3);
    team_planner.team = Some("forge".to_owned());
    team_planner.role = Some("planner".to_owned());
    let mut team_coder = agent("codex", "team-coder", "/code/forge", None, 4);
    team_coder.team = Some("forge".to_owned());
    team_coder.role = Some("coder".to_owned());
    let mut inline_planner = agent("claude", "inline", "/code/forge", None, 2);
    inline_planner.launch_group = Some("launch_inline".to_owned());
    inline_planner.launch_ordinal = Some(0);
    let mut inline_coder = agent("codex", "inline-coder", "/code/forge", None, 1);
    inline_coder.launch_group = Some("launch_inline".to_owned());
    inline_coder.launch_ordinal = Some(1);
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
    let mut planner = agent("claude", "planner", "/code/feature", None, 3);
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    planner.ended_at = Some(Timestamp::UNIX_EPOCH);
    let mut reviewer = agent("codex", "reviewer", "/code/feature", None, 1);
    reviewer.team = Some("forge".to_owned());
    reviewer.role = Some("reviewer".to_owned());

    assert_eq!(
        inspect_cohort_relaunch(
            &[planner, reviewer],
            Path::new("/code/topic/../feature"),
            &[cohort_cell("claude", Some("planner"))],
            Some("forge"),
        ),
        CohortRelaunchState::Present {
            focus_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_reviewer")),
        }
    );
}

#[test]
fn cohort_relaunch_uses_newest_inline_launch_group() {
    let mut old_planner = agent("claude", "old-planner", "/code/feature", None, 10);
    old_planner.launch_group = Some("launch_old".to_owned());
    old_planner.launch_ordinal = Some(0);
    let mut old_coder = agent("codex", "old-coder", "/code/feature", None, 9);
    old_coder.launch_group = Some("launch_old".to_owned());
    old_coder.launch_ordinal = Some(1);
    let mut new_planner = agent("claude", "new-planner", "/code/feature", None, 2);
    new_planner.launch_group = Some("launch_new".to_owned());
    new_planner.launch_ordinal = Some(0);
    new_planner.ended_at = Some(Timestamp::UNIX_EPOCH);
    let mut new_coder = agent("codex", "new-coder", "/code/feature", None, 1);
    new_coder.launch_group = Some("launch_new".to_owned());
    new_coder.launch_ordinal = Some(1);
    new_coder.ended_at = Some(Timestamp::UNIX_EPOCH);

    assert_eq!(
        inspect_cohort_relaunch(
            &[old_planner, old_coder, new_planner, new_coder],
            Path::new("/code/feature"),
            &[cohort_cell("claude", None), cohort_cell("codex", None)],
            None,
        ),
        CohortRelaunchState::Closed
    );
}

#[test]
fn cohort_relaunch_presence_table() {
    let cell = cohort_cell("codex", None);
    let mut ended = agent("codex", "ended", "/code/feature", None, 4);
    ended.ended_at = Some(Timestamp::UNIX_EPOCH);
    let with_pane = agent("codex", "pane", "/code/feature", None, 3);
    let mut without_pane = agent("codex", "paneless", "/code/feature", None, 2);
    without_pane.pane = None;
    let mut live_without_pane = agent("codex", "live", "/code/feature", None, 1);
    live_without_pane.pane = None;
    live_without_pane.runtime_owner = Some(crate::store::runtime::current_process_owner(
        crate::pane::RuntimeOwnerKind::Agent,
        live_without_pane.agent_id.to_string(),
    ));

    for (label, agents, expected) in [
        ("absent", Vec::new(), CohortRelaunchState::Absent),
        ("ended", vec![ended], CohortRelaunchState::Closed),
        (
            "unknown with pane",
            vec![with_pane],
            CohortRelaunchState::Present {
                focus_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_pane")),
            },
        ),
        (
            "unknown without pane",
            vec![without_pane],
            CohortRelaunchState::Closed,
        ),
        (
            "live without pane",
            vec![live_without_pane],
            CohortRelaunchState::Present { focus_pane: None },
        ),
    ] {
        assert_eq!(
            inspect_cohort_relaunch(&agents, Path::new("/code/feature"), &[cell.clone()], None),
            expected,
            "{label}"
        );
    }
}

#[test]
fn cohort_relaunch_focuses_freshest_present_pane() {
    let mut older = agent("claude", "older", "/code/feature", None, 5);
    older.launch_group = Some("launch_group".to_owned());
    older.launch_ordinal = Some(0);
    let mut fresher = agent("codex", "fresher", "/code/feature", None, 1);
    fresher.launch_group = Some("launch_group".to_owned());
    fresher.launch_ordinal = Some(1);

    assert_eq!(
        inspect_cohort_relaunch(
            &[older, fresher],
            Path::new("/code/feature"),
            &[cohort_cell("claude", None), cohort_cell("codex", None)],
            None,
        ),
        CohortRelaunchState::Present {
            focus_pane: Some(PaneId::from_parts(MuxName::Zellij, "terminal_fresher")),
        }
    );
}

#[test]
fn cohort_resume_drops_members_whose_worktree_is_gone() {
    let agents = vec![agent("claude", "a1", "/code/gone", None, 1)];
    let err = plan_cohort_resume(
        &agents,
        dead,
        &[cohort_cell("claude", None)],
        None,
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
        agent("codex", "c1", "/code/query-engine", Some("main"), 30),
        agent(
            "claude",
            "a1",
            "/code/qe-feature",
            Some("feature-migration"),
            5,
        ),
    ];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );
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
        Some("pets-l"),
        4,
    )];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );

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
    let agents = vec![agent("claude", "a1", "/code/query-engine", Some("main"), 1)];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| false,
        Path::new("/bin/rimz"),
    );

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
        agent("claude", "a1", "/work/repoA/main", None, 5),
        agent("codex", "c1", "/work/repoB/main", None, 9),
    ];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );

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
    let mut agent = agent("claude", "a1", "/code/qe", Some("main"), 1);
    agent.name = Some("swift-otter".to_owned());
    agent.profile = Some("claude-planner".to_owned());
    agent.role = Some("planner".to_owned());
    agent.team = Some("forge".to_owned());
    agent.launch_group = Some("launch_group_1".to_owned());
    agent.launch_ordinal = Some(2);
    let argv = resume_command(Path::new("/bin/rimz"), &agent, agent.channel.as_deref());
    assert_eq!(&argv[..4], ["/bin/rimz", "agents", "exec", "claude"]);
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
fn filters_subagents_and_ended_candidates_but_resumes_paneless_roots() {
    let mut child = agent("claude", "kid", "/code/query-engine", Some("main"), 1);
    child.parent_agent_id = Some("parent".into());
    // A rebirth boundary retires the pane stamp, but durable provider identity
    // keeps the root session resumable.
    let mut paneless = agent("claude", "paneless", "/code/query-engine", Some("main"), 1);
    paneless.pane = None;
    let ended: BTreeSet<(AgentKind, AgentSessionId)> =
        [(AgentKind::new_unchecked("claude"), "ended".into())]
            .into_iter()
            .collect();
    let plan = plan_resume(
        &[
            child,
            paneless,
            agent("claude", "ended", "/code/query-engine", Some("main"), 1),
        ],
        &ended,
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
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
    let mut older = agent("claude", "same", "/code/query-engine", Some("main"), 60);
    older.pane = None;
    let mut newer = agent("claude", "same", "/code/query-engine", Some("main"), 2);
    newer.pane = None;

    let plan = plan_resume(
        &[older, newer],
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );

    assert_eq!(plan.tabs.len(), 1);
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![exec_resume("claude", "same")]
    );
}

#[test]
fn stamps_a_missing_worktree_session_ended() {
    let agents = vec![agent("claude", "a1", "/code/gone", Some("dead-branch"), 1)];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| false,
        |_| true,
        Path::new("/bin/rimz"),
    );
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
        agent_on_pane(
            "claude",
            "old",
            "/code/query-engine",
            Some("main"),
            60,
            "terminal_4",
        ),
        agent_on_pane(
            "claude",
            "new",
            "/code/query-engine",
            Some("main"),
            2,
            "terminal_4",
        ),
    ];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );
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
        agent_on_pane(
            "claude",
            "old",
            "/code/query-engine",
            Some("main"),
            60,
            "terminal_4",
        ),
        agent_on_pane(
            "claude",
            "new",
            "/code/query-engine",
            Some("feature"),
            2,
            "terminal_4",
        ),
    ];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );
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
        agent_on_pane(
            "claude",
            "a1",
            "/code/query-engine",
            Some("main"),
            5,
            "terminal_4",
        ),
        agent_on_pane(
            "claude",
            "a2",
            "/code/query-engine",
            Some("main"),
            9,
            "terminal_5",
        ),
        agent("codex", "c1", "/code/query-engine", Some("main"), 12),
    ];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );
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
        agent("claude", "a1", "/code/wt-1", Some("b1"), 5),
        agent("claude", "a2", "/code/wt-2", Some("b2"), 10),
    ];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        1,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );
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
fn labels_fall_back_to_the_worktree_dir_without_a_branch() {
    let agents = vec![agent("codex", "c1", "/code/query-engine", None, 1)];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );
    assert_eq!(plan.tabs[0].label, "#query-engine");
    assert_eq!(
        build_label("codex", None, Path::new("/code/query-engine")),
        "codex:query-engine"
    );
    assert_eq!(
        build_label("codex", None, Path::new("/code/query-engine")),
        "codex:query-engine"
    );
}

#[test]
fn named_channel_groups_by_explicit_channel_and_replays_identity() {
    let mut design = agent("codex", "c1", "/code/query-engine", Some("main"), 1);
    design.channel = Some("design".to_owned());
    let plan = plan_resume(
        &[design],
        &no_ended(),
        DEFAULT_RESUME_MAX,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );

    assert_eq!(plan.tabs[0].label, "#design");
    assert_eq!(
        build_label("codex", Some("design"), Path::new("/code/query-engine")),
        "codex:design"
    );
    let request = decode_exec_request(&first_argv(&plan.tabs[0]));
    assert_eq!(request.identity.params.channel.as_deref(), Some("design"));
}

#[test]
fn worktree_team_resume_replays_flat_worktree_channel() {
    let mut planner = agent(
        "claude",
        "planner",
        "/code/project-wt/auth",
        Some("feature/auth"),
        1,
    );
    planner.team = Some("forge".to_owned());
    planner.role = Some("planner".to_owned());
    let plan = plan_resume(
        &[planner],
        &no_ended(),
        DEFAULT_RESUME_MAX,
        Some(Path::new("/code/project")),
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );

    assert_eq!(plan.tabs[0].label, "#auth");
    let request = decode_exec_request(&first_argv(&plan.tabs[0]));
    assert_eq!(request.identity.params.channel.as_deref(), Some("auth"));
    assert_eq!(request.identity.params.team.as_deref(), Some("forge"));
}

fn team_configs() -> (TeamsConfig, ProfilesConfig, CommandsConfig) {
    let mut profiles = ProfilesConfig::default();
    profiles.0.insert(
        "claude-plan".to_owned(),
        Profile {
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
        Profile {
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
    let mut teams = TeamsConfig::default();
    teams.0.insert(
        "forge".to_owned(),
        Team {
            roles: vec![
                RoleBinding {
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
                RoleBinding {
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
    (teams, profiles, CommandsConfig::default())
}

fn team_agent(kind: &str, id: &str, role: &str, worktree: &str, secs_ago: i64) -> AgentState {
    let mut agent = agent(kind, id, worktree, None, secs_ago);
    agent.team = Some("forge".to_owned());
    agent.role = Some(role.to_owned());
    agent
}

#[test]
fn resume_session_present_requires_non_empty_recorded_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let present = dir.path().join("present.jsonl");
    std::fs::write(&present, "{}\n").expect("write transcript");
    let empty = dir.path().join("empty.jsonl");
    std::fs::write(&empty, "").expect("write empty transcript");
    let missing = dir.path().join("missing.jsonl");

    let mut agent = agent("claude", "a1", "/repo", None, 0);
    assert!(resume_session_present(&agent));
    agent.transcript_path = Some(present.to_string_lossy().into_owned());
    assert!(resume_session_present(&agent));
    agent.transcript_path = Some(missing.to_string_lossy().into_owned());
    assert!(!resume_session_present(&agent));
    agent.transcript_path = Some(empty.to_string_lossy().into_owned());
    assert!(!resume_session_present(&agent));
    agent.transcript_path = Some(dir.path().to_string_lossy().into_owned());
    assert!(!resume_session_present(&agent));
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
    let flat = agent("codex", "flat", "/repo/other", None, 5);

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

fn lane_worktree(name: &str, branch: &str, from_pr: Option<u64>) -> LaneWorktree {
    LaneWorktree {
        name: name.to_owned(),
        path: PathBuf::from(format!("/repo-worktrees/{name}")),
        branch: Some(branch.to_owned()),
        from_pr,
    }
}

fn lane_request<'a>(
    selector: LaneResumeSelector,
    agents: &'a [AgentState],
    worktrees: &'a [LaneWorktree],
    max: usize,
) -> LaneResumeRequest<'a> {
    LaneResumeRequest {
        selector,
        agents,
        worktrees,
        current_root: Path::new("/repo"),
        project_root: Path::new("/repo"),
        max,
        rimz_bin: Path::new("/bin/rimz"),
    }
}

fn empty_lane_restore() -> Result<LaneRestoreConfig, LaneResumeError> {
    Ok(LaneRestoreConfig {
        teams: TeamsConfig::default(),
        profiles: ProfilesConfig::default(),
        commands: CommandsConfig::default(),
    })
}

#[test]
fn lane_scope_prefers_agent_over_colliding_worktree_name() {
    let mut durable = agent("codex", "durable", "/other/agent-lane", None, 1);
    durable.channel = Some("docs".to_owned());
    let agents = [durable];
    let worktrees = [lane_worktree("docs", "feat/docs", None)];
    let error = plan_lane_resume(
        lane_request(
            LaneResumeSelector::Scope("docs".to_owned()),
            &agents,
            &worktrees,
            128,
        ),
        |_| false,
        |_| true,
        |_| AgentLiveness::Live { pid: 7 },
        |_| Vec::new(),
        empty_lane_restore,
    )
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
        let error = plan_lane_resume(
            lane_request(
                LaneResumeSelector::Scope(scope.to_owned()),
                &[],
                &worktrees,
                128,
            ),
            |_| false,
            |_| true,
            dead,
            |_| Vec::new(),
            empty_lane_restore,
        )
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
    let error = plan_lane_resume(
        lane_request(LaneResumeSelector::PullRequest(42), &[], &worktrees, 128),
        |_| false,
        |_| true,
        dead,
        |_| Vec::new(),
        empty_lane_restore,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        LaneResumeError::Removed { worktree, .. } if worktree == "review"
    ));
}

#[test]
fn lane_focus_uses_freshest_live_member_after_pane_dedupe() {
    let older = agent_on_pane("codex", "older", "/lane", None, 20, "shared");
    let newer = agent_on_pane("codex", "newer", "/lane", None, 2, "shared");
    let other = agent_on_pane("claude", "other", "/lane", None, 5, "other");
    let agents = [older, other, newer];
    let action = plan_lane_resume(
        LaneResumeRequest {
            current_root: Path::new("/lane"),
            ..lane_request(LaneResumeSelector::Current, &agents, &[], 128)
        },
        |_| true,
        |_| true,
        |_| AgentLiveness::Live { pid: 7 },
        |_| Vec::new(),
        || panic!("focus must not load restore config"),
    )
    .unwrap();

    assert!(matches!(
        action,
        LaneResumeAction::Focus { pane_id, .. }
            if pane_id == PaneId::from_parts(MuxName::Zellij, "shared")
    ));
}

#[test]
fn lane_partial_resume_targets_live_pane_and_only_seeds_closed_members() {
    let live = agent_on_pane("claude", "live", "/lane", None, 1, "live-pane");
    let closed = agent_on_pane("codex", "closed", "/lane", None, 2, "closed-pane");
    let agents = [live, closed];
    let action = plan_lane_resume(
        LaneResumeRequest {
            current_root: Path::new("/lane"),
            ..lane_request(LaneResumeSelector::Current, &agents, &[], 128)
        },
        |_| true,
        |_| true,
        |agent| {
            if agent.agent_id.as_str() == "live" {
                AgentLiveness::Live { pid: 7 }
            } else {
                AgentLiveness::Dead
            }
        },
        |_| Vec::new(),
        || panic!("partial resume must not load restore config"),
    )
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
    assert_eq!(
        target_pane_id,
        PaneId::from_parts(MuxName::Zellij, "live-pane")
    );
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
    let provisional = agent("codex", "launch_pending", "/lane", None, 1);
    let agents = [provisional];
    let provisional_error = plan_lane_resume(
        LaneResumeRequest {
            current_root: Path::new("/lane"),
            ..lane_request(LaneResumeSelector::Current, &agents, &[], 128)
        },
        |_| true,
        |_| true,
        dead,
        |_| Vec::new(),
        empty_lane_restore,
    )
    .unwrap_err();
    assert_eq!(
        provisional_error,
        LaneResumeError::Nothing {
            scope: "#lane".to_owned()
        }
    );

    let durable = [agent("codex", "durable", "/lane", None, 1)];
    let no_conversation = plan_lane_resume(
        LaneResumeRequest {
            current_root: Path::new("/lane"),
            ..lane_request(LaneResumeSelector::Current, &durable, &[], 128)
        },
        |_| true,
        |_| false,
        dead,
        |_| Vec::new(),
        empty_lane_restore,
    )
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
        agent_on_pane("codex", "old", "/docs", None, 30, "docs"),
        agent_on_pane("codex", "new", "/docs", None, 10, "docs"),
        agent_on_pane("claude", "api", "/api", None, 2, "api"),
    ];
    let action = plan_lane_resume(
        lane_request(LaneResumeSelector::List, &agents, &[], 128),
        |_| true,
        |_| true,
        |agent| {
            if agent.agent_id.as_str() == "api" {
                AgentLiveness::Live { pid: 7 }
            } else {
                AgentLiveness::Dead
            }
        },
        |_| Vec::new(),
        || panic!("listing must not load restore config"),
    )
    .unwrap();

    let LaneResumeAction::List { lanes } = action else {
        panic!("expected listing");
    };
    assert_eq!(lanes.len(), 2);
    assert_eq!(lanes[0].label, "#api");
    assert_eq!(lanes[0].live, 1);
    assert_eq!(lanes[1].members, 1);
}

#[test]
fn lane_all_closed_counts_team_panes_before_flat_capacity() {
    let (teams, profiles, commands) = team_configs();
    let planner = team_agent("claude", "planner", "planner", "/lane", 1);
    let coder = team_agent("codex", "coder", "coder", "/lane", 2);
    let flat = agent("codex", "flat", "/lane", None, 3);
    let agents = [planner, coder, flat];
    let action = plan_lane_resume(
        LaneResumeRequest {
            current_root: Path::new("/lane"),
            ..lane_request(LaneResumeSelector::Current, &agents, &[], 2)
        },
        |_| true,
        |_| true,
        dead,
        |_| Vec::new(),
        || {
            Ok(LaneRestoreConfig {
                teams,
                profiles,
                commands,
            })
        },
    )
    .unwrap();

    let LaneResumeAction::RestoreClosed { plan, .. } = action else {
        panic!("expected closed restore");
    };
    assert_eq!(
        plan.recovery
            .entries
            .iter()
            .filter(|entry| matches!(entry, RecoveryEntry::Team(_)))
            .count(),
        1
    );
    assert!(
        !plan
            .recovery
            .entries
            .iter()
            .any(|entry| matches!(entry, RecoveryEntry::Flat(_)))
    );
    assert!(
        plan.recovery
            .skipped
            .iter()
            .any(|skip| { skip.label == "codex:lane" && skip.reason == ResumeSkipReason::OverCap })
    );
}

#[test]
fn lane_all_closed_restores_team_and_flat_remainder() {
    let (teams, profiles, commands) = team_configs();
    let agents = [
        team_agent("claude", "planner", "planner", "/lane", 1),
        team_agent("codex", "coder", "coder", "/lane", 2),
        agent("codex", "flat", "/lane", None, 3),
    ];
    let action = plan_lane_resume(
        LaneResumeRequest {
            current_root: Path::new("/lane"),
            ..lane_request(LaneResumeSelector::Current, &agents, &[], 3)
        },
        |_| true,
        |_| true,
        dead,
        |_| Vec::new(),
        || {
            Ok(LaneRestoreConfig {
                teams,
                profiles,
                commands,
            })
        },
    )
    .unwrap();

    let LaneResumeAction::RestoreClosed { plan, .. } = action else {
        panic!("expected closed restore");
    };
    assert_eq!(plan.recovery.entries.len(), 2);
    assert!(matches!(plan.recovery.entries[0], RecoveryEntry::Team(_)));
    assert!(matches!(plan.recovery.entries[1], RecoveryEntry::Flat(_)));
    assert_eq!(plan.recovery.entries[1].pane_count(), 1);
}

#[test]
fn lane_recovery_materializes_team_first_and_fails_strictly() {
    let (teams, profiles, commands) = team_configs();
    let agents = [
        team_agent("claude", "planner", "planner", "/lane", 1),
        team_agent("codex", "coder", "coder", "/lane", 2),
        agent("codex", "flat", "/lane", None, 3),
    ];
    let action = plan_lane_resume(
        LaneResumeRequest {
            current_root: Path::new("/lane"),
            ..lane_request(LaneResumeSelector::Current, &agents, &[], 3)
        },
        |_| true,
        |_| true,
        dead,
        |_| Vec::new(),
        || {
            Ok(LaneRestoreConfig {
                teams,
                profiles,
                commands,
            })
        },
    )
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
    let mut zed = agent("codex", "zed", "/work/zed", None, 1);
    zed.last_activity = when;
    let mut alpha = agent("codex", "alpha", "/work/alpha", None, 1);
    alpha.last_activity = when;
    let flat = plan_resume_detailed(
        &[zed, alpha],
        &BTreeSet::new(),
        2,
        None,
        |_| true,
        |_| true,
        Path::new("/bin/rimz"),
    );
    let mut recovery = RecoveryPlan::new(TeamsConfig::default(), Vec::new(), flat);
    recovery.sort_by_freshness();

    assert_eq!(recovery.labels(), ["#alpha", "#zed"]);
}

#[test]
fn lane_listing_discovers_only_worktrees_without_durable_members() {
    let durable = [agent("codex", "docs", "/repo-worktrees/docs", None, 5)];
    let worktrees = [
        lane_worktree("docs", "feat/docs", None),
        lane_worktree("native", "feat/native", None),
    ];
    let action = plan_lane_resume(
        lane_request(LaneResumeSelector::List, &durable, &worktrees, 128),
        |_| true,
        |_| true,
        dead,
        |path| {
            assert_eq!(path, Path::new("/repo-worktrees/native"));
            vec![local_session(
                "claude",
                "native",
                "2025-01-04T09:00:00Z",
                "2025-01-04T10:00:00Z",
            )]
        },
        || panic!("listing must not load restore config"),
    )
    .unwrap();

    let LaneResumeAction::List { lanes } = action else {
        panic!("expected listing");
    };
    assert_eq!(lanes.len(), 2);
    assert!(lanes.iter().any(|lane| lane.label == "#native"));
}
