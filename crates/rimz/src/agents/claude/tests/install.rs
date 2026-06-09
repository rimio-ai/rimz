use super::*;

#[test]
fn install_into_empty_dir_creates_managed_entries() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
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

    // Lock the full on-disk shape: event set, sync flags, command strings,
    // and the 120 s blocking-hook timeout. Every command is identical (no
    // `--event`; the helper reads the event from stdin), and every event
    // installs as a single broad hook with no matcher — `PreToolUse`
    // self-classifies its blocking sub-events from `tool_name`. The file is
    // deterministic, so the whole settings.json snapshots cleanly.
    let written = std::fs::read_to_string(&path).unwrap();
    insta::assert_snapshot!(written, @r###"
        {
          "hooks": {
            "Notification": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "PermissionRequest": [
              {
                "_rimz_managed": true,
                "_rimz_sync": true,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "PostCompact": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "PostToolUse": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "PreCompact": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "PreToolUse": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "SessionEnd": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "SessionStart": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "Stop": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "StopFailure": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "SubagentStart": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "SubagentStop": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ],
            "UserPromptSubmit": [
              {
                "_rimz_managed": true,
                "_rimz_sync": false,
                "hooks": [
                  {
                    "command": "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source claude",
                    "timeout": 120,
                    "type": "command"
                  }
                ]
              }
            ]
          },
          "statusLine": {
            "_rimz_managed": true,
            "command": "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source claude",
            "type": "command"
          },
          "subagentStatusLine": {
            "_rimz_managed": true,
            "command": "RIMZ_AGENT_PID=$PPID exec rimz statusline feed --source claude --subagent",
            "type": "command"
          }
        }
        "###);
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
fn install_wires_non_blocking_per_tool_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let report = install_into(&path).unwrap();
    assert!(report.installed_events.contains(&"PreToolUse".to_owned()));
    assert!(report.installed_events.contains(&"PostToolUse".to_owned()));

    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
    // Exactly 1 broad per-tool hook (no matcher); the blocking sub-events
    // self-classify off it rather than getting a dedicated matcher entry.
    assert_eq!(pre_tool.len(), 1);
    // The broad per-tool hook has no matcher key and is non-blocking.
    let broad = pre_tool
        .iter()
        .find(|e| !e.as_object().unwrap().contains_key("matcher"))
        .unwrap();
    assert_eq!(broad["_rimz_managed"], true);
    assert_eq!(broad["_rimz_sync"], false);
}

#[test]
fn install_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    install_into(&path).unwrap();
    let first = std::fs::read_to_string(&path).unwrap();
    install_into(&path).unwrap();
    let second = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        first, second,
        "second install must produce identical config"
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

    let parsed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(parsed["model"], "claude-opus-4-7");
    let pre_tool = parsed["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre_tool.len(), 1);
    assert_eq!(pre_tool[0]["matcher"], "Bash");
    // PermissionRequest was rimz-only — entire key removed when empty.
    assert!(parsed["hooks"].get("PermissionRequest").is_none());
}

#[test]
fn uninstall_on_missing_file_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let report = uninstall_from(&path).unwrap();
    assert!(!report.existed);
    assert!(report.removed_events.is_empty());
}

#[test]
fn hooks_installed_at_detects_managed_matcher() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    assert!(
        !hooks_installed_at(&path),
        "a missing settings file reads as not installed"
    );
    install_into(&path).unwrap();
    assert!(
        hooks_installed_at(&path),
        "an installed settings file reads as installed"
    );
    uninstall_from(&path).unwrap();
    assert!(
        !hooks_installed_at(&path),
        "an uninstalled settings file reads as not installed"
    );
}

#[test]
fn hooks_installed_at_ignores_user_only_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{ "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [] } ] } }"#,
    )
    .unwrap();
    assert!(
        !hooks_installed_at(&path),
        "user-managed hooks with no _rimz_managed marker are not installed"
    );
}

#[test]
fn hooks_installed_at_detects_by_command_marker_without_rimz_managed() {
    // Simulate a settings.json where an external tool (e.g. Claude Code
    // auto-migration) preserved the hook command but stripped _rimz_managed.
    // Detection must still succeed so the consent gate does not re-fire.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    let command = format!(r#"RIMZ_AGENT_PID=$PPID exec {RIMZ_HOOK_MARKER}"#);
    let payload = serde_json::json!({
        "hooks": {
            "SessionStart": [
                {
                    "hooks": [{"type": "command", "command": command}]
                }
            ]
        }
    });
    std::fs::write(&path, serde_json::to_string(&payload).unwrap()).unwrap();
    assert!(
        hooks_installed_at(&path),
        "a hook entry whose command contains the rimz marker reads as installed even without _rimz_managed"
    );
}

#[test]
fn install_rejects_async_blocking_marker() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    // A tampered config marks a rimz-managed PermissionRequest matcher
    // with `_rimz_sync = false`. The installer must refuse — the source
    // of truth for "must block" is BLOCKING_EVENTS, never the file.
    std::fs::write(
        &path,
        r#"{
              "hooks": {
                "PermissionRequest": [
                  {
                    "_rimz_managed": true,
                    "_rimz_sync": false,
                    "hooks": [{ "type": "command", "command": "x" }]
                  }
                ]
              }
            }"#,
    )
    .unwrap();
    let err = install_into(&path).unwrap_err();
    assert!(matches!(
        err,
        AgentErr::Install {
            agent: "claude",
            ..
        }
    ));
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
