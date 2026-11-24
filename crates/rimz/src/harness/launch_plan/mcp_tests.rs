//! RimZ's MCP server entry, its grant, and the `mcps` allowlist, through the
//! launch compiler, on Claude, Codex, and a kind that takes neither.

use super::*;

struct Room {
    project: tempfile::TempDir,
    runtime: RuntimePaths,
    state: StatePaths,
    ambient: BTreeMap<String, String>,
}

impl Room {
    fn new() -> Self {
        let project = tempfile::tempdir().unwrap();
        let id = crate::WorkspaceId::from_project_root(project.path());
        let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
        let state = StatePaths::under(id, project.path()).unwrap();
        let home = project.path().join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let plugin = home.join(".gemini/config/plugins/rimz");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"rimz","author":"RimZ"}"#,
        )
        .unwrap();
        let ambient = BTreeMap::from([
            ("HOME".into(), home.display().to_string()),
            (
                "CLAUDE_CONFIG_DIR".into(),
                home.join(".claude").display().to_string(),
            ),
            (
                "CODEX_HOME".into(),
                home.join(".codex").display().to_string(),
            ),
            ("RIMZ_MCP_PROBE".into(), "1".into()),
        ]);
        Self {
            project,
            runtime,
            state,
            ambient,
        }
    }

    fn codex_config(&self, name: &str, text: &str) {
        std::fs::write(Path::new(&self.ambient["CODEX_HOME"]).join(name), text).unwrap();
    }

    fn compile(&self, machine: &str, request: &ExecRequest) -> LaunchPlan {
        self.compile_at(machine, request, Isolation::Host)
    }

    fn compile_at(&self, machine: &str, request: &ExecRequest, isolation: Isolation) -> LaunchPlan {
        self.compile_config(&toml::from_str(machine).unwrap(), request, isolation)
    }

    fn compile_config(
        &self,
        machine: &crate::config::MachineConfig,
        request: &ExecRequest,
        isolation: Isolation,
    ) -> LaunchPlan {
        let effective = crate::config::effective::load_with_roots(
            machine,
            self.project.path(),
            self.project.path(),
        )
        .unwrap();
        compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request,
            cwd: self.project.path(),
            project_root: self.project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &self.runtime,
            state: &self.state,
            effective: Some(&effective),
            machine,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &self.ambient,
            agent_shell: None,
            hibernate: false,
        })
        .unwrap()
    }
}

fn launch(kind: &str, mcps: Option<&[&str]>, args: &[&str]) -> ExecRequest {
    let mut request = request(
        kind,
        ExecAction::Launch {
            prompt: Some("go".into()),
            extra_args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        },
    );
    request.mcps = mcps.map(|names| names.iter().map(|name| name.parse().unwrap()).collect());
    request
}

fn switches(routine: bool, tools: bool) -> String {
    format!("[agents]\nallow-routine-rimz = {routine}\nrimz-mcp-server = {tools}\n")
}

fn flag_values<'a>(argv: &'a [String], flag: &str) -> Vec<&'a str> {
    argv.windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].as_str())
        .collect()
}

/// The Claude `--mcp-config` entry for `rimz`, and the next argument after it
/// (empty at the end of the argv).
fn claude_entry(argv: &[String]) -> Option<(serde_json::Value, &str)> {
    let index = argv.iter().position(|arg| arg == "--mcp-config")?;
    let config: serde_json::Value = serde_json::from_str(&argv[index + 1]).unwrap();
    Some((
        config["mcpServers"]["rimz"].clone(),
        argv.get(index + 2).map_or("", String::as_str),
    ))
}

fn claude_settings(argv: &[String]) -> serde_json::Value {
    flag_values(argv, "--settings")
        .last()
        .map_or(serde_json::json!({}), |value| {
            serde_json::from_str(value).unwrap()
        })
}

fn granted(settings: &serde_json::Value) -> bool {
    settings["permissions"]["allow"]
        .as_array()
        .is_some_and(|allow| allow.contains(&serde_json::json!("mcp__rimz__*")))
}

fn allowlist(settings: &serde_json::Value) -> Option<Vec<String>> {
    settings.get("allowedMcpServers").map(|list| {
        list.as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["serverName"].as_str().unwrap().to_owned())
            .collect()
    })
}

/// Every Codex `-c` override, parsed as `key = value` TOML.
fn codex_overrides(argv: &[String]) -> Vec<toml::Table> {
    flag_values(argv, "-c")
        .into_iter()
        .filter(|value| value.starts_with("mcp_servers"))
        .map(|value| value.parse::<toml::Table>().unwrap())
        .collect()
}

fn codex_entry(argv: &[String]) -> Option<toml::Table> {
    let entry: toml::Table = codex_overrides(argv)
        .into_iter()
        .filter_map(|table| table.get("mcp_servers")?.get("rimz")?.as_table().cloned())
        .flatten()
        .collect();
    (!entry.is_empty()).then_some(entry)
}

/// The servers a Codex launch switches off.
fn codex_disabled(argv: &[String]) -> Vec<String> {
    codex_overrides(argv)
        .into_iter()
        .filter_map(|table| table.get("mcp_servers")?.as_table().cloned())
        .flatten()
        .filter(|(name, entry)| name != "rimz" && entry.get("enabled") == Some(&false.into()))
        .map(|(name, _)| name)
        .collect()
}

#[test]
fn claude_entry_carries_the_room_link_and_the_grant_follows_the_switch() {
    let room = Room::new();
    let argv = room
        .compile(&switches(true, true), &launch("claude", None, &[]))
        .process()
        .provider_argv
        .clone();
    let (entry, next) = claude_entry(&argv).expect("entry by default");
    assert_eq!(entry["command"], "/bin/rimz");
    assert_eq!(entry["args"], serde_json::json!(["mcp", "serve"]));
    assert!(next.starts_with('-'), "{argv:?}");
    std::fs::create_dir_all(room.state.room_bin.parent().unwrap()).unwrap();
    std::fs::write(&room.state.room_bin, "").unwrap();
    for routine in [true, false] {
        let argv = room
            .compile(&switches(routine, true), &launch("claude", None, &[]))
            .process()
            .provider_argv
            .clone();
        let (entry, next) = claude_entry(&argv).unwrap();
        assert_eq!(entry["command"], room.state.room_bin.display().to_string());
        assert!(next.starts_with('-'), "{argv:?}");
        let settings = claude_settings(&argv);
        assert_eq!(granted(&settings), routine, "{settings}");
        assert_eq!(allowlist(&settings), None);
    }
}

/// `--mcp-config` takes several values, so no positional may follow its
/// value on any action: a passthrough `hello` stays the positional it was.
#[test]
fn claude_entry_is_never_followed_by_a_passthrough_positional() {
    let room = Room::new();
    let passthrough = || vec!["hello".to_owned()];
    for action in [
        ExecAction::Launch {
            prompt: None,
            extra_args: passthrough(),
        },
        ExecAction::Launch {
            prompt: Some("go".into()),
            extra_args: passthrough(),
        },
        ExecAction::Pick {
            extra_args: passthrough(),
        },
        ExecAction::Resume {
            session_id: "11111111-2222-3333-4444-555555555555".into(),
            extra_args: passthrough(),
        },
        ExecAction::Fork {
            session_id: "11111111-2222-3333-4444-555555555555".into(),
            extra_args: passthrough(),
        },
    ] {
        let argv = room
            .compile(&switches(true, true), &request("claude", action))
            .process()
            .provider_argv
            .clone();
        let (_, next) = claude_entry(&argv).expect("an entry");
        assert!(next.is_empty() || next.starts_with('-'), "{argv:?}");
        assert!(argv.iter().any(|arg| arg == "hello"), "{argv:?}");
    }
}

#[test]
fn claude_mcps_lists_become_only_args_and_the_exact_allowlist() {
    let room = Room::new();
    let tools = ["--tools", "Bash"];
    let argv = |machine: &str, mcps: Option<&[&str]>| {
        room.compile(machine, &launch("claude", mcps, &tools))
            .process()
            .provider_argv
            .clone()
    };
    let unlisted = argv(&switches(true, true), None);
    assert!(!unlisted.iter().any(|arg| arg == "--strict-mcp-config"));
    assert_eq!(allowlist(&claude_settings(&unlisted)), None);

    let listed = argv(
        &switches(true, true),
        Some(&["rimz-lsp", "github", "rimz-notes"]),
    );
    let (entry, _) = claude_entry(&listed).unwrap();
    assert_eq!(
        entry["args"],
        serde_json::json!(["mcp", "serve", "--only", "rimz-lsp", "--only", "rimz-notes"])
    );
    assert!(!listed.iter().any(|arg| arg == "--strict-mcp-config"));
    assert_eq!(
        allowlist(&claude_settings(&listed)),
        Some(vec!["rimz".to_owned(), "github".to_owned()])
    );

    let cleared = argv(&switches(true, true), Some(&[]));
    assert_eq!(claude_entry(&cleared), None);
    assert!(!granted(&claude_settings(&cleared)));
    assert_eq!(allowlist(&claude_settings(&cleared)), Some(Vec::new()));

    let off = argv(&switches(true, false), Some(&["github"]));
    assert_eq!(claude_entry(&off), None);
    assert!(!granted(&claude_settings(&off)));
    assert_eq!(
        allowlist(&claude_settings(&off)),
        Some(vec!["github".to_owned()])
    );
    assert_eq!(claude_entry(&argv(&switches(true, false), None)), None);
}

