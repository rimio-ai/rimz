use serde_json::json;

use super::*;
use crate::agents::AgentHookClass;
use crate::feed::ResolutionMethod;
use std::io::Write;
use std::path::Path;

#[test]
fn resume_command_is_codex_resume_with_the_session_id() {
    let argv = CodexAdapter
        .resume_command("sess-abc", Path::new("/code/query-engine"))
        .expect("codex resumes");
    assert_eq!(argv, vec!["codex", "resume", "sess-abc"]);
}

fn fixture(kind: FeedKind) -> FeedItem {
    crate::agents::testkit::feed_item(kind, "codex")
}

#[test]
fn codex_registers_its_session_lazily() {
    // Codex's instances can be present before a session binds (lazy
    // `SessionStart`, daemon-routed unstamped hooks), so it opts into the
    // sidebar's cwd-bind + idle-instance synthesis. Claude declares the
    // opposite (it stamps a pane on every session).
    assert!(CodexAdapter.descriptor().capabilities.registers_lazily);
    assert!(
        !crate::agents::ClaudeAdapter
            .descriptor()
            .capabilities
            .registers_lazily
    );
}

#[test]
fn permission_decision_has_no_reserved_keys() {
    let item = fixture(FeedKind::Permission);
    let resolution = Resolution::new(json!({ "choice": "allow" }), ResolutionMethod::HookBridge);
    let rendered = CodexAdapter.render_decision(&item, &resolution).unwrap();
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
    assert!(rendered.get("updatedInput").is_none());
    assert!(rendered.get("updatedPermissions").is_none());
    assert!(rendered.get("interrupt").is_none());
}

#[test]
fn permission_deny_shape_is_pinned() {
    let item = fixture(FeedKind::Permission);
    let resolution = Resolution::new(json!({ "choice": "deny" }), ResolutionMethod::HookBridge);
    let rendered = CodexAdapter.render_decision(&item, &resolution).unwrap();

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
fn neutral_payload_is_empty_stdout() {
    let rendered = CodexAdapter.render_neutral("PermissionRequest").unwrap();

    insta::assert_snapshot!(
        serde_json::to_string(&rendered).unwrap(),
        @"null"
    );
}

#[test]
fn session_start_observes_idle() {
    let obs = CodexAdapter
        .observe_lifecycle("SessionStart", &json!({ "session_id": "sess-1" }))
        .unwrap();
    assert_eq!(obs.agent_id.as_deref(), Some("sess-1"));
    // Wired in, nothing asked yet — a plain startup registers fresh (not a
    // compaction), no task.
    assert_eq!(obs.signal, LifecycleSignal::Registered);
    assert_eq!(obs.task, None);
}

#[test]
fn session_start_compact_source_flags_compaction() {
    // Codex re-fires `SessionStart` with `source = "compact"` once the
    // context has been condensed; that is the one SessionStart that flags the
    // compaction marker, the others (startup/resume/clear) do not.
    let compact = CodexAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({ "session_id": "sess-1", "source": "compact" }),
        )
        .unwrap();
    assert_eq!(compact.signal, LifecycleSignal::Compacting);
    for source in ["startup", "resume", "clear"] {
        let obs = CodexAdapter
            .observe_lifecycle(
                "SessionStart",
                &json!({ "session_id": "sess-1", "source": source }),
            )
            .unwrap();
        assert_eq!(
            obs.signal,
            LifecycleSignal::Registered,
            "{source} is not a compaction",
        );
    }
}

#[test]
fn user_prompt_submit_observes_running_with_prompt_task() {
    let obs = CodexAdapter
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
fn subagent_start_observes_child_id_and_type() {
    let obs = CodexAdapter
        .observe_lifecycle(
            "SubagentStart",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
            }),
        )
        .unwrap();

    assert_eq!(obs.agent_id.as_deref(), Some("child-thread-1"));
    assert_eq!(obs.signal, LifecycleSignal::SubagentStarted);
    assert_eq!(obs.task.as_deref(), Some("review"));
    // The child keys off `agent_id`; the payload's `session_id` is its parent
    // root, captured so the sidebar can nest it.
    assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
}

