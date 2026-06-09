use super::*;

#[test]
fn install_adds_status_line_when_none_existed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    install_into(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
    assert_eq!(parsed["statusLine"]["_rimz_managed"], true);
    // Nothing was wrapped, so no `_rimz_wrapped`.
    assert!(parsed["statusLine"].get("_rimz_wrapped").is_none());
}

#[test]
fn install_wraps_existing_status_line_command() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{ "statusLine": { "type": "command", "command": "npx -y ccstatusline@latest" } }"#,
    )
    .unwrap();
    install_into(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
    assert_eq!(parsed["statusLine"]["_rimz_managed"], true);
    // The user's whole original value is captured verbatim.
    assert_eq!(
        parsed["statusLine"]["_rimz_wrapped"]["command"],
        "npx -y ccstatusline@latest"
    );
    assert_eq!(parsed["statusLine"]["_rimz_wrapped"]["type"], "command");
}

#[test]
fn install_wraps_and_restores_existing_subagent_status_line() {
    // The per-child `subagentStatusLine` is wrapped exactly like the session
    // `statusLine`: the user's command is captured, replaced by Rimz's
    // `--subagent` reader, and restored verbatim on uninstall.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{ "subagentStatusLine": { "type": "command", "command": "my-subagent-line" } }"#,
    )
    .unwrap();
    install_into(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        parsed["subagentStatusLine"]["command"],
        SUBAGENT_STATUS_LINE.command
    );
    assert_eq!(parsed["subagentStatusLine"]["_rimz_managed"], true);
    assert_eq!(
        parsed["subagentStatusLine"]["_rimz_wrapped"]["command"],
        "my-subagent-line"
    );
    // The session statusLine is wrapped independently and is unaffected.
    assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);

    // The feed reads the wrapped subagent command back as its pass-through
    // target, never the recursive Rimz one.
    let root = read_existing_json(&path).unwrap();
    assert_eq!(
        wrapped_status_line_command_from(&root, &SUBAGENT_STATUS_LINE).as_deref(),
        Some("my-subagent-line")
    );

    uninstall_from(&path).unwrap();
    let restored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        restored["subagentStatusLine"]["command"],
        "my-subagent-line"
    );
    assert_eq!(restored["subagentStatusLine"]["type"], "command");
    assert!(
        restored["subagentStatusLine"]
            .get("_rimz_managed")
            .is_none()
    );
}

#[test]
fn install_preserves_status_line_sibling_keys() {
    // A real ccstatusline config carries rendering options alongside the
    // command. They must ride the managed object so the wrap stays visually
    // faithful while installed, and the whole original still restores.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
            &path,
            r#"{ "statusLine": { "type": "command", "command": "npx -y ccstatusline@latest", "padding": 0, "refreshInterval": 10 } }"#,
        )
        .unwrap();
    install_into(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
    // Sibling rendering keys are carried onto the managed object.
    assert_eq!(parsed["statusLine"]["padding"], 0);
    assert_eq!(parsed["statusLine"]["refreshInterval"], 10);
    // The whole original is still captured for restoration.
    assert_eq!(parsed["statusLine"]["_rimz_wrapped"]["refreshInterval"], 10);

    uninstall_from(&path).unwrap();
    let restored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        restored["statusLine"]["command"],
        "npx -y ccstatusline@latest"
    );
    assert_eq!(restored["statusLine"]["padding"], 0);
    assert_eq!(restored["statusLine"]["refreshInterval"], 10);
    assert!(restored["statusLine"].get("_rimz_managed").is_none());
}

#[test]
fn reinstall_does_not_double_wrap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{ "statusLine": { "type": "command", "command": "user-line" } }"#,
    )
    .unwrap();
    install_into(&path).unwrap();
    let first = std::fs::read_to_string(&path).unwrap();
    install_into(&path).unwrap();
    let second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(first, second, "re-install must be byte-identical");
    let parsed: Value = serde_json::from_str(&second).unwrap();
    // Still the user's command, not a nested Rimz wrapper.
    assert_eq!(
        parsed["statusLine"]["_rimz_wrapped"]["command"],
        "user-line"
    );
    assert!(
        parsed["statusLine"]["_rimz_wrapped"]
            .get("_rimz_wrapped")
            .is_none()
    );
}

