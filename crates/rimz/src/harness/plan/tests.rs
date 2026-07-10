use std::collections::BTreeMap;
use std::path::Path;

use jiff::Timestamp;

use super::*;
use crate::agents::AgentStatus;
use crate::config::{Profile, Team, TeamsConfig};
use crate::harness::run::PermissionMode;
use crate::harness::spec::Column;
use crate::ids::AgentKind;

fn role_binding(role: &str) -> RoleBinding {
    RoleBinding {
        role: role.to_owned(),
        profile: format!("{role}-profile"),
        mode: None,
        model: None,
        effort: None,
        budget: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        args: None,
    }
}

fn agent_cell_with_role(role: Option<&str>) -> Cell {
    Cell::Agent {
        kind: AgentKind::new_unchecked("claude"),
        args: Vec::new(),
        mode: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: role.map(|role| format!("{role}-profile")),
        role: role.map(ToOwned::to_owned),
        model: None,
        effort: None,
        budget: None,
    }
}

fn assert_arg_pair(argv: &[String], flag: &str, value: &str) {
    assert!(
        argv.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value),
        "missing `{flag} {value}` in {argv:?}"
    );
}

#[test]
fn launch_placement_matrix() {
    use Placement::{NewPane, NewTab, SamePane};

    for (name, new_tab, new_pane, policy, is_worktree, single_cell, has_pane, expected) in [
        (
            "auto single same-pane",
            false,
            false,
            LaunchPlacement::Auto,
            false,
            true,
            true,
            SamePane,
        ),
        (
            "auto worktree tab",
            false,
            false,
            LaunchPlacement::Auto,
            true,
            true,
            true,
            NewTab,
        ),
        (
            "auto multi tab",
            false,
            false,
            LaunchPlacement::Auto,
            false,
            false,
            true,
            NewTab,
        ),
        (
            "auto no ambient pane tab",
            false,
            false,
            LaunchPlacement::Auto,
            false,
            true,
            false,
            NewTab,
        ),
        (
            "explicit tab",
            true,
            false,
            LaunchPlacement::Auto,
            false,
            true,
            true,
            NewTab,
        ),
        (
            "explicit pane",
            false,
            true,
            LaunchPlacement::Auto,
            true,
            true,
            true,
            NewPane,
        ),
        (
            "pane policy split",
            false,
            false,
            LaunchPlacement::Pane,
            false,
            true,
            true,
            NewPane,
        ),
        (
            "pane policy worktree tab",
            false,
            false,
            LaunchPlacement::Pane,
            true,
            true,
            true,
            NewTab,
        ),
        (
            "pane policy multi tab",
            false,
            false,
            LaunchPlacement::Pane,
            false,
            false,
            true,
            NewTab,
        ),
        (
            "pane policy no ambient pane tab",
            false,
            false,
            LaunchPlacement::Pane,
            false,
            true,
            false,
            NewTab,
        ),
        (
            "tab policy",
            false,
            false,
            LaunchPlacement::Tab,
            false,
            true,
            true,
            NewTab,
        ),
    ] {
        assert_eq!(
            resolve_placement(
                new_tab,
                new_pane,
                policy,
                is_worktree,
                single_cell,
                has_pane
            )
            .unwrap(),
            expected,
            "{name}"
        );
    }

    for (placement, bg, allow_in_place, expected) in [
        (SamePane, true, true, NewPane),
        (SamePane, false, false, NewPane),
        (NewTab, false, false, NewTab),
        (NewPane, true, false, NewPane),
    ] {
        assert_eq!(
            apply_in_place_downgrade(placement, bg, allow_in_place),
            expected
        );
    }

    let err = resolve_placement(false, true, LaunchPlacement::Auto, false, false, true)
        .expect_err("multi-cell new-pane");
    assert!(err.to_string().contains("single agent cell"), "{err:#}");

    let err = resolve_placement(false, true, LaunchPlacement::Auto, false, true, false)
        .expect_err("paneless new-pane");
    assert!(err.to_string().contains("inside the room"), "{err:#}");
}

#[test]
fn single_role_team_launch_takes_over_caller_pane() {
    let profiles = crate::config::ProfilesConfig(BTreeMap::from([(
        "planner-profile".to_owned(),
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
    )]));
    let teams = TeamsConfig(BTreeMap::from([(
        "solo".to_owned(),
        Team {
            roles: vec![role_binding("planner")],
            leader: None,
            layout: None,
        },
    )]));

    for spec in ["solo", "solo.planner"] {
        let layout = crate::harness::spec::resolve_spec(
            Some(spec),
            &profiles,
            &crate::config::CommandsConfig::default(),
            &teams,
        )
        .expect("single-role team launch");
        let placement = apply_in_place_downgrade(
            resolve_placement(
                false,
                false,
                LaunchPlacement::Auto,
                false,
                layout.agent_cells().count() == 1,
                true,
            )
            .unwrap(),
            false,
            true,
        );

        assert_eq!(crate::harness::spec::spec_team(spec, &teams), Some("solo"));
        assert_eq!(placement, Placement::SamePane);
    }
}

