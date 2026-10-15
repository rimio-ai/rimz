use crate::agents::{AgentDefinition, AgentHookClass, AskKind, HookReply, LifecycleSignal};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn muse() -> &'static AgentDefinition {
    let definition = crate::agents::find_definition("muse");
    assert!(definition.is_some(), "Muse must be a built-in adapter");
    definition.unwrap()
}

#[test]
fn native_events_keep_turn_identity_and_map_without_tool_fallbacks() {
    let adapter = muse();
    for (event, expected) in [
        ("SessionStart", LifecycleSignal::Registered),
        (
            "UserPromptSubmit",
            LifecycleSignal::TurnStarted {
                turn_id: Some("turn".into()),
            },
        ),
        (
            "PermissionRequest",
            LifecycleSignal::AwaitingInput {
                kind: AskKind::Permission,
                ask_id: None,
                detail: None,
                native_key: None,
            },
        ),
        (
            "Stop",
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: Some("turn".into()),
            },
        ),
        (
            "StopFailure",
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
                turn_id: Some("turn".into()),
            },
        ),
        (
            "Interrupt",
            LifecycleSignal::TurnInterrupted {
                turn_id: Some("turn".into()),
            },
        ),
        ("SessionEnd", LifecycleSignal::Ended),
    ] {
        let decoded = adapter.decode_hook(event, &json!({"session_id": "root", "turn_id": "turn", "source": "startup", "cwd": "/project", "tool_name": "ask_user", "prompt": "hello"})).unwrap();
        let observation = decoded.lifecycle().expect("native lifecycle observation");
        assert_eq!(observation.signal, expected, "{event}");
        assert_eq!(observation.agent_id.as_ref().unwrap().as_str(), "root");
        assert_eq!(observation.worktree_path.as_deref(), Some("/project"));
    }
    for event in ["PreToolUse", "PostToolUse", "PostToolUseFailure"] {
        let decoded = adapter.decode_hook(event, &json!({"session_id": "root", "turn_id": "turn", "tool_name": "shell", "tool_use_id": "tool"})).unwrap();
        assert!(
            matches!(decoded.lifecycle().unwrap().signal, LifecycleSignal::ToolUsed { ref name, ref native_key, ref turn_id, .. } if name.as_deref() == Some("shell") && native_key.as_deref() == Some("tool") && turn_id.as_deref() == Some("turn")),
            "{event}"
        );
    }
    let unknown = adapter
        .decode_hook("Notification", &json!({"session_id": "root"}))
        .unwrap();
    assert_eq!(unknown.class(), AgentHookClass::Unknown);
    assert!(unknown.lifecycle().is_none());
}

#[test]
fn captured_user_shell_hooks_do_not_change_lifecycle() {
    for sample in [
        include_str!("fixtures/PreToolUse.json"),
        include_str!("fixtures/PostToolUse.json"),
        include_str!("fixtures/PostToolUseFailure.json"),
    ] {
        let payload: Value = serde_json::from_str(sample).unwrap();
        let event = payload["hook_event_name"].as_str().unwrap();
        let decoded = muse().decode_hook(event, &payload).unwrap();
        assert!(
            decoded.lifecycle().is_none(),
            "{event}: explicit user shell input is not a model turn"
        );
        assert_eq!(decoded.reply(), &HookReply::Silent);
        assert_eq!(
            decoded.event_agent_id().unwrap().as_str(),
            payload["session_id"].as_str().unwrap()
        );
    }
}

#[test]
fn permission_and_malformed_payloads_have_silent_stdout() {
    let adapter = muse();
    let permission = adapter
        .decode_hook("PermissionRequest", &json!({"session_id": "root"}))
        .unwrap();
    assert_eq!(permission.reply(), &HookReply::Silent);
    insta::assert_debug_snapshot!(permission.reply(), @"Silent");
    let malformed = adapter
        .decode_hook("Stop", &json!({"session_id": [], "turn_id": 42}))
        .unwrap();
    assert_eq!(malformed.reply(), &HookReply::Silent);
    assert!(malformed.event_agent_id().is_none());
    insta::assert_debug_snapshot!(malformed.reply(), @"Silent");
}

