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
        let decoded =
            decode_lifecycle_fields("PreInvocation", &payload, |_, _| Some(raw.to_owned()));
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
    for input in ["/compact", "  /compact\n"] {
        let opened = observe_lifecycle_with_prompt_reader(
            "PreInvocation",
            &with(
                &common,
                [("invocationNum", json!(0)), ("lastUserInput", json!(input))],
            ),
            |_, _| panic!("a compaction reads no prompt"),
        )
        .unwrap();
        assert_eq!(opened.signal, LifecycleSignal::Compacting, "{input:?}");
        assert!(opened.prompt.is_none());
        for (stop, expected) in [
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
            ),
            (
                with(
                    &common,
                    [
                        ("lastUserInput", json!(input)),
                        ("terminationReason", json!("TERMINAL_CUSTOM_HOOK")),
                    ],
                ),
                closed(false),
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
                closed(true),
            ),
        ] {
            let decoded = AntigravityAdapter.decode_hook("Stop", &stop).unwrap();
            assert_eq!(
                decoded.lifecycle().map(|observation| &observation.signal),
                Some(&expected),
                "{stop}"
            );
            assert_eq!(decoded.final_message(), None, "{stop}");
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
        let started = observe_lifecycle_with_prompt_reader("PreInvocation", &pre, |_, _| {
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
        Some("{\"plugins\":{\"rimz\":{\"enabled\":\"false\"}}}"),
    ] {
        if let Some(config) = config {
            std::fs::write(&home.config, config).unwrap();
        }
        assert!(managed.hooks_disabled(&env()).is_none(), "{config:?}");
    }
    std::fs::write(
        &home.config,
        "{\n  // agy writes this file\n  \"plugins\": {\"rimz\": {\"enabled\": false}},\n}\n",
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
    assert_eq!(
        context.model_id.as_deref(),
        Some("Gemini 3.5 Flash (Medium)")
    );
    assert_eq!(context.model_display_name.as_deref(), Some(FLASH));
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
fn statusline_normalizes_reasoning_qualifiers_without_changing_model_id() {
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
                &json!({"conversation_id": SESSION_ID, "model": {"id": "  provider/model:id  ", "display_name": display}}),
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
            (Some("  provider/model:id  "), Some(model), effort, thinking)
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
            "context_window": {"current_usage": {"input_tokens": 10, "output_tokens": 20}}
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
    for calls in [
        r#""not-an-array""#,
        r#"[{"name":"ask_question","args":{"questions":"not-json"}}]"#,
        r#"[{"name":"ask_question","args":{"questions":[]}}]"#,
        r#"[{"name":"another_tool","args":{"questions":[{"question":"ignored"}]}}]"#,
    ] {
        let ending = vec![planner_record(1, AT_10, Some(json_value(calls)))];
        let state = state_after(dir.path(), workspace, ending);
        assert_eq!(state.status, AgentStatus::Success);
        assert_eq!(state.phase, TurnPhase::Idle);
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
    let started = observe_lifecycle_with_prompt_reader("PreInvocation", &payload, |path, id| {
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
        |_, _| panic!("Stop must not read transcripts"),
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
        |path, id| session::latest_prompt_under(dir.path(), path, id),
    )
    .unwrap();
    assert!(rejected.prompt.is_none());
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

    std::fs::write(root.path().join("list.json"), "[1]").unwrap();
    for (source, reason) in [
        ("{oops", "invalid JSON"),
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
        (PermissionMode::Auto, &["--mode", "accept-edits"][..]),
        (PermissionMode::Plan, &["--mode", "plan"][..]),
        (
            PermissionMode::Yolo,
            &["--dangerously-skip-permissions", "--mode", "accept-edits"][..],
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
                no_credentials,
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
        let read = usage::read(Some(dir.path().join("offline-home")), Some("/login/home"));
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
