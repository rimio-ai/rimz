use super::*;
use crate::config::{RoleBinding, Team};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn profile(agent: &str) -> Profile {
    Profile {
        agent: agent.to_owned(),
        mode: None,
        model: None,
        effort: None,
        budget: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        args: None,
    }
}

fn profiles(entries: impl IntoIterator<Item = (&'static str, Profile)>) -> ProfilesConfig {
    ProfilesConfig(
        entries
            .into_iter()
            .map(|(name, profile)| (name.to_owned(), profile))
            .collect(),
    )
}

fn commands(entries: impl IntoIterator<Item = (&'static str, &'static str)>) -> CommandsConfig {
    CommandsConfig(
        entries
            .into_iter()
            .map(|(name, command)| (name.to_owned(), command.to_owned()))
            .collect(),
    )
}

fn role(role: &str, profile: &str) -> RoleBinding {
    RoleBinding {
        role: role.to_owned(),
        profile: profile.to_owned(),
        mode: None,
        model: None,
        effort: None,
        budget: None,
        system_prompt_file: None,
        append_system_prompt_file: None,
        args: None,
    }
}

fn team(roles: Vec<RoleBinding>) -> Team {
    Team {
        roles,
        leader: None,
        layout: None,
    }
}

fn team_with_layout(roles: Vec<RoleBinding>, layout: &str) -> Team {
    Team {
        roles,
        leader: None,
        layout: Some(layout.to_owned()),
    }
}

fn no_profiles() -> ProfilesConfig {
    ProfilesConfig::default()
}

fn no_commands() -> CommandsConfig {
    CommandsConfig::default()
}

fn agent_cell(raw: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> AgentCell {
    let spec = parse_layout_spec(raw, profiles, commands).expect("parse agent");
    agent_at(&spec, 0, 0).clone()
}

fn agent_at(spec: &LayoutSpec, column: usize, row: usize) -> &AgentCell {
    match &spec.columns[column].rows[row] {
        Cell::Agent(agent) => agent,
        Cell::Command { .. } => panic!("agent cell"),
    }
}

#[test]
fn parses_layout_shape_and_command_precedence() {
    let commands = commands([
        ("vim", "nvim -p"),
        ("htop", "htop"),
        ("claude", "nvim"),
        ("logs:tail", "tail -f rimz.log"),
    ]);

    let spec = parse_layout_spec("term,vim+htop,claude/logs:tail", &no_profiles(), &commands)
        .expect("layout");
    assert_eq!(
        spec.columns
            .iter()
            .map(|column| (column.rows.len(), column.stacked))
            .collect::<Vec<_>>(),
        vec![(1, false), (2, false), (2, true)]
    );
    assert_eq!(spec.columns[0].rows[0], Cell::shell());
    for (column, row, expected) in [
        (1, 0, vec!["nvim", "-p"]),
        (1, 1, vec!["htop"]),
        (2, 0, vec!["nvim"]),
        (2, 1, vec!["tail", "-f", "rimz.log"]),
    ] {
        assert!(matches!(
            &spec.columns[column].rows[row],
            Cell::Command { argv }
                if argv.iter().map(String::as_str).eq(expected)
        ));
    }
    assert_eq!(
        parse_layout_spec("claude+codex/term", &no_profiles(), &no_commands()),
        Err(LayoutErr::MixedRowOperators {
            column: "claude+codex/term".to_owned(),
        })
    );
}

#[test]
fn path_executables_are_last_resort_layout_cells() {
    let spec = parse_layout_spec("sh,claude", &no_profiles(), &no_commands()).expect("layout");
    assert!(matches!(
        &spec.columns[0].rows[0],
        Cell::Command { argv } if argv == &["sh"]
    ));
    assert_eq!(agent_at(&spec, 1, 0).kind.as_str(), "claude");

    let configured_command =
        parse_layout_spec("sh", &no_profiles(), &commands([("sh", "echo shadowed")]))
            .expect("configured command");
    assert!(matches!(
        &configured_command.columns[0].rows[0],
        Cell::Command { argv } if argv == &["echo", "shadowed"]
    ));

    let configured_profile =
        parse_layout_spec("sh", &profiles([("sh", profile("claude"))]), &no_commands())
            .expect("configured profile");
    assert!(matches!(
        agent_at(&configured_profile, 0, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { profile, .. }, .. }
            if kind.as_str() == "claude" && profile.as_deref() == Some("sh")
    ));

    assert!(matches!(
        parse_layout_spec(
            "definitely-not-a-binary-xyz,claude",
            &no_profiles(),
            &no_commands()
        ),
        Err(LayoutErr::UnknownCell { cell, .. })
            if cell == "definitely-not-a-binary-xyz"
    ));
}

#[test]
fn structural_layout_grammar_table() {
    let cases = [
        (
            "claude,codex",
            Ok(vec![(false, vec!["claude"]), (false, vec!["codex"])]),
        ),
        (
            " claude + codex ",
            Ok(vec![(false, vec!["claude", "codex"])]),
        ),
        (
            " claude / codex ",
            Ok(vec![(true, vec!["claude", "codex"])]),
        ),
        (" ", Err(LayoutErr::Empty)),
        ("claude, ", Err(LayoutErr::EmptyCell("claude,".to_owned()))),
        (
            "claude+codex/term",
            Err(LayoutErr::MixedRowOperators {
                column: "claude+codex/term".to_owned(),
            }),
        ),
    ];

    for (raw, expected) in cases {
        let actual = parse_layout_structure(raw).map(|layout| {
            layout
                .columns
                .iter()
                .map(|column| (column.stacked, column.cells.clone()))
                .collect::<Vec<_>>()
        });
        assert_eq!(actual, expected, "{raw:?}");
    }
}

#[test]
fn inline_roles_compose_and_validate() {
    let profiles = profiles([("planner", profile("claude"))]);
    let commands = commands([("logs", "tail -f rimz.log")]);
    let spec = parse_layout_spec(
        "claude:planner,planner:lead,claude-auto:coder",
        &profiles,
        &commands,
    )
    .expect("inline roles");

    assert!(matches!(
        agent_at(&spec, 0, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { role, profile, .. }, .. }
            if kind.as_str() == "claude"
                && role.as_deref() == Some("planner")
                && profile.is_none()
    ));
    assert!(matches!(
        agent_at(&spec, 1, 0),
        AgentCell { launch: crate::agents::LaunchParams { role, profile, .. }, .. }
            if role.as_deref() == Some("lead") && profile.as_deref() == Some("planner")
    ));
    assert!(matches!(
        agent_at(&spec, 2, 0),
        AgentCell { launch: crate::agents::LaunchParams { mode, role, .. }, .. }
            if *mode == Some(PermissionMode::Auto) && role.as_deref() == Some("coder")
    ));

    for (raw, cell) in [("term:x", "term"), ("logs:x", "logs")] {
        assert_eq!(
            parse_layout_spec(raw, &profiles, &commands),
            Err(LayoutErr::RoleOnCommandCell {
                cell: cell.to_owned(),
                role: "x".to_owned(),
            })
        );
    }
    for role in ["codex", "all", "claude-2"] {
        assert!(matches!(
            parse_layout_spec(&format!("claude:{role}"), &profiles, &commands),
            Err(LayoutErr::InlineRoleShadowsAddress { name, .. }) if name == role
        ));
    }
    for role in ["", "bad role"] {
        assert_eq!(
            parse_layout_spec(&format!("claude:{role}"), &profiles, &commands),
            Err(LayoutErr::InvalidInlineRole {
                name: role.to_owned(),
            })
        );
    }
    assert_eq!(
        parse_layout_spec("claude:x,codex:x", &profiles, &commands),
        Err(LayoutErr::DuplicateInlineRole {
            role: "x".to_owned(),
        })
    );

    let exact_commands = CommandsConfig(BTreeMap::from([(
        "logs:lead".to_owned(),
        "tail -f exact.log".to_owned(),
    )]));
    let exact = parse_layout_spec("logs:lead", &profiles, &exact_commands)
        .expect("exact command name wins before inline role splitting");
    assert!(matches!(
        &exact.columns[0].rows[0],
        Cell::Command { argv } if argv == &["tail", "-f", "exact.log"]
    ));
}

#[test]
fn known_spec_tokens_require_parseable_layouts() {
    let profiles = no_profiles();
    let commands = no_commands();
    let teams = TeamsConfig::default();

    for raw in ["claude/codex", "claude:planner"] {
        assert!(is_known_spec_token(raw, &profiles, &commands, &teams));
    }
    for raw in ["sh", "https://example.invalid", "claude: fix the bug"] {
        assert!(!is_known_spec_token(raw, &profiles, &commands, &teams));
    }
}

#[test]
fn resolve_spec_dispatches_default_inline_peer_and_team() {
    let profiles = profiles([
        ("planner", profile("claude")),
        ("reviewer", profile("codex")),
    ]);
    let mut teams = TeamsConfig(BTreeMap::from([(
        "stacked".to_owned(),
        team(vec![
            role("planner", "planner"),
            role("reviewer", "reviewer"),
        ]),
    )]));

    for arg in [None, Some("  ")] {
        assert_eq!(
            resolve_spec(arg, &profiles, &no_commands(), &teams),
            Ok(LayoutSpec::single(Cell::shell()))
        );
    }
    assert_eq!(
        resolve_spec(Some("claude"), &profiles, &no_commands(), &teams),
        Ok(LayoutSpec::single(Cell::agent(AgentKind::new_unchecked(
            "claude"
        ))))
    );
    assert_eq!(
        resolve_spec(Some("peer"), &profiles, &no_commands(), &teams)
            .expect("peer")
            .agent_kinds()
            .collect::<Vec<_>>(),
        vec!["claude", "codex"]
    );
    assert_eq!(
        resolve_spec(Some("sh"), &profiles, &no_commands(), &teams),
        Ok(LayoutSpec::single(Cell::Command {
            argv: vec!["sh".to_owned()]
        }))
    );
    assert_eq!(
        resolve_spec(Some("sh:role"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::RoleOnCommandCell {
            cell: "sh".to_owned(),
            role: "role".to_owned(),
        })
    );
    assert_eq!(
        resolve_spec(Some("stacked"), &profiles, &no_commands(), &teams)
            .expect("team")
            .columns
            .len(),
        2
    );

    teams
        .0
        .insert("claude".to_owned(), team(vec![role("lead", "planner")]));
    assert_eq!(
        resolve_spec(Some("claude"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::ReservedTeamName("claude".to_owned()))
    );
    assert!(matches!(
        resolve_spec(Some("missing"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::UnknownTeam { team, .. }) if team == "missing"
    ));
}

#[test]
fn profile_inheritance_and_builtin_overrides_resolve() {
    let profiles = profiles([
        (
            "base",
            Profile {
                agent: "codex".to_owned(),
                model: Some("base-model".to_owned()),
                effort: Some("medium".to_owned()),
                args: Some("--base".to_owned()),
                ..profile("codex")
            },
        ),
        (
            "child",
            Profile {
                agent: "base".to_owned(),
                effort: Some("high".to_owned()),
                args: Some("--child".to_owned()),
                ..profile("base")
            },
        ),
        (
            "inherits-args",
            Profile {
                agent: "base".to_owned(),
                model: Some("child-model".to_owned()),
                ..profile("base")
            },
        ),
        (
            "claude",
            Profile {
                agent: "claude".to_owned(),
                effort: Some("high".to_owned()),
                ..profile("claude")
            },
        ),
        ("claude-child", profile("claude")),
    ]);

    assert_eq!(
        resolve_profile("claude", &no_profiles()),
        Ok(ResolvedProfile::bare("claude"))
    );
    let child = resolve_profile("child", &profiles).expect("child");
    assert_eq!(child.kind.as_str(), "codex");
    assert_eq!(child.launch.model.as_deref(), Some("base-model"));
    assert_eq!(child.launch.effort.as_deref(), Some("high"));
    assert_eq!(child.args.as_deref(), Some("--child"));

    let inherited = resolve_profile("inherits-args", &profiles).expect("inherited");
    assert_eq!(inherited.launch.model.as_deref(), Some("child-model"));
    assert_eq!(inherited.launch.effort.as_deref(), Some("medium"));
    assert_eq!(inherited.args.as_deref(), Some("--base"));

    for name in ["claude", "claude-child"] {
        let resolved = resolve_profile(name, &profiles).expect("override");
        assert_eq!(resolved.kind.as_str(), "claude");
        assert_eq!(resolved.launch.effort.as_deref(), Some("high"));
    }
}

#[test]
fn profile_cells_render_fields_in_contract_order() {
    let profiles = profiles([
        (
            "codex-deep",
            Profile {
                agent: "codex".to_owned(),
                mode: Some(PermissionMode::Auto),
                model: Some("gpt-5-codex".to_owned()),
                effort: Some("high".to_owned()),
                args: Some("--profile reviewer".to_owned()),
                ..profile("codex")
            },
        ),
        (
            "prompt-base",
            Profile {
                agent: "claude".to_owned(),
                append_system_prompt_file: Some("/prompts/base-extra.md".into()),
                ..profile("claude")
            },
        ),
        (
            "prompt-child",
            Profile {
                agent: "prompt-base".to_owned(),
                system_prompt_file: Some("/prompts/planner.md".into()),
                ..profile("prompt-base")
            },
        ),
    ]);
    let spec =
        parse_layout_spec("codex-deep,prompt-child", &profiles, &no_commands()).expect("profiles");

    let codex = agent_at(&spec, 0, 0);
    let mut expected = vec![
        "--model".to_owned(),
        "gpt-5-codex".to_owned(),
        "-c".to_owned(),
        "model_reasoning_effort=high".to_owned(),
    ];
    expected.extend(
        crate::agents::find_definition("codex")
            .expect("codex")
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
    );
    expected.extend(["--profile".to_owned(), "reviewer".to_owned()]);
    assert_eq!(codex.args, expected);
    assert_eq!(codex.launch.mode, Some(PermissionMode::Auto));
    assert_eq!(codex.launch.profile.as_deref(), Some("codex-deep"));
    assert_eq!(codex.launch.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(codex.launch.effort.as_deref(), Some("high"));

    let prompt = agent_at(&spec, 1, 0);
    assert_eq!(prompt.launch.profile.as_deref(), Some("prompt-child"));
    assert_eq!(
        prompt.system_prompt_file.as_deref(),
        Some(Path::new("/prompts/planner.md"))
    );
    assert_eq!(
        prompt.append_system_prompt_file.as_deref(),
        Some(Path::new("/prompts/base-extra.md"))
    );
    assert_eq!(
        prompt.args,
        vec![
            "--system-prompt-file".to_owned(),
            "/prompts/planner.md".to_owned(),
            "--append-system-prompt-file".to_owned(),
            "/prompts/base-extra.md".to_owned(),
        ]
    );
}

#[test]
fn profile_resolution_rejects_invalid_chains_and_fields() {
    assert_eq!(
        resolve_profile("planner", &profiles([("planner", profile("ghost"))])),
        Err(LayoutErr::UnknownProfileBase {
            profile: "planner".to_owned(),
            base: "ghost".to_owned(),
        })
    );
    let cycle = profiles([
        ("a", profile("b")),
        ("b", profile("c")),
        ("c", profile("a")),
    ]);
    assert!(matches!(
        resolve_profile("a", &cycle),
        Err(LayoutErr::ProfileCycle { chain }) if chain == "a -> b -> c -> a"
    ));

    let mut chain = BTreeMap::new();
    for i in 0..=MAX_PROFILE_DEPTH {
        chain.insert(format!("p{i}"), profile(&format!("p{}", i + 1)));
    }
    assert_eq!(
        resolve_profile("p0", &ProfilesConfig(chain)),
        Err(LayoutErr::ProfileChainTooDeep {
            profile: "p0".to_owned(),
        })
    );

    let bad_args = profiles([(
        "bad-args",
        Profile {
            agent: "claude".to_owned(),
            args: Some("'unterminated".to_owned()),
            ..profile("claude")
        },
    )]);
    assert!(matches!(
        parse_layout_spec("bad-args", &bad_args, &no_commands()),
        Err(LayoutErr::InvalidProfile { profile, reason })
            if profile == "bad-args" && reason.contains("shell quoting")
    ));

    let unsupported = profiles([(
        "pi-deep",
        Profile {
            agent: "pi".to_owned(),
            system_prompt_file: Some(PathBuf::from("/abs/prompt.md")),
            ..profile("pi")
        },
    )]);
    assert!(matches!(
        parse_layout_spec("pi-deep", &unsupported, &no_commands()),
        Err(LayoutErr::InvalidProfile { profile, reason })
            if profile == "pi-deep" && reason.contains("system-prompt-file")
    ));
}

#[test]
fn virtual_cells_obey_adapter_capabilities_and_profile_overrides() {
    for (raw, mode) in [
        ("pi-ask", PermissionMode::Ask),
        ("pi-plan", PermissionMode::Plan),
    ] {
        let cell = agent_cell(raw, &no_profiles(), &no_commands());
        assert_eq!(cell.kind.as_str(), "pi");
        assert_eq!(cell.launch.mode, Some(mode));
        assert!(cell.args.is_empty());
    }

    let profiles = profiles([
        (
            "claude",
            Profile {
                agent: "claude".to_owned(),
                mode: Some(PermissionMode::Plan),
                args: Some("--append".to_owned()),
                ..profile("claude")
            },
        ),
        (
            "pi",
            Profile {
                agent: "pi".to_owned(),
                mode: Some(PermissionMode::Auto),
                args: Some("--profile-arg".to_owned()),
                ..profile("pi")
            },
        ),
    ]);

    let auto = agent_cell("claude-auto", &profiles, &no_commands());
    let mut expected_auto = vec!["--append".to_owned()];
    expected_auto.extend(
        crate::agents::find_definition("claude")
            .expect("claude")
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
    );
    assert_eq!(auto.kind.as_str(), "claude");
    assert_eq!(auto.launch.profile.as_deref(), Some("claude"));
    assert_eq!(auto.launch.mode, Some(PermissionMode::Auto));
    assert_eq!(auto.args, expected_auto);

    let ask = agent_cell("pi-ask", &profiles, &no_commands());
    assert_eq!(ask.launch.profile.as_deref(), Some("pi"));
    assert_eq!(ask.launch.mode, Some(PermissionMode::Ask));
    assert_eq!(ask.args, vec!["--profile-arg".to_owned()]);

    assert!(matches!(
        parse_layout_spec("pi-yolo", &profiles, &no_commands()),
        Err(LayoutErr::UnknownCell { cell, valid })
            if cell == "pi-yolo" && !valid.contains("pi-yolo")
    ));
}

#[test]
fn config_names_reject_grammar_clashes() {
    assert!(
        parse_layout_spec(
            "claude",
            &profiles([("claude", profile("claude"))]),
            &no_commands()
        )
        .is_ok()
    );

    assert_eq!(
        parse_layout_spec(
            "term",
            &profiles([("bad/name", profile("claude"))]),
            &no_commands()
        ),
        Err(LayoutErr::InvalidProfileName {
            name: "bad/name".to_owned(),
        })
    );
    assert_eq!(
        parse_layout_spec("term", &no_profiles(), &commands([("bad name", "nvim")])),
        Err(LayoutErr::InvalidCommandName {
            name: "bad name".to_owned(),
        })
    );
    assert_eq!(
        parse_layout_spec(
            "claude",
            &profiles([("term", profile("claude"))]),
            &no_commands()
        ),
        Err(LayoutErr::ReservedProfileName {
            name: "term".to_owned(),
        })
    );
    assert_eq!(
        parse_layout_spec("claude", &no_profiles(), &commands([("term", "nvim")])),
        Err(LayoutErr::ReservedCommandName {
            name: "term".to_owned(),
        })
    );

    for name in ["all", "claude-2", "a:b", "a#b"] {
        assert!(matches!(
            parse_layout_spec(
                "term",
                &profiles([(name, profile("claude"))]),
                &no_commands()
            ),
            Err(LayoutErr::ProfileShadowsAddress { name: actual, .. }) if actual == name
        ));
    }
    assert!(matches!(
        parse_layout_spec(
            "bad-command",
            &no_profiles(),
            &commands([("bad-command", "nvim 'unterminated")])
        ),
        Err(LayoutErr::InvalidCommand { command, reason })
            if command == "bad-command" && reason.contains("shell quoting")
    ));
}

#[test]
fn named_teams_compile_roles_and_apply_overrides() {
    let profiles = profiles([
        (
            "coder-base",
            Profile {
                agent: "codex".to_owned(),
                mode: Some(PermissionMode::Auto),
                model: Some("base-model".to_owned()),
                effort: Some("medium".to_owned()),
                args: Some("--base".to_owned()),
                ..profile("codex")
            },
        ),
        (
            "planner-base",
            Profile {
                agent: "claude".to_owned(),
                append_system_prompt_file: Some("/prompts/base-extra.md".into()),
                ..profile("claude")
            },
        ),
    ]);
    let teams = TeamsConfig(BTreeMap::from([(
        "review".to_owned(),
        team(vec![
            RoleBinding {
                role: "coder".to_owned(),
                profile: "coder-base".to_owned(),
                mode: Some(PermissionMode::Ask),
                model: Some("role-model".to_owned()),
                effort: Some("high".to_owned()),
                budget: None,
                system_prompt_file: Some("/prompts/coder.md".into()),
                append_system_prompt_file: None,
                args: Some("--role".to_owned()),
            },
            RoleBinding {
                role: "planner".to_owned(),
                profile: "planner-base".to_owned(),
                mode: None,
                model: None,
                effort: None,
                budget: None,
                system_prompt_file: None,
                append_system_prompt_file: Some("/prompts/role-extra.md".into()),
                args: None,
            },
        ]),
    )]));

    let spec = resolve_spec(Some("review"), &profiles, &no_commands(), &teams).expect("team");
    assert_eq!(spec.columns.len(), 2);
    assert!(spec.columns.iter().all(|column| !column.stacked));

    let coder = agent_at(&spec, 0, 0);
    assert_eq!(coder.kind.as_str(), "codex");
    assert_eq!(coder.launch.profile.as_deref(), Some("coder-base"));
    assert_eq!(coder.launch.role.as_deref(), Some("coder"));
    assert_eq!(coder.launch.mode, Some(PermissionMode::Ask));
    assert_eq!(coder.launch.model.as_deref(), Some("role-model"));
    assert_eq!(coder.launch.effort.as_deref(), Some("high"));
    assert_eq!(
        coder.system_prompt_file.as_deref(),
        Some(Path::new("/prompts/coder.md"))
    );
    assert!(coder.args.contains(&"--role".to_owned()));
    assert!(!coder.args.contains(&"--base".to_owned()));

    let planner = agent_at(&spec, 1, 0);
    assert_eq!(planner.kind.as_str(), "claude");
    assert_eq!(planner.launch.profile.as_deref(), Some("planner-base"));
    assert_eq!(planner.launch.role.as_deref(), Some("planner"));
    assert_eq!(
        planner.append_system_prompt_file.as_deref(),
        Some(Path::new("/prompts/role-extra.md"))
    );
    assert_eq!(
        planner.args,
        vec![
            "--append-system-prompt-file".to_owned(),
            "/prompts/role-extra.md".to_owned(),
        ]
    );
}

#[test]
fn team_role_specs_preserve_identity_and_disambiguate_dots() {
    let profiles = profiles([
        ("planner-profile", profile("codex")),
        ("notateam.planner", profile("claude")),
    ]);
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![
            role("planner", "planner-profile"),
            role("sub.planner", "planner-profile"),
        ]),
    )]));

    let planner =
        resolve_spec(Some("forge.planner"), &profiles, &no_commands(), &teams).expect("role");
    assert_eq!(planner.columns.len(), 1);
    assert!(matches!(
        agent_at(&planner, 0, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { profile, role, .. }, .. }
            if kind.as_str() == "codex"
                && profile.as_deref() == Some("planner-profile")
                && role.as_deref() == Some("planner")
    ));
    let dotted = resolve_spec(Some("forge.sub.planner"), &profiles, &no_commands(), &teams)
        .expect("dotted role");
    assert_eq!(
        agent_at(&dotted, 0, 0).launch.role.as_deref(),
        Some("sub.planner")
    );
    assert!(matches!(
        resolve_spec(Some("forge.bogus"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::UnknownRoleInTeam { team, role, valid_roles })
            if team == "forge"
                && role == "bogus"
                && valid_roles == "planner, sub.planner"
    ));

    let profile_spec = resolve_spec(Some("notateam.planner"), &profiles, &no_commands(), &teams)
        .expect("dotted profile");
    assert_eq!(
        agent_at(&profile_spec, 0, 0).launch.profile.as_deref(),
        Some("notateam.planner")
    );
    assert_eq!(spec_team("forge", &teams), Some("forge"));
    assert_eq!(spec_team("forge.planner", &teams), Some("forge"));
    assert_eq!(spec_team("notateam.planner", &teams), None);

    let bad_teams = TeamsConfig(BTreeMap::from([(
        "pc.r".to_owned(),
        team(vec![role("planner", "planner-profile")]),
    )]));
    assert_eq!(
        validate_config(&profiles, &no_commands(), &bad_teams),
        Err(LayoutErr::InvalidTeamName {
            name: "pc.r".to_owned(),
        })
    );
}

#[test]
fn explicit_team_layouts_place_roles_and_roleless_cells() {
    let profiles = profiles([
        ("planner-profile", profile("claude")),
        ("coder-profile", profile("codex")),
        ("reviewer-profile", profile("claude")),
    ]);
    let commands = commands([("logs", "tail -f rimz.log")]);
    let teams = TeamsConfig(BTreeMap::from([
        (
            "review".to_owned(),
            team_with_layout(
                vec![
                    role("planner", "planner-profile"),
                    role("coder", "coder-profile"),
                    role("reviewer", "reviewer-profile"),
                ],
                "reviewer/planner,coder+term+logs",
            ),
        ),
        (
            "pair".to_owned(),
            team_with_layout(Vec::new(), "claude,codex"),
        ),
    ]));

    let spec = resolve_spec(Some("review"), &profiles, &commands, &teams).expect("team");
    assert_eq!(spec.columns.len(), 2);
    assert!(spec.columns[0].stacked);
    assert_eq!(
        agent_at(&spec, 0, 0).launch.role.as_deref(),
        Some("reviewer")
    );
    assert_eq!(
        agent_at(&spec, 0, 1).launch.role.as_deref(),
        Some("planner")
    );
    assert_eq!(agent_at(&spec, 1, 0).launch.role.as_deref(), Some("coder"));
    assert_eq!(spec.columns[1].rows[1], Cell::shell());
    assert_eq!(
        spec.columns[1].rows[2],
        Cell::Command {
            argv: vec!["tail".to_owned(), "-f".to_owned(), "rimz.log".to_owned()],
        }
    );

    let pair = resolve_spec(Some("pair"), &profiles, &commands, &teams).expect("roleless team");
    assert_eq!(
        pair.agent_kinds().collect::<Vec<_>>(),
        vec!["claude", "codex"]
    );
    assert!(pair.columns.iter().all(|column| !column.stacked));
}

#[test]
fn team_validation_rejects_invalid_roles_and_layouts() {
    let profiles = profiles([("planner", profile("claude")), ("coder", profile("codex"))]);
    let config = |team| TeamsConfig(BTreeMap::from([("review".to_owned(), team)]));
    let error = |candidate| {
        validate_config(&profiles, &no_commands(), &config(candidate)).expect_err("invalid team")
    };

    assert!(matches!(
        error(team(Vec::new())),
        LayoutErr::EmptyTeam { .. }
    ));
    assert!(matches!(
        error(team(vec![role("bad role", "planner")])),
        LayoutErr::InvalidRoleName { name, .. } if name == "bad role"
    ));
    assert!(matches!(
        error(team(vec![role("planner", "planner"), role("planner", "planner")])),
        LayoutErr::DuplicateRole { role, .. } if role == "planner"
    ));
    assert!(matches!(
        error(team(vec![role("reviewer", "missing")])),
        LayoutErr::UnknownRoleProfile { profile, .. } if profile == "missing"
    ));
    assert!(matches!(
        error(team_with_layout(
            vec![role("planner", "planner")],
            "planner,claude:helper"
        )),
        LayoutErr::UnknownRoleInLayout { role, .. } if role == "claude:helper"
    ));
    let roles = || vec![role("planner", "planner"), role("coder", "coder")];
    assert!(matches!(
        error(team_with_layout(roles(), "planner+term")),
        LayoutErr::RoleNotPlaced { role, .. } if role == "coder"
    ));
    assert!(matches!(
        error(team_with_layout(roles(), "planner+planner,coder")),
        LayoutErr::DuplicateRoleInLayout { role, .. } if role == "planner"
    ));
    assert!(matches!(
        error(team_with_layout(roles(), "planner,coder+ghost")),
        LayoutErr::UnknownRoleInLayout { role, .. } if role == "ghost"
    ));
    for name in ["all", "claude"] {
        assert!(matches!(
            validate_config(
                &profiles,
                &no_commands(),
                &config(team(vec![role(name, "planner")]))
            ),
            Err(LayoutErr::RoleShadowsAddress { name: actual, .. }) if actual == name
        ));
    }
}

#[test]
fn default_team_defers_role_budget_validation() {
    let profiles = profiles([("planner", profile("claude"))]);
    let mut budget_role = role("planner", "planner");
    budget_role.budget = Some("not-a-budget".to_owned());
    let mut teams = TeamsConfig(BTreeMap::from([(
        "review".to_owned(),
        team(vec![budget_role, role("bad role", "planner")]),
    )]));

    assert!(matches!(
        validate_config(&profiles, &no_commands(), &teams),
        Err(LayoutErr::InvalidRoleName { name, .. }) if name == "bad role"
    ));

    teams.0.get_mut("review").expect("team").roles[1].role = "coder".to_owned();
    validate_config(&profiles, &no_commands(), &teams)
        .expect("default team defers budget normalization");
    assert!(matches!(
        resolve_spec(Some("review"), &profiles, &no_commands(), &teams),
        Err(LayoutErr::InvalidProfile { profile, .. }) if profile == "planner"
    ));
}

#[test]
fn team_roles_accept_implicit_builtin_profiles() {
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![role("planner", "claude"), role("coder", "codex")]),
    )]));

    validate_config(&no_profiles(), &no_commands(), &teams).expect("built-in profiles validate");
    let spec = resolve_spec(Some("forge"), &no_profiles(), &no_commands(), &teams)
        .expect("built-in profiles resolve");
    assert!(matches!(
        agent_at(&spec, 0, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { profile, role, .. }, .. }
            if kind.as_str() == "claude"
                && profile.as_deref() == Some("claude")
                && role.as_deref() == Some("planner")
    ));
    assert!(matches!(
        agent_at(&spec, 1, 0),
        AgentCell { kind, launch: crate::agents::LaunchParams { profile, role, .. }, .. }
            if kind.as_str() == "codex"
                && profile.as_deref() == Some("codex")
                && role.as_deref() == Some("coder")
    ));
}

