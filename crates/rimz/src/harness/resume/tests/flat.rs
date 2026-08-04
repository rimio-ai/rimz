//! Flat rebirth resume planning: what `plan_resume` seeds into a reborn
//! session, and what it deliberately leaves out.

use super::*;

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
fn resumes_pane_backed_launched_children_with_their_ancestry() {
    let launched = AgentState {
        parent_agent_id: Some("parent".into()),
        parent_agent_kind: Some(AgentKind::new_unchecked("codex")),
        launch_depth: Some(1),
        ..agent("claude", "child", "/code/query-engine", 1)
    };

    let plan = plan(&[launched]);

    assert_eq!(plan.tabs.len(), 1);
    let request = decode_exec_request(&single_column(&plan.tabs[0])[0]);
    assert_eq!(request.identity.launch_id.as_deref(), Some("child"));
    assert_eq!(
        request.identity.params.parent_agent_id.as_deref(),
        Some("parent")
    );
    assert_eq!(
        request.identity.params.parent_agent_kind.as_deref(),
        Some("codex")
    );
    assert_eq!(request.identity.params.launch_depth, Some(1));
}

#[test]
fn resumed_peer_keeps_its_launch_generation_without_a_parent() {
    let peer = AgentState {
        launch_depth: Some(2),
        ..agent("claude", "peer", "/code/query-engine", 1)
    };

    let plan = plan(&[peer]);

    assert_eq!(plan.tabs.len(), 1);
    let request = decode_exec_request(&single_column(&plan.tabs[0])[0]);
    assert_eq!(request.identity.launch_id.as_deref(), Some("peer"));
    assert_eq!(request.identity.params.parent_agent_id, None);
    assert_eq!(request.identity.params.parent_agent_kind, None);
    assert_eq!(request.identity.params.launch_depth, Some(2));
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
    let argv = crate::harness::plan::resume_command(
        Path::new(RIMZ_BIN),
        &crate::harness::plan::ResumeLaunchIdentity::from(&agent),
        agent.channel.as_deref(),
        &crate::harness::plan::ResumeLaunchPosture::default(),
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
        request.identity.launch_id.as_deref(),
        Some("a1"),
        "pre-launch-id rows seed a stable identity from the provider session"
    );
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
