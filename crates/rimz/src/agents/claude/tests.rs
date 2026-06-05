use super::*;
use crate::agents::AgentHookClass;
use crate::feed::ResolutionMethod;
use serde_json::json;
use std::path::Path;

#[test]
fn resume_command_is_claude_resume_with_the_session_id() {
    let argv = ClaudeAdapter
        .resume_command("sess-123", Path::new("/code/query-engine"))
        .expect("claude resumes");
    assert_eq!(argv, vec!["claude", "--resume", "sess-123"]);
}

fn fixture(kind: FeedKind) -> FeedItem {
    crate::agents::testkit::feed_item(kind, "claude")
}

#[test]
fn permission_allow_shape_is_pinned() {
    let item = fixture(FeedKind::Permission);
    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    let rendered = ClaudeAdapter.render_decision(&item, &resolution).unwrap();
    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "allow"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);
    assert_eq!(
        rendered["hookSpecificOutput"]["decision"]["behavior"],
        "allow"
    );
    assert_eq!(
        rendered["hookSpecificOutput"]["hookEventName"],
        "PermissionRequest"
    );
}

#[test]
fn plan_approval_requires_updated_input() {
    let item = fixture(FeedKind::PlanApproval);
    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    let err = ClaudeAdapter
        .render_decision(&item, &resolution)
        .unwrap_err();
    assert!(matches!(
        err,
        AgentErr::MissingField {
            agent: "claude",
            field: "updatedInput"
        }
    ));
}

#[test]
fn neutral_payload_is_empty_stdout() {
    let value = ClaudeAdapter.render_neutral("PermissionRequest").unwrap();
    insta::assert_snapshot!(
        serde_json::to_string(&value).unwrap(),
        @"null"
    );
    assert_eq!(value, None);
}

#[test]
fn permission_deny_shape_is_pinned() {
    let item = fixture(FeedKind::Permission);
    let resolution = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    let rendered = ClaudeAdapter.render_decision(&item, &resolution).unwrap();

    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "decision": {
              "behavior": "deny"
            },
            "hookEventName": "PermissionRequest"
          }
        }
        "###);
}

#[test]
fn plan_approval_allow_shape_is_pinned() {
    let item = fixture(FeedKind::PlanApproval);
    let resolution = Resolution::new(
        json!({ "choice": "allow", "updatedInput": "ship the plan" }),
        ResolutionMethod::HookBridge,
    );
    let rendered = ClaudeAdapter.render_decision(&item, &resolution).unwrap();

    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": "ship the plan"
          }
        }
        "###);
}

#[test]
fn ask_user_question_allow_shape_carries_updated_input_object() {
    let item = fixture(FeedKind::Question);
    let resolution = Resolution::new(
        json!({ "choice": "allow", "updatedInput": { "question": "ready?" } }),
        ResolutionMethod::HookBridge,
    );
    let rendered = ClaudeAdapter.render_decision(&item, &resolution).unwrap();

    insta::assert_json_snapshot!(rendered, @r###"
        {
          "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": {
              "question": "ready?"
            }
          }
        }
        "###);
}

#[test]
fn classify_pretooluse_exit_plan_mode_is_plan_approval() {
    let c = ClaudeAdapter.classify_hook("PreToolUse", &json!({ "tool_name": "ExitPlanMode" }));
    assert_eq!(c.class, AgentHookClass::BlockingFeed);
    assert_eq!(c.feed_kind, Some(FeedKind::PlanApproval));
}

#[test]
fn classify_pretooluse_ask_user_question_is_question() {
    let c = ClaudeAdapter.classify_hook("PreToolUse", &json!({ "tool_name": "AskUserQuestion" }));
    assert_eq!(c.class, AgentHookClass::BlockingFeed);
    assert_eq!(c.feed_kind, Some(FeedKind::Question));
}

#[test]
fn classify_subagent_events_are_lifecycle() {
    for event in ["SubagentStart", "SubagentStop"] {
        let c = ClaudeAdapter.classify_hook(event, &json!({}));
        assert_eq!(c.class, AgentHookClass::Lifecycle, "{event}");
        assert_eq!(c.feed_kind, None, "{event}");
    }
}

