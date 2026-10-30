use super::*;
use rimz::agents::{AgentState, PermissionMode};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::run::ReportTo;
fn test_agent(id: &str) -> AgentState {
    rimz::testkit::agent_state("codex", id, jiff::Timestamp::UNIX_EPOCH)
}
use rimz::store::run::RunRecord;

#[test]
fn launch_receipt_names_accounts_and_aligns_provider_columns() {
    let mut identities = [
        launch_identity("claude", "named"),
        launch_identity("claude", "default"),
    ];
    identities[0].launch.login = Some("rimio".parse().unwrap());
    for identity in &mut identities {
        identity.launch.model = Some("fable".into());
    }
    let mut output = anstream::StripStream::new(Vec::new());
    write_launch_receipt(
        &mut output,
        &LaunchReceipt {
            team: None,
            channel: Some("auth"),
            cwd: Path::new("/repo"),
            identities: &identities,
            handles: &receipt_handles(&identities, None),
            leader_index: None,
            terminal_width: 100,
            hints: false,
        },
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(output.into_inner()).unwrap(),
        "launched 2 agents in #auth (/repo)\n\n  @named     claude@rimio  fable\n  @default   claude        fable\n\n"
    );
}

#[test]
fn launch_receipt_carries_moved_stamp_to_the_roster() {
    let mut identity = launch_identity("codex", "coder");
    identity.launch.model = Some("astra".into());
    identity.launch.tier = Some(Box::new(
        serde_json::from_value(serde_json::json!({
            "tier": "senior", "model": "astra",
            "unbuildable": [{"model": "opus", "error": "enable the skill"}]
        }))
        .unwrap(),
    ));
    let identities = [identity];
    let mut output = anstream::StripStream::new(Vec::new());
    write_launch_receipt(
        &mut output,
        &LaunchReceipt {
            team: None,
            channel: None,
            cwd: Path::new("/repo"),
            identities: &identities,
            handles: &["coder".into()],
            leader_index: None,
            terminal_width: 100,
            hints: false,
        },
    )
    .unwrap();
    let text = String::from_utf8(output.into_inner()).unwrap();
    assert!(
        text.contains("astra (senior; opus: not buildable)"),
        "{text}"
    );
}

#[test]
fn picker_receipt_names_lane_and_binding_boundary() {
    for (kind, boundary) in [
        ("claude", "once a session is picked"),
        ("codex", "at the first prompt"),
        ("kiro", "at the first prompt"),
    ] {
        let mut output = Vec::new();
        write_picker_receipt(&mut output, kind, Some("docs")).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("opened {kind} session picker in #docs; the card binds {boundary}\n")
        );
    }
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
    let store = rimz::Store::open(
        paths.clone(),
        rimz::RuntimePaths::under(paths.workspace_id.clone(), dir.path()).unwrap(),
    )
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
            pick_session: false,
            peer,
            run: None,
        };
        let mut output = anstream::StripStream::new(Vec::new());
        layout
            .write_receipt(&mut output, &store, dir.path())
            .unwrap();
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
            handles: &receipt_handles(&identities, None),
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
                        smart_compact: None,
                        keep_warm: None,
                        role: "coder".to_owned(),
                        profile: "codex".to_owned(),
                        mode: None,
                        model: None,
                        effort: None,
                        fast: None,
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
            handles: &receipt_handles(&identities, None),
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
            handles: &receipt_handles(&identities, None),
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
            handles: &receipt_handles(&identities, None),
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
fn launch_receipt_names_each_agent_by_its_list_handle() {
    let lane_agent = |name: &str| {
        let mut agent = rimz::testkit::agent_state(
            "claude",
            &format!("launch-{name}"),
            jiff::Timestamp::UNIX_EPOCH,
        );
        agent.name = Some(name.to_owned());
        agent.profile = Some("coder".to_owned());
        agent.channel = Some("auth".to_owned());
        agent
    };
    let receipt_for = |identities: &[AgentLaunchIdentity], handles: &[String]| {
        let mut output = anstream::StripStream::new(Vec::new());
        write_launch_receipt(
            &mut output,
            &LaunchReceipt {
                team: None,
                channel: Some("auth"),
                cwd: Path::new("/repo"),
                identities,
                handles,
                leader_index: Some(0),
                terminal_width: 100,
                hints: true,
            },
        )
        .unwrap();
        String::from_utf8(output.into_inner()).unwrap()
    };
    let mut identities = [launch_identity("claude", "round-binder")];
    identities[0].prompt = Some("Fix the login.".to_owned());
    let launched = lane_agent("round-binder");

    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(Path::new("/repo")),
        vec![launched.clone()],
        jiff::Timestamp::UNIX_EPOCH,
    );
    let handles = receipt_handles(&identities, Some(&snapshot));
    assert_eq!(handles, ["coder"]);
    let output = receipt_for(&identities, &handles);
    assert!(
        output.contains("launched @coder (round-binder) in #auth (/repo)"),
        "{output}"
    );
    assert!(
        output.contains("  @coder (round-binder)   claude"),
        "{output}"
    );
    assert!(output.contains("  prompt    → @coder  \"Fix the login.\""));
    assert!(output.contains("Reach: rimz message @coder#auth '<text>'"));
    assert!(output.contains("Wait:  rimz agents wait @coder#auth\n"));

    let peer = lane_agent("able-fox");
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        snapshot.workspace_id.clone(),
        vec![peer, launched],
        jiff::Timestamp::UNIX_EPOCH,
    );
    let handles = receipt_handles(&identities, Some(&snapshot));
    assert_eq!(handles, ["round-binder"]);
    let output = receipt_for(&identities, &handles);
    assert!(output.contains("launched @round-binder in #auth (/repo)"));
    assert!(output.contains("  @round-binder   claude"), "{output}");
    assert!(output.contains("Wait:  rimz agents wait @round-binder#auth\n"));
}

