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
            isolation: Isolation::Host,
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
    let strict = ["--strict-mcp-config", "--tools", "Bash"];
    let argv = |machine: &str, mcps: Option<&[&str]>| {
        room.compile(machine, &launch("claude", mcps, &strict))
            .process()
            .provider_argv
            .clone()
    };
    let unlisted = argv(&switches(true, true), None);
    assert!(unlisted.iter().any(|arg| arg == "--strict-mcp-config"));
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
        "[mcp_servers.github]\ncommand = 'gh'\n[mcp_servers.\"my.server\"]\ncommand = 'x'\n[mcp_servers.keep]\ncommand = 'k'\n",
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
        ["github", "my.server"]
    );
    assert_eq!(
        disabled(Some(&["keep"]), &["--profile", "work"]),
        ["chosen", "github", "my.server"]
    );
    assert_eq!(
        disabled(Some(&["keep"]), &["-pwork"]),
        ["chosen", "github", "my.server"]
    );
    assert_eq!(disabled(Some(&[]), &[]), ["github", "keep", "my.server"]);
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
