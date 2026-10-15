use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::lifecycle::{LifecycleState, PriorTurnIds, TurnPhase, step};
use crate::agents::{
    AgentDefinition, AgentHookClass, AgentStatus, HookOutput, HookReply, LifecycleSignal,
};
use serde_json::{Value, json};

fn adapter() -> &'static AgentDefinition {
    crate::agents::registry::find_definition("minimax")
        .expect("MiniMax must be a registered built-in")
}

fn decode(event: &str, payload: Value) -> HookOutput {
    adapter().decode_hook(event, &payload).unwrap()
}

fn signal(event: &str, payload: Value) -> LifecycleSignal {
    decode(event, payload).lifecycle().unwrap().signal.clone()
}

#[test]
fn lifecycle_and_permission_follow_the_real_step_table() {
    let mut state: Option<LifecycleState> = None;
    for (event, payload, status, phase, compacting) in [
        (
            "SessionStart",
            json!({"source":"startup"}),
            AgentStatus::Idle,
            TurnPhase::Idle,
            false,
        ),
        (
            "UserPromptSubmit",
            json!({"prompt":"fix it", "turn_id":"turn-1"}),
            AgentStatus::Running,
            TurnPhase::Reasoning,
            false,
        ),
        (
            "PreToolUse",
            json!({"tool_name":"read", "tool_use_id":"read-1"}),
            AgentStatus::Running,
            TurnPhase::Reasoning,
            false,
        ),
        (
            "PermissionRequest",
            json!({"tool_name":"bash"}),
            AgentStatus::Waiting,
            TurnPhase::Idle,
            false,
        ),
        (
            "PostToolUse",
            json!({"tool_name":"edit", "tool_use_id":"edit-1"}),
            AgentStatus::Running,
            TurnPhase::Acting,
            false,
        ),
        (
            "PreCompact",
            json!({}),
            AgentStatus::Running,
            TurnPhase::Acting,
            true,
        ),
        (
            "PostCompact",
            json!({"trigger":"auto"}),
            AgentStatus::Running,
            TurnPhase::Acting,
            false,
        ),
        (
            "SessionStart",
            json!({"source":"compact"}),
            AgentStatus::Running,
            TurnPhase::Acting,
            false,
        ),
        (
            "PermissionRequest",
            json!({"tool_name":"write"}),
            AgentStatus::Waiting,
            TurnPhase::Idle,
            false,
        ),
        (
            "Stop",
            json!({"turn_id":"turn-1"}),
            AgentStatus::Success,
            TurnPhase::Idle,
            false,
        ),
        // MiniMax supplies no permission key: the next actual tool is new work, not a synthetic closing ToolUsed which would reopen this settled turn.
    ] {
        let transition = step(
            state.as_ref(),
            None,
            PriorTurnIds::default(),
            &signal(event, payload),
        );
        assert_eq!(
            (
                transition.next.status,
                transition.next.phase,
                transition.next.compacting
            ),
            (status, phase, compacting),
            "{event}"
        );
        state = Some(transition.next);
    }
    for reason in ["resume", "clear", "logout", "other"] {
        assert_eq!(
            signal("SessionEnd", json!({"reason":reason})),
            LifecycleSignal::Ended
        );
        assert!(decode("SessionEnd", json!({"reason":reason})).ends_session());
    }
    let prompt = decode(
        "UserPromptSubmit",
        json!({"session_id":"mvs_root", "prompt":"fix it", "cwd":"/project", "model":"minimax/model", "turn_id":"turn-1"}),
    );
    let observation = prompt.lifecycle().unwrap();
    assert_eq!(observation.agent_id.as_ref().unwrap().as_str(), "mvs_root");
    assert_eq!(observation.worktree_path.as_deref(), Some("/project"));
    assert_eq!(observation.launch.model.as_deref(), Some("minimax/model"));
    assert_eq!(observation.task.as_deref(), Some("fix it"));
    assert_eq!(
        observation.signal,
        LifecycleSignal::TurnStarted {
            turn_id: Some("turn-1".into())
        }
    );
}

