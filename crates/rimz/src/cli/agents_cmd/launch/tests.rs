use super::*;
use rimz::store::run::ReportTo;

#[test]
fn resume_prompt_is_queued_to_the_leader_before_registration() {
    use rimz::harness::plan::CohortSeed;
    use rimz::store::message::{DeliveryGate, MessageSender};
    for fresh in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let id = rimz::WorkspaceId::from_project_root(dir.path());
        let store = rimz::Store::open(
            rimz::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap(),
            rimz::RuntimePaths::under(id, &dir.path().join("runtime")).unwrap(),
        )
        .unwrap();
        let mut leader = test_agent("leader-session");
        leader.name = Some("leader".into());
        let seeds = vec![
            CohortSeed::Fresh,
            CohortSeed::Resume(test_agent("worker-session").into()),
            if fresh {
                CohortSeed::Fresh
            } else {
                CohortSeed::Resume(leader.into())
            },
        ];
        let identities = vec![
            launch_identity("codex", "worker"),
            launch_identity("codex", "leader"),
        ];
        queue_resume_prompt(&store, "room", &seeds, &identities, "say hi", 2).unwrap();
        let messages = store.list_messages().unwrap();
        assert_eq!(
            messages.len(),
            1,
            "prompt must be durable before any pane opens"
        );
        let message = &messages[0];
        assert_eq!(message.sender, MessageSender::Human);
        assert_eq!(message.gate, DeliveryGate::Done);
        assert_eq!(message.text, "say hi");
        assert_eq!(message.agent_name.as_deref(), Some("leader"));
        assert_eq!(
            message.agent_id.as_str(),
            if fresh {
                "launch-leader"
            } else {
                "leader-session"
            }
        );
        assert!(message.enter);
    }
}

