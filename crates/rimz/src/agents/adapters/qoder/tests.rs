use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::agents::lifecycle::{AskKind, LifecycleSignal, PriorTurnIds, step};
use crate::agents::testkit::{hook_lifecycle, hook_output, hook_signal};
use crate::agents::{AgentStatus, FolderTrust, HookReply, SessionOrigin, TurnErrorClass};

const EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "UserPromptSubmit",
    "Stop",
    "StopFailure",
    "QueryEnd",
    "Notification",
    "PostToolUse",
    "PostToolUseFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "PermissionRequest",
];

#[test]
fn neutral_replies_are_silent_including_blocking_malformed_and_unknown() {
    let permission = hook_output(
        &QoderAdapter,
        "PermissionRequest",
        &json!({"tool_name":"Bash"}),
    );
    insta::assert_json_snapshot!(permission.json_reply(), @"null");
    let malformed = hook_output(&QoderAdapter, "PermissionRequest", &Value::Null);
    insta::assert_json_snapshot!(malformed.json_reply(), @"null");
    for event in EVENTS.iter().copied().chain(["unknown"]) {
        for payload in [json!({"session_id":"root"}), json!([]), Value::Null] {
            assert_eq!(
                hook_output(&QoderAdapter, event, &payload).reply(),
                &HookReply::Silent
            );
        }
    }
}

#[test]
fn query_abort_interrupts_but_normal_query_end_and_notifications_do_not() {
    assert_eq!(
        hook_signal(&QoderAdapter, "QueryEnd", &json!({"reason":"abort"})),
        LifecycleSignal::TurnInterrupted { turn_id: None }
    );
    for (event, payload) in [
        ("QueryEnd", json!({"reason":"end_turn"})),
        ("QueryEnd", json!({"reason":"future"})),
        ("Notification", json!({"notification_type":"idle_prompt"})),
        (
            "Notification",
            json!({"notification_type":"permission_prompt","details":{"toolName":"AskUserQuestion"}}),
        ),
    ] {
        assert!(
            hook_output(&QoderAdapter, event, &payload)
                .lifecycle()
                .is_none()
        );
    }
}

#[test]
fn new_session_is_fresh_resume_is_not_and_compact_closes() {
    for source in ["startup", "clear", "new"] {
        let start = hook_lifecycle(
            &QoderAdapter,
            "SessionStart",
            &json!({"session_id":"root","source":source}),
        );
        assert_eq!(start.origin, Some(SessionOrigin::Fresh), "{source}");
        assert_eq!(start.signal, LifecycleSignal::Registered);
    }
    let resume = hook_lifecycle(&QoderAdapter, "SessionStart", &json!({"source":"resume"}));
    assert_eq!(resume.origin, None);
    assert_eq!(resume.signal, LifecycleSignal::Registered);
    assert_eq!(
        hook_signal(&QoderAdapter, "SessionStart", &json!({"source":"compact"})),
        LifecycleSignal::CompactionEnded {
            auto: None,
            failed: false
        }
    );
}

#[test]
fn permission_kind_comes_from_the_native_tool() {
    for (tool, kind) in [
        ("AskUserQuestion", AskKind::Question),
        ("ExitPlanMode", AskKind::PlanApproval),
        ("Bash", AskKind::Permission),
    ] {
        let decoded = hook_output(
            &QoderAdapter,
            "PermissionRequest",
            &json!({"session_id":"root","tool_name":tool,"tool_use_id":"call"}),
        );
        assert_eq!(decoded.ask_kind(), Some(kind), "{tool}");
        assert!(
            matches!(decoded.lifecycle().map(|o| &o.signal), Some(LifecycleSignal::AwaitingInput { kind: actual, native_key: Some(key), .. }) if *actual == kind && key == "call")
        );
    }
}

#[test]
fn failure_class_uses_error_not_error_type() {
    for (error, class) in [
        ("rate_limit", TurnErrorClass::PausedRateLimit),
        ("overloaded", TurnErrorClass::PausedOverloaded),
        ("server_error", TurnErrorClass::PausedOverloaded),
        ("authentication_failed", TurnErrorClass::Failed),
    ] {
        let decoded = hook_output(&QoderAdapter, "StopFailure", &json!({"error":error}));
        assert_eq!(
            decoded.turn_error().map(|e| e.class),
            Some(class),
            "{error}"
        );
        assert!(matches!(
            decoded.lifecycle().map(|o| &o.signal),
            Some(LifecycleSignal::TurnEnded { errored: true, .. })
        ));
    }
    let legacy = hook_output(
        &QoderAdapter,
        "StopFailure",
        &json!({"error_type":"rate_limit"}),
    );
    assert_ne!(
        legacy.turn_error().map(|e| e.class),
        Some(TurnErrorClass::PausedRateLimit)
    );
}