#[test]
fn launch_request_names_and_metadata() {
    let layout = LayoutSpec::single(Cell::Agent {
        kind: AgentKind::new_unchecked("codex"),
        args: Vec::new(),
        mode: Some(PermissionMode::Yolo),
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: Some("codex-coder".to_owned()),
        role: Some("coder".to_owned()),
        model: Some("gpt-5-codex".to_owned()),
        effort: Some("high".to_owned()),
        budget: None,
    });

    let requests = launch_identity_requests(
        &layout,
        Some("docs"),
        None,
        Some("forge"),
        None,
        Some("design"),
        Some(("draft it", 0)),
    )
    .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].name,
        AgentLaunchName::Explicit("docs".to_owned())
    );
    assert_eq!(requests[0].kind.as_str(), "codex");
    assert_eq!(requests[0].launch.profile.as_deref(), Some("codex-coder"));
    assert_eq!(requests[0].launch.mode, Some(PermissionMode::Yolo));
    assert_eq!(requests[0].launch.role.as_deref(), Some("coder"));
    assert_eq!(requests[0].launch.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(requests[0].launch.effort.as_deref(), Some("high"));
    assert_eq!(requests[0].launch.team.as_deref(), Some("forge"));
    assert_eq!(requests[0].launch.channel.as_deref(), Some("design"));
    assert_eq!(requests[0].prompt.as_deref(), Some("draft it"));

    let requests =
        launch_identity_requests(&layout, None, Some("my_feature"), None, None, None, None)
            .unwrap();
    assert_eq!(
        requests[0].name,
        AgentLaunchName::Soft("my_feature".to_owned())
    );
    assert_eq!(
        launch_identity_requests(&layout, None, None, None, None, None, None).unwrap()[0].name,
        AgentLaunchName::Mint
    );
    assert!(
        launch_identity_requests(&layout, Some("my_feature"), None, None, None, None, None)
            .unwrap_err()
            .to_string()
            .contains("invalid agent name")
    );
}

#[test]
fn launch_identity_requests_stamp_team_and_inline_cohort_order() {
    let team_roles = vec![
        role_binding("planner"),
        role_binding("coder"),
        role_binding("reviewer"),
    ];
    let team_layout = LayoutSpec {
        columns: vec![Column {
            rows: vec![
                agent_cell_with_role(Some("coder")),
                agent_cell_with_role(Some("planner")),
                agent_cell_with_role(None),
            ],
            stacked: false,
        }],
    };

    let requests = launch_identity_requests(
        &team_layout,
        None,
        None,
        Some("forge"),
        Some(&team_roles),
        None,
        Some(("implement", 1)),
    )
    .unwrap();
    assert_eq!(requests[0].launch.launch_ordinal, Some(1));
    assert_eq!(requests[1].launch.launch_ordinal, Some(0));
    assert_eq!(requests[2].launch.launch_ordinal, None);
    assert_eq!(requests[0].prompt, None);
    assert_eq!(requests[1].prompt.as_deref(), Some("implement"));
    assert!(
        requests
            .iter()
            .all(|request| request.launch.launch_group.is_none())
    );

    let inline = crate::harness::spec::parse_layout_spec(
        "claude:planner,codex:coder",
        &crate::config::ProfilesConfig::default(),
        &crate::config::CommandsConfig::default(),
    )
    .unwrap();
    let requests = launch_identity_requests(&inline, None, None, None, None, None, None).unwrap();
    let group = requests[0].launch.launch_group.as_deref().unwrap();
    assert!(group.starts_with("launch_"));
    assert_eq!(requests[1].launch.launch_group.as_deref(), Some(group));
    assert_eq!(requests[0].launch.launch_ordinal, Some(0));
    assert_eq!(requests[1].launch.launch_ordinal, Some(1));

    let single = LayoutSpec::single(agent_cell_with_role(None));
    let requests = launch_identity_requests(&single, None, None, None, None, None, None).unwrap();
    assert_eq!(requests[0].launch.launch_group, None);
    assert_eq!(requests[0].launch.launch_ordinal, None);
}

#[test]
fn layout_panes_put_the_prompt_only_on_the_leader_agent() {
    let layout = LayoutSpec {
        columns: vec![
            Column {
                rows: vec![Cell::agent(AgentKind::new_unchecked("claude"))],
                stacked: false,
            },
            Column {
                rows: vec![
                    Cell::shell(),
                    Cell::agent(AgentKind::new_unchecked("codex")),
                ],
                stacked: false,
            },
        ],
    };
    let identity = |kind: &str, name: &str| AgentLaunchIdentity {
        kind: AgentKind::new_unchecked(kind),
        agent_id: AgentSessionId::from(format!("launch_{name}")),
        name: name.to_owned(),
        name_explicit: false,
        launch: crate::agents::LaunchParams::default(),
        run_id: None,
        prompt: None,
    };
    let panes = layout_panes_with_names(
        &layout,
        LayoutPaneParams {
            cwd: Path::new("/tmp/project"),
            prompt: Some("lead this"),
            prompt_agent_index: Some(1),
            cleanup_worktree: false,
            in_place: false,
            team: None,
            channel: None,
            resume_seeds: None,
        },
        &[identity("claude", "first"), identity("codex", "leader")],
    )
    .unwrap();

    assert!(
        !panes.columns[0].panes[0]
            .argv
            .iter()
            .any(|arg| arg == "--prompt")
    );
    assert_arg_pair(&panes.columns[1].panes[1].argv, "--prompt", "lead this");
}

