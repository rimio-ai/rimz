use super::*;

#[test]
fn keep_warm_lifetime_warnings_name_sign_in_and_static_scope() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("codex", ""),
        ("warm", "agent: codex\nkeep-warm: 2h\ntools: []\n"),
        ("unknown", "agent: amp\nkeep-warm: 2h\n"),
    ] {
        let body = if name == "unknown" { "" } else { "Base." };
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\n{fields}---\n{body}"),
        )
        .unwrap();
    }
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let actual: Vec<_> = [
        ("", 0, "", false),
        (
            "[prompt_cache_ttl]\ncodex = { api-key = '5m' }",
            1,
            "codex api-key",
            true,
        ),
        (
            "[prompt_cache_ttl]\ncodex = 'off'",
            1,
            "prompt-cache lifetime is off",
            true,
        ),
        (
            "[prompt_cache_ttl]\nfallback = 'off'",
            1,
            "no prompt-cache lifetime is known for amp",
            true,
        ),
        ("cache_keepalive = false", 2, "cache_keepalive", false),
        ("cache_keepalive_max = '30m'", 2, "is capped", false),
    ]
    .map(|(config, count, detail, judges_lifetime)| {
        let warnings = keep_warm_warnings(&loaded, &toml::from_str(config).unwrap());
        (
            warnings.len() == count,
            warnings.iter().all(|warning| {
                warning.message.contains(detail)
                    && warning.message.contains("settings and defaults only") == judges_lifetime
                    && warning.message.contains("own cache writes") == judges_lifetime
            }),
        )
    })
    .into_iter()
    .collect();
    assert_eq!(actual, [(true, true); 6]);
}

#[test]
fn keep_warm_warnings_resolve_global_defaults_without_changing_declared_rows() {
    let root = tempfile::tempdir().unwrap();
    for directory in ["agents", "subagents", "teams"] {
        std::fs::create_dir(root.path().join(directory)).unwrap();
    }
    for (name, fields) in [
        ("claude", ""),
        ("warm", "agent: claude\ntools: []\n"),
        ("cold", "agent: warm\nkeep-warm: off\n"),
        ("heir", "agent: cold\n"),
        ("explicit", "agent: warm\nkeep-warm: 1h\n"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\n{fields}---\nBase."),
        )
        .unwrap();
    }
    std::fs::write(
        root.path().join("subagents/child.md"),
        "---\ndescription: Child\nagent: claude\ntools: []\n---\nChild.",
    )
    .unwrap();
    std::fs::write(root.path().join("teams/probe.md"), "---\nleader: lead\nstages: [Plan]\nroles:\n  - agent: cold\n    role: lead\n    owns: [Plan]\n  - agent: warm\n    role: cold\n    keep-warm: off\n  - agent: cold\n    role: short\n    keep-warm: 1h\n---\nPipeline.").unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let harness = toml::from_str("keep_warm = '2h'\ncache_keepalive_max = '30m'").unwrap();
    let warnings = keep_warm_warnings(&loaded, &harness);
    let mut paths: Vec<_> = warnings
        .iter()
        .map(|warning| {
            warning
                .path
                .strip_prefix(root.path())
                .unwrap()
                .to_str()
                .unwrap()
        })
        .collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "agents/claude.md",
            "agents/explicit.md",
            "agents/warm.md",
            "teams/probe.md",
            "teams/probe.md"
        ]
    );
    assert!(
        warnings
            .iter()
            .all(|warning| warning.message.contains("is capped"))
    );
    for name in ["warm", "probe.lead"] {
        let row = loaded.rows.iter().find(|row| row.name == name).unwrap();
        assert_eq!(
            row.keep_warm, None,
            "global defaults must not change declared rows"
        );
    }
}

#[test]
fn model_and_keep_warm_cells_stay_on_one_line() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("claude", ""),
        ("worker", "agent: claude\ntools: []\nkeep-warm: 2h\n"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\n{fields}---\nBase."),
        )
        .unwrap();
    }
    let mut loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let row = loaded
        .rows
        .iter_mut()
        .find(|row| row.name == "worker")
        .unwrap();
    row.model = Some("model\nforged".to_owned());
    row.keep_warm.as_mut().unwrap().while_clause = Some("stage\u{1b}[2K".to_owned());
    let row = &*row;

    let mut raw = Vec::new();
    rows(&mut raw, std::iter::once(row)).unwrap();
    let out = String::from_utf8(raw).unwrap();

    assert!(out.contains(r"model\nforged"), "{out:?}");
    assert!(out.contains(r"stage\u{1b}[2K"), "{out:?}");
    assert!(!out.contains("stage\u{1b}"), "{out:?}");
}