#[test]
fn subagent_stop_resolves_success_child_id() {
    let obs = CodexAdapter
        .observe_lifecycle(
            "SubagentStop",
            &json!({
                "session_id": "sess-parent",
                "agent_id": "child-thread-1",
                "agent_type": "review",
            }),
        )
        .unwrap();

    assert_eq!(obs.agent_id.as_deref(), Some("child-thread-1"));
    // Codex reports no subagent error signal, so a stop is always clean.
    assert_eq!(
        obs.signal,
        LifecycleSignal::SubagentStopped { errored: false }
    );
    // The type label persists across stop so a finished child stays labeled
    // while it lingers in the parent's list.
    assert_eq!(obs.task.as_deref(), Some("review"));
    assert_eq!(obs.parent_agent_id.as_deref(), Some("sess-parent"));
}

#[test]
fn foreign_child_mutating_post_tool_use_is_dropped() {
    // A non-Subagent* event carrying a distinct `agent_id` fired inside a
    // subagent: dropped rather than keyed as a parentless phantom root.
    // Latent today — Codex stamps `agent_id` only on Subagent* — but the
    // root arm shares Claude's rule so the door stays closed.
    let obs = CodexAdapter.observe_lifecycle(
        "PostToolUse",
        &json!({
            "session_id": "sess-parent",
            "agent_id": "child-thread-1",
            "tool_name": "shell",
        }),
    );
    assert!(
        obs.is_none(),
        "a foreign-id tool event never creates a parentless root row"
    );

    // Without the foreign id the same payload is a normal root `ToolUsed`.
    let root = CodexAdapter
        .observe_lifecycle(
            "PostToolUse",
            &json!({ "session_id": "sess-parent", "tool_name": "shell" }),
        )
        .unwrap();
    assert_eq!(root.agent_id.as_deref(), Some("sess-parent"));
    assert_eq!(root.parent_agent_id, None);
}

#[test]
fn clean_stop_observes_success() {
    let obs = CodexAdapter
        .observe_lifecycle("Stop", &json!({ "session_id": "sess-1" }))
        .unwrap();
    assert_eq!(
        obs.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        }
    );
}

#[test]
fn errored_stop_observes_failed() {
    let obs = CodexAdapter
        .observe_lifecycle(
            "Stop",
            &json!({ "session_id": "sess-1", "status": "failed" }),
        )
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
fn classification_unchanged_for_unknown_event() {
    let c = CodexAdapter.classify_hook("WatItIs", &Value::Null);
    assert_eq!(c.class, AgentHookClass::Unknown);
    assert!(c.feed_kind.is_none());
}

#[test]
fn configured_reasoning_effort_reads_the_actual_codex_setting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
model = "gpt-5.5-codex"
model_reasoning_effort = "xhigh"
plan_mode_reasoning_effort = "medium"
"#,
    )
    .unwrap();

    assert_eq!(
        configured_reasoning_effort_at(&path).as_deref(),
        Some("xhigh")
    );
}