#[test]
fn child_events_cannot_register_or_mark_the_root_waiting() {
    for event in [
        "PostToolUse",
        "PermissionRequest",
        "SessionStart",
        "Stop",
        "SessionEnd",
        "SubagentStart",
        "SubagentStop",
    ] {
        let decoded = decode(
            event,
            json!({"session_id":"mvs_child", "agent_id":"child-1", "tool_name":"edit"}),
        );
        assert!(
            decoded.lifecycle().is_none(),
            "{event} must not register a child as a new root"
        );
        assert!(decoded.event_agent_id().is_none());
        assert_eq!(decoded.class(), AgentHookClass::Unknown);
        assert!(!decoded.records_progress());
        assert!(!decoded.ends_session());
    }
    assert!(
        decode(
            "PostToolUse",
            json!({"session_id":"mvs_root", "tool_name":"edit"})
        )
        .lifecycle()
        .is_some()
    );
}

#[test]
fn blocking_events_reply_with_byte_empty_stdout() {
    let stdout = |event| match adapter().payload_hook_reply(event, &json!({})) {
        HookReply::Silent => String::new(),
        HookReply::Json(value) => format!("{value}\n"),
    };
    insta::assert_snapshot!(stdout("PreToolUse"), @"");
    insta::assert_snapshot!(stdout("PermissionRequest"), @"");
    insta::assert_snapshot!(stdout("UserPromptSubmit"), @"");
    insta::assert_snapshot!(stdout("Stop"), @"");
    insta::assert_snapshot!(stdout("PreCompact"), @"");
}

#[test]
fn malformed_payloads_are_tolerated_without_inventing_identity() {
    let mut rows = Vec::new();
    for payload in [
        Value::Null,
        json!([]),
        json!({"session_id":7,"tool_name":false,"turn_id":{}}),
    ] {
        let decoded = decode("PostToolUse", payload);
        rows.push(format!(
            "{:?} {:?}",
            decoded.event_agent_id(),
            decoded.lifecycle().unwrap().signal
        ));
    }
    insta::assert_snapshot!(rows.join("\n"), @r#"
    None ToolUsed { mutates: false, edits: false, name: None, native_key: None, turn_id: None }
    None ToolUsed { mutates: false, edits: false, name: None, native_key: None, turn_id: None }
    None ToolUsed { mutates: false, edits: false, name: None, native_key: None, turn_id: None }
    "#);
    assert_eq!(
        decode("unknown", json!({})).class(),
        AgentHookClass::Unknown
    );
}

#[test]
fn compaction_duplicates_preserve_the_result_and_failure_has_no_hook() {
    for trigger in ["auto", "manual"] {
        let running = LifecycleState {
            status: AgentStatus::Running,
            phase: TurnPhase::Reasoning,
            compacting: true,
        };
        let ended = step(
            Some(&running),
            None,
            PriorTurnIds::default(),
            &signal("PostCompact", json!({"trigger":trigger})),
        )
        .next;
        let duplicate = step(
            Some(&ended),
            None,
            PriorTurnIds::default(),
            &signal("SessionStart", json!({"source":"compact"})),
        );
        assert_eq!(duplicate.next, ended);
        assert!(matches!(
            duplicate.kind,
            crate::agents::lifecycle::TransitionKind::Ignored { .. }
        ));
    }
    assert!(
        decode("StopFailure", json!({})).lifecycle().is_none(),
        "failed and cancelled turns have no verified terminal hook"
    );
    let waiting = step(
        None,
        None,
        PriorTurnIds::default(),
        &signal("PermissionRequest", json!({"tool_name":"bash"})),
    )
    .next;
    let settled = step(
        Some(&waiting),
        None,
        PriorTurnIds::default(),
        &signal("Stop", json!({})),
    )
    .next;
    assert_eq!(settled.status, AgentStatus::Success);
    assert!(
        !adapter().tool_call_resolved(&json!({}), "permission"),
        "no synthetic closing ToolUsed may reopen the settled turn"
    );
    assert!(
        !decode("Stop", json!({})).records_progress(),
        "a settle must not immediately wake a cleared ask as work"
    );
}

#[cfg(unix)]
#[test]
fn package_install_refuses_a_symlinked_package_or_foreign_manifest() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let package = root.path().join("plugins/rimz");
    std::fs::create_dir_all(package.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), &package).unwrap();
    let env = BTreeMap::from([("MINIMAX_DATA_DIR".into(), root.path().display().to_string())]);
    let managed = adapter().managed_integration().unwrap();
    assert!(
        managed
            .install(&env, crate::agents::HookInstallOptions::default())
            .is_err()
    );
    assert!(!managed.installed(&env));
    std::fs::remove_file(&package).unwrap();
    std::fs::create_dir_all(package.join(".minimax-plugin")).unwrap();
    let manifest = package.join(".minimax-plugin/plugin.json");
    std::fs::write(&manifest, r#"{"name":"user-plugin"}"#).unwrap();
    assert!(
        managed
            .install(&env, crate::agents::HookInstallOptions::default())
            .is_err()
    );
    assert!(managed.uninstall(&env).is_err());
    assert_eq!(
        std::fs::read_to_string(manifest).unwrap(),
        r#"{"name":"user-plugin"}"#
    );
}

