use super::*;

#[test]
fn install_into_empty_dir_creates_managed_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    assert!(
        !hooks_installed_at(&path),
        "a missing settings file reads as not installed"
    );
    let report = install_into(&path).unwrap();
    assert!(!report.merged);
    assert_eq!(report.agent, "claude");
    assert!(report.installed_events.contains(&"SessionStart".to_owned()));
    assert!(report.installed_events.contains(&"PreToolUse".to_owned()));
    assert!(
        report
            .installed_events
            .contains(&"PermissionRequest".to_owned())
    );
    assert!(hooks_installed_at(&path));

    assert_managed_settings_json(&path);

    let first = std::fs::read_to_string(&path).unwrap();
    install_into(&path).unwrap();
    let second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        first, second,
        "second install must produce identical config"
    );
}

fn assert_managed_settings_json(path: &std::path::Path) {
    let parsed: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let top_keys = parsed.as_object().unwrap().keys().collect::<Vec<_>>();
    assert_eq!(top_keys, vec!["hooks", "statusLine", "subagentStatusLine"]);
    assert_managed_hook_entries(parsed["hooks"].as_object().unwrap());
    assert_status_command(&parsed, STATUS_LINE.key, STATUS_LINE.command);
    assert_status_command(
        &parsed,
        SUBAGENT_STATUS_LINE.key,
        SUBAGENT_STATUS_LINE.command,
    );
}

fn assert_managed_hook_entries(hooks: &serde_json::Map<String, Value>) {
    let expected = INSTALLED_EVENTS
        .iter()
        .map(|(event, _)| *event)
        .collect::<std::collections::BTreeSet<_>>();
    let actual = hooks
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(actual, expected);
    for (event, matcher) in INSTALLED_EVENTS {
        let entries = hooks[*event].as_array().unwrap();
        assert_eq!(entries.len(), 1, "event {event}");
        assert_managed_hook_entry(&entries[0], event, *matcher);
    }
}

fn assert_managed_hook_entry(entry: &Value, event: &str, matcher: Option<&str>) {
    assert_eq!(entry.get("matcher").and_then(Value::as_str), matcher);
    assert_eq!(entry["_rimz_managed"], true);
    assert_eq!(entry["_rimz_sync"], blocking_event_sync(event, matcher));
    let commands = entry["hooks"].as_array().unwrap();
    assert_eq!(commands.len(), 1, "event {event}");
    assert_eq!(commands[0]["type"], "command");
    assert_eq!(commands[0]["command"], RIMZ_HOOK_COMMAND);
    assert_eq!(commands[0]["timeout"], CLAUDE_HOOK_TIMEOUT_SECS);
}

fn blocking_event_sync(event: &str, matcher: Option<&str>) -> bool {
    BLOCKING_EVENTS
        .iter()
        .any(|(blocking, blocking_matcher)| *blocking == event && *blocking_matcher == matcher)
}

fn assert_status_command(root: &Value, key: &str, command: &str) {
    assert_eq!(root[key]["_rimz_managed"], true);
    assert!(
        root[key].get("_rimz_wrapped").is_none(),
        "{key} should not mark an empty install as wrapping a user command"
    );
    assert_eq!(root[key]["command"], command);
    assert_eq!(root[key]["type"], "command");
}

#[test]
fn install_preserves_user_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{
              "model": "claude-opus-4-7",
              "hooks": {
                "PreToolUse": [
                  { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ],
                "UserPromptSubmit": [
                  { "hooks": [{ "type": "command", "command": "echo prompt" }] }
                ]
              }
            }"#,
    )
    .unwrap();
    let report = install_into(&path).unwrap();
    assert!(report.merged);

    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["model"], "claude-opus-4-7");
    let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
    // user `Bash` matcher + 1 rimz broad per-tool hook (no matcher).
    assert_eq!(pre_tool.len(), 2);
    assert!(
        pre_tool.iter().any(|e| e["matcher"] == "Bash"
            && e.get("_rimz_managed").and_then(Value::as_bool) != Some(true))
    );
    // UserPromptSubmit is state signal, so a default install wires it. The
    // user's own UserPromptSubmit hook is preserved alongside ours.
    let ups = parsed["hooks"]["UserPromptSubmit"].as_array().unwrap();
    assert_eq!(ups.len(), 2);
    assert!(
        ups.iter()
            .any(|e| e.get("_rimz_managed").and_then(Value::as_bool) != Some(true))
    );
    assert!(
        ups.iter()
            .any(|e| e.get("_rimz_managed").and_then(Value::as_bool) == Some(true))
    );
}

#[test]
fn install_reclaims_legacy_and_duplicate_entries() {
    // Reproduces a bloated real-world file: legacy *unmarked* rimz copies
    // (older builds wrote `--event` and no marker) stacked alongside an old
    // separate-matcher managed entry, plus a genuine user hook. Install must
    // reclaim every rimz-owned entry — marked or not — and leave exactly the
    // canonical set, with the user hook untouched.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
            &path,
            r#"{
              "hooks": {
                "Notification": [
                  { "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event Notification" }] },
                  { "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event Notification" }] }
                ],
                "PreToolUse": [
                  { "matcher": "ExitPlanMode", "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event PreToolUse" }] },
                  { "matcher": "AskUserQuestion", "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event PreToolUse" }] },
                  { "_rimz_managed": true, "_rimz_sync": true, "matcher": "ExitPlanMode", "hooks": [{ "type": "command", "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude --event PreToolUse" }] },
                  { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ]
              }
            }"#,
        )
        .unwrap();
    install_into(&path).unwrap();

    let written = std::fs::read_to_string(&path).unwrap();
    assert!(
        !written.contains("--event"),
        "every legacy `--event` command must be reclaimed: {written}"
    );

    let parsed: Value = serde_json::from_slice(written.as_bytes()).unwrap();
    let managed = |entry: &Value| entry.get("_rimz_managed").and_then(Value::as_bool) == Some(true);

    // Two stacked legacy copies collapse to one managed Notification hook.
    let notif = parsed["hooks"]["Notification"].as_array().unwrap();
    assert_eq!(notif.len(), 1);
    assert!(managed(&notif[0]));

    // PreToolUse: the user `Bash` hook survives; the two unmarked legacy
    // matchers and the old separate managed matcher are all reclaimed,
    // replaced by the single broad hook.
    let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre_tool.len(), 2);
    assert!(
        pre_tool
            .iter()
            .any(|e| e["matcher"] == "Bash" && !managed(e)),
        "user Bash hook preserved"
    );
    assert!(
        pre_tool.iter().any(|e| managed(e)
            && !e.as_object().unwrap().contains_key("matcher")
            && e["_rimz_sync"] == false),
        "broad enrichment hook present"
    );
    // Exactly the one canonical rimz entry — no stale duplicates.
    assert_eq!(pre_tool.iter().filter(|e| managed(e)).count(), 1);
}