#[test]
fn launch_prompt_enrolls_only_the_prompted_peer_before_registration() {
    let dir = tempfile::tempdir().unwrap();
    let id = rimz::WorkspaceId::from_project_root(dir.path());
    let store = rimz::Store::open(
        rimz::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap(),
        rimz::RuntimePaths::under(id, &dir.path().join("runtime")).unwrap(),
    )
    .unwrap();
    let mut identities = [
        launch_identity("claude", "worker"),
        launch_identity("claude", "leader"),
    ];
    for identity in &mut identities {
        identity.launch.launched_by = Some(Box::new(rimz::agents::LaunchedBy {
            kind: AgentKind::new_unchecked("codex"),
            agent_id: "launcher".into(),
        }));
    }
    identities[1].prompt = Some("launch task".into());
    let requests = identities
        .iter()
        .map(|identity| rimz::store::writer::AgentLaunchRequest {
            login: rimz::store::writer::LaunchLogin::RoomDefault,
            kind: identity.kind.clone(),
            agent_id: identity.agent_id.clone(),
            name: rimz::store::writer::AgentLaunchName::Explicit(identity.name.clone()),
            launch: identity.launch.clone(),
            run_id: None,
            prompt: identity.prompt.clone(),
        })
        .collect::<Vec<_>>();
    let batch = store
        .begin_agent_launch_batch(
            &requests,
            AgentLaunchScope {
                session_name: "room".into(),
                cwd: dir.path().to_owned(),
                branch: None,
                description: None,
            },
        )
        .unwrap();
    let (peer, run) =
        prepare_peer_prompt(&store, batch.identities(), dir.path(), ReportTo::Launcher)
            .unwrap()
            .expect("prompt leader owes a report before opening the pane");
    assert_eq!(run.status, rimz::store::run::RunStatus::Pending);
    assert_eq!(run.agent_name.as_deref(), Some("leader"));
    assert_eq!(run.peer.as_ref().unwrap().launch_id, identities[1].agent_id);
    assert!(run.agent_id.is_none());
    assert_eq!(rimz::harness::run::list(store.paths()).unwrap().len(), 1);
    assert_eq!(
        run.reader, None,
        "an unresolved launcher leaves the reader to the fallback"
    );
    let mut receipt = Vec::new();
    write_peer_receipt(
        &mut receipt,
        batch.identities(),
        Some(&run),
        None,
        store.paths(),
    )
    .unwrap();
    let receipt = String::from_utf8(receipt).unwrap();
    assert!(receipt.contains("AGENT_REPORT"), "{receipt}");
    assert!(receipt.contains("rimz agents wait @leader\n"), "{receipt}");
    assert!(
        receipt.contains(
            &store
                .paths()
                .out_reader_dir(Some("leader"))
                .join(format!("leader.{}.output", run.run_id))
                .display()
                .to_string()
        ),
        "{receipt}"
    );
    let (_, reused) = prepare_peer_prompt(&store, batch.identities(), dir.path(), ReportTo::Nobody)
        .unwrap()
        .unwrap();
    assert_eq!(
        (&reused.run_id, reused.report_to),
        (&run.run_id, ReportTo::Launcher),
        "a reused open run keeps its recorded policy"
    );
    rimz::harness::run::fail_peer_run(&store, &peer, "launch failed").unwrap();
    for identity in &mut identities {
        identity.prompt = None;
    }
    assert!(
        prepare_peer_prompt(&store, &identities, dir.path(), ReportTo::Launcher)
            .unwrap()
            .is_none()
    );
    identities[1].prompt = Some("human task".into());
    identities[1].launch.launched_by = None;
    assert!(
        prepare_peer_prompt(&store, &identities, dir.path(), ReportTo::Launcher)
            .unwrap()
            .is_none()
    );
    assert_eq!(rimz::harness::run::list(store.paths()).unwrap().len(), 1);

    let (_, detached) =
        prepare_peer_prompt(&store, batch.identities(), dir.path(), ReportTo::Nobody)
            .unwrap()
            .unwrap();
    assert_eq!(detached.report_to, ReportTo::Nobody);
    assert_eq!(
        rimz::harness::run::load(store.paths(), &detached.run_id)
            .unwrap()
            .report_to,
        ReportTo::Nobody
    );
    let mut receipt = Vec::new();
    write_peer_receipt(
        &mut receipt,
        batch.identities(),
        Some(&detached),
        Some("auth"),
        store.paths(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(receipt).unwrap(),
        format!(
            "Running detached in its own pane: no AGENT_REPORT will reach you for this launch turn.\nIts response lands at {} when this turn settles. To read it: rimz agents wait @leader#auth\nIt stays open after this turn. A message of yours reports as usual only if it lands after this turn settles. To follow up: rimz message @leader#auth '<text>'\n",
            store
                .paths()
                .out_reader_dir(Some("leader"))
                .join(format!("leader.{}.output", detached.run_id))
                .display(),
        )
    );
}

#[test]
fn peer_receipt_names_the_host_out_path_under_its_reader() {
    let dir = tempfile::tempdir().unwrap();
    let paths =
        rimz::StatePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path())
            .unwrap();
    let mut run = RunRecord::new(
        paths.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "task".into(),
        dir.path().to_owned(),
    );
    run.agent_name = Some("peer".into());
    run.peer = Some(rimz::store::run::PeerRun {
        launch_id: "peer-launch".into(),
        opened_by: Vec::new(),
    });
    run.reader = Some("launcher".into());
    let expected = paths
        .out_reader_dir(Some("launcher"))
        .join(format!("peer.{}.output", run.run_id));
    let receipt = |run: &RunRecord, channel| {
        let mut receipt = Vec::new();
        write_peer_receipt(&mut receipt, &[], Some(run), channel, &paths).unwrap();
        String::from_utf8(receipt).unwrap()
    };
    assert_eq!(
        receipt(&run, Some("auth")),
        format!(
            "Running in its own pane. Keep working or end your turn: one AGENT_REPORT reaches you once every agent you launched has settled, and another after each turn a message of yours opens.\nIts response lands at {} when this turn settles. To block instead: rimz agents wait @peer#auth\nIt stays open after this turn. To follow up: rimz message @peer#auth '<text>'\n",
            expected.display()
        )
    );
    let unlaned = receipt(&run, None);
    assert!(
        unlaned.contains("rimz agents wait @peer\n")
            && unlaned.contains("rimz message @peer '<text>'\n"),
        "{unlaned}"
    );
    let mut empty = Vec::new();
    write_peer_receipt(
        &mut empty,
        &[launch_identity("claude", "unprompted")],
        None,
        None,
        &paths,
    )
    .unwrap();
    assert!(empty.is_empty());

    run.team = Some(rimz::store::run::TeamRun {
        launch_id: "leader-launch".into(),
        instance: "forge#feat-x".into(),
    });
    assert_eq!(
        receipt(&run, Some("feat-x")),
        format!(
            "@peer leads forge#feat-x. Keep working or end your turn: one TEAM_REPORT reaches you when its board reaches Done; stop it after: rimz teams stop forge#feat-x\nThe leader's final response lands at {}. To block instead: rimz teams wait forge#feat-x\n",
            expected.display()
        )
    );

    run.report_to = ReportTo::Nobody;
    assert_eq!(
        receipt(&run, Some("feat-x")),
        format!(
            "@peer leads forge#feat-x, detached: no TEAM_REPORT will reach you.\nThe leader's final response lands at {}. To block until Done: rimz teams wait forge#feat-x\n",
            expected.display()
        )
    );
}

