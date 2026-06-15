use super::*;
use crate::config::Alias;
use std::collections::BTreeMap;

fn aliases(entries: impl IntoIterator<Item = (&'static str, Alias)>) -> AliasesConfig {
    AliasesConfig(
        entries
            .into_iter()
            .map(|(name, alias)| (name.to_owned(), alias))
            .collect(),
    )
}

#[test]
fn parses_columns_and_stacked_rows() {
    let spec = parse_layout_spec("claude,codex+term", &AliasesConfig::default()).expect("parse");
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
fn rejects_empty_and_unknown_cells() {
    let aliases = AliasesConfig::default();
    assert_eq!(parse_layout_spec("", &aliases), Err(LayoutErr::Empty));
    assert_eq!(
        parse_layout_spec("claude,,term", &aliases),
        Err(LayoutErr::EmptyCell("claude,,term".to_owned()))
    );
    assert!(matches!(
        parse_layout_spec("claude,bogus", &aliases),
        Err(LayoutErr::UnknownCell { cell, .. }) if cell == "bogus"
    ));
}

#[test]
fn resolves_default_inline_builtin_and_named_layouts() {
    let aliases = AliasesConfig::default();
    let mut layouts = LayoutsConfig::default();
    layouts
        .0
        .insert("stacked".to_owned(), "claude,codex+term".to_owned());

    assert_eq!(
        resolve_layout(None, &aliases, &layouts).expect("default"),
        LayoutSpec::single(Cell::shell())
    );
    assert_eq!(
        resolve_layout(Some("claude"), &aliases, &layouts).expect("inline"),
        LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")))
    );
    layouts
        .0
        .insert("claude".to_owned(), "claude,codex".to_owned());
    assert_eq!(
        resolve_layout(Some("claude"), &aliases, &layouts),
        Err(LayoutErr::ReservedLayoutName("claude".to_owned()))
    );
    assert_eq!(
        resolve_layout(Some("peer"), &aliases, &layouts)
            .expect("builtin")
            .columns
            .len(),
        2
    );
    assert!(matches!(
        resolve_layout(Some("dual"), &aliases, &layouts),
        Err(LayoutErr::UnknownLayout { layout, .. }) if layout == "dual"
    ));
    assert_eq!(
        resolve_layout(Some("stacked"), &aliases, &layouts)
            .expect("named")
            .columns
            .len(),
        2
    );
    assert!(matches!(
        resolve_layout(Some("missing"), &aliases, &layouts),
        Err(LayoutErr::UnknownLayout { layout, .. }) if layout == "missing"
    ));
}

#[test]
fn command_keywords_parse_to_raw_argv_cells() {
    let aliases = aliases([
        ("vim", Alias::Command("nvim -p".to_owned())),
        ("htop", Alias::Command("htop".to_owned())),
        (
            "zsh",
            Alias::CommandTable {
                command: "zsh".to_owned(),
            },
        ),
    ]);

    let spec = parse_layout_spec("vim,htop+zsh", &aliases).expect("parse commands");

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
                argv: vec!["zsh".to_owned()]
            }
        ]
    );
}

#[test]
fn agent_keyword_mode_precedes_extra_args() {
    let aliases = aliases([(
        "codex-deep",
        Alias::Agent {
            agent: "codex".to_owned(),
            mode: Some(PermissionMode::Auto),
            model: None,
            effort: None,
            system_prompt_file: None,
            args: Some("--model gpt-5-codex -c model_reasoning_effort=high".to_owned()),
        },
    )]);
    let Cell::Agent { args, .. } = parse_layout_spec("codex-deep", &aliases)
        .expect("parse agent keyword")
        .columns[0]
        .rows[0]
        .clone()
    else {
        panic!("agent cell");
    };
    let mut expected = crate::agents::find_adapter("codex")
        .expect("codex")
        .permission_args(PermissionMode::Auto);
    expected.extend([
        "--model".to_owned(),
        "gpt-5-codex".to_owned(),
        "-c".to_owned(),
        "model_reasoning_effort=high".to_owned(),
    ]);

    assert_eq!(args, expected);
}