#[test]
fn codex_entry_names_the_spawn_env_and_approval_follows_the_switch() {
    let room = Room::new();
    for (routine, approval) in [(true, "approve"), (false, "prompt")] {
        let plan = room.compile(&switches(routine, true), &launch("codex", None, &[]));
        let entry = codex_entry(&plan.process().provider_argv).expect("entry by default");
        assert_eq!(entry["command"].as_str(), Some("/bin/rimz"));
        assert_eq!(
            entry["args"],
            toml::Value::try_from(["mcp", "serve", "--parent-env"]).unwrap()
        );
        assert_eq!(entry["tool_timeout_sec"].as_integer(), Some(600));
        assert_eq!(
            entry["default_tools_approval_mode"].as_str(),
            Some(approval)
        );
        let names: Vec<_> = entry["env_vars"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        for name in plan
            .process()
            .env
            .keys()
            .map(String::as_str)
            .chain(["RIMZ_MCP_PROBE", "HOME"])
        {
            assert!(names.contains(&name), "{name} missing from {names:?}");
        }
    }
    let argv = room
        .compile(
            &switches(true, true),
            &launch("codex", Some(&["rimz-wait"]), &[]),
        )
        .process()
        .provider_argv
        .clone();
    assert_eq!(
        codex_entry(&argv).unwrap()["args"],
        toml::Value::try_from(["mcp", "serve", "--only", "rimz-wait", "--parent-env"]).unwrap()
    );
    for (machine, mcps) in [
        (switches(true, false), None),
        (switches(true, true), Some(&[][..])),
    ] {
        let plan = room.compile(&machine, &launch("codex", mcps, &[]));
        assert_eq!(codex_entry(&plan.process().provider_argv), None);
    }
}

#[test]
fn codex_mcps_lists_disable_only_the_unlisted_servers_the_account_configures() {
    let room = Room::new();
    room.codex_config(
        "config.toml",
        "[mcp_servers.github]\ncommand = 'gh'\n[mcp_servers.\"my.server\"]\ncommand = 'x'\n[mcp_servers.keep]\ncommand = 'k'\n[mcp_servers.rimz-lsp]\ncommand = 'l'\n",
    );
    room.codex_config("work.config.toml", "[mcp_servers.chosen]\ncommand = 'c'\n");
    let disabled = |mcps: Option<&[&str]>, args: &[&str]| {
        let plan = room.compile(&switches(true, true), &launch("codex", mcps, args));
        let mut names = codex_disabled(&plan.process().provider_argv);
        names.sort();
        names
    };
    assert_eq!(disabled(None, &[]), Vec::<String>::new());
    assert_eq!(
        disabled(Some(&["keep", "absent", "rimz-lsp"]), &[]),
        ["github", "my.server", "rimz-lsp"]
    );
    assert_eq!(
        disabled(Some(&["keep", "rimz-lsp:always"]), &[]),
        ["github", "my.server", "rimz-lsp"]
    );
    assert_eq!(
        disabled(Some(&["keep"]), &["--profile", "work"]),
        ["chosen", "github", "my.server", "rimz-lsp"]
    );
    assert_eq!(
        disabled(Some(&["keep"]), &["-pwork"]),
        ["chosen", "github", "my.server", "rimz-lsp"]
    );
    assert_eq!(
        disabled(Some(&[]), &[]),
        ["github", "keep", "my.server", "rimz-lsp"]
    );
    let plan = room.compile(&switches(true, true), &launch("codex", Some(&[]), &[]));
    assert_eq!(codex_entry(&plan.process().provider_argv), None);

    room.codex_config("config.toml", "[mcp_servers.github\n");
    let plan = room.compile(
        &switches(true, true),
        &launch("codex", Some(&["keep"]), &[]),
    );
    assert_eq!(
        codex_disabled(&plan.process().provider_argv),
        Vec::<String>::new()
    );
    let warnings: Vec<_> = plan.warnings.iter().map(ToString::to_string).collect();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("mcps") && warning.contains("config.toml")),
        "{warnings:?}"
    );
}

#[test]
fn other_kinds_get_no_entry_and_a_warning_for_a_list() {
    let room = Room::new();
    for mcps in [None, Some(&["github"][..])] {
        let plan = room.compile(&switches(true, true), &launch("opencode", mcps, &[]));
        assert!(
            !plan
                .process()
                .provider_argv
                .iter()
                .any(|arg| arg.contains("mcp")),
            "{:?}",
            plan.process().provider_argv
        );
        let warnings = mcp_warnings(&plan);
        let expected: &[&str] = if mcps.is_some() {
            &[
                "opencode takes no per-launch MCP servers; the profile's mcps list is not applied and the agent loads its MCP servers as usual",
            ]
        } else {
            &[]
        };
        assert_eq!(warnings, expected);
    }
}