#[test]
fn keep_warm_rows_report_resolved_fields_and_warn_on_an_unbounded_while() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::create_dir(root.path().join("teams")).unwrap();
    for (name, fields) in [
        ("claude", ""),
        ("worker", "agent: claude\ntools: []\nkeep-warm: 2h\n"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\n{fields}---\nBase."),
        )
        .unwrap();
    }
    std::fs::write(root.path().join("teams/probe.md"), "---\nleader: lead\nstages: [Plan]\nkeep-warm: { while: 'team.stage!=Done' }\nroles:\n  - agent: worker\n    role: lead\n    owns: [Plan]\n  - agent: worker\n    role: helper\n    keep-warm: 1h\n---\nPipeline.").unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let rows_json = serde_json::to_value(&loaded.rows).unwrap();
    let row = |name| {
        rows_json
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap()
    };
    assert_eq!(
        row("worker")["keep_warm"],
        serde_json::json!({"max": "2h", "while": null})
    );
    assert_eq!(
        row("probe.lead")["keep_warm"],
        serde_json::json!({"max": null, "while": "team.stage!=Done"})
    );
    assert_eq!(
        row("probe.helper")["keep_warm"],
        serde_json::json!({"max": "1h", "while": "team.stage!=Done"})
    );
    let warnings = keep_warm_warnings(
        &loaded,
        &toml::from_str("cache_keepalive_max = 'off'").unwrap(),
    );
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains("holds nothing"));
    assert!(warnings[0].message.contains("set `max`"));
    assert!(!warnings[0].message.contains("settings and defaults only"));
    assert!(!warnings[0].message.contains("own cache writes"));
    let global = keep_warm_warnings(
        &loaded,
        &toml::from_str("keep_warm = '2h'\ncache_keepalive_max = 'off'").unwrap(),
    );
    assert!(global.is_empty(), "the global max bounds a while-only role");
    for cap in ["6h", "off"] {
        for (config, expected) in [
            (
                "cache_keepalive = false",
                "`harness.cache_keepalive` is off",
            ),
            (
                "[prompt_cache_ttl]\nclaude = 'off'",
                "prompt-cache lifetime is off",
            ),
            (
                "[prompt_cache_ttl]\nclaude = '5m'",
                "below `harness.keep_warm_min_ttl`",
            ),
        ] {
            let warnings = keep_warm_warnings(
                &loaded,
                &toml::from_str(&format!("cache_keepalive_max = '{cap}'\n{config}")).unwrap(),
            );
            let applicable: Vec<_> = warnings
                .iter()
                .filter(|warning| warning.message.contains(expected))
                .collect();
            assert_eq!(
                applicable.len(),
                3,
                "all max/while rows must warn: {config}"
            );
            assert_eq!(
                applicable
                    .iter()
                    .filter(|warning| warning.path.ends_with("probe.md"))
                    .count(),
                2,
                "both the max role and the while-only role must warn"
            );
        }
    }
    let capped = keep_warm_warnings(
        &loaded,
        &toml::from_str("cache_keepalive_max = '30m'").unwrap(),
    );
    assert_eq!(capped.len(), 2, "only explicit maxima can exceed the cap");
    assert!(
        capped
            .iter()
            .all(|warning| warning.message.contains("is capped"))
    );
    let mut human = Vec::new();
    rows(&mut human, loaded.rows.iter()).unwrap();
    let human = String::from_utf8(human).unwrap();
    assert!(human.contains("team.stage!=Done"));
    assert!(human.contains("2h"));
}

#[test]
fn keep_warm_warns_at_the_definition_for_an_unknown_or_short_cache_lifetime() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("claude", ""),
        ("warm", "agent: claude\nkeep-warm: 2h\ntools: []\n"),
        ("unknown", "agent: amp\nkeep-warm: 2h\n"),
        ("cold", "agent: claude\nkeep-warm: off\ntools: []\n"),
    ] {
        let body = if name == "unknown" { "" } else { "Base." };
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\n{fields}---\n{body}"),
        )
        .unwrap();
    }
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let harness = |toml: &str| toml::from_str::<rimz::config::HarnessConfig>(toml).unwrap();

    let defaults = keep_warm_warnings(&loaded, &harness("[prompt_cache_ttl]\nfallback = 'off'"));
    assert_eq!(defaults.len(), 1, "a 60m Claude cache clears the 30m floor");
    assert!(defaults[0].path.ends_with("unknown.md"));
    assert!(
        defaults[0]
            .message
            .contains("set `[harness.prompt_cache_ttl] amp`"),
        "{}",
        defaults[0].message
    );

    let capped = keep_warm_warnings(&loaded, &harness("cache_keepalive_max = \"1h\""));
    let warm = capped
        .iter()
        .find(|warning| warning.path.ends_with("warm.md"))
        .expect("a horizon past the keepalive maximum warns");
    assert!(
        warm.message
            .contains("keep-warm 2h is capped at `harness.cache_keepalive_max` (1h)"),
        "{}",
        warm.message
    );
    assert_eq!(
        keep_warm_warnings(
            &loaded,
            &harness("cache_keepalive_max = \"off\"\n[prompt_cache_ttl]\nfallback = 'off'")
        )
        .len(),
        1,
        "no maximum, no cap"
    );

    let short = "[prompt_cache_ttl]\nclaude = \"5m\"\namp = \"30m\"";
    let floored = keep_warm_warnings(&loaded, &harness(short));
    assert_eq!(floored.len(), 1, "an override makes amp known");
    assert!(floored[0].path.ends_with("warm.md"));
    assert!(
        floored[0]
            .message
            .contains("lifetime 5m is below `harness.keep_warm_min_ttl` (30m)"),
        "{}",
        floored[0].message
    );
    let open = format!("keep_warm_min_ttl = \"off\"\n{short}");
    assert!(keep_warm_warnings(&loaded, &harness(&open)).is_empty());

    let off = keep_warm_warnings(&loaded, &harness("cache_keepalive = false"));
    assert_eq!(off.len(), 2, "every set horizon is inert without pings");
    assert!(
        off.iter()
            .all(|warning| warning.message.contains("`harness.cache_keepalive` is off")),
        "{:?}",
        off.iter().map(|w| &w.message).collect::<Vec<_>>()
    );

    std::fs::create_dir(root.path().join("subagents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\nkeep-warm: 2h\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        root.path().join("subagents/child.md"),
        "---\ndescription: Test\nagent: claude\nkeep-warm: 2h\ntools: []\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        root.path().join("subagents/inherited_base.md"),
        "---\ndescription: Test\nagent: claude\ntools: []\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        root.path().join("subagents/inherited_parent.md"),
        "---\ndescription: Test\nagent: warm\n---\nBase.",
    )
    .unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let inert = keep_warm_warnings(&loaded, &harness(""));
    let child = inert
        .iter()
        .find(|warning| warning.path.ends_with("subagents/child.md"))
        .expect("a subagents definition that sets keep-warm warns");
    assert!(!child.message.contains("settings and defaults only"));
    assert!(!child.message.contains("own cache writes"));
    assert!(
        child
            .message
            .contains("keep-warm has no effect on a subagents definition"),
        "{}",
        child.message
    );
    assert!(
        !inert.iter().any(|warning| {
            warning.path.ends_with("subagents/inherited_base.md")
                || warning.path.ends_with("subagents/inherited_parent.md")
        }),
        "keep-warm from a kind base or direct parent must be dropped on a child without warning: {:?}",
        inert
            .iter()
            .map(|w| (&w.path, &w.message))
            .collect::<Vec<_>>()
    );
}

