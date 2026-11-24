use super::*;
use crate::agents::ManagedIntegration as _;
use crate::agents::testkit::{hook_lifecycle, hook_observation, hook_output, hook_signal};
use crate::agents::{
    AgentStatus, LocalSessionObservation, LocalSessionProjection, LocalSessionState, PriceBook,
    StatusLineChange, TranscriptRole, TurnPhase,
};
use crate::ids::AgentSessionId;
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const CHILD_ALPHA: &str = "15b124d3-7753-412b-988b-88b2cd518cf8";
const CHILD_BETA: &str = "c86b1a56-5c62-43b4-9055-102461a074ef";
const NESTED_CHILD: &str = "44444444-4444-4444-8444-444444444444";
const AT_09: &str = "2026-07-13T23:23:09Z";
const AT_10: &str = "2026-07-13T23:23:10Z";
const AT_11: &str = "2026-07-13T23:23:11Z";
const AT_12: &str = "2026-07-13T23:23:12Z";
const FLASH: &str = "Gemini 3.5 Flash";
const PRO: &str = "Gemini 3.1 Pro";
const SONNET: &str = "Claude Sonnet 4.6";
const TURBO: &str = "Gemini 3.5 Flash (Turbo)";
const MALFORMED_MODEL: &str = "Gemini 3.5 Flash (Low";
fn local_state(observation: &LocalSessionObservation) -> &LocalSessionState {
    let LocalSessionProjection::Lifecycle(state) = &observation.projection else {
        panic!("Antigravity discovery must project lifecycle")
    };
    state
}
fn hook_payload() -> Value {
    json_value(
        r#"{"conversationId":"11111111-1111-4111-8111-111111111111","workspacePaths":["/workspace/project"],"transcriptPath":"/tmp/transcript_full.jsonl","modelName":"Gemini 3.5 Flash"}"#,
    )
}
#[test]
fn observer_neutral_hooks_leave_pre_tool_policy_untouched() {
    let payload = hook_payload();
    assert_eq!(
        hook_output(&AntigravityAdapter, "PreInvocation", &Value::Null)
            .json_reply()
            .cloned(),
        Some(json!({}))
    );
    assert_eq!(
        hook_output(&AntigravityAdapter, "Stop", &Value::Null)
            .json_reply()
            .cloned(),
        Some(json!({"decision": ""}))
    );
    assert_eq!(
        hook_output(&AntigravityAdapter, "PreToolUse", &payload).class(),
        AgentHookClass::Unknown
    );
    assert_eq!(
        hook_output(&AntigravityAdapter, "PreToolUse", &Value::Null)
            .json_reply()
            .cloned(),
        None
    );
}
#[test]
fn turn_start_sanitizes_the_transcript_prompt() {
    let payload = with(&hook_payload(), [("invocationNum", json!(0))]);
    for raw in
        std::iter::once("a real request").chain(crate::agents::CONTROL_TAG_PREFIXES.iter().copied())
    {
        let decoded = decode_lifecycle_fields("PreInvocation", &payload, |_, _, _| {
            PromptEvidence::Prompt(raw.to_owned())
        });
        assert_eq!(
            decoded.lifecycle.unwrap().prompt.as_deref(),
            (raw == "a real request").then_some(raw),
        );
    }
}

#[test]
fn native_hooks_normalize_lifecycle() {
    let common = hook_payload();
    let tool = |effect| LifecycleSignal::ToolUsed {
        effect,
        name: None,
        native_key: None,
        turn_id: None,
    };
    let ended = |errored, parked_on_background| LifecycleSignal::TurnEnded {
        turn_id: None,
        errored,
        parked_on_background,
    };
    let started = hook_lifecycle(
        &AntigravityAdapter,
        "PreInvocation",
        &with(&common, [("invocationNum", json!(0))]),
    );
    assert_eq!(
        started.signal,
        LifecycleSignal::TurnStarted { turn_id: None }
    );
    assert_eq!(started.agent_id.as_deref(), Some(SESSION_ID));
    assert_eq!(started.worktree_path.as_deref(), Some("/workspace/project"));
    assert_eq!(
        started.transcript_path.as_deref(),
        Some("/tmp/transcript_full.jsonl")
    );
    assert_eq!(started.launch.model.as_deref(), Some(FLASH));
    assert_eq!(started.launch.effort, None);
    for (model_name, model, effort) in [
        (
            "gemini-4-hydrogen-xhigh",
            "gemini-4-hydrogen",
            Some("xhigh"),
        ),
        ("gemini-3.8-flash-high", "gemini-3.8-flash", Some("high")),
        (
            "gemini-pro-orchestrator-neon",
            "gemini-pro-orchestrator-neon",
            None,
        ),
    ] {
        let suffixed = hook_lifecycle(
            &AntigravityAdapter,
            "PreInvocation",
            &with(
                &common,
                [
                    ("invocationNum", json!(0)),
                    ("modelName", json!(model_name)),
                ],
            ),
        );
        assert_eq!(
            (
                suffixed.launch.model.as_deref(),
                suffixed.launch.effort.as_deref()
            ),
            (Some(model), effort)
        );
    }
    for (event, payload, expected) in [
        (
            "PreInvocation",
            with(&common, [("invocationNum", json!(1))]),
            None,
        ),
        (
            "PostToolUse:edit",
            with(&common, [("error", json!(""))]),
            Some(tool(ToolEffect::Editing)),
        ),
        (
            "PostToolUse:mutating",
            with(&common, [("error", json!(null))]),
            Some(tool(ToolEffect::Mutating)),
        ),
        (
            "PostToolUse:edit",
            with(&common, [("error", json!("write failed"))]),
            Some(tool(ToolEffect::Observed)),
        ),
        (
            "Stop",
            with(
                &common,
                [
                    ("terminationReason", json!("model_stop")),
                    ("error", json!("")),
                    ("fullyIdle", json!(false)),
                ],
            ),
            Some(ended(false, true)),
        ),
        (
            "Stop",
            with(
                &common,
                [
                    ("terminationReason", json!("max_steps_exceeded")),
                    ("fullyIdle", json!(true)),
                ],
            ),
            Some(ended(true, false)),
        ),
        (
            "Stop",
            with(&common, [("terminationReason", json!("model_stop"))]),
            None,
        ),
        ("Stop", json!({"fullyIdle": true}), None),
    ] {
        let signal =
            hook_observation(&AntigravityAdapter, event, &payload).map(|value| value.signal);
        assert_eq!(signal, expected);
    }
}

#[test]
fn bare_compact_hooks_bracket_a_manual_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &[
            user_record(0, AT_09, "<USER_REQUEST>\nremember maple\n</USER_REQUEST>"),
            record(1, "MODEL", "PLANNER_RESPONSE", "DONE", AT_10, "noted.").to_string(),
        ]
        .join("\n"),
    );
    let common = with(&hook_payload(), [("transcriptPath", json!(transcript))]);
    let closed = |failed| LifecycleSignal::CompactionEnded {
        auto: Some(false),
        failed,
    };
    let ended = |errored, parked_on_background| LifecycleSignal::TurnEnded {
        errored,
        parked_on_background,
        turn_id: None,
    };
    for input in ["/compact", "  /compact\n"] {
        // Without `initialNumSteps`, `lastUserInput` decides the opener without a read.
        let opened = observe_lifecycle_with_prompt_reader(
            "PreInvocation",
            &with(
                &common,
                [("invocationNum", json!(0)), ("lastUserInput", json!(input))],
            ),
            |_, _, _| panic!("an unbounded compaction reads no prompt"),
        )
        .unwrap();
        assert_eq!(opened.signal, LifecycleSignal::Compacting, "{input:?}");
        assert!(opened.prompt.is_none());
        for (stop, expected, expected_message) in [
            (
                with(
                    &common,
                    [
                        ("lastUserInput", json!(input)),
                        ("terminationReason", json!("TERMINAL_CUSTOM_HOOK")),
                        ("fullyIdle", json!(true)),
                    ],
                ),
                closed(false),
                None,
            ),
            (
                with(
                    &common,
                    [
                        ("lastUserInput", json!(input)),
                        ("terminationReason", json!("terminal_custom_hook")),
                    ],
                ),
                closed(false),
                None,
            ),
            (
                with(
                    &common,
                    [
                        ("lastUserInput", json!(input)),
                        ("terminationReason", json!("TERMINAL_CUSTOM_HOOK")),
                        ("error", json!("compaction failed")),
                        ("fullyIdle", json!(true)),
                    ],
                ),
                closed(true),
                None,
            ),
            (
                with(
                    &common,
                    [
                        ("lastUserInput", json!(input)),
                        ("terminationReason", json!("error")),
                        ("fullyIdle", json!(true)),
                    ],
                ),
                ended(true, false),
                Some("noted."),
            ),
            (
                with(
                    &common,
                    [
                        ("lastUserInput", json!(input)),
                        ("terminationReason", json!("NO_TOOL_CALL")),
                        ("fullyIdle", json!(false)),
                    ],
                ),
                ended(false, true),
                Some("noted."),
            ),
        ] {
            let decoded = AntigravityAdapter.decode_hook("Stop", &stop).unwrap();
            assert_eq!(
                decoded.lifecycle().map(|observation| &observation.signal),
                Some(&expected),
                "{stop}"
            );
            assert_eq!(decoded.final_message(), expected_message, "{stop}");
            assert_eq!(
                hook_output(&AntigravityAdapter, "Stop", &stop)
                    .json_reply()
                    .cloned(),
                Some(json!({"decision": ""}))
            );
        }
    }
    let ended = AntigravityAdapter
        .decode_hook(
            "Stop",
            &with(
                &common,
                [
                    ("terminationReason", json!("NO_TOOL_CALL")),
                    ("fullyIdle", json!(true)),
                ],
            ),
        )
        .unwrap();
    assert_eq!(ended.final_message(), Some("noted."));
}

#[test]
fn other_inputs_keep_the_turn_bracket() {
    let common = hook_payload();
    let ended = LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    };
    for input in [
        Some(json!("/compact keep the notes")),
        Some(json!("Reply exactly: noted.")),
        Some(json!("/compactor")),
        None,
    ] {
        let mut pre = with(&common, [("invocationNum", json!(0))]);
        let mut stop = with(
            &common,
            [
                ("terminationReason", json!("NO_TOOL_CALL")),
                ("fullyIdle", json!(true)),
            ],
        );
        if let Some(input) = &input {
            pre["lastUserInput"] = input.clone();
            stop["lastUserInput"] = input.clone();
        }
        let started = observe_lifecycle_with_prompt_reader("PreInvocation", &pre, |_, _, _| {
            Some("asked".into())
        })
        .unwrap();
        assert_eq!(
            (started.signal, started.prompt.as_deref()),
            (
                LifecycleSignal::TurnStarted { turn_id: None },
                Some("asked")
            ),
            "{input:?}"
        );
        assert_eq!(
            hook_signal(&AntigravityAdapter, "Stop", &stop),
            ended.clone(),
            "{input:?}"
        );
    }
}

#[test]
fn post_tool_use_tool_call_names_the_tool_when_present() {
    let named = |name: Option<&str>| LifecycleSignal::ToolUsed {
        effect: ToolEffect::Observed,
        name: name.map(ToOwned::to_owned),
        native_key: None,
        turn_id: None,
    };
    for (tool_call, expected) in [
        (
            json!({"name": " browser_click_element ", "args": {"Index": 3}}),
            Some("browser_click_element"),
        ),
        (json!({"name": "  "}), None),
        (json!({"args": {}}), None),
        (json!(null), None),
    ] {
        let payload = with(
            &hook_payload(),
            [("toolCall", tool_call), ("stepIdx", json!(4))],
        );
        assert_eq!(
            hook_signal(&AntigravityAdapter, "PostToolUse:observed", &payload),
            named(expected)
        );
    }
    let edited = hook_signal(
        &AntigravityAdapter,
        "PostToolUse:edit",
        &with(
            &hook_payload(),
            [("toolCall", json!({"name": "write_to_file"}))],
        ),
    );
    assert!(matches!(
        edited,
        LifecycleSignal::ToolUsed { effect: ToolEffect::Editing, name: Some(ref name), .. }
            if name == "write_to_file"
    ));
}

#[test]
fn catalog_events_without_lifecycle_signal_keep_context_identity() {
    let camel = hook_output(&AntigravityAdapter, "PostInvocation", &hook_payload());
    assert_eq!(
        camel.event_agent_id().map(AgentSessionId::as_str),
        Some(SESSION_ID)
    );
    assert_eq!(
        camel.context_agent_id().map(AgentSessionId::as_str),
        Some(SESSION_ID)
    );
    assert_eq!(camel.routing().worktree_path(), Some("/workspace/project"));
    assert!(camel.lifecycle().cloned().is_none());

    let snake = hook_output(
        &AntigravityAdapter,
        "PostInvocation",
        &json!({
            "conversation_id": SESSION_ID,
            "conversationId": "camel-fallback",
            "workspacePaths": ["/workspace/project"],
        }),
    );
    assert_eq!(
        snake.event_agent_id().map(AgentSessionId::as_str),
        Some(SESSION_ID)
    );
    assert_eq!(
        snake.context_agent_id().map(AgentSessionId::as_str),
        Some(SESSION_ID)
    );
    assert!(snake.lifecycle().cloned().is_none());
}
#[test]
fn post_tool_use_matchers_route_legacy_and_current_tool_names() {
    for (tool, event, effect) in [
        ("write_to_file", "PostToolUse:edit", ToolEffect::Editing),
        (
            "replace_file_content",
            "PostToolUse:edit",
            ToolEffect::Editing,
        ),
        (
            "multi_replace_file_content",
            "PostToolUse:edit",
            ToolEffect::Editing,
        ),
        ("write_file", "PostToolUse:edit", ToolEffect::Editing),
        ("edit_file", "PostToolUse:edit", ToolEffect::Editing),
        ("run_command", "PostToolUse:mutating", ToolEffect::Mutating),
        ("run_workflow", "PostToolUse:mutating", ToolEffect::Mutating),
        (
            "call_mcp_tool",
            "PostToolUse:mutating",
            ToolEffect::Mutating,
        ),
        ("view_file", "PostToolUse:observed", ToolEffect::Observed),
        ("read_file", "PostToolUse:observed", ToolEffect::Observed),
        ("search_web", "PostToolUse:observed", ToolEffect::Observed),
        (
            "invoke_subagent",
            "PostToolUse:observed",
            ToolEffect::Observed,
        ),
        (
            "browser_click_element",
            "PostToolUse:observed",
            ToolEffect::Observed,
        ),
    ] {
        assert_eq!(post_tool_event(tool), Some(event), "{tool}");
        let payload = with(&hook_payload(), [("toolCall", json!({"name": tool}))]);
        assert_eq!(
            hook_signal(&AntigravityAdapter, event, &payload),
            LifecycleSignal::ToolUsed {
                effect,
                name: Some(tool.to_owned()),
                native_key: None,
                turn_id: None,
            },
            "{tool}"
        );
    }
    for tool in ["mcp_rimz_tools_lsp", "unknown_tool"] {
        assert_eq!(post_tool_event(tool), None, "{tool}");
    }
}
#[test]
fn untyped_stop_errors_stay_terminal_and_cannot_arm_recovery() {
    for error in [
        json!("rate limit reached; retry later"),
        json!({"message": "quota exhausted", "retryAfterSeconds": 30}),
    ] {
        let payload = with(
            &hook_payload(),
            [
                ("terminationReason", json!("error")),
                ("fullyIdle", json!(true)),
                ("error", error),
            ],
        );
        assert!(
            hook_output(&AntigravityAdapter, "Stop", &payload)
                .turn_error()
                .cloned()
                .is_none()
        );
        assert_eq!(
            hook_signal(&AntigravityAdapter, "Stop", &payload),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
                turn_id: None,
            }
        );
    }
}
#[test]
fn stop_termination_reasons_fail_only_on_errors_and_limits() {
    let ended = |errored, parked_on_background| LifecycleSignal::TurnEnded {
        errored,
        parked_on_background,
        turn_id: None,
    };
    for (reason, failed) in [
        ("ERROR", true),
        ("MAX_INVOCATIONS", true),
        ("MAX_FORCED_INVOCATIONS", true),
        ("MAX_TOKEN_BUDGET_EXCEEDED", true),
        ("Max_Token_Budget_Exceeded", true),
        ("error", true),
        ("max_steps_exceeded", true),
        (" Max_Steps_Exceeded ", true),
        ("NO_TOOL_CALL", false),
        ("USER_CANCELED", false),
        ("TERMINAL_STEP_TYPE", false),
        ("TERMINAL_CUSTOM_HOOK", false),
        ("terminal_custom_hook", false),
        ("INJECTED_RESPONSE", false),
        ("no_tool_call", false),
        ("SOMETHING_NEW", false),
    ] {
        for fully_idle in [true, false] {
            let payload = with(
                &hook_payload(),
                [
                    ("terminationReason", json!(reason)),
                    ("fullyIdle", json!(fully_idle)),
                    ("error", json!("")),
                ],
            );
            assert_eq!(
                hook_signal(&AntigravityAdapter, "Stop", &payload),
                ended(failed, !fully_idle && !failed),
                "{reason} fullyIdle={fully_idle}"
            );
            let compact = with(&payload, [("lastUserInput", json!("/compact"))]);
            let expected = if reason.trim().eq_ignore_ascii_case("TERMINAL_CUSTOM_HOOK") {
                LifecycleSignal::CompactionEnded {
                    auto: Some(false),
                    failed,
                }
            } else {
                ended(failed, !fully_idle && !failed)
            };
            assert_eq!(
                hook_signal(&AntigravityAdapter, "Stop", &compact),
                expected,
                "{reason} with /compact fullyIdle={fully_idle}"
            );
        }
        let compact_no_idle = with(
            &hook_payload(),
            [
                ("terminationReason", json!(reason)),
                ("lastUserInput", json!("/compact")),
            ],
        );
        let expected_no_idle = reason
            .trim()
            .eq_ignore_ascii_case("TERMINAL_CUSTOM_HOOK")
            .then_some(LifecycleSignal::CompactionEnded {
                auto: Some(false),
                failed,
            });
        assert_eq!(
            hook_observation(&AntigravityAdapter, "Stop", &compact_no_idle)
                .map(|observation| observation.signal),
            expected_no_idle,
            "{reason} with /compact and no fullyIdle"
        );
    }
}
struct AgyHome {
    _dir: tempfile::TempDir,
    paths: install::Paths,
    plugin: PathBuf,
    config: PathBuf,
}

impl AgyHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join(".gemini/config");
        let settings_dir = dir.path().join(".gemini/antigravity-cli");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&settings_dir).unwrap();
        Self {
            paths: install::Paths {
                hooks: config_dir.join("hooks.json"),
                settings: settings_dir.join("settings.json"),
                state: dir.path().join("rimz/antigravity-statusline.json"),
            },
            plugin: config_dir.join("plugins/rimz"),
            config: config_dir.join("config.json"),
            _dir: dir,
        }
    }

    fn integration(&self) -> &'static install::AntigravityManagedIntegration {
        Box::leak(Box::new(install::AntigravityManagedIntegration::at(
            self.paths.clone(),
        )))
    }

    fn plugin_hooks(&self) -> PathBuf {
        self.plugin.join("hooks.json")
    }

    fn manifest(&self) -> PathBuf {
        self.plugin.join("plugin.json")
    }

    fn mcp_config(&self) -> PathBuf {
        self.plugin.join("mcp_config.json")
    }

    fn snapshot(&self) -> Vec<Option<Vec<u8>>> {
        [
            self.plugin_hooks(),
            self.mcp_config(),
            self.manifest(),
            self.paths.hooks.clone(),
            self.paths.settings.clone(),
            self.paths.state.clone(),
        ]
        .iter()
        .map(|path| std::fs::read(path).ok())
        .collect()
    }
}

/// The global-file \`rimz\` hook earlier RimZ releases merged into hooks.json.
fn legacy_global_rimz_hook() -> Value {
    let mut rimz = serde_json::Map::new();
    for hook in &ANTIGRAVITY_HOOKS {
        let handler = json!({"type": "command", "command": hook.command, "timeout": 5});
        let entry = match hook.config_matcher {
            Some(matcher) => json!({"matcher": matcher, "hooks": [handler]}),
            None => handler,
        };
        rimz.entry(hook.config_event.to_owned())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap()
            .push(entry);
    }
    Value::Object(rimz)
}

fn rimz_statusline(extra: Value) -> Value {
    let mut statusline = json!({"type": "command", "command": STATUS_LINE_COMMAND});
    for (key, value) in extra.as_object().unwrap() {
        statusline[key] = value.clone();
    }
    statusline
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
}

fn env() -> BTreeMap<String, String> {
    BTreeMap::new()
}

fn report_paths(files: &[crate::agents::HookInstallFileReport]) -> Vec<(PathBuf, bool)> {
    files
        .iter()
        .map(|file| (file.path.clone(), file.existed))
        .collect()
}

#[test]
fn fresh_install_writes_the_plugin_and_settings_and_no_global_hooks() {
    let home = AgyHome::new();
    let managed = home.integration();
    assert!(!managed.installed(&env()));
    assert!(!managed.managed_artifacts_present(&env()));
    let preview = managed.preview(&env(), Default::default()).unwrap();
    assert_eq!(preview.planned_events, ANTIGRAVITY_EVENT_NAMES);
    assert_eq!(preview.status_line_change, Some(StatusLineChange::Added));
    let previewed: Vec<_> = preview.files.iter().map(|file| file.path.clone()).collect();
    assert_eq!(
        previewed,
        [
            home.plugin_hooks(),
            home.mcp_config(),
            home.manifest(),
            home.paths.settings.clone()
        ]
    );
    let report = managed.install(&env(), Default::default()).unwrap();
    assert_eq!(report.installed_events, ANTIGRAVITY_EVENT_NAMES);
    assert_eq!(
        report_paths(&report.files),
        [
            (home.plugin_hooks(), false),
            (home.mcp_config(), false),
            (home.manifest(), false),
            (home.paths.settings.clone(), false),
        ]
    );
    assert!(!home.paths.hooks.exists());
    assert_eq!(
        read_json(&home.mcp_config()),
        json!({"mcpServers": {"tools": {
            "command": "rimz", "args": ["mcp", "serve", "--launch-env"]
        }}})
    );
    assert_eq!(
        read_json(&home.paths.settings)["permissions"],
        json!({"allow": ["mcp(rimz_tools/*)"]})
    );
    assert!(!home.paths.state.exists());
    assert!(!home.config.exists());
    let manifest = read_json(&home.manifest());
    assert_eq!(manifest["name"], "rimz");
    assert_eq!(manifest["author"], "RimZ");
    let hooks = read_json(&home.plugin_hooks());
    assert_eq!(hooks, json!({"rimz": legacy_global_rimz_hook()}));
    assert!(hooks["rimz"].get("PreToolUse").is_none());
    assert_eq!(
        read_json(&home.paths.settings)["statusLine"],
        rimz_statusline(json!({"stack_with_default": true}))
    );
    assert!(managed.installed(&env()));
    assert!(
        managed
            .outdated_files(&env(), Default::default())
            .is_empty()
    );
    let once = home.snapshot();
    managed.install(&env(), Default::default()).unwrap();
    assert_eq!(home.snapshot(), once);
}

#[test]
fn a_legacy_install_agy_stripped_migrates_into_the_plugin() {
    let home = AgyHome::new();
    let mine = json!({"Stop": [{"type": "command", "command": "my-stop", "timeout": 9}]});
    write_json(
        &home.paths.hooks,
        &json!({"mine": mine, "rimz": legacy_global_rimz_hook()}),
    );
    write_json(
        &home.paths.settings,
        &json!({
            "colorScheme": "tokyo night",
            "permissions": {"allow": ["mcp(rimz_tools/*)"]},
            "statusLine": rimz_statusline(json!({"stack_with_default": true}))
        }),
    );
    let settings_before = std::fs::read(&home.paths.settings).unwrap();
    let managed = home.integration();
    assert!(!managed.installed(&env()));
    assert!(managed.managed_artifacts_present(&env()));
    assert_eq!(
        managed.outdated_files(&env(), Default::default()),
        std::slice::from_ref(&home.paths.hooks)
    );
    let preview = managed.preview(&env(), Default::default()).unwrap();
    let global = preview
        .files
        .iter()
        .find(|file| file.path == home.paths.hooks)
        .expect("the stripped global hooks file is previewed");
    assert!(!global.candidate.contains("rimz hooks feed"));
    assert_eq!(
        preview.status_line_change,
        Some(StatusLineChange::Unchanged)
    );
    managed.install(&env(), Default::default()).unwrap();
    assert_eq!(read_json(&home.paths.hooks), json!({"mine": mine}));
    assert_eq!(
        std::fs::read(&home.paths.settings).unwrap(),
        settings_before
    );
    assert!(!home.paths.state.exists());
    assert!(managed.installed(&env()));
    assert!(
        managed
            .outdated_files(&env(), Default::default())
            .is_empty()
    );
    let once = home.snapshot();
    managed.install(&env(), Default::default()).unwrap();
    assert_eq!(home.snapshot(), once);
}

#[test]
fn a_legacy_marked_statusline_moves_its_original_into_the_record() {
    let home = AgyHome::new();
    let original =
        json!({"type": "command", "command": "my-statusline --compact", "custom": "kept"});
    write_json(
        &home.paths.settings,
        &json!({"statusLine": rimz_statusline(json!({
            "stack_with_default": false,
            "_rimz_managed": true,
            "_rimz_wrapped": original,
        }))}),
    );
    let managed = home.integration();
    assert_eq!(
        managed.wrapped_status_line_command(&env()).as_deref(),
        Some("my-statusline --compact")
    );
    assert!(
        managed
            .outdated_files(&env(), Default::default())
            .contains(&home.paths.settings)
    );
    let preview = managed.preview(&env(), Default::default()).unwrap();
    assert!(
        preview
            .files
            .iter()
            .any(|file| file.path == home.paths.state)
    );
    let report = managed.install(&env(), Default::default()).unwrap();
    assert!(
        report_paths(&report.files).contains(&(home.paths.state.clone(), false)),
        "the displaced-statusline record is a receipt row"
    );
    assert_eq!(
        read_json(&home.paths.state),
        json!({"statusLine": original})
    );
    assert_eq!(
        read_json(&home.paths.settings)["statusLine"],
        rimz_statusline(json!({"stack_with_default": false}))
    );
    assert_eq!(
        managed.wrapped_status_line_command(&env()).as_deref(),
        Some("my-statusline --compact")
    );
    assert!(
        managed
            .outdated_files(&env(), Default::default())
            .is_empty()
    );
    managed.uninstall(&env()).unwrap();
    assert_eq!(read_json(&home.paths.settings)["statusLine"], original);
    assert!(!home.paths.state.exists());
    assert!(!home.plugin.exists());
    assert!(!managed.managed_artifacts_present(&env()));
}

#[test]
fn a_complete_plugin_beside_a_surviving_global_handler_is_not_installed() {
    let home = AgyHome::new();
    let managed = home.integration();
    managed.install(&env(), Default::default()).unwrap();
    write_json(
        &home.paths.hooks,
        &json!({"rimz": legacy_global_rimz_hook()}),
    );
    assert!(!managed.installed(&env()));
    assert_eq!(
        managed.outdated_files(&env(), Default::default()),
        std::slice::from_ref(&home.paths.hooks)
    );
    managed.install(&env(), Default::default()).unwrap();
    assert_eq!(read_json(&home.paths.hooks), json!({}));
    assert!(managed.installed(&env()));
}

#[test]
fn a_user_rimz_hook_in_the_global_file_stays_untouched() {
    let home = AgyHome::new();
    std::fs::write(
        &home.paths.hooks,
        r#"{"rimz":{"Stop":[{"type":"command","command":"user-command"}]}}"#,
    )
    .unwrap();
    let before = std::fs::read(&home.paths.hooks).unwrap();
    let managed = home.integration();
    assert!(
        managed
            .outdated_files(&env(), Default::default())
            .is_empty()
    );
    let preview = managed.preview(&env(), Default::default()).unwrap();
    assert!(
        preview
            .files
            .iter()
            .all(|file| file.path != home.paths.hooks)
    );
    managed.install(&env(), Default::default()).unwrap();
    assert_eq!(std::fs::read(&home.paths.hooks).unwrap(), before);
    assert!(managed.installed(&env()));
    managed.uninstall(&env()).unwrap();
    assert_eq!(std::fs::read(&home.paths.hooks).unwrap(), before);
}

