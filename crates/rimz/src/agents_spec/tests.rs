use super::*;
use std::collections::BTreeMap;

fn profile(agent: &str) -> Profile {
    Profile {
        agent: agent.to_owned(),
        mode: None,
        model: None,
        effort: None,
        system_prompt_file: None,
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

fn no_profiles() -> ProfilesConfig {
    ProfilesConfig::default()
}

fn no_commands() -> CommandsConfig {
    CommandsConfig::default()
}

#[test]
fn parses_columns_and_stacked_rows() {
    let spec =
        parse_layout_spec("claude,codex+term", &no_profiles(), &no_commands()).expect("parse");
    assert_eq!(
        spec,
        LayoutSpec {
            columns: vec![
                Column {
                    rows: vec![Cell::agent(AgentKind::new_unchecked("claude"))]
                },
                Column {
                    rows: vec![
                        Cell::agent(AgentKind::new_unchecked("codex")),
                        Cell::shell()
                    ]
                }
            ]
        }
    );
}

#[test]
fn resolves_default_inline_builtin_and_named_layouts() {
    let profiles = no_profiles();
    let commands = no_commands();
    let mut layouts = LayoutsConfig::default();
    layouts
        .0
        .insert("stacked".to_owned(), "claude,codex+term".to_owned());

    assert_eq!(
        resolve_layout(None, &profiles, &commands, &layouts).expect("default"),
        LayoutSpec::single(Cell::shell())
    );
    assert_eq!(
        resolve_layout(Some("claude"), &profiles, &commands, &layouts).expect("inline"),
        LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")))
    );
    layouts
        .0
        .insert("claude".to_owned(), "claude,codex".to_owned());
    assert_eq!(
        resolve_layout(Some("claude"), &profiles, &commands, &layouts),
        Err(LayoutErr::ReservedLayoutName("claude".to_owned()))
    );
    assert_eq!(
        resolve_layout(Some("peer"), &profiles, &commands, &layouts)
            .expect("builtin")
            .columns
            .len(),
        2
    );
    assert_eq!(
        resolve_layout(Some("stacked"), &profiles, &commands, &layouts)
            .expect("named")
            .columns
            .len(),
        2
    );
    assert!(matches!(
        resolve_layout(Some("missing"), &profiles, &commands, &layouts),
        Err(LayoutErr::UnknownLayout { layout, .. }) if layout == "missing"
    ));
}

#[test]
fn commands_parse_as_raw_argv_cells_and_shadow_agent_words() {
    let commands = commands([("vim", "nvim -p"), ("htop", "htop"), ("claude", "nvim")]);

    let spec =
        parse_layout_spec("vim,htop+claude", &no_profiles(), &commands).expect("parse commands");

    assert_eq!(
        spec.columns[0].rows[0],
        Cell::Command {
            argv: vec!["nvim".to_owned(), "-p".to_owned()]
        }
    );
    assert_eq!(
        spec.columns[1].rows,
        vec![
            Cell::Command {
                argv: vec!["htop".to_owned()]
            },
            Cell::Command {
                argv: vec!["nvim".to_owned()]
            }
        ]
    );
}

#[test]
fn profile_mode_preset_and_extra_args_render_in_order_and_stamp_profile() {
    let profiles = profiles([(
        "codex-deep",
        Profile {
            agent: "codex".to_owned(),
            mode: Some(PermissionMode::Auto),
            model: Some("gpt-5-codex".to_owned()),
            effort: Some("high".to_owned()),
            system_prompt_file: None,
            args: Some("--profile reviewer".to_owned()),
        },
    )]);

    let Cell::Agent {
        args,
        mode,
        profile,
        ..
    } = parse_layout_spec("codex-deep", &profiles, &no_commands())
        .expect("parse profile")
        .columns[0]
        .rows[0]
        .clone()
    else {
        panic!("agent cell");
    };

    let mut expected = vec![
        "--model".to_owned(),
        "gpt-5-codex".to_owned(),
        "-c".to_owned(),
        "model_reasoning_effort=high".to_owned(),
    ];
    expected.extend(
        crate::agents::find_adapter("codex")
            .expect("codex")
            .permission_args(PermissionMode::Auto),
    );
    expected.extend(["--profile".to_owned(), "reviewer".to_owned()]);
    assert_eq!(args, expected);
    assert_eq!(mode, Some(PermissionMode::Auto));
    assert_eq!(profile.as_deref(), Some("codex-deep"));
}

#[test]
fn profile_system_prompt_file_renders() {
    let profiles = profiles([(
        "planner",
        Profile {
            agent: "claude".to_owned(),
            system_prompt_file: Some("/prompts/planner.md".into()),
            ..profile("claude")
        },
    )]);
    let Cell::Agent { args, profile, .. } = parse_layout_spec("planner", &profiles, &no_commands())
        .expect("parse planner")
        .columns[0]
        .rows[0]
        .clone()
    else {
        panic!("agent cell");
    };

    assert_eq!(profile.as_deref(), Some("planner"));
    assert_eq!(
        args,
        vec![
            "--system-prompt-file".to_owned(),
            "/prompts/planner.md".to_owned(),
        ]
    );
}

#[test]
fn profile_inheritance_folds_child_wins_and_args_replace() {
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
    ]);

    let child = resolve_profile("child", &profiles).expect("child resolves");
    assert_eq!(child.kind.as_str(), "codex");
    assert_eq!(child.model.as_deref(), Some("base-model"));
    assert_eq!(child.effort.as_deref(), Some("high"));
    assert_eq!(child.args.as_deref(), Some("--child"));

    let inherits_args = resolve_profile("inherits-args", &profiles).expect("child resolves");
    assert_eq!(inherits_args.model.as_deref(), Some("child-model"));
    assert_eq!(inherits_args.effort.as_deref(), Some("medium"));
    assert_eq!(inherits_args.args.as_deref(), Some("--base"));
}

