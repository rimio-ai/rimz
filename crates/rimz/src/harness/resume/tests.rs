use super::*;
use crate::agents::AgentStatus;
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
    vec![
        "/bin/rimz".to_owned(),
        "agents".to_owned(),
        "exec".to_owned(),
        kind.to_owned(),
        "--resume".to_owned(),
        id.to_owned(),
        "--close-pane-on-exit".to_owned(),
    ]
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

#[test]
fn cohort_resume_selects_newest_team_member_per_role() {
    let mut old_planner = agent("claude", "old-planner", "/code/pcr", Some("pcr"), 30);
    old_planner.team = Some("pcr".to_owned());
    old_planner.role = Some("planner".to_owned());
    let mut planner = agent("claude", "planner", "/code/pcr", Some("pcr"), 2);
    planner.team = Some("pcr".to_owned());
    planner.role = Some("planner".to_owned());
    planner.channel = Some("design".to_owned());
    let mut coder = agent("codex", "coder", "/code/pcr", Some("pcr"), 4);
    coder.team = Some("pcr".to_owned());
    coder.role = Some("coder".to_owned());
    let cells = vec![
        cohort_cell("claude", Some("planner")),
        cohort_cell("codex", Some("coder")),
    ];

    let plan = plan_cohort_resume(
        &[old_planner, planner, coder],
        &no_ended(),
        dead,
        &cells,
        Some("pcr"),
        |_| true,
    )
    .expect("cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("planner"), Some("coder")]
    );
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/pcr")));
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

    let plan = plan_cohort_resume(&scoped, &no_ended(), dead, &cells, Some("forge"), |_| true)
        .expect("filtered cohort plan");

    assert_eq!(
        plan.seeds.iter().map(resume_id).collect::<Vec<_>>(),
        [Some("target-planner"), Some("target-coder")]
    );
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/restore")));
}

#[test]
fn cohort_resume_excludes_ended_members() {
    let mut ended_agent = agent("claude", "ended", "/code/pcr", Some("pcr"), 1);
    ended_agent.team = Some("pcr".to_owned());
    ended_agent.role = Some("planner".to_owned());
    let ended = [(AgentKind::new_unchecked("claude"), "ended".into())]
        .into_iter()
        .collect();
    let err = plan_cohort_resume(
        &[ended_agent],
        &ended,
        dead,
        &[cohort_cell("claude", Some("planner"))],
        Some("pcr"),
        |_| true,
    )
    .expect_err("ended member is not resumable");

    assert_eq!(
        err,
        CohortResumeErr::NothingToResume {
            spec: "pcr".to_owned()
        }
    );
}

#[test]
fn cohort_resume_refuses_a_still_live_member() {
    let mut planner = agent("claude", "planner", "/code/pcr", Some("pcr"), 1);
    planner.team = Some("pcr".to_owned());
    planner.role = Some("planner".to_owned());
    planner.name = Some("swift-otter".to_owned());
    let err = plan_cohort_resume(
        &[planner],
        &no_ended(),
        |_| AgentLiveness::Live { pid: 42 },
        &[cohort_cell("claude", Some("planner"))],
        Some("pcr"),
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
        &no_ended(),
        dead,
        &[cohort_cell("claude", None)],
        None,
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
        &no_ended(),
        dead,
        &[cohort_cell("ghost", None)],
        None,
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
        &no_ended(),
        dead,
        &[cohort_cell("codex", Some("coder"))],
        Some("forge"),
        |_| true,
    )
    .expect("provisional placeholder still matched the cohort");

    assert_eq!(plan.seeds, vec![CohortSeed::Fresh]);
    assert_eq!(plan.fresh, vec!["codex:pets-l".to_owned()]);
    assert_eq!(plan.cwd.as_deref(), Some(Path::new("/code/pets-l")));
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
        &no_ended(),
        dead,
        &cells,
        None,
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
fn cohort_resume_drops_members_whose_worktree_is_gone() {
    let agents = vec![agent("claude", "a1", "/code/gone", None, 1)];
    let err = plan_cohort_resume(
        &agents,
        &no_ended(),
        dead,
        &[cohort_cell("claude", None)],
        None,
        |_| false,
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
    assert!(plan.tombstone.is_empty());
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
    agent.team = Some("pcr".to_owned());
    agent.launch_group = Some("launch_group_1".to_owned());
    agent.launch_ordinal = Some(2);
    assert_eq!(
        resume_command(Path::new("/bin/rimz"), &agent),
        vec![
            "/bin/rimz",
            "agents",
            "exec",
            "claude",
            "--resume",
            "a1",
            "--agent-name",
            "swift-otter",
            "--agent-profile",
            "claude-planner",
            "--agent-role",
            "planner",
            "--agent-team",
            "pcr",
            "--launch-group",
            "launch_group_1",
            "--launch-ordinal",
            "2",
            "--close-pane-on-exit",
        ]
    );
}

#[test]
fn filters_subagents_paneless_and_ended_candidates() {
    let mut child = agent("claude", "kid", "/code/query-engine", Some("main"), 1);
    child.parent_agent_id = Some("parent".into());
    // A `None` pane is both a subagent/ghost with no presence and the shape
    // a rebirth boundary leaves behind for an agent that was not live in the
    // dying incarnation — neither is resumed.
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
        |_| true,
        Path::new("/bin/rimz"),
    );
    assert!(plan.is_empty());
    assert!(plan.skipped.is_empty());
}

#[test]
fn tombstones_a_missing_worktree() {
    let agents = vec![agent("claude", "a1", "/code/gone", Some("dead-branch"), 1)];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
        |_| false,
        Path::new("/bin/rimz"),
    );
    assert!(plan.tabs.is_empty());
    assert!(plan.skipped.is_empty());
    assert_eq!(
        plan.tombstone,
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
    let plan = plan_resume(&agents, &no_ended(), 1, |_| true, Path::new("/bin/rimz"));
    assert_eq!(plan.tabs.len(), 1);
    // The freshest survives the cap; the older overflows.
    assert_eq!(
        single_column(&plan.tabs[0]),
        vec![exec_resume("claude", "a1")]
    );
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].reason, ResumeSkipReason::OverCap);
}

#[test]
fn labels_fall_back_to_the_worktree_dir_without_a_branch() {
    let agents = vec![agent("codex", "c1", "/code/query-engine", None, 1)];
    let plan = plan_resume(
        &agents,
        &no_ended(),
        DEFAULT_RESUME_MAX,
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
        |_| true,
        Path::new("/bin/rimz"),
    );

    assert_eq!(plan.tabs[0].label, "#design");
    assert_eq!(
        build_label("codex", Some("design"), Path::new("/code/query-engine")),
        "codex:design"
    );
    assert!(
        first_argv(&plan.tabs[0])
            .windows(2)
            .any(|pair| { pair[0].as_str() == "--agent-channel" && pair[1].as_str() == "design" }),
        "resume argv re-stamps the named channel: {:?}",
        first_argv(&plan.tabs[0])
    );
}