#[test]
fn a_plugin_path_rimz_does_not_own_is_refused_and_left() {
    for case in [
        "file",
        "symlink",
        "foreign manifest",
        "manifest-less user hooks",
        "manifest-less user server",
        "symlinked plugin file",
    ] {
        let home = AgyHome::new();
        std::fs::create_dir_all(home.plugin.parent().unwrap()).unwrap();
        match case {
            "file" => std::fs::write(&home.plugin, "not a plugin").unwrap(),
            "symlink" => {
                let target = home.plugin.with_file_name("elsewhere");
                std::fs::create_dir_all(&target).unwrap();
                std::os::unix::fs::symlink(&target, &home.plugin).unwrap();
            }
            "manifest-less user hooks" => write_json(
                &home.plugin_hooks(),
                &json!({"mine": {"Stop": [{"type": "command", "command": "my-stop"}]}}),
            ),
            "manifest-less user server" => write_json(
                &home.mcp_config(),
                &json!({"mcpServers": {"mine": {"command": "my-server"}}}),
            ),
            "symlinked plugin file" => {
                write_json(&home.manifest(), &json!({"name": "rimz", "author": "RimZ"}));
                let target = home.plugin.with_file_name("elsewhere.json");
                std::fs::write(&target, "{}").unwrap();
                std::os::unix::fs::symlink(&target, home.plugin_hooks()).unwrap();
            }
            _ => write_json(
                &home.manifest(),
                &json!({"name": "rimz", "author": "someone else"}),
            ),
        }
        write_json(&home.paths.settings, &json!({"theme": "dark"}));
        let before = home.snapshot();
        let managed = home.integration();
        let error = managed.install(&env(), Default::default()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&home.plugin.display().to_string()),
            "{case}: {error}"
        );
        assert!(
            managed.preview(&env(), Default::default()).is_err(),
            "{case}"
        );
        assert_eq!(home.snapshot(), before, "{case}");
        assert!(!managed.installed(&env()), "{case}");
        assert!(!managed.managed_artifacts_present(&env()), "{case}");
        let report = managed.uninstall(&env()).unwrap();
        assert!(
            report.files.is_empty() && report.removed_events.is_empty(),
            "{case}: uninstall reports Antigravity untouched: {report:?}"
        );
        assert!(
            std::fs::symlink_metadata(&home.plugin).is_ok(),
            "{case}: uninstall leaves a plugin path RimZ does not own"
        );
        assert_eq!(home.snapshot(), before, "{case}");
    }
}

/// Antigravity's spec, with the installer pointed at a scratch home.
struct ScratchAgy {
    managed: &'static install::AntigravityManagedIntegration,
}

impl CoreCapability for ScratchAgy {
    fn spec(&self) -> &'static AgentSpec {
        &ANTIGRAVITY_DESCRIPTOR
    }
}
impl HookCapability for ScratchAgy {}
impl LaunchCapability for ScratchAgy {}
impl SessionCapability for ScratchAgy {}
impl TranscriptCapability for ScratchAgy {}
impl ContextCapability for ScratchAgy {}
impl AccountCapability for ScratchAgy {}
impl SpendingCapability for ScratchAgy {}
impl RuntimeControlCapability for ScratchAgy {}
impl InstallationCapability for ScratchAgy {
    fn managed_integration(&self) -> Option<&'static dyn crate::agents::ManagedIntegration> {
        Some(self.managed)
    }
}

#[test]
fn a_disabled_plugin_is_reported_and_preflight_returns_it_before_reinstalling() {
    let home = AgyHome::new();
    let managed = home.integration();
    let definition =
        crate::agents::AgentDefinition::new(Box::leak(Box::new(ScratchAgy { managed })));
    for config in [
        None,
        Some("{\"plugins\":{\"rimz\":{\"enabled\":true}}}"),
        Some("{\"plugins\":{\"other\":{\"enabled\":false}}}"),
        Some("not json"),
        Some("\u{feff}{\"plugins\":{\"rimz\":{\"enabled\":false}},,}"),
        Some("{\"plugins\":{\"rimz\":{\"enabled\":\"false\"}}}"),
    ] {
        if let Some(config) = config {
            std::fs::write(&home.config, config).unwrap();
        }
        assert!(managed.hooks_disabled(&env()).is_none(), "{config:?}");
    }
    std::fs::write(
        &home.config,
        "\u{feff}{\n  // agy writes this file\n  \"plugins\": {\"rimz\": {\"enabled\": false}},\n}\n",
    )
    .unwrap();
    for installed in [false, true] {
        if installed {
            managed.install(&env(), Default::default()).unwrap();
            assert!(
                managed.installed(&env()),
                "installed stays a statement about files"
            );
        }
        let Some(crate::agents::HookPreflightErr::HooksDisabled { reason, fix }) =
            managed.hooks_disabled(&env())
        else {
            panic!("a disabled plugin must answer hooks_disabled");
        };
        assert!(
            reason.contains(&home.config.display().to_string()),
            "{reason}"
        );
        assert!(reason.contains("plugins.rimz.enabled"), "{reason}");
        assert_eq!(fix, "agy plugin enable rimz");
        let preflight = crate::agents::preflight_hooks(
            &definition,
            &env(),
            crate::agents::TurnLifecycleNeed::None,
        );
        assert!(
            matches!(preflight, Err(crate::agents::HookPreflightErr::HooksDisabled { ref fix, .. }) if fix == "agy plugin enable rimz"),
            "installed={installed}: {preflight:?}"
        );
    }
}

#[test]
fn a_statusline_the_user_replaced_is_left_by_uninstall_and_wrapped_by_install() {
    let home = AgyHome::new();
    let managed = home.integration();
    managed.install(&env(), Default::default()).unwrap();
    let replaced = json!({"type": "command", "command": "user-statusline"});
    write_json(&home.paths.settings, &json!({"statusLine": replaced}));
    assert!(!managed.installed(&env()));
    assert!(
        managed
            .outdated_files(&env(), Default::default())
            .is_empty()
    );
    managed.uninstall(&env()).unwrap();
    assert_eq!(read_json(&home.paths.settings)["statusLine"], replaced);
    let preview = managed.preview(&env(), Default::default()).unwrap();
    assert_eq!(
        preview.status_line_change,
        Some(StatusLineChange::Wrapping {
            original: "user-statusline".to_owned()
        })
    );
    managed.install(&env(), Default::default()).unwrap();
    assert_eq!(
        read_json(&home.paths.state),
        json!({"statusLine": replaced})
    );
    assert_eq!(
        managed.wrapped_status_line_command(&env()).as_deref(),
        Some("user-statusline")
    );
    managed.uninstall(&env()).unwrap();
    assert_eq!(read_json(&home.paths.settings)["statusLine"], replaced);
}

#[test]
fn hook_install_accepts_only_object_statuslines() {
    let home = AgyHome::new();
    let managed = home.integration();
    let object = json!({"custom": "kept", "stack_with_default": false});
    write_json(&home.paths.settings, &json!({"statusLine": object}));
    assert_eq!(
        managed
            .preview(&env(), Default::default())
            .unwrap()
            .status_line_change,
        Some(StatusLineChange::Added)
    );
    managed.install(&env(), Default::default()).unwrap();
    let installed = read_json(&home.paths.settings);
    assert_eq!(installed["statusLine"]["stack_with_default"], false);
    assert_eq!(read_json(&home.paths.state)["statusLine"], object);
    managed.uninstall(&env()).unwrap();
    assert_eq!(read_json(&home.paths.settings)["statusLine"], object);

    let home = AgyHome::new();
    let managed = home.integration();
    std::fs::write(&home.paths.settings, br#"{"statusLine":"user command"}"#).unwrap();
    let before = home.snapshot();
    let error = managed.install(&env(), Default::default()).unwrap_err();
    assert!(error.to_string().contains("to be a JSON object"), "{error}");
    assert_eq!(home.snapshot(), before);
    assert!(!home.plugin.exists());
}

#[test]
fn uninstall_reaches_each_artifact_alone() {
    for artifact in ["plugin", "empty plugin dir", "global handler", "statusline"] {
        let home = AgyHome::new();
        let managed = home.integration();
        match artifact {
            "plugin" => {
                managed.install(&env(), Default::default()).unwrap();
                std::fs::remove_file(&home.paths.settings).unwrap();
            }
            "empty plugin dir" => std::fs::create_dir_all(&home.plugin).unwrap(),
            "global handler" => write_json(
                &home.paths.hooks,
                &json!({"mine": {"Stop": []}, "rimz": legacy_global_rimz_hook()}),
            ),
            _ => write_json(
                &home.paths.settings,
                &json!({"theme": "dark", "statusLine": rimz_statusline(json!({}))}),
            ),
        }
        assert!(managed.managed_artifacts_present(&env()), "{artifact}");
        let report = managed.uninstall(&env()).unwrap();
        assert!(!managed.managed_artifacts_present(&env()), "{artifact}");
        assert!(!home.plugin.exists(), "{artifact}");
        match artifact {
            "plugin" => assert_eq!(report.removed_events, ANTIGRAVITY_EVENT_NAMES),
            "empty plugin dir" => assert!(report.removed_events.is_empty(), "{report:?}"),
            "global handler" => {
                assert_eq!(report.removed_events, ANTIGRAVITY_EVENT_NAMES);
                assert_eq!(read_json(&home.paths.hooks), json!({"mine": {"Stop": []}}));
            }
            _ => assert_eq!(read_json(&home.paths.settings), json!({"theme": "dark"})),
        }
    }
    let report = AgyHome::new().integration().uninstall(&env()).unwrap();
    assert!(
        report.files.is_empty() && report.removed_events.is_empty(),
        "never installed: {report:?}"
    );
}

#[test]
fn agent_files_in_the_plugin_survive_reinstall_and_leave_with_uninstall() {
    let home = AgyHome::new();
    let managed = home.integration();
    managed.install(&env(), Default::default()).unwrap();
    let agent = home.plugin.join("agents/rimz-antigravity-000000000000.md");
    std::fs::create_dir_all(agent.parent().unwrap()).unwrap();
    std::fs::write(&agent, "---\nname: rimz-antigravity-000000000000\n---\n").unwrap();
    assert!(managed.installed(&env()));
    assert!(
        managed
            .outdated_files(&env(), Default::default())
            .is_empty()
    );
    managed.install(&env(), Default::default()).unwrap();
    assert!(agent.exists());
    managed.uninstall(&env()).unwrap();
    assert!(!home.plugin.exists());
}

#[test]
fn reinstall_after_the_statusline_key_is_deleted_drops_the_stale_record() {
    let home = AgyHome::new();
    let managed = home.integration();
    write_json(
        &home.paths.settings,
        &json!({"statusLine": {"type": "command", "command": "user-statusline"}}),
    );
    managed.install(&env(), Default::default()).unwrap();
    assert!(home.paths.state.exists());
    write_json(&home.paths.settings, &json!({"theme": "dark"}));
    let preview = managed.preview(&env(), Default::default()).unwrap();
    let record = preview
        .files
        .iter()
        .find(|file| file.path == home.paths.state)
        .expect("the record's removal is previewed");
    assert!(record.existed && record.candidate.is_empty());
    let report = managed.install(&env(), Default::default()).unwrap();
    assert!(!home.paths.state.exists());
    assert!(
        report
            .files
            .iter()
            .any(|file| file.path == home.paths.state && file.existed),
        "the receipt lists the removed record"
    );
    assert_eq!(managed.wrapped_status_line_command(&env()), None);
    managed.uninstall(&env()).unwrap();
    assert_eq!(read_json(&home.paths.settings), json!({"theme": "dark"}));
}

#[test]
fn a_failed_record_write_leaves_the_global_handlers_the_only_hook_set() {
    let home = AgyHome::new();
    write_json(
        &home.paths.hooks,
        &json!({"rimz": legacy_global_rimz_hook()}),
    );
    write_json(
        &home.paths.settings,
        &json!({"statusLine": {"type": "command", "command": "user-statusline"}}),
    );
    // A directory at the record's path fails its rename with EISDIR, root included.
    std::fs::create_dir_all(&home.paths.state).unwrap();
    let before = home.snapshot();
    let managed = home.integration();
    managed.install(&env(), Default::default()).unwrap_err();
    assert!(
        !home.plugin.exists(),
        "a plugin beside the global handlers fires twice"
    );
    assert_eq!(home.snapshot(), before);
}

#[test]
fn a_settings_file_install_leaves_alone_previews_without_a_diff() {
    let home = AgyHome::new();
    let managed = home.integration();
    managed.install(&env(), Default::default()).unwrap();
    let agy_formatted = serde_json::to_string(&read_json(&home.paths.settings)).unwrap();
    std::fs::write(&home.paths.settings, &agy_formatted).unwrap();
    let preview = managed.preview(&env(), Default::default()).unwrap();
    let settings = preview
        .files
        .iter()
        .find(|file| file.path == home.paths.settings)
        .expect("settings.json is previewed");
    assert_eq!(settings.original.as_deref(), Some(agy_formatted.as_str()));
    assert_eq!(settings.candidate, agy_formatted);
    managed.install(&env(), Default::default()).unwrap();
    assert_eq!(
        std::fs::read_to_string(&home.paths.settings).unwrap(),
        agy_formatted
    );
}

#[test]
fn a_bom_led_settings_file_reads_installed_and_a_rewrite_drops_the_bom() {
    let home = AgyHome::new();
    let managed = home.integration();
    managed.install(&env(), Default::default()).unwrap();
    let written = std::fs::read_to_string(&home.paths.settings).unwrap();
    std::fs::write(&home.paths.settings, format!("\u{feff}{written}")).unwrap();
    assert!(managed.installed(&env()));
    assert!(managed.managed_artifacts_present(&env()));
    assert!(
        managed
            .outdated_files(&env(), Default::default())
            .is_empty()
    );

    write_json(&home.paths.settings, &json!({"colorScheme": "tokyo night"}));
    let unowned = std::fs::read_to_string(&home.paths.settings).unwrap();
    std::fs::write(&home.paths.settings, format!("\u{feff}{unowned}")).unwrap();
    assert!(!managed.installed(&env()));
    managed.install(&env(), Default::default()).unwrap();
    let rewritten = std::fs::read_to_string(&home.paths.settings).unwrap();
    assert!(!rewritten.starts_with('\u{feff}'), "{rewritten:?}");
    assert_eq!(
        read_json(&home.paths.settings)["colorScheme"],
        "tokyo night"
    );
    assert!(managed.installed(&env()));

    std::fs::write(&home.paths.settings, "\u{feff}{\"statusLine\": ").unwrap();
    assert!(!managed.installed(&env()));
    let error = managed
        .install(&env(), Default::default())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&home.paths.settings.display().to_string()),
        "{error}"
    );
}

#[test]
fn outdated_files_name_each_drifted_file() {
    let home = AgyHome::new();
    let managed = home.integration();
    let outdated = || managed.outdated_files(&env(), Default::default());
    assert!(outdated().is_empty());
    managed.install(&env(), Default::default()).unwrap();
    assert!(outdated().is_empty());
    let canonical = std::fs::read(home.plugin_hooks()).unwrap();
    let mut drift = read_json(&home.plugin_hooks());
    drift["rimz"]["Stop"][0]["timeout"] = json!(999);
    write_json(&home.plugin_hooks(), &drift);
    assert_eq!(outdated(), std::slice::from_ref(&home.plugin_hooks()));
    assert!(!managed.installed(&env()));
    std::fs::write(home.plugin_hooks(), &canonical).unwrap();
    std::fs::remove_file(home.manifest()).unwrap();
    assert_eq!(outdated(), [home.manifest()]);
    managed.install(&env(), Default::default()).unwrap();
    let mut settings = read_json(&home.paths.settings);
    settings["statusLine"]["_rimz_managed"] = json!(true);
    write_json(&home.paths.settings, &settings);
    assert_eq!(outdated(), std::slice::from_ref(&home.paths.settings));
    write_json(
        &home.paths.settings,
        &json!({"statusLine": {"type": "command", "command": "echo user"}}),
    );
    std::fs::write(&home.paths.hooks, "not json").unwrap();
    assert!(outdated().is_empty());
}

#[test]
fn a_plugin_upgraded_from_one_without_its_mcp_config_is_missing_until_reinstalled() {
    let home = AgyHome::new();
    let managed = home.integration();
    managed.install(&env(), Default::default()).unwrap();
    let canonical = std::fs::read(home.mcp_config()).unwrap();
    for stale in [None, Some(r#"{"mcpServers": {}}"#)] {
        match stale {
            None => std::fs::remove_file(home.mcp_config()).unwrap(),
            Some(bytes) => std::fs::write(home.mcp_config(), bytes).unwrap(),
        }
        assert!(!managed.installed(&env()), "{stale:?}");
        assert!(managed.managed_artifacts_present(&env()), "{stale:?}");
        assert_eq!(
            managed.outdated_files(&env(), Default::default()),
            [home.mcp_config()],
            "{stale:?}"
        );
        let state = managed.shipped_mcp_server(&env()).unwrap().state;
        let expected = match stale {
            None => crate::agents::ShippedMcpState::Missing,
            Some(_) => crate::agents::ShippedMcpState::Drifted,
        };
        assert_eq!(state, expected);
        managed.install(&env(), Default::default()).unwrap();
        assert_eq!(std::fs::read(home.mcp_config()).unwrap(), canonical);
        assert!(managed.installed(&env()), "{stale:?}");
    }
    let shipped = managed.shipped_mcp_server(&env()).unwrap();
    assert_eq!(
        (shipped.name, shipped.config, shipped.state),
        (
            "rimz_tools",
            home.mcp_config(),
            crate::agents::ShippedMcpState::Current
        )
    );
    write_json(
        &home.config,
        &json!({"plugins": {"rimz": {"enabled": false}}}),
    );
    assert!(matches!(
        managed.shipped_mcp_server(&env()).unwrap().state,
        crate::agents::ShippedMcpState::Disabled { fix, .. } if fix == "agy plugin enable rimz"
    ));
    std::fs::remove_file(&home.config).unwrap();
    // An install stopped before its manifest leaves the hook and server files, which
    // are RimZ's to finish.
    std::fs::remove_file(home.manifest()).unwrap();
    assert!(managed.managed_artifacts_present(&env()));
    managed.install(&env(), Default::default()).unwrap();
    assert!(managed.installed(&env()));
}

#[test]
fn the_saved_allow_rule_follows_the_switch_and_keeps_the_users_rules() {
    const RULE: &str = "mcp(rimz_tools/*)";
    let on = crate::agents::HookInstallOptions {
        allow_routine_rimz: true,
    };
    let off = crate::agents::HookInstallOptions {
        allow_routine_rimz: false,
    };
    let home = AgyHome::new();
    let managed = home.integration();
    let permissions = || read_json(&home.paths.settings)["permissions"].clone();
    write_json(
        &home.paths.settings,
        &json!({"theme": "dark", "permissions": {"allow": ["command(ls)"], "deny": ["command(rm)"]}}),
    );
    let preview = managed.preview(&env(), on).unwrap();
    let settings = preview
        .files
        .iter()
        .find(|file| file.path == home.paths.settings)
        .unwrap();
    assert!(
        settings.candidate.contains(RULE),
        "the dry-run diff shows the rule"
    );
    managed.install(&env(), on).unwrap();
    assert_eq!(
        permissions(),
        json!({"allow": ["command(ls)", RULE], "deny": ["command(rm)"]})
    );
    assert_eq!(read_json(&home.paths.settings)["theme"], "dark");
    let once = home.snapshot();
    managed.install(&env(), on).unwrap();
    assert_eq!(home.snapshot(), once, "a reinstall writes no duplicate");

    let preview = managed.preview(&env(), off).unwrap();
    let settings = preview
        .files
        .iter()
        .find(|file| file.path == home.paths.settings)
        .unwrap();
    assert!(!settings.candidate.contains(RULE));
    managed.install(&env(), off).unwrap();
    assert_eq!(
        permissions(),
        json!({"allow": ["command(ls)"], "deny": ["command(rm)"]})
    );
    assert!(
        managed.installed(&env()),
        "a missing rule leaves the hooks installed"
    );
    assert!(managed.outdated_files(&env(), on).is_empty());

    managed.install(&env(), on).unwrap();
    managed.uninstall(&env()).unwrap();
    assert_eq!(
        permissions(),
        json!({"allow": ["command(ls)"], "deny": ["command(rm)"]})
    );

    for before in [
        json!({"theme": "dark"}),
        json!({"theme": "dark", "permissions": {"allow": [RULE]}}),
    ] {
        let home = AgyHome::new();
        let managed = home.integration();
        write_json(&home.paths.settings, &before);
        managed.install(&env(), on).unwrap();
        let mut installed = read_json(&home.paths.settings);
        installed.as_object_mut().unwrap().remove("statusLine");
        assert_eq!(
            installed["permissions"],
            json!({"allow": [RULE]}),
            "{before}"
        );
        managed.install(&env(), off).unwrap();
        let mut removed = read_json(&home.paths.settings);
        removed.as_object_mut().unwrap().remove("statusLine");
        assert_eq!(removed, json!({"theme": "dark"}), "{before}");
        managed.install(&env(), on).unwrap();
        managed.uninstall(&env()).unwrap();
        assert_eq!(
            read_json(&home.paths.settings),
            json!({"theme": "dark"}),
            "{before}"
        );
    }
}

#[test]
fn wiring_inputs_name_every_file_the_answers_read() {
    let home = AgyHome::new();
    let paths = home
        .integration()
        .wiring_input_paths(&ANTIGRAVITY_DESCRIPTOR, &env());
    for path in [
        home.plugin_hooks(),
        home.manifest(),
        home.paths.hooks.clone(),
        home.config.clone(),
        home.paths.settings.clone(),
        home.paths.state.clone(),
    ] {
        assert!(
            paths.contains(&path),
            "{} missing from {paths:?}",
            path.display()
        );
    }
}

#[test]
fn statusline_projects_model_account_and_context_usage() {
    let context = AntigravityAdapter
        .observe_context(
            "antigravity",
            &json_value(r#"{"conversation_id":"11111111-1111-4111-8111-111111111111","version":"1.1.2","model":{"id":"Gemini 3.5 Flash (Medium)","display_name":"Gemini 3.5 Flash (Medium)"},"plan_tier":"ultra","email":"user@example.com","tool_confirmation_pending":true,"context_window":{"context_window_size":1048576,"used_percentage":8.4156,"remaining_percentage":91.5844,"current_usage":{"input_tokens":63382,"output_tokens":346,"cache_creation_input_tokens":0,"cache_read_input_tokens":20857}},"future_field":{"ignored":true}}"#),
        )
        .unwrap()
        .context;
    assert_eq!(context.model_id.as_deref(), Some(FLASH));
    assert_eq!(context.model_display_name.as_deref(), Some(FLASH));
    assert_eq!(context.effort.as_deref(), Some("medium"));
    assert_eq!(context.agent_version.as_deref(), Some("1.1.2"));
    assert_eq!(
        context.settle.map(|settle| settle.outcome),
        Some(crate::agents::TurnSettleOutcome::NativeWait)
    );
    let account = context.account.unwrap();
    assert_eq!(
        (
            account.plan.as_deref(),
            account.account_id.as_deref(),
            account.metered
        ),
        (Some("ultra"), Some("user@example.com"), Some(true))
    );
    let tokens = context.tokens.unwrap();
    assert_eq!(
        (
            tokens.context_window_size,
            tokens.used_percentage,
            tokens.remaining_percentage,
            tokens.current_usage.unwrap().cache_read_input_tokens
        ),
        (Some(1_048_576), Some(8), Some(92), Some(20_857))
    );
    assert!(
        AntigravityAdapter
            .observe_context(
                "antigravity",
                &json!({"conversation_id": SESSION_ID, "tool_confirmation_pending": false})
            )
            .unwrap()
            .context
            .settle
            .is_none()
    );
}
#[test]
fn statusline_splits_reasoning_qualifiers_off_model_id_and_display_name() {
    for (display, model, effort, thinking) in [
        ("Gemini 3.5 Flash (Low)", FLASH, Some("low"), None),
        ("Gemini 3.5 Flash (Medium)", FLASH, Some("medium"), None),
        ("Gemini 3.1 Pro (High)", PRO, Some("high"), None),
        // These selector spellings are uncaptured; the effort levels are documented.
        ("Gemini 3.1 Pro (Extra High)", PRO, Some("xhigh"), None),
        ("Gemini 3.1 Pro (XHigh)", PRO, Some("xhigh"), None),
        ("Gemini 3.1 Pro (Max)", PRO, Some("max"), None),
        ("Claude Sonnet 4.6 (Thinking)", SONNET, None, Some(true)),
        (
            "  Gemini 3.5 Flash (mEdIuM) \t",
            FLASH,
            Some("medium"),
            None,
        ),
        (TURBO, TURBO, None, None),
        (MALFORMED_MODEL, MALFORMED_MODEL, None, None),
    ] {
        let context = AntigravityAdapter
            .observe_context(
                "antigravity",
                &json!({"conversation_id": SESSION_ID, "model": {"id": display, "display_name": display}}),
            )
            .unwrap()
            .context;
        assert_eq!(
            (
                context.model_id.as_deref(),
                context.model_display_name.as_deref(),
                context.effort.as_deref(),
                context.thinking_enabled
            ),
            (Some(model), Some(model), effort, thinking)
        );
    }
    // A model variant and an id that is not a label stay as sent.
    for id in ["Orchestrator (Neon)", "  provider/model:id  "] {
        let context = AntigravityAdapter
            .observe_context(
                "antigravity",
                &json!({"conversation_id": SESSION_ID, "model": {"id": id}}),
            )
            .unwrap()
            .context;
        assert_eq!(context.model_id.as_deref(), Some(id));
    }
    // With no display name or native effort, the id's qualifier carries the effort.
    for (id, model, effort, thinking) in [
        ("Gemini 3.1 Pro (High)", PRO, Some("high"), None),
        ("Claude Sonnet 4.6 (Thinking)", SONNET, None, Some(true)),
    ] {
        let context = AntigravityAdapter
            .observe_context(
                "antigravity",
                &json!({"conversation_id": SESSION_ID, "model": {"id": id}}),
            )
            .unwrap()
            .context;
        assert_eq!(
            (
                context.model_id.as_deref(),
                context.effort.as_deref(),
                context.thinking_enabled
            ),
            (Some(model), effort, thinking)
        );
    }
}
#[test]
fn statusline_native_model_effort_wins_over_label_qualifier() {
    for (model, effort) in [
        // Live 1.2.2 shape: the label qualifier and native effort agree.
        (
            json!({"id": "Gemini 3.8 Flash (Low)", "display_name": "Gemini 3.8 Flash (Low)", "effort": "low"}),
            Some("low"),
        ),
        (
            json!({"display_name": "Gemini 3.1 Pro (Low)", "effort": " HIGH "}),
            Some("high"),
        ),
        (
            json!({"display_name": "Gemini 3.1 Pro (Medium)", "effort": ""}),
            Some("medium"),
        ),
        (json!({"display_name": TURBO, "effort": "low"}), Some("low")),
    ] {
        let context = AntigravityAdapter
            .observe_context(
                "antigravity",
                &json!({"conversation_id": SESSION_ID, "model": model}),
            )
            .unwrap()
            .context;
        assert_eq!(context.effort.as_deref(), effort);
    }
}
#[test]
fn statusline_payloads_carry_no_cost_and_keep_the_turn_end_total() {
    let prices = PriceBook::from_litellm_json(
        r#"{"agy-priced":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6},"gemini-3.8-flash":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6}}"#,
    );
    let mut record = crate::agents::context::record::AgentContextRecord::new(
        "antigravity",
        SESSION_ID,
        crate::agents::AgentContext::new("antigravity", Timestamp::now()),
    );
    record.context.cost = Some(crate::agents::AgentCost {
        total_cost_usd: Some(0.42),
        ..crate::agents::AgentCost::default()
    });
    for id in ["agy-priced", "Gemini 3.8 Flash (Max)"] {
        let payload = json!({
            "conversation_id": SESSION_ID,
            "model": {"id": id, "display_name": id},
            "context_window": {"current_usage": {"input_tokens": 10, "output_tokens": 20}},
            "cost": {"total_usd": 0.07, "subagent_usd": 0.02, "estimated": true}
        });
        assert!(
            AntigravityAdapter
                .context_cost(&payload, &|| std::sync::Arc::new(prices.clone()))
                .is_none(),
            "{id}: the status line prices nothing"
        );
        let context = AntigravityAdapter
            .observe_context("antigravity", &payload)
            .unwrap()
            .context;
        assert!(context.cost.is_none());
        record.apply_context_refresh("antigravity", SESSION_ID, context, None);
        assert_eq!(
            record
                .context
                .cost
                .as_ref()
                .and_then(|cost| cost.total_cost_usd),
            Some(0.42),
            "{id}: a later payload keeps the turn-end session total"
        );
    }
}
#[test]
fn statusline_reports_rest_only_when_idle_with_no_background_work() {
    for (label, fields, at_rest) in [
        ("idle", json!({"agent_state": "idle"}), true),
        ("IDLE", json!({"agent_state": "IDLE"}), true),
        (
            "no tasks",
            json!({"agent_state": "idle", "task_count": 0}),
            true,
        ),
        (
            "a background task",
            json!({"agent_state": "idle", "task_count": 1}),
            false,
        ),
        ("working", json!({"agent_state": "working"}), false),
        ("an unknown state", json!({"agent_state": "resting"}), false),
        ("no state", json!({}), false),
        (
            "a pending confirmation",
            json!({"agent_state": "idle", "tool_confirmation_pending": true}),
            false,
        ),
    ] {
        let mut payload = fields;
        payload["conversation_id"] = json!(SESSION_ID);
        let observation = AntigravityAdapter
            .observe_context("antigravity", &payload)
            .unwrap();
        assert_eq!(observation.provider_at_rest, at_rest, "{label}");
        let settle = observation.context.settle.map(|settle| settle.outcome);
        let expected = (label == "a pending confirmation")
            .then_some(crate::agents::TurnSettleOutcome::NativeWait);
        assert_eq!(
            settle, expected,
            "{label}: the adapter builds no rest settle"
        );
    }
}

#[test]
fn a_rest_frame_decodes_to_a_rest_report_routed_to_its_conversation() {
    let payload = json!({"conversation_id": SESSION_ID, "agent_state": "idle", "quota": {}});
    let rest = AntigravityAdapter
        .decode_hook(crate::agents::PROVIDER_REST_EVENT, &payload)
        .unwrap();
    assert!(rest.reports_provider_rest());
    assert!(
        rest.lifecycle().is_none(),
        "the hook transaction decides the signal"
    );
    assert_eq!(
        rest.event_agent_id()
            .map(crate::ids::AgentSessionId::as_str),
        Some(SESSION_ID)
    );
    assert_eq!(rest.reply(), &crate::agents::HookReply::Silent);
    let stop = AntigravityAdapter.decode_hook("Stop", &payload).unwrap();
    assert!(!stop.reports_provider_rest());
}

fn statusline_fixture(source: &str) -> statusline::StatuslinePayload {
    serde_json::from_str(source).unwrap()
}
#[test]
fn statusline_quota_yields_only_the_gemini_windows() {
    let observed_at: Timestamp = "2026-11-19T19:42:27Z".parse().unwrap();
    let context = statusline_fixture(include_str!("tests/fixtures/statusline_full.json"))
        .into_context("antigravity", observed_at);
    let windows: Vec<_> = context
        .rate_limits
        .expect("the gemini buckets are a reading")
        .windows
        .into_iter()
        .map(|window| {
            (
                window.duration_mins,
                window.used_percentage,
                window.resets_at.map(|at| at.to_string()),
                window.observed_at,
                window.source,
                window.scope,
            )
        })
        .collect();
    let best_effort = crate::agents::context::WindowSource::BestEffort;
    assert_eq!(
        windows,
        [
            (
                Some(300),
                Some(2),
                Some("2026-11-19T23:12:50Z".to_owned()),
                Some(observed_at),
                best_effort,
                None
            ),
            (
                Some(10_080),
                Some(2),
                Some("2026-11-26T08:37:04Z".to_owned()),
                Some(observed_at),
                best_effort,
                None
            ),
        ]
    );
    let account = context.account.unwrap();
    assert_eq!(
        (account.plan.as_deref(), account.account_id.as_deref()),
        (Some("Google AI Ultra"), Some("user@example.com"))
    );

    let initializing =
        statusline_fixture(include_str!("tests/fixtures/statusline_initializing.json"))
            .into_context("antigravity", observed_at);
    assert!(initializing.rate_limits.is_none(), "no quota is no reading");
    assert_eq!(initializing.account.unwrap().plan, None);

    let only_third_party = statusline_fixture(
        r#"{"quota":{"3p-5h":{"remaining_fraction":0,"reset_time":"2026-11-20T00:42:16Z"},"3p-weekly":{"remaining_fraction":"bad"}}}"#,
    )
    .into_context("antigravity", observed_at);
    assert!(only_third_party.rate_limits.is_none());
}

#[test]
fn a_statusline_quota_that_is_no_map_keeps_the_rest_of_the_reading() {
    let observed_at: Timestamp = "2026-11-19T19:42:27Z".parse().unwrap();
    let context = statusline_fixture(
        r#"{"quota":"unavailable","plan_tier":"Google AI Ultra","model":{"id":"gemini-4"}}"#,
    )
    .into_context("antigravity", observed_at);
    assert!(context.rate_limits.is_none());
    assert_eq!(context.model_id.as_deref(), Some("gemini-4"));
    assert_eq!(
        context.account.unwrap().plan.as_deref(),
        Some("Google AI Ultra")
    );
}

#[test]
fn an_expired_gemini_bucket_drops_only_its_own_window() {
    let observed_at: Timestamp = "2026-11-19T19:42:27Z".parse().unwrap();
    let context = statusline_fixture(
        r#"{"quota":{
            "gemini-5h":{"remaining_fraction":0,"reset_time":"2026-11-19T19:42:00Z"},
            "gemini-weekly":{"remaining_fraction":0.5,"reset_time":"2026-11-26T08:37:04Z"}
        }}"#,
    )
    .into_context("antigravity", observed_at);
    let windows: Vec<_> = context
        .rate_limits
        .expect("the weekly bucket is a reading")
        .windows
        .into_iter()
        .map(|window| (window.duration_mins, window.used_percentage))
        .collect();
    assert_eq!(windows, [(Some(300), None), (Some(10_080), Some(50))]);
}
mod spend_rows {
    use super::*;
    use crate::agents::spending::SpendParse;