#[test]
fn tier_exclusions_warn_once_per_definition_and_family() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("claude", ""),
        ("codex", "tier: senior\n"),
        ("routed", "tier: junior\ntools: []\n"),
        ("exact", "agent: claude\nmodel: claude-custom\ntools: []\n"),
        ("blocked", "model: astra\ntools: []\n"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\n{fields}---\nBase."),
        )
        .unwrap();
    }
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    let [codex, blocked] = &loaded.errors[..] else {
        panic!("{:?}", loaded.errors);
    };
    assert!(blocked.path.ends_with("blocked.md"));
    assert!(blocked.message.contains("codex.md"));
    assert!(blocked.message.contains("tier: senior"));
    assert!(codex.path.ends_with("agents/codex.md"));
    let warnings = warnings(&loaded, Isolation::Host);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].path.ends_with("routed.md"));
    assert!(
        warnings[0]
            .message
            .contains("`junior` lists sol (codex), which cannot run this definition:")
    );
    assert!(warnings[0].message.contains("codex.md"));
}

#[test]
fn allowed_tools_warnings_follow_resolved_kind_and_nonempty_rules() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for kind in ["claude", "codex"] {
        std::fs::write(
            root.path().join(format!("agents/{kind}.md")),
            "---\ndescription: Base\n---\nBase.",
        )
        .unwrap();
        for (suffix, rules) in [("rules", "[Read]"), ("empty", "[]")] {
            std::fs::write(root.path().join(format!("agents/{kind}-{suffix}.md")), format!("---\ndescription: Worker\nagent: {kind}\ntools: [Read]\nallowed-tools: {rules}\n---\n")).unwrap();
        }
    }
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let warnings = warnings(&loaded, Isolation::Host);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].path.ends_with("codex-rules.md"));
    assert_eq!(
        warnings[0].message,
        "codex has no per-launch permission rules; allowed-tools is not applied and the agent prompts as usual: remove allowed-tools from the definition or run it on claude"
    );
}

#[test]
fn antigravity_warnings_name_untranslated_rules_and_the_sandbox_override() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/antigravity.md"),
        "---\ndescription: Base\n---\nBase.",
    )
    .unwrap();
    for (name, fields) in [
        (
            "agy-yolo",
            "isolation: sandbox\nmode: yolo\nallowed-tools: [\"Bash(git status:*)\", Glob, \"Read(~/notes)\"]\n",
        ),
        (
            "agy-auto",
            "isolation: sandbox\nmode: auto\nallowed-tools: [Read, \"mcp__github\"]\n",
        ),
        ("agy-host", "isolation: host\nmode: yolo\n"),
        ("agy-machine", "mode: yolo\n"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Worker\nagent: antigravity\ntools: [Read]\n{fields}---\n"),
        )
        .unwrap();
    }
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let warnings: Vec<_> = warnings(&loaded, Isolation::Sandbox)
        .into_iter()
        .map(|warning| {
            (
                warning
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                warning.message,
            )
        })
        .collect();
    let untranslated = |rule: &str| {
        format!("allowed-tools rule `{rule}` has no antigravity permission rule; it is left out")
    };
    let overridden = "the antigravity launch carries its skip-permissions flag, which overrides its sandbox; sandbox isolation is not in effect for this launch";
    let mut expected = vec![
        ("agy-yolo.md".to_owned(), overridden.to_owned()),
        ("agy-machine.md".to_owned(), overridden.to_owned()),
        ("agy-yolo.md".to_owned(), untranslated("Glob")),
        ("agy-yolo.md".to_owned(), untranslated("Read(~/notes)")),
    ];
    let mut actual = warnings;
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}

#[test]
fn host_skill_warnings_follow_definition_isolation_and_keep_success() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/pi.md"),
        "---\ndescription: Pi base\n---\nBase.",
    )
    .unwrap();
    for (name, isolation, skills) in [
        ("host", "host", "[not-installed]"),
        ("boxed", "scoped", "[]"),
    ] {
        std::fs::write(
                root.path().join(format!("agents/{name}.md")),
                format!(
                    "---\ndescription: Worker\nagent: pi\nisolation: {isolation}\nskills: {skills}\n---\n"
                ),
            )
            .unwrap();
    }
    let env =
        std::collections::BTreeMap::from([("HOME".to_owned(), root.path().display().to_string())]);
    let loaded = load(
        root.path(),
        SkillCheck::Check {
            env: &env,
            machine_isolation: Isolation::Scoped,
            cwd: None,
        },
        &Default::default(),
    );
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let warnings = warnings(&loaded, Isolation::Scoped);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].path.ends_with("host.md"));
    let json = serde_json::to_value(&warnings).unwrap();
    assert!(
        json[0]["message"]
            .as_str()
            .unwrap()
            .contains("pi has no per-launch skill switch")
    );
}