#[test]
fn manifestless_foreign_packages_are_preserved() {
    for operation in ["install", "uninstall"] {
        for foreign in [
            ".claude-plugin/plugin.json",
            "hooks/user.json",
            ".minimax-plugin/user.json",
        ] {
            let root = tempfile::tempdir().unwrap();
            let env =
                BTreeMap::from([("MINIMAX_DATA_DIR".into(), root.path().display().to_string())]);
            let package = root.path().join("plugins/rimz");
            let file = package.join(foreign);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, "user plugin").unwrap();
            let managed = adapter().managed_integration().unwrap();
            let result = if operation == "install" {
                managed
                    .install(&env, crate::agents::HookInstallOptions::default())
                    .map(|_| ())
            } else {
                managed.uninstall(&env).map(|_| ())
            };
            assert!(result.is_err(), "{operation} must refuse {foreign}");
            assert!(result.unwrap_err().to_string().contains("move it aside"));
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "user plugin");
            assert!(!package.join(".minimax-plugin/plugin.json").exists());
        }
    }
}

#[test]
fn partial_package_install_can_resume() {
    let root = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([("MINIMAX_DATA_DIR".into(), root.path().display().to_string())]);
    let package = root.path().join("plugins/rimz");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("icon.png"), "partial icon").unwrap();
    let managed = adapter().managed_integration().unwrap();
    managed
        .install(&env, crate::agents::HookInstallOptions::default())
        .unwrap();
    assert!(managed.installed(&env));
}

#[test]
fn package_install_is_idempotent_upgradeable_and_directory_scoped() {
    let root = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([("MINIMAX_DATA_DIR".into(), root.path().display().to_string())]);
    let managed = adapter().managed_integration().unwrap();
    let package = root.path().join("plugins/rimz");
    let sibling = root.path().join("plugins/user");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("keep"), "user plugin").unwrap();
    let preview = managed
        .preview(&env, crate::agents::HookInstallOptions::default())
        .unwrap();
    assert!(!package.exists(), "preview must not write");
    assert_eq!(preview.planned_events.len(), 9);
    let installed = managed
        .install(&env, crate::agents::HookInstallOptions::default())
        .unwrap();
    assert_eq!(installed.installed_events, preview.planned_events);
    assert!(managed.installed(&env));
    assert!(!managed.upgrade_available(&env, crate::agents::HookInstallOptions::default()));
    let hooks = package.join("hooks/rimz.json");
    let metadata = std::fs::metadata(&hooks).unwrap();
    managed
        .install(&env, crate::agents::HookInstallOptions::default())
        .unwrap();
    assert_eq!(
        std::fs::metadata(&hooks).unwrap().modified().unwrap(),
        metadata.modified().unwrap(),
        "second install must not rewrite"
    );
    let original = std::fs::read(&hooks).unwrap();
    std::fs::write(&hooks, b"{}\n").unwrap();
    assert!(managed.upgrade_available(&env, crate::agents::HookInstallOptions::default()));
    managed
        .install(&env, crate::agents::HookInstallOptions::default())
        .unwrap();
    assert_eq!(std::fs::read(&hooks).unwrap(), original);
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(package.join(".minimax-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "apps",
            "author",
            "category",
            "description",
            "exampleQueries",
            "hooks",
            "icon",
            "mcpServers",
            "name",
            "schemaVersion",
            "skills",
            "version"
        ]
    );
    assert_eq!(manifest["name"], "rimz");
    assert_eq!(manifest["schemaVersion"], 1);
    assert_eq!(manifest["hooks"], json!(["hooks/rimz.json"]));
    let icon = std::fs::File::open(package.join(manifest["icon"].as_str().unwrap())).unwrap();
    let mut reader = png::Decoder::new(std::io::BufReader::new(icon))
        .read_info()
        .expect("manifest icon must be a real PNG");
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    reader.next_frame(&mut pixels).unwrap();
    let hook_doc: Value = serde_json::from_slice(&original).unwrap();
    for (event, entries) in hook_doc["hooks"].as_object().unwrap() {
        let command = &entries[0]["hooks"][0];
        assert_eq!(
            command["command"],
            "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source minimax"
        );
        assert_eq!(command["type"], "command");
        let timeout = command["timeout"].as_u64().unwrap();
        assert!(timeout > 0 && timeout <= if event == "SessionEnd" { 2 } else { 10 });
        assert_eq!(command.as_object().unwrap().len(), 3);
    }
    managed.uninstall(&env).unwrap();
    assert!(!package.exists());
    assert_eq!(
        std::fs::read_to_string(sibling.join("keep")).unwrap(),
        "user plugin"
    );
    assert!(!managed.installed(&env));
    managed.uninstall(&env).unwrap();
}