    const NEON: &str = r#"{"gemini-4-neon":{"input_cost_per_token":3e-6,"output_cost_per_token":1.5e-5,"cache_read_input_token_cost":1.5e-7}}"#;
    const OTHER: &str = "22222222-2222-4222-8222-222222222222";

    fn token_row(step: u64, model: &str, input: u64, cache_read: u64, output: u64) -> String {
        json!({"step_index": step, "source": "MODEL", "type": "PLANNER_RESPONSE",
            "status": "DONE", "created_at": AT_10, "content": "answer", "model": model,
            "input_tokens": input, "cache_read_tokens": cache_read, "output_tokens": output})
        .to_string()
    }

    fn parse(path: &Path, book: &str) -> SpendParse {
        AntigravityAdapter.parse_spend(path, None, &PriceBook::from_litellm_json(book))
    }

    fn neon_cost(input: u64, cache_read: u64, output: u64) -> f64 {
        input as f64 * 3e-6 + cache_read as f64 * 1.5e-7 + output as f64 * 1.5e-5
    }

    #[test]
    fn token_rows_become_one_entry_each_keyed_by_conversation_and_step() {
        let dir = tempfile::tempdir().unwrap();
        let parsed = parse(&write_transcript(dir.path(), SESSION_ID), NEON);
        let summary: Vec<_> = parsed
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.message_id.as_deref().map(str::to_owned),
                    entry.thread_id.as_deref().map(str::to_owned),
                    entry.model.as_deref().map(str::to_owned),
                    (
                        entry.input,
                        entry.cache_read,
                        entry.output,
                        entry.cache_write,
                    ),
                )
            })
            .collect();
        assert_eq!(
            summary,
            [(
                Some(format!("{SESSION_ID}:2")),
                Some(SESSION_ID.to_owned()),
                Some("gemini-4-neon".to_owned()),
                (1200, 300, 34, 0),
            )]
        );
        assert!((parsed.entries[0].cost_usd - neon_cost(1200, 300, 34)).abs() < 1e-12);
        assert!(parsed.unknown_models.is_empty());
    }

    #[test]
    fn identical_first_steps_of_two_conversations_stay_two_entries() {
        let dir = tempfile::tempdir().unwrap();
        let row = token_row(1, "gemini-4-neon-high", 100, 0, 10);
        let ids: Vec<_> = [SESSION_ID, OTHER]
            .into_iter()
            .flat_map(|id| {
                let path = write_transcript_named(dir.path(), id, "transcript_full.jsonl", &row);
                parse(&path, NEON).entries
            })
            .map(|entry| entry.message_id.as_deref().map(str::to_owned))
            .collect();
        assert_eq!(
            ids,
            [Some(format!("{SESSION_ID}:1")), Some(format!("{OTHER}:1"))]
        );
    }

    #[test]
    fn a_base_name_priced_only_by_a_related_key_gets_no_dollars() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_transcript(dir.path(), SESSION_ID);
        // The raw slug `gemini-4-neon-high` answers `gemini-4` by fuzzy match only.
        let parsed = parse(
            &path,
            r#"{"gemini-4":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6}}"#,
        );
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].cost_usd, 0.0);
        assert_eq!(parsed.entries[0].input, 1200);
        assert_eq!(
            parsed.unknown_models.keys().collect::<Vec<_>>(),
            ["gemini-4-neon"]
        );
        let raw = parse(
            &path,
            r#"{"gemini-4-neon-high":{"input_cost_per_token":1e-6,"output_cost_per_token":2e-6,"cache_read_input_token_cost":1e-7},"gemini-4-neon":{"input_cost_per_token":3e-6,"output_cost_per_token":1.5e-5}}"#,
        );
        let raw_cost = 1200.0 * 1e-6 + 300.0 * 1e-7 + 34.0 * 2e-6;
        assert!(
            (raw.entries[0].cost_usd - raw_cost).abs() < 1e-12,
            "the raw slug first"
        );
    }

    #[test]
    fn session_cost_sums_the_conversation_rows() {
        let dir = tempfile::tempdir().unwrap();
        let body = [
            token_row(1, "gemini-4-neon-high", 100, 0, 10),
            token_row(3, "gemini-4-neon-high", 50, 100, 5),
            json!({"step_index": 4, "source": "USER_EXPLICIT", "type": "USER_INPUT",
                "status": "DONE", "created_at": AT_11, "content": "next"})
            .to_string(),
        ]
        .join("\n");
        let path = write_transcript_named(dir.path(), SESSION_ID, "transcript_full.jsonl", &body);
        let cost = crate::agents::spending::session_cost_usd(
            crate::agents::definition_by_kind("antigravity").unwrap(),
            SESSION_ID,
            &path,
            &PriceBook::from_litellm_json(NEON),
        )
        .and_then(|cost| cost.total_cost_usd)
        .unwrap();
        assert!((cost - (neon_cost(100, 0, 10) + neon_cost(50, 100, 5))).abs() < 1e-12);
    }

    struct LiveCost {
        dir: tempfile::TempDir,
        env: std::collections::BTreeMap<String, String>,
        pricing: PathBuf,
        prior: Option<crate::agents::LocalContextRefresh>,
        hint: Option<PathBuf>,
    }

    impl LiveCost {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let env = std::collections::BTreeMap::from([(
                "HOME".to_owned(),
                dir.path().display().to_string(),
            )]);
            let pricing = dir.path().join("pricing.sqlite");
            crate::agents::pricing::testkit::seed_cache_at(&pricing, NEON);
            Self {
                dir,
                env,
                pricing,
                prior: None,
                hint: None,
            }
        }

        fn write(&self, rows: &[String]) -> PathBuf {
            self.write_named("transcript_full.jsonl", rows)
        }

        fn write_named(&self, basename: &str, rows: &[String]) -> PathBuf {
            let home = self.dir.path().join(".gemini/antigravity-cli");
            write_transcript_named(&home, SESSION_ID, basename, &rows.join("\n"))
        }

        /// One refresh with the sidecar state the previous one left, as the hook,
        /// tick, and watch callers pass it.
        fn refresh(
            &mut self,
            trigger: crate::agents::RefreshTrigger<'_>,
        ) -> Option<crate::agents::LocalContextRefresh> {
            let fold = self
                .prior
                .as_ref()
                .and_then(|prior| match &prior.spend_fold {
                    crate::agents::FieldPatch::Set(fold) => Some(fold.clone()),
                    _ => None,
                });
            let ctx = crate::agents::LocalContextRefreshCtx {
                open_permission_since: None,
                prior_permission_settle: None,
                login_env: &self.env,
                agent_id: SESSION_ID,
                model_hint: None,
                prior_session_name: None,
                current_transcript_path: self.hint.as_deref().and_then(Path::to_str),
                prior_transcript_path: self
                    .prior
                    .as_ref()
                    .and_then(|prior| prior.transcript_path.as_deref()),
                prior_transcript_stat: self
                    .prior
                    .as_ref()
                    .and_then(|prior| prior.transcript_stat.as_ref()),
                prior_spend_fold: fold.as_ref(),
                prior_model_context_limits_key: None,
                shared_pricing_cache_path: &self.pricing,
            };
            let refresh = AntigravityAdapter.local_context_refresh(trigger, &ctx);
            if let Some(refresh) = &refresh {
                let mut stored = refresh.clone();
                if stored.spend_fold == crate::agents::FieldPatch::Keep {
                    stored.spend_fold = self
                        .prior
                        .as_ref()
                        .map_or(crate::agents::FieldPatch::Keep, |prior| {
                            prior.spend_fold.clone()
                        });
                }
                self.prior = Some(stored);
            }
            refresh
        }
    }

    fn cost_of(refresh: &crate::agents::LocalContextRefresh) -> Option<f64> {
        match &refresh.context.cost {
            crate::agents::FieldPatch::Set(cost) => cost.total_cost_usd,
            _ => None,
        }
    }

    #[test]
    fn live_cost_prices_the_transcript_as_it_grows_on_every_trigger() {
        use crate::agents::RefreshTrigger;
        let mut live = LiveCost::new();
        let first = token_row(1, "gemini-4-neon-high", 100, 0, 10);
        let path = live.write(std::slice::from_ref(&first));
        // No turn has ended: the first priced record already reads in USD.
        let refresh = live.refresh(RefreshTrigger::Tick).expect("priced");
        assert_eq!(
            refresh.context,
            crate::agents::LocalContextPatch {
                cost: crate::agents::FieldPatch::Set(crate::agents::AgentCost {
                    total_cost_usd: Some(neon_cost(100, 0, 10)),
                    ..crate::agents::AgentCost::default()
                }),
                ..crate::agents::LocalContextPatch::default()
            },
            "cost only, as a session total; the statusline keeps model, tokens, and settle"
        );
        assert_eq!(
            refresh.transcript_path.as_deref(),
            Some(path.to_str().unwrap())
        );
        assert!(refresh.transcript_stat.is_some());

        let second = token_row(3, "gemini-4-neon-high", 50, 100, 5);
        live.write(&[first.clone(), second.clone()]);
        let refresh = live.refresh(RefreshTrigger::Watch).expect("grown");
        let two = neon_cost(100, 0, 10) + neon_cost(50, 100, 5);
        assert!((cost_of(&refresh).unwrap() - two).abs() < 1e-12);

        let third = token_row(5, "gemini-4-neon-high", 10, 0, 1);
        live.write(&[first, second, third]);
        let refresh = live
            .refresh(RefreshTrigger::Hook("PostToolUse"))
            .expect("grown");
        let three = two + neon_cost(10, 0, 1);
        assert!((cost_of(&refresh).unwrap() - three).abs() < 1e-12);
        let turn_end = crate::agents::spending::session_cost_usd(
            crate::agents::definition_by_kind("antigravity").unwrap(),
            SESSION_ID,
            &path,
            &PriceBook::from_litellm_json(NEON),
        )
        .and_then(|cost| cost.total_cost_usd)
        .unwrap();
        assert!(
            (turn_end - three).abs() < 1e-12,
            "the turn-end total agrees with the live fold"
        );
    }

    #[test]
    fn an_unchanged_transcript_costs_a_stat_and_no_price_book() {
        let mut live = LiveCost::new();
        live.write(&[token_row(1, "gemini-4-neon-high", 100, 0, 10)]);
        live.refresh(crate::agents::RefreshTrigger::Tick)
            .expect("priced");
        let loads = crate::agents::pricing::testkit::book_loads();
        crate::agents::pricing::testkit::reset_cached_book();
        assert_eq!(
            live.refresh(crate::agents::RefreshTrigger::Watch),
            None,
            "an unchanged stat writes no sidecar"
        );
        assert_eq!(crate::agents::pricing::testkit::book_loads(), loads);
    }

    #[test]
    fn a_refresh_that_reads_no_new_bytes_writes_nothing_and_loads_no_price_book() {
        let mut live = LiveCost::new();
        live.write(&[token_row(1, "gemini-4-neon-high", 100, 0, 10)]);
        live.refresh(crate::agents::RefreshTrigger::Tick)
            .expect("priced");
        let stored = live.prior.clone().unwrap();
        let changed = crate::agents::TranscriptStat {
            len: stored.transcript_stat.unwrap().len + 1,
            ..stored.transcript_stat.unwrap()
        };
        for prior_stat in [None, Some(changed)] {
            live.prior = Some(crate::agents::LocalContextRefresh {
                transcript_stat: prior_stat,
                ..stored.clone()
            });
            crate::agents::pricing::testkit::reset_cached_book();
            let loads = crate::agents::pricing::testkit::book_loads();
            assert_eq!(
                live.refresh(crate::agents::RefreshTrigger::Hook("PostToolUse")),
                None,
                "past the stat gate with nothing new: {prior_stat:?}"
            );
            assert_eq!(crate::agents::pricing::testkit::book_loads(), loads);
        }
    }

    #[test]
    fn a_hint_naming_the_compact_transcript_still_prices_the_full_one() {
        let mut live = LiveCost::new();
        let full = live.write(&[token_row(1, "gemini-4-neon-high", 100, 0, 10)]);
        live.hint = Some(live.write_named(
            "transcript.jsonl",
            &[token_row(1, "gemini-4-neon-high", 900, 0, 90)],
        ));
        let refresh = live
            .refresh(crate::agents::RefreshTrigger::Hook("PostToolUse"))
            .expect("priced");
        assert_eq!(
            refresh.transcript_path.as_deref(),
            Some(full.to_str().unwrap())
        );
        assert!((cost_of(&refresh).unwrap() - neon_cost(100, 0, 10)).abs() < 1e-12);
    }

    #[test]
    fn a_shrunk_transcript_never_lowers_the_live_cost() {
        use crate::agents::RefreshTrigger;
        let mut live = LiveCost::new();
        let rows = [
            token_row(1, "gemini-4-neon-high", 100, 0, 10),
            token_row(3, "gemini-4-neon-high", 50, 100, 5),
        ];
        let path = live.write(&rows);
        let old_len = std::fs::metadata(&path).unwrap().len();
        let shown = cost_of(&live.refresh(RefreshTrigger::Tick).unwrap()).unwrap();

        live.write(&rows[..1]);
        let refresh = live
            .refresh(RefreshTrigger::Watch)
            .expect("a new stat lands");
        assert!(
            (cost_of(&refresh).unwrap() - shown).abs() < 1e-12,
            "the spent figure stands"
        );
        assert_eq!(
            refresh.transcript_path.as_deref(),
            Some(path.to_str().unwrap())
        );
        assert_eq!(
            refresh.transcript_stat,
            crate::agents::TranscriptStat::from_path(&path)
        );
        let crate::agents::FieldPatch::Set(fold) = &refresh.spend_fold else {
            panic!("the fresh fold is stored: {:?}", refresh.spend_fold);
        };
        assert_eq!(fold.cursor.offset, std::fs::metadata(&path).unwrap().len());

        // Appended below the old end of the file, then past it: each row is priced once.
        let below = token_row(5, "gemini-4-neon-high", 10, 0, 1);
        live.write(&[rows[0].clone(), below.clone()]);
        assert!(std::fs::metadata(&path).unwrap().len() < old_len);
        let refresh = live.refresh(RefreshTrigger::Watch).expect("grown");
        let with_below = shown + neon_cost(10, 0, 1);
        assert!((cost_of(&refresh).unwrap() - with_below).abs() < 1e-12);

        let past = [
            token_row(7, "gemini-4-neon-high", 70, 0, 7),
            token_row(9, "gemini-4-neon-high", 70, 0, 7),
        ];
        live.write(&[rows[0].clone(), below, past[0].clone(), past[1].clone()]);
        assert!(std::fs::metadata(&path).unwrap().len() > old_len);
        let refresh = live.refresh(RefreshTrigger::Watch).expect("grown");
        let with_past = with_below + 2.0 * neon_cost(70, 0, 7);
        assert!((cost_of(&refresh).unwrap() - with_past).abs() < 1e-12);
    }

    #[test]
    fn spend_sources_are_the_full_transcripts_under_the_login_home() {
        let dir = tempfile::tempdir().unwrap();
        let provider_home = dir.path().join(".gemini/antigravity-cli");
        let full = write_transcript(&provider_home, SESSION_ID);
        write_transcript_named(&provider_home, OTHER, "transcript.jsonl", "");
        let env = std::collections::BTreeMap::from([(
            "HOME".to_owned(),
            dir.path().display().to_string(),
        )]);
        let files: Vec<_> = AntigravityAdapter
            .spending_sources(&env)
            .iter()
            .flat_map(crate::agents::spending::SpendingSource::complete_files)
            .collect();
        assert_eq!(files, [full]);
    }

    fn timed_model_row(step: u64, at: &str, output: u64) -> String {
        json!({
            "step_index": step,
            "source": "MODEL",
            "type": "PLANNER_RESPONSE",
            "status": "DONE",
            "created_at": at,
            "content": "answer",
            "model": "gemini-4-neon-high",
            "input_tokens": 100,
            "cache_read_tokens": 0,
            "output_tokens": output,
            "tool_calls": [{"name": "run_command", "args": {}}]
        })
        .to_string()
    }

    fn generic_row(step: u64, status: &str, at: &str) -> String {
        json!({
            "step_index": step,
            "source": "MODEL",
            "type": "GENERIC",
            "status": status,
            "created_at": at,
            "content": "tool output"
        })
        .to_string()
    }

    #[test]
    fn response_timing_times_only_model_rows_followed_by_a_generic_row() {
        use crate::agents::spending::response_timing;

        const AT_15: &str = "2026-07-13T23:23:15Z";
        const AT_19: &str = "2026-07-13T23:23:19Z";
        const AT_OVER_MAX: &str = "2026-07-13T23:28:11Z";

        let dir = tempfile::tempdir().unwrap();
        let book = PriceBook::from_litellm_json(NEON);
        let checkpoint = |step, at| {
            json!({
                "step_index": step,
                "source": "SYSTEM",
                "type": "CHECKPOINT",
                "status": "DONE",
                "created_at": at,
                "input_tokens": 100,
                "output_tokens": 250
            })
            .to_string()
        };
        let cases = [
            (
                "done generic follower",
                vec![
                    timed_model_row(1, AT_10, 250),
                    generic_row(2, "DONE", AT_15),
                ],
                vec![response_timing(250, 5_000)],
            ),
            (
                "running generic follower",
                vec![
                    timed_model_row(1, AT_10, 300),
                    generic_row(2, "RUNNING", AT_12),
                ],
                vec![response_timing(300, 2_000)],
            ),
            (
                "no follower",
                vec![timed_model_row(1, AT_10, 250)],
                vec![None],
            ),
            (
                "user input follower",
                vec![
                    timed_model_row(1, AT_10, 250),
                    user_record(2, AT_15, "next"),
                ],
                vec![None],
            ),
            (
                "system message follower",
                vec![
                    timed_model_row(1, AT_10, 250),
                    record(2, "SYSTEM", "SYSTEM_MESSAGE", "DONE", AT_15, "notice").to_string(),
                ],
                vec![None],
            ),
            (
                "checkpoint follower stays priced and untimed",
                vec![timed_model_row(1, AT_10, 250), checkpoint(2, AT_15)],
                vec![None, None],
            ),
            (
                "model row follower leaves first untimed and times second",
                vec![
                    timed_model_row(1, AT_10, 250),
                    timed_model_row(2, AT_15, 200),
                    generic_row(3, "DONE", AT_19),
                ],
                vec![None, response_timing(200, 4_000)],
            ),
            (
                "invoke_subagent follower stays untimed",
                vec![
                    timed_model_row(1, AT_10, 250),
                    record(2, "MODEL", "INVOKE_SUBAGENT", "DONE", AT_15, "{}").to_string(),
                ],
                vec![None],
            ),
            (
                "non-model generic follower stays untimed",
                vec![
                    timed_model_row(1, AT_10, 250),
                    record(2, "SYSTEM", "GENERIC", "DONE", AT_15, "other").to_string(),
                ],
                vec![None],
            ),
            (
                "later generic past another row stays untimed",
                vec![
                    timed_model_row(1, AT_10, 250),
                    user_record(2, AT_12, "interleaved"),
                    generic_row(3, "DONE", AT_15),
                ],
                vec![None],
            ),
            (
                "checkpoint start row stays untimed",
                vec![checkpoint(1, AT_10), generic_row(2, "DONE", AT_15)],
                vec![None],
            ),
            (
                "zero second gap fails shared floor",
                vec![
                    timed_model_row(1, AT_10, 250),
                    generic_row(2, "DONE", AT_10),
                ],
                vec![None],
            ),
            (
                "under 100 output tokens stays untimed",
                vec![timed_model_row(1, AT_10, 99), generic_row(2, "DONE", AT_15)],
                vec![None],
            ),
            (
                "exact 100 output tokens and 1s gap is timed",
                vec![
                    timed_model_row(1, AT_10, 100),
                    generic_row(2, "DONE", AT_11),
                ],
                vec![response_timing(100, 1_000)],
            ),
            (
                "gap over 300s ceiling stays untimed",
                vec![
                    timed_model_row(1, AT_10, 250),
                    generic_row(2, "DONE", AT_OVER_MAX),
                ],
                vec![None],
            ),
            (
                "negative clock skew gap stays untimed",
                vec![
                    timed_model_row(1, AT_10, 250),
                    generic_row(2, "DONE", AT_09),
                ],
                vec![None],
            ),
            (
                "quoted generic text in user content does not time",
                vec![
                    timed_model_row(1, AT_10, 250),
                    user_record(2, AT_15, r#"source "MODEL" type "GENERIC""#),
                ],
                vec![None],
            ),
        ];

        let mut actual = Vec::with_capacity(cases.len());
        let mut expected = Vec::with_capacity(cases.len());
        for (label, rows, want) in cases {
            let body = format!("{}\n", rows.join("\n"));
            let path =
                write_transcript_named(dir.path(), SESSION_ID, "transcript_full.jsonl", &body);
            let got: Vec<_> = AntigravityAdapter
                .parse_spend(&path, None, &book)
                .entries
                .into_iter()
                .map(|entry| entry.timing)
                .collect();
            actual.push((label, got));
            expected.push((label, want));
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn resumed_parse_matches_cold_parse_at_every_line_boundary() {
        use crate::agents::spending::{response_timing, session_entries};

        const AT_15: &str = "2026-07-13T23:23:15Z";
        const AT_16: &str = "2026-07-13T23:23:16Z";
        const AT_19: &str = "2026-07-13T23:23:19Z";
        const AT_20: &str = "2026-07-13T23:23:20Z";
        const AT_21: &str = "2026-07-13T23:23:21Z";
        const AT_25: &str = "2026-07-13T23:23:25Z";
        const AT_26: &str = "2026-07-13T23:23:26Z";
        const AT_27: &str = "2026-07-13T23:23:27Z";

        let dir = tempfile::tempdir().unwrap();
        let book = PriceBook::from_litellm_json(NEON);
        let lines = [
            user_record(0, AT_09, "start"),
            timed_model_row(1, AT_10, 400),
            generic_row(2, "DONE", AT_15),
            timed_model_row(3, AT_16, 250),
            generic_row(4, "RUNNING", AT_19),
            record(5, "SYSTEM", "SYSTEM_MESSAGE", "DONE", AT_20, "bg done").to_string(),
            timed_model_row(6, AT_21, 80),
            generic_row(7, "DONE", AT_25),
            json!({
                "step_index": 8,
                "source": "SYSTEM",
                "type": "CHECKPOINT",
                "status": "DONE",
                "created_at": AT_26,
                "input_tokens": 50,
                "output_tokens": 200
            })
            .to_string(),
            timed_model_row(9, AT_27, 300),
        ];
        let full_body = format!("{}\n", lines.join("\n"));
        let full_path =
            write_transcript_named(dir.path(), SESSION_ID, "transcript_full.jsonl", &full_body);
        let cold = AntigravityAdapter.parse_spend(&full_path, None, &book);
        let cold_timings: Vec<_> = cold.entries.iter().map(|entry| entry.timing).collect();
        assert_eq!(
            cold_timings,
            [
                response_timing(400, 5_000),
                response_timing(250, 3_000),
                None,
                None,
                None,
            ]
        );

        for split in 1..lines.len() {
            for trailing_newline in [true, false] {
                let prefix = if trailing_newline {
                    format!("{}\n", lines[..split].join("\n"))
                } else {
                    lines[..split].join("\n")
                };
                let path = write_transcript_named(
                    dir.path(),
                    SESSION_ID,
                    "transcript_full.jsonl",
                    &prefix,
                );
                let first = AntigravityAdapter.parse_spend(&path, None, &book);
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap();
                if !trailing_newline {
                    file.write_all(b"\n").unwrap();
                }
                for line in &lines[split..] {
                    writeln!(file, "{line}").unwrap();
                }
                let resumed = AntigravityAdapter.parse_spend(&path, Some(&first.cursor), &book);
                assert_eq!(
                    resumed.cursor, cold.cursor,
                    "cursor mismatch at split {split} (newline={trailing_newline})"
                );
                // Cut between the <100-token model row (index 6) and its follower (index 7)
                // must not re-emit step 6 because it still has no timing.
                if split == 7 {
                    assert!(
                        resumed
                            .entries
                            .iter()
                            .all(|entry| entry.message_id.as_deref()
                                != Some(&format!("{SESSION_ID}:6"))),
                        "untimed row must not re-emit at split {split} (newline={trailing_newline})"
                    );
                }
                // Cut between step 1 (index 1) and its DONE follower (index 2) re-emits step 1 timed.
                if split == 2 {
                    assert_eq!(
                        resumed.entries.first().and_then(|entry| entry.timing),
                        response_timing(400, 5_000),
                        "timed re-emit expected at split {split} (newline={trailing_newline})"
                    );
                }
                let mut combined = first.entries;
                combined.extend(resumed.entries);
                let mut deduped: Vec<_> = session_entries(&combined, SESSION_ID)
                    .into_iter()
                    .cloned()
                    .collect();
                deduped.sort_by_key(|entry| entry.ts_secs);
                assert_eq!(
                    deduped, cold.entries,
                    "deduped entries mismatch at split {split} (newline={trailing_newline})"
                );
                let combined_usd: f64 = deduped.iter().map(|entry| entry.cost_usd).sum();
                let cold_usd: f64 = cold.entries.iter().map(|entry| entry.cost_usd).sum();
                assert!(
                    (combined_usd - cold_usd).abs() < 1e-12,
                    "cost mismatch at split {split} (newline={trailing_newline})"
                );
            }
        }
    }

    #[test]
    fn live_cost_shows_trailing_model_row_at_once_and_counts_it_once_after_follower() {
        use crate::agents::RefreshTrigger;
        use crate::agents::spending::response_timing;

        const AT_15: &str = "2026-07-13T23:23:15Z";
        const AT_20: &str = "2026-07-13T23:23:20Z";
        const AT_24: &str = "2026-07-13T23:23:24Z";

        let mut live = LiveCost::new();
        let row1 = timed_model_row(1, AT_10, 200);
        let path = live.write(std::slice::from_ref(&row1));
        let first = live.refresh(RefreshTrigger::Tick).expect("priced at once");
        let first_usd = neon_cost(100, 0, 200);
        assert!((cost_of(&first).unwrap() - first_usd).abs() < 1e-12);
        let crate::agents::FieldPatch::Set(fold1) = &first.spend_fold else {
            panic!("first fold stored: {:?}", first.spend_fold);
        };

        // Follower lands: a resumed parse emits the timed copy, and the live fold keeps the same total.
        let follower1 = generic_row(2, "DONE", AT_15);
        live.write(&[row1.clone(), follower1.clone()]);
        let resumed = AntigravityAdapter.parse_spend(
            &path,
            Some(&fold1.cursor),
            &PriceBook::from_litellm_json(NEON),
        );
        assert_eq!(resumed.entries.len(), 1);
        assert_eq!(resumed.entries[0].timing, response_timing(200, 5_000));

        let after_follower = live.refresh(RefreshTrigger::Watch).expect("grown");
        assert!(
            (cost_of(&after_follower).unwrap() - first_usd).abs() < 1e-12,
            "re-emitted timed row must not double-count live cost"
        );
        let crate::agents::FieldPatch::Set(fold2) = &after_follower.spend_fold else {
            panic!("second fold stored: {:?}", after_follower.spend_fold);
        };
        assert_eq!((fold2.input, fold2.output), (100, 200));

        // Second trailing model row lands, then its follower and a third model row arrive together.
        let row3 = timed_model_row(3, AT_20, 150);
        live.write(&[row1.clone(), follower1.clone(), row3.clone()]);
        let with_row3 = live.refresh(RefreshTrigger::Watch).expect("row3 priced");
        let two_usd = first_usd + neon_cost(100, 0, 150);
        assert!((cost_of(&with_row3).unwrap() - two_usd).abs() < 1e-12);

        let follower3 = generic_row(4, "RUNNING", AT_24);
        let row5 = timed_model_row(5, AT_24, 120);
        live.write(&[row1, follower1, row3, follower3, row5]);
        let with_row5 = live.refresh(RefreshTrigger::Watch).expect("row5 priced");
        let three_usd = two_usd + neon_cost(100, 0, 120);
        assert!(
            (cost_of(&with_row5).unwrap() - three_usd).abs() < 1e-12,
            "batched follower + next model row counts each once"
        );
    }

    #[test]
    fn truncated_or_rewritten_file_with_pending_cursor_parses_without_panic_or_wrong_timing() {
        const AT_15: &str = "2026-07-13T23:23:15Z";

        let dir = tempfile::tempdir().unwrap();
        let book = PriceBook::from_litellm_json(NEON);
        let prefix = format!(
            "{}\n{}\n",
            user_record(0, AT_09, "prompt"),
            timed_model_row(1, AT_10, 250)
        );
        let path = write_transcript_named(dir.path(), SESSION_ID, "transcript_full.jsonl", &prefix);
        let first = AntigravityAdapter.parse_spend(&path, None, &book);
        assert!(
            first.cursor.state().is_some(),
            "trailing model row must record pending start in cursor state"
        );
        let cursor = first.cursor;

        // Truncated shorter than pending_start (empty and short).
        for truncated in ["", "{\"short\":1}\n"] {
            std::fs::write(&path, truncated).unwrap();
            let parsed = AntigravityAdapter.parse_spend(&path, Some(&cursor), &book);
            assert!(parsed.entries.is_empty(), "truncated {truncated:?}");
        }

        // Truncated between pending_start and cursor.offset.
        std::fs::write(&path, &prefix[..prefix.len() - 20]).unwrap();
        let mid_truncated = AntigravityAdapter.parse_spend(&path, Some(&cursor), &book);
        assert!(mid_truncated.entries.is_empty());

        // Rewritten longer than cursor.offset where pending_start is now a non-model row.
        let mut rewritten = format!("{}\n", user_record(0, AT_09, "prompt"));
        let pad_len = (cursor.offset as usize).saturating_sub(rewritten.len());
        let non_model = user_record(1, AT_10, &"x".repeat(pad_len));
        rewritten.push_str(&non_model);
        rewritten.push('\n');
        rewritten.push_str(&generic_row(2, "DONE", AT_15));
        rewritten.push('\n');
        std::fs::write(&path, &rewritten).unwrap();
        let after_non_model = AntigravityAdapter.parse_spend(&path, Some(&cursor), &book);
        assert!(
            after_non_model
                .entries
                .iter()
                .all(|entry| entry.timing.is_none()),
            "rewritten non-model prefix must not produce timing"
        );

        // Corrupted cursor whose pending_start is past the file end and past offset.
        let valid_body = format!(
            "{}\n{}\n",
            timed_model_row(1, AT_10, 250),
            generic_row(2, "DONE", AT_15)
        );
        std::fs::write(&path, &valid_body).unwrap();
        let bad_cursor =
            crate::agents::spending::SpendCursor::with_state(0, &json!({"pending_start": 999_999}));
        let from_bad = AntigravityAdapter.parse_spend(&path, Some(&bad_cursor), &book);
        assert_eq!(from_bad.entries.len(), 1);
    }
}
#[test]
fn visible_transcript_contract_exposes_only_normalized_messages() {
    let messages = AntigravityAdapter
        .parse_transcript_messages(include_str!("tests/fixtures/transcript_full.jsonl"));
    assert_eq!(
        messages
            .iter()
            .map(|message| (message.role, message.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (TranscriptRole::User, "ping"),
            (TranscriptRole::Assistant, "pong")
        ]
    );
    assert!(
        messages
            .iter()
            .all(|message| !message.text.contains("checkpoint"))
    );
    let wrapped = "  <USER_REQUEST>\nrefine the card\nwithout metadata\n</USER_REQUEST>\n<ADDITIONAL_METADATA>{\"secret\":true}</ADDITIONAL_METADATA>  ";
    let lines = [
        user_record(0, AT_09, wrapped),
        user_record(1, AT_10, "<USER_REQUEST>missing close"),
        user_record(2, AT_11, "<USER_REQUEST> </USER_REQUEST>"),
        user_record(3, AT_12, "  legacy prompt  "),
    ]
    .join("\n");
    assert_eq!(
        session::messages(&lines)
            .into_iter()
            .map(|message| message.text)
            .collect::<Vec<_>>(),
        ["refine the card\nwithout metadata", "legacy prompt"]
    );
    let compacted = [
        user_record(0, AT_09, "<USER_REQUEST>\n/compact\n</USER_REQUEST>"),
        record(
            1,
            "SYSTEM",
            "CHECKPOINT",
            "DONE",
            AT_10,
            "# Resuming from a compaction",
        )
        .to_string(),
        user_record(
            2,
            AT_11,
            "<USER_REQUEST>\n/compact keep the notes\n</USER_REQUEST>",
        ),
    ]
    .join("\n");
    assert_eq!(
        session::messages(&compacted)
            .into_iter()
            .map(|message| message.text)
            .collect::<Vec<_>>(),
        ["/compact keep the notes"]
    );
    assert!(
        AntigravityAdapter
            .parse_transcript_messages(concat!(
                "not-json\n{}\n",
                r#"{"step_index":4,"source":"MODEL","type":"PLANNER_THOUGHT","status":"DONE","created_at":"2026-07-13T23:23:13Z","content":"hidden"}"#,
                "\n",
                r#"{"step_index":5,"source":"MODEL","type":"PLANNER_RESPONSE","status":"IN_PROGRESS","created_at":"2026-07-13T23:23:14Z","content":"partial"}"#,
            ))
            .is_empty()
    );
    let dir = tempfile::tempdir().unwrap();
    let path = write_transcript_named(dir.path(), SESSION_ID, "transcript_full.jsonl", "");
    for (content, expected) in [
        (wrapped, Some("refine the card\nwithout metadata")),
        ("<USER_REQUEST>missing close", None),
        ("<USER_REQUEST> </USER_REQUEST><SETTINGS />", None),
        ("  legacy prompt  ", Some("legacy prompt")),
    ] {
        std::fs::write(&path, user_record(0, AT_09, content)).unwrap();
        assert_eq!(
            session::latest_prompt_under(dir.path(), &path, SESSION_ID).as_deref(),
            expected
        );
    }
}

#[test]
fn final_assistant_uses_tail_then_full_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    let assistant = r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-07-13T23:23:09Z","content":"tail answer"}"#;
    let padding = format!(
        "{{\"step_index\":2,\"source\":\"SYSTEM\",\"type\":\"CHECKPOINT\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:10Z\",\"content\":{}}}",
        serde_json::to_string(&"x".repeat(70_000)).unwrap()
    );
    std::fs::write(&path, format!("{padding}\n{assistant}\n")).unwrap();
    assert_eq!(
        session::last_assistant_message(&path).as_deref(),
        Some("tail answer")
    );
    std::fs::write(&path, format!("{assistant}\n{padding}\n")).unwrap();
    assert_eq!(
        session::last_assistant_message(&path).as_deref(),
        Some("tail answer")
    );
}

#[test]
fn final_assistant_is_bounded_to_the_latest_turn() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    let old = record(1, "MODEL", "PLANNER_RESPONSE", "DONE", AT_10, "old answer");
    let padding = record(
        4,
        "SYSTEM",
        "CHECKPOINT",
        "DONE",
        AT_12,
        &"x".repeat(70_000),
    );
    for prompt in ["second", "<USER_REQUEST>missing close"] {
        for suffix in [String::new(), format!("{padding}\n")] {
            let lines = format!(
                "{}\n{old}\n{}\n{suffix}",
                user_record(0, AT_09, "first"),
                user_record(2, AT_11, prompt)
            );
            std::fs::write(&path, lines).unwrap();
            assert_eq!(session::last_assistant_message(&path), None);
        }
    }
}

#[test]
fn final_assistant_does_not_reuse_an_answer_after_error() {
    let dir = tempfile::tempdir().unwrap();
    // Terminal error source/type are uncaptured; assume MODEL/PLANNER_RESPONSE.
    let error = record(
        3,
        "MODEL",
        "PLANNER_RESPONSE",
        "ERROR",
        AT_12,
        "error details",
    );
    let lines = format!(
        "{}\n{}\n{}\n{error}\n",
        user_record(0, AT_09, "first"),
        planner_record(1, AT_10, None),
        user_record(2, AT_11, "second")
    );
    let path = write_transcript_named(dir.path(), SESSION_ID, "transcript_full.jsonl", &lines);
    assert_eq!(session::last_assistant_message(&path), None);
    assert_eq!(session::messages(&lines).len(), 3);
}

#[test]
fn planner_error_projects_failed_and_idle() {
    let dir = tempfile::tempdir().unwrap();
    // Terminal error source/type are uncaptured; assume MODEL/PLANNER_RESPONSE.
    let error = record(
        3,
        "MODEL",
        "PLANNER_RESPONSE",
        "ERROR",
        AT_12,
        "error details",
    );
    let lines = format!(
        "{}\n{}\n{}\n{error}\n",
        user_record(0, AT_09, "first"),
        planner_record(1, AT_10, None),
        user_record(2, AT_11, "second")
    );
    write_transcript_named(dir.path(), SESSION_ID, "transcript_full.jsonl", &lines);
    let observation =
        session::discover_under(dir.path(), Path::new("/workspace/project")).remove(0);
    let state = local_state(&observation);
    assert_eq!(
        (state.status, state.phase),
        (AgentStatus::Failed, TurnPhase::Idle)
    );
    assert_eq!(observation.last_activity, AT_12.parse().unwrap());
    assert_eq!(observation.first_event_at, Some(AT_09.parse().unwrap()));
    assert!(state.native_prompt_detail.is_none() && state.waiting_since.is_none());
}

#[test]
fn planner_error_recovery_and_tool_error_keep_existing_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let error = record(
        1,
        "MODEL",
        "PLANNER_RESPONSE",
        "ERROR",
        AT_10,
        "error details",
    );
    let answer = record(
        2,
        "MODEL",
        "PLANNER_RESPONSE",
        "DONE",
        AT_11,
        "retry answer",
    );
    let state = state_after(
        dir.path(),
        workspace,
        vec![error.to_string(), answer.to_string()],
    );
    assert_eq!(
        (state.status, state.phase),
        (AgentStatus::Success, TurnPhase::Idle)
    );
    let path = dir
        .path()
        .join("brain")
        .join(SESSION_ID)
        .join(".system_generated/logs/transcript_full.jsonl");
    assert_eq!(
        session::last_assistant_message(&path).as_deref(),
        Some("retry answer")
    );
    let tool_error = record(1, "MODEL", "RUN_COMMAND", "ERROR", AT_10, "tool failed");
    let state = state_after(dir.path(), workspace, vec![tool_error.to_string()]);
    assert_eq!(
        (state.status, state.phase),
        (AgentStatus::Running, TurnPhase::Reasoning)
    );
}