#[test]
fn install_into_empty_dir_creates_documented_inline_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let report = install_into(&path).unwrap();
    assert!(!report.merged);
    assert_eq!(report.agent, "codex");
    let expected: Vec<&str> = INSTALLED_EVENTS.iter().map(|(event, _)| *event).collect();
    assert_eq!(report.installed_events, expected);
    assert!(hooks_installed_at(&path));

    // Every command is identical (no `--event`; the helper reads the event
    // from the stdin payload's `hook_event_name`).
    let text = std::fs::read_to_string(&path).unwrap();
    insta::assert_snapshot!(text, @r###"
        [[hooks.PermissionRequest]]
        matcher = ".*"

        [[hooks.PermissionRequest.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing PermissionRequest through Rimz"
        timeout = 60
        type = "command"

        [[hooks.PostToolUse]]
        matcher = ".*"

        [[hooks.PostToolUse.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing PostToolUse through Rimz"
        timeout = 60
        type = "command"

        [[hooks.PreToolUse]]
        matcher = ".*"

        [[hooks.PreToolUse.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing PreToolUse through Rimz"
        timeout = 60
        type = "command"

        [[hooks.SessionStart]]
        matcher = "startup|resume|clear|compact"

        [[hooks.SessionStart.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing SessionStart through Rimz"
        timeout = 60
        type = "command"

        [[hooks.Stop]]

        [[hooks.Stop.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing Stop through Rimz"
        timeout = 60
        type = "command"

        [[hooks.SubagentStart]]
        matcher = ".*"

        [[hooks.SubagentStart.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing SubagentStart through Rimz"
        timeout = 60
        type = "command"

        [[hooks.SubagentStop]]
        matcher = ".*"

        [[hooks.SubagentStop.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing SubagentStop through Rimz"
        timeout = 60
        type = "command"

        [[hooks.UserPromptSubmit]]

        [[hooks.UserPromptSubmit.hooks]]
        command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex"
        statusMessage = "Routing UserPromptSubmit through Rimz"
        timeout = 60
        type = "command"
        "###);
}

#[test]
fn install_preserves_user_hooks_and_wires_per_tool() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"model = "gpt-5.5"

[[hooks.PreToolUse]]
matcher = "^Bash$"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "echo user"
"#,
    )
    .unwrap();

    let report = install_into(&path).unwrap();
    assert!(report.merged);
    for per_tool_event in ["PreToolUse", "PostToolUse"] {
        assert!(report.installed_events.iter().any(|e| e == per_tool_event));
    }

    let parsed: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        parsed.get("model").and_then(toml::Value::as_str),
        Some("gpt-5.5")
    );
    let pre_tool = parsed
        .get("hooks")
        .and_then(toml::Value::as_table)
        .and_then(|hooks| hooks.get("PreToolUse"))
        .and_then(toml::Value::as_array)
        .unwrap();
    assert!(
        pre_tool.iter().any(|group| {
            group
                .as_table()
                .and_then(|table| table.get("hooks"))
                .and_then(toml::Value::as_array)
                .is_some_and(|handlers| {
                    handlers.iter().any(|handler| {
                        handler
                            .as_table()
                            .and_then(|table| table.get("command"))
                            .and_then(toml::Value::as_str)
                            == Some("echo user")
                    })
                })
        }),
        "user hook must survive install"
    );
    assert!(
        has_rimz_hook_command(&parsed, "PreToolUse"),
        "install wires the broad PreToolUse hook"
    );
}

#[test]
fn uninstall_removes_legacy_block_and_preserves_user_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
            &path,
            "model = \"o4-mini\"\n[hooks.user_custom]\ncommand = [\"echo\", \"hi\"]\n[hooks.rimz]\nevents = [\"SessionStart\", \"PermissionRequest\"]\nmanaged_by = \"rimz\"\n",
        )
        .unwrap();
    let report = uninstall_from(&path).unwrap();
    assert!(report.existed);
    assert_eq!(
        report.removed_events,
        vec!["PermissionRequest".to_owned(), "SessionStart".to_owned()]
    );
    let parsed: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        parsed.get("model").and_then(toml::Value::as_str),
        Some("o4-mini")
    );
    let hooks = parsed.get("hooks").and_then(toml::Value::as_table).unwrap();
    assert!(hooks.contains_key("user_custom"));
    assert!(!hooks.contains_key(RIMZ_BLOCK));
}

#[test]
fn uninstall_removes_rimz_hook_commands_and_preserves_user_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    install_into(&path).unwrap();
    std::fs::write(
            &path,
            format!(
                "{}\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"echo user stop\"\n",
                std::fs::read_to_string(&path).unwrap()
            ),
        )
        .unwrap();

    let report = uninstall_from(&path).unwrap();
    assert!(report.existed);
    assert!(report.removed_events.contains(&"SessionStart".to_owned()));
    assert!(
        report
            .removed_events
            .contains(&"PermissionRequest".to_owned())
    );
    assert!(!hooks_installed_at(&path));

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("echo user stop"));
    assert!(!text.contains("rimz hooks feed --source codex"));
}

#[test]
fn hooks_installed_rejects_legacy_unwrapped_commands() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"[[hooks.SessionStart]]
[[hooks.SessionStart.hooks]]
type = "command"
command = "rimz hooks feed --source codex --event SessionStart"

[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "rimz hooks feed --source codex --event Stop"

[[hooks.PermissionRequest]]
[[hooks.PermissionRequest.hooks]]
type = "command"
command = "rimz hooks feed --source codex --event PermissionRequest"
"#,
    )
    .unwrap();
    assert!(
        !hooks_installed_at(&path),
        "legacy commands lack the PID wrapper and must be reinstalled"
    );
    install_into(&path).unwrap();
    assert!(hooks_installed_at(&path));
}

#[test]
fn install_reclaims_legacy_event_tables() {
    // Version drift: an older build wrote the exec form *with* `--event`,
    // and a duplicate stacked up. Reinstall must reclaim every old rimz
    // table — regardless of `--event` — and leave exactly one current
    // handler per event, with the user hook untouched.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"[[hooks.SessionStart]]
matcher = "startup|resume|clear|compact"
[[hooks.SessionStart.hooks]]
type = "command"
command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event SessionStart"

[[hooks.SessionStart]]
matcher = "startup|resume|clear|compact"
[[hooks.SessionStart.hooks]]
type = "command"
command = "RIMZ_AGENT_PID=$PPID exec rimz hooks feed --source codex --event SessionStart"

[[hooks.PreToolUse]]
matcher = "^Bash$"
[[hooks.PreToolUse.hooks]]
type = "command"
command = "echo user"
"#,
    )
    .unwrap();
    install_into(&path).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        !text.contains("--event"),
        "every legacy `--event` table must be reclaimed: {text}"
    );
    assert!(text.contains("echo user"), "user hook must survive install");

    let parsed: toml::Table = toml::from_str(&text).unwrap();
    let group_count = |event: &str| {
        parsed
            .get("hooks")
            .and_then(toml::Value::as_table)
            .and_then(|hooks| hooks.get(event))
            .and_then(toml::Value::as_array)
            .map_or(0, Vec::len)
    };
    // Two stacked legacy SessionStart tables collapse to one.
    assert_eq!(group_count("SessionStart"), 1);
    // PreToolUse keeps the user group and gains exactly one rimz group.
    assert_eq!(group_count("PreToolUse"), 2);
    assert!(hooks_installed_at(&path));
}

#[test]
fn uninstall_on_missing_file_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let report = uninstall_from(&path).unwrap();
    assert!(!report.existed);
    assert!(report.removed_events.is_empty());
}

#[test]
fn post_lifecycle_refresh_fires_on_turn_boundaries_only() {
    let ctx = crate::agents::LifecycleRefreshCtx {
        agent_id: "sess-1",
        workspace_id: "ws-1",
        model_hint: Some("gpt-5"),
    };
    let spawn = CodexAdapter
        .post_lifecycle_refresh("Stop", &ctx)
        .expect("Stop refreshes");
    assert_eq!(
        spawn.args,
        [
            "codex",
            "refresh-context",
            "--session-id",
            "sess-1",
            "--workspace-id",
            "ws-1",
            "--model",
            "gpt-5",
        ]
    );
    // No model hint → no --model flag.
    let bare = crate::agents::LifecycleRefreshCtx {
        model_hint: None,
        ..ctx
    };
    let spawn = CodexAdapter
        .post_lifecycle_refresh("SessionStart", &bare)
        .expect("SessionStart refreshes");
    assert!(!spawn.args.iter().any(|arg| arg == "--model"));
    // Per-tool events stay silent here — an app-server spawn per call is too frequent.
    // The cheap local transcript refresh is a separate inline lane.
    for event in ["PreToolUse", "PostToolUse", "SubagentStop", "Notification"] {
        assert!(
            CodexAdapter.post_lifecycle_refresh(event, &ctx).is_none(),
            "{event}"
        );
    }
}

#[test]
fn local_context_refresh_fires_for_progress_events_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":500,\"cached_input_tokens\":300,\
             \"output_tokens\":20,\"total_tokens\":520},\
             \"model_context_window\":1000}}}\n",
    )
    .unwrap();
    let path = path.to_string_lossy().into_owned();
    let ctx = crate::agents::LocalContextRefreshCtx {
        agent_id: "sess-1",
        model_hint: Some("gpt-5"),
        prior_effort: None,
        prior_transcript_path: Some(&path),
        prior_transcript_stat: None,
    };

    let refresh = CodexAdapter
        .local_context_refresh("PostToolUse", &ctx)
        .expect("PostToolUse reads local transcript context");
    assert_eq!(
        refresh
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(50)
    );
    assert!(
        CodexAdapter
            .local_context_refresh("PreToolUse", &ctx)
            .is_none()
    );
    assert!(
        CodexAdapter
            .local_context_refresh("PermissionRequest", &ctx)
            .is_none()
    );
}