#[test]
fn versioned_comm_is_scoped_to_muse_and_never_claims_the_launcher() {
    let spec = muse().spec();
    for name in ["muse-bin-1.4.9-", "muse-bin-1.10.12-R1", "muse-bin-1.10.1"] {
        assert!(spec.runs_as(name), "{name}");
        for other in
            crate::agents::all_definitions().filter(|definition| definition.spec().kind != "muse")
        {
            assert!(
                !other.spec().runs_as(name),
                "{} claimed {name}",
                other.spec().kind
            );
        }
    }
    for name in ["muse", "muse-bin-", "muse-bin-nope", "not-muse-bin-1.4.9-"] {
        assert!(!spec.runs_as(name), "{name}");
    }
    assert!(spec.launches_as("muse"));
}

#[test]
fn launch_resume_permissions_and_native_sandbox_switch_are_explicit() {
    use crate::agents::PermissionMode;
    let adapter = muse();
    let spec = adapter.spec();
    for (mode, expected) in [
        (
            PermissionMode::Auto,
            vec!["--permission-profile", ":auto-review"],
        ),
        (PermissionMode::Ask, vec!["--permission-profile", ":ask-me"]),
        (
            PermissionMode::Plan,
            vec!["--permission-profile", ":read-only"],
        ),
        (PermissionMode::Yolo, vec!["--yolo"]),
    ] {
        assert_eq!(spec.launch.permission_args(mode), expected);
    }
    assert_eq!(
        spec.launch.launch_command(&[], Some("-inspect")).unwrap(),
        ["muse", "--", "-inspect"]
    );
    assert_eq!(
        spec.launch.resume_command("root").unwrap(),
        ["muse", "resume", "root"]
    );
    assert_eq!(spec.launch.picker, Some(["muse", "resume"].as_slice()));
    assert!(spec.launch.fork.is_none());
    assert!(spec.capabilities.registers_lazily);
    let mut args = vec!["--yolo".to_owned()];
    adapter.disable_native_sandbox_args(&mut args);
    assert_eq!(args, ["--yolo", "--disable-sandbox"]);
}

#[test]
fn managed_install_is_two_files_without_markers_and_restores_user_settings() {
    let adapter = muse();
    let dir = tempfile::tempdir().unwrap();
    let env = BTreeMap::from([("XDG_CONFIG_HOME".into(), dir.path().display().to_string())]);
    let integration = adapter.managed_integration().expect("managed install");
    let home = dir.path().join("muse");
    let version = Some(crate::agents::version::CliVersion::new(1, 4, 9));
    let preview = super::install::preview_at(&home, version).unwrap();
    assert_eq!(preview.files.len(), 2);
    assert!(!home.exists(), "preview must not write");
    super::install::install_at(&home, version).unwrap();
    let path = home.join("settings.json");
    let settings: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(settings["schema_version"], 1);
    assert!(integration.installed(&env));
    let canonical_settings = std::fs::read(&path).unwrap();
    for (key, value) in [
        ("schema_version", json!(2)),
        ("managed_hooks_path", json!("/other/hooks.json")),
    ] {
        let mut drift = settings.clone();
        drift[key] = value;
        std::fs::write(&path, drift.to_string()).unwrap();
        assert!(
            !integration.installed(&env),
            "{key} drift must reoffer install"
        );
    }
    std::fs::write(&path, &canonical_settings).unwrap();
    let hooks = std::fs::read(home.join("rimz-hooks.json")).unwrap();
    for bytes in [&hooks, &std::fs::read(&path).unwrap()] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("_rimz_managed") && !text.contains("_rimz_sync"));
    }
    let hooks_json: Value = serde_json::from_slice(&hooks).unwrap();
    std::fs::write(home.join("rimz-hooks.json"), "{}").unwrap();
    assert!(!integration.installed(&env));
    std::fs::write(home.join("rimz-hooks.json"), &hooks).unwrap();
    let names = settings["managed_hooks_env_vars"].as_array().unwrap();
    assert!(
        adapter
            .hook_env_keys()
            .iter()
            .all(|name| names.contains(&json!(name)))
    );
    assert!(!names.contains(&json!("RIMZ_AGENT_PID")));
    assert!(!names.contains(&json!("RIMZ_HOOK_OWNER_KIND")));
    assert_eq!(hooks_json["hooks"].as_object().unwrap().len(), 10);
    for (event, groups) in hooks_json["hooks"].as_object().unwrap() {
        assert_eq!(groups[0].as_object().unwrap().len(), 1);
        let handler = &groups[0]["hooks"][0];
        assert_eq!(handler["command"], "rimz hooks feed --source muse");
        assert_eq!(handler["timeout"], 4);
        assert_eq!(
            handler["async"],
            if event == "Interrupt" {
                json!(true)
            } else {
                Value::Null
            }
        );
    }
    let original = json!({"schema_version": 1, "theme": "mine", "hooks": {"Stop": [{"hooks": [{"type":"command", "command":"user-hook"}]}]}});
    std::fs::write(&path, original.to_string()).unwrap();
    super::install::install_at(&home, version).unwrap();
    let installed = std::fs::read(&path).unwrap();
    super::install::install_at(&home, version).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), installed);
    let mut drift: Value = serde_json::from_slice(&installed).unwrap();
    drift["managed_hooks_env_vars"]
        .as_array_mut()
        .unwrap()
        .pop();
    std::fs::write(&path, drift.to_string()).unwrap();
    assert!(!integration.installed(&env));
    std::fs::write(&path, &installed).unwrap();
    super::install::uninstall_at(&home).unwrap();
    assert!(!home.join("rimz-hooks.json").exists());
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
        original
    );
}