#[test]
fn a_pending_tool_call_reads_running_and_its_native_prompt_reads_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let command = json_value(r#"[{"name":"run_command","args":{"CommandLine":"cargo test"}}]"#);
    let local = state_after(
        dir.path(),
        workspace,
        vec![planner_record(1, AT_10, Some(command))],
    );
    assert_eq!(
        (local.status, local.phase),
        (AgentStatus::Running, TurnPhase::Reasoning),
        "a requested tool with no later row keeps the turn open"
    );
    let mut agent = crate::testkit::agent_state("antigravity", SESSION_ID, AT_10.parse().unwrap());
    (agent.status, agent.phase) = (local.status, local.phase);
    let now = AT_12.parse().unwrap();
    assert_eq!(agent.effective_status(now), AgentStatus::Running);
    agent.context = Some(
        statusline_fixture(
            &json!({"conversation_id": SESSION_ID, "tool_confirmation_pending": true}).to_string(),
        )
        .into_context("antigravity", AT_11.parse().unwrap()),
    );
    assert_eq!(
        agent.effective_status(now),
        AgentStatus::Waiting,
        "a later native permission prompt raises the card"
    );
}

/// agy 1.5.14 (c7 capture): a `run_command` that becomes a background task leaves a
/// `MODEL` / `GENERIC` row `RUNNING` and a no-tool planner `DONE`; the task's end appends
/// `SYSTEM` / `SYSTEM_MESSAGE` rows and a woken planner row, with no `USER_INPUT`.
#[test]
fn a_running_background_task_parks_the_turn_until_its_completion_wakes_the_planner() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let task = format!("{SESSION_ID}/task-10");
    let started = record(
        2,
        "MODEL",
        "GENERIC",
        "RUNNING",
        AT_10,
        &format!(
            "Created At: {AT_10}\nTool is running as a background task with task id: {task}\n\
             Task Description: sleep 30 && echo SLEPT\n"
        ),
    )
    .to_string();
    let ended = |id: &str, outcome: &str| {
        record(
            4,
            "SYSTEM",
            "SYSTEM_MESSAGE",
            "DONE",
            AT_12,
            &format!(
                "<SYSTEM_MESSAGE>\n[Message] timestamp={AT_12} sender={id} \
                 priority=MESSAGE_PRIORITY_HIGH content=Task id \"{id}\" {outcome}:\n\
                 Command exited with code 0.\n</SYSTEM_MESSAGE>"
            ),
        )
        .to_string()
    };
    let reminder = record(
        5,
        "SYSTEM",
        "SYSTEM_MESSAGE",
        "DONE",
        AT_12,
        "<SYSTEM_MESSAGE>\nReminder: work efficiently.\n</SYSTEM_MESSAGE>",
    )
    .to_string();
    let command = json_value(r#"[{"name":"run_command","args":{"CommandLine":"sleep 30"}}]"#);
    let parked = vec![
        planner_record(1, AT_10, Some(command)),
        started,
        planner_record(3, AT_11, None),
    ];
    let phase = |ending: Vec<String>| {
        let local = state_after(dir.path(), workspace, ending);
        (local.status, local.phase)
    };
    assert_eq!(
        phase(parked.clone()),
        (AgentStatus::Running, TurnPhase::Parked),
        "the foreground reply does not end a turn whose task still runs"
    );
    let other = format!("{SESSION_ID}/task-99");
    let mut ending = parked.clone();
    ending.extend([
        ended(&other, "finished with result"),
        planner_record(6, AT_12, None),
    ]);
    assert_eq!(
        phase(ending),
        (AgentStatus::Running, TurnPhase::Parked),
        "another task's completion leaves this one running"
    );
    let mut ending = parked.clone();
    ending.extend([
        ended(&task, "finished with result"),
        reminder,
        planner_record(6, AT_12, None),
    ]);
    assert_eq!(
        phase(ending),
        (AgentStatus::Success, TurnPhase::Idle),
        "the planner row the completion wakes is the turn's result"
    );
    let mut ending = parked.clone();
    ending.extend([
        ended(&task, "failed with error"),
        planner_record(6, AT_12, None),
    ]);
    assert_eq!(
        phase(ending),
        (AgentStatus::Success, TurnPhase::Idle),
        "a task that ends any other way ends too"
    );
    let mut ending = parked.clone();
    ending.extend([
        user_record(6, AT_12, "next"),
        planner_record(7, AT_12, None),
    ]);
    assert_eq!(
        phase(ending),
        (AgentStatus::Running, TurnPhase::Parked),
        "a background task outlives a later user prompt and keeps its no-tool end parked"
    );
    let restart_notice = "<SYSTEM_MESSAGE>\n[Message] timestamp=2026-07-13T23:23:12Z \
        sender=system priority=MESSAGE_PRIORITY_LOW content=[Notice] All your subagents and \
        background tasks have been stopped due to server restart. If you want a subagent to \
        continue working, it needs to be revived by sending a message.\n</SYSTEM_MESSAGE>";
    let mut quoted_restart = parked.clone();
    quoted_restart.extend([
        record(6, "MODEL", "GENERIC", "DONE", AT_12, restart_notice).to_string(),
        planner_record(7, AT_12, None),
    ]);
    assert_eq!(
        phase(quoted_restart),
        (AgentStatus::Running, TurnPhase::Parked),
        "quoted restart text in tool output does not clear running tasks"
    );
    let still_running_quoting_finish = format!(
        "<SYSTEM_MESSAGE>\n[Message] timestamp={AT_12} sender={task} \
         priority=MESSAGE_PRIORITY_HIGH content=Background command is still running after 300s: \
         echo 'content=Task id \"{task}\" finished with result:'\n</SYSTEM_MESSAGE>"
    );
    let still_running_quoting_restart = format!(
        "<SYSTEM_MESSAGE>\n[Message] timestamp={AT_12} sender={task} \
         priority=MESSAGE_PRIORITY_HIGH content=Background command is still running after 300s: \
         echo 'content=[Notice] All your subagents and background tasks have been stopped due to \
         server restart'\n</SYSTEM_MESSAGE>"
    );
    let mut still_running_quoted = parked.clone();
    still_running_quoted.extend([
        record(
            6,
            "SYSTEM",
            "SYSTEM_MESSAGE",
            "DONE",
            AT_12,
            &still_running_quoting_finish,
        )
        .to_string(),
        record(
            7,
            "SYSTEM",
            "SYSTEM_MESSAGE",
            "DONE",
            AT_12,
            &still_running_quoting_restart,
        )
        .to_string(),
        planner_record(8, AT_12, None),
    ]);
    assert_eq!(
        phase(still_running_quoted),
        (AgentStatus::Running, TurnPhase::Parked),
        "a still-running SYSTEM_MESSAGE quoting finish or restart text ends no task"
    );
    let mut after_restart = parked;
    after_restart.extend([
        record(6, "SYSTEM", "SYSTEM_MESSAGE", "DONE", AT_12, restart_notice).to_string(),
        planner_record(7, AT_12, None),
    ]);
    assert_eq!(
        phase(after_restart),
        (AgentStatus::Success, TurnPhase::Idle),
        "the server-restart notice clears running background tasks"
    );
}

#[test]
fn background_shell_reports_follow_parked_stop_finish_kill_and_idle_repair() {
    use crate::agents::{BackgroundShell, BackgroundShellReport};

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let login_env = BTreeMap::from([(session::HOME_ENV.to_owned(), home.display().to_string())]);
    let conv = "0c100c27-850f-4226-b07a-72701c1f9d19";
    let task2 = format!("{conv}/task-2");
    let task4 = format!("{conv}/task-4");
    let t2_at = "2026-11-24T15:06:23Z";
    let t4_at = "2026-11-24T15:06:30Z";
    let start_row = |step: u64, at: &str, id: &str, cmd: &str| {
        record(
            step,
            "MODEL",
            "GENERIC",
            "RUNNING",
            at,
            &format!(
                "Created At: {at}\nTool is running as a background task with task id: {id}\n\
                 Task Description: {cmd}\n\
                 Task logs are available at: file://{}/brain/{conv}/.system_generated/tasks/task-{step}.log",
                home.display()
            ),
        )
        .to_string()
    };
    let finish_row = |step: u64, at: &str, id: &str, outcome: &str| {
        record(
            step,
            "SYSTEM",
            "SYSTEM_MESSAGE",
            "DONE",
            at,
            &format!(
                "The following is a <SYSTEM_MESSAGE> not actually sent by the user.\n\n\
                 <SYSTEM_MESSAGE>\n[Message] timestamp={at} sender={id} \
                 priority=MESSAGE_PRIORITY_HIGH content=Task id \"{id}\" {outcome}:\n\
                 Command exited with code 0.\n</SYSTEM_MESSAGE>"
            ),
        )
        .to_string()
    };
    let decode = |event: &str, payload: &Value| {
        AntigravityAdapter
            .decode_hook_in(event, payload, &login_env)
            .unwrap()
            .lifecycle()
            .cloned()
            .unwrap()
            .background_shells
    };
    let write_rows = |rows: &[String]| {
        write_transcript_named(home, conv, "transcript_full.jsonl", &rows.join("\n"))
    };
    let stop_payload = |path: &Path, fully_idle: bool| {
        json!({
            "conversationId": conv,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": path.display().to_string(),
            "fullyIdle": fully_idle,
            "terminationReason": "NO_TOOL_CALL",
            "error": "",
        })
    };

    // Sequence from live probe 0c100c27: two background commands start, turn parks.
    let mut rows = vec![
        user_record(0, "2026-11-24T15:06:12Z", "start two background tasks"),
        planner_record(
            1,
            "2026-11-24T15:06:12Z",
            Some(
                json!([{"name":"run_command","args":{"CommandLine":"sleep 240 && echo AAA","WaitMsBeforeAsync":500}}]),
            ),
        ),
        start_row(2, t2_at, &task2, "sleep 240 && echo AAA"),
        planner_record(
            3,
            "2026-11-24T15:06:26Z",
            Some(
                json!([{"name":"run_command","args":{"CommandLine":"sleep 900 && echo BBB","WaitMsBeforeAsync":500}}]),
            ),
        ),
        start_row(4, t4_at, &task4, "sleep 900 && echo BBB"),
        planner_record(5, "2026-11-24T15:06:32Z", None),
    ];
    let path = write_rows(&rows);
    let expected_two = vec![
        BackgroundShell {
            id: task2.clone(),
            command: Some("sleep 240 && echo AAA".to_owned()),
            description: None,
            started_at: t2_at.parse().unwrap(),
        },
        BackgroundShell {
            id: task4.clone(),
            command: Some("sleep 900 && echo BBB".to_owned()),
            description: None,
            started_at: t4_at.parse().unwrap(),
        },
    ];
    assert_eq!(
        decode("Stop", &stop_payload(&path, false)),
        Some(BackgroundShellReport::Snapshot {
            shells: expected_two.clone(),
        }),
        "parked Stop snapshots both running commands with id, command, and row created_at"
    );

    // Later turn ("Reply only OK") ends parked while earlier turn's commands still run.
    rows.extend([
        user_record(6, "2026-11-24T15:08:43Z", "Reply only OK"),
        planner_record(7, "2026-11-24T15:08:43Z", None),
    ]);
    write_rows(&rows);
    assert_eq!(
        decode("Stop", &stop_payload(&path, false)),
        Some(BackgroundShellReport::Snapshot {
            shells: expected_two,
        }),
        "a later turn's parked Stop keeps commands started in an earlier turn"
    );

    // Kill task-4 through manage_task PostToolUse:observed.
    let kill_payload = json!({
        "conversationId": conv,
        "workspacePaths": ["/workspace/project"],
        "transcriptPath": path.display().to_string(),
        "stepIdx": 10,
        "error": "",
        "toolCall": {
            "name": "manage_task",
            "args": { "Action": "kill", "TaskId": task4 }
        }
    });
    assert_eq!(
        decode("PostToolUse:observed", &kill_payload),
        Some(BackgroundShellReport::Finished {
            ids: vec![task4.clone()],
        }),
        "manage_task kill reports Finished for its TaskId"
    );

    // After the cancel row lands, a parked Stop lists only task-2.
    rows.push(finish_row(
        11,
        "2026-11-24T15:09:13Z",
        &task4,
        "was canceled with result",
    ));
    write_rows(&rows);
    assert_eq!(
        decode("Stop", &stop_payload(&path, false)),
        Some(BackgroundShellReport::Snapshot {
            shells: vec![BackgroundShell {
                id: task2.clone(),
                command: Some("sleep 240 && echo AAA".to_owned()),
                description: None,
                started_at: t2_at.parse().unwrap(),
            }],
        })
    );

    // Quoted start/finish/restart text in DONE tool output and still-running SYSTEM_MESSAGE
    // (including a command quoting finish or restart text) are ignored; task-2 stays listed.
    let quoted = format!(
        "Tool is running as a background task with task id: {conv}/task-99\n\
         Task Description: echo fake\n\
         Task id \"{task2}\" finished with result:\n\
         [Notice] All your subagents and background tasks have been stopped due to server restart."
    );
    let still_running = format!(
        "<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-24T15:09:30Z sender={task2} \
         priority=MESSAGE_PRIORITY_HIGH content=Background command is still running after 300s: \
         echo 'Task id \"{task2}\" finished with result:' && echo '[Notice] All your subagents and \
         background tasks have been stopped due to server restart'\n</SYSTEM_MESSAGE>"
    );
    rows.extend([
        record(
            12,
            "MODEL",
            "GENERIC",
            "DONE",
            "2026-11-24T15:09:20Z",
            &quoted,
        )
        .to_string(),
        record(
            13,
            "SYSTEM",
            "SYSTEM_MESSAGE",
            "DONE",
            "2026-11-24T15:09:30Z",
            &still_running,
        )
        .to_string(),
    ]);
    write_rows(&rows);
    assert_eq!(
        decode("Stop", &stop_payload(&path, false)),
        Some(BackgroundShellReport::Snapshot {
            shells: vec![BackgroundShell {
                id: task2.clone(),
                command: Some("sleep 240 && echo AAA".to_owned()),
                description: None,
                started_at: t2_at.parse().unwrap(),
            }],
        }),
        "quoted start/finish/restart text in DONE output and still-running notices are not task rows"
    );

    // Start row more than 64 KiB above the transcript's end is still listed (whole-file read).
    let mut padded = rows.clone();
    let filler = "x".repeat(8_000);
    for idx in 0..10 {
        padded.push(
            record(
                20 + idx,
                "MODEL",
                "GENERIC",
                "DONE",
                "2026-11-24T15:09:40Z",
                &filler,
            )
            .to_string(),
        );
    }
    let padded_path = write_rows(&padded);
    assert!(
        std::fs::metadata(&padded_path).unwrap().len() > 65_536,
        "fixture must exceed the 64 KiB tail"
    );
    assert_eq!(
        decode("Stop", &stop_payload(&padded_path, false)),
        Some(BackgroundShellReport::Snapshot {
            shells: vec![BackgroundShell {
                id: task2.clone(),
                command: Some("sleep 240 && echo AAA".to_owned()),
                description: None,
                started_at: t2_at.parse().unwrap(),
            }],
        }),
        "start rows beyond 64 KiB stay listed via whole-file read"
    );

    // Finish through run_command PostToolUse:mutating (clean and errored).
    let finish_hook = json!({
        "conversationId": conv,
        "workspacePaths": ["/workspace/project"],
        "transcriptPath": path.display().to_string(),
        "stepIdx": 2,
        "error": "",
        "toolCall": {
            "name": "run_command",
            "id": "call_1",
            "args": { "CommandLine": "sleep 240 && echo AAA", "WaitMsBeforeAsync": 500 }
        }
    });
    assert_eq!(
        decode("PostToolUse:mutating", &finish_hook),
        Some(BackgroundShellReport::Finished {
            ids: vec![task2.clone()],
        }),
        "run_command PostToolUse reports Finished for <conv>/task-<stepIdx>"
    );
    assert_eq!(
        decode(
            "PostToolUse:mutating",
            &with(&finish_hook, [("error", json!("exit 1"))]),
        ),
        Some(BackgroundShellReport::Finished {
            ids: vec![task2.clone()],
        }),
        "an errored run_command still finished"
    );

    // Missed finish repaired by fullyIdle: true Stop, from the payload alone (even while transcript
    // still lists task-2 or points at a missing file).
    assert_eq!(
        decode("Stop", &stop_payload(&padded_path, true)),
        Some(BackgroundShellReport::Snapshot { shells: Vec::new() }),
        "fullyIdle: true repairs a missed finish from the payload alone"
    );
    assert_eq!(
        decode("Stop", &stop_payload(Path::new("/tmp/missing.jsonl"), true)),
        Some(BackgroundShellReport::Snapshot { shells: Vec::new() }),
        "fullyIdle: true does not depend on reading the transcript"
    );

    // Stop parked on a native subagent alone lists nothing.
    let subagent_only = vec![
        user_record(0, AT_09, "spawn a child"),
        planner_record(
            1,
            AT_10,
            Some(
                json!([{"name":"invoke_subagent","args":{"Subagents":[{"Prompt":"work","Role":"helper","TypeName":"general"}]}}]),
            ),
        ),
        planner_record(2, AT_11, None),
    ];
    let sub_path = write_rows(&subagent_only);
    assert_eq!(
        decode("Stop", &stop_payload(&sub_path, false)),
        Some(BackgroundShellReport::Snapshot { shells: Vec::new() }),
        "a Stop parked on a native subagent alone lists no background shell"
    );

    // Real server-restart notice clears running tasks on parked Stop too.
    rows.push(
        record(
            14,
            "SYSTEM",
            "SYSTEM_MESSAGE",
            "DONE",
            "2026-11-24T15:10:00Z",
            "<SYSTEM_MESSAGE>\n[Message] timestamp=2026-11-24T15:10:00Z sender=system \
             content=[Notice] All your subagents and background tasks have been stopped due to server restart.\n</SYSTEM_MESSAGE>",
        )
        .to_string(),
    );
    write_rows(&rows);
    assert_eq!(
        decode("Stop", &stop_payload(&path, false)),
        Some(BackgroundShellReport::Snapshot { shells: Vec::new() }),
        "a server-restart notice clears earlier start rows from the Stop snapshot"
    );
}