#[test]
fn children_have_split_routing_and_parented_start_stop() {
    let payload =
        json!({"session_id":"root","agent_id":"child","agent_type":"review","tool_name":"Edit"});
    let tool = hook_output(&QoderAdapter, "PostToolUse", &payload);
    assert_eq!(tool.event_agent_id().map(|id| id.as_str()), Some("child"));
    assert_eq!(tool.context_agent_id().map(|id| id.as_str()), Some("root"));
    for (event, signal) in [
        ("SubagentStart", LifecycleSignal::SubagentStarted),
        (
            "SubagentStop",
            LifecycleSignal::SubagentStopped { errored: false },
        ),
    ] {
        let observation = hook_lifecycle(&QoderAdapter, event, &payload);
        assert_eq!(observation.agent_id.as_deref(), Some("child"));
        assert_eq!(observation.parent_agent_id.as_deref(), Some("root"));
        assert_eq!(observation.task.as_deref(), Some("review"));
        assert_eq!(observation.signal, signal);
    }
    assert!(
        hook_output(
            &QoderAdapter,
            "SubagentStart",
            &json!({"session_id":"root"})
        )
        .lifecycle()
        .is_none()
    );
}

#[test]
fn every_tool_arm_carries_the_right_mutation_and_edit_flags() {
    for (tool, mutates, edits) in [
        ("Bash", true, false),
        ("PowerShell", true, false),
        ("BashInput", true, false),
        ("Monitor", true, false),
        ("Write", true, true),
        ("Edit", true, true),
        ("NotebookEdit", true, true),
        ("Read", false, false),
    ] {
        assert_eq!(
            hook_signal(
                &QoderAdapter,
                "PostToolUse",
                &json!({"tool_name":tool,"tool_use_id":"call"})
            ),
            LifecycleSignal::ToolUsed {
                mutates,
                edits,
                name: None,
                native_key: Some("call".into()),
                turn_id: None
            }
        );
    }
    assert_eq!(
        hook_signal(
            &QoderAdapter,
            "PostToolUseFailure",
            &json!({"tool_name":"Edit"})
        ),
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            name: None,
            native_key: None,
            turn_id: None
        }
    );
}

#[test]
fn lifecycle_flows_through_step_and_query_end_distinguishes_completion_from_abort() {
    let mut state = None;
    for (event, payload, expected) in [
        ("SessionStart", json!({}), AgentStatus::Idle),
        (
            "UserPromptSubmit",
            json!({"prompt":"  fix auth  "}),
            AgentStatus::Running,
        ),
        (
            "PostToolUse",
            json!({"tool_name":"Read"}),
            AgentStatus::Running,
        ),
        (
            "PermissionRequest",
            json!({"tool_name":"Bash"}),
            AgentStatus::Waiting,
        ),
        (
            "Stop",
            json!({"last_assistant_message":"all done"}),
            AgentStatus::Success,
        ),
        (
            "QueryEnd",
            json!({"reason":"end_turn"}),
            AgentStatus::Success,
        ),
    ] {
        let decoded = hook_output(&QoderAdapter, event, &payload);
        assert_eq!(
            decoded.lifecycle().is_none(),
            event == "QueryEnd",
            "{event}"
        );
        if event == "UserPromptSubmit" {
            assert_eq!(
                decoded.lifecycle().unwrap().prompt.as_deref(),
                Some("fix auth")
            );
            assert_eq!(
                decoded.lifecycle().unwrap().task.as_deref(),
                Some("fix auth")
            );
        }
        if event == "Stop" {
            assert_eq!(decoded.final_message(), Some("all done"));
        }
        if let Some(observation) = decoded.lifecycle() {
            state = Some(
                step(
                    state.as_ref(),
                    None,
                    PriorTurnIds::default(),
                    &observation.signal,
                )
                .next,
            );
        }
        assert_eq!(state.unwrap().status, expected, "{event}");
    }
    let abort = hook_signal(&QoderAdapter, "QueryEnd", &json!({"reason":"abort"}));
    let running = step(
        None,
        None,
        PriorTurnIds::default(),
        &hook_signal(&QoderAdapter, "UserPromptSubmit", &json!({"prompt":"go"})),
    )
    .next;
    assert_eq!(
        step(Some(&running), None, PriorTurnIds::default(), &abort)
            .next
            .status,
        AgentStatus::Idle
    );
    assert_eq!(
        hook_signal(&QoderAdapter, "SessionEnd", &json!({})),
        LifecycleSignal::Ended
    );
}