#[test]
fn uninstall_removes_managed_entries_only() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing-settings.json");
    let missing_report = uninstall_from(&missing).unwrap();
    assert!(!missing_report.existed);
    assert!(missing_report.removed_events.is_empty());

    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{
              "model": "claude-opus-4-7",
              "hooks": {
                "PreToolUse": [
                  { "matcher": "Bash", "hooks": [{ "type": "command", "command": "echo hi" }] }
                ]
              }
            }"#,
    )
    .unwrap();
    install_into(&path).unwrap();
    let report = uninstall_from(&path).unwrap();
    assert!(report.existed);
    assert!(!report.removed_events.is_empty());
    assert!(!hooks_installed_at(&path));

    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["model"], "claude-opus-4-7");
    let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre_tool.len(), 1);
    assert_eq!(pre_tool[0]["matcher"], "Bash");
    // PermissionRequest was rimz-only — entire key removed when empty.
    assert!(parsed["hooks"].get("PermissionRequest").is_none());
}

#[test]
fn hooks_installed_at_accepts_command_marker_and_rejects_stale_or_user_only_configs() {
    let dir = tempfile::tempdir().unwrap();

    let path = dir.path().join("partial.json");
    install_into(&path).unwrap();
    let mut parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    parsed["hooks"]
        .as_object_mut()
        .unwrap()
        .remove("PostCompact");
    std::fs::write(&path, serde_json::to_string(&parsed).unwrap()).unwrap();

    assert!(
        !hooks_installed_at(&path),
        "a partial managed hook set must re-offer install"
    );

    let path = dir.path().join("async.json");
    install_into(&path).unwrap();
    let mut parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    parsed["hooks"]["PermissionRequest"][0]["_rimz_sync"] = Value::Bool(false);
    std::fs::write(&path, serde_json::to_string(&parsed).unwrap()).unwrap();

    assert!(
        !hooks_installed_at(&path),
        "a blocking Rimz hook marked async is not a usable install"
    );
    assert!(matches!(
        install_into(&path).unwrap_err(),
        AgentErr::Install {
            agent: "claude",
            ..
        }
    ));

    let path = dir.path().join("user-only.json");
    std::fs::write(
        &path,
        r#"{ "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [] } ] } }"#,
    )
    .unwrap();
    assert!(
        !hooks_installed_at(&path),
        "user-managed hooks with no _rimz_managed marker are not installed"
    );

    // Simulate a settings.json where an external tool (e.g. Claude Code
    // auto-migration) preserved the hook command but stripped _rimz_managed.
    // Detection must still succeed so the consent gate does not re-fire.
    let path = dir.path().join("marker-only.json");
    let command = format!(r#"RIMZ_AGENT_PID=$PPID exec {RIMZ_HOOK_MARKER}"#);
    let mut hooks = serde_json::Map::new();
    for (event, _) in INSTALLED_EVENTS {
        hooks.insert(
            (*event).to_owned(),
            serde_json::json!([
                {
                    "hooks": [{"type": "command", "command": command.clone()}]
                }
            ]),
        );
    }
    let payload = serde_json::json!({ "hooks": hooks });
    std::fs::write(&path, serde_json::to_string(&payload).unwrap()).unwrap();
    assert!(
        hooks_installed_at(&path),
        "a hook entry whose command contains the rimz marker reads as installed even without _rimz_managed"
    );

    let path = dir.path().join("legacy-matcher.json");
    let mut hooks = serde_json::Map::new();
    for (event, matcher) in INSTALLED_EVENTS {
        let mut entry = serde_json::json!({
            "_rimz_managed": true,
            "hooks": [{"type": "command", "command": command.clone()}],
        });
        let on_disk_matcher = if *event == "PreToolUse" {
            Some("ExitPlanMode")
        } else {
            *matcher
        };
        if let Some(matcher) = on_disk_matcher {
            entry
                .as_object_mut()
                .unwrap()
                .insert("matcher".to_owned(), Value::String(matcher.to_owned()));
        }
        hooks.insert((*event).to_owned(), serde_json::json!([entry]));
    }
    let payload = serde_json::json!({ "hooks": hooks });
    std::fs::write(&path, serde_json::to_string(&payload).unwrap()).unwrap();

    assert!(
        !hooks_installed_at(&path),
        "a legacy managed PreToolUse matcher must not satisfy the broad canonical hook"
    );
}

#[test]
fn install_rejects_top_level_non_object() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, "[]").unwrap();
    let err = install_into(&path).unwrap_err();
    assert!(matches!(
        err,
        AgentErr::Install {
            agent: "claude",
            ..
        }
    ));
}