#[test]
fn background_shell_reports_handle_edges_guards_and_queued_post_tool_use() {
    use crate::agents::{BackgroundShell, BackgroundShellReport};

    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("account");
    let view_home = dir.path().join("view_home");
    let login_env = BTreeMap::from([
        ("HOME".to_owned(), view_home.display().to_string()),
        (session::HOME_ENV.to_owned(), home.display().to_string()),
    ]);
    let decode = |event: &str, payload: &Value| {
        AntigravityAdapter
            .decode_hook_in(event, payload, &login_env)
            .expect("hook decode must not fail")
            .lifecycle()
            .cloned()
    };

    // Missing transcript, path outside path guard, and symlink all report nothing on parked Stop.
    let outside = dir.path().join("outside/transcript_full.jsonl");
    std::fs::create_dir_all(outside.parent().unwrap()).unwrap();
    let start_line = record(
        2,
        "MODEL",
        "GENERIC",
        "RUNNING",
        AT_10,
        &format!(
            "Created At: {AT_10}\nTool is running as a background task with task id: {SESSION_ID}/task-2\n\
             Task Description: sleep 60\n"
        ),
    )
    .to_string();
    std::fs::write(&outside, &start_line).unwrap();

    let symlink_path = home
        .join("brain")
        .join(CHILD_ALPHA)
        .join(".system_generated/logs/transcript_full.jsonl");
    std::fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &symlink_path).unwrap();

    let missing_path = home
        .join("brain")
        .join(SESSION_ID)
        .join(".system_generated/logs/transcript_full.jsonl");

    for (label, session_id, transcript) in [
        ("missing", SESSION_ID, missing_path.as_path()),
        ("outside guard", SESSION_ID, outside.as_path()),
        ("symlink", CHILD_ALPHA, symlink_path.as_path()),
    ] {
        let obs = decode(
            "Stop",
            &json!({
                "conversationId": session_id,
                "workspacePaths": ["/workspace/project"],
                "transcriptPath": transcript.display().to_string(),
                "fullyIdle": false,
                "terminationReason": "NO_TOOL_CALL",
            }),
        )
        .unwrap();
        assert_eq!(obs.background_shells, None, "{label} reports nothing");
    }

    // Named-account mount remap + missing Task Description + unparseable created_at fallback +
    // failed Stop with fullyIdle: false still snapshots running commands.
    let no_desc_bad_time = record(
        3,
        "MODEL",
        "GENERIC",
        "RUNNING",
        "not-a-timestamp",
        &format!(
            "Tool is running as a background task with task id: {SESSION_ID}/task-3\n\
             Task logs are available at: file:///tmp/task-3.log"
        ),
    )
    .to_string();
    write_transcript_named(
        &home,
        SESSION_ID,
        "transcript_full.jsonl",
        &[start_line.clone(), no_desc_bad_time].join("\n"),
    );
    let mounted_transcript = view_home
        .join(".gemini/antigravity-cli/brain")
        .join(SESSION_ID)
        .join(".system_generated/logs/transcript_full.jsonl");
    let before = Timestamp::now();
    let failed_stop = decode(
        "Stop",
        &json!({
            "conversationId": SESSION_ID,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": mounted_transcript.display().to_string(),
            "fullyIdle": false,
            "terminationReason": "ERROR",
            "error": "boom",
        }),
    )
    .unwrap();
    let after = Timestamp::now();
    assert_eq!(
        failed_stop.signal,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
        }
    );
    let Some(BackgroundShellReport::Snapshot { shells }) = failed_stop.background_shells else {
        panic!("failed Stop with fullyIdle: false must still snapshot running commands");
    };
    assert_eq!(shells.len(), 2);
    assert_eq!(
        shells[0],
        BackgroundShell {
            id: format!("{SESSION_ID}/task-2"),
            command: Some("sleep 60".to_owned()),
            description: None,
            started_at: AT_10.parse().unwrap(),
        }
    );
    assert_eq!(shells[1].id, format!("{SESSION_ID}/task-3"));
    assert_eq!(shells[1].command, None);
    assert_eq!(shells[1].description, None);
    assert!(
        shells[1].started_at >= before && shells[1].started_at <= after,
        "unparseable created_at falls back to hook time: {:?}",
        shells[1].started_at
    );

    // A >64 KiB transcript containing invalid UTF-8 bytes in an unrelated row still snapshots
    // running commands via a single lossy whole-file read.
    let mut lossy_bytes = format!("{start_line}\n").into_bytes();
    let filler = "x".repeat(8_000);
    for idx in 0..10 {
        lossy_bytes.extend_from_slice(
            record(20 + idx, "MODEL", "GENERIC", "DONE", AT_11, &filler)
                .to_string()
                .as_bytes(),
        );
        lossy_bytes.push(b'\n');
    }
    lossy_bytes.extend_from_slice(b"{\"step_index\":31,\"content\":\"\xff\xfe\"}\n");
    let lossy_path = home
        .join("brain")
        .join(SESSION_ID)
        .join(".system_generated/logs/transcript_full.jsonl");
    std::fs::write(&lossy_path, &lossy_bytes).unwrap();
    assert!(
        std::fs::metadata(&lossy_path).unwrap().len() > 65_536,
        "lossy fixture must exceed 64 KiB"
    );
    assert_eq!(
        decode(
            "Stop",
            &json!({
                "conversationId": SESSION_ID,
                "workspacePaths": ["/workspace/project"],
                "transcriptPath": mounted_transcript.display().to_string(),
                "fullyIdle": false,
                "terminationReason": "NO_TOOL_CALL",
            }),
        )
        .unwrap()
        .background_shells,
        Some(BackgroundShellReport::Snapshot {
            shells: vec![BackgroundShell {
                id: format!("{SESSION_ID}/task-2"),
                command: Some("sleep 60".to_owned()),
                description: None,
                started_at: AT_10.parse().unwrap(),
            }],
        }),
        "lossy whole-file read tolerates invalid UTF-8 bytes in a >64 KiB transcript"
    );

    // Failed Stop with fullyIdle: true still emits empty Snapshot.
    let failed_idle = decode(
        "Stop",
        &json!({
            "conversationId": SESSION_ID,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": mounted_transcript.display().to_string(),
            "fullyIdle": true,
            "terminationReason": "ERROR",
        }),
    )
    .unwrap();
    assert_eq!(
        failed_idle.background_shells,
        Some(BackgroundShellReport::Snapshot { shells: Vec::new() })
    );

    // Bare `/compact` Stop reports nothing regardless of fullyIdle.
    for fully_idle in [Some(true), Some(false), None] {
        let mut payload = json!({
            "conversationId": SESSION_ID,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": mounted_transcript.display().to_string(),
            "terminationReason": "TERMINAL_CUSTOM_HOOK",
            "lastUserInput": "/compact",
        });
        if let Some(idle) = fully_idle {
            payload["fullyIdle"] = json!(idle);
        }
        let compact = decode("Stop", &payload).unwrap();
        assert!(matches!(
            compact.signal,
            LifecycleSignal::CompactionEnded { .. }
        ));
        assert_eq!(
            compact.background_shells, None,
            "bare /compact Stop reports no shells (fullyIdle={fully_idle:?})"
        );
    }

    // A post-compaction continuation's Stop still carries `lastUserInput: "/compact"` but ends
    // as an ordinary turn (`NO_TOOL_CALL`), so it carries the background shell snapshot for both
    // `fullyIdle: false` (transcript read) and `fullyIdle: true` (empty repair snapshot).
    let continuation_parked = decode(
        "Stop",
        &json!({
            "conversationId": SESSION_ID,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": mounted_transcript.display().to_string(),
            "fullyIdle": false,
            "terminationReason": "NO_TOOL_CALL",
            "lastUserInput": "/compact",
        }),
    )
    .unwrap();
    assert_eq!(
        continuation_parked.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
            turn_id: None,
        }
    );
    assert_eq!(
        continuation_parked.background_shells,
        Some(BackgroundShellReport::Snapshot {
            shells: vec![BackgroundShell {
                id: format!("{SESSION_ID}/task-2"),
                command: Some("sleep 60".to_owned()),
                description: None,
                started_at: AT_10.parse().unwrap(),
            }],
        }),
        "post-compaction continuation Stop with fullyIdle: false carries running shells"
    );
    let continuation_idle = decode(
        "Stop",
        &json!({
            "conversationId": SESSION_ID,
            "workspacePaths": ["/workspace/project"],
            "transcriptPath": mounted_transcript.display().to_string(),
            "fullyIdle": true,
            "terminationReason": "NO_TOOL_CALL",
            "lastUserInput": "/compact",
        }),
    )
    .unwrap();
    assert_eq!(
        continuation_idle.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        }
    );
    assert_eq!(
        continuation_idle.background_shells,
        Some(BackgroundShellReport::Snapshot { shells: Vec::new() }),
        "post-compaction continuation Stop with fullyIdle: true carries an empty snapshot"
    );

    // PostToolUse negative cases: missing stepIdx, non-kill manage_task, missing/blank/failed
    // TaskId, and unrelated tools all report nothing.
    for (label, event, payload) in [
        (
            "run_command without stepIdx",
            "PostToolUse:mutating",
            json!({"conversationId": SESSION_ID, "toolCall": {"name": "run_command"}}),
        ),
        (
            "manage_task list",
            "PostToolUse:observed",
            json!({"conversationId": SESSION_ID, "toolCall": {"name": "manage_task", "args": {"Action": "list"}}}),
        ),
        (
            "manage_task kill without TaskId",
            "PostToolUse:observed",
            json!({"conversationId": SESSION_ID, "toolCall": {"name": "manage_task", "args": {"Action": "kill"}}}),
        ),
        (
            "manage_task kill blank TaskId",
            "PostToolUse:observed",
            json!({"conversationId": SESSION_ID, "toolCall": {"name": "manage_task", "args": {"Action": "kill", "TaskId": "   "}}}),
        ),
        (
            "manage_task kill non-string TaskId",
            "PostToolUse:observed",
            json!({"conversationId": SESSION_ID, "toolCall": {"name": "manage_task", "args": {"Action": "kill", "TaskId": 42}}}),
        ),
        (
            "manage_task kill failed",
            "PostToolUse:observed",
            json!({"conversationId": SESSION_ID, "error": "not found", "toolCall": {"name": "manage_task", "args": {"Action": "kill", "TaskId": "conv/task-2"}}}),
        ),
        (
            "read_file with stepIdx",
            "PostToolUse:observed",
            json!({"conversationId": SESSION_ID, "stepIdx": 2, "toolCall": {"name": "read_file"}}),
        ),
    ] {
        let obs = decode(event, &payload).unwrap();
        assert_eq!(obs.background_shells, None, "{label}");
    }

    // Queued PostToolUse keeps stepIdx and manage_task args intact across queue round-trip.
    for (event, payload, expected_id) in [
        (
            "PostToolUse:mutating",
            json!({
                "conversationId": SESSION_ID,
                "stepIdx": 7,
                "error": "",
                "toolCall": {"name": "run_command", "args": {"CommandLine": "sleep 30"}}
            }),
            format!("{SESSION_ID}/task-7"),
        ),
        (
            "PostToolUse:observed",
            json!({
                "conversationId": SESSION_ID,
                "stepIdx": 10,
                "error": "",
                "toolCall": {"name": "manage_task", "args": {"Action": "kill", "TaskId": format!("{SESSION_ID}/task-9")}}
            }),
            format!("{SESSION_ID}/task-9"),
        ),
    ] {
        let queued = AntigravityAdapter
            .queued_hook_payload(event, &payload)
            .unwrap_or_else(|| payload.clone());
        let round_tripped: Value =
            serde_json::from_str(&serde_json::to_string(&queued).unwrap()).unwrap();
        assert_eq!(
            decode(event, &round_tripped).unwrap().background_shells,
            Some(BackgroundShellReport::Finished {
                ids: vec![expected_id],
            }),
            "queued {event} preserves shell report"
        );
    }
}

#[test]
fn bare_compact_user_row_leaves_transcript_fold_state_and_activity_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let fold_lines = |lines: &[String]| {
        write_transcript_named(
            dir.path(),
            SESSION_ID,
            "transcript_full.jsonl",
            &lines.join("\n"),
        );
        session::discover_under(dir.path(), workspace).remove(0)
    };
    let checkpoint = record(5, "SYSTEM", "CHECKPOINT", "DONE", AT_12, "summary").to_string();

    // At rest: bare /compact (wrapped or unwrapped) + checkpoint leaves status, prompt, and
    // activity identical.
    let rested = vec![
        user_record(0, AT_09, "start"),
        planner_record(1, AT_10, None),
    ];
    let rested_obs = fold_lines(&rested);
    let rested_state = local_state(&rested_obs);
    assert_eq!(
        (rested_state.status, rested_state.phase),
        (AgentStatus::Success, TurnPhase::Idle)
    );
    for (label, compact_content) in [
        ("wrapped", "<USER_REQUEST>\n/compact\n</USER_REQUEST>"),
        ("unwrapped", "/compact"),
    ] {
        let mut with_compact = rested.clone();
        with_compact.push(user_record(2, AT_12, compact_content));
        with_compact.push(checkpoint.clone());
        let after = fold_lines(&with_compact);
        assert_eq!(
            local_state(&after),
            rested_state,
            "{label} /compact at rest must keep status, phase, and latest prompt"
        );
        assert_eq!(
            after.last_activity, rested_obs.last_activity,
            "{label} /compact at rest must not advance last_activity"
        );
    }

    // Parked turn (live capture): lines 0..4 vs lines 0..6 (with /compact + CHECKPOINT) keep
    // status, phase, latest prompt, and last_activity unchanged instead of flipping to Reasoning.
    let captured: Vec<String> = include_str!("tests/fixtures/compaction/parked-transcript.jsonl")
        .lines()
        .map(ToOwned::to_owned)
        .collect();
    let before_compact = fold_lines(&captured[..4]);
    let after_compact = fold_lines(&captured[..6]);
    assert_eq!(
        (
            local_state(&before_compact).status,
            local_state(&before_compact).phase
        ),
        (AgentStatus::Running, TurnPhase::Parked)
    );
    assert_eq!(
        local_state(&after_compact),
        local_state(&before_compact),
        "bare /compact on a parked turn must keep Parked state and latest prompt"
    );
    assert_eq!(
        after_compact.last_activity, before_compact.last_activity,
        "bare /compact on a parked turn must not advance last_activity"
    );

    // `/compact <message>` is still an ordinary user turn start.
    let mut with_instruction = rested;
    with_instruction.push(user_record(2, AT_12, "/compact focus on tests"));
    let instructed = fold_lines(&with_instruction);
    assert_eq!(
        (
            local_state(&instructed).status,
            local_state(&instructed).phase
        ),
        (AgentStatus::Running, TurnPhase::Reasoning)
    );
    assert_eq!(
        local_state(&instructed).latest_prompt.as_deref(),
        Some("/compact focus on tests")
    );
}

#[test]
fn transcript_questions_project_and_clear_native_waits() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = Path::new("/workspace/project");
    let array_questions = json_value(
        r#"[{"name":"ask_question","args":{"questions":[{"question":"  First choice?  ","options":["a","b"]},{"question":"Second choice?","options":[]}]}}]"#,
    );
    let encoded_questions = json_value(
        r#"[{"name":"ask_question","args":{"questions":"[{\"question\":\"Replacement?\",\"options\":[\"yes\",\"no\"]}]"}}]"#,
    );
    let ask_again =
        json_value(r#"[{"name":"ask_question","args":{"questions":[{"question":"One more?"}]}}]"#);
    let waiting = |detail, at| {
        (
            AgentStatus::Waiting,
            TurnPhase::Idle,
            Some(detail),
            Some(at),
        )
    };
    for (ending, expected) in [
        (
            vec![planner_record(1, AT_10, Some(array_questions))],
            waiting("First choice?", AT_10),
        ),
        (
            vec![planner_record(1, AT_11, Some(encoded_questions))],
            waiting("Replacement?", AT_11),
        ),
        (
            vec![
                planner_record(1, AT_10, Some(ask_again.clone())),
                planner_record(2, AT_11, None),
            ],
            (AgentStatus::Success, TurnPhase::Idle, None, None),
        ),
        (
            vec![
                planner_record(1, AT_10, Some(ask_again)),
                user_record(2, AT_11, "continue"),
            ],
            (AgentStatus::Running, TurnPhase::Reasoning, None, None),
        ),
    ] {
        let state = state_after(dir.path(), workspace, ending);
        assert_eq!(
            (
                state.status,
                state.phase,
                state.native_prompt_detail.as_deref(),
                state.waiting_since
            ),
            (
                expected.0,
                expected.1,
                expected.2,
                expected.3.map(|at| at.parse().unwrap())
            )
        );
    }
    for (calls, expected) in [
        (r#""not-an-array""#, (AgentStatus::Success, TurnPhase::Idle)),
        (
            r#"[{"name":"ask_question","args":{"questions":"not-json"}}]"#,
            (AgentStatus::Running, TurnPhase::Reasoning),
        ),
        (
            r#"[{"name":"ask_question","args":{"questions":[]}}]"#,
            (AgentStatus::Running, TurnPhase::Reasoning),
        ),
        (
            r#"[{"name":"another_tool","args":{"questions":[{"question":"ignored"}]}}]"#,
            (AgentStatus::Running, TurnPhase::Reasoning),
        ),
    ] {
        let ending = vec![planner_record(1, AT_10, Some(json_value(calls)))];
        let state = state_after(dir.path(), workspace, ending);
        assert_eq!((state.status, state.phase), expected, "{calls}");
        assert!(state.native_prompt_detail.is_none() && state.waiting_since.is_none());
    }
}

#[test]
fn invoke_subagent_results_pair_ordered_child_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let parent = write_subagent_fixture(
        dir.path(),
        SESSION_ID,
        include_str!("tests/fixtures/subagents_two_children.jsonl"),
    );
    for child in [CHILD_ALPHA, CHILD_BETA] {
        write_transcript_named(
            dir.path(),
            child,
            "transcript.jsonl",
            include_str!("tests/fixtures/transcript_full.jsonl"),
        );
    }

    let alpha = session::correlate_subagent_under(
        dir.path(),
        &parent,
        SESSION_ID,
        &workspace,
        CHILD_ALPHA,
        &workspace,
    )
    .unwrap();
    let beta = session::correlate_subagent_under(
        dir.path(),
        &parent,
        SESSION_ID,
        &workspace,
        CHILD_BETA,
        &workspace,
    )
    .unwrap();
    let alpha_prompt = format!(
        "Read the security policy in {}/SECURITY.md and provide a concise summary of the reporting process.",
        workspace.display()
    );
    let beta_prompt = format!(
        "Search {}/AGENTS.md for references to nextest and summarize the testing requirements related to it.",
        workspace.display()
    );
    let spawned = session::spawned_subagents_under(dir.path(), &parent, SESSION_ID, &workspace);
    assert_eq!(
        spawned
            .iter()
            .map(|child| (
                child.child_agent_id.as_str(),
                child.agent_name.as_deref(),
                child.role.as_deref(),
                child.prompt.as_deref(),
            ))
            .collect::<Vec<_>>(),
        [
            (
                CHILD_ALPHA,
                Some("summary_agent"),
                Some("Security Document Summarizer"),
                Some(alpha_prompt.as_str()),
            ),
            (
                CHILD_BETA,
                Some("research"),
                Some("Codebase Researcher"),
                Some(beta_prompt.as_str()),
            ),
        ]
    );
    assert_eq!(
        (
            alpha.type_name.as_str(),
            alpha.role.as_str(),
            alpha.prompt.as_str()
        ),
        (
            "summary_agent",
            "Security Document Summarizer",
            alpha_prompt.as_str()
        )
    );
    assert_eq!(
        (
            beta.type_name.as_str(),
            beta.role.as_str(),
            beta.prompt.as_str()
        ),
        ("research", "Codebase Researcher", beta_prompt.as_str())
    );
}

#[test]
fn nested_child_relation_uses_its_immediate_parent_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let parent = write_subagent_fixture(
        dir.path(),
        CHILD_ALPHA,
        include_str!("tests/fixtures/subagent_nested_child.jsonl"),
    );
    write_transcript_named(
        dir.path(),
        NESTED_CHILD,
        "transcript.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );

    let nested = session::correlate_subagent_under(
        dir.path(),
        &parent,
        CHILD_ALPHA,
        &workspace,
        NESTED_CHILD,
        &workspace,
    )
    .unwrap();
    assert_eq!(nested.type_name, "explore");
    assert_eq!(nested.role, "Nested researcher");
    assert_eq!(nested.prompt, "Trace the nested call.");
    let spawned = session::spawned_subagents_under(dir.path(), &parent, CHILD_ALPHA, &workspace);
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].child_agent_id.as_str(), NESTED_CHILD);
    assert_eq!(spawned[0].agent_name.as_deref(), Some("explore"));
    assert_eq!(spawned[0].role.as_deref(), Some("Nested researcher"));
    assert_eq!(spawned[0].prompt.as_deref(), Some("Trace the nested call."));
}

#[test]
fn subagent_correlation_rejects_unsafe_identity_uri_and_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let other_workspace = dir.path().join("other-workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&other_workspace).unwrap();
    let fixture = include_str!("tests/fixtures/subagents_two_children.jsonl");
    let parent = write_subagent_fixture(dir.path(), SESSION_ID, fixture);
    for child in [CHILD_ALPHA, CHILD_BETA] {
        write_transcript_named(
            dir.path(),
            child,
            "transcript.jsonl",
            include_str!("tests/fixtures/transcript_full.jsonl"),
        );
    }
    let correlate = |path: &Path, parent_id: &str, child_workspace: &Path| {
        session::correlate_subagent_under(
            dir.path(),
            path,
            parent_id,
            &workspace,
            CHILD_ALPHA,
            child_workspace,
        )
    };
    assert!(correlate(&parent, CHILD_ALPHA, &workspace).is_none());
    assert!(correlate(&parent, SESSION_ID, &other_workspace).is_none());

    let escaped = rewrite_subagent_result(fixture, |content| {
        content.replace(
            &format!("brain/{CHILD_ALPHA}/.system_generated/logs/transcript.jsonl"),
            &format!("brain/{CHILD_BETA}/.system_generated/logs/transcript.jsonl"),
        )
    });
    let escaped_parent = write_subagent_fixture(dir.path(), SESSION_ID, &escaped);
    assert!(correlate(&escaped_parent, SESSION_ID, &workspace).is_none());
    assert!(
        session::correlate_subagent_under(
            dir.path(),
            &escaped_parent,
            SESSION_ID,
            &workspace,
            CHILD_BETA,
            &workspace,
        )
        .is_some()
    );

    let remote = rewrite_subagent_result(fixture, |content| {
        content.replacen("__HOME_URI__", "https://example.invalid", 1)
    });
    let remote_parent = write_subagent_fixture(dir.path(), SESSION_ID, &remote);
    assert!(correlate(&remote_parent, SESSION_ID, &workspace).is_none());
    assert!(
        session::correlate_subagent_under(
            dir.path(),
            &remote_parent,
            SESSION_ID,
            &workspace,
            CHILD_BETA,
            &workspace,
        )
        .is_some()
    );

    let remote_workspace = fixture.replace("__WORKSPACE_URI__", "https://example.invalid");
    let remote_workspace_parent = write_subagent_fixture(dir.path(), SESSION_ID, &remote_workspace);
    assert!(correlate(&remote_workspace_parent, SESSION_ID, &workspace).is_none());

    let relative_workspace = fixture.replacen(
        "\"TypeName\":\"summary_agent\"}",
        "\"TypeName\":\"summary_agent\",\"Workspace\":\"relative\"}",
        1,
    );
    let relative_parent = write_subagent_fixture(dir.path(), SESSION_ID, &relative_workspace);
    assert!(correlate(&relative_parent, SESSION_ID, &workspace).is_none());
}

#[test]
fn subagent_correlation_keeps_inherit_sibling_of_branch_child() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    for child in [CHILD_ALPHA, CHILD_BETA] {
        write_transcript_named(
            dir.path(),
            child,
            "transcript.jsonl",
            include_str!("tests/fixtures/transcript_full.jsonl"),
        );
    }
    let fixture = include_str!("tests/fixtures/subagents_two_children.jsonl")
        .replacen(
            "\"TypeName\":\"summary_agent\"}",
            "\"TypeName\":\"summary_agent\",\"Workspace\":\"branch\"}",
            1,
        )
        .replacen(
            "\"TypeName\":\"research\"}",
            "\"TypeName\":\"research\",\"Workspace\":\"inherit\"}",
            1,
        );
    let parent = write_subagent_fixture(dir.path(), SESSION_ID, &fixture);
    assert!(
        session::correlate_subagent_under(
            dir.path(),
            &parent,
            SESSION_ID,
            &workspace,
            CHILD_ALPHA,
            &workspace
        )
        .is_none()
    );
    assert!(
        session::correlate_subagent_under(
            dir.path(),
            &parent,
            SESSION_ID,
            &workspace,
            CHILD_BETA,
            &workspace
        )
        .is_some()
    );
    let spawned = session::spawned_subagents_under(dir.path(), &parent, SESSION_ID, &workspace);
    assert_eq!(spawned.len(), 1);
    assert_eq!(spawned[0].child_agent_id.as_str(), CHILD_BETA);
}

#[test]
fn subagent_correlation_rejects_torn_mismatched_and_duplicate_results() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    for child in [CHILD_ALPHA, CHILD_BETA] {
        write_transcript_named(
            dir.path(),
            child,
            "transcript.jsonl",
            include_str!("tests/fixtures/transcript_full.jsonl"),
        );
    }
    let fixture = include_str!("tests/fixtures/subagents_two_children.jsonl");
    let assert_rejected = |fixture: &str| {
        let parent = write_subagent_fixture(dir.path(), SESSION_ID, fixture);
        for child in [CHILD_ALPHA, CHILD_BETA] {
            assert!(
                session::correlate_subagent_under(
                    dir.path(),
                    &parent,
                    SESSION_ID,
                    &workspace,
                    child,
                    &workspace,
                )
                .is_none()
            );
        }
        assert!(
            session::spawned_subagents_under(dir.path(), &parent, SESSION_ID, &workspace,)
                .is_empty()
        );
    };
    let mismatched = rewrite_subagent_result(fixture, |content| {
        content
            .split_once(&format!("{{\n  \"conversationId\": \"{CHILD_BETA}\""))
            .map(|(before, _)| before.to_owned())
            .unwrap()
    });
    assert_rejected(&mismatched);
    let extra = rewrite_subagent_result(fixture, |content| {
        format!(
            "{content}\n{{\"conversationId\":\"extra-child\",\"logAbsoluteUri\":\"file:///tmp/extra.jsonl\"}}"
        )
    });
    assert_rejected(&extra);
    let duplicate = fixture.replace(CHILD_BETA, CHILD_ALPHA);
    assert_rejected(&duplicate);
    assert_rejected(&format!("{fixture}\nnot json\n"));
    let malformed = rewrite_subagent_result(fixture, |content| {
        content.replacen("\"logAbsoluteUri\"", "\"missingLogUri\"", 1)
    });
    assert_rejected(&malformed);

    let parent = write_subagent_fixture(dir.path(), SESSION_ID, fixture);
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&parent)
        .unwrap();
    write!(file, "\n{{\"step_index\":3").unwrap();
    for child in [CHILD_ALPHA, CHILD_BETA] {
        assert!(
            session::correlate_subagent_under(
                dir.path(),
                &parent,
                SESSION_ID,
                &workspace,
                child,
                &workspace,
            )
            .is_none()
        );
    }
    assert!(
        session::spawned_subagents_under(dir.path(), &parent, SESSION_ID, &workspace,).is_empty()
    );
}