#[test]
fn codex_hook_cap_is_shorter_than_claude_default() {
    use crate::agents::ClaudeAdapter;
    assert!(CodexAdapter.descriptor().hook_cap < ClaudeAdapter.descriptor().hook_cap);
}

#[test]
fn transcript_tail_populates_context_gauge() {
    // Codex reports token usage only in the rollout JSONL; the lifecycle
    // hooks read its tail to fill the context gauge. Half the model's
    // 258_400-token window = 50% with the `last_token_usage.total_tokens`
    // surfacing through to `total_tokens`.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-1\"}}\n\
             {\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5.5\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":\
             {\"last_token_usage\":{\"input_tokens\":129200,\"total_tokens\":130000},\
             \"model_context_window\":258400}}}\n",
    )
    .unwrap();
    let usage = usage_from_transcript(&path);
    assert_eq!(usage.context_pct, Some(50));
    assert_eq!(usage.total_tokens, Some(130_000));
    assert_eq!(usage.model.as_deref(), Some("gpt-5.5"));
}

#[test]
fn fresh_transcript_reports_zero_context_not_unknown() {
    // A brand-new session has a rollout with no `token_count` event yet.
    // It must read as 0% (empty gauge), not `None` (no gauge), so a
    // just-launched idle Codex shows an empty context bar — matching the
    // Claude adapter's fresh-session behaviour.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"sess-1\"}}\n",
    )
    .unwrap();
    let usage = usage_from_transcript(&path);
    assert_eq!(usage.context_pct, Some(0));
    assert_eq!(usage.total_tokens, Some(0));
    // The per-call split reads an explicit zero too, mirroring the totals.
    assert_eq!(usage.last_input_tokens, Some(0));
    assert_eq!(usage.last_cached_input_tokens, Some(0));
    assert_eq!(usage.last_output_tokens, Some(0));
}