#[test]
fn pre_compact_is_a_lifecycle_compaction_marker() {
    let c = ClaudeAdapter.classify_hook("PreCompact", &json!({ "session_id": "sess-1" }));
    assert_eq!(c.class, AgentHookClass::Lifecycle);
    assert_eq!(c.feed_kind, None);
    let obs = ClaudeAdapter
        .observe_lifecycle("PreCompact", &json!({ "session_id": "sess-1" }))
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    // It carries the compaction signal; the reducer keeps the prior status
    // and only stamps the compacting head, never a false transition.
    assert_eq!(obs.signal, LifecycleSignal::Compacting);
}

#[test]
fn subagent_start_observes_running_child_keyed_by_agent_id() {
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SubagentStart",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "subagent_type": "Explore",
                "description": "search the ledger",
                "permission_mode": "acceptEdits",
            }),
        )
        .unwrap();

    // Keyed off the child's own id, not the parent session.
    assert_eq!(obs.agent_id.as_deref(), Some("child-1"));
    assert_eq!(obs.signal, LifecycleSignal::SubagentStarted);
    // The type labels the child row; `session_id` is captured as the parent.
    assert_eq!(obs.task.as_deref(), Some("Explore"));
    assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
}

#[test]
fn subagent_stop_returns_child_idle_keeping_its_label() {
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SubagentStop",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-1",
                "agent_type": "Explore",
            }),
        )
        .unwrap();

    assert_eq!(obs.agent_id.as_deref(), Some("child-1"));
    assert_eq!(obs.signal, LifecycleSignal::SubagentStopped);
    // The label persists past stop; the parent link survives.
    assert_eq!(obs.task.as_deref(), Some("Explore"));
    assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
}

#[test]
fn root_lifecycle_event_carries_no_parent() {
    let obs = ClaudeAdapter
        .observe_lifecycle("UserPromptSubmit", &json!({ "session_id": "sess-root" }))
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-root"));
    assert_eq!(obs.parent_agent_id, None);
}

#[test]
fn subagent_event_without_child_id_is_quarantined() {
    // A SubagentStart that carries only the parent `session_id` (no distinct
    // child `agent_id`) must produce no observation — it can never fold onto
    // the parent's row and rename it to the subagent type. This is the
    // "main row becomes Explore" regression.
    let obs = ClaudeAdapter.observe_lifecycle(
        "SubagentStart",
        &json!({ "session_id": "sess-parent", "subagent_type": "Explore" }),
    );
    assert!(
        obs.is_none(),
        "a child with no distinct id is dropped, not folded onto the parent"
    );
}

#[test]
fn foreign_child_mutating_post_tool_use_is_dropped() {
    // Claude stamps `agent_id` on every payload fired inside a subagent, so
    // a backgrounded child's mutating tool must not fold onto the parent's
    // rollup — it would advance the parent's `last_activity` past a pending
    // native_ui ask and un-fold its `waiting` row while still blocked. The
    // child-keyed activity heartbeat carries this progress instead.
    let obs = ClaudeAdapter.observe_lifecycle(
        "PostToolUse",
        &json!({
            "session_id": "sess-parent",
            "agent_id": "child-1",
            "tool_name": "Edit",
        }),
    );
    assert!(
        obs.is_none(),
        "a backgrounded child's mutating tool must not fold onto the parent"
    );
}

#[test]
fn foreign_child_pre_compact_is_dropped() {
    // An in-subagent compaction must not stamp the *parent's* compacting
    // head — same foreign-id family as the per-tool drop.
    let obs = ClaudeAdapter.observe_lifecycle(
        "PreCompact",
        &json!({ "session_id": "sess-parent", "agent_id": "child-1" }),
    );
    assert!(obs.is_none(), "a child's compaction never marks the parent");
}