#[test]
fn subagent_correlation_does_not_scan_beyond_the_transcript_tail() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    write_transcript_named(
        dir.path(),
        CHILD_ALPHA,
        "transcript.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );
    write_transcript_named(
        dir.path(),
        CHILD_BETA,
        "transcript.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );
    let mut records = include_str!("tests/fixtures/subagents_two_children.jsonl")
        .lines()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    records.insert(
        2,
        json!({
            "step_index": 99,
            "source": "SYSTEM",
            "type": "CHECKPOINT",
            "status": "DONE",
            "created_at": AT_12,
            "content": "x".repeat(70 * 1024),
        })
        .to_string(),
    );
    let parent = write_subagent_fixture(dir.path(), SESSION_ID, &records.join("\n"));
    assert!(std::fs::metadata(&parent).unwrap().len() > 64 * 1024);
    assert!(
        session::correlate_subagent_under(
            dir.path(),
            &parent,
            SESSION_ID,
            &workspace,
            CHILD_ALPHA,
            &workspace,
        )
        .is_none()
    );
}
#[test]
fn discovery_prefers_full_transcripts_and_cache_for_fresh_pairing_only() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let full = write_transcript(dir.path(), SESSION_ID);
    let legacy = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript.jsonl",
        &include_str!("tests/fixtures/transcript_full.jsonl").replace("ping", "legacy prompt"),
    );
    assert_eq!(
        session::latest_prompt_under(dir.path(), &full, SESSION_ID).as_deref(),
        Some("ping")
    );
    assert_eq!(
        session::latest_prompt_under(dir.path(), &legacy, SESSION_ID).as_deref(),
        Some("legacy prompt")
    );
    std::fs::create_dir_all(dir.path().join("cache")).unwrap();
    std::fs::write(
        dir.path().join("cache/last_conversations.json"),
        json!({workspace.to_str().unwrap(): SESSION_ID}).to_string(),
    )
    .unwrap();
    let observations = session::discover_under(dir.path(), &workspace);
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    let at = AT_09.parse().unwrap();
    assert_eq!(
        (
            observation.session_id.as_str(),
            observation.transcript_path.as_path(),
            observation.first_event_at,
            observation.last_activity,
            observation.fresh_binding_at,
            local_state(observation).latest_prompt.as_deref()
        ),
        (
            SESSION_ID,
            full.as_path(),
            Some(at),
            at,
            Some(at),
            Some("ping")
        )
    );
    let other = dir.path().join("other");
    std::fs::create_dir(&other).unwrap();
    let exact_resume_only = session::discover_under(dir.path(), &other);
    assert_eq!(exact_resume_only.len(), 1);
    assert!(exact_resume_only[0].fresh_binding_at.is_none());
    let unknown = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_debug.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );
    assert!(session::latest_prompt_under(dir.path(), &unknown, SESSION_ID).is_none());
    assert!(session::latest_prompt_under(dir.path(), &full, "other-conversation").is_none());
    assert_eq!(
        session::resolve_home(Some(OsStr::new("/tmp/agy")), Some(OsStr::new("/home/user"))),
        Some(Path::new("/tmp/agy").to_path_buf())
    );
    assert_eq!(
        session::resolve_home(None, Some(OsStr::new("/home/user"))),
        Some(Path::new("/home/user/.gemini/antigravity-cli").to_path_buf())
    );
}
#[test]
fn discovery_cache_batches_workspaces_and_revalidates_changed_dependencies() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let transcript = write_transcript(dir.path(), SESSION_ID);
    std::fs::create_dir_all(dir.path().join("cache")).unwrap();
    let index = dir.path().join("cache/last_conversations.json");
    std::fs::write(
        &index,
        json!({first.to_str().unwrap(): SESSION_ID}).to_string(),
    )
    .unwrap();
    let mut cache = session::DiscoveryCacheHarness::new();
    let start = Instant::now();

    assert_eq!(
        cache
            .refresh(dir.path(), &[first.as_path(), second.as_path()], start)
            .len(),
        2
    );
    assert_eq!(cache.work(), (1, 1, 1));
    assert_eq!(
        cache
            .refresh(dir.path(), &[first.as_path(), second.as_path()], start)
            .len(),
        2
    );
    assert_eq!(cache.work(), (1, 1, 1));

    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(transcript)
            .unwrap(),
        "{}",
        user_record(9, "2026-07-13T23:24:00Z", "changed")
    )
    .unwrap();
    assert_eq!(
        cache
            .refresh(dir.path(), &[first.as_path(), second.as_path()], start)
            .len(),
        2
    );
    assert_eq!(cache.work(), (1, 1, 2));

    std::fs::write(
        index,
        json!({second.to_str().unwrap(): SESSION_ID}).to_string(),
    )
    .unwrap();
    cache.refresh(dir.path(), &[first.as_path(), second.as_path()], start);
    assert_eq!(cache.work(), (1, 2, 2));

    cache.refresh(
        dir.path(),
        &[first.as_path(), second.as_path()],
        start + Duration::from_secs(30),
    );
    assert_eq!(cache.work(), (2, 3, 3));
}
#[test]
fn bounded_discovery_can_lose_an_older_prompt_from_a_long_turn() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let mut records = vec![user_record(0, "2026-07-13T23:00:00Z", "sticky label")];
    for step in 1..=900 {
        records.push(planner_record(step, AT_10, Some(json!([]))));
    }
    let transcript = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &records.join("\n"),
    );
    assert!(std::fs::metadata(&transcript).unwrap().len() > 64 * 1024);
    let observation = session::discover_under(dir.path(), &workspace).remove(0);
    assert_eq!(local_state(&observation).status, AgentStatus::Success);
    assert!(local_state(&observation).latest_prompt.is_none());
}
#[test]
fn first_invocation_recovers_only_the_latest_completed_visible_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let torn = user_record(5, "2026-07-13T23:23:13Z", "torn");
    let mut lines = vec!["not-json".to_owned()];
    for (step, source, kind, status, at, content) in [
        (
            0,
            "SYSTEM",
            "CHECKPOINT",
            "DONE",
            "2026-07-13T23:23:08Z",
            "internal description",
        ),
        (
            1,
            "USER_EXPLICIT",
            "USER_INPUT",
            "IN_PROGRESS",
            AT_10,
            "partial description",
        ),
        (2, "MODEL", "USER_INPUT", "DONE", AT_11, "wrong source"),
    ] {
        lines.push(record(step, source, kind, status, at, content).to_string());
    }
    lines.extend([
        user_record(4, AT_12, "  label this turn  "),
        torn[..torn.len() / 2].to_owned(),
    ]);
    let transcript = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &lines.join("\n"),
    );
    let payload = with(
        &hook_payload(),
        [
            ("transcriptPath", json!(transcript)),
            ("invocationNum", json!(0)),
        ],
    );
    let started = observe_lifecycle_with_prompt_reader("PreInvocation", &payload, |path, id, _| {
        session::latest_prompt_under(dir.path(), path, id)
    })
    .unwrap();
    assert_eq!(started.prompt.as_deref(), Some("label this turn"));
    let stopped = observe_lifecycle_with_prompt_reader(
        "Stop",
        &with(
            &payload,
            [
                ("fullyIdle", json!(true)),
                ("terminationReason", json!("model_stop")),
            ],
        ),
        |_, _, _| panic!("Stop must not read transcripts"),
    )
    .unwrap();
    assert!(stopped.prompt.is_none());
    let unknown = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_debug.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    );
    let rejected = observe_lifecycle_with_prompt_reader(
        "PreInvocation",
        &with(&payload, [("transcriptPath", json!(unknown))]),
        |path, id, _| session::latest_prompt_under(dir.path(), path, id),
    )
    .unwrap();
    assert!(rejected.prompt.is_none());
}
#[test]
fn post_invocation_reports_each_prompt_taken_in_mid_turn_once() {
    // Step indexes and invocation fields from the 1.5.14 capture: one steer taken in by
    // invocation 2 (begun with 8 steps), one by invocation 5 (begun with 15), and the
    // continuation opener once the background command finished.
    let dir = tempfile::tempdir().unwrap();
    let mut lines = vec![
        user_record(0, AT_09, "run three commands"),
        planner_record(1, AT_09, None),
        planner_record(7, AT_09, None),
    ];
    let post = |lines: &[String], invocation: u64, initial_steps: u64| {
        let transcript = write_transcript_named(
            dir.path(),
            SESSION_ID,
            "transcript_full.jsonl",
            &lines.join("\n"),
        );
        let payload = with(
            &hook_payload(),
            [
                ("transcriptPath", json!(transcript)),
                ("invocationNum", json!(invocation)),
                ("initialNumSteps", json!(initial_steps)),
            ],
        );
        observe_lifecycle_with_prompt_reader("PostInvocation", &payload, |path, id, from| {
            session::prompt_taken_in_under(dir.path(), path, id, from?)
        })
    };
    assert!(
        post(&lines, 0, 1).is_none(),
        "the opener reported the first prompt"
    );
    assert!(post(&lines, 1, 7).is_none());
    lines.extend([
        user_record(
            8,
            AT_10,
            "<USER_REQUEST>Also include STEERAPPLE.</USER_REQUEST>",
        ),
        planner_record(9, AT_10, None),
    ]);
    let reported = post(&lines, 2, 8).expect("the invocation that took the steer in reports it");
    assert_eq!(
        reported.signal,
        LifecycleSignal::TurnStarted { turn_id: None }
    );
    assert!(
        reported.is_state_snapshot(),
        "a steer joins the running turn rather than opening one"
    );
    assert_eq!(reported.prompt.as_deref(), Some("Also include STEERAPPLE."));
    assert!(
        post(&lines, 3, 10).is_none(),
        "each later invocation begins past it"
    );
    assert!(post(&lines, 4, 13).is_none());
    lines.extend([user_record(15, AT_11, "Also include STEERBERRY.")]);
    assert_eq!(
        post(&lines, 5, 15).and_then(|reported| reported.prompt.map(|prompt| prompt.to_string())),
        Some("Also include STEERBERRY.".to_owned()),
    );
    lines.push(planner_record(16, AT_11, None));
    assert!(
        post(&lines, 0, 17).is_none(),
        "a continuation takes no prompt in"
    );

    // Live run 1, case 3: two steers into one invocation, one row each, both reported in the
    // order agy took them in.
    let two_steers = vec![
        user_record(0, AT_09, "sleep"),
        planner_record(32, AT_10, None),
        user_record(33, AT_10, "Also include the word RZLIVE3TA."),
        user_record(34, AT_10, "Also include the word RZLIVE3TB."),
        planner_record(35, AT_11, None),
    ];
    assert_eq!(
        post(&two_steers, 4, 33)
            .and_then(|reported| reported.prompt.map(|prompt| prompt.to_string())),
        Some("Also include the word RZLIVE3TA.\n\nAlso include the word RZLIVE3TB.".to_owned()),
    );

    let drained = vec![
        user_record(0, AT_09, "sleep"),
        planner_record(3, AT_10, None),
        user_record(4, AT_11, "Also include PLUM."),
    ];
    assert!(
        post(&drained, 0, 5).is_none(),
        "a queued prompt agy drains at the turn's end is the opener's to report"
    );
    assert!(
        post(&drained, 1, 3).is_none(),
        "a report applied after the drain stops at its own invocation's answer"
    );
    let apart = vec![
        user_record(0, AT_09, "sleep"),
        user_record(8, AT_10, "Also include APPLE."),
        record(
            9,
            "SYSTEM",
            "SYSTEM_MESSAGE",
            "DONE",
            AT_10,
            "Task 2 completed",
        )
        .to_string(),
        user_record(10, AT_10, "Also include BERRY."),
        planner_record(11, AT_11, None),
        user_record(12, AT_11, "the next turn's prompt"),
    ];
    assert_eq!(
        post(&apart, 2, 8).and_then(|reported| reported.prompt.map(|prompt| prompt.to_string())),
        Some("Also include APPLE.\n\nAlso include BERRY.".to_owned()),
        "a row that is no answer between two steers ends nothing"
    );
    // A prompt steered into a post-compaction continuation (`lastUserInput: "/compact"`) is
    // reported once, and the read stops at a `CHECKPOINT` row so a late pre-compaction
    // report never picks up a post-compaction prompt.
    let continuation_steer = [
        user_record(0, AT_09, "run background"),
        planner_record(1, AT_09, None),
        user_record(4, AT_10, "/compact"),
        record(5, "SYSTEM", "CHECKPOINT", "DONE", AT_10, "summary").to_string(),
        user_record(7, AT_11, "Also include STEERAFTER."),
        planner_record(8, AT_11, None),
    ];
    let continuation_path = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &continuation_steer.join("\n"),
    );
    let compact_steer = with(
        &hook_payload(),
        [
            ("transcriptPath", json!(continuation_path)),
            ("invocationNum", json!(1)),
            ("initialNumSteps", json!(7)),
            ("lastUserInput", json!("/compact")),
        ],
    );
    let reported =
        observe_lifecycle_with_prompt_reader("PostInvocation", &compact_steer, |p, id, from| {
            session::prompt_taken_in_under(dir.path(), p, id, from?)
        })
        .expect("a steer into a post-compaction continuation is reported");
    assert_eq!(reported.prompt.as_deref(), Some("Also include STEERAFTER."));
    assert!(reported.is_state_snapshot());
    let after_steer = with(
        &compact_steer,
        [("invocationNum", json!(2)), ("initialNumSteps", json!(9))],
    );
    assert!(
        observe_lifecycle_with_prompt_reader("PostInvocation", &after_steer, |p, id, from| {
            session::prompt_taken_in_under(dir.path(), p, id, from?)
        })
        .is_none(),
        "a later invocation begins past the steered prompt"
    );
    let pre_checkpoint = with(
        &compact_steer,
        [("invocationNum", json!(0)), ("initialNumSteps", json!(4))],
    );
    assert!(
        observe_lifecycle_with_prompt_reader("PostInvocation", &pre_checkpoint, |p, id, from| {
            session::prompt_taken_in_under(dir.path(), p, id, from?)
        })
        .is_none(),
        "a read starting before a CHECKPOINT stops at the checkpoint and drops /compact"
    );
    let unreadable = with(
        &hook_payload(),
        [("invocationNum", json!(2)), ("initialNumSteps", json!(8))],
    );
    let fields = decode_lifecycle_fields("PostInvocation", &unreadable, |_, _, _| {
        PromptEvidence::Unread
    });
    assert!(
        fields.lifecycle.is_none(),
        "an unread transcript reports nothing"
    );
    assert_eq!(fields.agent_id.as_deref(), Some(SESSION_ID));
}
/// The invocation-0 openers and transcripts of the agy 1.5.15 capture: a first prompt (bound 1),
/// a message drained at a turn's end (bound 5), a background command's result after a steer
/// taken in at step 3 (bound 8), and a native subagent's result waking the parent (bound 4).
#[test]
fn an_opener_reports_only_the_prompt_that_opened_its_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let openers = include_str!("tests/fixtures/openers/openers.jsonl")
        .lines()
        .map(json_value);
    for opener in openers {
        let transcript = match opener["case"].as_str().unwrap() {
            "background" => include_str!("tests/fixtures/openers/background.jsonl"),
            "drain" => include_str!("tests/fixtures/openers/drain.jsonl"),
            "subagent" => include_str!("tests/fixtures/openers/subagent.jsonl"),
            _ => continue,
        };
        let payload = &opener["payload"];
        let bound = payload["initialNumSteps"].as_u64().unwrap();
        let continuation = matches!(
            (opener["case"].as_str().unwrap(), bound),
            ("background", 8) | ("subagent", 4)
        );
        let at_hook = transcript
            .lines()
            .filter(|line| json_value(line)["step_index"].as_u64().unwrap() < bound)
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        for contents in [at_hook.as_str(), transcript] {
            let id = payload["conversationId"].as_str().unwrap();
            let path = write_transcript_named(dir.path(), id, "transcript_full.jsonl", contents);
            let payload = with(payload, [("transcriptPath", json!(path))]);
            let fields = decode_lifecycle_fields("PreInvocation", &payload, |path, id, read| {
                session::read_prompt_under(dir.path(), path, id, read)
            });
            assert_eq!(fields.agent_id.as_deref(), Some(id));
            if continuation {
                assert!(
                    fields.lifecycle.is_none(),
                    "{opener}: {:?}",
                    fields.lifecycle
                );
                continue;
            }
            let started = fields.lifecycle.expect("a prompt opened the invocation");
            assert_eq!(
                started.signal,
                LifecycleSignal::TurnStarted { turn_id: None }
            );
            assert_eq!(
                started.prompt.as_deref(),
                payload["lastUserInput"].as_str(),
                "{opener}"
            );
            assert!(!started.is_state_snapshot());
        }
    }
    // An execution that ended with no answer row leaves its prompt below the next opener's.
    let unanswered = [
        user_record(0, AT_09, "an interrupted prompt"),
        user_record(1, AT_10, "the next prompt"),
    ]
    .join("\n");
    let opened = |contents: &str, bound: u64| {
        let path =
            write_transcript_named(dir.path(), SESSION_ID, "transcript_full.jsonl", contents);
        let payload = with(
            &hook_payload(),
            [
                ("transcriptPath", json!(path)),
                ("invocationNum", json!(0)),
                ("initialNumSteps", json!(bound)),
            ],
        );
        decode_lifecycle_fields("PreInvocation", &payload, |path, id, read| {
            session::read_prompt_under(dir.path(), path, id, read)
        })
        .lifecycle
        .map(|started| started.prompt.map(|prompt| prompt.to_string()))
    };
    assert_eq!(
        opened(&unanswered, 2),
        Some(Some("the next prompt".to_owned()))
    );

    // Rows that are no answer, more than the 64 KiB tail holds.
    let filler = |steps: std::ops::Range<u64>| {
        steps
            .map(|step| {
                let content = "x".repeat(1024);
                format!(
                    "{}\n",
                    record(step, "MODEL", "GENERIC", "DONE", AT_11, &content)
                )
            })
            .collect::<String>()
    };
    let background = include_str!("tests/fixtures/openers/background.jsonl");
    assert_eq!(
        opened(&format!("{background}{}", filler(10..90)), 8),
        None,
        "a continuation applied after agy wrote past the tail"
    );
    let fresh = format!(
        "{}\n{}",
        user_record(0, AT_09, "a fresh prompt"),
        filler(1..81)
    );
    assert_eq!(
        opened(&fresh, 1),
        Some(Some("a fresh prompt".to_owned())),
        "a fresh prompt applied after agy wrote past the tail"
    );
    assert_eq!(
        opened(&fresh, 81),
        Some(Some("a fresh prompt".to_owned())),
        "the tail starts inside the interval, after its prompt"
    );
    // A `/compact` sent while the turn was parked writes a user row and a checkpoint, and no
    // answer (the decoding rule for `/compact`); no agy capture covers a wake after one.
    let compacted = [
        user_record(0, AT_09, "run it in the background"),
        planner_record(1, AT_09, None),
        user_record(2, AT_10, "/compact"),
        record(3, "SYSTEM", "CHECKPOINT", "DONE", AT_10, "summary").to_string(),
    ]
    .join("\n");
    assert_eq!(opened(&compacted, 4), None, "a /compact opens nothing");
}
#[test]
fn an_opener_that_cannot_read_its_evidence_still_starts_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let opener = |case: &str, bound: u64| {
        include_str!("tests/fixtures/openers/openers.jsonl")
            .lines()
            .map(json_value)
            .find(|opener| opener["case"] == case && opener["payload"]["initialNumSteps"] == bound)
            .unwrap()["payload"]
            .clone()
    };
    let decode = |payload: &Value| {
        decode_lifecycle_fields("PreInvocation", payload, |path, id, read| {
            session::read_prompt_under(dir.path(), path, id, read)
        })
        .lifecycle
        .map(|started| {
            (
                started.signal,
                started.prompt.map(|prompt| prompt.to_string()),
            )
        })
    };
    let started = |prompt: Option<&str>| {
        Some((
            LifecycleSignal::TurnStarted { turn_id: None },
            prompt.map(ToOwned::to_owned),
        ))
    };
    let background = include_str!("tests/fixtures/openers/background.jsonl");
    let continuation = opener("background", 8);
    let id = continuation["conversationId"].as_str().unwrap();
    let path = write_transcript_named(dir.path(), id, "transcript_full.jsonl", background);
    let continuation = with(&continuation, [("transcriptPath", json!(path))]);

    let mut unbounded = continuation.clone();
    unbounded.as_object_mut().unwrap().remove("initialNumSteps");
    assert_eq!(
        decode(&unbounded),
        started(Some(
            "After the first command finishes, launch the second command in the background and reply STARTED without waiting."
        )),
        "with no bound the opener reads the newest prompt"
    );
    let short = background.lines().take(7).collect::<Vec<_>>().join("\n");
    write_transcript_named(dir.path(), id, "transcript_full.jsonl", &short);
    assert_eq!(
        decode(&continuation),
        started(None),
        "the row below the bound is not on disk"
    );
    let missing = with(
        &continuation,
        [("transcriptPath", json!(dir.path().join("missing.jsonl")))],
    );
    assert_eq!(decode(&missing), started(None));
    // A native child's first opener: bound 0, and agy has not created its transcript yet.
    let child = opener("subagent-child", 0);
    let child_id = child["conversationId"].as_str().unwrap();
    let child_path = dir
        .path()
        .join("brain")
        .join(child_id)
        .join(".system_generated/logs/transcript_full.jsonl");
    assert_eq!(
        decode(&with(&child, [("transcriptPath", json!(child_path))])),
        started(None)
    );
}
#[test]
fn parked_turn_compaction_capture_decodes_every_hook_by_its_opener_and_stop_reason() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = include_str!("tests/fixtures/compaction/parked-transcript.jsonl");
    let lines: Vec<&str> = transcript.lines().collect();
    let payloads: Vec<Value> = include_str!("tests/fixtures/compaction/parked-payloads.jsonl")
        .lines()
        .map(json_value)
        .collect();
    let env = BTreeMap::from([(
        "RIMZ_ANTIGRAVITY_HOME".to_owned(),
        dir.path().to_string_lossy().into_owned(),
    )]);
    let decode_sequence = |full_transcript: bool| {
        let mut decoded_rows = Vec::new();
        for row in &payloads {
            let payload = &row["payload"];
            let event = catalog_event_for_capture(row["event"].as_str().unwrap(), payload);
            let id = payload["conversationId"].as_str().unwrap();
            let content = if full_transcript {
                transcript.to_owned()
            } else {
                let count = row
                    .get("steps")
                    .and_then(Value::as_u64)
                    .map_or(lines.len(), |n| n as usize);
                format!("{}\n", lines[..count].join("\n"))
            };
            let path = write_transcript_named(dir.path(), id, "transcript_full.jsonl", &content);
            let payload = with(payload, [("transcriptPath", json!(path))]);
            let output = AntigravityAdapter
                .decode_hook_in(event, &payload, &env)
                .unwrap();
            assert_eq!(
                output.event_agent_id().map(AgentSessionId::as_str),
                Some(id)
            );
            decoded_rows.push((
                output.lifecycle().map(|obs| obs.signal.clone()),
                output
                    .lifecycle()
                    .and_then(|obs| obs.prompt.as_deref().map(ToOwned::to_owned)),
                output.final_message().map(ToOwned::to_owned),
            ));
        }
        decoded_rows
    };
    let expected = vec![
        (
            Some(LifecycleSignal::TurnStarted { turn_id: None }),
            Some(
                "Call run_command with CommandLine \"sleep 150 && echo SLEPT\" and WaitMsBeforeAsync 500. Do not poll or wait for the command; immediately reply STARTED and end the turn. If later woken by the background result, reply only SLEPT with no tools."
                    .to_owned(),
            ),
            None,
        ),
        (None, None, None),
        (None, None, None),
        (None, None, None),
        (
            Some(LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: true,
                turn_id: None,
            }),
            None,
            Some("STARTED".to_owned()),
        ),
        (Some(LifecycleSignal::Compacting), None, None),
        (
            Some(LifecycleSignal::CompactionEnded {
                auto: Some(false),
                failed: false,
            }),
            None,
            None,
        ),
        (
            Some(LifecycleSignal::ToolUsed {
                effect: ToolEffect::Mutating,
                name: Some("run_command".to_owned()),
                native_key: None,
                turn_id: None,
            }),
            None,
            None,
        ),
        (None, None, None),
        (None, None, None),
        (
            Some(LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            }),
            None,
            Some("SLEPT".to_owned()),
        ),
    ];
    assert_eq!(
        decode_sequence(false),
        expected,
        "at-hook transcript snapshots"
    );
    // Applying the openers late against the final transcript produces the exact same signals.
    let late = decode_sequence(true);
    let late_signals: Vec<_> = late.iter().map(|(signal, _, _)| signal.clone()).collect();
    let expected_signals: Vec<_> = expected
        .iter()
        .map(|(signal, _, _)| signal.clone())
        .collect();
    assert_eq!(
        late_signals, expected_signals,
        "late apply against final transcript"
    );
}
#[test]
fn queued_midturn_compact_opener_decodes_to_compacting() {
    let dir = tempfile::tempdir().unwrap();
    let opener = json_value(include_str!(
        "tests/fixtures/compaction/midturn-enter-opener.json"
    ));
    let payload = &opener["payload"];
    let id = payload["conversationId"].as_str().unwrap();
    let path = write_transcript_named(
        dir.path(),
        id,
        "transcript_full.jsonl",
        include_str!("tests/fixtures/compaction/midturn-enter-transcript.jsonl"),
    );
    let payload = with(payload, [("transcriptPath", json!(path))]);
    let fields = decode_lifecycle_fields("PreInvocation", &payload, |path, id, read| {
        session::read_prompt_under(dir.path(), path, id, read)
    });
    let obs = fields.lifecycle.expect("queued /compact opens compaction");
    assert_eq!(obs.signal, LifecycleSignal::Compacting);
    assert!(obs.prompt.is_none());
    // Transcript evidence classifies the opener even without `lastUserInput`.
    let mut without_last = payload.clone();
    without_last
        .as_object_mut()
        .unwrap()
        .remove("lastUserInput");
    let from_transcript =
        decode_lifecycle_fields("PreInvocation", &without_last, |path, id, read| {
            session::read_prompt_under(dir.path(), path, id, read)
        })
        .lifecycle
        .expect("step initialNumSteps - 1 being /compact opens compaction");
    assert_eq!(from_transcript.signal, LifecycleSignal::Compacting);
    assert!(from_transcript.prompt.is_none());
}
#[test]
fn opener_with_unread_evidence_or_missing_bound_falls_back_to_last_user_input() {
    let dir = tempfile::tempdir().unwrap();
    let decode = |payload: &Value| {
        decode_lifecycle_fields("PreInvocation", payload, |path, id, read| {
            session::read_prompt_under(dir.path(), path, id, read)
        })
        .lifecycle
        .map(|obs| (obs.signal, obs.prompt.map(|p| p.to_string())))
    };
    let missing_path = dir.path().join("missing.jsonl");
    let compact_unbounded = with(
        &hook_payload(),
        [
            ("invocationNum", json!(0)),
            ("lastUserInput", json!("/compact")),
            ("transcriptPath", json!(missing_path)),
        ],
    );
    assert_eq!(
        decode(&compact_unbounded),
        Some((LifecycleSignal::Compacting, None)),
        "missing initialNumSteps falls back to lastUserInput"
    );
    let compact_unread = with(&compact_unbounded, [("initialNumSteps", json!(5))]);
    assert_eq!(
        decode(&compact_unread),
        Some((LifecycleSignal::Compacting, None)),
        "Unread transcript falls back to lastUserInput"
    );
    let prompt_unread = with(
        &hook_payload(),
        [
            ("invocationNum", json!(0)),
            ("initialNumSteps", json!(5)),
            ("lastUserInput", json!("hello")),
            ("transcriptPath", json!(missing_path)),
        ],
    );
    assert_eq!(
        decode(&prompt_unread),
        Some((LifecycleSignal::TurnStarted { turn_id: None }, None)),
        "Unread non-compact opener falls back to TurnStarted"
    );
    // A CHECKPOINT row starts an opener interval on its own, even when the 64 KiB tail
    // starts after the earlier /compact row.
    let big_checkpoint = format!(
        "{}\n{}\n{}\n",
        user_record(0, AT_09, "initial"),
        user_record(1, AT_10, "/compact"),
        record(
            2,
            "SYSTEM",
            "CHECKPOINT",
            "DONE",
            AT_10,
            &"s".repeat(70_000)
        ),
    );
    let checkpoint_path = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &big_checkpoint,
    );
    let continuation = with(
        &hook_payload(),
        [
            ("invocationNum", json!(0)),
            ("initialNumSteps", json!(3)),
            ("lastUserInput", json!("/compact")),
            ("transcriptPath", json!(checkpoint_path)),
        ],
    );
    assert_eq!(
        decode(&continuation),
        None,
        "a CHECKPOINT row at initialNumSteps - 1 classifies a continuation as routing only"
    );
}
#[test]
fn steered_altm_compact_capture_decodes_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = include_str!("tests/fixtures/compaction/midturn-altm-transcript.jsonl");
    let payloads: Vec<Value> =
        include_str!("tests/fixtures/compaction/midturn-altm-payloads.jsonl")
            .lines()
            .map(json_value)
            .collect();
    let env = BTreeMap::from([(
        "RIMZ_ANTIGRAVITY_HOME".to_owned(),
        dir.path().to_string_lossy().into_owned(),
    )]);
    let mut signals = Vec::new();
    for row in &payloads {
        let event = match row["event"].as_str().unwrap() {
            "PostToolUse" => "PostToolUse:mutating",
            other => other,
        };
        let payload = &row["payload"];
        let id = payload["conversationId"].as_str().unwrap();
        let path = write_transcript_named(dir.path(), id, "transcript_full.jsonl", transcript);
        let payload = with(payload, [("transcriptPath", json!(path))]);
        let output = AntigravityAdapter
            .decode_hook_in(event, &payload, &env)
            .unwrap();
        signals.push(output.lifecycle().map(|obs| obs.signal.clone()));
    }
    assert_eq!(
        signals,
        vec![
            Some(LifecycleSignal::TurnStarted { turn_id: None }),
            Some(LifecycleSignal::ToolUsed {
                effect: ToolEffect::Mutating,
                name: Some("run_command".to_owned()),
                native_key: None,
                turn_id: None,
            }),
            None,
            None,
            Some(LifecycleSignal::CompactionEnded {
                auto: Some(false),
                failed: false,
            }),
            Some(LifecycleSignal::TurnStarted { turn_id: None }),
            None,
            Some(LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            }),
        ]
    );
}
#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_conversation_directories() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let escaped = tempfile::tempdir().unwrap();
    let transcript = escaped
        .path()
        .join(".system_generated/logs/transcript_full.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        include_str!("tests/fixtures/transcript_full.jsonl"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("brain")).unwrap();
    symlink(escaped.path(), dir.path().join("brain").join(SESSION_ID)).unwrap();
    assert!(session::discover_under(dir.path(), Path::new("/workspace/project")).is_empty());
}
#[test]
fn allowed_tools_translate_row_by_row_and_leave_the_rest_out() {
    let rows = [
        ("Bash", Some("command(*)")),
        ("Bash(*)", Some("command(*)")),
        ("Bash(git status)", Some("command(git status)")),
        ("Bash(git status:*)", Some("command(git status)")),
        ("Bash(git *)", Some("command(git)")),
        ("Bash(git * --force)", None),
        ("Bash(git*)", None),
        ("Read", Some("read_file(*)")),
        ("Edit", Some("write_file(*)")),
        ("Write", Some("write_file(*)")),
        ("Read(//etc/hosts)", Some("read_file(/etc/hosts)")),
        ("Read(/docs/**)", Some("read_file(docs)")),
        ("Read(./src)", Some("read_file(src)")),
        ("Edit(src/lib.rs)", Some("write_file(src/lib.rs)")),
        ("Write(//tmp/out/**)", Some("write_file(/tmp/out)")),
        ("Read(/**)", None),
        ("Read(~/notes)", None),
        ("Read(src/*.rs)", None),
        ("Read(src/**/mod.rs)", None),
        ("Edit(file?.md)", None),
        ("Read(docs/[ab])", None),
        ("Read({a,b})", None),
        ("WebFetch", Some("read_url(*)")),
        (
            "WebFetch(domain:example.com)",
            Some("read_url(example.com)"),
        ),
        ("WebFetch(https://example.com)", None),
        ("WebFetch(domain:*)", Some("read_url(*)")),
        ("WebFetch(domain:https://example.com)", None),
        ("WebFetch(domain:*.example.com)", None),
        ("WebFetch(domain:example.com/docs)", None),
        ("WebFetch(domain:example.com:8080)", None),
        ("WebFetch(domain:a b)", None),
        ("Bash(regex:^git)", None),
        ("Bash(regex:rm *)", None),
        ("Bash(echo (a))", None),
        ("Write(.//var/opt/reports/**)", None),
        ("Read(///x)", None),
        ("Read(//)", None),
        ("Read(src/(a))", None),
        (
            "mcp__github__create_issue",
            Some("mcp(github/create_issue)"),
        ),
        ("mcp__github", Some("mcp(github/*)")),
        ("mcp__github__*", Some("mcp(github/*)")),
        (
            "mcp__rimz__agents_list",
            Some("mcp(rimz_tools/agents_list)"),
        ),
        ("mcp__rimz", Some("mcp(rimz_tools/*)")),
        ("mcp__github__create_*", None),
        ("mcp__*", None),
        ("Glob", None),
        ("Grep", None),
        ("Agent", None),
        ("Skill(commit)", None),
        ("WebSearch", None),
        ("Bogus", None),
        ("Bash*", None),
    ];
    let actual: Vec<_> = rows
        .iter()
        .map(|(rule, _)| (*rule, permissions::translate(&rule.parse().unwrap())))
        .collect();
    let expected: Vec<_> = rows
        .iter()
        .map(|(rule, translated)| (*rule, translated.map(ToOwned::to_owned)))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn allowed_tools_merge_into_the_one_settings_document() {
    let root = tempfile::tempdir().unwrap();
    let paths = (root.path(), root.path());
    let rules: Vec<crate::config::ToolRule> =
        ["Bash(git status:*)", "Read", "Glob", "Bash(git status)"]
            .iter()
            .map(|rule| rule.parse().unwrap())
            .collect();
    let settings = |args: &[String]| -> Vec<String> {
        args.windows(2)
            .filter(|pair| pair[0] == "--settings")
            .map(|pair| pair[1].clone())
            .collect()
    };

    let mut args = vec!["--mode".to_owned(), "plan".to_owned()];
    let mut artifact = None;
    permissions::render(&rules, paths, &mut args, &mut artifact).unwrap();
    assert!(artifact.is_none());
    assert_eq!(args[..2], ["--mode", "plan"]);
    let [inline] = settings(&args).try_into().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&inline).unwrap(),
        json!({"permissions": {"allow": ["command(git status)", "read_file(*)"]}})
    );

    let mut args = vec![
        "--settings".to_owned(),
        r#"{"permissions":{"allow":["command(ls)"]},"enableTerminalSandbox":true}"#.to_owned(),
    ];
    permissions::render(&rules, paths, &mut args, &mut None).unwrap();
    let [inline] = settings(&args).try_into().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&inline).unwrap(),
        json!({
            "permissions": {"allow": ["command(ls)", "command(git status)", "read_file(*)"]},
            "enableTerminalSandbox": true
        })
    );

    std::fs::write(
        root.path().join("user.json"),
        r#"{"permissions":{"deny":["command(rm)"]}}"#,
    )
    .unwrap();
    let mut args = vec!["--settings".to_owned(), "user.json".to_owned()];
    let mut artifact = None;
    permissions::render(&rules, paths, &mut args, &mut artifact).unwrap();
    let (path, value, source) = artifact.expect("a file source becomes a private artifact");
    assert_eq!(settings(&args), [path.display().to_string()]);
    assert_eq!(source, root.path().join("user.json"));
    assert_eq!(
        value,
        json!({"permissions": {
            "deny": ["command(rm)"],
            "allow": ["command(git status)", "read_file(*)"]
        }})
    );

    let mut args = vec![
        "--settings".to_owned(),
        "\u{feff}{\"enableTerminalSandbox\":true}".to_owned(),
    ];
    let mut artifact = None;
    permissions::render(&rules, paths, &mut args, &mut artifact).unwrap();
    assert!(artifact.is_none(), "a BOM before the brace is still inline");
    let [inline] = settings(&args).try_into().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&inline).unwrap(),
        json!({
            "permissions": {"allow": ["command(git status)", "read_file(*)"]},
            "enableTerminalSandbox": true
        })
    );

    std::fs::write(
        root.path().join("bom.json"),
        "\u{feff}{\"permissions\":{\"deny\":[\"command(rm)\"]}}",
    )
    .unwrap();
    let mut args = vec!["--settings".to_owned(), "bom.json".to_owned()];
    let mut artifact = None;
    permissions::render(&rules, paths, &mut args, &mut artifact).unwrap();
    let (_, value, _) = artifact.expect("a file source becomes a private artifact");
    assert_eq!(value["permissions"]["deny"], json!(["command(rm)"]));

    std::fs::write(root.path().join("list.json"), "[1]").unwrap();
    std::fs::write(root.path().join("bad-bom.json"), "\u{feff}{oops").unwrap();
    for (source, reason) in [
        ("{oops", "invalid JSON"),
        ("\u{feff}{oops", "invalid JSON"),
        ("bad-bom.json", "invalid JSON"),
        ("list.json", "expected a JSON object"),
    ] {
        let mut args = vec!["--settings".to_owned(), source.to_owned()];
        let error = permissions::render(&rules, paths, &mut args, &mut None)
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("invalid launch settings at ")
                && error.contains(reason)
                && error
                    .ends_with("; correct that key, or remove the definition's allowed-tools list"),
            "{error}"
        );
    }

    let untranslated: Vec<crate::config::ToolRule> = ["Glob", "Grep"]
        .iter()
        .map(|rule| rule.parse().unwrap())
        .collect();
    let mut args = vec!["--mode".to_owned(), "plan".to_owned()];
    permissions::render(&untranslated, paths, &mut args, &mut None).unwrap();
    assert_eq!(args, ["--mode", "plan"]);
}