#[test]
fn missing_transcript_leaves_context_unknown() {
    // No readable rollout means unknown, not zero — the gauge stays
    // hidden rather than asserting a false 0%.
    let usage = usage_from_transcript(Path::new("/nonexistent/path/rollout.jsonl"));
    assert_eq!(usage.context_pct, None);
    assert_eq!(usage.total_tokens, None);
}

#[test]
fn transcript_tail_populates_cumulative_totals() {
    // total_token_usage carries the cumulative session billing totals;
    // usage_from_transcript must surface them so refresh_context can
    // price the session cost.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"codex-mini\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":500,\"total_tokens\":600},\
             \"total_token_usage\":{\"input_tokens\":1000,\"output_tokens\":200,\
             \"cached_input_tokens\":400},\
             \"model_context_window\":100000}}}\n",
    )
    .unwrap();
    let usage = usage_from_transcript(&path);
    assert_eq!(usage.cumulative_input_tokens, Some(1000));
    assert_eq!(usage.cumulative_output_tokens, Some(200));
    assert_eq!(usage.cumulative_cached_tokens, 400);
    assert_eq!(usage.model.as_deref(), Some("codex-mini"));
}

#[test]
fn transcript_tail_populates_the_per_call_split() {
    // `last_token_usage` carries the latest call's full field set —
    // `input_tokens` (the cached slice included), `cached_input_tokens`, and
    // `output_tokens` — which the card's composition line legends (`◌`
    // cache-read, `↘` fresh input, `↗` output). The parser must surface them
    // raw; the adapter derives fresh input as `input − cached`.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":129200,\"cached_input_tokens\":120000,\
             \"output_tokens\":800,\"total_tokens\":130000},\
             \"model_context_window\":258400}}}\n",
    )
    .unwrap();
    let usage = usage_from_transcript(&path);
    assert_eq!(usage.last_input_tokens, Some(129_200));
    assert_eq!(usage.last_cached_input_tokens, Some(120_000));
    assert_eq!(usage.last_output_tokens, Some(800));
}