#[test]
fn launch_receipt_handles_round_trip_beside_running_launched_child() {
    assert_receipt_handle_round_trip_beside_launched_child(None);
}

#[test]
fn launch_receipt_handles_round_trip_beside_ended_launched_child() {
    assert_receipt_handle_round_trip_beside_launched_child(Some(jiff::Timestamp::UNIX_EPOCH));
}

#[track_caller]
fn assert_receipt_handle_round_trip_beside_launched_child(ended_at: Option<jiff::Timestamp>) {
    let identity = launch_identity("codex", "round-binder");
    let mut launched = test_agent(identity.agent_id.as_str());
    launched.name = Some(identity.name.clone());
    launched.profile = Some("gatekeeper".to_owned());
    launched.channel = Some("auth".to_owned());
    let parent = test_agent("parent");
    let mut child = launched.clone();
    child.agent_id = "child".into();
    child.name = Some("quiet-otter".to_owned());
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.launch_depth = Some(1);
    child.launch_id = Some("launch-child".into());
    child.ended_at = ended_at;
    assert!(child.is_launched_child());
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(Path::new("/repo")),
        vec![launched.clone(), parent, child],
        jiff::Timestamp::UNIX_EPOCH,
    );

    let handles = receipt_handles(&[identity], Some(&snapshot));
    let handle = format!("@{}#auth", handles[0]);
    let resolved = rimz::address::resolve_one(
        &snapshot,
        &handle,
        None,
        &rimz::address::AddressContext {
            channel: Some("auth".to_owned()),
            origin: rimz::address::ChannelOrigin::Explicit,
        },
    );
    assert_eq!(
        resolved.map(|agent| &agent.agent_id),
        Ok(&launched.agent_id),
        "{handle}"
    );
    assert_ne!(handles[0], "gatekeeper");
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
            handles: &receipt_handles(&identities, None),
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
    assert!(output.contains("  board     .rimz/team/blackboard.md"));
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
    let handles = receipt_handles(&identities, None);
    let receipt = LaunchReceipt {
        team: Some(("forge", &team)),
        channel: Some("feat-x"),
        cwd: Path::new("/repo-worktrees/feat-x"),
        identities: &identities,
        handles: &handles,
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
        spec: "forge".to_owned(),
        lane_label: "#feat-x".to_owned(),
        matching_lanes: 1,
        seeds: vec![rimz::harness::plan::CohortSeed::Resume(Box::new(agent))],
        cwd: Some(PathBuf::from("/repo")),
        channel: Some("feat-x".to_owned()),
        fresh: Vec::new(),
        refilled: Default::default(),
        launch_group: None,
    };

    let mut announcement = Vec::new();
    write_resume_announcement(&mut announcement, &plan).unwrap();
    assert_eq!(
        String::from_utf8(announcement).unwrap(),
        "resuming forge in #feat-x\n"
    );
    let mut alternatives = plan.clone();
    alternatives.matching_lanes = 2;
    let mut announcement = Vec::new();
    write_resume_announcement(&mut announcement, &alternatives).unwrap();
    assert_eq!(
        String::from_utf8(announcement).unwrap(),
        "resuming forge in #feat-x (newest of 2; others: run rimz agents resume from the project root)\n"
    );
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

#[test]
fn prompted_peer_receipt_uses_its_fallback_reader_and_detach_policy() {
    let dir = tempfile::tempdir().unwrap();
    let id = rimz::WorkspaceId::from_project_root(dir.path());
    let store = rimz::Store::open(
        rimz::StatePaths::under(id.clone(), &dir.path().join("state")).unwrap(),
        rimz::RuntimePaths::under(id, &dir.path().join("runtime")).unwrap(),
    )
    .unwrap();
    let mut run = RunRecord::new(
        store.paths().workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "launch task".into(),
        dir.path().to_owned(),
    );
    run.agent_name = Some("leader".into());
    run.peer = Some(rimz::store::run::PeerRun {
        launch_id: "launch-leader".into(),
        opened_by: Vec::new(),
    });
    let identities = [launch_identity("claude", "leader")];
    let mut receipt = Vec::new();
    write_peer_receipt(&mut receipt, &identities, Some(&run), None, store.paths()).unwrap();
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
    let mut detached = run;
    detached.report_to = ReportTo::Nobody;
    let mut receipt = Vec::new();
    write_peer_receipt(
        &mut receipt,
        &identities,
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