#[test]
fn sandbox_and_added_dirs_ride_agy_flags_once() {
    use crate::config::Isolation;
    assert!(AntigravityAdapter.has_native_sandbox());
    assert_eq!(AntigravityAdapter.added_dirs(), AddedDirs::Workspace);
    for (isolation, passthrough, expected) in [
        (Isolation::Host, false, &[][..]),
        (Isolation::Scoped, false, &[][..]),
        (Isolation::Sandbox, false, &["--sandbox"][..]),
        (Isolation::Sandbox, true, &["--sandbox"][..]),
        (Isolation::Host, true, &["--sandbox"][..]),
    ] {
        let mut args: Vec<String> = passthrough
            .then(|| "--sandbox".to_owned())
            .into_iter()
            .collect();
        AntigravityAdapter.configure_room_permissions(
            &mut args,
            &BTreeMap::new(),
            (Path::new("/workspace/project"), None),
            &[],
            isolation,
        );
        assert_eq!(args, expected, "{isolation:?} passthrough={passthrough}");
    }

    let mut args = vec!["--add-dir".to_owned(), "/a".to_owned()];
    AntigravityAdapter
        .allow_added_dirs_args(
            &["/a".to_owned(), "/b".to_owned(), "/c".to_owned()],
            (Path::new("/workspace/project"), Path::new("/tmp")),
            &mut args,
            &mut None,
        )
        .unwrap();
    assert_eq!(
        args,
        ["--add-dir", "/a", "--add-dir", "/b", "--add-dir", "/c"]
    );

    for (mode, overridden) in [
        (PermissionMode::Ask, false),
        (PermissionMode::Auto, false),
        (PermissionMode::Plan, false),
        (PermissionMode::Yolo, true),
    ] {
        let argv = AntigravityAdapter.spec().launch.permission_args(mode);
        assert_eq!(
            AntigravityAdapter.native_sandbox_overridden(&argv),
            overridden,
            "{mode:?}"
        );
    }
}
#[test]
fn launch_resume_permissions_and_model_preset_match_agy() {
    let extra = ["--sandbox".to_owned()];
    assert_eq!(
        AntigravityAdapter
            .launch_command(&extra, Some("review"))
            .unwrap(),
        ["agy", "--sandbox", "--prompt-interactive", "review"]
    );
    assert_eq!(
        AntigravityAdapter
            .resume_command(SESSION_ID, Path::new("/workspace/project"))
            .unwrap(),
        ["agy", "--conversation", SESSION_ID]
    );
    for (mode, expected) in [
        (
            PermissionMode::Ask,
            &["--settings", r#"{"agentMode":""}"#][..],
        ),
        (PermissionMode::Auto, &["--mode", "auto"][..]),
        (PermissionMode::Plan, &["--mode", "plan"][..]),
        (
            PermissionMode::Yolo,
            &["--dangerously-skip-permissions"][..],
        ),
    ] {
        assert_eq!(
            AntigravityAdapter.spec().launch.permission_args(mode),
            expected
        );
    }
    for (command, expected) in [
        (
            "agy --conversation 11111111-1111-4111-8111-111111111111",
            Some(SESSION_ID),
        ),
        (
            "/home/user/.local/bin/agy --conversation=11111111-1111-4111-8111-111111111111",
            Some(SESSION_ID),
        ),
        ("agy --continue", None),
        ("agy -c", None),
        ("agy --conversation=", None),
        ("agy --conversation --mode plan", None),
        ("agy", None),
        (
            "echo agy --conversation 11111111-1111-4111-8111-111111111111",
            None,
        ),
    ] {
        let resumed = AntigravityAdapter.resumed_session_id_from_cmdline(command);
        assert_eq!(resumed.as_deref(), expected);
    }
}
mod usage_probe {
    use super::super::usage::{self, Quota, Read};
    use super::*;
    use crate::agents::account::{AccountProbe, AccountProbeError};
    use crate::agents::context::{AgentAccount, AgentRateLimits, WindowSource};
    use crate::agents::{AccountUsageProbe, AccountUsageSnapshot};
    use jiff::Timestamp;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};
    const OBSERVED_AT: &str = "2026-06-15T08:00:00Z";
    const FIVE_HOUR_RESET: &str = "2026-06-15T12:00:00Z";
    const WEEK_RESET: &str = "2026-06-20T08:00:00Z";
    fn observed_at() -> Timestamp {
        OBSERVED_AT.parse().unwrap()
    }
    fn envelope(buckets: Value) -> String {
        json!({"status": "SUCCESS", "num_turns": 0, "command": {"name": "usage", "data": {
            "groups": [{"name": "Gemini", "buckets": buckets}]
        }}})
        .to_string()
    }
    fn usage_buckets() -> Value {
        json!([
            {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.75, "reset_time": FIVE_HOUR_RESET},
            {"id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0.5, "reset_time": WEEK_RESET}
        ])
    }
    fn fold(buckets: Value) -> Quota {
        match usage::classify(Some(0), envelope(buckets).as_bytes(), observed_at()) {
            Read::Usage(quota) => quota,
            other => panic!("expected a usage read, got {other:?}"),
        }
    }
    fn windows(buckets: Value) -> AgentRateLimits {
        match fold(buckets) {
            Quota::Windows(limits) => limits,
            other => panic!("expected windows, got {other:?}"),
        }
    }
    fn script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("agy");
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
    #[test]
    fn every_outcome_maps_both_probe_columns() {
        let found = format!(
            "{:?}",
            AccountProbe::Found(AgentAccount {
                metered: Some(true),
                ..AgentAccount::default()
            })
        );
        let failed = AccountUsageProbe::Failed(Default::default());
        let no_credentials = AccountUsageProbe::NoCredentials(Default::default());
        let usage_found = AccountUsageProbe::Found {
            identity: Default::default(),
            snapshot: AccountUsageSnapshot {
                rate_limits: Some(windows(usage_buckets())),
                plan: None,
                ..Default::default()
            },
        };
        let unknown = json!([{"id": "gemini-month", "window": "monthly", "remaining_fraction": 0.5,
            "reset_time": FIVE_HOUR_RESET}]);
        let auth =
            r#"{"status":"ERROR","error":"Authentication failed or timed out","num_turns":0}"#;
        let offline = r#"{"status":"ERROR","error":"/usage failed: retrieving quota summary"}"#;
        let credits = r#"{"status":"SUCCESS","command":{"name":"credits","data":{}}}"#;
        // agy 1.5.14 under GEMINI_API_KEY with modelProvider "gemini", verbatim.
        let api_key = r#"{"conversation_id":"","status":"ERROR","response":"","error":"/usage failed: not available with a Gemini API key; see https://aistudio.google.com/usage for the key's usage","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"finished_reason":"error"}"#;
        let signed_out = r#"{"conversation_id":"","status":"ERROR","response":"","error":"authentication failed or timed out","duration_seconds":0,"num_turns":0,"usage":{"input_tokens":0,"output_tokens":0,"thinking_tokens":0,"cache_read_tokens":0,"total_tokens":0},"finished_reason":"error"}"#;
        let api_key_account = format!(
            "{:?}",
            AccountProbe::Found(AgentAccount {
                plan: Some("API key".to_owned()),
                metered: Some(false),
                ..AgentAccount::default()
            })
        );
        let no_data = r#"{"status":"SUCCESS","command":{"name":"usage"}}"#;
        let unavailable = |cause| format!("{:?}", AccountProbe::Unavailable(cause));
        for (case, code, stdout, account, usage) in [
            (
                "windows",
                Some(0),
                envelope(usage_buckets()),
                found.clone(),
                usage_found,
            ),
            (
                "no bucket",
                Some(0),
                envelope(json!([])),
                found.clone(),
                no_credentials.clone(),
            ),
            (
                "no data",
                Some(0),
                no_data.to_owned(),
                found.clone(),
                no_credentials.clone(),
            ),
            (
                "unknown windows",
                Some(0),
                envelope(unknown),
                found.clone(),
                failed.clone(),
            ),
            (
                "logged out",
                Some(1),
                auth.to_owned(),
                format!("{:?}", AccountProbe::LoggedOut),
                no_credentials.clone(),
            ),
            (
                "an API key",
                Some(2),
                api_key.to_owned(),
                api_key_account.clone(),
                no_credentials.clone(),
            ),
            (
                "an API key at another exit",
                Some(0),
                api_key.to_owned(),
                api_key_account,
                no_credentials.clone(),
            ),
            (
                "signed out on 1.5.14",
                Some(1),
                signed_out.to_owned(),
                format!("{:?}", AccountProbe::LoggedOut),
                no_credentials,
            ),
            (
                "another error at exit 2",
                Some(2),
                offline.to_owned(),
                unavailable(AccountProbeError::SourceUnavailable),
                failed.clone(),
            ),
            (
                "offline",
                Some(1),
                offline.to_owned(),
                unavailable(AccountProbeError::SourceUnavailable),
                failed.clone(),
            ),
            (
                "an error envelope at exit 0",
                Some(0),
                offline.to_owned(),
                unavailable(AccountProbeError::SourceUnavailable),
                failed.clone(),
            ),
            (
                "a numeric bucket id",
                Some(0),
                envelope(json!([{"id": 7, "window": "5h", "remaining_fraction": 0.5,
                    "reset_time": FIVE_HOUR_RESET}])),
                found.clone(),
                failed.clone(),
            ),
            (
                "a string fraction",
                Some(0),
                envelope(
                    json!([{"id": "gemini-5h", "window": "5h", "remaining_fraction": "0.5",
                    "reset_time": FIVE_HOUR_RESET}]),
                ),
                found.clone(),
                failed.clone(),
            ),
            (
                "exit without envelope",
                Some(2),
                String::new(),
                unavailable(AccountProbeError::ExitNonZero(Some(2))),
                failed.clone(),
            ),
            (
                "not the envelope",
                Some(0),
                "Signed in.".to_owned(),
                unavailable(AccountProbeError::UnparseableOutput),
                failed.clone(),
            ),
            (
                "another command",
                Some(0),
                credits.to_owned(),
                unavailable(AccountProbeError::UnparseableOutput),
                failed.clone(),
            ),
        ] {
            let read = usage::classify(code, stdout.as_bytes(), observed_at());
            assert_eq!((case, format!("{:?}", read.account(None))), (case, account));
            assert_eq!((case, read.usage()), (case, usage));
        }
        for cause in [AccountProbeError::TimedOut, AccountProbeError::SpawnFailed] {
            let read = Read::Failed(cause);
            assert_eq!(format!("{:?}", read.account(None)), unavailable(cause));
            assert_eq!(read.usage(), failed);
        }
    }
    #[test]
    fn the_credential_file_names_the_account_at_its_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let read = Read::Usage(Quota::Absent);
        let found = |probe: AccountProbe| match probe {
            AccountProbe::Found(account) => (account.account_id, account.credentials_updated_at_ms),
            other => panic!("expected an account, got {other:?}"),
        };
        assert_eq!(
            found(read.account(Some(dir.path()))),
            (None, None),
            "a keyring install"
        );
        let token = dir.path().join("antigravity-oauth-token");
        std::fs::write(
            &token,
            r#"{"access_token":"secret","id_token_email":" user@example.com "}"#,
        )
        .unwrap();
        assert_eq!(
            found(read.account(Some(dir.path()))),
            (
                Some("user@example.com".to_owned()),
                crate::agents::account::file_mtime_ms(&token)
            )
        );
        let api_key = usage::classify(
            Some(2),
            br#"{"status":"ERROR","error":"/usage failed: not available with a Gemini API key; see https://aistudio.google.com/usage for the key's usage"}"#,
            observed_at(),
        );
        assert_eq!(
            found(api_key.account(Some(dir.path()))),
            (None, None),
            "a key session is not the login the token names"
        );
        std::fs::write(&token, r#"{"id_token_email":""}"#).unwrap();
        assert_eq!(found(read.account(Some(dir.path()))), (None, None));
        assert!(matches!(
            Read::LoggedOut.account(Some(dir.path())),
            AccountProbe::LoggedOut
        ));
    }
    #[test]
    fn offline_reads_answer_without_starting_agy() {
        let dir = tempfile::tempdir().unwrap();
        if !crate::agents::credits::oauth_usage_offline() {
            script(dir.path(), "exit 0\n");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "agents::adapters::antigravity::tests::usage_probe::offline_reads_answer_without_starting_agy",
                    "--nocapture",
                ])
                .env("RIMZ_OAUTH_USAGE_OFFLINE", "1")
                .env("PATH", dir.path())
                .env("HOME", dir.path())
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let spawns = crate::proc::testkit::spawn_count();
        let read = usage::read(
            Some(dir.path().join("offline-home")),
            &BTreeMap::from([("HOME".to_owned(), "/login/home".to_owned())]),
        );
        assert_eq!(read, Read::Failed(AccountProbeError::SourceUnavailable));
        assert_eq!(crate::proc::testkit::spawn_count(), spawns, "no agy start");
    }
    #[test]
    fn a_missing_binary_could_not_start() {
        let dir = tempfile::tempdir().unwrap();
        for binary in [None, Some(dir.path().join("agy"))] {
            assert_eq!(
                usage::run(binary, None, Duration::from_secs(10)),
                Read::Failed(AccountProbeError::SpawnFailed)
            );
        }
    }
    #[test]
    fn the_child_gets_the_print_argv_a_newline_an_ssh_session_and_the_login_home() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path();
        let reset = (Timestamp::now() + jiff::SignedDuration::from_hours(1)).to_string();
        let answer = envelope(json!([
            {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.25, "reset_time": reset}
        ]));
        let agy = script(
            out,
            &format!(
                "printf '%s\\n' \"$@\" > '{d}/argv'\ncat > '{d}/stdin'\n\
                 printf '%s' \"$SSH_CONNECTION\" > '{d}/ssh'\nprintf '%s' \"$HOME\" > '{d}/home'\n\
                 printf '%s\\n' '{answer}'\n",
                d = out.display()
            ),
        );
        let read = usage::run(Some(agy), Some("/login/home"), Duration::from_secs(10));
        let Read::Usage(Quota::Windows(limits)) = read else {
            panic!("expected windows, got {read:?}")
        };
        assert_eq!(limits.windows[0].used_percentage, Some(75));
        let file = |name: &str| std::fs::read_to_string(out.join(name)).unwrap();
        assert_eq!(file("argv"), "-p\n/usage\n--output-format\njson\n");
        assert_eq!(file("stdin"), "\n");
        assert_eq!(file("ssh"), usage::SSH_SESSION_VALUE);
        assert_eq!(file("home"), "/login/home");
    }

    #[test]
    fn the_child_serves_no_rimz_tools_whatever_launch_it_runs_in() {
        let command = usage::command("agy".into(), None);
        let removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_owned())
            .collect();
        assert_eq!(removed, ["RIMZ_MCP_CWD", "RIMZ_MCP_SERVE"]);
    }
    /// A child left in the caller's session takes agy's sign-in prompt from the caller's
    /// terminal, not from stdin, and waits out the deadline.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_child_leads_its_own_session() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("session");
        let agy = script(
            dir.path(),
            &format!(
                "echo \"$$ $(cut -d' ' -f6 /proc/$$/stat)\" > '{}'\n",
                out.display()
            ),
        );
        usage::run(Some(agy), None, Duration::from_secs(10));
        let ids = std::fs::read_to_string(&out).unwrap();
        let (pid, session) = ids.trim().split_once(' ').unwrap();
        assert_eq!(session, pid, "pid and session id: {ids}");
    }
    #[test]
    fn the_deadline_kills_and_reaps_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let agy = script(
            dir.path(),
            &format!(
                "sleep 347.913 &\necho $! > '{}'\nwait\n",
                pid_file.display()
            ),
        );
        let started = Instant::now();
        let read = usage::run(Some(agy), None, Duration::from_millis(500));
        assert_eq!(read, Read::Failed(AccountProbeError::TimedOut));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let gone_by = Instant::now() + Duration::from_secs(5);
        while crate::proc::process_is_live(pid, None) && Instant::now() < gone_by {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !crate::proc::process_is_live(pid, None),
            "sleep {pid} survived the deadline"
        );
    }
    #[test]
    fn one_read_serves_a_provider_home_for_sixty_seconds() {
        let reads = Default::default();
        let start = Instant::now();
        let home = Some(PathBuf::from("/home/a/.gemini/antigravity-cli"));
        let mut runs = 0;
        let mut read = |key: &Option<PathBuf>, at: Duration, answer: Read| {
            usage::read_with(&reads, key.clone(), start + at, || {
                runs += 1;
                answer
            })
        };
        let failed = Read::Failed(AccountProbeError::TimedOut);
        assert_eq!(read(&home, Duration::ZERO, failed.clone()), failed);
        assert_eq!(
            read(&home, Duration::from_secs(59), Read::LoggedOut),
            failed
        );
        assert_eq!(
            read(&None, Duration::from_secs(59), Read::LoggedOut),
            Read::LoggedOut
        );
        assert_eq!(
            read(&home, Duration::from_secs(61), Read::LoggedOut),
            Read::LoggedOut
        );
        assert_eq!(runs, 3, "a second home and an expired read each spawn once");
    }
    #[test]
    fn only_gemini_buckets_count_and_third_party_ones_cannot_fail_the_read() {
        let limits = windows(json!([
            {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.8, "reset_time": FIVE_HOUR_RESET},
            {"id": "3p-5h", "window": "5h", "remaining_fraction": 0, "reset_time": "2026-06-15T11:00:00Z"},
            {"id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0.4, "reset_time": WEEK_RESET},
            {"id": "3p-weekly", "window": "weekly", "remaining_fraction": "malformed"}
        ]));
        let summary: Vec<_> = limits
            .windows
            .iter()
            .map(|window| {
                (
                    window.duration_mins,
                    window.used_percentage,
                    window.resets_at,
                )
            })
            .collect();
        assert_eq!(
            summary,
            [
                (Some(300), Some(20), Some(FIVE_HOUR_RESET.parse().unwrap())),
                (Some(10_080), Some(60), Some(WEEK_RESET.parse().unwrap())),
            ]
        );
        assert!(limits.windows.iter().all(|window| {
            window.source == WindowSource::Authoritative
                && !window.lifted
                && window.scope.is_none()
                && window.observed_at == Some(observed_at())
        }));
    }
    #[test]
    fn empty_and_disabled_buckets_keep_their_window_and_exhaustion_reads_full() {
        let limits = windows(json!([
            {"id": "gemini-5h", "window": "5h", "disabled": true, "remaining_fraction": 0.5},
            {"id": "3p-5h", "window": "5h"},
            {"id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0.001, "reset_time": WEEK_RESET}
        ]));
        let summary: Vec<_> = limits
            .windows
            .iter()
            .map(|window| (window.used_percentage, window.resets_at))
            .collect();
        assert_eq!(
            summary,
            [(None, None), (Some(99), Some(WEEK_RESET.parse().unwrap()))]
        );
        let exhausted = windows(json!([
            {"id": "gemini-weekly", "window": "weekly", "remaining_fraction": 0, "reset_time": WEEK_RESET}
        ]));
        assert_eq!(exhausted.windows.len(), 1);
        assert_eq!(exhausted.windows[0].used_percentage, Some(100));
    }
    #[test]
    fn bad_fractions_past_resets_ownerless_and_unknown_only_buckets_are_rejected() {
        for bucket in [
            json!({"id": "gemini-5h", "window": "5h", "remaining_fraction": -0.1, "reset_time": FIVE_HOUR_RESET}),
            json!({"id": "gemini-5h", "window": "5h", "remaining_fraction": 1.5, "reset_time": FIVE_HOUR_RESET}),
            json!({"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.5, "reset_time": "invalid"}),
            json!({"id": " ", "window": "5h", "remaining_fraction": 0.5, "reset_time": FIVE_HOUR_RESET}),
        ] {
            let buckets = json!([
                bucket,
                {"id": "3p-5h", "window": "5h", "remaining_fraction": 0.9, "reset_time": FIVE_HOUR_RESET}
            ]);
            assert_eq!(fold(buckets.clone()), Quota::Invalid, "{buckets}");
        }
        let unknown = json!([
            {"id": "legacy-model-quota", "remaining_fraction": 0.5, "reset_time": FIVE_HOUR_RESET},
            {"id": "gemini-month", "window": "monthly", "remaining_fraction": 0.5, "reset_time": FIVE_HOUR_RESET}
        ]);
        assert_eq!(fold(unknown), Quota::Invalid);
        // A bucket that has just reset drops alone; the read stays valid.
        let just_reset = json!([
            {"id": "gemini-5h", "window": "5h", "remaining_fraction": 0.5, "reset_time": OBSERVED_AT}
        ]);
        assert!(matches!(fold(just_reset), Quota::Windows(_)));
    }
}
fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}
fn json_value(source: &str) -> Value {
    serde_json::from_str(source).unwrap()
}
fn write_transcript(home: &Path, session_id: &str) -> std::path::PathBuf {
    write_transcript_named(
        home,
        session_id,
        "transcript_full.jsonl",
        include_str!("tests/fixtures/transcript_full.jsonl"),
    )
}
fn mounted_login(home: &Path, account: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("HOME".to_owned(), home.display().to_string()),
        (session::HOME_ENV.to_owned(), account.display().to_string()),
    ])
}

