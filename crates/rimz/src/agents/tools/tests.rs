use super::*;

fn tools(entries: &[&str]) -> ToolSet {
    ToolSet::parse(&entries.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap()
}

#[test]
fn skills_require_a_supplied_applied_tools_list_without_skill() {
    let without_skill = tools(&["Read", "Bash"]);
    let with_skill = tools(&["Skill", "Read"]);
    let preset = tools(&["Read", "Default"]);
    let actual: Vec<_> = ["claude", "codex", "pi", "opencode", "omp"]
        .into_iter()
        .map(|kind| {
            (
                kind,
                [None, Some(&without_skill), Some(&with_skill), Some(&preset)]
                    .map(|tools| skills_lack_skill_tool(kind, tools)),
            )
        })
        .collect();
    assert_eq!(
        actual,
        [
            ("claude", [false, true, false, false]),
            ("codex", [false, true, false, true]),
            ("pi", [false, true, false, true]),
            ("opencode", [false, false, false, false]),
            ("omp", [false, false, false, false]),
        ]
    );
}

#[test]
fn pi_tool_aliases_have_one_portable_spelling() {
    let parsed = tools(&[
        "read",
        "Read(path)",
        "bash",
        "Bash",
        "edit",
        "write",
        "grep",
        "find",
        "ls",
        "ask_user_question",
        "AskUserQuestion",
        "powershell",
    ]);
    assert_eq!(
        parsed.bases(),
        [
            "Read",
            "Bash",
            "Edit",
            "Write",
            "Grep",
            "Glob",
            "LS",
            "AskUserQuestion",
            "powershell"
        ]
    );
    assert_eq!(parsed.restrictions(), ["Read"]);
}

#[test]
fn pi_reports_restrictions_only_for_mapped_tools() {
    let parsed = tools(&[
        "Read(path)",
        "Agent(Explore)",
        "WebFetch(url)",
        "Bash(git *)",
    ]);
    let (args, notices) = render_launch_tool_args("pi", Some(&parsed), None).unwrap();
    assert_eq!(args, ["--tools", "read,bash", "--no-skills"]);
    assert_eq!(
        notices,
        [
            ToolNotice::Dropped(vec!["Agent".into(), "WebFetch".into()]),
            ToolNotice::Restrictions(vec!["Read".into(), "Bash".into()]),
        ]
    );
}

#[test]
fn pi_keeps_skill_only_beside_a_reader() {
    for (entries, expected_args, expected_notices) in [
        (vec!["Skill", "Read"], vec!["--tools", "read"], vec![]),
        (vec!["Bash", "Skill"], vec!["--tools", "bash"], vec![]),
        (
            vec!["Skill(foo)", "Read"],
            vec!["--tools", "read"],
            vec![ToolNotice::Restrictions(vec!["Skill".into()])],
        ),
        (
            vec!["Skill(foo)"],
            vec!["--no-tools"],
            vec![ToolNotice::Dropped(vec!["Skill".into()])],
        ),
    ] {
        let (args, notices) = render_launch_tool_args("pi", Some(&tools(&entries)), None).unwrap();
        assert_eq!(args, expected_args, "{entries:?}");
        assert_eq!(notices, expected_notices, "{entries:?}");
    }
}

#[test]
fn code_mode_appends_each_declared_tool_once_and_off_renders_as_today() {
    let launch_in = |kind: &str, entries: Option<&[&str]>, code_mode: Option<bool>| {
        let parsed = entries.map(tools);
        render_launch_tool_args(kind, parsed.as_ref(), code_mode)
            .unwrap()
            .0
    };
    let launch = |kind: &str, entries: Option<&[&str]>, on: bool| {
        launch_in(kind, entries, on.then_some(true))
    };
    let read_bash: &[&str] = &["Read", "Bash"];
    // Kind, profile tools, argv with code mode on, argv with it off.
    type Case = (
        &'static str,
        Option<&'static [&'static str]>,
        &'static [&'static str],
        &'static [&'static str],
    );
    let cases: [Case; 14] = [
        (
            "claude",
            Some(read_bash),
            &["--tools", "Read,Bash,REPL,Poll"],
            &["--tools", "Read,Bash"],
        ),
        (
            "claude",
            Some(&["REPL", "Read"]),
            &["--tools", "REPL,Read,Poll"],
            &["--tools", "REPL,Read"],
        ),
        (
            "claude",
            Some(&[]),
            &["--tools", "REPL,Poll"],
            &["--tools", ""],
        ),
        // Claude's preset already holds what code mode adds: alone it renders as written,
        // and beside other names as the one spelling Claude reads inside a list.
        (
            "claude",
            Some(&["Default"]),
            &["--tools", "Default"],
            &["--tools", "Default"],
        ),
        (
            "claude",
            Some(&["default", "Agent"]),
            &["--tools", "preset:default,Agent"],
            &["--tools", "preset:default,Agent"],
        ),
        (
            "claude",
            Some(&["Agent(Explore)", "Default", "Read"]),
            &[
                "--disallowedTools",
                "Agent(Plan)",
                "Agent(general-purpose)",
                "Agent(statusline-setup)",
                "Agent(fork)",
                "--tools",
                "preset:default,Agent,Read",
            ],
            &[
                "--disallowedTools",
                "Agent(Plan)",
                "Agent(general-purpose)",
                "Agent(statusline-setup)",
                "Agent(fork)",
                "--tools",
                "preset:default,Agent,Read",
            ],
        ),
        // The preset written twice, in two cases, is still the one preset.
        (
            "claude",
            Some(&["default", "Default"]),
            &["--tools", "default"],
            &["--tools", "default"],
        ),
        (
            "claude",
            Some(&["default", "Default", "Agent"]),
            &["--tools", "preset:default,Agent"],
            &["--tools", "preset:default,Agent"],
        ),
        ("claude", None, &[], &[]),
        ("pi", None, &["--tools", "+codemode"], &[]),
        (
            "pi",
            Some(&["default"]),
            &["--tools", "codemode", "--no-skills"],
            &["--no-tools", "--no-skills"],
        ),
        (
            "pi",
            Some(read_bash),
            &["--tools", "read,bash,codemode", "--no-skills"],
            &["--tools", "read,bash", "--no-skills"],
        ),
        (
            "pi",
            Some(&["WebSearch"]),
            &["--tools", "codemode", "--no-skills"],
            &["--no-tools", "--no-skills"],
        ),
        (
            "pi",
            Some(&["codemode", "Read", "Skill"]),
            &["--tools", "read,codemode"],
            &["--tools", "read"],
        ),
    ];
    for (kind, entries, on, off) in cases {
        assert_eq!(launch(kind, entries, true), on, "{kind} {entries:?} on");
        assert_eq!(launch(kind, entries, false), off, "{kind} {entries:?} off");
        // Claude's off rides the env and Pi's the final argv, so the tools render as auto.
        assert_eq!(
            launch_in(kind, entries, Some(false)),
            off,
            "{kind} {entries:?}"
        );
    }
    // Codex takes the switch as config argv after any tools list.
    for entries in [Some(read_bash), None] {
        let mut on = launch("codex", entries, false);
        on.extend(["-c".to_owned(), "features.code_mode_only=true".to_owned()]);
        assert_eq!(launch("codex", entries, true), on, "codex {entries:?}");
        // Off renders both settings at the same position, over saved config values.
        let mut off = launch("codex", entries, false);
        off.extend(
            [
                "-c",
                "features.code_mode=false",
                "-c",
                "features.code_mode_only=false",
            ]
            .map(str::to_owned),
        );
        assert_eq!(
            launch_in("codex", entries, Some(false)),
            off,
            "codex {entries:?}"
        );
    }
    for kind in ["opencode", "omp", "unregistered"] {
        for entries in [Some(read_bash), None] {
            assert_eq!(
                launch(kind, entries, true),
                launch(kind, entries, false),
                "{kind} {entries:?}"
            );
        }
    }
}