#[test]
fn root_event_with_agent_id_equal_to_session_id_is_root() {
    // A session-equal `agent_id` is the main thread, not a child — a normal
    // root observation, never dropped.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({
                "session_id": "sess-1",
                "agent_id": "sess-1",
                "tool_name": "Edit",
            }),
        )
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(obs.parent_agent_id, None);
}

#[test]
fn harness_control_prompt_is_not_adopted_as_description() {
    // The harness injects synthetic user turns (a completed background task);
    // their raw text must never become the agent's description line.
    let obs = ClaudeAdapter
            .observe_lifecycle(
                "UserPromptSubmit",
                &json!({
                    "session_id": "sess-1",
                    "prompt": "<task-notification><task-id>afdc639e18e7ebdb9</task-id></task-notification>",
                }),
            )
            .unwrap();
    assert_eq!(obs.prompt, None, "control text is rejected, not shown");
    assert_eq!(obs.task, None);
}

#[test]
fn post_tool_use_rides_lifecycle_only_for_mutating_tools() {
    // A mutating tool proves real work, so it records a `ToolUsed` signal;
    // a read-only tool stays silent so the lifecycle channel isn't flooded.
    // A file edit also sets the `edits` bit (ends the thinking head); a
    // shell command mutates without editing.
    let edit = ClaudeAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({ "session_id": "sess-1", "tool_name": "Edit" }),
        )
        .unwrap();
    assert_eq!(
        edit.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
        }
    );
    let shell = ClaudeAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({ "session_id": "sess-1", "tool_name": "Bash" }),
        )
        .unwrap();
    assert_eq!(
        shell.signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
        }
    );
    let read = ClaudeAdapter.observe_lifecycle(
        "PostToolUse",
        &json!({ "session_id": "sess-1", "tool_name": "Read" }),
    );
    assert!(read.is_none(), "a read-only tool stays silent");
}

#[test]
fn hook_cap_is_120_seconds() {
    assert_eq!(
        ClaudeAdapter.descriptor().hook_cap,
        Duration::from_secs(120)
    );
}

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

#[test]
fn session_start_observes_idle_status() {
    let obs = ClaudeAdapter
        .observe_lifecycle("SessionStart", &json!({ "session_id": "sess-1" }))
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    // Wired in, nothing asked yet — registered, no task.
    assert_eq!(obs.signal, LifecycleSignal::Registered);
    assert_eq!(obs.task, None);
}

#[test]
fn user_prompt_submit_observes_running_with_prompt_task() {
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "UserPromptSubmit",
            &json!({ "session_id": "sess-1", "prompt": "fix auth flow" }),
        )
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(obs.signal, LifecycleSignal::TurnStarted);
    assert_eq!(obs.task.as_deref(), Some("fix auth flow"));
}

#[test]
fn todo_write_payload_extracts_progress() {
    // Claude TodoWrite hooks expose the todo list in `tool_input.todos`;
    // the reducer projects the count of completed items onto the row.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "UserPromptSubmit",
            &json!({
                "session_id": "sess-1",
                "tool_input": {
                    "todos": [
                        { "status": "completed" },
                        { "status": "completed" },
                        { "status": "in_progress" },
                        { "status": "pending" },
                    ]
                }
            }),
        )
        .unwrap();
    assert_eq!(obs.todo_done, Some(2));
    assert_eq!(obs.todo_total, Some(4));
}

#[test]
fn notification_event_is_not_a_lifecycle_observation() {
    let obs = ClaudeAdapter.observe_lifecycle("Notification", &json!({}));
    assert!(obs.is_none());
}

#[test]
fn clean_stop_observes_success() {
    // A Stop fires only after a turn ran; a clean end completes it.
    let obs = ClaudeAdapter
        .observe_lifecycle("Stop", &json!({ "session_id": "sess-1" }))
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    );
    // Turn over: the task clears back to "—".
    assert_eq!(obs.task, None);
}

#[test]
fn errored_stop_observes_failed() {
    let obs = ClaudeAdapter
        .observe_lifecycle("Stop", &json!({ "session_id": "sess-1", "is_error": true }))
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        }
    );
}