#[test]
fn profile_resolution_reports_unknown_base_cycles_depth_and_unsupported_fields() {
    let unknown = profiles([("planner", profile("ghost"))]);
    assert_eq!(
        resolve_profile("planner", &unknown),
        Err(LayoutErr::UnknownProfileBase {
            profile: "planner".to_owned(),
            base: "ghost".to_owned()
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
    let too_deep = ProfilesConfig(chain);
    assert_eq!(
        resolve_profile("p0", &too_deep),
        Err(LayoutErr::ProfileChainTooDeep {
            profile: "p0".to_owned()
        })
    );

    let unsupported = profiles([(
        "pi-deep",
        Profile {
            agent: "pi".to_owned(),
            model: Some("large".to_owned()),
            ..profile("pi")
        },
    )]);
    assert!(matches!(
        parse_layout_spec("pi-deep", &unsupported, &no_commands()),
        Err(LayoutErr::InvalidProfile { profile, reason })
            if profile == "pi-deep" && reason.contains("does not support profile field `model`")
    ));
}

#[test]
fn kind_override_flows_into_bare_children_and_virtual_cells() {
    let profiles = profiles([
        (
            "claude",
            Profile {
                agent: "claude".to_owned(),
                args: Some("--append".to_owned()),
                ..profile("claude")
            },
        ),
        ("planner", profile("claude")),
    ]);

    let Cell::Agent {
        args,
        profile: stamped,
        ..
    } = parse_layout_spec("claude", &profiles, &no_commands())
        .expect("kind override")
        .columns[0]
        .rows[0]
        .clone()
    else {
        panic!("agent cell");
    };
    assert_eq!(stamped.as_deref(), Some("claude"));
    assert_eq!(args, vec!["--append".to_owned()]);

    let Cell::Agent { args, profile, .. } = parse_layout_spec("planner", &profiles, &no_commands())
        .expect("child")
        .columns[0]
        .rows[0]
        .clone()
    else {
        panic!("agent cell");
    };
    assert_eq!(profile.as_deref(), Some("planner"));
    assert_eq!(args, vec!["--append".to_owned()]);

    let Cell::Agent { args, profile, .. } =
        parse_layout_spec("claude-auto", &profiles, &no_commands())
            .expect("virtual auto")
            .columns[0]
            .rows[0]
            .clone()
    else {
        panic!("agent cell");
    };
    let mut expected = vec!["--append".to_owned()];
    expected.extend(
        crate::agents::find_adapter("claude")
            .expect("claude")
            .permission_args(PermissionMode::Auto),
    );
    assert_eq!(profile.as_deref(), Some("claude"));
    assert_eq!(args, expected);

    let Cell::Agent { args, profile, .. } =
        parse_layout_spec("claude-ping", &profiles, &no_commands())
            .expect("virtual ping")
            .columns[0]
            .rows[0]
            .clone()
    else {
        panic!("agent cell");
    };
    assert_eq!(profile.as_deref(), Some("claude"));
    assert_eq!(
        args,
        vec![
            "--append".to_owned(),
            "--effort".to_owned(),
            "low".to_owned(),
            "ping".to_owned(),
        ]
    );
}

#[test]
fn virtual_cells_override_kind_profile_mode_but_keep_other_defaults() {
    let profiles = profiles([(
        "claude",
        Profile {
            agent: "claude".to_owned(),
            mode: Some(PermissionMode::Plan),
            args: Some("--append".to_owned()),
            ..profile("claude")
        },
    )]);

    let Cell::Agent { args, mode, .. } = parse_layout_spec("claude", &profiles, &no_commands())
        .expect("bare override")
        .columns[0]
        .rows[0]
        .clone()
    else {
        panic!("agent cell");
    };
    let mut expected_bare = crate::agents::find_adapter("claude")
        .expect("claude")
        .permission_args(PermissionMode::Plan);
    expected_bare.push("--append".to_owned());
    assert_eq!(mode, Some(PermissionMode::Plan));
    assert_eq!(args, expected_bare);

    let Cell::Agent { args, mode, .. } =
        parse_layout_spec("claude-auto", &profiles, &no_commands())
            .expect("virtual override")
            .columns[0]
            .rows[0]
            .clone()
    else {
        panic!("agent cell");
    };
    let mut expected_virtual = vec!["--append".to_owned()];
    expected_virtual.extend(
        crate::agents::find_adapter("claude")
            .expect("claude")
            .permission_args(PermissionMode::Auto),
    );
    assert_eq!(mode, Some(PermissionMode::Auto));
    assert_eq!(args, expected_virtual);

    let Cell::Agent { args, mode, .. } =
        parse_layout_spec("claude-ping", &profiles, &no_commands())
            .expect("ping override")
            .columns[0]
            .rows[0]
            .clone()
    else {
        panic!("agent cell");
    };
    assert_eq!(mode, None);
    assert_eq!(
        args,
        vec![
            "--append".to_owned(),
            "--effort".to_owned(),
            "low".to_owned(),
            "ping".to_owned(),
        ]
    );
}

#[test]
fn virtual_agent_modes_and_ping_work_without_config() {
    let spec = parse_layout_spec(
        "claude-auto,codex-yolo+pi-ask",
        &no_profiles(),
        &no_commands(),
    )
    .expect("virtual modes");

    assert_eq!(
        spec.columns[0].rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: crate::agents::find_adapter("claude")
                .expect("claude")
                .permission_args(PermissionMode::Auto),
            mode: Some(PermissionMode::Auto),
            profile: None,
        }
    );
    assert_eq!(
        spec.columns[1].rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("codex"),
            args: crate::agents::find_adapter("codex")
                .expect("codex")
                .permission_args(PermissionMode::Yolo),
            mode: Some(PermissionMode::Yolo),
            profile: None,
        }
    );
    assert_eq!(
        spec.columns[1].rows[1],
        Cell::Agent {
            kind: AgentKind::new_unchecked("pi"),
            args: Vec::new(),
            mode: Some(PermissionMode::Ask),
            profile: None,
        }
    );

    assert_eq!(
        parse_layout_spec("claude-ping", &no_profiles(), &no_commands())
            .expect("claude-ping")
            .columns[0]
            .rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: vec!["--effort".to_owned(), "low".to_owned(), "ping".to_owned()],
            mode: None,
            profile: None,
        }
    );
    assert!(matches!(
        parse_layout_spec("pi-yolo", &no_profiles(), &no_commands()),
        Err(LayoutErr::UnknownCell { cell, .. }) if cell == "pi-yolo"
    ));
}