#[test]
fn post_compact_then_session_start_counts_one_bracket() {
    let start = hook_signal(&QoderAdapter, "PreCompact", &json!({}));
    let compacting = step(None, None, PriorTurnIds::default(), &start).next;
    assert!(compacting.compacting);
    let end = hook_signal(&QoderAdapter, "PostCompact", &json!({"trigger":"manual"}));
    assert_eq!(
        end,
        LifecycleSignal::CompactionEnded {
            auto: Some(false),
            failed: false
        }
    );
    let closed = step(Some(&compacting), None, PriorTurnIds::default(), &end);
    assert!(closed.compaction_closed);
    let duplicate = step(
        Some(&closed.next),
        None,
        PriorTurnIds::default(),
        &hook_signal(&QoderAdapter, "SessionStart", &json!({"source":"compact"})),
    );
    assert!(!duplicate.compaction_closed);
    assert_eq!(duplicate.next, closed.next);
    assert_eq!(
        hook_signal(&QoderAdapter, "PostCompact", &json!({"trigger":"auto"})),
        LifecycleSignal::CompactionEnded {
            auto: Some(true),
            failed: false
        }
    );
}

#[test]
fn stamped_pid_is_accepted_unchanged() {
    assert_eq!(
        QoderAdapter.hook_ingress(Some(42)),
        crate::agents::HookIngressDecision::Accept(crate::agents::HookIngressAcceptance::agent(
            Some(42)
        ))
    );
}

struct Fixture {
    _dir: tempfile::TempDir,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    settings: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("project");
        let home = dir.path().join("home");
        let config = dir.path().join("selected");
        for path in [&cwd, &home, &config] {
            std::fs::create_dir_all(path).unwrap();
        }
        let env = BTreeMap::from([
            ("HOME".into(), home.to_string_lossy().into()),
            ("QODER_CONFIG_DIR".into(), config.to_string_lossy().into()),
        ]);
        Self {
            _dir: dir,
            cwd,
            env,
            settings: config.join("settings.json"),
        }
    }

    fn write(&self, path: &Path, value: &Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
    }

    fn blocker(&self, argv: &[&str]) -> Option<LaunchBlocker> {
        QoderAdapter.launch_blocker(
            &self.cwd,
            &self.env,
            &argv.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
    }

    fn trust(&self) -> Option<FolderTrust> {
        QoderAdapter.folder_trust(&self.cwd, Some(self._dir.path()), &self.env)
    }
}