/// Antigravity takes RimZ's server from the plugin's entry, so every launch that serves sets
/// `RIMZ_MCP_SERVE` and `RIMZ_MCP_CWD`, a fresh launch with or without a system prompt and
/// a resume alike, and its agent file carries only the list over the user's servers. A launch
/// that serves nothing, on any provider, removes and unsets both even when its caller's
/// environment carries them.
#[test]
fn antigravity_serves_through_the_launch_env_and_its_agent_file_lists_only_user_servers() {
    let mut room = Room::new();
    room.ambient
        .insert("RIMZ_MCP_SERVE".into(), "rimz-notes:always".into());
    room.ambient.insert("RIMZ_MCP_CWD".into(), "/caller".into());
    let prompted = |mut request: ExecRequest| {
        request.system_prompt_file = Some(crate::config::PromptSource::Text {
            origin: room.project.path().join("antigravity.md"),
            text: "Base.\n".to_owned(),
        });
        request
    };
    let mcp_lines = |plan: &LaunchPlan| {
        let file = plan
            .agent_file
            .as_ref()
            .expect("a fresh launch plans a file");
        let text = String::from_utf8(file.bytes.clone()).unwrap();
        let lines: Vec<String> = text
            .lines()
            .filter(|line| line.starts_with("inheritMcp: ") || line.starts_with("mcpServers: "))
            .map(str::to_owned)
            .collect();
        lines
    };
    let no_mcp_argv = |plan: &LaunchPlan| {
        let argv = &plan.process().provider_argv;
        assert!(
            !argv
                .iter()
                .enumerate()
                .any(|(i, arg)| arg.contains("mcp") && !(i > 0 && argv[i - 1] == "--settings")),
            "{argv:?}"
        );
        for value in flag_values(argv, "--settings") {
            let doc: serde_json::Value = serde_json::from_str(value).unwrap();
            assert!(doc.get("mcpServers").is_none(), "{doc}");
            assert!(doc.get("allowedMcpServers").is_none(), "{doc}");
        }
    };
    let cwd = room.project.path().display().to_string();
    let serves = |plan: &LaunchPlan, value: &str| {
        let process = plan.process();
        assert_eq!(
            process.env.get("RIMZ_MCP_SERVE").map(String::as_str),
            Some(value)
        );
        assert_eq!(process.env.get("RIMZ_MCP_CWD"), Some(&cwd));
        assert!(!process.unset.contains("RIMZ_MCP_SERVE"));
        assert!(!process.unset.contains("RIMZ_MCP_CWD"));
    };
    let serves_nothing = |plan: &LaunchPlan| {
        let process = plan.process();
        for key in ["RIMZ_MCP_SERVE", "RIMZ_MCP_CWD"] {
            assert!(!process.env.contains_key(key), "{key}: {:?}", process.env);
            assert!(process.unset.contains(key), "{key}: {:?}", process.unset);
        }
    };
    let all = "rimz-lsp,rimz-wait,rimz-subagents,rimz-agents,rimz-teams,rimz-workflow,rimz-loop,rimz-message,rimz-control,rimz-notes";

    let limited = prompted(launch("antigravity", Some(&["rimz-lsp", "github"]), &[]));
    let plan = room.compile(&switches(true, true), &limited);
    assert_eq!(
        mcp_lines(&plan),
        ["inheritMcp: false", r#"mcpServers: [{"name": "github"}]"#]
    );
    serves(&plan, "rimz-lsp");
    assert_eq!(
        plan.mcp_capabilities,
        [crate::mcp::catalog::Capability::Lsp]
    );
    assert_eq!(mcp_warnings(&plan), Vec::<String>::new());
    no_mcp_argv(&plan);

    let plan = room.compile(
        &switches(true, true),
        &prompted(launch("antigravity", None, &[])),
    );
    assert_eq!(mcp_lines(&plan), Vec::<String>::new());
    serves(&plan, all);
    assert_eq!(
        plan.mcp_capabilities,
        crate::mcp::catalog::Capability::ALL.to_vec()
    );

    let plan = room.compile(&switches(true, false), &limited);
    assert_eq!(
        mcp_lines(&plan),
        ["inheritMcp: false", r#"mcpServers: [{"name": "github"}]"#]
    );
    assert!(plan.mcp_capabilities.is_empty());
    serves_nothing(&plan);

    // A capability's own mark wins over the `rimz` mark, which every other served capability
    // takes. A marked user server warns and keeps its name-only item.
    let marked = prompted(launch(
        "antigravity",
        Some(&["rimz:deferred", "rimz-lsp:always", "github:always"]),
        &[],
    ));
    let marks = "rimz-lsp:always,rimz-wait:deferred,rimz-subagents:deferred,rimz-agents:deferred,\
                 rimz-teams:deferred,rimz-workflow:deferred,rimz-loop:deferred,rimz-message:deferred,rimz-control:deferred,\
                 rimz-notes:deferred";
    let plan = room.compile(&switches(true, true), &marked);
    assert_eq!(
        mcp_lines(&plan),
        ["inheritMcp: false", r#"mcpServers: [{"name": "github"}]"#]
    );
    serves(&plan, marks);
    let github = "the mcps entry `github:always` is not applied: Antigravity takes a load mode only for RimZ's capabilities";
    assert_eq!(mcp_warnings(&plan), [github]);
    assert_eq!(plan.process().mcp_warnings, [github]);
    let plan = room.compile(&switches(true, false), &marked);
    assert_eq!(
        mcp_warnings(&plan),
        [
            "the mcps entry `rimz:deferred` is not applied: RimZ's MCP server is off for this launch",
            "the mcps entry `rimz-lsp:always` is not applied: RimZ's MCP server is off for this launch",
            github,
        ]
    );
    serves_nothing(&plan);

    let mut resumed = marked.clone();
    resumed.action = ExecAction::Resume {
        session_id: "conversation".to_owned(),
        extra_args: Vec::new(),
    };
    let plan = room.compile(&switches(true, true), &resumed);
    assert_eq!(plan.agent_file, None);
    serves(&plan, marks);
    assert_eq!(
        plan.mcp_capabilities,
        crate::mcp::catalog::Capability::ALL.to_vec()
    );
    no_mcp_argv(&plan);

    for (mcps, value) in [
        (None, all),
        (Some(&["*"][..]), all),
        (
            Some(&["*", "rimz-lsp:always"]),
            "rimz-lsp:always,rimz-wait,rimz-subagents,rimz-agents,rimz-teams,rimz-workflow,rimz-loop,rimz-message,rimz-control,rimz-notes",
        ),
    ] {
        let plan = room.compile(&switches(true, true), &launch("antigravity", mcps, &[]));
        assert_eq!(plan.agent_file, None);
        serves(&plan, value);
        assert_eq!(mcp_warnings(&plan), Vec::<String>::new());
        no_mcp_argv(&plan);
    }

    let plan = room.compile(
        &switches(true, true),
        &launch("antigravity", Some(&[]), &[]),
    );
    serves_nothing(&plan);
    for kind in ["claude", "codex", "opencode"] {
        let plan = room.compile(&switches(true, true), &launch(kind, None, &[]));
        serves_nothing(&plan);
    }
}

/// The plan's MCP warnings, whichever key they name: the launch's own MCP warnings (carried as
/// `LaunchEnv`), the wording warnings, and a kind's refusal of an `mcps` list.
fn mcp_warnings(plan: &LaunchPlan) -> Vec<String> {
    let own = &plan.process().mcp_warnings;
    plan.warnings
        .iter()
        .filter(|warning| match warning {
            LaunchPlanWarning::LaunchEnv(text) => own.contains(text),
            LaunchPlanWarning::McpWording(_) | LaunchPlanWarning::McpsUnsupported { .. } => true,
            _ => false,
        })
        .map(ToString::to_string)
        .collect()
}

/// `*` limits nothing, alone or beside marks: every capability, no
/// `--only`, no allowlist or switch-off, and the `lsp` tool as when absent.
#[test]
fn a_star_list_limits_nothing_on_claude_or_codex() {
    let room = Room::new();
    room.codex_config("config.toml", "[mcp_servers.github]\ncommand = 'gh'\n");
    for mcps in [
        &["*"][..],
        &["*", "rimz:always"],
        &["rimz-lsp:deferred", "*", "github:always"],
    ] {
        let request = launch("claude", Some(mcps), &["--tools", "Bash"]);
        let argv = room
            .compile(&switches(true, true), &request)
            .process()
            .provider_argv
            .clone();
        let (entry, _) = claude_entry(&argv).unwrap();
        let args: Vec<_> = entry["args"].as_array().unwrap().iter().collect();
        assert!(!args.contains(&&serde_json::json!("--only")), "{argv:?}");
        assert_eq!(allowlist(&claude_settings(&argv)), None, "{mcps:?}");
        let reminders = crate::harness::launch_reminders::LaunchReminders {
            mcp_server: Some((PathBuf::from("/bin/rimz"), Vec::new())),
            ..Default::default()
        };
        assert!(
            reminders
                .mcp_capabilities(&request)
                .contains(&crate::mcp::catalog::Capability::Lsp),
            "{mcps:?}"
        );

        let argv = room
            .compile(&switches(true, true), &launch("codex", Some(mcps), &[]))
            .process()
            .provider_argv
            .clone();
        assert_eq!(codex_disabled(&argv), Vec::<String>::new(), "{mcps:?}");
        let args = codex_entry(&argv).unwrap()["args"].clone();
        assert!(
            !args.as_array().unwrap().contains(&"--only".into()),
            "{args}"
        );
    }
}

/// Claude: a mark on `rimz` sets the server's `alwaysLoad` unless a
/// capability's own `always` would be overridden by it; a capability's own
/// mark becomes a `serve` flag.
#[test]
fn claude_marks_become_always_load_and_serve_flags() {
    let room = Room::new();
    let compile = |mcps: &[&str]| {
        let plan = room.compile(&switches(true, true), &launch("claude", Some(mcps), &[]));
        let (entry, _) = claude_entry(&plan.process().provider_argv).unwrap();
        (entry, mcp_warnings(&plan))
    };
    for (mcps, always_load, args) in [
        (&["rimz:always"][..], Some(true), &["mcp", "serve"][..]),
        (&["rimz:deferred"], Some(false), &["mcp", "serve"]),
        (
            &["rimz:deferred", "rimz-lsp:always"],
            None,
            &["mcp", "serve", "--always", "rimz-lsp"],
        ),
        (
            &["rimz:always", "rimz-lsp:deferred"],
            Some(true),
            &["mcp", "serve", "--deferred", "rimz-lsp"],
        ),
        (
            &["*", "rimz-wait:deferred", "rimz-notes:always"],
            None,
            &[
                "mcp",
                "serve",
                "--deferred",
                "rimz-wait",
                "--always",
                "rimz-notes",
            ],
        ),
        (
            &["rimz-lsp:always", "rimz-wait"],
            None,
            &[
                "mcp",
                "serve",
                "--only",
                "rimz-lsp",
                "--only",
                "rimz-wait",
                "--always",
                "rimz-lsp",
            ],
        ),
    ] {
        let (entry, warnings) = compile(mcps);
        assert_eq!(
            entry.get("alwaysLoad").and_then(serde_json::Value::as_bool),
            always_load,
            "{mcps:?}"
        );
        assert_eq!(entry["args"], serde_json::json!(args), "{mcps:?}");
        assert_eq!(warnings, Vec::<String>::new(), "{mcps:?}");
    }
}

/// A mark a launch cannot apply warns once, names its entry, and the launch
/// still compiles without it.
#[test]
fn claude_marks_it_cannot_apply_warn_once_each() {
    let room = Room::new();
    let plan = room.compile(
        &switches(true, true),
        &launch(
            "claude",
            Some(&["*", "github:always", "linear:deferred"]),
            &[],
        ),
    );
    let (entry, _) = claude_entry(&plan.process().provider_argv).unwrap();
    assert_eq!(entry.get("alwaysLoad"), None);
    assert_eq!(
        mcp_warnings(&plan),
        [
            "the mcps entry `github:always` is not applied: Claude takes a load mode only for RimZ's server and its capabilities",
            "the mcps entry `linear:deferred` is not applied: Claude takes a load mode only for RimZ's server and its capabilities",
        ]
    );
    let plan = room.compile(
        &switches(true, false),
        &launch(
            "claude",
            Some(&["rimz:always", "rimz-lsp:deferred", "github"]),
            &[],
        ),
    );
    assert_eq!(claude_entry(&plan.process().provider_argv), None);
    assert_eq!(
        mcp_warnings(&plan),
        [
            "the mcps entry `rimz:always` is not applied: RimZ's MCP server is off for this launch",
            "the mcps entry `rimz-lsp:deferred` is not applied: RimZ's MCP server is off for this launch",
        ]
    );
}

/// The `omit_tools_from` override for `server`, when the argv carries one.
fn codex_omit(argv: &[String], server: &str) -> Option<toml::Value> {
    codex_overrides(argv).into_iter().find_map(|table| {
        table
            .get("mcp_servers")?
            .get(server)?
            .get("omit_tools_from")
            .cloned()
    })
}

/// Codex: always omits a server's tools from deferral, deferred restores
/// Codex's default; a user server takes a mark only when the account's config
/// defines it under a name a `-c` key can address.
#[test]
fn codex_marks_set_omit_tools_from_per_server() {
    let room = Room::new();
    room.codex_config(
        "config.toml",
        "[mcp_servers.github]\ncommand = 'gh'\n[mcp_servers.\"my.server\"]\ncommand = 'x'\n\
         [mcp_servers.\"team=lab\"]\ncommand = 't'\n",
    );
    room.codex_config("work.config.toml", "[mcp_servers.chosen]\ncommand = 'c'\n");
    let deferred = toml::Value::Array(Vec::new());
    let always = toml::Value::try_from(["deferred"]).unwrap();
    let compile = |mcps: &[&str], args: &[&str]| {
        let plan = room.compile(&switches(true, true), &launch("codex", Some(mcps), args));
        (plan.process().provider_argv.clone(), mcp_warnings(&plan))
    };
    let (argv, warnings) = compile(&["*", "rimz:always", "github:deferred"], &[]);
    assert_eq!(codex_omit(&argv, "rimz"), Some(always.clone()));
    assert_eq!(codex_omit(&argv, "github"), Some(deferred.clone()));
    assert_eq!(warnings, Vec::<String>::new());
    let (argv, _) = compile(&["rimz:deferred", "chosen:always"], &["--profile", "work"]);
    assert_eq!(codex_omit(&argv, "rimz"), Some(deferred));
    assert_eq!(codex_omit(&argv, "chosen"), Some(always));
    let (argv, _) = compile(&["*"], &[]);
    assert_eq!(codex_omit(&argv, "rimz"), None);

    let (argv, warnings) = compile(
        &[
            "*",
            "rimz-lsp:always",
            "absent:always",
            "my.server:deferred",
            "team=lab:always",
        ],
        &[],
    );
    assert_eq!(codex_omit(&argv, "rimz"), None);
    assert!(
        !argv.iter().any(|arg| arg.starts_with("mcp_servers.team")),
        "{argv:?}"
    );
    assert_eq!(
        warnings,
        [
            "the mcps entry `rimz-lsp:always` is not applied: Codex takes a load mode per server, so rimz-lsp loads as the rimz server does",
            "the mcps entry `absent:always` is not applied: the account's Codex config defines no MCP server `absent`",
            "the mcps entry `my.server:deferred` is not applied: a Codex -c override cannot name a server whose name holds a `.` or `=`",
            "the mcps entry `team=lab:always` is not applied: a Codex -c override cannot name a server whose name holds a `.` or `=`",
        ]
    );
}

#[test]
fn a_kind_without_mcp_servers_takes_a_bare_star_silently() {
    let room = Room::new();
    for (mcps, warns) in [
        (&["*"][..], false),
        (&["*", "*"], false),
        (&["*", "github"], true),
    ] {
        let plan = room.compile(&switches(true, true), &launch("opencode", Some(mcps), &[]));
        assert_eq!(mcp_warnings(&plan).len(), usize::from(warns), "{mcps:?}");
    }
}

/// A launch that registers a capability's tools hides the rimz skill of the
/// same name from the model, on Claude and Codex, with a profile list and
/// without one; a launch that registers nothing hides nothing.
/// Antigravity hides the skills of the capabilities it serves only in the scoped view, on a
/// launch with no agent file and on a resume too: on the host agy has no per-launch skill
/// switch, so the launch hides none and reports none hidden.
#[test]
fn antigravity_hides_the_skills_of_the_capabilities_it_serves_only_when_scoped() {
    let room = Room::new();
    let mut request = launch("antigravity", Some(&["rimz-lsp", "rimz-wait"]), &[]);
    let hidden = |request: &ExecRequest, tools: bool, isolation| {
        room.compile_at(&switches(true, tools), request, isolation)
            .hidden_skills
    };
    assert_eq!(
        hidden(&request, true, Isolation::Scoped),
        ["rimz-lsp", "rimz-wait"]
    );
    assert_eq!(hidden(&request, true, Isolation::Host), Vec::<&str>::new());
    request.action = ExecAction::Resume {
        session_id: "conversation".to_owned(),
        extra_args: Vec::new(),
    };
    assert_eq!(
        hidden(&request, true, Isolation::Scoped),
        ["rimz-lsp", "rimz-wait"]
    );
    assert_eq!(hidden(&request, true, Isolation::Host), Vec::<&str>::new());
    assert_eq!(
        hidden(&request, false, Isolation::Scoped),
        Vec::<&str>::new()
    );
}

#[test]
fn a_child_and_a_judge_are_served_no_capability_that_launches() {
    use crate::mcp::catalog::Capability;

    let room = Room::new();
    let served = |request: &ExecRequest| {
        room.compile(&switches(true, true), request)
            .mcp_capabilities
    };
    let peer = served(&launch("claude", None, &[]));
    for capability in [Capability::Subagents, Capability::Agents, Capability::Teams] {
        assert!(peer.contains(&capability), "{peer:?}");
    }
    let mut child = launch("claude", None, &[]);
    child.subagent = true;
    let mut judge = launch("claude", None, &[]);
    judge.identity.params.workflow = Some(crate::agents::WorkflowAttemptRef {
        run: crate::ids::WorkflowRunId::new(),
        seq: 1,
        role: crate::agents::WorkflowAttemptRole::Judge,
    });
    for request in [child, judge] {
        assert_eq!(
            served(&request),
            [
                Capability::Lsp,
                Capability::Wait,
                Capability::Workflow,
                Capability::Message,
                Capability::Control,
                Capability::Notes
            ]
        );
    }
}

#[test]
fn a_registered_capability_hides_its_skill_from_the_host_view() {
    let room = Room::new();
    let home = Path::new(&room.ambient["HOME"]);
    let names = [
        "commit",
        "rimz-agents",
        "rimz-control",
        "rimz-lsp",
        "rimz-notes",
        "rimz-subagents",
        "rimz-teams",
        "rimz-wait",
    ];
    for root in [home.join(".claude/skills"), home.join(".agents/skills")] {
        for name in names {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(
                root.join(name).join("SKILL.md"),
                format!("---\nname: {name}\ndescription: {name}\n---\n{name}\n"),
            )
            .unwrap();
        }
    }
    let all = [
        "rimz-agents",
        "rimz-control",
        "rimz-lsp",
        "rimz-notes",
        "rimz-subagents",
        "rimz-teams",
        "rimz-wait",
    ];
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for kind in ["claude", "codex"] {
        for listed in [false, true] {
            for (server, mcps, hidden) in [
                (true, None, &all[..]),
                (
                    true,
                    Some(&["rimz-wait", "rimz-lsp", "github"][..]),
                    &["rimz-lsp", "rimz-wait"][..],
                ),
                (true, Some(&[][..]), &[][..]),
                (false, None, &[][..]),
            ] {
                let mut request = launch(kind, mcps, &[]);
                request.skills = listed.then(|| names.map(|name| name.parse().unwrap()).to_vec());
                let plan = room.compile(&switches(true, server), &request);
                let unlisted = match &plan.process().host_skills {
                    Some(crate::agents::skills::HostSkillPlan::Applied { unlisted, .. }) => {
                        serde_json::to_value(unlisted).unwrap()
                    }
                    _ => serde_json::json!([]),
                };
                actual.push((
                    format!("{kind} listed={listed} {mcps:?} server={server}"),
                    unlisted,
                ));
                expected.push((
                    format!("{kind} listed={listed} {mcps:?} server={server}"),
                    serde_json::json!(hidden),
                ));
            }
        }
    }
    for listed in [false, true] {
        for off in [
            ["-c", "mcp_servers.rimz.enabled=false"],
            ["--config", "mcp_servers={rimz={enabled=false}}"],
        ] {
            let mut request = launch("codex", None, &off);
            request.skills = listed.then(|| names.map(|name| name.parse().unwrap()).to_vec());
            let plan = room.compile(&switches(true, true), &request);
            let unlisted = match &plan.process().host_skills {
                Some(crate::agents::skills::HostSkillPlan::Applied { unlisted, .. }) => {
                    serde_json::to_value(unlisted).unwrap()
                }
                _ => serde_json::json!([]),
            };
            actual.push((format!("codex listed={listed} {off:?}"), unlisted));
            expected.push((
                format!("codex listed={listed} {off:?}"),
                serde_json::json!([]),
            ));
        }
    }
    assert_eq!(actual, expected);
}

/// A launch that gets no subagent surface (a profile with `subagents = []`, a child, a judge)
/// is handed a server without `rimz-subagents`, and its plan, launch env, and withheld skills
/// agree with what the server serves.
#[test]
fn a_launch_allowed_no_subagents_is_served_no_subagent_tool_and_withheld_its_skill() {
    use crate::mcp::catalog::Capability;
    let room = Room::new();
    let home = Path::new(&room.ambient["HOME"]);
    let names = ["rimz-lsp", "rimz-subagents"];
    for name in names {
        let root = home.join(".claude/skills").join(name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name}\n---\n{name}\n"),
        )
        .unwrap();
    }
    let mut machine: crate::config::MachineConfig = toml::from_str(&switches(true, true)).unwrap();
    machine.agents.profiles = toml::from_str(
        "[mute]\nagent = 'claude'\nsubagents = []\n[agy-mute]\nagent = 'antigravity'\nsubagents = []\n",
    )
    .unwrap();
    // `subagents = []` limits a launch's children only, so it keeps the agent and team tools;
    // a child and a judge lose every capability that launches agents, and `rimz-loop` too.
    let served = |withheld: &[Capability]| -> Vec<Capability> {
        Capability::ALL
            .into_iter()
            .filter(|capability| !withheld.contains(capability))
            .collect()
    };
    let launchers = [
        Capability::Subagents,
        Capability::Agents,
        Capability::Teams,
        Capability::Loop,
    ];
    let mute = |kind: &str, profile: &str| {
        let mut request = launch(kind, None, &[]);
        request.identity.params.profile = Some(profile.to_owned());
        request
    };
    let mut child = launch("claude", None, &[]);
    child.subagent = true;
    let mut judge = launch("claude", None, &[]);
    judge.identity.params.workflow = Some(crate::agents::WorkflowAttemptRef {
        run: crate::ids::WorkflowRunId::new(),
        seq: 1,
        role: crate::agents::WorkflowAttemptRole::Judge,
    });
    for (case, mut request, served) in [
        (
            "profile",
            mute("claude", "mute"),
            served(&[Capability::Subagents]),
        ),
        ("child", child, served(&launchers)),
        ("judge", judge, served(&launchers)),
    ] {
        let only: Vec<_> = served
            .iter()
            .flat_map(|capability| ["--only", capability.name()])
            .collect();
        let plan = room.compile_config(&machine, &request, Isolation::Host);
        let (entry, _) = claude_entry(&plan.process().provider_argv).unwrap();
        let mut args = vec!["mcp", "serve"];
        args.extend(&only);
        assert_eq!(entry["args"], serde_json::json!(args), "{case}");
        assert_eq!(plan.mcp_capabilities, served, "{case}");

        request.skills = Some(names.map(|name| name.parse().unwrap()).to_vec());
        for isolation in [Isolation::Host, Isolation::Scoped] {
            let plan = room.compile_config(&machine, &request, isolation);
            assert!(
                plan.withheld_skills.contains(&"rimz-subagents"),
                "{case} {isolation:?}: {:?}",
                plan.withheld_skills
            );
            assert!(!plan.hidden_skills.contains(&"rimz-subagents"), "{case}");
        }
        let plan = room.compile_config(&machine, &request, Isolation::Host);
        let Some(crate::agents::skills::HostSkillPlan::Applied { unlisted, .. }) =
            &plan.process().host_skills
        else {
            panic!("{case}: {:?}", plan.process().host_skills);
        };
        assert_eq!(
            serde_json::to_value(unlisted).unwrap(),
            serde_json::json!(names),
            "{case}"
        );
    }

    let plan = room.compile_config(&machine, &mute("antigravity", "agy-mute"), Isolation::Host);
    assert_eq!(plan.mcp_capabilities, served(&[Capability::Subagents]));
    assert_eq!(
        plan.process().env.get("RIMZ_MCP_SERVE").map(String::as_str),
        Some(
            "rimz-lsp,rimz-wait,rimz-agents,rimz-teams,rimz-workflow,rimz-loop,rimz-message,rimz-control,\
             rimz-notes"
        )
    );
}

/// Every Antigravity launch carries the routine `command(<prefix>)` rules while the routine switch
/// is on and adds `mcp(rimz_tools/*)` exactly when the launch also serves RimZ's MCP server,
/// whether or not the profile has an `allowed-tools` list, and carries neither when the switch is
/// off.
#[test]
fn an_antigravity_allow_list_carries_the_served_tools_while_routine_is_on() {
    let room = Room::new();
    let routine_rules: Vec<String> = crate::agents::routine_rimz_prefixes()
        .iter()
        .map(|prefix| format!("command({prefix})"))
        .collect();
    let room_rules = [
        antigravity_write_rule(&room.state.root),
        antigravity_write_rule(&room.runtime.root),
        antigravity_write_rule(&room.runtime.shared_root),
    ];
    let expected = |prefix: &[&str], routine: bool, mcp_served: bool| {
        let mut rules: Vec<String> = prefix.iter().map(|rule| (*rule).to_owned()).collect();
        if routine {
            rules.extend(routine_rules.iter().cloned());
        }
        rules.extend(room_rules.iter().cloned());
        if routine && mcp_served && !rules.iter().any(|rule| rule == "mcp(rimz_tools/*)") {
            rules.push("mcp(rimz_tools/*)".to_owned());
        }
        Some(serde_json::json!(rules))
    };
    let allow = |routine: bool, tools: bool, mcps: Option<&[&str]>, rules: Option<&[&str]>| {
        let mut request = launch("antigravity", mcps, &[]);
        request.allowed_tools =
            rules.map(|rules| rules.iter().map(|rule| rule.parse().unwrap()).collect());
        let plan = room.compile(&switches(routine, tools), &request);
        flag_values(&plan.process().provider_argv, "--settings")
            .last()
            .map(|value| serde_json::from_str::<serde_json::Value>(value).unwrap())
            .and_then(|settings| settings.pointer("/permissions/allow").cloned())
    };
    let read = Some(&["Read"][..]);
    assert_eq!(
        allow(true, true, None, read),
        expected(&["read_file(*)"], true, true)
    );
    assert_eq!(
        allow(
            true,
            true,
            Some(&["rimz-lsp", "github"]),
            Some(&["Read", "mcp__github"])
        ),
        expected(&["read_file(*)", "mcp(github/*)"], true, true)
    );
    assert_eq!(
        allow(true, true, None, Some(&["mcp__rimz"])),
        expected(&["mcp(rimz_tools/*)"], true, true)
    );
    assert_eq!(
        allow(false, true, None, read),
        expected(&["read_file(*)"], false, false)
    );
    assert_eq!(allow(false, true, None, None), expected(&[], false, false));
    assert_eq!(
        allow(true, true, Some(&[]), read),
        expected(&["read_file(*)"], true, false)
    );
    assert_eq!(allow(true, false, None, None), expected(&[], true, false));
    assert_eq!(allow(true, true, None, None), expected(&[], true, true));
    assert_eq!(
        allow(true, true, None, Some(&["Glob"])),
        expected(&[], true, true)
    );
    assert_eq!(
        allow(true, true, None, Some(&[])),
        expected(&[], true, true)
    );
}

/// A `capabilities:` grant serves exactly its capabilities, each with its mark, and `mcps` keeps
/// only the user's own servers: absent it limits none, listed it keeps RimZ's server beside them.
/// A grant of no capability starts no server, and a kind without MCP servers takes a grant
/// silently.
#[test]
fn a_grant_serves_its_capabilities_and_mcps_keeps_the_users_servers() {
    let room = Room::new();
    let compile = |kind: &str, grant: &[&str], mcps: Option<&[&str]>| {
        let mut request = launch(kind, mcps, &[]);
        request.rimz_capabilities =
            Some(grant.iter().map(|entry| entry.parse().unwrap()).collect());
        room.compile(&switches(true, true), &request)
    };
    let granted_args = serde_json::json!([
        "mcp",
        "serve",
        "--only",
        "rimz-lsp",
        "--only",
        "rimz-wait",
        "--always",
        "rimz-lsp"
    ]);
    let grant = ["rimz-lsp:always", "rimz-wait"];
    let mut actual = Vec::new();
    for mcps in [None, Some(&["github"][..]), Some(&[][..])] {
        let plan = compile("claude", &grant, mcps);
        let argv = &plan.process().provider_argv;
        actual.push((
            claude_entry(argv).map(|(entry, _)| entry["args"].clone()),
            allowlist(&claude_settings(argv)),
            mcp_warnings(&plan),
        ));
    }
    assert_eq!(
        actual,
        [
            (Some(granted_args.clone()), None, Vec::new()),
            (
                Some(granted_args.clone()),
                Some(vec!["rimz".to_owned(), "github".to_owned()]),
                Vec::new()
            ),
            (
                Some(granted_args),
                Some(vec!["rimz".to_owned()]),
                Vec::new()
            ),
        ]
    );
    let none_granted = compile("claude", &[], None);
    assert_eq!(claude_entry(&none_granted.process().provider_argv), None);
    assert_eq!(
        allowlist(&claude_settings(&none_granted.process().provider_argv)),
        None
    );
    let plan = compile("opencode", &grant, None);
    assert_eq!(mcp_warnings(&plan), Vec::<String>::new());
}

/// A grant of every capability through `rimz` hands the provider `rimz`'s mark as an
/// `mcps: [rimz:<mark>]` list does: Codex's server-level `omit_tools_from` with no warning, and
/// Claude's server `alwaysLoad`. With the server off, the warning names `capabilities`.
#[test]
fn a_rimz_grant_marks_the_server_as_an_mcps_rimz_entry_does() {
    let room = Room::new();
    let mut actual = Vec::new();
    for mark in ["always", "deferred"] {
        let wildcard = format!("rimz:{mark}");
        let mut granted = launch("codex", None, &[]);
        granted.rimz_capabilities = Some(
            crate::mcp::catalog::Capability::ALL
                .iter()
                .map(|capability| capability.name())
                .chain([wildcard.as_str()])
                .map(|entry| entry.parse().unwrap())
                .collect(),
        );
        let listed = launch("codex", Some(&[wildcard.as_str()]), &[]);
        for kind in ["codex", "claude"] {
            let [granted, listed] = [&granted, &listed].map(|request| {
                let mut request = request.clone();
                request.kind = crate::ids::AgentKind::new_unchecked(kind);
                let plan = room.compile(&switches(true, true), &request);
                let argv = &plan.process().provider_argv;
                (
                    codex_omit(argv, "rimz"),
                    claude_entry(argv).map(|(entry, _)| entry["alwaysLoad"].clone()),
                    mcp_warnings(&plan),
                )
            });
            assert_eq!(granted, listed, "{kind} {mark}");
            actual.push(granted);
        }
    }
    let omit = |mode: &str| {
        Some(toml::Value::Array(
            (mode == "always")
                .then(|| toml::Value::String("deferred".to_owned()))
                .into_iter()
                .collect(),
        ))
    };
    assert_eq!(
        actual,
        [
            (omit("always"), None, Vec::new()),
            (None, Some(serde_json::json!(true)), Vec::new()),
            (omit("deferred"), None, Vec::new()),
            (None, Some(serde_json::json!(false)), Vec::new()),
        ]
    );
    let mut granted = launch("claude", None, &[]);
    granted.rimz_capabilities = Some(
        crate::mcp::catalog::Capability::ALL
            .iter()
            .map(|capability| capability.name())
            .chain(["rimz:deferred"])
            .map(|entry| entry.parse().unwrap())
            .collect(),
    );
    assert_eq!(
        mcp_warnings(&room.compile(&switches(true, false), &granted)),
        [
            "the capabilities entry `rimz:deferred` is not applied: RimZ's MCP server is off for \
             this launch"
        ]
    );
}

/// A prompt's `rimz(x)` reference reaches only what the grant names: a capability the merge base
/// would serve warns as unreachable once the grant leaves it out.
#[test]
fn a_grant_leaves_a_prompt_reference_outside_it_unreachable() {
    let room = Room::new();
    let unreachable = |grant: Option<&[&str]>| {
        let mut request = launch("claude", None, &[]);
        request.rimz_capabilities =
            grant.map(|grant| grant.iter().map(|entry| entry.parse().unwrap()).collect());
        request.system_prompt_file = Some(crate::config::PromptSource::Text {
            origin: room.project.path().join("granted.md"),
            text: "Read with rimz(lsp); wait with rimz(wait).".to_owned(),
        });
        room.compile(&switches(true, true), &request)
            .warnings
            .iter()
            .filter(|warning| matches!(warning, LaunchPlanWarning::UnreachableCapability(_)))
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    };
    assert_eq!(unreachable(None), Vec::<String>::new());
    assert_eq!(
        unreachable(Some(&["rimz-lsp"])),
        [
            "the system prompt names rimz(wait), which this launch reaches neither as a tool of \
          RimZ's MCP server nor as a callable skill; the agent starts without it"
        ]
    );
}

#[test]
fn prompt_rewrite_resolves_capabilities_and_skill_shorthand_by_reach() {
    let room = Room::new();
    let skills_root = Path::new(&room.ambient["CLAUDE_CONFIG_DIR"]).join("skills");
    for name in ["rimz-lsp", "rimz-wait", "rimz-reflect"] {
        let dir = skills_root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "---\nname: test\n---\nBody.\n").unwrap();
    }
    let body = "Tools: rimz(lsp), rimz(subagents), rimz(agents), rimz(teams), rimz(loop), \
                rimz(control), rimz(reflect), rimz(nope). Author: skill(rimz-lsp), \
                skill(rimz-notes), skill(other), mcp(custom), Skill(rimz-lsp), \
                Agent(explorer), subagent(explorer).";
    let mut request = launch("claude", None, &[]);
    request.system_prompt_file = Some(crate::config::PromptSource::Text {
        origin: room.project.path().join("prompt.md"),
        text: body.to_owned(),
    });

    let served = room.compile(&switches(true, true), &request);
    assert_eq!(
        served.prompt.composed.as_deref(),
        Some(
            "Tools: mcp(lsp), mcp(subagent), mcp(agent), mcp(team), mcp(schedule), \
             mcp(self_stop), mcp(self_compact), mcp(self_tune), mcp(self_molt), \
             skill(rimz-reflect), rimz(nope). Author: mcp(lsp), mcp(notes), skill(other), \
             mcp(custom), Skill(rimz-lsp), Agent(explorer), subagent(explorer)."
        )
    );
    let unreachable = |plan: &LaunchPlan| -> Vec<String> {
        plan.warnings
            .iter()
            .filter_map(|warning| match warning {
                LaunchPlanWarning::UnreachableCapability(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    };
    assert_eq!(unreachable(&served), ["nope"]);

    let skills_only = room.compile(&switches(true, false), &request);
    assert_eq!(
        skills_only.prompt.composed.as_deref(),
        Some(
            "Tools: skill(rimz-lsp), rimz(subagents), rimz(agents), rimz(teams), rimz(loop), \
             rimz(control), skill(rimz-reflect), rimz(nope). Author: skill(rimz-lsp), \
             skill(rimz-notes), skill(other), mcp(custom), Skill(rimz-lsp), Agent(explorer), \
             subagent(explorer)."
        )
    );
    assert_eq!(
        unreachable(&skills_only),
        [
            "agents",
            "control",
            "loop",
            "nope",
            "notes",
            "subagents",
            "teams"
        ]
    );
}

#[test]
fn prompt_rewrite_reaches_every_prompt_channel_and_keeps_unmodified_author_files() {
    let room = Room::new();
    let home = Path::new(&room.ambient["HOME"]);
    let skill_dir = home.join(".rimz/skills/rimz-lsp");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: rimz-lsp\n---\nBody.\n",
    )
    .unwrap();

    // Lone-file base on a path kind: rewritten text gets an artifact; untouched text keeps the
    // author's own file path.
    let rewritten_file = room.project.path().join("rewritten.md");
    std::fs::write(&rewritten_file, "Use rimz(lsp) here.\n").unwrap();
    let mut claude = launch("claude", None, &[]);
    claude.system_prompt_file = Some(rewritten_file.clone().into());
    let plan = room.compile(&switches(true, true), &claude);
    let artifact = plan
        .prompt
        .artifact
        .as_ref()
        .expect("rewritten lone file plans an artifact");
    assert_ne!(artifact, &rewritten_file);
    assert_eq!(
        flag_values(&plan.process().provider_argv, "--system-prompt-file"),
        [artifact.to_string_lossy().as_ref()]
    );
    prompt_compose::apply_system_prompt(&plan.prompt).unwrap();
    assert_eq!(
        std::fs::read_to_string(artifact).unwrap(),
        "Use mcp(lsp) here.\n"
    );

    let plain_file = room.project.path().join("plain.md");
    std::fs::write(&plain_file, "No shorthand here.\n").unwrap();
    claude.system_prompt_file = Some(plain_file.clone().into());
    let plain_plan = room.compile(&switches(true, true), &claude);
    assert_eq!(plain_plan.prompt.artifact, None);
    assert_eq!(
        flag_values(&plain_plan.process().provider_argv, "--system-prompt-file"),
        [plain_file.to_string_lossy().as_ref()]
    );

    let skill_file = room.project.path().join("skill-ref.md");
    std::fs::write(&skill_file, "Use skill(rimz-lsp) here.\n").unwrap();
    claude.system_prompt_file = Some(skill_file.clone().into());
    let unchanged_skill = room.compile(&switches(true, false), &claude);
    assert_eq!(unchanged_skill.prompt.artifact, None);
    assert_eq!(
        unchanged_skill.prompt.composed.as_deref(),
        Some("Use skill(rimz-lsp) here.\n")
    );
    assert!(
        !unchanged_skill
            .warnings
            .iter()
            .any(|warning| matches!(warning, LaunchPlanWarning::UnreachableCapability(_)))
    );

    let unreachable_skill_file = room.project.path().join("unreachable-skill-ref.md");
    std::fs::write(&unreachable_skill_file, "Use skill(rimz-notes) here.\n").unwrap();
    claude.system_prompt_file = Some(unreachable_skill_file.clone().into());
    let unreachable_skill = room.compile(&switches(true, false), &claude);
    assert_eq!(unreachable_skill.prompt.artifact, None);
    assert_eq!(
        unreachable_skill.prompt.composed.as_deref(),
        Some("Use skill(rimz-notes) here.\n")
    );
    assert!(
        unreachable_skill
            .warnings
            .iter()
            .any(|warning| matches!(warning, LaunchPlanWarning::UnreachableCapability(name) if name == "notes"))
    );

    // TextFlag kind (Pi): argv carries the rewritten text.
    let mut pi = launch("pi", None, &[]);
    pi.system_prompt_file = Some(rewritten_file.clone().into());
    let pi_plan = room.compile(&switches(true, false), &pi);
    assert_eq!(
        flag_values(&pi_plan.process().provider_argv, "--system-prompt"),
        ["Use skill(rimz-lsp) here.\n"]
    );

    // AgentFile kind (Antigravity): the generated agent file carries the rewritten text.
    let mut agy = launch("antigravity", None, &[]);
    agy.system_prompt_file = Some(rewritten_file.clone().into());
    let agy_plan = room.compile(&switches(true, true), &agy);
    let agent_file = agy_plan.agent_file.as_ref().expect("agent file");
    let contents = String::from_utf8_lossy(&agent_file.bytes);
    assert!(contents.contains("Use mcp(lsp) here."), "{contents}");
    assert!(!contents.contains("rimz(lsp)"), "{contents}");

    // EnvConfigMember kind (OpenCode): artifact carries the rewritten text.
    let mut opencode = launch("opencode", None, &[]);
    opencode.system_prompt_file = Some(rewritten_file.into());
    let opencode_plan = room.compile(&switches(true, false), &opencode);
    assert_eq!(
        opencode_plan.prompt.composed.as_deref(),
        Some("Use skill(rimz-lsp) here.\n")
    );
    assert!(opencode_plan.prompt.artifact.is_some());
}

#[test]
fn system_prompt_digest_tracks_the_rewritten_text_across_reaches_and_replays() {
    let room = Room::new();
    let skills_root = Path::new(&room.ambient["CLAUDE_CONFIG_DIR"]).join("skills");
    let dir = skills_root.join("rimz-lsp");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: rimz-lsp\n---\nBody.\n").unwrap();

    let prompt_file = room.project.path().join("digest.md");
    std::fs::write(&prompt_file, "Query with rimz(lsp).\n").unwrap();
    let mut req = launch("claude", None, &[]);
    req.system_prompt_file = Some(prompt_file.clone().into());

    let tool_a = room.compile(&switches(true, true), &req);
    let tool_b = room.compile(&switches(true, true), &req);
    let skill = room.compile(&switches(true, false), &req);
    assert_eq!(tool_a.system_prompt_digest, tool_b.system_prompt_digest);
    assert_ne!(tool_a.system_prompt_digest, skill.system_prompt_digest);

    for action in [
        ExecAction::Resume {
            session_id: "sess-1".into(),
            extra_args: Vec::new(),
        },
        ExecAction::Fork {
            session_id: "sess-1".into(),
            extra_args: Vec::new(),
        },
    ] {
        let mut replay = request("claude", action);
        replay.system_prompt_file = Some(prompt_file.clone().into());
        let plan = room.compile(&switches(true, true), &replay);
        assert_eq!(
            plan.prompt.composed.as_deref(),
            Some("Query with mcp(lsp).\n")
        );
        assert_eq!(plan.system_prompt_digest, tool_a.system_prompt_digest);
    }

    let plain_file = room.project.path().join("plain-digest.md");
    std::fs::write(&plain_file, "Plain text.\n").unwrap();
    req.system_prompt_file = Some(plain_file.into());
    assert_eq!(
        room.compile(&switches(true, true), &req)
            .system_prompt_digest,
        room.compile(&switches(true, false), &req)
            .system_prompt_digest
    );
}

#[test]
fn seat_added_rimz_message_is_settled_at_launch() {
    let room = Room::new();
    let home = Path::new(&room.ambient["HOME"]);
    let agents_home = home.join(".rimz");
    std::fs::create_dir_all(agents_home.join("agents")).unwrap();
    std::fs::create_dir_all(agents_home.join("teams")).unwrap();
    for kind in ["claude", "codex"] {
        std::fs::write(
            agents_home.join(format!("agents/{kind}.md")),
            "---\ndescription: Base.\n---\nBase.",
        )
        .unwrap();
    }
    std::fs::write(
        agents_home.join("agents/listed.md"),
        "---\ndescription: Listed.\nagent: claude\ntools: [Bash, Skill]\nskills: [work]\n---\nCraft.",
    )
    .unwrap();
    std::fs::write(
        agents_home.join("agents/empty.md"),
        "---\ndescription: Empty.\nagent: claude\ntools: [Bash, Skill]\nskills: []\n---\nCraft.",
    )
    .unwrap();
    std::fs::write(
        agents_home.join("teams/probe.md"),
        "---\nleader: judge\nstages: [Review]\nroles:\n  - agent: listed\n    role: judge\n    owns: [Review]\n  - agent: empty\n    role: scribe\n---\nPipeline.",
    )
    .unwrap();
    let write_skill = |path: PathBuf, body: &str| {
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), body).unwrap();
    };
    let lib_skills = agents_home.join("skills");
    write_skill(
        lib_skills.join("work"),
        "---\nname: work\ndescription: Work.\n---\nWork.\n",
    );

    let mut ambient = room.ambient.clone();
    ambient.insert("RIMZ_AGENTS_HOME".into(), agents_home.display().to_string());
    let named_claude = home.join(".claude-work");
    std::fs::create_dir_all(&named_claude).unwrap();
    let machine_toml = format!(
        "[agents]\nallow-routine-rimz = true\nrimz-mcp-server = false\n\
         [accounts.claude.work]\nhome = '{}'\n",
        named_claude.display()
    );
    let base_machine: crate::config::MachineConfig = toml::from_str(&machine_toml).unwrap();

    let load_defs = |check: bool| {
        let skill_check = if check {
            crate::config::definitions::SkillCheck::Check {
                env: &ambient,
                machine_isolation: Isolation::Host,
            }
        } else {
            crate::config::definitions::SkillCheck::Skip
        };
        let loaded = crate::config::definitions::load(
            &agents_home,
            skill_check,
            &base_machine.agents.commands,
            &agents_home.join("config.toml"),
            &base_machine.tiers,
            base_machine.harness.auto_compact,
        );
        assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
        loaded
    };
    let seat_cell = |loaded: &crate::config::definitions::LoadedDefinitions,
                     spec: &str,
                     agent_override: Option<&str>| {
        let layout = crate::harness::spec::resolve_spec_with_agent_override(
            Some(spec),
            &loaded.agent_profiles,
            &loaded.agent_profiles,
            &base_machine.agents.commands,
            &loaded.teams,
            agent_override,
        )
        .unwrap();
        layout.agent_cells().next().unwrap().clone()
    };
    let try_compile = |loaded: &crate::config::definitions::LoadedDefinitions,
                       req: &ExecRequest,
                       isolation: Isolation| {
        let mut machine = base_machine.clone();
        machine.agents.profiles = loaded.agent_profiles.clone();
        machine.agents.teams = loaded.teams.clone();
        let effective = crate::config::effective::load_with_roots(
            &machine,
            room.project.path(),
            room.project.path(),
        )
        .unwrap();
        compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: req,
            cwd: room.project.path(),
            project_root: room.project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &room.runtime,
            state: &room.state,
            effective: Some(&effective),
            machine: &machine,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation,
            bwrap: isolation.is_scoped().then_some(Path::new("/usr/bin/bwrap")),
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
            hibernate: false,
        })
    };
    let launch_skill_names = |plan: &LaunchPlan| {
        plan.request
            .launch_skills(&plan.withheld_skills)
            .map(|skills| {
                skills
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
    };

    // 1. Load with SkillCheck::Skip (scoped pane), no rimz-message on disk: Claude seats with
    //    `skills: [work]` and `skills: []` compile under Host and Scoped, dropping rimz-message.
    let skipped = load_defs(false);
    for (spec, expected) in [("probe.judge", vec!["work"]), ("probe.scribe", Vec::new())] {
        let cell = seat_cell(&skipped, spec, None);
        let req = ExecRequest::fresh(&cell, ExecIdentity::default(), None, false);
        for isolation in [Isolation::Host, Isolation::Scoped] {
            let plan = try_compile(&skipped, &req, isolation)
                .unwrap_or_else(|err| panic!("{spec} {isolation:?}: {err}"));
            assert_eq!(
                launch_skill_names(&plan),
                Some(expected.iter().map(|s| (*s).to_owned()).collect()),
                "{spec} {isolation:?}"
            );
            assert!(
                !plan
                    .rimz_reach
                    .iter()
                    .any(|(name, _)| name == "rimz-message"),
                "{spec} {isolation:?}: {:?}",
                plan.rimz_reach
            );
        }
        // Marker also survives ResumeLaunchPosture and swap::restart_cell.
        let posture = crate::harness::plan::ResumeLaunchPosture::from(&cell);
        let resumed_req = posture.exec_request(
            cell.kind.clone(),
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
        );
        let plan = try_compile(&skipped, &resumed_req, Isolation::Host).unwrap();
        assert_eq!(
            launch_skill_names(&plan),
            Some(expected.iter().map(|s| (*s).to_owned()).collect()),
            "resumed {spec}"
        );
        let agent = crate::agents::AgentState::stub(
            cell.kind.as_str(),
            "seat",
            crate::agents::AgentStatus::Running,
        );
        let crate::harness::spec::Cell::Agent(restarted) = crate::launcher::swap::restart_cell(
            &agent,
            &crate::harness::resume::ResumePosture {
                launch: posture,
                degraded: None,
            },
        ) else {
            panic!("restart cell");
        };
        let restarted_req = ExecRequest::fresh(&restarted, ExecIdentity::default(), None, false);
        let plan = try_compile(&skipped, &restarted_req, Isolation::Host).unwrap();
        assert_eq!(
            launch_skill_names(&plan),
            Some(expected.iter().map(|s| (*s).to_owned()).collect()),
            "restarted {spec}"
        );
    }

    // 2. `--agent codex` rebase where only Claude's root has rimz-message: Codex compiles and
    //    drops rimz-message; when only Codex's root has it, Codex keeps and names it.
    let claude_msg = Path::new(&ambient["CLAUDE_CONFIG_DIR"]).join("skills/rimz-message");
    let codex_msg = home.join(".agents/skills/rimz-message");
    write_skill(
        claude_msg.clone(),
        "---\nname: rimz-message\ndescription: Message.\n---\nMessage.\n",
    );
    let checked = load_defs(true);
    let rebased = seat_cell(&checked, "probe.judge", Some("codex"));
    let rebased_req = ExecRequest::fresh(&rebased, ExecIdentity::default(), None, false);
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let plan = try_compile(&checked, &rebased_req, isolation)
            .unwrap_or_else(|err| panic!("codex rebase missing {isolation:?}: {err}"));
        assert_eq!(
            launch_skill_names(&plan),
            Some(vec!["work".to_owned()]),
            "codex rebase missing {isolation:?}"
        );
        assert!(
            !plan
                .rimz_reach
                .iter()
                .any(|(name, _)| name == "rimz-message"),
            "codex rebase missing {isolation:?}: {:?}",
            plan.rimz_reach
        );
    }
    std::fs::remove_dir_all(&claude_msg).unwrap();
    write_skill(
        codex_msg.clone(),
        "---\nname: rimz-message\ndescription: Message.\n---\nMessage.\n",
    );
    let checked = load_defs(true);
    let rebased = seat_cell(&checked, "probe.judge", Some("codex"));
    let rebased_req = ExecRequest::fresh(&rebased, ExecIdentity::default(), None, false);
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let plan = try_compile(&checked, &rebased_req, isolation)
            .unwrap_or_else(|err| panic!("codex rebase present {isolation:?}: {err}"));
        assert_eq!(
            launch_skill_names(&plan),
            Some(vec!["work".to_owned(), "rimz-message".to_owned()]),
            "codex rebase present {isolation:?}"
        );
        assert!(
            plan.rimz_reach
                .contains(&("rimz-message".to_owned(), "skill(rimz-message)".to_owned())),
            "codex rebase present {isolation:?}: {:?}",
            plan.rimz_reach
        );
        assert!(
            plan.prompt
                .composed
                .as_deref()
                .unwrap()
                .contains("skill(rimz-message)"),
            "codex rebase present {isolation:?}"
        );
    }
    std::fs::remove_dir_all(&codex_msg).unwrap();

    // 3. `--login work` where only the default login's root has rimz-message drops it, and
    //    where only `work`'s root has it lists it.
    let work_msg = named_claude.join("skills/rimz-message");
    write_skill(
        claude_msg.clone(),
        "---\nname: rimz-message\ndescription: Message.\n---\nMessage.\n",
    );
    let checked = load_defs(true);
    let cell = seat_cell(&checked, "probe.judge", None);
    let mut login_req = ExecRequest::fresh(&cell, ExecIdentity::default(), None, false);
    login_req.identity.params.login = Some("work".parse().unwrap());
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let plan = try_compile(&checked, &login_req, isolation)
            .unwrap_or_else(|err| panic!("login work missing {isolation:?}: {err}"));
        assert_eq!(launch_skill_names(&plan), Some(vec!["work".to_owned()]));
        assert!(
            !plan
                .rimz_reach
                .iter()
                .any(|(name, _)| name == "rimz-message")
        );
    }
    std::fs::remove_dir_all(&claude_msg).unwrap();
    write_skill(
        work_msg.clone(),
        "---\nname: rimz-message\ndescription: Message.\n---\nMessage.\n",
    );
    let checked = load_defs(true);
    let cell = seat_cell(&checked, "probe.judge", None);
    let mut login_req = ExecRequest::fresh(&cell, ExecIdentity::default(), None, false);
    login_req.identity.params.login = Some("work".parse().unwrap());
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let plan = try_compile(&checked, &login_req, isolation)
            .unwrap_or_else(|err| panic!("login work present {isolation:?}: {err}"));
        assert_eq!(
            launch_skill_names(&plan),
            Some(vec!["work".to_owned(), "rimz-message".to_owned()])
        );
        assert!(
            plan.rimz_reach
                .contains(&("rimz-message".to_owned(), "skill(rimz-message)".to_owned()))
        );
    }
    std::fs::remove_dir_all(&work_msg).unwrap();

    // 4. User-only copy on disk (loaded under SkillCheck::Skip): dropped silently, never callable
    //    and never named as reachable for Claude or Codex.
    let lib_msg = lib_skills.join("rimz-message");
    write_skill(
        lib_msg.clone(),
        "---\nname: rimz-message\ndescription: Message.\ndisable-model-invocation: true\n---\nMessage.\n",
    );
    std::fs::create_dir_all(lib_msg.join("agents")).unwrap();
    std::fs::write(
        lib_msg.join("agents/openai.yaml"),
        "policy:\n  allow_implicit_invocation: false\n",
    )
    .unwrap();
    let skipped = load_defs(false);
    for (label, agent_override) in [("claude", None), ("codex", Some("codex"))] {
        let cell = seat_cell(&skipped, "probe.judge", agent_override);
        let req = ExecRequest::fresh(&cell, ExecIdentity::default(), None, false);
        for isolation in [Isolation::Host, Isolation::Scoped] {
            let plan = try_compile(&skipped, &req, isolation)
                .unwrap_or_else(|err| panic!("user-only {label} {isolation:?}: {err}"));
            assert_eq!(
                launch_skill_names(&plan),
                Some(vec!["work".to_owned()]),
                "user-only {label} {isolation:?}"
            );
            assert!(
                !plan
                    .rimz_reach
                    .iter()
                    .any(|(name, _)| name == "rimz-message"),
                "user-only {label} {isolation:?}: {:?}",
                plan.rimz_reach
            );
            assert!(
                !plan
                    .prompt
                    .composed
                    .as_deref()
                    .unwrap()
                    .contains("skill(rimz-message)"),
                "user-only {label} {isolation:?}"
            );
            if isolation == Isolation::Host {
                let Some(crate::agents::skills::HostSkillPlan::Applied {
                    listed, unlisted, ..
                }) = &plan.process().host_skills
                else {
                    panic!(
                        "user-only {label} host plan: {:?}",
                        plan.process().host_skills
                    );
                };
                assert!(
                    !serde_json::to_value(listed)
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!("rimz-message")),
                    "user-only {label} listed: {listed:?}"
                );
                assert!(
                    serde_json::to_value(unlisted)
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!("rimz-message")),
                    "user-only {label} unlisted: {unlisted:?}"
                );
            }
        }
    }

    // 5. Installed callable copy on disk (loaded under SkillCheck::Skip): listed and reachable.
    write_skill(
        lib_msg.clone(),
        "---\nname: rimz-message\ndescription: Message.\n---\nMessage.\n",
    );
    std::fs::remove_file(lib_msg.join("agents/openai.yaml")).unwrap();
    let skipped = load_defs(false);
    let cell = seat_cell(&skipped, "probe.judge", None);
    let req = ExecRequest::fresh(&cell, ExecIdentity::default(), None, false);
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let plan = try_compile(&skipped, &req, isolation)
            .unwrap_or_else(|err| panic!("callable {isolation:?}: {err}"));
        assert_eq!(
            launch_skill_names(&plan),
            Some(vec!["work".to_owned(), "rimz-message".to_owned()]),
            "callable {isolation:?}"
        );
        assert!(
            plan.rimz_reach
                .contains(&("rimz-message".to_owned(), "skill(rimz-message)".to_owned())),
            "callable {isolation:?}: {:?}",
            plan.rimz_reach
        );
        assert!(
            plan.prompt
                .composed
                .as_deref()
                .unwrap()
                .contains("skill(rimz-message)"),
            "callable {isolation:?}"
        );
        if isolation == Isolation::Host {
            let Some(crate::agents::skills::HostSkillPlan::Applied { listed, .. }) =
                &plan.process().host_skills
            else {
                panic!("callable host plan: {:?}", plan.process().host_skills);
            };
            assert!(
                serde_json::to_value(listed)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("rimz-message")),
                "callable listed: {listed:?}"
            );
        }
    }
    std::fs::remove_dir_all(&lib_msg).unwrap();

    // 6. Older wire request (no seat marker) and author-written `skills: [rimz-message]` still
    //    refuse when rimz-message is missing on disk.
    let mut legacy_src = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    legacy_src.skills = Some(vec![
        "work".parse().unwrap(),
        "rimz-message".parse().unwrap(),
    ]);
    let legacy_json = serde_json::to_value(&legacy_src).unwrap();
    assert!(legacy_json.get("seat_consensus_skill").is_none());
    let legacy: ExecRequest = serde_json::from_value(legacy_json).unwrap();
    assert!(!legacy.seat_consensus_skill);
    for isolation in [Isolation::Host, Isolation::Scoped] {
        assert!(
            try_compile(&skipped, &legacy, isolation).is_err(),
            "legacy wire request must refuse missing skill ({isolation:?})"
        );
    }
    std::fs::write(
        agents_home.join("teams/probe.md"),
        "---\nleader: judge\nstages: [Review]\nroles:\n  - agent: listed\n    role: judge\n    owns: [Review]\n    skills: [work, rimz-message]\n---\nPipeline.",
    )
    .unwrap();
    let authored_skipped = load_defs(false);
    let authored_cell = seat_cell(&authored_skipped, "probe.judge", None);
    let authored_req = ExecRequest::fresh(&authored_cell, ExecIdentity::default(), None, false);
    for isolation in [Isolation::Host, Isolation::Scoped] {
        assert!(
            try_compile(&authored_skipped, &authored_req, isolation).is_err(),
            "author-written rimz-message must refuse when missing ({isolation:?})"
        );
    }
}