#[test]
fn kind_override_does_not_make_unsupported_virtual_cells_valid() {
    let profiles = profiles([(
        "pi",
        Profile {
            agent: "pi".to_owned(),
            args: Some("--profile-arg".to_owned()),
            ..profile("pi")
        },
    )]);

    let Cell::Agent {
        args,
        mode,
        profile,
        ..
    } = parse_layout_spec("pi-ask", &profiles, &no_commands())
        .expect("ask remains valid")
        .columns[0]
        .rows[0]
        .clone()
    else {
        panic!("agent cell");
    };
    assert_eq!(args, vec!["--profile-arg".to_owned()]);
    assert_eq!(mode, Some(PermissionMode::Ask));
    assert_eq!(profile.as_deref(), Some("pi"));

    let err =
        parse_layout_spec("pi-yolo", &profiles, &no_commands()).expect_err("yolo unsupported");
    assert!(matches!(
        err,
        LayoutErr::UnknownCell { ref cell, ref valid }
            if cell == "pi-yolo" && !valid.contains("pi-yolo") && !valid.contains("pi-ping")
    ));

    let layouts = LayoutsConfig(BTreeMap::from([("bad".to_owned(), "pi-yolo".to_owned())]));
    assert!(matches!(
        validate_config(&profiles, &no_commands(), &layouts),
        Err(LayoutErr::UnknownCell { cell, .. }) if cell == "pi-yolo"
    ));
}

