use super::*;
use serde_json::{Value, json};
use std::fs;

const EVENTS: &[&str] = &[
    "on_session_start",
    "on_session_reset",
    "pre_llm_call",
    "post_tool_call",
    "on_session_end",
    "post_api_request",
    "on_human_input_request",
    "on_human_input_resolved",
    "subagent_start",
    "subagent_stop",
];

#[test]
fn preview_install_uninstall_preserve_user_values_and_outside_bytes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    let prefix = "# before\nmodel: 'keep me'\n";
    let suffix = "# after\nagent:\n  system_prompt: unchanged\n";
    let hooks = "hooks:\n  on_session_start:\n    - command: user-command\n      timeout: 7\n  output_spill: {enabled: true}\n  outbound: [https://example.invalid]\n";
    let original = format!("{prefix}{hooks}{suffix}");
    fs::write(&path, &original).unwrap();
    let preview = preview_at(&path, None).expect("preview");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert!(preview.files[0].candidate.starts_with(prefix));
    assert!(preview.files[0].candidate.ends_with(suffix));
    install_at(&path, None).expect("install");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        preview.files[0].candidate
    );
    assert!(installed_at(&path));
    let first = fs::read_to_string(&path).unwrap();
    install_at(&path, None).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), first);
    uninstall_at(&path).unwrap();
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.starts_with(prefix));
    assert!(after.ends_with(suffix));
    assert_eq!(
        serde_saphyr::from_str::<Value>(&after).unwrap(),
        serde_saphyr::from_str::<Value>(&original).unwrap()
    );
}

#[test]
fn missing_file_and_underwired_config_are_repaired() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    install_at(&path, None).expect("create");
    let mut config: Value = serde_saphyr::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(config["hooks"].as_object().unwrap().len(), EVENTS.len());
    for event in EVENTS {
        assert_eq!(
            config["hooks"][event][0],
            json!({"command":COMMAND,"timeout":10})
        );
    }
    config["hooks"].as_object_mut().unwrap().remove(EVENTS[0]);
    // A block root with a flow hooks value is still a supported YAML mapping.
    fs::write(&path, format!("hooks: {}\n", config["hooks"])).unwrap();
    assert!(!installed_at(&path));
    assert_ne!(
        preview_at(&path, None).unwrap().files[0].candidate,
        fs::read_to_string(&path).unwrap()
    );
    install_at(&path, None).unwrap();
    assert!(installed_at(&path));
    // Hermes's own config writes re-emit `hooks` in their own layout; a
    // fully wired file in any layout is left byte-identical.
    let config: Value = serde_saphyr::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let reformatted = format!("hooks: {}\n", config["hooks"]);
    fs::write(&path, &reformatted).unwrap();
    assert_eq!(
        preview_at(&path, None).unwrap().files[0].candidate,
        reformatted
    );
    install_at(&path, None).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), reformatted);
    uninstall_at(&path).unwrap();
    assert!(!fs::read_to_string(&path).unwrap().contains("hooks"));
}

#[test]
fn empty_and_unicode_hooks_keep_neighboring_keys() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    for hooks in [
        "hooks:\n",
        "hooks: null\n",
        "hooks: {}\n",
        "'hooks': {custom: null}\n",
    ] {
        let original = format!("# café\nmodel: modèle\n{hooks}# untouched\nafter: unchanged\n");
        fs::write(&path, &original).unwrap();
        install_at(&path, None).unwrap();
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.starts_with("# café\nmodel: modèle\n"), "{after}");
        assert!(
            after.ends_with("# untouched\nafter: unchanged\n"),
            "{after}"
        );
        assert_eq!(
            serde_saphyr::from_str::<Value>(&after).unwrap()["after"],
            "unchanged"
        );
    }
}

#[test]
fn unsupported_yaml_refuses_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    for input in [
        "hooks: [unterminated",
        "{hooks: {}}\n",
        "hooks: {}\n---\nhooks: {}\n",
        "hooks: &shared {}\n",
        "shared: &shared {}\nhooks: *shared\n",
        "hooks: !custom {}\n",
        "hooks:\n  on_session_start:\n    - command: !custom user\n",
        "hooks:\n  ? [complex, key]\n  : value\n",
        "hooks:\n  on_session_start: wrong-shape\n",
    ] {
        fs::write(&path, input).unwrap();
        let error = preview_at(&path, None).expect_err(input).to_string();
        assert!(
            error.contains("rimz hooks install hermes --dry-run"),
            "{error}"
        );
        assert!(error.contains(&path.display().to_string()), "{error}");
        assert!(install_at(&path, None).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), input);
    }
}

#[test]
fn version_floor_refuses_old_and_abstains_when_unreadable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    let error = preview_at(&path, Some(CliVersion::new(0, 21, 9)))
        .unwrap_err()
        .to_string();
    assert!(error.contains("older than 0.22.2"), "{error}");
    assert!(!path.exists());
    assert_eq!(
        "v0.22.2".parse::<CliVersion>().unwrap(),
        CliVersion::new(0, 22, 2)
    );
    assert!(preview_at(&path, None).is_ok());
}

#[test]
fn consent_is_exact_per_event_and_command_and_never_granted_by_rimz() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.yaml");
    install_at(&path, None).unwrap();
    let allowlist = root.path().join("shell-hooks-allowlist.json");
    assert_eq!(untrusted_at(&path), EVENTS);
    assert!(!allowlist.exists());
    fs::write(&allowlist, json!({"approvals":[{"event":EVENTS[0],"command":COMMAND},{"event":EVENTS[1],"command":"another command"}]}).to_string()).unwrap();
    assert_eq!(untrusted_at(&path), EVENTS[1..]);
    for malformed in ["broken", "{}", r#"{"approvals":42}"#] {
        fs::write(&allowlist, malformed).unwrap();
        assert_eq!(untrusted_at(&path), EVENTS);
    }
}