#[test]
fn launch_receipt_carries_either_the_hints_or_the_peer_lines() {
    let dir = tempfile::tempdir().unwrap();
    let paths =
        rimz::StatePaths::under(rimz::WorkspaceId::from_project_root(dir.path()), dir.path())
            .unwrap();
    let mut run = RunRecord::new(
        paths.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "task".into(),
        dir.path().to_owned(),
    );
    run.agent_name = Some("peer".into());
    run.peer = Some(rimz::store::run::PeerRun {
        launch_id: "peer-launch".into(),
        opened_by: Vec::new(),
    });
    let receipt = |peer: Option<RunRecord>| {
        let layout = LaunchedLayout {
            identities: vec![launch_identity("claude", "peer")],
            leader_index: Some(0),
            team: None,
            channel: Some("auth".into()),
            cwd: dir.path().to_owned(),
            in_place: false,
            peer,
        };
        let mut output = anstream::StripStream::new(Vec::new());
        layout.write_receipt(&mut output, &paths).unwrap();
        String::from_utf8(output.into_inner()).unwrap()
    };

    let prompted = receipt(Some(run));
    assert!(!prompted.contains("Reach:"), "{prompted}");
    assert!(!prompted.contains("Wait:"), "{prompted}");
    assert!(
        prompted.contains("\n\nRunning in its own pane. ")
            && prompted.contains("To block instead: rimz agents wait @peer#auth\n")
            && prompted.ends_with("To follow up: rimz message @peer#auth '<text>'\n"),
        "{prompted}"
    );

    let plain = receipt(None);
    assert!(
        plain.ends_with(
            "\nReach: rimz message @peer#auth '<text>'\nWait:  rimz agents wait @peer#auth\n"
        ),
        "{plain}"
    );
    assert!(!plain.contains("Running"), "{plain}");
}