#[test]
fn install_preview_reclaim_drift_and_uninstall_keep_user_hooks() {
    let fixture = Fixture::new();
    fixture.write(&fixture.settings, &json!({"model":"custom","hooks":{"Notification":[{"hooks":[{"type":"command","command":"echo user"}]},{"hooks":[{"type":"command","command":"old/rimz hooks feed --source qoder"}]}]}}));
    let original = std::fs::read_to_string(&fixture.settings).unwrap();
    let preview = QoderAdapter.preview_hook_install(&fixture.env);
    assert!(preview.is_ok(), "{preview:?}");
    let preview = preview.unwrap();
    assert_eq!(
        std::fs::read_to_string(&fixture.settings).unwrap(),
        original
    );
    assert_eq!(preview.files[0].path, fixture.settings);
    QoderAdapter.install_hooks(&fixture.env).unwrap();
    assert_eq!(
        std::fs::read_to_string(&fixture.settings).unwrap(),
        preview.files[0].candidate
    );
    assert!(QoderAdapter.hooks_installed(&fixture.env));
    QoderAdapter.install_hooks(&fixture.env).unwrap();
    assert_eq!(
        std::fs::read_to_string(&fixture.settings).unwrap(),
        preview.files[0].candidate
    );
    let mut root: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
    assert_eq!(
        root["hooks"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>(),
        EVENTS.iter().copied().collect()
    );
    assert_eq!(root["hooks"]["Notification"].as_array().unwrap().len(), 2);
    assert_ne!(
        root["hooks"]["PermissionRequest"][0]["hooks"][0]["async"],
        true
    );
    assert!(!root.as_object().unwrap().contains_key("statusLine"));
    root["hooks"].as_object_mut().unwrap().remove("QueryEnd");
    fixture.write(&fixture.settings, &root);
    assert!(!QoderAdapter.hooks_installed(&fixture.env));
    QoderAdapter.install_hooks(&fixture.env).unwrap();
    assert!(QoderAdapter.hooks_installed(&fixture.env));
    QoderAdapter.uninstall_hooks(&fixture.env).unwrap();
    let root: Value = serde_json::from_slice(&std::fs::read(&fixture.settings).unwrap()).unwrap();
    assert_eq!(root["model"], "custom");
    assert_eq!(root["hooks"]["Notification"].as_array().unwrap().len(), 1);
    assert_eq!(
        root["hooks"]["Notification"][0]["hooks"][0]["command"],
        "echo user"
    );
}

#[test]
fn config_home_and_skills_follow_explicit_env() {
    let fixture = Fixture::new();
    for extra in [
        BTreeMap::new(),
        BTreeMap::from([("RIMZ_QODER_SETTINGS".into(), "/ignored".into())]),
    ] {
        let mut env = fixture.env.clone();
        env.extend(extra);
        assert_eq!(
            QoderAdapter.config_home(&env),
            fixture.settings.parent().map(Path::to_path_buf)
        );
        assert_eq!(
            QoderAdapter.skills_home(&env),
            fixture.settings.parent().map(|p| p.join("skills"))
        );
        QoderAdapter.install_hooks(&env).unwrap();
        assert!(fixture.settings.exists());
        assert!(QoderAdapter.hooks_installed(&env));
    }
}

#[test]
fn synchronous_permission_install_refuses_an_async_managed_handler() {
    let fixture = Fixture::new();
    fixture.write(&fixture.settings, &json!({"hooks":{"PermissionRequest":[{"_rimz_managed":true,"hooks":[{"type":"command","command":"user-hook","async":true}]}]}}));
    let result = QoderAdapter.install_hooks(&fixture.env);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("async"));
}

#[test]
fn launch_uses_direct_cli_separate_prompt_args_and_native_resume_forms() {
    assert_eq!(
        QoderAdapter.launch_command(&[], Some("-x probe")),
        Some(vec!["qodercli".into(), "-i".into(), "-x probe".into()])
    );
    let spec = QoderAdapter.spec();
    assert_eq!(
        spec.launch.resume.unwrap().before_id,
        ["qodercli", "--resume"]
    );
    assert_eq!(spec.launch.fork.unwrap().after_id, ["--fork-session"]);
    assert_eq!(
        spec.launch.picker,
        Some(["qodercli", "--resume"].as_slice())
    );
    assert_eq!(spec.launch.compact_command.unwrap().command, "/compact");
    assert_eq!(
        spec.launch.compact_command.unwrap().instruction,
        crate::agents::CompactInstruction::Trailing
    );
    assert_eq!(
        QoderAdapter.min_version(),
        Some(crate::agents::version::CliVersion::new(1, 1, 72))
    );
    assert_eq!(
        QoderAdapter.version_upgrade_fix(),
        Some("run `qodercli update`")
    );
    assert!(!spec.runs_as("node-MainThread"));
    assert!(spec.runs_as("node"));
}