#[test]
fn a_named_login_resolves_its_directory_and_its_hook_path_onto_it() {
    let env = mounted_login(Path::new("/home/me"), Path::new("/accounts/work"));
    assert_eq!(
        AntigravityAdapter.config_home(&env),
        Some(PathBuf::from("/accounts/work"))
    );
    assert_eq!(
        AntigravityAdapter.mounted_home_target(&env),
        Some(PathBuf::from("/home/me/.gemini/antigravity-cli"))
    );
    let inside = json!({
        "transcriptPath": "/home/me/.gemini/antigravity-cli/brain/a/.system_generated/logs/t.jsonl"
    });
    assert_eq!(
        session::payload_on_host(&inside, &env)["transcriptPath"],
        "/accounts/work/brain/a/.system_generated/logs/t.jsonl"
    );
    let elsewhere = json!({"transcriptPath": "/home/me/.gemini/antigravity-cli-2/brain/a"});
    assert_eq!(*session::payload_on_host(&elsewhere, &env), elsewhere);
    let default = BTreeMap::from([("HOME".to_owned(), "/home/me".to_owned())]);
    assert_eq!(*session::payload_on_host(&inside, &default), inside);
    assert_eq!(session::mounted_view(&default), None);
}

#[test]
fn a_named_login_with_no_credential_file_of_its_own_is_foreign() {
    let dir = tempfile::tempdir().unwrap();
    let signed_in = usage::Read::Usage(usage::Quota::Absent);
    assert_eq!(
        usage::own_login(signed_in.clone(), dir.path()),
        usage::Read::ForeignLogin
    );
    assert_eq!(
        usage::own_login(usage::Read::ApiKey, dir.path()),
        usage::Read::ForeignLogin
    );
    assert_eq!(
        usage::own_login(usage::Read::LoggedOut, dir.path()),
        usage::Read::LoggedOut
    );
    std::fs::write(dir.path().join("antigravity-oauth-token"), "{}").unwrap();
    assert_eq!(usage::own_login(signed_in.clone(), dir.path()), signed_in);
}

#[test]
fn only_a_definite_logged_out_read_lets_a_credential_less_account_launch() {
    assert_eq!(usage::Read::LoggedOut.launch_refusal(), None);
    let foreign = usage::Read::ForeignLogin.launch_refusal().unwrap();
    assert!(
        foreign.starts_with("agy found a login outside"),
        "{foreign}"
    );
    let failed = usage::Read::Failed(crate::agents::account::AccountProbeError::SourceUnavailable)
        .launch_refusal()
        .unwrap();
    assert!(failed.starts_with("RimZ could not confirm"), "{failed}");
}

#[test]
fn a_named_logins_usage_read_mounts_its_directory_over_agys() {
    let command = usage::mounted_command(
        Path::new("/accounts/work"),
        Path::new("/home/me/.gemini/antigravity-cli"),
        PathBuf::from("/bin/agy"),
        Some("/home/me"),
    );
    assert_eq!(command.get_program(), "bwrap");
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_str().unwrap())
        .collect();
    assert_eq!(
        args[..10],
        [
            "--dev-bind",
            "/",
            "/",
            "--die-with-parent",
            "--new-session",
            "--bind",
            "/accounts/work",
            "/home/me/.gemini/antigravity-cli",
            "--",
            "/bin/agy",
        ]
    );
    assert_eq!(args[10], "-p");
}

#[test]
fn a_named_accounts_uninstall_leaves_the_plugin_every_account_runs_on() {
    let home = AgyHome::new();
    let managed = home.integration();
    managed.install(&env(), Default::default()).unwrap();
    let named = mounted_login(Path::new("/home/me"), Path::new("/accounts/work"));
    managed.uninstall(&named).unwrap();
    assert!(home.plugin.join("plugin.json").exists());
    managed.uninstall(&env()).unwrap();
    assert!(!home.plugin.exists());
}

#[test]
fn a_mounted_accounts_subagent_logs_resolve_from_agys_path_to_the_account() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let user_home = dir.path().join("user");
    let view = user_home.join(".gemini/antigravity-cli");
    let view_uri = url::Url::from_directory_path(&view).unwrap().to_string();
    let workspace_uri = url::Url::from_directory_path(&workspace)
        .unwrap()
        .to_string();
    let parent = write_transcript_named(
        dir.path(),
        SESSION_ID,
        "transcript_full.jsonl",
        &include_str!("tests/fixtures/subagents_two_children.jsonl")
            .replace("__HOME_URI__", view_uri.trim_end_matches('/'))
            .replace("__WORKSPACE__", &workspace.display().to_string())
            .replace("__WORKSPACE_URI__", workspace_uri.trim_end_matches('/')),
    );
    for child in [CHILD_ALPHA, CHILD_BETA] {
        write_transcript_named(
            dir.path(),
            child,
            "transcript.jsonl",
            include_str!("tests/fixtures/transcript_full.jsonl"),
        );
    }
    let login = mounted_login(&user_home, dir.path());
    assert_eq!(
        session::spawned_subagents(&parent, SESSION_ID, &workspace, &login).len(),
        2
    );
}

fn write_subagent_fixture(home: &Path, session_id: &str, fixture: &str) -> std::path::PathBuf {
    let home_uri = url::Url::from_directory_path(home).unwrap().to_string();
    let workspace = home.join("workspace");
    let workspace_uri = url::Url::from_directory_path(&workspace)
        .unwrap()
        .to_string();
    write_transcript_named(
        home,
        session_id,
        "transcript_full.jsonl",
        &fixture
            .replace("__HOME_URI__", home_uri.trim_end_matches('/'))
            .replace("__WORKSPACE__", &workspace.display().to_string())
            .replace("__WORKSPACE_URI__", workspace_uri.trim_end_matches('/')),
    )
}
fn rewrite_subagent_result(fixture: &str, rewrite: impl FnOnce(&str) -> String) -> String {
    let mut records = fixture
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    let result = records
        .iter_mut()
        .find(|record| record.get("type").and_then(Value::as_str) == Some("INVOKE_SUBAGENT"))
        .unwrap();
    let content = result.get("content").and_then(Value::as_str).unwrap();
    result["content"] = Value::String(rewrite(content));
    records
        .into_iter()
        .map(|record| record.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
fn state_after(home: &Path, workspace: &Path, ending: Vec<String>) -> LocalSessionState {
    let mut lines = vec![user_record(0, AT_09, "start")];
    lines.extend(ending);
    write_transcript_named(home, SESSION_ID, "transcript_full.jsonl", &lines.join("\n"));
    let observation = session::discover_under(home, workspace).remove(0);
    local_state(&observation).clone()
}
fn write_transcript_named(
    home: &Path,
    session_id: &str,
    basename: &str,
    contents: &str,
) -> std::path::PathBuf {
    let path = home
        .join("brain")
        .join(session_id)
        .join(".system_generated/logs")
        .join(basename);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, contents).unwrap();
    path
}
fn record(
    step: u64,
    source: &str,
    record_type: &str,
    status: &str,
    at: &str,
    content: &str,
) -> Value {
    json!({
        "step_index": step,
        "source": source,
        "type": record_type,
        "status": status,
        "created_at": at,
        "content": content,
    })
}
fn user_record(step: u64, at: &str, content: &str) -> String {
    record(step, "USER_EXPLICIT", "USER_INPUT", "DONE", at, content).to_string()
}
fn planner_record(step: u64, at: &str, tool_calls: Option<Value>) -> String {
    let mut value = record(
        step,
        "MODEL",
        "PLANNER_RESPONSE",
        "DONE",
        at,
        "planner response",
    );
    if let Some(tool_calls) = tool_calls {
        value["tool_calls"] = tool_calls;
    }
    value.to_string()
}
fn with<const N: usize>(base: &Value, fields: [(&str, Value); N]) -> Value {
    let mut value = base.clone();
    let object = value.as_object_mut().unwrap();
    for (key, field) in fields {
        object.insert(key.to_owned(), field);
    }
    value
}

mod folder_trust {
    use super::AntigravityAdapter;
    use crate::agents::capabilities::{InstallationCapability as _, LaunchCapability as _};
    use crate::agents::{FolderTrust, FolderTrustGap};
    use serde_json::{Value, json};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    /// A primary checkout with a subdirectory, a linked worktree and a pull-request worktree of
    /// it (`.git` metadata written by hand, as `git worktree add` and RimZ leave it), an
    /// empty-dir agent directory, and an agy home.
    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        env: BTreeMap<String, String>,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            std::fs::create_dir_all(root.join("repo/.git")).unwrap();
            std::fs::write(root.join("repo/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
            std::fs::create_dir_all(root.join("repo/sub")).unwrap();
            std::fs::create_dir_all(root.join("empty/room/agent")).unwrap();
            std::fs::create_dir_all(root.join("home")).unwrap();
            let fixture = Self {
                env: BTreeMap::from([("HOME".into(), root.join("home").display().to_string())]),
                _dir: dir,
                root,
            };
            fixture.link("wt", None);
            fixture.link("pr", Some(7));
            fixture
        }

        fn link(&self, name: &str, from_pr: Option<u64>) {
            let tree = self.path(&format!("worktrees/{name}"));
            let admin = self.repo().join(".git/worktrees").join(name);
            std::fs::create_dir_all(tree.join("sub")).unwrap();
            std::fs::create_dir_all(&admin).unwrap();
            std::fs::write(tree.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
            std::fs::write(admin.join("commondir"), "../..\n").unwrap();
            let marker = json!({
                "version": 2,
                "name": name,
                "branch": name,
                "from_pr": from_pr,
                "base_ref": "0123456789abcdef0123456789abcdef01234567",
                "repo_root": self.repo(),
                "worktree_path": tree,
                "created_at": "2026-06-10T00:00:00Z",
            });
            std::fs::write(admin.join("rimz-worktree.json"), marker.to_string()).unwrap();
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        fn repo(&self) -> PathBuf {
            self.path("repo")
        }

        fn settings(&self) -> PathBuf {
            self.path("home/.gemini/antigravity-cli/settings.json")
        }

        fn write_settings(&self, text: &str) {
            std::fs::create_dir_all(self.settings().parent().unwrap()).unwrap();
            std::fs::write(self.settings(), text).unwrap();
        }

        fn trust(&self, entries: &[PathBuf]) {
            self.write_settings(
                &json!({"theme": "dark", "trustedWorkspaces": entries}).to_string(),
            );
        }

        fn decide(&self, cwd: &Path, repo_root: Option<&Path>) -> FolderTrust {
            AntigravityAdapter
                .folder_trust(cwd, repo_root, &self.env)
                .expect("agy models folder trust")
        }

        fn gap(&self, cwd: &Path, repo_root: Option<&Path>) -> FolderTrustGap {
            match self.decide(cwd, repo_root) {
                FolderTrust::Undecided(gap) => gap,
                other => panic!("expected a gap at {}, got {other:?}", cwd.display()),
            }
        }

        /// The launch argv after the bridge, from `args`.
        fn bridge(&self, cwd: &Path, args: &[&str]) -> Vec<String> {
            let mut args = args.iter().map(|arg| (*arg).to_owned()).collect();
            let mut artifact = None;
            AntigravityAdapter
                .bridge_saved_settings_args(&self.env, (cwd, &self.root), &mut args, &mut artifact)
                .unwrap();
            assert!(artifact.is_none(), "an inline document stays inline");
            args
        }
    }

    fn candidate(gap: &FolderTrustGap) -> Value {
        let preview = gap.grant.as_ref().expect("a grantable gap");
        serde_json::from_str(&preview.candidate).unwrap()
    }

    fn settings_arg(args: &[String]) -> Value {
        assert_eq!(args.len(), 2, "{args:?}");
        assert_eq!(args[0], "--settings");
        serde_json::from_str(&args[1]).unwrap()
    }

    #[test]
    fn trust_keys_on_the_primary_checkout_and_inherits_below_it() {
        let f = Fixture::new();
        let (repo, wt, pr) = (f.repo(), f.path("worktrees/wt"), f.path("worktrees/pr"));
        let gap = f.gap(&repo, Some(&repo));
        assert_eq!(
            (gap.path.clone(), gap.key.clone()),
            (f.settings(), repo.clone())
        );
        assert_eq!(gap.grant.as_ref().unwrap().original, None);
        assert_eq!(candidate(&gap), json!({"trustedWorkspaces": [repo]}));
        assert_eq!(
            f.gap(&wt.join("sub"), None).key,
            repo,
            "a worktree keys on its primary"
        );
        assert_eq!(
            f.gap(&pr, Some(&repo)).key,
            pr,
            "a pull request keys on itself"
        );
        let agent = f.path("empty/room/agent");
        assert_eq!(f.gap(&agent, None).key, agent);

        f.trust(std::slice::from_ref(&repo));
        for cwd in [repo.clone(), repo.join("sub"), wt.clone(), wt.join("sub")] {
            assert_eq!(
                f.decide(&cwd, Some(&repo)),
                FolderTrust::Decided,
                "{}",
                cwd.display()
            );
            assert_eq!(
                f.decide(&cwd, None),
                FolderTrust::Decided,
                "{}",
                cwd.display()
            );
        }
        let gap = f.gap(&pr, Some(&repo));
        assert_eq!(
            candidate(&gap),
            json!({"theme": "dark", "trustedWorkspaces": [repo, pr]})
        );
        assert_eq!(f.gap(&pr.join("sub"), Some(&repo)).key, pr.join("sub"));

        f.trust(&[repo.clone(), pr.clone()]);
        assert_eq!(f.decide(&pr, Some(&repo)), FolderTrust::Decided);
        assert!(
            matches!(
                f.decide(&pr.join("sub"), Some(&repo)),
                FolderTrust::Undecided(_)
            ),
            "a pull-request worktree inherits nothing, its own root included"
        );

        f.trust(&[f.path("worktrees")]);
        assert_eq!(f.decide(&wt, None), FolderTrust::Decided);
        assert!(matches!(f.decide(&pr, None), FolderTrust::Undecided(_)));

        f.trust(&[f.path("empty")]);
        assert_eq!(f.decide(&f.path("empty"), None), FolderTrust::Decided);
        assert_eq!(f.decide(&agent, None), FolderTrust::Decided);
    }

    #[test]
    fn unreadable_settings_leave_a_gap_that_names_the_repair() {
        let f = Fixture::new();
        let repo = f.repo();
        for (text, reason) in [
            ("[", "invalid JSON"),
            ("[]", "expected a JSON object"),
            (
                r#"{"trustedWorkspaces":"x"}"#,
                "expected trustedWorkspaces to be an array",
            ),
            (
                r#"{"permissions":"x"}"#,
                "expected permissions to be an object",
            ),
            (
                r#"{"permissions":[]}"#,
                "expected permissions to be an object",
            ),
            (
                r#"{"permissions":{"allow":"x"}}"#,
                "expected permissions.allow to be an array",
            ),
            (
                r#"{"permissions":{"allow":{}}}"#,
                "expected permissions.allow to be an array",
            ),
        ] {
            f.write_settings(text);
            let gap = f.gap(&repo, Some(&repo));
            let error = gap.grant.unwrap_err();
            let prefix = format!("repair `{}`: ", f.settings().display());
            assert!(
                error.starts_with(&prefix) && error.contains(reason),
                "{text}: {error}"
            );
        }
        f.write_settings(&format!(
            "\u{feff}{{\n  // agy's own\n  \"trustedWorkspaces\": [{:?}],\n  \"permissions\": {{\"allow\": [\"mcp(rimz_tools/*)\", 42]}}\n}}",
            repo.display().to_string()
        ));
        assert_eq!(f.decide(&repo, Some(&repo)), FolderTrust::Decided);
        let gap = AntigravityAdapter.folder_trust(&repo, Some(&repo), &BTreeMap::new());
        let Some(FolderTrust::Undecided(gap)) = gap else {
            panic!("expected a gap without HOME, got {gap:?}");
        };
        assert!(gap.grant.unwrap_err().contains("$HOME is not set"));
    }

    #[test]
    fn a_named_account_reads_and_grants_its_own_settings() {
        let mut f = Fixture::new();
        let repo = f.repo();
        f.trust(std::slice::from_ref(&repo));
        let account = f.path("accounts/work");
        std::fs::create_dir_all(&account).unwrap();
        f.env.insert(
            "RIMZ_ANTIGRAVITY_HOME".into(),
            account.display().to_string(),
        );
        let gap = f.gap(&repo, Some(&repo));
        assert_eq!(gap.path, account.join("settings.json"));
        crate::agents::grant_folder_trust(&gap).unwrap();
        assert_eq!(f.decide(&repo, Some(&repo)), FolderTrust::Decided);
    }

    #[test]
    fn a_grant_appends_its_key_once_and_keeps_the_other_keys() {
        let f = Fixture::new();
        let (repo, other) = (f.repo(), f.path("other"));
        f.trust(std::slice::from_ref(&other));
        crate::agents::grant_folder_trust(&f.gap(&f.path("worktrees/wt"), None)).unwrap();
        assert_eq!(
            f.decide(&f.path("worktrees/wt"), None),
            FolderTrust::Decided
        );
        let saved: Value =
            serde_json::from_str(&std::fs::read_to_string(f.settings()).unwrap()).unwrap();
        assert_eq!(
            saved,
            json!({"theme": "dark", "trustedWorkspaces": [other, repo]})
        );
    }

    /// agy trusts only an exact `trustedWorkspaces` entry (agy 1.5.16), so a launch RimZ judges
    /// trusted by inheritance carries the saved list and its own directory in the one settings
    /// document, and every other launch leaves argv alone.
    #[test]
    fn launch_bridges_inherited_trust_into_the_one_settings_document() {
        let f = Fixture::new();
        let (repo, wt, pr) = (f.repo(), f.path("worktrees/wt"), f.path("worktrees/pr"));
        f.trust(std::slice::from_ref(&repo));
        assert_eq!(
            settings_arg(&f.bridge(&wt, &[])),
            json!({"trustedWorkspaces": [repo, wt]})
        );
        assert_eq!(
            settings_arg(&f.bridge(&repo.join("sub"), &[])),
            json!({"trustedWorkspaces": [repo, repo.join("sub")]})
        );
        let user = r#"{"agentMode":"","queuedMessages":"queue","trustedWorkspaces":["/user"]}"#;
        assert_eq!(
            settings_arg(&f.bridge(&wt.join("sub"), &["--settings", user])),
            json!({
                "agentMode": "",
                "queuedMessages": "queue",
                "trustedWorkspaces": ["/user", repo, wt.join("sub")],
            })
        );
        assert!(
            f.bridge(&repo, &[]).is_empty(),
            "an exact entry needs no bridge"
        );
        assert!(
            f.bridge(&pr, &[]).is_empty(),
            "a pull-request worktree is never bridged"
        );
        assert_eq!(f.bridge(&pr, &["--settings", user]), ["--settings", user]);

        f.trust(&[]);
        assert!(
            f.bridge(&wt, &[]).is_empty(),
            "an untrusted primary bridges nothing"
        );

        f.trust(&[f.path("empty")]);
        let agent = f.path("empty/room/agent");
        assert_eq!(
            settings_arg(&f.bridge(&agent, &[])),
            json!({"trustedWorkspaces": [f.path("empty"), agent]})
        );

        f.write_settings("[");
        assert!(
            f.bridge(&wt, &[]).is_empty(),
            "agy asks for itself past a broken file"
        );
    }

    /// Which spelling of a symlinked directory agy compares is unobserved, so an entry for only
    /// one spelling still bridges both.
    #[test]
    fn a_symlinked_launch_bridges_both_spellings() {
        let f = Fixture::new();
        let (repo, link) = (f.repo(), f.path("link"));
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        for saved in [&repo, &link] {
            f.trust(std::slice::from_ref(saved));
            assert_eq!(f.decide(&link, None), FolderTrust::Decided);
            let mut expected = vec![saved.clone()];
            expected.extend(
                [link.clone(), repo.clone()]
                    .into_iter()
                    .filter(|dir| dir != saved),
            );
            assert_eq!(
                settings_arg(&f.bridge(&link, &[])),
                json!({"trustedWorkspaces": expected})
            );
        }
        f.trust(&[repo.clone(), link.clone()]);
        assert!(f.bridge(&link, &[]).is_empty(), "both spellings are saved");
    }

    /// A worktree whose RimZ marker cannot be read might be a pull request's, so it takes trust
    /// only from an entry naming it.
    #[test]
    fn an_unreadable_marker_reads_as_a_pull_request() {
        let f = Fixture::new();
        let (repo, wt) = (f.repo(), f.path("worktrees/wt"));
        std::fs::write(
            repo.join(".git/worktrees/wt/rimz-worktree.json"),
            "invalid marker",
        )
        .unwrap();
        f.trust(std::slice::from_ref(&repo));
        assert_eq!(f.gap(&wt, Some(&repo)).key, wt);
        assert!(f.bridge(&wt, &[]).is_empty());
    }

    /// Saved `permissions.allow` rules bridge into the launch's one settings document
    /// unconditionally, dropping non-string entries, keeping any profile rules first without
    /// duplicates, reading a named account's own file, and skipping silently on a missing or
    /// malformed file without modifying the saved settings on disk.
    #[test]
    fn saved_allow_rules_bridge_into_the_one_settings_document() {
        let mut f = Fixture::new();
        let (repo, wt, pr) = (f.repo(), f.path("worktrees/wt"), f.path("worktrees/pr"));

        assert!(
            f.bridge(&repo, &[]).is_empty(),
            "a missing saved file adds nothing and fails nothing"
        );

        let saved_text = json!({
            "trustedWorkspaces": [repo],
            "permissions": {
                "allow": ["mcp(rimz_tools/*)", 123, null, "command(cargo test)"]
            }
        })
        .to_string();
        f.write_settings(&saved_text);

        // Exact-trusted repo and untrusted PR worktree both carry saved allow rules without
        // bridging trust.
        assert_eq!(
            settings_arg(&f.bridge(&repo, &[])),
            json!({"permissions": {"allow": ["mcp(rimz_tools/*)", "command(cargo test)"]}})
        );
        assert_eq!(
            settings_arg(&f.bridge(&pr, &[])),
            json!({"permissions": {"allow": ["mcp(rimz_tools/*)", "command(cargo test)"]}})
        );

        // Inherited worktree carries both trustedWorkspaces and permissions.allow in one document.
        assert_eq!(
            settings_arg(&f.bridge(&wt, &[])),
            json!({
                "trustedWorkspaces": [repo, wt],
                "permissions": {"allow": ["mcp(rimz_tools/*)", "command(cargo test)"]}
            })
        );

        // Existing profile settings keep their own allow rules first, deduplicate, and keep deny.
        let profile = r#"{"queuedMessages":"queue","permissions":{"allow":["command(cargo test)","read_file(docs)"],"deny":["command(rm)"]}}"#;
        assert_eq!(
            settings_arg(&f.bridge(&repo, &["--settings", profile])),
            json!({
                "queuedMessages": "queue",
                "permissions": {
                    "allow": ["command(cargo test)", "read_file(docs)", "mcp(rimz_tools/*)"],
                    "deny": ["command(rm)"]
                }
            })
        );
        assert_eq!(
            std::fs::read_to_string(f.settings()).unwrap(),
            saved_text,
            "bridge must never modify the saved settings file"
        );

        // A wrong-shaped saved file skips the bridge without failing compile.
        for bad in [
            "[",
            r#"{"permissions":"x"}"#,
            r#"{"permissions":[]}"#,
            r#"{"permissions":{"allow":"x"}}"#,
            r#"{"permissions":{"allow":{}}}"#,
        ] {
            f.write_settings(bad);
            assert!(
                f.bridge(&wt, &[]).is_empty(),
                "malformed saved file must skip bridge without error: {bad}"
            );
        }

        // A named account reads saved allow rules from its own account directory.
        let account = f.path("accounts/work");
        std::fs::create_dir_all(&account).unwrap();
        std::fs::write(
            account.join("settings.json"),
            json!({"permissions": {"allow": ["command(just)"]}}).to_string(),
        )
        .unwrap();
        f.env.insert(
            "RIMZ_ANTIGRAVITY_HOME".into(),
            account.display().to_string(),
        );
        assert_eq!(
            settings_arg(&f.bridge(&repo, &[])),
            json!({"permissions": {"allow": ["command(just)"]}})
        );
    }
}

/// Room-root and notes-folder write rules resolve symlinks, fall back to the path as written when
/// a root does not exist, apply identically at every isolation level, and union cleanly into the
/// launch's one settings document (inline or private file artifact).
#[cfg(unix)]
#[test]
fn room_roots_and_notes_write_rules_resolve_symlinks_and_keep_missing_paths() {
    use crate::config::Isolation;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let real_state = root.join("real-state");
    let link_state = root.join("link-state");
    let real_runtime = root.join("real-runtime");
    let missing_shared = root.join("missing-shared");
    let real_notes = root.join("real-notes");
    let link_notes = root.join("link-notes");
    let missing_notes = root.join("missing-notes");
    std::fs::create_dir_all(&real_state).unwrap();
    std::fs::create_dir_all(&real_runtime).unwrap();
    std::fs::create_dir_all(&real_notes).unwrap();
    std::os::unix::fs::symlink(&real_state, &link_state).unwrap();
    std::os::unix::fs::symlink(&real_notes, &link_notes).unwrap();

    let roots = [link_state, real_runtime.clone(), missing_shared.clone()];
    let expected_root_rules = vec![
        format!("write_file({})", real_state.display()),
        format!("write_file({})", real_runtime.display()),
        format!("write_file({})", missing_shared.display()),
    ];

    let mut actual_by_isolation = Vec::new();
    for isolation in [Isolation::Host, Isolation::Scoped, Isolation::Sandbox] {
        let mut args = vec![
            "--settings".to_owned(),
            json!({
                "queuedMessages": "queue",
                "permissions": {
                    "allow": ["command(git status)", format!("write_file({})", real_state.display())],
                    "deny": ["command(rm)"]
                }
            })
            .to_string(),
        ];
        let mut artifact = None;
        AntigravityAdapter
            .allow_team_notes_args(&link_notes, (&root, &root), &mut args, &mut artifact)
            .unwrap();
        AntigravityAdapter
            .allow_team_notes_args(&missing_notes, (&root, &root), &mut args, &mut artifact)
            .unwrap();
        AntigravityAdapter
            .configure_sandbox_settings((&root, &root), &roots, isolation, &mut args, &mut artifact)
            .unwrap();
        assert!(artifact.is_none());
        let settings_flags: Vec<_> = args
            .windows(2)
            .filter(|pair| pair[0] == "--settings")
            .map(|pair| serde_json::from_str::<Value>(&pair[1]).unwrap())
            .collect();
        assert_eq!(settings_flags.len(), 1, "{isolation:?}: {args:?}");
        assert!(!args.iter().any(|arg| arg == "--add-dir"), "{args:?}");
        actual_by_isolation.push((isolation, settings_flags.into_iter().next().unwrap()));
    }

    let expected_allow = vec![
        "command(git status)".to_owned(),
        format!("write_file({})", real_state.display()),
        format!("write_file({})", real_notes.display()),
        format!("write_file({})", missing_notes.display()),
        expected_root_rules[1].clone(),
        expected_root_rules[2].clone(),
    ];
    let expected_doc = json!({
        "queuedMessages": "queue",
        "permissions": {
            "allow": expected_allow,
            "deny": ["command(rm)"]
        }
    });
    let expected_by_isolation: Vec<_> = [Isolation::Host, Isolation::Scoped, Isolation::Sandbox]
        .into_iter()
        .map(|isolation| (isolation, expected_doc.clone()))
        .collect();
    assert_eq!(actual_by_isolation, expected_by_isolation);

    // A file-backed --settings source merges room rules into a private artifact.
    std::fs::write(
        root.join("profile.json"),
        r#"{"permissions":{"deny":["command(rm)"]}}"#,
    )
    .unwrap();
    let mut args = vec!["--settings".to_owned(), "profile.json".to_owned()];
    let mut artifact = None;
    AntigravityAdapter
        .configure_sandbox_settings(
            (&root, &root),
            &roots,
            Isolation::Host,
            &mut args,
            &mut artifact,
        )
        .unwrap();
    let (_, value, source) = artifact.expect("file source stays a private artifact");
    assert_eq!(source, root.join("profile.json"));
    assert_eq!(
        value,
        json!({
            "permissions": {
                "deny": ["command(rm)"],
                "allow": expected_root_rules
            }
        })
    );
}