#[test]
fn profile_names_allow_kind_override_but_reject_address_grammar_clashes() {
    assert!(
        parse_layout_spec(
            "claude",
            &profiles([("claude", profile("claude"))]),
            &no_commands()
        )
        .is_ok()
    );

    for name in ["all", "claude-2", "a:b", "a#b"] {
        let profiles = profiles([(name, profile("claude"))]);
        assert!(matches!(
            parse_layout_spec("term", &profiles, &no_commands()),
            Err(LayoutErr::ProfileShadowsAddress { .. })
        ));
    }
}

#[test]
fn invalid_names_and_keyword_errors_are_specific() {
    let bad_profile = profiles([("bad,name", profile("claude"))]);
    assert_eq!(
        parse_layout_spec("term", &bad_profile, &no_commands()),
        Err(LayoutErr::InvalidProfileName {
            name: "bad,name".to_owned()
        })
    );
    let bad_command = commands([("bad name", "nvim")]);
    assert_eq!(
        parse_layout_spec("term", &no_profiles(), &bad_command),
        Err(LayoutErr::InvalidCommandName {
            name: "bad name".to_owned()
        })
    );
    let bad_quote = commands([("bad-command", "nvim 'unterminated")]);
    assert_eq!(
        parse_layout_spec("bad-command", &no_profiles(), &bad_quote),
        Err(LayoutErr::InvalidCommand {
            command: "bad-command".to_owned(),
            reason: "check shell quoting in command".to_owned()
        })
    );
    let reserved = profiles([("term", profile("claude"))]);
    assert_eq!(
        parse_layout_spec("claude", &reserved, &no_commands()),
        Err(LayoutErr::ReservedProfileName {
            name: "term".to_owned()
        })
    );
}

#[test]
fn named_layouts_compose_profiles_and_commands() {
    let profiles = profiles([(
        "claude-plan",
        Profile {
            agent: "claude".to_owned(),
            args: Some("--permission-mode plan".to_owned()),
            ..profile("claude")
        },
    )]);
    let commands = commands([("vim", "nvim")]);
    let layouts = LayoutsConfig(BTreeMap::from([(
        "review".to_owned(),
        "claude-plan,codex+vim".to_owned(),
    )]));

    let spec = resolve_layout(Some("review"), &profiles, &commands, &layouts).expect("layout");

    assert_eq!(
        spec.columns[0].rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: vec!["--permission-mode".to_owned(), "plan".to_owned()],
            mode: None,
            profile: Some("claude-plan".to_owned()),
        }
    );
    assert_eq!(
        spec.columns[1].rows[1],
        Cell::Command {
            argv: vec!["nvim".to_owned()]
        }
    );
}

#[test]
fn title_uses_first_agent_or_terminal_and_worktree_name() {
    let agent = parse_layout_spec("term,codex", &no_profiles(), &no_commands()).expect("parse");
    assert_eq!(
        default_tab_title(&agent, Path::new("/code/query-engine"), None, "⑂"),
        "codex:query-engine"
    );
    assert_eq!(
        default_tab_title(
            &LayoutSpec::single(Cell::shell()),
            Path::new("/code/main"),
            None,
            "⑂"
        ),
        "term:main"
    );
    assert_eq!(
        default_tab_title(
            &agent,
            Path::new("/code/wt/tab-name"),
            Some("tab-name"),
            "⑂"
        ),
        "⑂ tab-name"
    );
}