#[test]
fn team_leader_validation_accepts_one_target() {
    let profiles = profiles([("planner", profile("claude")), ("coder", profile("codex"))]);
    let validate = |team| {
        validate_config(
            &profiles,
            &no_commands(),
            &TeamsConfig(BTreeMap::from([("forge".to_owned(), team)])),
        )
    };
    let mut declared = team(vec![role("planner", "planner"), role("coder", "coder")]);
    declared.leader = Some("coder".to_owned());
    validate(declared.clone()).expect("declared leader");

    declared.leader = Some("reviewer".to_owned());
    assert!(matches!(
        validate(declared),
        Err(LayoutErr::UnknownLeaderRole { leader, valid_roles, .. })
            if leader == "reviewer" && valid_roles == "planner, coder"
    ));

    let layout_only = |leader: &str, layout: &str| Team {
        roles: Vec::new(),
        leader: Some(leader.to_owned()),
        layout: Some(layout.to_owned()),
    };
    validate(layout_only("claude", "claude,codex")).expect("unique layout leader");
    assert!(matches!(
        validate(layout_only("pi", "claude,codex")),
        Err(LayoutErr::UnknownLeaderRole { leader, .. }) if leader == "pi"
    ));
    assert_eq!(
        validate(layout_only("claude", "claude,claude")),
        Err(LayoutErr::AmbiguousPromptLeader {
            token: "claude".to_owned(),
        })
    );
}