#[test]
fn reinstall_repairs_recursive_status_line_wrap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        serde_json::to_string(&json!({
            "statusLine": {
                "_rimz_managed": true,
                "_rimz_wrapped": {
                    "type": "command",
                    "command": STATUS_LINE_COMMAND,
                    "padding": 0,
                    "refreshInterval": 10
                },
                "type": "command",
                "command": STATUS_LINE_COMMAND,
                "padding": 0,
                "refreshInterval": 10
            }
        }))
        .unwrap(),
    )
    .unwrap();

    install_into(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
    assert!(
        parsed["statusLine"].get("_rimz_wrapped").is_none(),
        "a Rimz statusline command is not a user command to wrap"
    );
    assert_eq!(parsed["statusLine"]["padding"], 0);
    assert_eq!(parsed["statusLine"]["refreshInterval"], 10);
}

#[test]
fn uninstall_removes_recursive_status_line_wrap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        serde_json::to_string(&json!({
            "statusLine": {
                "_rimz_managed": true,
                "_rimz_wrapped": {
                    "type": "command",
                    "command": STATUS_LINE_COMMAND
                },
                "type": "command",
                "command": STATUS_LINE_COMMAND
            }
        }))
        .unwrap(),
    )
    .unwrap();

    uninstall_from(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        parsed.get("statusLine").is_none(),
        "uninstall must not restore Rimz's own statusline command"
    );
}

#[test]
fn wrapped_status_line_command_ignores_recursive_rimz_wrap() {
    let root: Map<String, Value> = serde_json::from_value(json!({
        "statusLine": {
            "_rimz_managed": true,
            "_rimz_wrapped": {
                "type": "command",
                "command": STATUS_LINE_COMMAND
            },
            "type": "command",
            "command": STATUS_LINE_COMMAND
        }
    }))
    .unwrap();

    assert_eq!(wrapped_status_line_command_from(&root, &STATUS_LINE), None);
}

#[test]
fn uninstall_restores_original_status_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let original = r#"{ "statusLine": { "type": "command", "command": "npx ccstatusline" } }"#;
    std::fs::write(&path, original).unwrap();
    install_into(&path).unwrap();
    uninstall_from(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["statusLine"]["command"], "npx ccstatusline");
    assert_eq!(parsed["statusLine"]["type"], "command");
    assert!(parsed["statusLine"].get("_rimz_managed").is_none());
}

#[test]
fn uninstall_removes_status_line_when_none_existed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    install_into(&path).unwrap();
    uninstall_from(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        parsed.get("statusLine").is_none(),
        "a Rimz-added statusLine is removed on uninstall"
    );
}

#[test]
fn install_captures_and_restores_bare_string_status_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, r#"{ "statusLine": "echo hi" }"#).unwrap();
    install_into(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["statusLine"]["_rimz_wrapped"], "echo hi");
    // The feed command reads the bare string back as the pass-through target.
    let root = read_existing_json(&path).unwrap();
    assert_eq!(
        wrapped_status_line_command_from(&root, &STATUS_LINE).as_deref(),
        Some("echo hi")
    );
    uninstall_from(&path).unwrap();
    let restored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(restored["statusLine"], "echo hi");
}

#[test]
fn classify_status_line_change_reports_each_case() {
    let none = Map::new();
    assert_eq!(
        classify_status_line_change(&none, &STATUS_LINE),
        StatusLineChange::Added
    );

    let user: Map<String, Value> = serde_json::from_str(
        r#"{ "statusLine": { "type": "command", "command": "npx ccstatusline" } }"#,
    )
    .unwrap();
    assert_eq!(
        classify_status_line_change(&user, &STATUS_LINE),
        StatusLineChange::Wrapping {
            original: "npx ccstatusline".to_owned()
        }
    );

    let mut managed = Map::new();
    upsert_rimz_status_line(&mut managed, &STATUS_LINE);
    assert_eq!(
        classify_status_line_change(&managed, &STATUS_LINE),
        StatusLineChange::Unchanged
    );
}