#[test]
fn agent_alias_model_and_effort_render_before_extra_args() {
    let aliases = aliases([(
        "codex-deep",
        Alias::Agent {
            agent: "codex".to_owned(),
            mode: None,
            model: Some("gpt-5-codex".to_owned()),
            effort: Some("high".to_owned()),
            system_prompt_file: None,
            args: Some("--profile reviewer".to_owned()),
        },
    )]);
    let Cell::Agent { args, .. } = parse_layout_spec("codex-deep", &aliases)
        .expect("parse agent alias")
        .columns[0]
        .rows[0]
        .clone()
    else {
        panic!("agent cell");
    };

    assert_eq!(
        args,
        vec![
            "--model".to_owned(),
            "gpt-5-codex".to_owned(),
            "-c".to_owned(),
            "model_reasoning_effort=high".to_owned(),
            "--profile".to_owned(),
            "reviewer".to_owned(),
        ]
    );
}

#[test]
fn agent_alias_system_prompt_file_renders_and_stamps_role() {
    let aliases = aliases([(
        "planner",
        Alias::Agent {
            agent: "claude".to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: Some("/prompts/planner.md".into()),
            args: None,
        },
    )]);
    let Cell::Agent { args, alias, .. } = parse_layout_spec("planner", &aliases)
        .expect("parse planner role")
        .columns[0]
        .rows[0]
        .clone()
    else {
        panic!("agent cell");
    };
    // The role is stamped onto the cell, and the system prompt renders the
    // adapter's native flag.
    assert_eq!(alias.as_deref(), Some("planner"));
    assert_eq!(
        args,
        vec![
            "--system-prompt-file".to_owned(),
            "/prompts/planner.md".to_owned(),
        ]
    );
}

#[test]
fn agent_alias_name_must_not_shadow_the_address_grammar() {
    fn agent_alias(agent: &str) -> Alias {
        Alias::Agent {
            agent: agent.to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: None,
            args: None,
        }
    }
    // A kind-named agent alias is rejected so `@claude` stays the kind.
    let shadow = aliases([("claude", agent_alias("claude"))]);
    assert!(matches!(
        parse_layout_spec("term", &shadow),
        Err(LayoutErr::AliasShadowsAddress { name, .. }) if name == "claude"
    ));
    // An ordinal-shaped agent alias is rejected so `@codex-2` stays an ordinal.
    let ordinal = aliases([("codex-2", agent_alias("codex"))]);
    assert!(matches!(
        parse_layout_spec("term", &ordinal),
        Err(LayoutErr::AliasShadowsAddress { .. })
    ));
    // A command alias keeps its freedom to override a cell word — it never
    // launches an addressable agent.
    let command = aliases([("claude", Alias::Command("nvim".to_owned()))]);
    assert!(parse_layout_spec("term", &command).is_ok());
}

#[test]
fn unsupported_agent_alias_preset_field_errors() {
    let aliases = aliases([(
        "pi-deep",
        Alias::Agent {
            agent: "pi".to_owned(),
            mode: None,
            model: Some("large".to_owned()),
            effort: None,
            system_prompt_file: None,
            args: None,
        },
    )]);

    assert!(matches!(
        parse_layout_spec("pi-deep", &aliases),
        Err(LayoutErr::InvalidAlias { alias, reason })
            if alias == "pi-deep"
                && reason.contains("does not support alias field `model`")
    ));
}

#[test]
fn user_alias_overrides_agent_and_virtual_cell_words() {
    let aliases = aliases([
        ("claude", Alias::Command("nvim".to_owned())),
        (
            "codex-yolo",
            Alias::Command("codex --profile reviewer".to_owned()),
        ),
    ]);

    assert_eq!(
        parse_layout_spec("claude", &aliases)
            .expect("agent override")
            .columns[0]
            .rows[0],
        Cell::Command {
            argv: vec!["nvim".to_owned()]
        }
    );
    assert_eq!(
        parse_layout_spec("codex-yolo", &aliases)
            .expect("virtual override")
            .columns[0]
            .rows[0],
        Cell::Command {
            argv: vec![
                "codex".to_owned(),
                "--profile".to_owned(),
                "reviewer".to_owned()
            ]
        }
    );
}