#[test]
fn shared_lsp_warning_requires_machine_servers_and_named_tool() {
    let root = tempfile::tempdir().unwrap();
    for namespace in ["agents", "subagents", "teams"] {
        std::fs::create_dir(root.path().join(namespace)).unwrap();
    }
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Base\n---\nBase.",
    )
    .unwrap();
    for (name, namespace, tools, skills) in [
        ("navigator", "agents", "Bash, LSP, Skill", None),
        ("skillless", "agents", "Bash, LSP", None),
        ("writer", "agents", "Write, Read, Skill", None),
        ("native", "agents", "Bash, LSP, Skill", Some(vec!["commit"])),
        ("empty", "agents", "Bash, LSP, Skill", Some(vec![])),
        (
            "child",
            "subagents",
            "Bash, LSP, Skill",
            Some(vec!["commit"]),
        ),
    ] {
        let skills = skills.map_or_else(String::new, |skills| {
            format!("skills: {}\n", serde_json::to_string(&skills).unwrap())
        });
        std::fs::write(
            root.path().join(format!("{namespace}/{name}.md")),
            format!("---\ndescription: Test\nagent: claude\ntools: [{tools}]\n{skills}---\n"),
        )
        .unwrap();
    }
    std::fs::write(
        root.path().join("agents/listed.md"),
        "---\ndescription: Test\nagent: claude\ntools: [Bash, LSP]\nmcps: [github]\n---\n",
    )
    .unwrap();
    std::fs::write(root.path().join("teams/probe.md"), "---\nleader: lead\nstages: [Plan]\nroles:\n  - agent: native\n    role: lead\n    owns: [Plan]\n---\nPipeline.").unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    assert!(lsp_warnings(&loaded, &rimz::config::MachineConfig::default()).is_empty());
    let servers = "[lsp.servers.rust]\ncommand = ['rust-analyzer']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']";
    let warned = |machine: &str| {
        let mut names: Vec<_> = lsp_warnings(&loaded, &toml::from_str(machine).unwrap())
            .into_iter()
            .map(|warning| warning.message)
            .collect();
        names.sort();
        names
    };
    assert_eq!(
        warned(&format!("[agents]\nrimz-mcp-server = false\n{servers}")),
        ["profile navigator: tools names LSP, which a room with a shared language server strips"]
    );
    let names: Vec<_> = warned(servers)
        .into_iter()
        .map(|message| message.split(':').next().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "profile child",
            "profile empty",
            "profile native",
            "profile navigator",
            "profile skillless"
        ]
    );
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Base\ntools: [Bash, LSP, Skill]\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/heir.md"),
        "---\ndescription: Test\nagent: claude\n---\n",
    )
    .unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let mut names: Vec<_> = lsp_warnings(&loaded, &toml::from_str(servers).unwrap())
        .into_iter()
        .map(|warning| warning.message.split(':').next().unwrap().to_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "profile child",
            "profile claude",
            "profile empty",
            "profile heir",
            "profile native",
            "profile navigator",
            "profile skillless"
        ]
    );
}

#[test]
fn effort_cell_appends_fast_only_when_on() {
    let mut row = definitions::DefinitionRow {
        keep_warm: None,
        tier: None,
        tier_fallback: None,
        name: "quick".into(),
        namespace: "agents".into(),
        kind: "codex".into(),
        model: None,
        effort: Some("high".into()),
        fast: false,
        source: "agents/quick.md".into(),
        origin: Default::default(),
        team: None,
        role: None,
        owns: Vec::new(),
        signals: Vec::new(),
    };
    assert_eq!(effort_label(&row), "high");
    row.fast = true;
    assert_eq!(effort_label(&row), "high · fast");
    row.effort = None;
    assert_eq!(effort_label(&row), "— · fast");
}

#[test]
fn validation_keeps_good_rows_and_collects_definition_errors() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\n---\nClaude base.",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/good.md"),
        "---\ndescription: Good\nmodel: opus\ntools: [Read, LSP]\n---\n",
    )
    .unwrap();
    std::fs::write(root.path().join("agents/bad.md"), "no frontmatter").unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.rows.iter().any(|row| row.name == "good"));
    assert_eq!(
        loaded.agent_profiles.0["good"]
            .tools
            .as_ref()
            .map(rimz::agents::ToolSet::bases),
        Some(["Read".to_owned(), "LSP".to_owned()].as_slice())
    );
    assert!(
        loaded
            .errors
            .iter()
            .any(|error| error.path.ends_with("bad.md"))
    );
    let machine =
        rimz::config::MachineConfig::parse_text(&root.path().join("config.toml"), "", root.path())
            .unwrap();
    let effective =
        rimz::config::effective::load_with_roots(&machine, root.path(), root.path()).unwrap();
    for name in loaded.failed.keys() {
        let error = effective
            .block_failed_reference(
                rimz::config::effective::ProfileScope::Agents,
                Some(name),
                None,
            )
            .unwrap_err();
        let detail = loaded
            .errors
            .iter()
            .filter(|error| loaded.failed[name].contains(&error.path))
            .map(|error| format!("{}: {}", error.path.display(), error.message))
            .collect::<Vec<_>>()
            .join("\n\n");
        assert_eq!(error.to_string(), detail);
    }
}

#[test]
fn validation_checks_skills_even_without_sandbox_config() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Claude base\n---\nClaude base.",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/worker.md"),
        "---\ndescription: Worker\nmodel: opus\ntools: [Skill]\nskills: [missing]\n---\n",
    )
    .unwrap();
    assert!(
        load(root.path(), SkillCheck::Skip, &Default::default())
            .errors
            .is_empty()
    );
    let env =
        std::collections::BTreeMap::from([("HOME".to_owned(), root.path().display().to_string())]);
    let check = SkillCheck::Check {
        env: &env,
        machine_isolation: Isolation::Host,
        cwd: Some(root.path()),
    };
    let checked = load(root.path(), check, &Default::default());
    assert!(
        checked
            .errors
            .iter()
            .any(|error| error.message.contains("missing"))
    );
    std::fs::create_dir_all(root.path().join(".claude/skills/missing")).unwrap();
    std::fs::write(
        root.path().join(".claude/skills/missing/SKILL.md"),
        "---\ndescription: provider-root only\n---\nSkill.",
    )
    .unwrap();
    let checked = load(root.path(), check, &Default::default());
    assert!(checked.errors.is_empty(), "{:?}", checked.errors);
}