#[test]
fn prompt_leader_selects_or_rejects_targets() {
    let profiles = profiles([("planner", profile("claude")), ("coder", profile("codex"))]);
    let commands = commands([("vim", "vim")]);
    let mut configured = team_with_layout(
        vec![role("planner", "planner"), role("coder", "coder")],
        "coder,planner",
    );
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), configured.clone())]));
    let layout = resolve_spec(Some("forge"), &profiles, &commands, &teams).expect("reordered team");
    assert_eq!(prompt_leader(&layout, Some(&configured)), Ok(1));

    configured.leader = Some("coder".to_owned());
    assert_eq!(prompt_leader(&layout, Some(&configured)), Ok(0));

    let command_first =
        parse_layout_spec("vim,codex+term", &profiles, &commands).expect("one agent");
    assert_eq!(prompt_leader(&command_first, None), Ok(0));
    let unique =
        parse_layout_spec("claude,codex", &no_profiles(), &no_commands()).expect("unique first");
    assert_eq!(prompt_leader(&unique, None), Ok(0));
    let inline = parse_layout_spec("claude:lead,claude", &no_profiles(), &no_commands())
        .expect("inline leader");
    assert_eq!(prompt_leader(&inline, None), Ok(0));

    let ambiguous =
        parse_layout_spec("claude,claude", &no_profiles(), &no_commands()).expect("duplicate");
    assert_eq!(
        prompt_leader(&ambiguous, None),
        Err(LayoutErr::AmbiguousPromptLeader {
            token: "claude".to_owned(),
        })
    );
    assert_eq!(
        prompt_leader(
            &parse_layout_spec("term", &no_profiles(), &no_commands()).expect("terminal"),
            None
        ),
        Err(LayoutErr::NoPromptTarget)
    );
}