/// Settling a seat-added `rimz-message` entry and discovering `reminders.rimz_skills` read
/// the same effective environment the two skill enforcers read: login env extended with the
/// trusted project `[[agents]]` env, while a named-login override still wins, an untrusted
/// project's env is never read, and earlier `compile` refusals keep their order.
#[test]
fn seat_added_rimz_message_reads_trusted_project_env() {
    if std::env::var_os("RIMZ_TEST_SEAT_TRUSTED_ENV").is_none() {
        let rimz_home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "harness::launch_plan::tests::mcp::seat_added_rimz_message_reads_trusted_project_env",
                "--nocapture",
            ])
            .env("RIMZ_UNIT_TEST_RIMZ_HOME", rimz_home.path().join(".rimz"))
            .env("RIMZ_TEST_SEAT_TRUSTED_ENV", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let room = Room::new();
    let home = Path::new(&room.ambient["HOME"]);
    let agents_home = home.join(".rimz");
    std::fs::create_dir_all(agents_home.join("agents")).unwrap();
    std::fs::create_dir_all(agents_home.join("teams")).unwrap();
    std::fs::write(
        agents_home.join("agents/claude.md"),
        "---\ndescription: Base.\n---\nBase.",
    )
    .unwrap();
    std::fs::write(
        agents_home.join("agents/listed.md"),
        "---\ndescription: Listed.\nagent: claude\ntools: [Bash, Skill]\nskills: [work]\n---\nCraft.",
    )
    .unwrap();
    std::fs::write(
        agents_home.join("teams/probe.md"),
        "---\nleader: judge\nstages: [Review]\nroles:\n  - agent: listed\n    role: judge\n    owns: [Review]\n---\nPipeline.",
    )
    .unwrap();
    let write_skill = |path: &Path, body: &str| {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("SKILL.md"), body).unwrap();
    };
    write_skill(
        &agents_home.join("skills/work"),
        "---\nname: work\ndescription: Work.\n---\nWork.\n",
    );

    let mut ambient = room.ambient.clone();
    ambient.insert("RIMZ_AGENTS_HOME".into(), agents_home.display().to_string());
    let unmoved_claude = PathBuf::from(&ambient["CLAUDE_CONFIG_DIR"]);
    let moved_claude = home.join(".claude-project");
    let named_claude = home.join(".claude-work");
    std::fs::create_dir_all(&unmoved_claude).unwrap();
    std::fs::create_dir_all(&moved_claude).unwrap();
    std::fs::create_dir_all(&named_claude).unwrap();

    let machine_toml = format!(
        "[agents]\nallow-routine-rimz = true\nrimz-mcp-server = false\n\
         [accounts.claude.work]\nhome = '{}'\n",
        named_claude.display()
    );
    let base_machine: crate::config::MachineConfig = toml::from_str(&machine_toml).unwrap();
    let loaded = crate::config::definitions::load(
        &agents_home,
        crate::config::definitions::SkillCheck::Skip,
        &base_machine.agents.commands,
        &agents_home.join("config.toml"),
        &base_machine.tiers,
        base_machine.harness.auto_compact,
    );
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let layout = crate::harness::spec::resolve_spec_with_agent_override(
        Some("probe.judge"),
        &loaded.agent_profiles,
        &loaded.agent_profiles,
        &base_machine.agents.commands,
        &loaded.teams,
        None,
    )
    .unwrap();
    let cell = layout.agent_cells().next().unwrap().clone();
    assert!(cell.seat_consensus_skill);

    let mut machine = base_machine.clone();
    machine.agents.profiles = loaded.agent_profiles.clone();
    machine.agents.teams = loaded.teams.clone();

    let try_compile = |req: &ExecRequest, isolation: Isolation| {
        let effective = crate::config::effective::load_with_roots(
            &machine,
            room.project.path(),
            room.project.path(),
        )
        .unwrap();
        compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: req,
            cwd: room.project.path(),
            project_root: room.project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &room.runtime,
            state: &room.state,
            effective: Some(&effective),
            machine: &machine,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation,
            bwrap: isolation.is_scoped().then_some(Path::new("/usr/bin/bwrap")),
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
            hibernate: false,
        })
    };
    let launch_skill_names = |plan: &LaunchPlan| {
        plan.request
            .launch_skills(&plan.withheld_skills)
            .map(|skills| {
                skills
                    .into_iter()
                    .map(|skill| skill.to_string())
                    .collect::<Vec<_>>()
            })
    };

    // Configure a trusted project env that moves CLAUDE_CONFIG_DIR.
    std::fs::create_dir_all(room.project.path().join(".rimz")).unwrap();
    std::fs::write(
        room.project.path().join(".rimz/config.toml"),
        format!(
            "[[agents]]\nname = 'claude'\nenv = {{ CLAUDE_CONFIG_DIR = '{}' }}\n",
            moved_claude.display()
        ),
    )
    .unwrap();
    crate::trust::grant_with_roots(room.project.path(), &crate::disk::paths::rimz_home()).unwrap();

    let msg_body = "---\nname: rimz-message\ndescription: Message.\n---\nMessage.\n";
    let moved_msg = moved_claude.join("skills/rimz-message");
    let unmoved_msg = unmoved_claude.join("skills/rimz-message");
    let named_msg = named_claude.join("skills/rimz-message");
    let req = ExecRequest::fresh(&cell, ExecIdentity::default(), None, false);

    // 1. Skill installed only under the moved root: kept, listed, prompt names skill(rimz-message).
    write_skill(&moved_msg, msg_body);
    let mut moved_outcomes = Vec::new();
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let outcome = try_compile(&req, isolation).map(|plan| {
            let skills = launch_skill_names(&plan);
            let reach_has_msg = plan
                .rimz_reach
                .contains(&("rimz-message".to_owned(), "skill(rimz-message)".to_owned()));
            let prompt_names_skill = plan
                .prompt
                .composed
                .as_deref()
                .is_some_and(|text| text.contains("skill(rimz-message)"));
            let host_listed = matches!(
                &plan.process().host_skills,
                Some(crate::agents::skills::HostSkillPlan::Applied { listed, .. })
                    if serde_json::to_value(listed)
                        .unwrap()
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!("rimz-message"))
            );
            (
                isolation,
                skills,
                reach_has_msg,
                prompt_names_skill,
                host_listed,
            )
        });
        moved_outcomes.push(outcome.map_err(|err| format!("{isolation:?}: {err}")));
    }
    assert_eq!(
        moved_outcomes,
        vec![
            Ok((
                Isolation::Host,
                Some(vec!["work".to_owned(), "rimz-message".to_owned()]),
                true,
                true,
                true,
            )),
            Ok((
                Isolation::Scoped,
                Some(vec!["work".to_owned(), "rimz-message".to_owned()]),
                true,
                true,
                false,
            )),
        ]
    );
    std::fs::remove_dir_all(&moved_msg).unwrap();

    // 2. Skill installed only under the unmoved root: dropped, launch compiles cleanly.
    write_skill(&unmoved_msg, msg_body);
    let mut unmoved_outcomes = Vec::new();
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let outcome = try_compile(&req, isolation).map(|plan| {
            let skills = launch_skill_names(&plan);
            let reach_has_msg = plan
                .rimz_reach
                .iter()
                .any(|(name, _)| name == "rimz-message");
            let prompt_names_skill = plan
                .prompt
                .composed
                .as_deref()
                .is_some_and(|text| text.contains("skill(rimz-message)"));
            (isolation, skills, reach_has_msg, prompt_names_skill)
        });
        unmoved_outcomes.push(outcome.map_err(|err| format!("{isolation:?}: {err}")));
    }
    assert_eq!(
        unmoved_outcomes,
        vec![
            Ok((Isolation::Host, Some(vec!["work".to_owned()]), false, false)),
            Ok((
                Isolation::Scoped,
                Some(vec!["work".to_owned()]),
                false,
                false
            )),
        ]
    );
    std::fs::remove_dir_all(&unmoved_msg).unwrap();

    // 3. Named-login override still wins over trusted project env.
    write_skill(&moved_msg, msg_body);
    let mut named_req = req.clone();
    named_req.identity.params.login = Some("work".parse().unwrap());
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let plan = try_compile(&named_req, isolation)
            .unwrap_or_else(|err| panic!("named login over moved root ({isolation:?}): {err}"));
        assert_eq!(
            launch_skill_names(&plan),
            Some(vec!["work".to_owned()]),
            "named login must ignore moved root ({isolation:?})"
        );
        assert!(
            !plan
                .rimz_reach
                .iter()
                .any(|(name, _)| name == "rimz-message"),
            "named login reach ({isolation:?}): {:?}",
            plan.rimz_reach
        );
    }
    std::fs::remove_dir_all(&moved_msg).unwrap();
    write_skill(&named_msg, msg_body);
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let plan = try_compile(&named_req, isolation)
            .unwrap_or_else(|err| panic!("named login own root ({isolation:?}): {err}"));
        assert_eq!(
            launch_skill_names(&plan),
            Some(vec!["work".to_owned(), "rimz-message".to_owned()]),
            "named login own root ({isolation:?})"
        );
        assert!(
            plan.rimz_reach
                .contains(&("rimz-message".to_owned(), "skill(rimz-message)".to_owned())),
            "named login own root reach ({isolation:?}): {:?}",
            plan.rimz_reach
        );
    }
    std::fs::remove_dir_all(&named_msg).unwrap();

    // 4. The project env moves only RIMZ_AGENTS_HOME: the RimZ skill chain follows it.
    let moved_agents = home.join(".rimz-project");
    write_skill(
        &moved_agents.join("skills/work"),
        "---\nname: work\ndescription: Work.\n---\nWork.\n",
    );
    write_skill(&moved_agents.join("skills/rimz-message"), msg_body);
    std::fs::write(
        room.project.path().join(".rimz/config.toml"),
        format!(
            "[[agents]]\nname = 'claude'\nenv = {{ RIMZ_AGENTS_HOME = '{}' }}\n",
            moved_agents.display()
        ),
    )
    .unwrap();
    crate::trust::grant_with_roots(room.project.path(), &crate::disk::paths::rimz_home()).unwrap();
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let plan = try_compile(&req, isolation)
            .unwrap_or_else(|err| panic!("moved agents home ({isolation:?}): {err}"));
        assert_eq!(
            launch_skill_names(&plan),
            Some(vec!["work".to_owned(), "rimz-message".to_owned()]),
            "moved agents home ({isolation:?})"
        );
        assert!(
            plan.rimz_reach
                .contains(&("rimz-message".to_owned(), "skill(rimz-message)".to_owned())),
            "moved agents home reach ({isolation:?}): {:?}",
            plan.rimz_reach
        );
    }

    // 5. Untrusted project env is never read and trust refusal stays behind earlier refusals.
    let untrusted_claude = home.join(".claude-untrusted");
    std::fs::create_dir_all(&untrusted_claude).unwrap();
    write_skill(&untrusted_claude.join("skills/rimz-message"), msg_body);
    std::fs::write(
        room.project.path().join(".rimz/config.toml"),
        format!(
            "[[agents]]\nname = 'claude'\nenv = {{ CLAUDE_CONFIG_DIR = '{}' }}\n",
            untrusted_claude.display()
        ),
    )
    .unwrap();
    let blocked = try_compile(&req, Isolation::Host)
        .err()
        .expect("must refuse");
    assert!(
        matches!(
            blocked,
            LaunchPlanErr::Process(launch::AgentProcessStageErr::Compile(
                launch::AgentProcessCompileErr::BlockedEnv { .. }
            ))
        ),
        "expected BlockedEnv, got: {blocked:?}"
    );
    let mut bad_prompt_req = req.clone();
    bad_prompt_req.action.extra_args_mut().extend([
        "--append-system-prompt-file".to_owned(),
        room.project
            .path()
            .join("missing-native-prompt.md")
            .display()
            .to_string(),
    ]);
    let earlier = try_compile(&bad_prompt_req, Isolation::Host)
        .err()
        .expect("must refuse");
    assert!(
        matches!(earlier, LaunchPlanErr::Prompt(_)),
        "trust refusal must not jump ahead of prompt digest refusal: {earlier:?}"
    );
}