#[test]
fn validation_resolves_listed_skills_against_the_current_checkout() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for kind in ["claude", "codex", "qoder"] {
        std::fs::write(
            root.path().join(format!("agents/{kind}-worker.md")),
            format!(
                "---\ndescription: Worker\nagent: {kind}\ntools: [Skill]\nskills: [proj]\n---\n"
            ),
        )
        .unwrap();
    }
    std::fs::write(
        root.path().join("agents/codex-reference.md"),
        "---\ndescription: Reference\nagent: codex\ntools: [Skill]\nskills: [proj]\n---\nUse skill(proj).",
    )
    .unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let tier = checkout.path().join(".agents/skills");
    std::fs::create_dir_all(tier.join("proj")).unwrap();
    std::fs::write(
        tier.join("proj/SKILL.md"),
        "---\nname: proj\ndescription: Project.\n---\nProject.",
    )
    .unwrap();
    let env =
        std::collections::BTreeMap::from([("HOME".to_owned(), root.path().display().to_string())]);
    let errors = |cwd: &Path| {
        load(
            root.path(),
            SkillCheck::Check {
                env: &env,
                machine_isolation: Isolation::Host,
                cwd: Some(cwd),
            },
            &Default::default(),
        )
        .errors
        .into_iter()
        .map(|error| {
            let file = error.path.file_name().unwrap().to_str().unwrap().to_owned();
            (file, error.message)
        })
        .collect::<Vec<_>>()
    };
    let only = |kind: &str| {
        format!(
            "listed skill 'proj' is only in the project skills at {tier}, which {kind} does not \
             read at host isolation; add {checkout}/.{kind}/skills/proj (a symlink to \
             {tier}/proj works), launch with scoped isolation, or remove it from skills",
            tier = tier.display(),
            checkout = checkout.path().display()
        )
    };
    assert_eq!(
        errors(checkout.path()),
        [
            ("claude-worker.md".to_owned(), only("claude")),
            ("qoder-worker.md".to_owned(), only("qoder")),
        ],
        "codex reads the checkout's .agents/skills, so its workers validate, while a qoder \
         launch refuses the name though qoder has no switch"
    );
    let loaded = load(
        root.path(),
        SkillCheck::Check {
            env: &env,
            machine_isolation: Isolation::Host,
            cwd: Some(checkout.path()),
        },
        &Default::default(),
    );
    let chain = rimz::agents::skills::SkillChain::from_home(root.path());
    let references: Vec<_> = capability_errors(
        &loaded,
        &Default::default(),
        disk_skills(&env, &chain, Some(checkout.path())),
    )
    .into_iter()
    .filter(|error| error.path.ends_with("codex-reference.md"))
    .map(|error| error.message)
    .collect();
    assert!(
        references.is_empty(),
        "skill(proj) names the project skill the launch reaches: {references:?}"
    );
    let marked = checkout.path().join(".qoder/skills/proj");
    std::fs::create_dir_all(&marked).unwrap();
    std::fs::write(
        marked.join("SKILL.md"),
        "---\nname: proj\ndescription: Project.\ndisable-model-invocation: true\n---\nProject.",
    )
    .unwrap();
    assert_eq!(
        errors(checkout.path()),
        [("claude-worker.md".to_owned(), only("claude"))],
        "qoder reads its own user-only copy, and a kind without a switch skips marker checks"
    );
    std::fs::remove_dir_all(checkout.path().join(".qoder")).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let missing = errors(elsewhere.path());
    assert_eq!(missing.len(), 3, "{missing:?}");
    assert!(
        missing.iter().all(|(_, message)| message.contains(
            &elsewhere
                .path()
                .join(".agents/skills/proj/SKILL.md")
                .display()
                .to_string()
        )),
        "outside the checkout the miss names the tier it searched: {missing:?}"
    );
}

/// Config load has no checkout, so a RimZ-owned link left in the provider root and a broken
/// provider-root link, both of which discovery skips, leave the listed name to the launch.
#[test]
fn load_leaves_a_name_behind_a_skipped_provider_root_link_to_the_launch() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/worker.md"),
        "---\ndescription: Worker\nagent: claude\ntools: [Skill]\nskills: [deploy, gone]\n---\n",
    )
    .unwrap();
    let provider = root.path().join(".claude/skills");
    std::fs::create_dir_all(&provider).unwrap();
    std::os::unix::fs::symlink(root.path().join("skills/deploy"), provider.join("deploy")).unwrap();
    std::os::unix::fs::symlink(root.path().join("missing"), provider.join("gone")).unwrap();
    let env =
        std::collections::BTreeMap::from([("HOME".to_owned(), root.path().display().to_string())]);
    let loaded = load(
        root.path(),
        SkillCheck::Check {
            env: &env,
            machine_isolation: Isolation::Host,
            cwd: None,
        },
        &Default::default(),
    );
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
}

/// A scoped Claude launch reads its checkout's own `.claude/skills` natively, so validate
/// resolves a listed name found only there and names that folder among the roots of a miss.
#[test]
fn validation_of_a_scoped_profile_reads_the_kinds_own_project_folder() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/scoped.md"),
        "---\ndescription: Scoped\nagent: claude\nisolation: scoped\ntools: [Skill]\nskills: [own, absent]\n---\n",
    )
    .unwrap();
    let checkout = tempfile::tempdir().unwrap();
    let own = checkout.path().join(".claude/skills");
    std::fs::create_dir_all(own.join("own")).unwrap();
    std::fs::write(
        own.join("own/SKILL.md"),
        "---\nname: own\ndescription: Own.\n---\nOwn.",
    )
    .unwrap();
    let env =
        std::collections::BTreeMap::from([("HOME".to_owned(), root.path().display().to_string())]);
    let errors = load(
        root.path(),
        SkillCheck::Check {
            env: &env,
            machine_isolation: Isolation::Host,
            cwd: Some(checkout.path()),
        },
        &Default::default(),
    )
    .errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    let message = &errors[0].message;
    assert!(
        message.starts_with("lists skill 'absent', missing at "),
        "only the name no root holds fails: {message}"
    );
    assert!(
        message.contains(&own.join("absent/SKILL.md").display().to_string()),
        "the miss names the kind's own project folder: {message}"
    );
}