#[test]
fn disable_all_hooks_refuses_with_source_and_fix() {
    let fixture = Fixture::new();
    for path in [
        &fixture.settings,
        &fixture.cwd.join(".qoder/settings.json"),
        &fixture.cwd.join(".qoder/settings.local.json"),
    ] {
        fixture.write(path, &json!({"disableAllHooks":true}));
        let blocker = fixture.blocker(&[]);
        assert!(blocker.is_some(), "{}", path.display());
        let message = blocker.unwrap().to_string();
        assert!(
            message.contains("disableAllHooks")
                && message.contains(&path.display().to_string())
                && message.contains("false"),
            "{message}"
        );
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn hooks_config_disabled_refuses_and_later_layers_win() {
    let fixture = Fixture::new();
    fixture.write(&fixture.settings, &json!({"hooksConfig":{"enabled":false}}));
    assert!(fixture.blocker(&[]).is_some());
    fixture.write(
        &fixture.cwd.join(".qoder/settings.json"),
        &json!({"hooksConfig":{"enabled":true}}),
    );
    assert!(fixture.blocker(&[]).is_none());
    fixture.write(
        &fixture.cwd.join(".qoder/settings.local.json"),
        &json!({"hooksConfig":{"enabled":false}}),
    );
    assert!(fixture.blocker(&[]).is_some());
    fixture.write(&fixture.settings, &json!({"disableAllHooks":true}));
    fixture.write(
        &fixture.cwd.join(".qoder/settings.json"),
        &json!({"disableAllHooks":false}),
    );
    std::fs::remove_file(fixture.cwd.join(".qoder/settings.local.json")).unwrap();
    assert!(fixture.blocker(&[]).is_none());
}

#[test]
fn config_dir_arg_refuses_both_forms_but_not_prompt_text() {
    let fixture = Fixture::new();
    for args in [
        vec!["--config-dir", "/elsewhere"],
        vec!["--config-dir=/elsewhere"],
        vec!["-p", "--config-dir", "/elsewhere"],
        vec!["--print", "--config-dir", "/elsewhere"],
    ] {
        assert!(fixture.blocker(&args).is_some());
    }
    assert!(fixture.blocker(&["--", "--config-dir"]).is_none());
    assert!(fixture.blocker(&["-i", "--config-dir"]).is_none());
}

#[test]
fn alternate_root_env_refuses_with_unset_fix() {
    let mut fixture = Fixture::new();
    for key in ["QODER_CLI_HOME", "QODER_CONFIG_DIR_NAME"] {
        fixture.env.insert(key.into(), "elsewhere".into());
        let blocker = fixture.blocker(&[]);
        assert!(blocker.is_some(), "{key}");
        let message = blocker.unwrap().to_string();
        assert!(message.contains(key) && message.contains("unset"));
        fixture.env.remove(key);
    }
}

#[test]
fn malformed_settings_abstain_without_rewriting() {
    let fixture = Fixture::new();
    std::fs::write(&fixture.settings, "not json").unwrap();
    assert!(fixture.blocker(&[]).is_none());
    assert_eq!(
        std::fs::read_to_string(&fixture.settings).unwrap(),
        "not json"
    );
    std::fs::write(
        &fixture.settings,
        "{ /* user comment */ \"disableAllHooks\": true, }",
    )
    .unwrap();
    assert!(fixture.blocker(&[]).is_some());
}

#[test]
fn folder_trust_matches_exact_ancestor_additional_and_disabled() {
    let fixture = Fixture::new();
    for value in [
        json!({"permissions":{"trustDirectories":[fixture.cwd]}}),
        json!({"permissions":{"trustDirectories":[fixture._dir.path()]}}),
        json!({"permissions":{"additionalDirectories":[fixture.cwd]}}),
        json!({"security":{"folderTrust":{"enabled":false}}}),
    ] {
        fixture.write(&fixture.settings, &value);
        assert_eq!(fixture.trust(), Some(FolderTrust::Decided));
    }
}

#[test]
fn folder_trust_preview_appends_canonical_cwd_preserving_hooks() {
    let fixture = Fixture::new();
    let partial = fixture.cwd.with_file_name("proj");
    std::fs::create_dir_all(&partial).unwrap();
    fixture.write(
        &fixture.settings,
        &json!({"hooks":{"Stop":[]},"permissions":{"trustDirectories":[partial]}}),
    );
    let trust = fixture.trust();
    assert!(matches!(trust, Some(FolderTrust::Undecided(_))));
    let Some(FolderTrust::Undecided(gap)) = trust else {
        unreachable!()
    };
    assert_eq!(gap.key, fixture.cwd.canonicalize().unwrap());
    let preview = gap.grant.as_ref().unwrap();
    let candidate: Value = serde_json::from_str(&preview.candidate).unwrap();
    assert_eq!(candidate["hooks"], json!({"Stop":[]}));
    assert_eq!(
        candidate["permissions"]["trustDirectories"],
        json!([partial, fixture.cwd.canonicalize().unwrap()])
    );
    crate::agents::grant_folder_trust(&gap).unwrap();
    assert_eq!(fixture.trust(), Some(FolderTrust::Decided));
}

#[test]
fn malformed_folder_trust_is_not_rewritten() {
    let fixture = Fixture::new();
    for raw in [
        "not json",
        "{ /* comments */ }",
        "[]",
        r#"{"permissions":{"trustDirectories":false}}"#,
    ] {
        std::fs::write(&fixture.settings, raw).unwrap();
        assert!(matches!(fixture.trust(), Some(FolderTrust::Undecided(gap)) if gap.grant.is_err()));
        assert_eq!(std::fs::read_to_string(&fixture.settings).unwrap(), raw);
    }
}