#[test]
fn stop_with_pending_background_tasks_observes_running() {
    // Claude Code v2.1.145+ reports in-flight `background_tasks` on Stop.
    // The main thread has parked waiting for that work — the turn is not
    // over, so the signal flags `parked_on_background` (the reducer keeps it
    // running) and the description is left to the real task/prompt, never a
    // synthetic background-task label.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "background_tasks": [
                    {
                        "id": "task-1",
                        "type": "command",
                        "command": "npm run build",
                        "status": "running",
                        "description": "Build process"
                    }
                ]
            }),
        )
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
        }
    );
    assert_eq!(obs.task, None);
}

#[test]
fn stop_with_multiple_pending_background_tasks_still_parks() {
    // Several in-flight tasks still just park the turn — there is no
    // synthetic "N background tasks" label overwriting the description.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "background_tasks": [
                    { "id": "a", "status": "running", "description": "lint" },
                    { "id": "b", "status": "running", "description": "test" }
                ]
            }),
        )
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
        }
    );
    assert_eq!(obs.task, None);
}

#[test]
fn stop_with_only_completed_background_tasks_observes_success() {
    // A registry that reports only terminal tasks has nothing in flight —
    // this is a genuine turn end, so the signal is a clean (unparked) end.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "background_tasks": [
                    { "id": "task-1", "status": "completed", "description": "Build process" }
                ]
            }),
        )
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    );
    assert_eq!(obs.task, None);
}

#[test]
fn errored_stop_with_pending_background_tasks_still_observes_failed() {
    // The failure is the attention signal: the signal carries `errored`
    // alongside the park flag, and `step` resolves errored over the park.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "is_error": true,
                "background_tasks": [
                    { "id": "task-1", "status": "running", "description": "Build process" }
                ]
            }),
        )
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: true,
        }
    );
}

#[test]
fn transcript_tail_populates_context_gauge() {
    // Claude reports token usage only in the transcript JSONL; the Stop hook
    // reads its tail to fill the context gauge. 100k of a 200k window = 50%.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("session.jsonl");
    std::fs::write(
            &transcript,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-7\",\"usage\":{\"input_tokens\":100000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":500}}}\n",
        )
        .unwrap();
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "transcript_path": transcript.to_str().unwrap(),
            }),
        )
        .unwrap();
    assert_eq!(obs.context_pct, Some(50));
    assert_eq!(obs.total_tokens, Some(100_500));
    assert_eq!(obs.model.as_deref(), Some("claude-opus-4-7"));
}

#[test]
fn payload_one_million_marker_widens_the_context_window() {
    // The 1M beta is signalled by a `[1m]` marker that rides only the hook
    // payload's model field — the transcript writes the bare id. The gauge
    // must divide by the payload-resolved window: 100k of 1M = 10%, where
    // the bare-id default would have over-read it as 50%.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("session.jsonl");
    std::fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"model\":\"claude-opus-4-8\",\"usage\":{\"input_tokens\":100000,\"cache_read_input_tokens\":0,\"cache_creation_input_tokens\":0,\"output_tokens\":500}}}\n",
        )
        .unwrap();
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "model": "claude-opus-4-8[1m]",
                "transcript_path": transcript.to_str().unwrap(),
            }),
        )
        .unwrap();
    assert_eq!(obs.context_pct, Some(10));
    assert_eq!(obs.total_tokens, Some(100_500));
    assert_eq!(obs.model.as_deref(), Some("claude-opus-4-8[1m]"));
}

