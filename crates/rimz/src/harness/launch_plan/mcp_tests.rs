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
        let machine: crate::config::MachineConfig = toml::from_str(machine).unwrap();
        let effective = crate::config::effective::load_with_roots(
            &machine,
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
            machine: &machine,
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
        let warnings: Vec<_> = plan
            .warnings
            .iter()
            .map(ToString::to_string)
            .filter(|warning| warning.contains("mcps"))
            .collect();
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
        assert!(!argv.iter().any(|arg| arg.contains("mcp")), "{argv:?}");
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
    let all = "rimz-lsp,rimz-wait,rimz-control,rimz-notes";

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
    assert_eq!(mcps_warnings(&plan), Vec::<String>::new());
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
    let marks = "rimz-lsp:always,rimz-wait:deferred,rimz-control:deferred,rimz-notes:deferred";
    let plan = room.compile(&switches(true, true), &marked);
    assert_eq!(
        mcp_lines(&plan),
        ["inheritMcp: false", r#"mcpServers: [{"name": "github"}]"#]
    );
    serves(&plan, marks);
    let github = "the mcps entry `github:always` is not applied: Antigravity takes a load mode only for RimZ's capabilities";
    assert_eq!(mcps_warnings(&plan), [github]);
    assert_eq!(plan.process().mcp_warnings, [github]);
    let plan = room.compile(&switches(true, false), &marked);
    assert_eq!(
        mcps_warnings(&plan),
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
            "rimz-lsp:always,rimz-wait,rimz-control,rimz-notes",
        ),
    ] {
        let plan = room.compile(&switches(true, true), &launch("antigravity", mcps, &[]));
        assert_eq!(plan.agent_file, None);
        serves(&plan, value);
        assert_eq!(mcps_warnings(&plan), Vec::<String>::new());
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

fn mcps_warnings(plan: &LaunchPlan) -> Vec<String> {
    plan.warnings
        .iter()
        .map(ToString::to_string)
        .filter(|warning| warning.contains("mcps"))
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
        assert!(reminders.lsp_tool(&request), "{mcps:?}");

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
        (entry, mcps_warnings(&plan))
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
        mcps_warnings(&plan),
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
        mcps_warnings(&plan),
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
        (plan.process().provider_argv.clone(), mcps_warnings(&plan))
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
        assert_eq!(mcps_warnings(&plan).len(), usize::from(warns), "{mcps:?}");
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
fn a_registered_capability_hides_its_skill_from_the_host_view() {
    let room = Room::new();
    let home = Path::new(&room.ambient["HOME"]);
    let names = [
        "commit",
        "rimz-control",
        "rimz-lsp",
        "rimz-notes",
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
    let all = ["rimz-control", "rimz-lsp", "rimz-notes", "rimz-wait"];
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

/// agy reads its own server's tools under the adapter's name, and an `allowed-tools` launch
/// that serves them allows them while the routine switch is on. A launch with no list, or a
/// list agy cannot read a rule of, emits none, so the account's saved allows stay in force.
#[test]
fn an_antigravity_allow_list_carries_the_served_tools_while_routine_is_on() {
    let room = Room::new();
    let allow = |routine: bool, mcps: Option<&[&str]>, rules: Option<&[&str]>| {
        let mut request = launch("antigravity", mcps, &[]);
        request.allowed_tools =
            rules.map(|rules| rules.iter().map(|rule| rule.parse().unwrap()).collect());
        let plan = room.compile(&switches(routine, true), &request);
        flag_values(&plan.process().provider_argv, "--settings")
            .last()
            .map(|value| serde_json::from_str::<serde_json::Value>(value).unwrap())
            .and_then(|settings| settings.pointer("/permissions/allow").cloned())
    };
    let read = Some(&["Read"][..]);
    assert_eq!(
        allow(true, None, read),
        Some(serde_json::json!(["read_file(*)", "mcp(rimz_tools/*)"]))
    );
    assert_eq!(
        allow(
            true,
            Some(&["rimz-lsp", "github"]),
            Some(&["Read", "mcp__github"])
        ),
        Some(serde_json::json!([
            "read_file(*)",
            "mcp(github/*)",
            "mcp(rimz_tools/*)"
        ]))
    );
    assert_eq!(
        allow(false, None, read),
        Some(serde_json::json!(["read_file(*)"]))
    );
    assert_eq!(
        allow(true, Some(&[]), read),
        Some(serde_json::json!(["read_file(*)"]))
    );
    assert_eq!(allow(true, None, None), None);
    assert_eq!(allow(true, None, Some(&["Glob"])), None);
    assert_eq!(allow(true, None, Some(&[])), None);
}