#[test]
fn virtual_agent_modes_work_without_config() {
    let spec = parse_layout_spec("claude-auto,codex-yolo+pi-ask", &AliasesConfig::default())
        .expect("virtual modes");

    assert_eq!(
        spec.columns[0].rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: crate::agents::find_adapter("claude")
                .expect("claude")
                .permission_args(PermissionMode::Auto),
            mode: Some(PermissionMode::Auto),
            alias: None,
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
            alias: None,
        }
    );
    assert_eq!(
        spec.columns[1].rows[1],
        Cell::Agent {
            kind: AgentKind::new_unchecked("pi"),
            args: Vec::new(),
            mode: Some(PermissionMode::Ask),
            alias: None,
        }
    );
}

#[test]
fn virtual_non_ask_modes_without_adapter_flags_are_unknown() {
    assert!(matches!(
        parse_layout_spec("pi-yolo", &AliasesConfig::default()),
        Err(LayoutErr::UnknownCell { cell, valid })
            if cell == "pi-yolo"
                && !valid.split(", ").any(|candidate| candidate == "pi-yolo")
                && valid.split(", ").any(|candidate| candidate == "pi-ask")
    ));
}

#[test]
fn plan_mode_works_without_config() {
    let aliases = AliasesConfig::default();

    // claude-plan: adapter returns non-empty args
    assert_eq!(
        parse_layout_spec("claude-plan", &aliases)
            .expect("claude-plan")
            .columns[0]
            .rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: vec!["--permission-mode".to_owned(), "plan".to_owned()],
            mode: Some(PermissionMode::Plan),
            alias: None,
        }
    );

    // codex-plan: adapter returns empty args (fallback to default, same as plain codex)
    assert_eq!(
        parse_layout_spec("codex-plan", &aliases)
            .expect("codex-plan")
            .columns[0]
            .rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("codex"),
            args: Vec::new(),
            mode: Some(PermissionMode::Plan),
            alias: None,
        }
    );

    // pi-plan: also valid with empty args (Plan is always accepted, like Ask)
    assert_eq!(
        parse_layout_spec("pi-plan", &aliases)
            .expect("pi-plan")
            .columns[0]
            .rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("pi"),
            args: Vec::new(),
            mode: Some(PermissionMode::Plan),
            alias: None,
        }
    );
}

#[test]
fn ping_aliases_work_without_config() {
    let aliases = AliasesConfig::default();

    assert_eq!(
        parse_layout_spec("claude-ping", &aliases)
            .expect("claude-ping")
            .columns[0]
            .rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: vec!["--effort".to_owned(), "low".to_owned(), "ping".to_owned()],
            mode: None,
            alias: None,
        }
    );

    assert_eq!(
        parse_layout_spec("codex-ping", &aliases)
            .expect("codex-ping")
            .columns[0]
            .rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("codex"),
            args: vec![
                "-c".to_owned(),
                "model_reasoning_effort=low".to_owned(),
                "ping".to_owned(),
            ],
            mode: None,
            alias: None,
        }
    );

    // pi has no ping_args implementation → unknown cell
    assert!(matches!(
        parse_layout_spec("pi-ping", &aliases),
        Err(LayoutErr::UnknownCell { cell, .. }) if cell == "pi-ping"
    ));
}

#[test]
fn keyword_errors_are_specific() {
    let mut map = BTreeMap::new();
    map.insert(
        "bad-agent".to_owned(),
        Alias::Agent {
            agent: "ghost".to_owned(),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: None,
            args: None,
        },
    );
    map.insert(
        "bad-command".to_owned(),
        Alias::Command("nvim 'unterminated".to_owned()),
    );
    let config = AliasesConfig(map);

    assert_eq!(
        parse_layout_spec("bad-agent", &config),
        Err(LayoutErr::UnknownAliasAgent {
            alias: "bad-agent".to_owned(),
            agent: "ghost".to_owned()
        })
    );
    assert_eq!(
        parse_layout_spec("bad-command", &config),
        Err(LayoutErr::InvalidAlias {
            alias: "bad-command".to_owned(),
            reason: "check shell quoting in command".to_owned()
        })
    );

    let invalid = aliases([("bad,name", Alias::Command("nvim".to_owned()))]);
    assert_eq!(
        parse_layout_spec("term", &invalid),
        Err(LayoutErr::InvalidAliasName {
            name: "bad,name".to_owned()
        })
    );
}