#[test]
fn observe_turn_error_reads_the_tail_from_the_payload_path() {
    // End-to-end over the real file path: the statusline payload names the
    // transcript, the adapter reads its bounded tail, and the verified
    // incident shape (flagged assistant entry + turn_duration, no Stop)
    // yields the marker. A missing path or file yields None, never an error.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("session.jsonl");
    std::fs::write(
            &transcript,
            concat!(
                "{\"type\":\"assistant\",\"isApiErrorMessage\":true,\"timestamp\":\"2026-06-04T02:56:32.919Z\",",
                "\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"API Error: Overloaded\"}]}}\n",
                "{\"type\":\"system\",\"subtype\":\"turn_duration\",\"timestamp\":\"2026-06-04T02:56:32.923Z\"}\n",
            ),
        )
        .unwrap();
    let error = ClaudeAdapter
        .observe_turn_error(&json!({
            "session_id": "sess-1",
            "transcript_path": transcript.to_str().unwrap(),
        }))
        .expect("the dead turn is detected");
    assert_eq!(error.label.as_deref(), Some("API Error: Overloaded"));

    assert!(
        ClaudeAdapter
            .observe_turn_error(&json!({ "session_id": "sess-1" }))
            .is_none(),
        "no transcript path, no marker"
    );
    assert!(
        ClaudeAdapter
            .observe_turn_error(&json!({
                "session_id": "sess-1",
                "transcript_path": dir.path().join("gone.jsonl").to_str().unwrap(),
            }))
            .is_none(),
        "an unreadable transcript degrades to no marker"
    );
}

#[test]
fn fresh_transcript_reports_zero_context_not_unknown() {
    // A brand-new session has a transcript with no assistant usage yet. It
    // must read as 0% (empty gauge), not None (no gauge), so a just-launched
    // idle agent shows an empty context bar.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        "{\"type\":\"user\",\"message\":{\"role\":\"user\"}}\n",
    )
    .unwrap();
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({
                "session_id": "sess-1",
                "transcript_path": transcript.to_str().unwrap(),
            }),
        )
        .unwrap();
    assert_eq!(obs.context_pct, Some(0));
    assert_eq!(obs.total_tokens, Some(0));
}

#[test]
fn missing_transcript_leaves_context_unknown() {
    // No readable transcript means unknown, not zero — the gauge stays
    // hidden rather than asserting a false 0%.
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({
                "session_id": "sess-1",
                "transcript_path": "/nonexistent/path/session.jsonl",
            }),
        )
        .unwrap();
    assert_eq!(obs.context_pct, None);
    assert_eq!(obs.total_tokens, None);
}

#[test]
fn transcript_requires_session_id() {
    // Transcript reads are keyed by the agent's own session identity. A
    // transcript path without a session id stays unknown; the sidebar row
    // projection is responsible for the visible 0% baseline.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("session.jsonl");
    std::fs::write(
        &transcript,
        "{\"message\":{\"model\":\"claude-opus-4-7\",\"usage\":\
             {\"input_tokens\":100000,\"output_tokens\":500}}}\n",
    )
    .unwrap();
    let obs = ClaudeAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({ "transcript_path": transcript.to_str().unwrap() }),
        )
        .unwrap();
    assert_eq!(obs.context_pct, None);
    assert_eq!(obs.total_tokens, None);
}

#[test]
fn session_end_is_recorded_and_ends_the_session() {
    // SessionEnd must produce an observation so the reducer drops the agent
    // from the rollup, and must report `ends_session` so the CLI expires
    // the dead session's pending asks.
    let obs = ClaudeAdapter
        .observe_lifecycle("SessionEnd", &json!({ "session_id": "sess-1" }))
        .expect("SessionEnd is a recorded lifecycle observation");
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    assert!(ClaudeAdapter.ends_session("SessionEnd"));
    assert!(!ClaudeAdapter.ends_session("Stop"));
}

#[test]
fn turn_boundaries_move_the_session_on() {
    // Stop and a fresh prompt clear the session's mid-turn native_ui asks;
    // SessionStart/SessionEnd and tool events do not.
    assert!(ClaudeAdapter.moves_on("Stop"));
    assert!(ClaudeAdapter.moves_on("UserPromptSubmit"));
    assert!(!ClaudeAdapter.moves_on("SessionStart"));
    assert!(!ClaudeAdapter.moves_on("SessionEnd"));
    assert!(!ClaudeAdapter.moves_on("PostToolUse"));
}
