use super::*;

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
    MANAGED_SOURCE.install_into(&path).unwrap();
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

    MANAGED_SOURCE.uninstall_from(&path).unwrap();
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
fn install_wraps_and_restores_user_status_line() {
    // A command-object statusline (a real ccstatusline config) carries rendering
    // options alongside the command. The siblings ride the managed object so the
    // wrap stays visually faithful while installed, and the whole original — read
    // back as the pass-through target — restores verbatim on uninstall.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
            &path,
            r#"{ "statusLine": { "type": "command", "command": "npx -y ccstatusline@latest", "padding": 0, "refreshInterval": 10 } }"#,
        )
        .unwrap();
    MANAGED_SOURCE.install_into(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
    assert_eq!(parsed["statusLine"]["padding"], 0);
    assert_eq!(parsed["statusLine"]["refreshInterval"], 10);
    assert_eq!(parsed["statusLine"]["_rimz_wrapped"]["refreshInterval"], 10);
    let root = read_existing_json(&path).unwrap();
    assert_eq!(
        wrapped_status_line_command_from(&root, &STATUS_LINE).as_deref(),
        Some("npx -y ccstatusline@latest")
    );
    MANAGED_SOURCE.uninstall_from(&path).unwrap();
    let restored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        restored["statusLine"]["command"],
        "npx -y ccstatusline@latest"
    );
    assert_eq!(restored["statusLine"]["refreshInterval"], 10);
    assert!(restored["statusLine"].get("_rimz_managed").is_none());

    // A bare-string statusline is captured whole the same way and restored.
    std::fs::write(&path, r#"{ "statusLine": "echo hi" }"#).unwrap();
    MANAGED_SOURCE.install_into(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["statusLine"]["_rimz_wrapped"], "echo hi");
    let root = read_existing_json(&path).unwrap();
    assert_eq!(
        wrapped_status_line_command_from(&root, &STATUS_LINE).as_deref(),
        Some("echo hi")
    );
    MANAGED_SOURCE.uninstall_from(&path).unwrap();
    let restored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(restored["statusLine"], "echo hi");
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
    MANAGED_SOURCE.install_into(&path).unwrap();
    let first = std::fs::read_to_string(&path).unwrap();
    MANAGED_SOURCE.install_into(&path).unwrap();
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
fn recursive_status_line_wrap_is_repaired_on_install_and_dropped_on_uninstall() {
    // A `_rimz_wrapped` that itself holds the Rimz command is a recursive wrap
    // (a prior bug's residue): never a user command. Install discards the inner
    // command but keeps the sibling rendering options; uninstall restores
    // nothing rather than re-installing Rimz's own command.
    let dir = tempfile::tempdir().unwrap();
    let recursive = |extra: serde_json::Value| {
        let mut wrapped = serde_json::Map::new();
        wrapped.insert("type".into(), json!("command"));
        wrapped.insert("command".into(), json!(STATUS_LINE_COMMAND));
        let mut managed = wrapped.clone();
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                wrapped.insert(k.clone(), v.clone());
                managed.insert(k.clone(), v.clone());
            }
        }
        managed.insert("_rimz_managed".into(), json!(true));
        managed.insert("_rimz_wrapped".into(), Value::Object(wrapped));
        json!({ "statusLine": managed })
    };

    let install_path = dir.path().join("install.json");
    std::fs::write(
        &install_path,
        serde_json::to_string(&recursive(json!({ "padding": 0, "refreshInterval": 10 }))).unwrap(),
    )
    .unwrap();
    MANAGED_SOURCE.install_into(&install_path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&install_path).unwrap()).unwrap();
    assert_eq!(parsed["statusLine"]["command"], STATUS_LINE_COMMAND);
    assert!(
        parsed["statusLine"].get("_rimz_wrapped").is_none(),
        "a Rimz statusline command is not a user command to wrap"
    );
    assert_eq!(parsed["statusLine"]["padding"], 0);
    assert_eq!(parsed["statusLine"]["refreshInterval"], 10);

    let uninstall_path = dir.path().join("uninstall.json");
    std::fs::write(
        &uninstall_path,
        serde_json::to_string(&recursive(json!({}))).unwrap(),
    )
    .unwrap();
    MANAGED_SOURCE.uninstall_from(&uninstall_path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&uninstall_path).unwrap()).unwrap();
    assert!(
        parsed.get("statusLine").is_none(),
        "uninstall must not restore Rimz's own statusline command"
    );
}

#[test]
fn uninstall_removes_status_line_when_none_existed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    MANAGED_SOURCE.install_into(&path).unwrap();
    MANAGED_SOURCE.uninstall_from(&path).unwrap();
    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(
        parsed.get("statusLine").is_none(),
        "a Rimz-added statusLine is removed on uninstall"
    );
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