#[test]
fn transcript_enrichment_maps_codex_split_to_rich_usage() {
    let usage = TranscriptUsage {
        context_pct: Some(42),
        context_window: Some(10_000),
        total_tokens: Some(4_200),
        model: Some("gpt-5".to_owned()),
        last_input_tokens: Some(1_200),
        last_cached_input_tokens: Some(1_000),
        last_output_tokens: Some(80),
        cumulative_input_tokens: None,
        cumulative_cached_tokens: 0,
        cumulative_output_tokens: None,
    };
    let (tokens, cost, model_id) = transcript_enrichment(&usage, None);
    let tokens = tokens.expect("tokens are mapped");
    let current = tokens.current_usage.expect("current usage is mapped");
    assert_eq!(tokens.context_window_size, Some(10_000));
    assert_eq!(tokens.used_percentage, Some(42));
    assert_eq!(tokens.remaining_percentage, Some(58));
    assert_eq!(current.input_tokens, Some(200));
    assert_eq!(current.cache_read_input_tokens, Some(1_000));
    assert_eq!(current.cache_creation_input_tokens, None);
    assert_eq!(current.output_tokens, Some(80));
    assert_eq!(
        current.input_tokens.unwrap()
            + current.cache_read_input_tokens.unwrap()
            + current.cache_creation_input_tokens.unwrap_or(0),
        usage.last_input_tokens.unwrap(),
        "rich context numerator matches the row-level fallback"
    );
    assert_eq!(cost, None);
    assert_eq!(model_id.as_deref(), Some("gpt-5"));
}

#[test]
fn transcript_enrichment_prices_cumulative_totals() {
    let usage = TranscriptUsage {
        context_pct: None,
        context_window: None,
        total_tokens: None,
        model: Some("gpt-5".to_owned()),
        last_input_tokens: None,
        last_cached_input_tokens: None,
        last_output_tokens: None,
        cumulative_input_tokens: Some(1_000),
        cumulative_cached_tokens: 400,
        cumulative_output_tokens: Some(200),
    };
    let (_tokens, cost, _model_id) = transcript_enrichment(&usage, None);
    let cost = cost
        .and_then(|cost| cost.total_cost_usd)
        .expect("known model prices cumulative totals");
    let price = PriceBook::embedded().price("gpt-5").unwrap();
    let expected = 600.0 * price.input + 400.0 * price.cache_read + 200.0 * price.output;
    assert!((cost - expected).abs() < f64::EPSILON);
}

#[test]
fn refresh_transcript_context_stat_gate_skips_unchanged_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n",
    )
    .unwrap();
    let stat = transcript_stat(&path).unwrap();
    let path_string = path.to_string_lossy().into_owned();
    assert!(
        refresh_transcript_context("sess-1", None, None, Some(&path_string), Some(&stat)).is_none(),
        "unchanged stat skips the tail read and sidecar write"
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
              \"last_token_usage\":{\"input_tokens\":50,\"total_tokens\":60},\
              \"model_context_window\":100}}}\n",
        )
        .unwrap();
    let refresh = refresh_transcript_context("sess-1", None, None, Some(&path_string), Some(&stat))
        .expect("changed stat refreshes");
    assert_eq!(
        refresh
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(50)
    );
    assert_ne!(refresh.transcript_stat, Some(stat));
}

#[test]
fn refresh_transcript_context_reruns_when_prior_effort_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":50,\"total_tokens\":60},\
             \"model_context_window\":100}}}\n",
    )
    .unwrap();
    let stat = transcript_stat(&path).unwrap();
    let path_string = path.to_string_lossy().into_owned();

    let refresh = refresh_transcript_context(
        "sess-1",
        None,
        Some("medium"),
        Some(&path_string),
        Some(&stat),
    )
    .expect("stale prior effort forces a local refresh despite unchanged stat");
    assert_eq!(
        refresh
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(50)
    );
}

#[test]
fn transcript_tail_without_split_fields_leaves_them_unknown() {
    // An older rollout whose `last_token_usage` reports only `input_tokens`
    // and `total_tokens` keeps the cached/output sides unknown rather than
    // asserting a false zero — the card then renders the input it does know.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":500,\"total_tokens\":600},\
             \"model_context_window\":100000}}}\n",
    )
    .unwrap();
    let usage = usage_from_transcript(&path);
    assert_eq!(usage.last_input_tokens, Some(500));
    assert_eq!(usage.last_cached_input_tokens, None);
    assert_eq!(usage.last_output_tokens, None);
}