#[test]
fn capability_references_error_when_unknown_or_unreachable_and_never_on_skill_text() {
    let root = tempfile::tempdir().unwrap();
    for namespace in ["agents", "subagents"] {
        std::fs::create_dir(root.path().join(namespace)).unwrap();
    }
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Base\n---\nBase names spawn_detached_rimz(x) and Rimz(Wait).",
    )
    .unwrap();
    for (name, namespace, fields, body) in [
        (
            "reaches",
            "agents",
            "tools: [Bash, Skill]\n",
            "Use rimz(watch), `rimz(chat)`, and rimz(lsp).",
        ),
        (
            "quoted",
            "agents",
            "tools: [Bash, Skill]\n",
            "Use Skill(rimz-nope) and Skill(rimz-lsp).",
        ),
        (
            "unknown",
            "agents",
            "tools: [Bash, Skill]\n",
            "Use rimz(nope).",
        ),
        // Named native tools beside RimZ's default outrank it, so neither capability is reached.
        (
            "yields",
            "agents",
            "tools: [Read, Skill, AskUserQuestion, Agent]\n",
            "Use rimz(ask) and rimz(subagents).",
        ),
        (
            "shut",
            "agents",
            "tools: [Bash]\nmcps: [github]\n",
            "Use rimz(watch) and rimz(chat).",
        ),
        (
            "unlisted",
            "agents",
            "tools: [Bash, Skill]\nskills: []\n",
            "Use rimz(chat) and rimz(notes).",
        ),
        (
            "child",
            "subagents",
            "tools: [Bash, Skill]\n",
            "Use rimz(subagents), rimz(agents), rimz(teams), and rimz(watch).",
        ),
    ] {
        std::fs::write(
            root.path().join(format!("{namespace}/{name}.md")),
            format!("---\ndescription: Test\nagent: claude\n{fields}---\n{body}"),
        )
        .unwrap();
    }
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let on_disk = |kind: &str, _scoped: bool| {
        assert_eq!(kind, "claude");
        Some(["rimz-chat".to_owned()].into())
    };
    let errors = |machine: &str| {
        let mut messages: Vec<_> =
            capability_errors(&loaded, &toml::from_str(machine).unwrap(), on_disk)
                .into_iter()
                .map(|error| {
                    let file = error.path.strip_prefix(root.path()).unwrap().to_owned();
                    format!("{}: {}", file.display(), error.message)
                })
                .collect();
        messages.sort();
        messages
    };
    assert_eq!(
        errors(""),
        [
            "agents/shut.md: profile shut: rimz(chat) names `rimz-chat`, which this \
             profile cannot reach: its tools leave out Skill",
            "agents/shut.md: profile shut: rimz(watch) names `rimz-watch`, which this profile \
             cannot reach: its mcps leave out rimz-watch; its tools leave out Skill",
            "agents/unknown.md: profile unknown: rimz(nope) names `rimz-nope`, which is neither \
             a RimZ capability nor a skill on disk for claude",
            "agents/unlisted.md: profile unlisted: rimz(chat) names `rimz-chat`, which \
             this profile cannot reach: its skills leave out rimz-chat",
            "agents/yields.md: profile yields: rimz(ask) names `rimz-ask`, which this profile \
             cannot reach: its tools list names the provider's own tool beside a blanket RimZ \
             grant",
            "agents/yields.md: profile yields: rimz(subagents) names `rimz-subagents`, which \
             this profile cannot reach: its tools list names the provider's own tool beside a \
             blanket RimZ grant",
            "subagents/child.md: profile child: rimz(agents) names `rimz-agents`, which this \
             profile cannot reach: a subagent cannot launch agents",
            "subagents/child.md: profile child: rimz(subagents) names `rimz-subagents`, which \
             this profile cannot reach: a subagent cannot launch subagents",
            "subagents/child.md: profile child: rimz(teams) names `rimz-teams`, which this \
             profile cannot reach: a subagent cannot launch teams",
        ]
    );
    // With the server off, a capability is reached only as a skill on disk.
    let off: Vec<_> = errors("[agents]\nrimz-mcp-server = false")
        .into_iter()
        .filter(|message| message.starts_with("agents/reaches.md"))
        .collect();
    assert_eq!(
        off,
        [
            "agents/reaches.md: profile reaches: rimz(lsp) names `rimz-lsp`, which this profile \
             cannot reach: the machine's rimz-mcp-server is off; no rimz-lsp skill is on disk for \
             claude",
            "agents/reaches.md: profile reaches: rimz(watch) names `rimz-watch`, which this \
             profile cannot reach: the machine's rimz-mcp-server is off; no rimz-watch skill is on \
             disk for claude",
        ]
    );
    // An unchecked disk (validate inside a scoped view) judges the lists alone.
    let unchecked: Vec<_> = capability_errors(&loaded, &Default::default(), |_, _| None)
        .into_iter()
        .map(|error| error.message)
        .filter(|message| {
            message.starts_with("profile unknown") || message.starts_with("profile unlisted")
        })
        .collect();
    assert_eq!(
        unchecked,
        [
            "profile unlisted: rimz(chat) names `rimz-chat`, which this profile cannot \
          reach: its skills leave out rimz-chat"
        ]
    );
}

#[test]
fn a_grant_decides_which_rimz_capabilities_a_profile_reaches() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Base\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        root.path().join("agents/granted.md"),
        "---\ndescription: Test\nagent: claude\ntools: [Bash, Skill]\nmcps: []\nskills: []\n---\nBody.",
    )
    .unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let (_, profile) = own_rows(&loaded)
        .find(|(row, _)| row.name == "granted")
        .unwrap();
    let mut profile = profile.clone();
    profile.tools = profile
        .tools
        .map(|tools| tools.with_rimz(vec!["rimz-watch".parse().unwrap()]));
    let machine = rimz::config::MachineConfig::default();
    let reasons = |name| shut_out(&machine, "claude", &profile, false, name);
    assert_eq!(reasons("rimz-watch"), (Vec::new(), Vec::new()));
    let left_out = vec!["its capabilities leave out rimz-message".to_owned()];
    assert_eq!(reasons("rimz-message"), (left_out.clone(), left_out));
    // A `rimz-*` skill alone is the skills list's to admit, whatever the grant names.
    let no_tools = vec!["rimz-reflect has no MCP tools".to_owned()];
    assert_eq!(
        reasons("rimz-reflect"),
        (
            no_tools.clone(),
            vec!["its skills leave out rimz-reflect".to_owned()]
        )
    );
    profile.skills = None;
    let reasons = |name| shut_out(&machine, "claude", &profile, false, name);
    assert_eq!(reasons("rimz-reflect"), (no_tools, Vec::new()));
}