#[test]
fn reserved_agent_verbs_reject_alias_and_layout_names() {
    let invalid_alias = aliases([("term", Alias::Command("zsh".to_owned()))]);
    assert_eq!(
        parse_layout_spec("claude", &invalid_alias),
        Err(LayoutErr::ReservedAliasName {
            name: "term".to_owned()
        })
    );

    let layouts = LayoutsConfig(BTreeMap::from([("wait".to_owned(), "claude".to_owned())]));
    assert_eq!(
        resolve_layout(Some("wait"), &AliasesConfig::default(), &layouts),
        Err(LayoutErr::ReservedLayoutName("wait".to_owned()))
    );
}

#[test]
fn layout_name_collision_with_keyword_is_reserved() {
    let aliases = aliases([("review", Alias::Command("nvim".to_owned()))]);
    let layouts = LayoutsConfig(BTreeMap::from([(
        "review".to_owned(),
        "claude,codex".to_owned(),
    )]));

    assert_eq!(
        resolve_layout(Some("review"), &aliases, &layouts),
        Err(LayoutErr::ReservedLayoutName("review".to_owned()))
    );
}

#[test]
fn named_layouts_compose_keywords() {
    let aliases = aliases([
        (
            "claude-plan",
            Alias::Agent {
                agent: "claude".to_owned(),
                mode: None,
                model: None,
                effort: None,
                system_prompt_file: None,
                args: Some("--permission-mode plan".to_owned()),
            },
        ),
        ("vim", Alias::Command("nvim".to_owned())),
    ]);
    let layouts = LayoutsConfig(BTreeMap::from([(
        "review".to_owned(),
        "claude-plan,codex+vim".to_owned(),
    )]));

    let spec = resolve_layout(Some("review"), &aliases, &layouts).expect("layout");

    assert_eq!(
        spec.columns[0].rows[0],
        Cell::Agent {
            kind: AgentKind::new_unchecked("claude"),
            args: vec!["--permission-mode".to_owned(), "plan".to_owned()],
            mode: None,
            alias: Some("claude-plan".to_owned()),
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
fn headline_examples_resolve_end_to_end() {
    let aliases = aliases([
        ("vim", Alias::Command("nvim".to_owned())),
        ("htop", Alias::Command("htop".to_owned())),
        ("zsh", Alias::Command("zsh".to_owned())),
    ]);

    assert_eq!(
        parse_layout_spec("vim,htop+zsh", &aliases)
            .expect("command layout")
            .columns
            .len(),
        2
    );
    let spec = parse_layout_spec("pi,claude-auto+codex-yolo", &AliasesConfig::default())
        .expect("agent mode layout");
    assert_eq!(spec.columns.len(), 2);
    assert_eq!(
        spec.columns[0].rows[0],
        Cell::agent(AgentKind::new_unchecked("pi"))
    );
    assert!(matches!(spec.columns[1].rows[0], Cell::Agent { ref args, .. } if !args.is_empty()));
    assert!(matches!(spec.columns[1].rows[1], Cell::Agent { ref args, .. } if !args.is_empty()));
}

#[test]
fn title_uses_first_agent_or_terminal_and_worktree_name() {
    // No worktree → kind-prefixed, so multiple agent tabs in one room stay distinct.
    let agent = parse_layout_spec("term,codex", &AliasesConfig::default()).expect("parse");
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
    // Worktree launch → worktree name behind the branch glyph, no kind prefix.
    assert_eq!(
        default_tab_title(
            &agent,
            Path::new("/code/wt/tab-name"),
            Some("tab-name"),
            "⑂"
        ),
        "⑂ tab-name"
    );
    // The Nerd Font set carries through to the tab prefix.
    assert_eq!(
        default_tab_title(
            &agent,
            Path::new("/code/wt/tab-name"),
            Some("tab-name"),
            "\u{e725}"
        ),
        "\u{e725} tab-name"
    );
}