#[test]
fn team_launch_receipt_is_compact() {
    let mut identities = [
        launch_identity("claude", "planner"),
        launch_identity("codex", "coder"),
    ];
    identities[0].launch.role = Some("planner".to_owned());
    identities[1].launch.role = Some("coder".to_owned());
    identities[0].launch.model = Some("fable".to_owned());
    identities[1].launch.model = Some("gpt-6-astra".to_owned());
    identities[0].prompt = Some("Read the handoff at /tmp/handoff-feat-x.md.".to_owned());
    let team = rimz::config::Team {
        leader: Some("planner".to_owned()),
        stages: vec![
            "Explore".to_owned(),
            "Plan".to_owned(),
            "Implement".to_owned(),
        ],
        ..Default::default()
    };
    let mut output = anstream::StripStream::new(Vec::new());

    write_launch_receipt(
        &mut output,
        &LaunchReceipt {
            team: Some(("forge", &team)),
            channel: Some("feat-x"),
            cwd: Path::new("/repo-worktrees/feat-x"),
            identities: &identities,
            leader_index: Some(0),
            terminal_width: 100,
            hints: true,
        },
    )
    .unwrap();

    insta::with_settings!({snapshot_path => "../snapshots"}, {
        insta::assert_snapshot!(String::from_utf8(output.into_inner()).unwrap());
    });

    let mut output = anstream::StripStream::new(Vec::new());
    write_launch_receipt(
        &mut output,
        &LaunchReceipt {
            team: Some((
                "forge",
                &rimz::config::Team {
                    roles: vec![rimz::config::RoleBinding {
                        owns: Vec::new(),
                        flip_compact: None,
                        idle_compact: None,
                        keep_warm: None,
                        role: "coder".to_owned(),
                        profile: "codex".to_owned(),
                        mode: None,
                        model: None,
                        effort: None,
                        budget: None,
                        auto_compact: None,
                        system_prompt_file: None,
                        append_system_prompt_files: Vec::new(),
                        args: None,
                        signals: vec![rimz::config::TeamSignalBinding {
                            signal: "ci.failed".to_owned(),
                            matches: Default::default(),
                            prompt: None,
                        }],
                    }],
                    ..Default::default()
                },
            )),
            channel: Some("feat-x"),
            cwd: Path::new("/repo-worktrees/feat-x"),
            identities: &identities,
            leader_index: None,
            terminal_width: 100,
            hints: true,
        },
    )
    .unwrap();
    assert!(
        String::from_utf8(output.into_inner())
            .unwrap()
            .contains("  signals   ci.failed → @coder\n")
    );
}