#[test]
fn pane_command_stamps_cli_identity_and_close_policy() {
    let cell = Cell::Agent {
        kind: AgentKind::new_unchecked("claude"),
        args: Vec::new(),
        mode: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: Some("claude-planner".to_owned()),
        role: Some("planner".to_owned()),
        model: Some("claude-sonnet".to_owned()),
        effort: Some("high".to_owned()),
        budget: None,
    };
    let launch = AgentLaunchIdentity {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("launch_0123456789abcdef0123456789abcdef"),
        name: "swift-otter".to_owned(),
        name_explicit: false,
        launch: crate::agents::LaunchParams {
            launch_group: Some("launch_group_1".to_owned()),
            launch_ordinal: Some(2),
            ..Default::default()
        },
        run_id: None,
        prompt: None,
    };

    let pane = pane_cmd_with_name(
        &cell,
        PaneCmdOptions {
            rimz_bin: Path::new("/usr/bin/rimz"),
            cwd: Path::new("/tmp/project"),
            prompt: None,
            cleanup_worktree: false,
            in_place: false,
            team: Some("forge"),
            channel: Some("design"),
            launch: Some(&launch),
            resume_seed: None,
        },
    )
    .unwrap();

    for (flag, value) in [
        ("--agent-name", "swift-otter"),
        ("--launch-id", "launch_0123456789abcdef0123456789abcdef"),
        ("--agent-profile", "claude-planner"),
        ("--agent-role", "planner"),
        ("--agent-team", "forge"),
        ("--launch-group", "launch_group_1"),
        ("--launch-ordinal", "2"),
        ("--agent-channel", "design"),
        ("--agent-model", "claude-sonnet"),
        ("--agent-effort", "high"),
    ] {
        assert_arg_pair(&pane.argv, flag, value);
    }
    assert!(pane.argv.iter().any(|arg| arg == "--close-pane-on-exit"));

    for (cleanup_worktree, in_place) in [(false, true), (true, false)] {
        let pane = pane_cmd_with_name(
            &cell,
            PaneCmdOptions {
                rimz_bin: Path::new("/usr/bin/rimz"),
                cwd: Path::new("/tmp/project"),
                prompt: None,
                cleanup_worktree,
                in_place,
                team: Some("forge"),
                channel: None,
                launch: Some(&launch),
                resume_seed: None,
            },
        )
        .unwrap();
        assert!(!pane.argv.iter().any(|arg| arg == "--close-pane-on-exit"));
    }
}

#[test]
fn pane_command_resume_replays_prior_identity_without_launch_preset() {
    let cell = Cell::Agent {
        kind: AgentKind::new_unchecked("claude"),
        args: vec!["--ignored".to_owned()],
        mode: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        profile: Some("new-profile".to_owned()),
        role: Some("new-role".to_owned()),
        model: Some("new-model".to_owned()),
        effort: Some("new-effort".to_owned()),
        budget: None,
    };
    let mut agent = crate::testkit::agent_state("claude", "sess-1", Timestamp::now());
    agent.status = AgentStatus::Idle;
    agent.name = Some("swift-otter".to_owned());
    agent.profile = Some("prior-profile".to_owned());
    agent.role = Some("prior-role".to_owned());
    agent.team = Some("forge".to_owned());
    agent.launch_group = Some("launch_group_1".to_owned());
    agent.launch_ordinal = Some(1);
    agent.channel = Some("design".to_owned());
    agent.model = Some("old-model".to_owned());
    agent.effort = Some("old-effort".to_owned());
    let seed = CohortSeed::Resume(Box::new(agent));

    let pane = pane_cmd_with_name(
        &cell,
        PaneCmdOptions {
            rimz_bin: Path::new("/usr/bin/rimz"),
            cwd: Path::new("/tmp/project"),
            prompt: Some("ignored prompt"),
            cleanup_worktree: false,
            in_place: false,
            team: Some("new-team"),
            channel: Some("new-channel"),
            launch: None,
            resume_seed: Some(&seed),
        },
    )
    .unwrap();

    for (flag, value) in [
        ("--resume", "sess-1"),
        ("--agent-name", "swift-otter"),
        ("--agent-profile", "prior-profile"),
        ("--agent-role", "prior-role"),
        ("--agent-team", "forge"),
        ("--launch-group", "launch_group_1"),
        ("--launch-ordinal", "1"),
        ("--agent-channel", "design"),
    ] {
        assert_arg_pair(&pane.argv, flag, value);
    }
    assert!(pane.argv.iter().any(|arg| arg == "--close-pane-on-exit"));
    assert!(!pane.argv.iter().any(|arg| matches!(
        arg.as_str(),
        "--agent-model" | "--agent-effort" | "--prompt"
    )));
}