#[test]
fn data_home_precedence_and_installed_checks_match_the_provider() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().display().to_string();
    let mut env = BTreeMap::from([("HOME".into(), home.clone())]);
    for (key, value, expected) in [
        ("HOME", home.as_str(), root.path().join(".minimax")),
        ("MAVIS_DATA_DIR", " /mavis ", PathBuf::from("/mavis")),
        ("MINIMAX_DATA_DIR", " /minimax ", PathBuf::from("/minimax")),
        ("MINIMAX_DATA_DIR", "  ", PathBuf::from("/mavis")),
    ] {
        env.insert(key.into(), value.into());
        assert_eq!(adapter().config_home(&env), Some(expected));
    }
    assert_eq!(adapter().config_home(&BTreeMap::new()), None);
    for key in ["HOME", "MINIMAX_DATA_DIR", "MAVIS_DATA_DIR"] {
        let mut env = BTreeMap::from([(key.into(), home.clone())]);
        let managed = adapter().managed_integration().unwrap();
        managed
            .install(&env, crate::agents::HookInstallOptions::default())
            .unwrap();
        assert!(managed.installed(&env));
        let mut pinned = crate::disk::paths::runtime_domain_env();
        pinned.extend(env.clone());
        assert!(managed.installed(&pinned));
        env.insert("XDG_CONFIG_HOME".into(), "/irrelevant".into());
        assert!(managed.installed(&env));
        managed.uninstall(&env).unwrap();
    }
}

#[test]
fn launch_and_process_identity_match_mcode() {
    let definition = adapter();
    assert_eq!(
        crate::agents::registry::find_definition("mcode")
            .unwrap()
            .spec()
            .kind,
        "minimax"
    );
    assert_eq!(
        definition.launch_command(&[], Some("-probe")).unwrap(),
        ["mcode", "--", "-probe"]
    );
    assert_eq!(
        definition
            .resume_command("mvs_root", Path::new("/project"))
            .unwrap(),
        ["mcode", "--session", "mvs_root"]
    );
    assert!(!definition.hook_inherits_env());
    for comm in ["node-MainThread", "node"] {
        assert_eq!(
            crate::agents::registry::command_agent_kind_with_comm("node server.js", Some(comm)),
            None,
            "a generic runtime must not identify an agent"
        );
        assert_eq!(
            crate::agents::registry::command_agent_kind_with_comm(
                "node /usr/local/bin/mcode",
                Some(comm)
            ),
            Some("minimax")
        );
    }
    for name in ["node", "minimax-code"] {
        assert!(definition.spec().process_names.contains(&name));
    }
    for command in ["mcode", "mcode --model minimax/model -- hello"] {
        assert!(definition.is_interactive_process(command));
    }
    for command in ["mcode exec hello", "mcode acp"] {
        assert!(!definition.is_interactive_process(command));
    }
    assert_eq!(
        definition.spec().launch.compact_command("keep details"),
        Some("/compact keep details".into())
    );
    assert_eq!(definition.config_home_env_keys(), ["MINIMAX_DATA_DIR"]);
    assert_eq!(
        definition.skills_home(&BTreeMap::from([("HOME".into(), "/home/test".into())])),
        None
    );
    assert!(definition.spec().launch.fork_command("mvs_root").is_none());
}