#[test]
fn default_tab_title_uses_launch_identity_and_layout_order() {
    let layout = parse_layout_spec("term,codex", &no_profiles(), &no_commands()).expect("layout");
    let single = parse_layout_spec("claude", &no_profiles(), &no_commands()).expect("agent");
    let terminal = LayoutSpec::single(Cell::shell());
    let command = parse_layout_spec(
        "vim,claude",
        &no_profiles(),
        &commands([("vim", "/usr/bin/nvim -p")]),
    )
    .expect("command layout");
    let profile = parse_layout_spec(
        "planner",
        &profiles([("planner", profile("claude"))]),
        &no_commands(),
    )
    .expect("profile");
    let capped = parse_layout_spec("claude,codex,pi,claude", &no_profiles(), &no_commands())
        .expect("four-cell layout");

    for (spec, cwd, worktree, team, expected) in [
        (
            &layout,
            Path::new("/code/wt/tab-name"),
            Some("tab-name"),
            Some("forge"),
            "#tab-name",
        ),
        (
            &layout,
            Path::new("/code/query-engine"),
            None,
            Some("forge"),
            "team:forge",
        ),
        (
            &layout,
            Path::new("/code/query-engine"),
            None,
            None,
            "term+codex:query-engine",
        ),
        (&single, Path::new("/code/main"), None, None, "claude:main"),
        (&terminal, Path::new("/code/main"), None, None, "term:main"),
        (
            &command,
            Path::new("/code/main"),
            None,
            None,
            "nvim+claude:main",
        ),
        (
            &profile,
            Path::new("/code/main"),
            None,
            None,
            "planner:main",
        ),
        (
            &capped,
            Path::new("/code/main"),
            None,
            None,
            "claude+codex+pi+…:main",
        ),
    ] {
        assert_eq!(default_tab_title(spec, cwd, worktree, team), expected);
    }
}