#[test]
fn transcript_tail_without_total_token_usage_leaves_cumulative_none() {
    // Older rollout files that only have last_token_usage must not produce
    // a spurious cost estimate.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
             \"last_token_usage\":{\"input_tokens\":500,\"total_tokens\":600},\
             \"model_context_window\":100000}}}\n",
    )
    .unwrap();
    let usage = usage_from_transcript(&path);
    assert_eq!(usage.cumulative_input_tokens, None);
    assert_eq!(usage.cumulative_output_tokens, None);
    assert_eq!(usage.cumulative_cached_tokens, 0);
}

#[test]
fn find_session_transcript_walks_codex_date_hierarchy() {
    // Codex shards rollouts under `YYYY/MM/DD/`; the locator finds a file
    // whose name ends with `{session_id}.jsonl` regardless of how deep the
    // shard is.
    let dir = tempfile::tempdir().unwrap();
    let day_dir = dir.path().join("2026").join("05").join("26");
    std::fs::create_dir_all(&day_dir).unwrap();
    let expected = day_dir.join("rollout-2026-05-26T21-57-38-sess-abc.jsonl");
    std::fs::write(&expected, "{}\n").unwrap();
    // A noise file for a different session in the same day must not match.
    std::fs::write(day_dir.join("rollout-other-sess.jsonl"), "{}\n").unwrap();

    let found = find_session_transcript_under(dir.path(), "sess-abc").unwrap();
    assert_eq!(found, expected);
    assert!(find_session_transcript_under(dir.path(), "sess-missing").is_none());
}

// --- the hook trust gate ---
//
// Codex records trust per hook-definition hash under `[hooks.state]` and
// silently skips an untrusted hook; these pin the presence-only detection
// that lets `rimz start` and `rimz doctor` surface the dead channel.

/// Append a `[hooks.state]` trust entry for `token`, key-shaped exactly as
/// Codex writes it: `"<config-path>:<event_token>:<i>:<j>"`.
fn trust_event(path: &std::path::Path, token: &str) {
    let entry = format!(
        "\n[hooks.state.\"{}:{token}:0:0\"]\ntrusted_hash = \"sha256:deadbeef\"\n",
        path.display(),
    );
    let mut text = std::fs::read_to_string(path).unwrap();
    text.push_str(&entry);
    std::fs::write(path, text).unwrap();
}

#[test]
fn untrusted_hooks_report_every_installed_event_when_state_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    install_into(&path).unwrap();

    let expected: Vec<String> = INSTALLED_EVENTS
        .iter()
        .map(|(event, _)| (*event).to_owned())
        .collect();
    assert_eq!(untrusted_hook_events_at(&path), expected);
}

#[test]
fn untrusted_hooks_empty_when_every_event_is_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    install_into(&path).unwrap();
    for (event, _) in INSTALLED_EVENTS {
        trust_event(&path, &snake_event_token(event));
    }

    assert_eq!(untrusted_hook_events_at(&path), Vec::<String>::new());
}

#[test]
fn untrusted_hooks_report_the_exact_missing_subset() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    install_into(&path).unwrap();
    for (event, _) in INSTALLED_EVENTS {
        // `subagent_stop` trusted while `stop` is not also proves the token
        // match is colon-delimited, not substring-loose.
        if *event != "Stop" && *event != "PermissionRequest" {
            trust_event(&path, &snake_event_token(event));
        }
    }

    assert_eq!(
        untrusted_hook_events_at(&path),
        vec!["Stop".to_owned(), "PermissionRequest".to_owned()],
    );
}

#[test]
fn untrusted_hooks_empty_when_rimz_hooks_are_not_installed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "model = \"gpt-5.5\"\n").unwrap();

    assert_eq!(untrusted_hook_events_at(&path), Vec::<String>::new());
}

#[test]
fn snake_event_token_matches_codex_state_keys() {
    assert_eq!(snake_event_token("PermissionRequest"), "permission_request");
    assert_eq!(snake_event_token("PreToolUse"), "pre_tool_use");
    assert_eq!(snake_event_token("SessionStart"), "session_start");
    assert_eq!(snake_event_token("Stop"), "stop");
    assert_eq!(snake_event_token("SubagentStop"), "subagent_stop");
}