#[test]
fn install_refuses_invalid_schema_foreign_owner_and_old_or_unknown_version() {
    muse();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let version = Some(crate::agents::version::CliVersion::new(1, 4, 9));
    for settings in [
        json!({}),
        json!({"schema_version": 2}),
        json!({"schema_version": 1, "managed_hooks_path": "/enterprise/hooks.json"}),
    ] {
        let bytes = settings.to_string();
        std::fs::write(home.join("settings.json"), &bytes).unwrap();
        let error = super::install::install_at(home, version)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("settings.json") || error.contains("/enterprise/hooks.json"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(home.join("settings.json")).unwrap(),
            bytes
        );
        assert!(!home.join("rimz-hooks.json").exists());
    }
    std::fs::remove_file(home.join("settings.json")).unwrap();
    for version in [None, Some(crate::agents::version::CliVersion::new(1, 4, 8))] {
        let error = super::install::install_at(home, version)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("1.4.9") && error.contains("update Muse"),
            "{error}"
        );
        assert!(!home.join("settings.json").exists());
    }
}

#[test]
fn uninstall_does_not_remove_another_managed_source() {
    muse();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let original = json!({"schema_version": 1, "managed_hooks_path": "/other/hooks.json", "managed_hooks_env_vars": ["USER_KEY"]});
    std::fs::write(home.join("settings.json"), original.to_string()).unwrap();
    std::fs::write(home.join("rimz-hooks.json"), "{}").unwrap();
    super::install::uninstall_at(home).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(home.join("settings.json")).unwrap())
            .unwrap(),
        original
    );
    assert!(!home.join("rimz-hooks.json").exists());
}

#[test]
fn default_config_home_install_survives_runtime_domain_xdg_pinning() {
    let adapter = muse();
    let dir = tempfile::tempdir().unwrap();
    let mut env = BTreeMap::from([("HOME".into(), dir.path().display().to_string())]);
    let home = adapter.config_home(&env).unwrap();
    super::install::install_at(
        &home,
        Some(crate::agents::version::CliVersion::new(1, 4, 9)),
    )
    .unwrap();
    // runtime_domain_env pins the default config root even when it was unset
    // at install time, as a tmux server's environment does for later panes.
    env.insert(
        "XDG_CONFIG_HOME".into(),
        dir.path().join(".config").display().to_string(),
    );
    assert!(adapter.hooks_installed(&env));
}