#[test]
fn bare_role_qualifies_against_the_lane_team() {
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![
            role("planner", "claude"),
            role("reviewer", "reviewer"),
        ]),
    )]));
    let profiles = profiles([("reviewer", profile("claude"))]);
    let commands = commands([]);

    // A role bound to an unrelated profile qualifies.
    assert_eq!(
        qualify_spec_in_channel("planner", "forge", "forge", &teams, &profiles, &commands)
            .expect("qualify"),
        Cow::Owned::<str>("forge.planner".to_owned())
    );
    // A role bound to its same-named profile refines it, so it qualifies too.
    assert_eq!(
        qualify_spec_in_channel("reviewer", "forge", "forge", &teams, &profiles, &commands)
            .expect("qualify"),
        Cow::Owned::<str>("forge.reviewer".to_owned())
    );
}

#[test]
fn role_colliding_with_an_unrelated_cell_word_refuses() {
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![role("planner", "claude"), role("codex", "claude")]),
    )]));
    let profiles = profiles([]);
    let commands = commands([]);

    // `codex` is a registered agent kind, and the role binds a different agent.
    let err = qualify_spec_in_channel("codex", "forge", "forge", &teams, &profiles, &commands)
        .expect_err("ambiguous");
    assert!(matches!(err, LayoutErr::AmbiguousInChannel { .. }));
    let message = err.to_string();
    assert!(message.contains("forge.codex"), "{message}");
}

#[test]
fn specs_that_need_no_help_pass_through_unchanged() {
    let teams = TeamsConfig(BTreeMap::from([(
        "forge".to_owned(),
        team(vec![role("planner", "claude")]),
    )]));
    let profiles = profiles([]);
    let commands = commands([]);

    for raw in [
        "forge.planner", // already qualified
        "forge",         // the whole team
        "claude+codex",  // an inline layout
        "claude:lead",   // an inline role
        "claude",        // a kind the team declares no role for
        "",              // no spec at all
    ] {
        assert_eq!(
            qualify_spec_in_channel(raw, "forge", "forge", &teams, &profiles, &commands)
                .expect("qualify"),
            Cow::Borrowed(raw),
            "{raw}"
        );
    }

    // A stale stamp naming a team the config no longer declares.
    assert_eq!(
        qualify_spec_in_channel("planner", "gone", "gone", &teams, &profiles, &commands)
            .expect("qualify"),
        Cow::Borrowed("planner")
    );
}