#[test]
fn skill_references_check_lists_disk_and_capability_shorthand() {
    let root = tempfile::tempdir().unwrap();
    for namespace in ["agents", "teams"] {
        std::fs::create_dir(root.path().join(namespace)).unwrap();
    }
    std::fs::write(
        root.path().join("agents/claude.md"),
        "---\ndescription: Base\n---\nBase.",
    )
    .unwrap();
    for (name, fields, body) in [
        (
            "open",
            "tools: [Bash, Skill]\n",
            "Use skill(commit), skill(_shared), skill(rimz-reflect), skill(missing), Skill(missing), and Agent(missing).",
        ),
        (
            "listed",
            "tools: [Bash, Skill]\nskills: [commit]\n",
            "Use skill(commit), skill(rebase), and skill(rimz-reflect).",
        ),
        ("no-skill-tool", "tools: [Bash]\n", "Use skill(commit)."),
        (
            "cap-skill",
            "capabilities: [read, rimz-lsp]\nskills: []\n",
            "Use skill(rimz-lsp) and skill(rimz-watch).",
        ),
        (
            "cap-skill-unlisted",
            "tools: [Bash, Skill]\nmcps: []\nskills: []\n",
            "Use skill(rimz-lsp).",
        ),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\nagent: claude\n{fields}---\n{body}"),
        )
        .unwrap();
    }
    std::fs::write(
        root.path().join("teams/probe.md"),
        "---\nleader: lead\nstages: [Plan]\nroles:\n  - agent: open\n    role: lead\n    owns: [Plan]\n---\nTeam body mentions skill(team-missing) and subagent(team-missing).",
    )
    .unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let on_disk = |kind: &str, _scoped: bool| {
        assert_eq!(kind, "claude");
        Some(
            ["commit", "_shared", "rebase", "rimz-reflect"]
                .map(str::to_owned)
                .into(),
        )
    };
    let collect = |disk: Option<std::collections::BTreeSet<String>>| {
        let mut messages: Vec<_> =
            capability_errors(&loaded, &Default::default(), |_, _| disk.clone())
                .into_iter()
                .map(|error| {
                    let file = error.path.strip_prefix(root.path()).unwrap().to_owned();
                    format!("{}: {}", file.display(), error.message)
                })
                .collect();
        messages.sort();
        messages
    };
    assert_eq!(
        collect(on_disk("claude", false)),
        [
            "agents/cap-skill-unlisted.md: profile cap-skill-unlisted: rimz(lsp) names `rimz-lsp`, \
             which this profile cannot reach: its mcps leave out rimz-lsp; its skills leave out \
             rimz-lsp",
            "agents/cap-skill.md: profile cap-skill: rimz(watch) names `rimz-watch`, which this \
             profile cannot reach: its capabilities leave out rimz-watch",
            "agents/listed.md: profile listed: skill(rebase) names `rebase`, which this profile \
             cannot reach: its skills leave out rebase",
            "agents/listed.md: profile listed: skill(rimz-reflect) names `rimz-reflect`, which \
             this profile cannot reach: its skills leave out rimz-reflect",
            "agents/no-skill-tool.md: profile no-skill-tool: skill(commit) names `commit`, which \
             this profile cannot reach: its tools leave out Skill",
            "agents/open.md: profile open: skill(missing) names `missing`, which is not a skill on \
             disk for claude",
        ]
    );
    // Inside a scoped view the disk check is skipped, while list and tool checks still run.
    assert_eq!(
        collect(None),
        [
            "agents/cap-skill-unlisted.md: profile cap-skill-unlisted: rimz(lsp) names `rimz-lsp`, \
             which this profile cannot reach: its mcps leave out rimz-lsp; its skills leave out \
             rimz-lsp",
            "agents/cap-skill.md: profile cap-skill: rimz(watch) names `rimz-watch`, which this \
             profile cannot reach: its capabilities leave out rimz-watch",
            "agents/listed.md: profile listed: skill(rebase) names `rebase`, which this profile \
             cannot reach: its skills leave out rebase",
            "agents/listed.md: profile listed: skill(rimz-reflect) names `rimz-reflect`, which \
             this profile cannot reach: its skills leave out rimz-reflect",
            "agents/no-skill-tool.md: profile no-skill-tool: skill(commit) names `commit`, which \
             this profile cannot reach: its tools leave out Skill",
        ]
    );
}