#[test]
fn plain_launch_receipt_uses_the_first_member_without_a_team_check() {
    let mut identities = [launch_identity("codex", "worker")];
    identities[0].prompt = Some("Read  the\n handoff.".to_owned());
    let mut output = anstream::StripStream::new(Vec::new());

    write_launch_receipt(
        &mut output,
        &LaunchReceipt {
            team: None,
            channel: None,
            cwd: Path::new("/repo"),
            identities: &identities,
            leader_index: Some(0),
            terminal_width: 100,
            hints: true,
        },
    )
    .unwrap();

    let output = String::from_utf8(output.into_inner()).unwrap();
    assert!(output.contains("launched @worker (/repo)"));
    assert!(!output.contains("Check:"));
    assert!(!output.contains("leader"));
    assert!(!output.contains("starting"));
    assert!(!output.contains("board"));
    assert!(output.contains("  @worker   codex  -"));
    assert!(output.contains("  prompt    → @worker  \"Read the handoff.\""));
    assert!(output.contains("Reach: rimz message @worker '<text>'"));
    assert!(output.contains("Wait:  rimz agents wait @worker\n"));

    let layout = rimz::harness::spec::parse_layout_spec(
        "claude,claude",
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let leader_index = rimz::harness::spec::prompt_leader(&layout, None).ok();
    assert!(leader_index.is_none());
    let identities = [
        launch_identity("claude", "first-peer"),
        launch_identity("claude", "second-peer"),
    ];
    let mut output = anstream::StripStream::new(Vec::new());
    write_launch_receipt(
        &mut output,
        &LaunchReceipt {
            team: None,
            channel: Some("parallel"),
            cwd: Path::new("/repo"),
            identities: &identities,
            leader_index,
            terminal_width: 100,
            hints: true,
        },
    )
    .unwrap();
    let output = String::from_utf8(output.into_inner()).unwrap();
    assert!(output.contains("Reach: rimz message @first-peer#parallel '<text>'"));
    let wait = output
        .lines()
        .find_map(|line| line.strip_prefix("Wait:  "))
        .unwrap();
    assert_eq!(wait, "rimz agents wait @first-peer#parallel");
    <crate::cli::Cli as clap::Parser>::try_parse_from(shlex::split(wait).unwrap()).unwrap();
    assert!(!output.contains("leader"));
    assert!(!output.contains("prompt"));
}

#[test]
fn team_tab_focuses_the_leader_pane_past_command_cells() {
    let team = toml::from_str::<rimz::config::Team>(
        r#"leader = "planner"
roles = [{role = "coder", profile = "codex"}, {role = "planner", profile = "claude"}]"#,
    )
    .unwrap();
    let layout = rimz::harness::spec::parse_layout_spec(
        "codex:coder+term,claude:planner",
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    assert_eq!(team_leader_pane(&layout, Some(&team)), 2);
    assert_eq!(team_leader_pane(&layout, None), 0);
}

#[test]
fn team_launch_receipt_marks_the_implicit_leader() {
    let mut identities = [
        launch_identity("codex", "coder"),
        launch_identity("claude", "planner"),
    ];
    identities[0].launch.role = Some("coder".to_owned());
    identities[1].launch.role = Some("planner".to_owned());
    let team = toml::from_str::<rimz::config::Team>(
        r#"roles = [{role = "planner", profile = "claude"}, {role = "coder", profile = "codex"}]"#,
    )
    .unwrap();
    let layout = rimz::harness::spec::parse_layout_spec(
        "codex:coder,claude:planner",
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let leader_index = rimz::harness::spec::prompt_leader(&layout, Some(&team)).unwrap();
    assert_eq!(leader_index, 1);
    let mut output = anstream::StripStream::new(Vec::new());

    write_launch_receipt(
        &mut output,
        &LaunchReceipt {
            team: Some(("forge", &team)),
            channel: Some("feat-x"),
            cwd: Path::new("/repo-worktrees/feat-x"),
            identities: &identities,
            leader_index: Some(leader_index),
            terminal_width: 100,
            hints: true,
        },
    )
    .unwrap();

    let output = String::from_utf8(output.into_inner()).unwrap();
    let planner = output
        .lines()
        .find(|line| line.contains("@planner"))
        .unwrap();
    let coder = output.lines().find(|line| line.contains("@coder")).unwrap();
    assert!(planner.contains("<- leader"));
    assert!(!coder.contains("leader"));
    assert!(!output.contains("stages"));
    assert!(!output.contains("prompt"));
    assert!(!output.contains("branch"));
    assert!(output.contains("  board     blackboard.md"));
    assert!(!output.contains("Reach:"));
}

#[test]
fn team_launch_receipt_uses_prompt_leader_and_clips_the_prompt() {
    let layout = rimz::harness::spec::parse_layout_spec(
        "codex,claude",
        &Default::default(),
        &Default::default(),
    )
    .unwrap();
    let team = rimz::config::Team {
        leader: Some("claude".to_owned()),
        ..Default::default()
    };
    let leader_index = rimz::harness::spec::prompt_leader(&layout, Some(&team)).unwrap();
    assert_eq!(leader_index, 1);
    let mut identities = [
        launch_identity("codex", "worker"),
        launch_identity("claude", "stable-haven"),
    ];
    identities[1].launch.role = Some("planner".to_owned());
    identities[1].prompt =
        Some("Read  \"the handoff\"\nthen design a fix for the team.".to_owned());
    let receipt = LaunchReceipt {
        team: Some(("forge", &team)),
        channel: Some("feat-x"),
        cwd: Path::new("/repo-worktrees/feat-x"),
        identities: &identities,
        leader_index: Some(leader_index),
        terminal_width: 52,
        hints: true,
    };
    let mut output = anstream::StripStream::new(Vec::new());
    write_launch_receipt(&mut output, &receipt).unwrap();
    let output = String::from_utf8(output.into_inner()).unwrap();
    let prompt = output
        .lines()
        .find(|line| line.starts_with("  prompt"))
        .unwrap();
    assert!(prompt.starts_with("  prompt    → @planner  \"Read \\\"the handoff\\\" then"));
    assert!(prompt.ends_with('…'));
    assert_eq!(prompt.chars().count(), 52);
    assert!(
        output
            .lines()
            .any(|line| line.starts_with("  @planner") && line.ends_with("leader"))
    );
    assert!(
        !output
            .lines()
            .any(|line| line.starts_with("  @worker") && line.contains("leader"))
    );
    assert!(!output.contains("Check:"));
    assert!(!output.contains("Reach:"));
    assert!(!output.contains("Wait:"));
    assert!(!output.contains("starting"));

    let mut output = anstream::StripStream::new(Vec::new());
    write_launch_receipt(
        &mut output,
        &LaunchReceipt {
            terminal_width: 100,
            ..receipt
        },
    )
    .unwrap();
    let output = String::from_utf8(output.into_inner()).unwrap();
    assert!(output.contains(
        "  prompt    → @planner  \"Read \\\"the handoff\\\" then design a fix for the team.\""
    ));
}

#[test]
fn resume_hint_uses_the_first_resumed_member_without_fresh_identities() {
    let mut agent = test_agent("sess-planner");
    agent.name = Some("stable-haven".to_owned());
    agent.role = Some("planner".to_owned());
    let plan = rimz::harness::plan::CohortResumePlan {
        seeds: vec![rimz::harness::plan::CohortSeed::Resume(Box::new(agent))],
        cwd: Some(PathBuf::from("/repo")),
        channel: Some("feat-x".to_owned()),
        fresh: Vec::new(),
        launch_group: None,
    };

    assert_eq!(resume_hint_handle(&plan, &[]), Some("planner"));
    let mut output = Vec::new();
    write_resume_receipt(&mut output, &plan, Some("forge"), Some("feat-x"), &[], None).unwrap();
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("\n\nCheck: rimz teams show forge#feat-x"));
    assert!(output.contains("Reach: rimz message @planner#feat-x '<text>'"));
    assert!(
        output.contains("Wait:  rimz loop add team-idle --wait @me --signal team.idle --match instance=forge#feat-x --once")
    );
    let wait = output
        .lines()
        .find_map(|line| line.strip_prefix("Wait:  "))
        .unwrap();
    <crate::cli::Cli as clap::Parser>::try_parse_from(shlex::split(wait).unwrap()).unwrap();
}

#[test]
fn resume_in_the_lane_directory_stays_in_the_origin_pane() {
    let project = Path::new("/repo");
    let worktree = Path::new("/repo-worktrees/single-card");

    // The dropped-to-shell origin pane: launch dir is the cohort cwd, so
    // in-place placement stays available despite the lane channel.
    assert!(!resume_outside_launch_dir(
        Some("single-card"),
        worktree,
        project,
        worktree,
        Some(worktree),
    ));

    // From anywhere else the lane resume still opens its own tab.
    assert!(resume_outside_launch_dir(
        Some("single-card"),
        worktree,
        project,
        project,
        Some(project),
    ));
    assert!(resume_outside_launch_dir(
        Some("single-card"),
        worktree,
        project,
        worktree,
        None,
    ));
}

#[test]
fn resume_launch_dir_comparison_is_lexically_normalized() {
    let project = Path::new("/repo");
    let worktree = Path::new("/repo-worktrees/single-card");
    let launch_dir = Path::new("/repo-worktrees/../repo-worktrees/single-card");

    assert!(!resume_outside_launch_dir(
        Some("single-card"),
        worktree,
        project,
        worktree,
        Some(launch_dir),
    ));
}

#[test]
fn worktree_filter_matches_normalized_agent_paths() {
    let target = rimz::utils::path::normalize_path_lexical(Path::new("/repo-worktrees/demo"));
    let mut agent = test_agent("sess-demo");
    agent.worktree_path = Some("/repo/../repo-worktrees/demo".to_owned());

    assert!(agent_matches_worktree_filter(&agent, &target));

    agent.worktree_path = Some("/repo-worktrees/other".to_owned());
    assert!(!agent_matches_worktree_filter(&agent, &target));

    agent.worktree_path = None;
    assert!(!agent_matches_worktree_filter(&agent, &target));
}

#[test]
fn resume_worktree_scope_resolves_named_worktree() {
    let expected = PathBuf::from("/repo-worktrees/restore-living-team");
    let called = std::cell::Cell::new(false);

    let scope = resume_worktree_scope_with(
        Some(" restore-living-team "),
        Path::new("/repo"),
        Path::new("/repo"),
        |name| {
            called.set(true);
            assert_eq!(name, "restore-living-team");
            Ok(expected.clone())
        },
    )
    .expect("named worktree scope");

    assert_eq!(scope, Some(expected));
    assert!(called.get());
}

#[test]
fn resume_named_worktree_uses_the_launch_repo_root() {
    let project_root = PathBuf::from("/rooms/marker");
    let workspace = rimz::ResolvedWorkspace {
        workspace_id: rimz::WorkspaceId::from_project_root(&project_root),
        project_root: project_root.clone(),
        cwd_project_root: Some(PathBuf::from("/repos/current")),
        root_class: rimz::workspace::RootClass::Marker,
        worktree_root: PathBuf::from("/repos/current"),
        worktree_branch: Some("main".to_owned()),
        session_name: "rimz-room".to_owned(),
        mux_hint: None,
    };

    let scope = resume_worktree_scope(
        Some("feat-x"),
        &workspace,
        &rimz::config::MachineConfig::default(),
    )
    .expect("named resume scope");

    assert_eq!(
        scope,
        Some(PathBuf::from("/repos/current/../current-worktrees/feat-x"))
    );
}

#[test]
fn resume_worktree_scope_uses_cwd_worktree_when_unnamed() {
    let worktree = Path::new("/repo-worktrees/restore-living-team");

    let scope =
        resume_worktree_scope_with(None, worktree, Path::new("/repo"), |_| -> Result<PathBuf> {
            panic!("unnamed scope must not resolve a worktree name")
        })
        .expect("cwd worktree scope");

    assert_eq!(scope.as_deref(), Some(worktree));
}

#[test]
fn resume_worktree_scope_keeps_repo_root_global_when_unnamed() {
    let scope = resume_worktree_scope_with(
        None,
        Path::new("/repo"),
        Path::new("/repo"),
        |_| -> Result<PathBuf> { panic!("repo-root scope must stay global") },
    )
    .expect("global resume scope");

    assert_eq!(scope, None);
}

#[test]
fn resume_worktree_scope_treats_bare_worktree_flag_as_unnamed() {
    let worktree = Path::new("/repo-worktrees/restore-living-team");

    let scope = resume_worktree_scope_with(
        Some("  "),
        worktree,
        Path::new("/repo"),
        |_| -> Result<PathBuf> { panic!("bare -w must not resolve a generated worktree name") },
    )
    .expect("bare worktree flag scope");

    assert_eq!(scope.as_deref(), Some(worktree));
}

fn test_agent(id: &str) -> AgentState {
    rimz::testkit::agent_state("codex", id, jiff::Timestamp::UNIX_EPOCH)
}

fn launch_identity(kind: &str, name: &str) -> AgentLaunchIdentity {
    AgentLaunchIdentity {
        kind: AgentKind::new_unchecked(kind),
        agent_id: AgentSessionId::from(format!("launch-{name}")),
        name: name.to_owned(),
        name_explicit: false,
        launch: rimz::agents::LaunchParams::default(),
        run_id: None,
        prompt: None,
    }
}