#[test]
fn claude_tools_and_generated_planner_parity() {
    let plain = tools(&[
        "Bash",
        "Read",
        "Grep",
        "Glob",
        "Edit",
        "Write",
        "AskUserQuestion",
        "LSP",
        "Skill",
    ]);
    // Literal args the planner agent launches with.
    let expected =
        shlex::split("--tools 'Bash,Read,Grep,Glob,Edit,Write,AskUserQuestion,LSP,Skill'").unwrap();
    assert_eq!(render_tool_args("claude", Some(&plain)).unwrap(), expected);
    let named = tools(&["Agent(Explore, Plan)", "Bash", "Agent(Explore)"]);
    assert_eq!(
        render_tool_args("claude", Some(&named)).unwrap(),
        [
            "--disallowedTools",
            "Agent(general-purpose)",
            "Agent(statusline-setup)",
            "Agent(fork)",
            "--tools",
            "Agent,Bash"
        ]
    );
    let error = render_tool_args("claude", Some(&tools(&["Agent(unknown)"]))).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("known types: Explore, Plan, general-purpose, statusline-setup, fork")
    );
}

#[test]
fn codex_tool_switches() {
    let ask = tools(&["AskUserQuestion", "Bash", "Skill"]);
    let expected = shlex::split(r#"--strict-config -c 'web_search="disabled"' -c agents.enabled=false -c features.goals=false -c features.multi_agent_v2=false -c features.shell_snapshot=true -c features.shell_tool=true -c features.skill_mcp_dependency_install=false -c features.tool_call_mcp_elicitation=false -c features.browser_use=false -c features.browser_use_external=false -c features.computer_use=false -c features.in_app_browser=false -c features.image_generation=false -c features.tool_suggest=false -c features.memories=false -c features.default_mode_request_user_input=true -c tools.experimental_request_user_input.enabled=true -c skills.include_instructions=true"#).unwrap();
    assert_eq!(render_tool_args("codex", Some(&ask)).unwrap(), expected);
    let web = tools(&["Bash", "Read", "WebFetch", "Agent"]);
    let expected = shlex::split(r#"--strict-config -c 'web_search="cached"' -c agents.enabled=true -c features.goals=false -c features.multi_agent_v2=true -c features.shell_snapshot=true -c features.shell_tool=true -c features.skill_mcp_dependency_install=false -c features.tool_call_mcp_elicitation=false -c features.browser_use=false -c features.browser_use_external=false -c features.computer_use=false -c features.in_app_browser=false -c features.image_generation=false -c features.tool_suggest=false -c features.memories=false -c features.default_mode_request_user_input=false -c tools.experimental_request_user_input.enabled=false -c skills.include_instructions=false"#).unwrap();
    assert_eq!(render_tool_args("codex", Some(&web)).unwrap(), expected);
}

#[test]
fn generated_implementer_parity() {
    // Literal args the implementer agent launches with.
    let expected = shlex::split(r#"--strict-config -c 'web_search="disabled"' -c 'agents.enabled=false' -c 'features.goals=false' -c 'features.multi_agent_v2=false' -c 'features.shell_snapshot=true' -c 'features.shell_tool=true' -c 'features.skill_mcp_dependency_install=false' -c 'features.tool_call_mcp_elicitation=false' -c 'features.browser_use=false' -c 'features.browser_use_external=false' -c 'features.computer_use=false' -c 'features.in_app_browser=false' -c 'features.image_generation=false' -c 'features.tool_suggest=false' -c 'features.memories=false' -c 'features.default_mode_request_user_input=false' -c 'tools.experimental_request_user_input.enabled=false' -c 'skills.include_instructions=false'"#).unwrap();
    assert_eq!(
        render_tool_args(
            "codex",
            Some(&tools(&[
                "Bash", "Read", "Grep", "Glob", "Edit", "Write", "LSP"
            ]))
        )
        .unwrap(),
        expected
    );
}

#[test]
fn tool_vocabulary_and_support() {
    let parsed = tools(&[" Bash ", "Agent(Explore, Plan)", "Bash", "Agent(Plan)"]);
    assert_eq!(parsed.bases(), ["Bash", "Agent"]);
    assert_eq!(parsed.agent_types(), ["Explore", "Plan"]);
    assert!(parsed.has("Agent"));
    for entry in [
        "",
        " ",
        "--Bash",
        "Bash extra",
        "123",
        "Agent(Explore",
        "Bash)",
    ] {
        assert!(ToolSet::parse(&[entry.to_owned()]).is_err(), "{entry}");
    }
    for kind in ["claude", "codex"] {
        assert!(tools_required(kind));
        assert!(matches!(
            render_tool_args(kind, None),
            Err(ToolErr::Missing)
        ));
    }
    assert!(!tools_required("pi"));
    assert!(render_tool_args("pi", None).unwrap().is_empty());
    assert_eq!(
        render_tool_args("pi", Some(&parsed)).unwrap(),
        ["--tools", "bash", "--no-skills"]
    );
    for (entries, expected) in [
        (vec![], vec!["--no-tools", "--no-skills"]),
        (vec!["WebSearch"], vec!["--no-tools", "--no-skills"]),
        (vec!["Bash(git *)"], vec!["--tools", "bash", "--no-skills"]),
        (
            vec![
                "Bash",
                "Read",
                "Edit",
                "MultiEdit",
                "Write",
                "Grep",
                "Glob",
                "LS",
                "AskUserQuestion",
            ],
            vec![
                "--tools",
                "bash,read,edit,write,grep,find,ls,ask_user_question",
                "--no-skills",
            ],
        ),
        (
            vec!["Edit", "MultiEdit", "Read", "Bash", "read"],
            vec!["--tools", "edit,read,bash", "--no-skills"],
        ),
        (
            vec![
                "read",
                "bash",
                "powershell",
                "edit",
                "write",
                "grep",
                "find",
                "ls",
                "ask_user_question",
            ],
            vec![
                "--tools",
                "read,bash,powershell,edit,write,grep,find,ls,ask_user_question",
                "--no-skills",
            ],
        ),
        (
            vec![
                "AskUserQuestion",
                "Bash",
                "Edit",
                "Glob",
                "Grep",
                "Read",
                "Skill",
                "WebFetch",
                "WebSearch",
                "Write",
            ],
            vec![
                "--tools",
                "ask_user_question,bash,edit,find,grep,read,write",
            ],
        ),
    ] {
        assert_eq!(
            render_tool_args("pi", Some(&tools(&entries))).unwrap(),
            expected,
            "{entries:?}"
        );
    }
    assert!(render_tool_args("unregistered", Some(&parsed)).is_ok_and(|args| args.is_empty()));
    assert!(render_tool_args("unregistered", None).unwrap().is_empty());
}

#[test]
fn model_catalog_and_defaults() {
    for (model, kind) in [
        ("fable", Some("claude")),
        ("gpt-6-astra", Some("codex")),
        ("astra", Some("codex")),
        ("claude-opus-4-6", Some("claude")),
        ("gpt-x", Some("codex")),
        ("custom", None),
        ("hydrogen", Some("antigravity")),
        ("flash", Some("antigravity")),
        ("gemini-4-hydrogen", Some("antigravity")),
        ("gemini-3.8-flash", Some("antigravity")),
        ("gemini-4-hydrogen-high", Some("antigravity")),
        ("gemini-x", Some("antigravity")),
        ("gemini-pro-orchestrator-neon", Some("antigravity")),
    ] {
        assert_eq!(definition_model_kind(model), kind, "{model}");
    }
    for id in [
        "gemini-pro-orchestrator-neon",
        "gemini-pro-orchestrator",
        "gemini-pro-orchestrator-argon",
    ] {
        assert_eq!(definition_defaults("antigravity", Some(id)).effort, None);
        assert!(
            definitions("antigravity")
                .models
                .iter()
                .any(|entry| entry.name == id && entry.id == id && entry.max_effort.is_none()),
            "{id}"
        );
    }
    for (name, id) in [
        ("hydrogen", "gemini-4-hydrogen"),
        ("neon", "gemini-4-neon"),
        ("carbon", "gemini-4-carbon"),
        ("argon", "gemini-4-argon"),
        ("flash", "gemini-3.8-flash"),
    ] {
        for model in [name, id] {
            assert_eq!(
                definition_defaults("antigravity", Some(model)).effort,
                Some("high"),
                "{model}"
            );
        }
        assert!(
            definitions("antigravity")
                .models
                .iter()
                .any(|entry| entry.name == name && entry.id == id)
        );
    }
    for model in [Some("gemini-4-hydrogen-high"), Some("gemini-x"), None] {
        assert_eq!(definition_defaults("antigravity", model).effort, None);
    }
    for (alias, id) in [
        ("astra", "gpt-6-astra"),
        ("sol", "gpt-6-sol"),
        ("luna", "gpt-6-luna"),
        ("terra", "gpt-5.6-terra"),
    ] {
        assert!(
            definitions("codex")
                .models
                .iter()
                .any(|entry| entry.name == alias && entry.id == id)
        );
        assert_eq!(definition_model_kind(id), Some("codex"));
    }
    assert_eq!(
        definition_defaults("claude", Some("fable")),
        DefinitionDefaults {
            effort: Some("high")
        }
    );
    assert_eq!(
        definition_defaults("codex", Some("astra")),
        DefinitionDefaults {
            effort: Some("xhigh")
        }
    );
    assert_eq!(definition_defaults("pi", None).effort, Some("xhigh"));
    assert_eq!(
        definition_defaults("unregistered", None),
        DefinitionDefaults { effort: None }
    );
}

#[test]
fn definition_effort_defaults_require_a_matcher() {
    let mut launch = super::super::LaunchSpec {
        definitions: DefinitionSpec {
            effort: Some("xhigh"),
            ..DefinitionSpec::EMPTY
        },
        ..super::super::LaunchSpec::EMPTY
    };
    let unsupported = defaults_for_launch(&launch, None, None);
    launch.presets.effort = Some(super::super::StaticPresetMatcher::Flag(&["--effort"]));
    let supported = defaults_for_launch(&launch, None, None);
    assert_eq!(
        [unsupported, supported],
        [
            DefinitionDefaults { effort: None },
            DefinitionDefaults {
                effort: Some("xhigh"),
            },
        ]
    );
}

/// A native question claim reaches only kinds whose questions RimZ sees, which is why the
/// native-wins warning carries no clause about an unmarked card.
#[test]
fn every_kind_that_renders_the_native_question_tool_has_question_coverage() {
    use crate::mcp::catalog::Capability;
    let named = vec!["AskUserQuestion".to_owned()];
    for definition in crate::agents::all_definitions() {
        let kind = definition.spec().kind;
        if native_claim(kind, &named, Capability::Ask).is_none() {
            continue;
        }
        assert!(
            !matches!(
                definition.concern_coverage(crate::agents::IntegrationConcern::UserQuestion),
                crate::agents::ConcernCoverage::Unsupported { .. }
            ),
            "{kind} renders AskUserQuestion but RimZ cannot see its questions: the native-wins \
             question warning must again say this agent's questions will not mark its card"
        );
    }
}

/// A tools list names a capability's native tool where the kind renders it: Pi has a question
/// tool but no `Agent`, and a kind that applies no tools list claims neither. Claude's preset
/// claims both and ranks with neither side; a named tool outranks a blanket grant, and two named
/// entries refuse.
#[test]
fn a_named_native_tool_outranks_a_blanket_grant_and_the_preset_ranks_with_neither() {
    use crate::mcp::catalog::Capability;
    let bases =
        |names: &[&str]| -> Vec<String> { names.iter().map(|name| (*name).to_owned()).collect() };
    let named = bases(&["Bash", "AskUserQuestion", "Agent"]);
    let preset = bases(&["default"]);
    let claims = |kind: &str| {
        [Capability::Ask, Capability::Subagents, Capability::Lsp]
            .map(|capability| native_claim(kind, &named, capability))
    };
    let (ask, agent) = (
        Some(NativeClaim::Named("AskUserQuestion")),
        Some(NativeClaim::Named("Agent")),
    );
    assert_eq!(
        ["claude", "codex", "antigravity", "pi", "amp", "omp"].map(claims),
        [
            [ask, agent, None],
            [ask, agent, None],
            [ask, agent, None],
            [ask, None, None],
            [None, None, None],
            [None, None, None],
        ]
    );
    assert_eq!(
        [
            native_claim("claude", &bases(&["Bash"]), Capability::Ask),
            native_claim("claude", &preset, Capability::Ask),
            native_claim("codex", &preset, Capability::Ask),
        ],
        [None, Some(NativeClaim::Preset("default")), None]
    );
    let named = Some(NativeClaim::Named("AskUserQuestion"));
    let preset = Some(NativeClaim::Preset("default"));
    assert_eq!(
        [
            contest(named, Some(RimzClaim::Blanket)),
            contest(named, Some(RimzClaim::Named)),
            contest(named, None),
            contest(preset, Some(RimzClaim::Blanket)),
            contest(preset, Some(RimzClaim::Named)),
            contest(None, Some(RimzClaim::Blanket)),
        ],
        [
            Some(Contest::NativeWins),
            Some(Contest::Refused),
            None,
            None,
            None,
            None,
        ]
    );
}