#[test]
fn subagent_references_check_allowlists_catalog_commands_aliases_and_children() {
    let root = tempfile::tempdir().unwrap();
    for namespace in ["agents", "subagents"] {
        std::fs::create_dir(root.path().join(namespace)).unwrap();
    }
    for kind in ["claude", "antigravity", "opencode"] {
        std::fs::write(
            root.path().join(format!("agents/{kind}.md")),
            "---\ndescription: Base\n---\nBase.",
        )
        .unwrap();
    }
    for name in ["explorer", "reviewer"] {
        std::fs::write(
            root.path().join(format!("subagents/{name}.md")),
            "---\ndescription: Child\nagent: claude\ntools: [Read]\n---\nChild.",
        )
        .unwrap();
    }
    std::fs::write(
        root.path().join("subagents/child.md"),
        "---\ndescription: Child\nagent: claude\ntools: [Read]\n---\nUse subagent(explorer).",
    )
    .unwrap();
    for (name, fields, body) in [
        (
            "open",
            "tools: [Bash, Skill]\n",
            "Use subagent(explorer), subagent(lint), subagent(agy), subagent(opencode-mini), subagent(missing), and Agent(missing).",
        ),
        (
            "narrow",
            "tools: [Bash, Skill]\nsubagents: [explorer]\n",
            "Use subagent(explorer), subagent(reviewer), subagent(lint), subagent(opencode-mini), and subagent(missing).",
        ),
        (
            "none",
            "tools: [Bash, Skill]\nsubagents: []\n",
            "Use subagent(explorer).",
        ),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\nagent: claude\n{fields}---\n{body}"),
        )
        .unwrap();
    }
    let machine: rimz::config::MachineConfig =
        toml::from_str("[agents.commands]\nlint = 'cargo clippy'\n").unwrap();
    let loaded = load(root.path(), SkillCheck::Skip, &machine);
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let mut messages: Vec<_> = capability_errors(&loaded, &machine, |_, _| None)
        .into_iter()
        .map(|error| {
            let file = error.path.strip_prefix(root.path()).unwrap().to_owned();
            format!("{}: {}", file.display(), error.message)
        })
        .collect();
    messages.sort();
    assert_eq!(
        messages,
        [
            "agents/narrow.md: profile narrow: subagent(lint) names `lint`, which this profile \
             cannot reach: its subagents leave out lint",
            "agents/narrow.md: profile narrow: subagent(missing) names `missing`, which is not a \
             configured subagent profile",
            "agents/narrow.md: profile narrow: subagent(opencode-mini) names `opencode-mini`, \
             which this profile cannot reach: its subagents leave out opencode-mini",
            "agents/narrow.md: profile narrow: subagent(reviewer) names `reviewer`, which this \
             profile cannot reach: its subagents leave out reviewer",
            "agents/none.md: profile none: subagent(explorer) names `explorer`, which this profile \
             cannot reach: its subagents leave out explorer",
            "agents/open.md: profile open: subagent(missing) names `missing`, which is not a \
             configured subagent profile",
            "subagents/child.md: profile child: subagent(explorer) names `explorer`, which this \
             profile cannot reach: a subagent cannot launch subagents",
        ]
    );

    // A `[agents.commands]` key equal to a provider alias matches the catalog directly even when
    // that provider has no home kind base.
    let cmd_root = tempfile::tempdir().unwrap();
    std::fs::create_dir(cmd_root.path().join("agents")).unwrap();
    std::fs::write(
        cmd_root.path().join("agents/claude.md"),
        "---\ndescription: Base\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        cmd_root.path().join("agents/open.md"),
        "---\ndescription: Test\nagent: claude\ntools: [Bash, Skill]\n---\nUse subagent(agy).",
    )
    .unwrap();
    std::fs::write(
        cmd_root.path().join("agents/narrow.md"),
        "---\ndescription: Test\nagent: claude\ntools: [Bash, Skill]\nsubagents: [claude]\n---\nUse subagent(agy).",
    )
    .unwrap();
    std::fs::write(
        cmd_root.path().join("agents/allow-agy.md"),
        "---\ndescription: Test\nagent: claude\ntools: [Bash, Skill]\nsubagents: [agy]\n---\nUse subagent(agy).",
    )
    .unwrap();
    let cmd_machine: rimz::config::MachineConfig =
        toml::from_str("[agents.commands]\nagy = 'echo hi'\n").unwrap();
    let cmd_loaded = load(cmd_root.path(), SkillCheck::Skip, &cmd_machine);
    assert!(cmd_loaded.errors.is_empty(), "{:?}", cmd_loaded.errors);
    let mut cmd_messages: Vec<_> = capability_errors(&cmd_loaded, &cmd_machine, |_, _| None)
        .into_iter()
        .map(|error| {
            let file = error.path.strip_prefix(cmd_root.path()).unwrap().to_owned();
            format!("{}: {}", file.display(), error.message)
        })
        .collect();
    cmd_messages.sort();
    assert_eq!(
        cmd_messages,
        [
            "agents/narrow.md: profile narrow: subagent(agy) names `agy`, which this profile \
             cannot reach: its subagents leave out agy",
        ]
    );
}

#[test]
fn notes_name_each_kind_whose_definitions_run_on_the_built_in_base() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("claude", "tools: [Read]\n"),
        ("on_claude", "agent: claude\n"),
        ("on_codex", "agent: codex\n"),
        ("also_codex", "agent: codex\n"),
        ("on_amp", "agent: amp\n"),
    ] {
        let body = if name == "on_amp" { "" } else { "Craft." };
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\n{fields}---\n{body}"),
        )
        .unwrap();
    }
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    assert_eq!(
        built_in_base_notes(&loaded, root.path()),
        [format!(
            "note: codex definitions run on the built-in kind base; write {} to replace it",
            root.path().join("agents/codex.md").display()
        )]
    );
}

/// A definition or kind base whose named native tool outranks a blanket RimZ grant warns once at
/// its own file with the load's attribution, and a `rimz(ask)` in its prompt reads as
/// unreachable; one whose chain lists no tools does not warn.
#[test]
fn a_named_native_tool_beside_a_blanket_grant_warns_at_its_definition() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("agents")).unwrap();
    for (name, fields) in [
        ("claude", ""),
        ("codex", "capabilities: [read, ask, rimz]\n"),
        ("asks", "agent: claude\ntools: [Bash, AskUserQuestion]\n"),
        ("plain", "agent: claude\ntools: [Bash]\n"),
    ] {
        std::fs::write(
            root.path().join(format!("agents/{name}.md")),
            format!("---\ndescription: Test\n{fields}---\nBase. Ask with rimz(ask)."),
        )
        .unwrap();
    }
    let loaded = load(root.path(), SkillCheck::Skip, &Default::default());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let machine = rimz::config::MachineConfig::default();
    let warnings = native_tool_warnings(&loaded, &machine);
    let mut warned: Vec<_> = warnings
        .iter()
        .map(|warning| (warning.path.clone(), warning.message.clone()))
        .collect();
    warned.sort();
    let [(asks, asks_message), (codex, codex_message)] = warned.as_slice() else {
        panic!("{warned:?}");
    };
    assert!(asks.ends_with("agents/asks.md"), "{asks:?}");
    assert!(asks_message.contains(
        "lists `AskUserQuestion` in `tools:` and gets `rimz-ask` from RimZ's default (no \
         `mcps`); using the native question tool"
    ));
    assert!(codex.ends_with("agents/codex.md"), "{codex:?}");
    assert!(
        codex_message.contains("codex.md lists `ask` in `capabilities:`"),
        "{codex_message}"
    );
    let mut unreachable: Vec<_> = capability_errors(&loaded, &machine, |_, _| None)
        .into_iter()
        .filter(|error| error.message.contains("rimz(ask)"))
        .map(|error| error.path)
        .collect();
    unreachable.sort();
    assert_eq!(
        unreachable,
        ["asks", "codex"].map(|name| root.path().join(format!("agents/{name}.md")))
    );
}
