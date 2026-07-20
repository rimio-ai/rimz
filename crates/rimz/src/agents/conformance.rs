//! Registry-wide adapter conformance tests.
//!
//! The adapter registry is the production dispatch surface; these tests use the
//! same table so a new adapter inherits the classification/render contracts
//! without adding a second hand-maintained per-agent switch.

use std::fs;
use std::path::{Path, PathBuf};

use super::lifecycle::{LifecycleSignal, LifecycleSignalKind, LifecycleState, TurnPhase, step};
use super::registry::BUILTINS;
use super::{
    AdapterConformance, AgentDefinition, AgentHookClass, AskReply, ClassificationSample,
    ClassifiedHook, ConcernCoverage, DerivedAskFixture, HookCoverage, IntegrationConcern,
    LaunchPreset, PresetArgMatcher, PresetField, PriceBook, SpendFixture, SpendFixtureBody,
};
use crate::agents::AgentStatus;
use crate::agents::AskKind;
use crate::transcript::{AskOption, AskQuestion};

#[test]
fn rendered_preset_flags_have_matching_argv_declarations() {
    let fields = [
        (PresetField::Model, "model"),
        (PresetField::Effort, "high"),
        (PresetField::SystemPromptFile, "/tmp/system.md"),
        (PresetField::AppendSystemPromptFile, "/tmp/append-system.md"),
    ];
    for adapter in BUILTINS {
        let definition = adapter.spec();
        for (field, value) in fields {
            let preset = field.launch_preset(value.to_owned());
            match definition.render_preset(&preset) {
                Ok(argv) => {
                    let matcher =
                        definition
                            .launch
                            .preset_arg_matcher(field)
                            .unwrap_or_else(|| {
                                panic!(
                                    "{} renders {field:?} without declaring its argv matcher",
                                    definition.kind
                                )
                            });
                    assert!(
                        matcher
                            .occurrences(&argv)
                            .iter()
                            .any(|occurrence| occurrence.argv_range == (0..argv.len())),
                        "{} {field:?} matcher {matcher:?} does not consume {argv:?}",
                        definition.kind
                    );
                }
                Err(_) => assert!(
                    definition.launch.preset_arg_matcher(field).is_none(),
                    "{} declares a matcher for unsupported {field:?}",
                    definition.kind
                ),
            }
        }
    }
}

#[test]
fn preset_matchers_find_split_joined_and_config_key_occurrences() {
    let flags = PresetArgMatcher::Flag(vec!["--model".to_owned(), "-m".to_owned()]);
    let argv = strings(&[
        "--debug",
        "--model",
        "split",
        "-m",
        "short",
        "-m=short-joined",
        "--model=joined",
        "--model",
    ]);
    let occurrences = flags.occurrences(&argv);
    assert_eq!(
        occurrences
            .iter()
            .map(|occurrence| (occurrence.argv_range.clone(), occurrence.value.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (1..3, "split"),
            (3..5, "short"),
            (5..6, "short-joined"),
            (6..7, "joined"),
        ]
    );

    let config = PresetArgMatcher::ConfigKey {
        flags: vec!["-c".to_owned(), "--config".to_owned()],
        key: "model_reasoning_effort".to_owned(),
    };
    let argv = strings(&[
        "-c",
        "web_search=cached",
        "-c",
        "model_reasoning_effort=high",
        "--config=model_reasoning_effort=low",
        "--config",
    ]);
    let occurrences = config.occurrences(&argv);
    assert_eq!(
        occurrences
            .iter()
            .map(|occurrence| (occurrence.argv_range.clone(), occurrence.value.as_str()))
            .collect::<Vec<_>>(),
        vec![(2..4, "high"), (4..5, "low")]
    );
}

#[test]
fn render_preset_characterization() {
    let root = tempfile::TempDir::new().expect("plugin root");
    let plugin_dir = root.path().join("fixturebot");
    fs::create_dir(&plugin_dir).expect("plugin dir");
    fs::write(plugin_dir.join("README.md"), "hook setup").expect("setup doc");
    fs::write(
        plugin_dir.join("agent.toml"),
        r#"protocol = 1
kind = "fixturebot"
display-name = "Fixture Bot"
process-names = ["fixturebot"]
emits = ["session_start", "turn_start", "turn_end"]
setup-doc = "README.md"

[launch]
bin = "fixturebot"
model-flag = "--model"
effort-flag = "--effort"
"#,
    )
    .expect("manifest");
    let loaded = super::plugins::load_from_root(root.path());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);

    let mut cases = Vec::new();
    for adapter in BUILTINS
        .iter()
        .copied()
        .chain(loaded.definitions.iter().copied())
    {
        for (field, value) in [
            (PresetField::Model, "model-x"),
            (PresetField::Effort, "high"),
            (PresetField::SystemPromptFile, "/tmp/system.md"),
            (PresetField::AppendSystemPromptFile, "/tmp/append-system.md"),
        ] {
            cases.push(format!(
                "{}.{field:?}={:?}",
                adapter.spec().kind,
                adapter
                    .spec()
                    .render_preset(&field.launch_preset(value.to_owned()))
            ));
        }
        for field in [PresetField::Model, PresetField::Effort] {
            cases.push(format!(
                "{}.empty-{field:?}={:?}",
                adapter.spec().kind,
                adapter
                    .spec()
                    .render_preset(&field.launch_preset(String::new()))
            ));
        }
        // Every field at once pins flag order and which field an adapter
        // rejects first — the composition the per-field cases cannot show.
        cases.push(format!(
            "{}.all={:?}",
            adapter.spec().kind,
            adapter.spec().render_preset(&LaunchPreset {
                model: Some("model-x".to_owned()),
                effort: Some("high".to_owned()),
                system_prompt_file: Some(PathBuf::from("/tmp/system.md")),
                append_system_prompt_file: Some(PathBuf::from("/tmp/append-system.md")),
            })
        ));
    }
    insta::assert_snapshot!(cases.join("\n"), @r###"
    claude.Model=Ok(["--model", "model-x"])
    claude.Effort=Ok(["--effort", "high"])
    claude.SystemPromptFile=Ok(["--system-prompt-file", "/tmp/system.md"])
    claude.AppendSystemPromptFile=Ok(["--append-system-prompt-file", "/tmp/append-system.md"])
    claude.empty-Model=Ok([])
    claude.empty-Effort=Ok([])
    claude.all=Ok(["--model", "model-x", "--effort", "high", "--system-prompt-file", "/tmp/system.md", "--append-system-prompt-file", "/tmp/append-system.md"])
    codex.Model=Ok(["--model", "model-x"])
    codex.Effort=Ok(["-c", "model_reasoning_effort=high"])
    codex.SystemPromptFile=Ok(["-c", "model_instructions_file=/tmp/system.md"])
    codex.AppendSystemPromptFile=Err(UnsupportedField { agent: "codex", field: "append-system-prompt-file" })
    codex.empty-Model=Ok([])
    codex.empty-Effort=Ok([])
    codex.all=Err(UnsupportedField { agent: "codex", field: "append-system-prompt-file" })
    amp.Model=Ok(["--mode", "model-x"])
    amp.Effort=Ok(["--effort", "high"])
    amp.SystemPromptFile=Err(UnsupportedField { agent: "amp", field: "system-prompt-file" })
    amp.AppendSystemPromptFile=Err(UnsupportedField { agent: "amp", field: "append-system-prompt-file" })
    amp.empty-Model=Ok([])
    amp.empty-Effort=Ok([])
    amp.all=Err(UnsupportedField { agent: "amp", field: "system-prompt-file" })
    copilot.Model=Ok(["--model", "model-x"])
    copilot.Effort=Ok(["--effort", "high"])
    copilot.SystemPromptFile=Err(UnsupportedField { agent: "copilot", field: "system-prompt-file" })
    copilot.AppendSystemPromptFile=Err(UnsupportedField { agent: "copilot", field: "append-system-prompt-file" })
    copilot.empty-Model=Ok([])
    copilot.empty-Effort=Ok([])
    copilot.all=Err(UnsupportedField { agent: "copilot", field: "system-prompt-file" })
    kimi.Model=Ok(["--model", "model-x"])
    kimi.Effort=Err(UnsupportedField { agent: "kimi", field: "effort" })
    kimi.SystemPromptFile=Err(UnsupportedField { agent: "kimi", field: "system-prompt-file" })
    kimi.AppendSystemPromptFile=Err(UnsupportedField { agent: "kimi", field: "append-system-prompt-file" })
    kimi.empty-Model=Ok([])
    kimi.empty-Effort=Ok([])
    kimi.all=Err(UnsupportedField { agent: "kimi", field: "effort" })
    pi.Model=Ok(["--model", "model-x"])
    pi.Effort=Ok(["--thinking", "high"])
    pi.SystemPromptFile=Err(UnsupportedField { agent: "pi", field: "system-prompt-file" })
    pi.AppendSystemPromptFile=Err(UnsupportedField { agent: "pi", field: "append-system-prompt-file" })
    pi.empty-Model=Ok([])
    pi.empty-Effort=Ok([])
    pi.all=Err(UnsupportedField { agent: "pi", field: "system-prompt-file" })
    opencode.Model=Ok(["--model", "model-x"])
    opencode.Effort=Err(UnsupportedField { agent: "opencode", field: "effort" })
    opencode.SystemPromptFile=Err(UnsupportedField { agent: "opencode", field: "system-prompt-file" })
    opencode.AppendSystemPromptFile=Err(UnsupportedField { agent: "opencode", field: "append-system-prompt-file" })
    opencode.empty-Model=Ok([])
    opencode.empty-Effort=Ok([])
    opencode.all=Err(UnsupportedField { agent: "opencode", field: "effort" })
    antigravity.Model=Ok(["--model", "model-x"])
    antigravity.Effort=Err(UnsupportedField { agent: "antigravity", field: "effort" })
    antigravity.SystemPromptFile=Err(UnsupportedField { agent: "antigravity", field: "system-prompt-file" })
    antigravity.AppendSystemPromptFile=Err(UnsupportedField { agent: "antigravity", field: "append-system-prompt-file" })
    antigravity.empty-Model=Ok([])
    antigravity.empty-Effort=Ok([])
    antigravity.all=Err(UnsupportedField { agent: "antigravity", field: "effort" })
    cursor.Model=Ok(["--model", "model-x"])
    cursor.Effort=Err(UnsupportedField { agent: "cursor", field: "effort" })
    cursor.SystemPromptFile=Err(UnsupportedField { agent: "cursor", field: "system-prompt-file" })
    cursor.AppendSystemPromptFile=Err(UnsupportedField { agent: "cursor", field: "append-system-prompt-file" })
    cursor.empty-Model=Ok([])
    cursor.empty-Effort=Ok([])
    cursor.all=Err(UnsupportedField { agent: "cursor", field: "effort" })
    droid.Model=Err(UnsupportedField { agent: "droid", field: "model" })
    droid.Effort=Err(UnsupportedField { agent: "droid", field: "effort" })
    droid.SystemPromptFile=Err(UnsupportedField { agent: "droid", field: "system-prompt-file" })
    droid.AppendSystemPromptFile=Ok(["--append-system-prompt-file", "/tmp/append-system.md"])
    droid.empty-Model=Ok([])
    droid.empty-Effort=Ok([])
    droid.all=Err(UnsupportedField { agent: "droid", field: "model" })
    kiro.Model=Ok(["--model", "model-x"])
    kiro.Effort=Ok(["--effort", "high"])
    kiro.SystemPromptFile=Err(UnsupportedField { agent: "kiro", field: "system-prompt-file" })
    kiro.AppendSystemPromptFile=Err(UnsupportedField { agent: "kiro", field: "append-system-prompt-file" })
    kiro.empty-Model=Ok([])
    kiro.empty-Effort=Ok([])
    kiro.all=Err(UnsupportedField { agent: "kiro", field: "system-prompt-file" })
    qwen.Model=Ok(["--model", "model-x"])
    qwen.Effort=Err(UnsupportedField { agent: "qwen", field: "effort" })
    qwen.SystemPromptFile=Err(UnsupportedField { agent: "qwen", field: "system-prompt-file" })
    qwen.AppendSystemPromptFile=Err(UnsupportedField { agent: "qwen", field: "append-system-prompt-file" })
    qwen.empty-Model=Ok([])
    qwen.empty-Effort=Ok([])
    qwen.all=Err(UnsupportedField { agent: "qwen", field: "effort" })
    grok.Model=Ok(["--model", "model-x"])
    grok.Effort=Ok(["--reasoning-effort", "high"])
    grok.SystemPromptFile=Err(UnsupportedField { agent: "grok", field: "system-prompt-file" })
    grok.AppendSystemPromptFile=Err(UnsupportedField { agent: "grok", field: "append-system-prompt-file" })
    grok.empty-Model=Ok([])
    grok.empty-Effort=Ok([])
    grok.all=Err(UnsupportedField { agent: "grok", field: "system-prompt-file" })
    fixturebot.Model=Ok(["--model", "model-x"])
    fixturebot.Effort=Ok(["--effort", "high"])
    fixturebot.SystemPromptFile=Err(UnsupportedField { agent: "fixturebot", field: "system-prompt-file" })
    fixturebot.AppendSystemPromptFile=Err(UnsupportedField { agent: "fixturebot", field: "append-system-prompt-file" })
    fixturebot.empty-Model=Ok([])
    fixturebot.empty-Effort=Ok([])
    fixturebot.all=Err(UnsupportedField { agent: "fixturebot", field: "system-prompt-file" })
    "###);
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn classify_matches_corpus() {
    for adapter in BUILTINS {
        let kind = adapter.spec().kind;
        let conformance = adapter.conformance();
        for sample in conformance.classification {
            let decoded = adapter
                .decode_hook(sample.event_name, &sample.payload)
                .expect("corpus payload decodes");
            let actual = ClassifiedHook {
                class: decoded.class(),
                ask_kind: decoded.ask_kind(),
                event_name: decoded.event_name().to_owned(),
            };
            assert_eq!(
                actual, sample.expected,
                "{kind} classification sample {}",
                sample.event_name
            );
        }
    }
}

#[test]
fn classification_corpus_samples_decode_to_declared_channels() {
    for adapter in BUILTINS {
        let kind = adapter.spec().kind;
        let conformance = adapter.conformance();
        for sample in conformance.classification {
            assert_ne!(
                sample.expected.class,
                AgentHookClass::Unknown,
                "{kind} corpus sample {} must expect a real channel",
                sample.event_name
            );
            assert_ne!(
                adapter
                    .decode_hook(sample.event_name, &sample.payload)
                    .expect("corpus payload decodes")
                    .class(),
                AgentHookClass::Unknown,
                "{kind} native sample {} classified as unknown",
                sample.event_name
            );
        }
    }
}

#[test]
fn capability_honesty() {
    for adapter in BUILTINS {
        let kind = adapter.spec().kind;
        let capabilities = adapter.spec().capabilities;
        let conformance = adapter.conformance();
        let samples = &conformance.classification;
        let ask_kinds = producible_ask_kinds(samples);
        let has_blocking = samples
            .iter()
            .any(|sample| sample.expected.class == AgentHookClass::AwaitingUser);

        assert_eq!(
            has_blocking,
            [
                IntegrationConcern::Permission,
                IntegrationConcern::PlanApproval,
                IntegrationConcern::UserQuestion,
            ]
            .into_iter()
            .any(|concern| adapter.spec().concern_coverage(concern).is_wired()),
            "{kind} blocking classification must match ask coverage"
        );

        // Subagent honesty (capability ⟹ an observed subagent lifecycle sample) is
        // enforced by `coverage_is_complete_and_honest`'s Subagents arm.

        if !capabilities.native_ask_ui {
            assert!(
                !ask_kinds
                    .iter()
                    .any(|kind| matches!(kind, AskKind::PlanApproval | AskKind::Question)),
                "{kind} declares no native ask UI but classifies native ask kinds"
            );
        }

        if capabilities.realtime_usage.windows_defer_to_fresh_realtime {
            assert!(
                adapter
                    .spec()
                    .concern_coverage(IntegrationConcern::RichContext)
                    .is_wired(),
                "{kind} realtime account-usage channel requires wired RichContext coverage"
            );
        }

        assert_eq!(
            capabilities.local_session_discovery,
            conformance.local_session.is_some(),
            "{kind} local-session discovery requires fixture-backed behavior"
        );
        if let Some(observation) = conformance.local_session {
            assert_eq!(observation.kind.as_str(), kind);
            assert!(!observation.session_id.as_str().is_empty());
            assert!(observation.workspace.is_absolute());
            assert!(observation.transcript_path.is_absolute());
            assert!(observation.last_activity >= observation.created_at);
        }
    }
}

#[test]
fn coverage_is_complete_and_honest() {
    for adapter in BUILTINS {
        let definition = adapter.spec();
        let kind = definition.kind;
        let conformance = adapter.conformance();
        let native_events = conformance.native_event_names();

        for concern in IntegrationConcern::ALL {
            let coverage = definition.concern_coverage(concern);
            assert!(
                !coverage.detail().trim().is_empty(),
                "{kind} {concern:?} coverage must explain its wire, derivation gap, or unsupported reason"
            );
            if let ConcernCoverage::Partial { via, .. } = coverage {
                assert!(
                    !via.trim().is_empty(),
                    "{kind} {concern:?} partial coverage must name the derivation that reconstructs it"
                );
            }
            assert_coverage_honest(adapter, &conformance, &native_events, concern, coverage);
        }
    }
}

#[test]
fn lifecycle_hooks_are_complete_and_honest() {
    for adapter in BUILTINS {
        let definition = adapter.spec();
        let kind = definition.kind;
        let conformance = adapter.conformance();
        let native_events = conformance.native_event_names();

        for signal_kind in LifecycleSignalKind::ALL {
            let coverage = definition.lifecycle_hooks.get(signal_kind);
            assert!(
                !coverage.detail().trim().is_empty(),
                "{kind} {signal_kind:?} hook coverage must name its native event, derivation gap, or absent reason"
            );
            if let HookCoverage::Derived { via, .. } = coverage {
                assert!(
                    !via.trim().is_empty(),
                    "{kind} {signal_kind:?} derived hook coverage must name the derivation that reconstructs it"
                );
            }
            assert_lifecycle_hook_honest(
                adapter,
                &conformance.classification,
                &native_events,
                signal_kind,
                coverage,
            );
        }

        assert_hook_matches_concern(
            adapter,
            LifecycleSignalKind::Ended,
            IntegrationConcern::SessionEnd,
        );
        assert_hook_matches_concern(
            adapter,
            LifecycleSignalKind::SubagentStarted,
            IntegrationConcern::Subagents,
        );
        assert_hook_matches_concern(
            adapter,
            LifecycleSignalKind::SubagentStopped,
            IntegrationConcern::Subagents,
        );
        assert_compaction_hooks_match_concern(adapter);
    }
}

#[test]
fn session_end_policy_follows_native_catalog_event() {
    for adapter in BUILTINS {
        let definition = adapter.spec();
        let conformance = adapter.conformance();
        let samples = &conformance.classification;
        let decoded_ends = |event: &str| {
            samples
                .iter()
                .find(|sample| sample.event_name == event)
                .is_some_and(|sample| {
                    adapter
                        .decode_hook(sample.event_name, &sample.payload)
                        .expect("catalog payload decodes")
                        .ends_session()
                })
        };
        match definition.lifecycle_hooks.ended {
            HookCoverage::Native { event } => assert!(
                decoded_ends(event),
                "{} must end on {event}",
                definition.kind
            ),
            HookCoverage::Derived { .. } | HookCoverage::Absent { .. } => assert!(
                !samples.iter().any(|sample| decoded_ends(sample.event_name)),
                "{} derived/absent end must stay false",
                definition.kind
            ),
        }
    }
}

#[test]
fn realtime_cost_matches_coverage() {
    for adapter in BUILTINS {
        let kind = adapter.spec().kind;
        let coverage = coverage_for(adapter, IntegrationConcern::RealtimeCost);
        let conformance = adapter.conformance();
        assert_eq!(
            !matches!(coverage, ConcernCoverage::Unsupported { .. }),
            realtime_cost_from_fixture(adapter, &conformance),
            "{kind} RealtimeCost coverage must match session_cost_usd fixture output"
        );
    }
}

#[test]
fn wiring_inputs_cover_every_provider_file_used_for_admission() {
    for adapter in BUILTINS {
        let kind = adapter.spec().kind;
        let paths = adapter.wiring_input_paths();
        match kind {
            "claude" | "antigravity" | "cursor" | "kiro" => assert!(
                paths.is_empty(),
                "{kind} local discovery needs no provider wiring input"
            ),
            "codex" => assert_eq!(paths.len(), 1, "Codex model config input"),
            "copilot" => assert_eq!(paths.len(), 2, "Copilot hook and settings inputs"),
            _ if !adapter.spec().capabilities.local_session_discovery
                && adapter.spec().has_wired_hook_install() =>
            {
                assert!(!paths.is_empty(), "{kind} hook admission input")
            }
            _ => {}
        }
    }
}

#[test]
fn awaiting_input_projects_to_waiting() {
    for adapter in BUILTINS {
        let kind = adapter.spec().kind;
        let conformance = adapter.conformance();
        for ask_kind in producible_ask_kinds(&conformance.classification) {
            let prior = LifecycleState {
                status: AgentStatus::Running,
                phase: TurnPhase::Reasoning,
                compacting: false,
            };
            let transition = step(
                Some(&prior),
                None,
                &LifecycleSignal::AwaitingInput {
                    kind: ask_kind,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                },
            );

            assert_eq!(
                transition.next.status,
                AgentStatus::Waiting,
                "{kind} pending {ask_kind:?} should project to waiting"
            );
            assert_eq!(
                transition.next.phase,
                TurnPhase::Idle,
                "{kind} waiting phase"
            );
        }
    }
}

#[test]
fn loaded_plugin_uses_the_same_descriptor_cross_checks() {
    let root = tempfile::TempDir::new().expect("plugin root");
    let plugin_dir = root.path().join("fixturebot");
    fs::create_dir(&plugin_dir).expect("plugin dir");
    fs::write(plugin_dir.join("README.md"), "hook setup").expect("setup doc");
    fs::write(
        plugin_dir.join("agent.toml"),
        r#"protocol = 1
kind = "fixturebot"
display-name = "Fixture Bot"
process-names = ["fixturebot"]
emits = ["session_start", "turn_start", "turn_end"]
setup-doc = "README.md"
"#,
    )
    .expect("manifest");
    let loaded = super::plugins::load_from_root(root.path());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let adapter = loaded.definitions[0];
    let conformance = adapter.conformance();
    let native_events = conformance.native_event_names();

    for concern in IntegrationConcern::ALL {
        let coverage = coverage_for(adapter, concern);
        assert_coverage_honest(adapter, &conformance, &native_events, concern, coverage);
    }

    for signal_kind in LifecycleSignalKind::ALL {
        let coverage = hook_coverage_for(adapter, signal_kind);
        assert_lifecycle_hook_honest(
            adapter,
            &conformance.classification,
            &native_events,
            signal_kind,
            coverage,
        );
    }
    assert_hook_matches_concern(
        adapter,
        LifecycleSignalKind::Ended,
        IntegrationConcern::SessionEnd,
    );
    assert_hook_matches_concern(
        adapter,
        LifecycleSignalKind::SubagentStarted,
        IntegrationConcern::Subagents,
    );
    assert_hook_matches_concern(
        adapter,
        LifecycleSignalKind::SubagentStopped,
        IntegrationConcern::Subagents,
    );
    assert_compaction_hooks_match_concern(adapter);
}

fn producible_ask_kinds(samples: &[ClassificationSample]) -> Vec<AskKind> {
    let mut kinds = Vec::new();
    for sample in samples {
        if let Some(kind) = sample.expected.ask_kind
            && !kinds.contains(&kind)
        {
            kinds.push(kind);
        }
    }
    kinds
}

fn assert_coverage_honest(
    adapter: &AgentDefinition,
    conformance: &AdapterConformance,
    native_events: &[&str],
    concern: IntegrationConcern,
    coverage: ConcernCoverage,
) {
    let definition = adapter.spec();
    let kind = definition.kind;
    let samples = &conformance.classification;
    // `Partial` and `Unsupported` generally assert no native signal carries
    // the concern. Compaction and subagents are the exceptions: a partial can
    // expose only part of their multi-signal or multi-provider surface.
    let wired = coverage.is_wired();
    match concern {
        IntegrationConcern::TurnLifecycle => assert_eq!(
            wired,
            observes_turn_lifecycle(adapter, samples),
            "{kind} TurnLifecycle coverage must match observed lifecycle samples"
        ),
        IntegrationConcern::Permission => assert_eq!(
            wired,
            has_ask_kind(samples, AskKind::Permission),
            "{kind} Permission coverage must match classified permission ask samples"
        ),
        IntegrationConcern::PlanApproval => assert_eq!(
            wired,
            has_ask_kind(samples, AskKind::PlanApproval)
                || has_blocking_tool_kind(definition, AskKind::PlanApproval)
                || derived_ask_kind(adapter, conformance.derived_ask.as_ref())
                    == Some(AskKind::PlanApproval),
            "{kind} PlanApproval coverage must match blocking plan ask/tool classification"
        ),
        IntegrationConcern::UserQuestion => assert_eq!(
            wired,
            has_ask_kind(samples, AskKind::Question)
                || has_blocking_tool_kind(definition, AskKind::Question),
            "{kind} UserQuestion coverage must match blocking question ask/tool classification"
        ),
        IntegrationConcern::Answer => assert_eq!(
            wired,
            has_answer_plan(adapter),
            "{kind} Answer coverage must match the adapter answer planner"
        ),
        IntegrationConcern::Compaction => {
            let opener = hook_coverage_for(adapter, LifecycleSignalKind::Compacting);
            let closer = hook_coverage_for(adapter, LifecycleSignalKind::CompactionEnded);
            let expected = match (opener, closer) {
                (HookCoverage::Native { .. }, HookCoverage::Native { .. }) => "wired",
                (HookCoverage::Absent { .. }, HookCoverage::Absent { .. }) => "unsupported",
                _ => "partial",
            };
            let actual = match coverage {
                ConcernCoverage::Wired { .. } => "wired",
                ConcernCoverage::Partial { .. } => "partial",
                ConcernCoverage::Unsupported { .. } => "unsupported",
            };
            assert_eq!(
                actual, expected,
                "{kind} Compaction coverage must match its opener/closer hook coverage"
            );
            assert_eq!(
                !matches!(coverage, ConcernCoverage::Unsupported { .. }),
                observes_compaction(adapter, samples),
                "{kind} Compaction coverage must match observed compaction lifecycle samples"
            );
        }
        IntegrationConcern::Subagents => {
            let observes_natively = [
                LifecycleSignalKind::SubagentStarted,
                LifecycleSignalKind::SubagentStopped,
            ]
            .into_iter()
            .any(|signal| {
                matches!(
                    hook_coverage_for(adapter, signal),
                    HookCoverage::Native { .. }
                )
            });
            assert_eq!(
                observes_natively,
                observes_subagent_lifecycle(adapter, samples),
                "{kind} Subagents coverage must match observed subagent lifecycle samples"
            );
        }
        IntegrationConcern::BackgroundParking => {}
        IntegrationConcern::SessionEnd => assert_eq!(
            wired,
            samples
                .iter()
                .filter(|sample| native_events.contains(&sample.event_name))
                .any(|sample| {
                    adapter
                        .decode_hook(sample.event_name, &sample.payload)
                        .expect("native event decodes")
                        .ends_session()
                }),
            "{kind} SessionEnd coverage must match a native session-ending event"
        ),
        IntegrationConcern::IdleNotification => {
            if wired {
                assert!(
                    native_event_classifies(adapter, samples, native_events, "Notification"),
                    "{kind} wired IdleNotification coverage requires a native Notification event"
                );
            }
        }
        IntegrationConcern::RichContext | IntegrationConcern::HookInstall => {}
        IntegrationConcern::RemoteControl => assert_eq!(
            wired,
            definition.capabilities.remote_control.pane_sessions
                || definition.capabilities.remote_control.background_sessions,
            "{kind} RemoteControl coverage must match remote-control capabilities"
        ),
        IntegrationConcern::ContextUsage => {}
        IntegrationConcern::RealtimeCost => assert_eq!(
            !matches!(coverage, ConcernCoverage::Unsupported { .. }),
            realtime_cost_from_fixture(adapter, conformance),
            "{kind} RealtimeCost coverage must match session_cost_usd fixture output"
        ),
        IntegrationConcern::AccountSpend => {}
    }
}

fn has_answer_plan(adapter: &AgentDefinition) -> bool {
    let reply = AskReply {
        picks: vec![0],
        text: None,
    };
    let question = AskQuestion {
        question: "Choose?".to_owned(),
        options: vec![AskOption::from("yes".to_owned())],
        multi_select: false,
        has_option_previews: false,
    };
    adapter
        .answer_plan(AskKind::Question, &[question], std::slice::from_ref(&reply))
        .is_ok()
        || adapter
            .answer_plan(AskKind::Permission, &[], std::slice::from_ref(&reply))
            .is_ok()
        || adapter
            .answer_plan(AskKind::PlanApproval, &[], &[reply])
            .is_ok()
}

fn coverage_for(adapter: &AgentDefinition, concern: IntegrationConcern) -> ConcernCoverage {
    adapter.spec().concern_coverage(concern)
}

fn hook_coverage_for(adapter: &AgentDefinition, signal_kind: LifecycleSignalKind) -> HookCoverage {
    adapter.spec().lifecycle_hooks.get(signal_kind)
}

fn assert_lifecycle_hook_honest(
    adapter: &AgentDefinition,
    samples: &[ClassificationSample],
    native_events: &[&str],
    signal_kind: LifecycleSignalKind,
    coverage: HookCoverage,
) {
    let kind = adapter.spec().kind;
    match coverage {
        HookCoverage::Native { event } => {
            assert!(
                native_events.contains(&event),
                "{kind} {signal_kind:?} native event {event} must be declared"
            );
            assert!(
                samples.iter().any(|sample| {
                    sample.event_name == event
                        && sample_produces_signal(adapter, sample, signal_kind)
                }),
                "{kind} {signal_kind:?} native event {event} must produce the declared lifecycle signal in the corpus"
            );
        }
        HookCoverage::Derived { .. } | HookCoverage::Absent { .. } => {
            assert!(
                !samples
                    .iter()
                    .any(|sample| sample_produces_signal(adapter, sample, signal_kind)),
                "{kind} {signal_kind:?} is declared non-native but a corpus sample produces it"
            );
        }
    }
}

fn sample_produces_signal(
    adapter: &AgentDefinition,
    sample: &ClassificationSample,
    signal_kind: LifecycleSignalKind,
) -> bool {
    if signal_kind == LifecycleSignalKind::AwaitingInput {
        return adapter.spec().capabilities.native_ask_ui
            && adapter
                .decode_hook(sample.event_name, &sample.payload)
                .expect("corpus payload decodes")
                .class()
                == AgentHookClass::AwaitingUser;
    }
    adapter
        .decode_hook(sample.event_name, &sample.payload)
        .expect("corpus payload decodes")
        .lifecycle()
        .is_some_and(|observation| observation.signal.kind() == signal_kind)
}

fn assert_hook_matches_concern(
    adapter: &AgentDefinition,
    signal_kind: LifecycleSignalKind,
    concern: IntegrationConcern,
) {
    let kind = adapter.spec().kind;
    let hook = hook_coverage_for(adapter, signal_kind);
    let concern_coverage = coverage_for(adapter, concern);
    let matches = if matches!(
        concern,
        IntegrationConcern::Compaction | IntegrationConcern::Subagents
    ) {
        matches!(
            (hook, concern_coverage),
            (
                HookCoverage::Native { .. }
                    | HookCoverage::Derived { .. }
                    | HookCoverage::Absent { .. },
                ConcernCoverage::Partial { .. }
            ) | (HookCoverage::Native { .. }, ConcernCoverage::Wired { .. })
                | (
                    HookCoverage::Absent { .. },
                    ConcernCoverage::Unsupported { .. }
                )
        )
    } else {
        matches!(
            (hook, concern_coverage),
            (HookCoverage::Native { .. }, ConcernCoverage::Wired { .. })
                | (
                    HookCoverage::Derived { .. },
                    ConcernCoverage::Partial { .. }
                )
                | (
                    HookCoverage::Absent { .. },
                    ConcernCoverage::Unsupported { .. }
                )
        )
    };
    assert!(
        matches,
        "{kind} {signal_kind:?} hook coverage must agree with {concern:?} concern coverage"
    );
}

fn assert_compaction_hooks_match_concern(adapter: &AgentDefinition) {
    let opening = hook_coverage_for(adapter, LifecycleSignalKind::Compacting);
    let closing = hook_coverage_for(adapter, LifecycleSignalKind::CompactionEnded);
    let concern = coverage_for(adapter, IntegrationConcern::Compaction);
    let matches = match (opening, closing) {
        (HookCoverage::Native { .. }, HookCoverage::Native { .. }) => {
            matches!(concern, ConcernCoverage::Wired { .. })
        }
        (HookCoverage::Absent { .. }, HookCoverage::Absent { .. }) => {
            matches!(concern, ConcernCoverage::Unsupported { .. })
        }
        _ => matches!(concern, ConcernCoverage::Partial { .. }),
    };
    assert!(
        matches,
        "{} compaction hook pair must agree with Compaction concern coverage",
        adapter.spec().kind
    );
}

fn realtime_cost_from_fixture(adapter: &AgentDefinition, conformance: &AdapterConformance) -> bool {
    let prices = PriceBook::embedded();
    if let Some(fixture) = conformance.context_cost.as_ref() {
        return adapter
            .context_cost(&fixture.payload, &prices)
            .and_then(|cost| cost.total_cost_usd)
            .is_some_and(|cost| cost > 0.0);
    }
    if let Some(fixture) = conformance.hook_turn_cost.as_ref() {
        return adapter
            .price_turn_locally(fixture.event_name, &fixture.payload, &prices)
            .is_some_and(|cost| cost.cost_usd > 0.0);
    }
    conformance.spend.as_ref().is_some_and(|fixture| {
        let dir = tempfile::TempDir::new().expect("spend fixture tempdir");
        let path = materialize_spend_fixture(dir.path(), fixture);
        super::spending::session_cost_usd(adapter, fixture.session_id, &path, &prices)
            .and_then(|cost| cost.total_cost_usd)
            .is_some_and(|cost| cost > 0.0)
    })
}

fn materialize_spend_fixture(dir: &Path, fixture: &SpendFixture) -> PathBuf {
    let path = dir.join(fixture.file_name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("spend fixture parent");
    }
    match fixture.body {
        SpendFixtureBody::Jsonl(body) => {
            fs::write(&path, format!("{body}\n")).expect("write spend JSONL fixture");
        }
        SpendFixtureBody::OpencodeSqlite { data } => {
            let conn = rusqlite::Connection::open(&path).expect("open spend SQLite fixture");
            conn.execute(
                "CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)",
                [],
            )
            .expect("create message table");
            conn.execute(
                "INSERT INTO message (id, session_id, data) VALUES ('msg', ?1, ?2)",
                (fixture.session_id, data),
            )
            .expect("insert message fixture");
        }
    }
    path
}

fn derived_ask_kind(
    adapter: &AgentDefinition,
    fixture: Option<&DerivedAskFixture>,
) -> Option<AskKind> {
    let fixture = fixture?;
    let dir = tempfile::TempDir::new().expect("derived ask fixture tempdir");
    let path = materialize_derived_ask_fixture(dir.path(), fixture);
    let mut payload = fixture.payload.clone();
    payload.as_object_mut()?.insert(
        "transcript_path".to_owned(),
        serde_json::json!(path.to_string_lossy()),
    );
    let observed = adapter
        .decode_hook(fixture.event_name, &payload)
        .expect("derived ask payload decodes")
        .lifecycle()
        .and_then(|observation| match &observation.signal {
            LifecycleSignal::AwaitingInput { kind, .. } => Some(*kind),
            _ => None,
        });
    assert_eq!(
        observed,
        Some(fixture.expected_kind),
        "{} derived ask fixture must produce its declared kind",
        adapter.spec().kind
    );
    observed
}

fn materialize_derived_ask_fixture(dir: &Path, fixture: &DerivedAskFixture) -> PathBuf {
    let path = dir.join(fixture.transcript_file_name);
    fs::write(&path, format!("{}\n", fixture.transcript_body))
        .expect("write derived ask JSONL fixture");
    path
}

fn has_ask_kind(samples: &[ClassificationSample], ask_kind: AskKind) -> bool {
    samples
        .iter()
        .any(|sample| sample.expected.ask_kind == Some(ask_kind))
}

fn has_blocking_tool_kind(definition: &super::AgentSpec, ask_kind: AskKind) -> bool {
    definition
        .tools
        .blocking
        .iter()
        .any(|(_, kind)| *kind == ask_kind)
}

fn observes_turn_lifecycle(adapter: &AgentDefinition, samples: &[ClassificationSample]) -> bool {
    samples.iter().any(|sample| {
        sample.expected.class == AgentHookClass::Lifecycle
            && adapter
                .decode_hook(sample.event_name, &sample.payload)
                .expect("corpus payload decodes")
                .lifecycle()
                .is_some_and(|obs| {
                    matches!(
                        obs.signal,
                        LifecycleSignal::Registered
                            | LifecycleSignal::TurnStarted
                            | LifecycleSignal::TurnEnded { .. }
                    )
                })
    })
}

fn observes_compaction(adapter: &AgentDefinition, samples: &[ClassificationSample]) -> bool {
    samples.iter().any(|sample| {
        sample.expected.class == AgentHookClass::Lifecycle
            && adapter
                .decode_hook(sample.event_name, &sample.payload)
                .expect("corpus payload decodes")
                .lifecycle()
                .is_some_and(|obs| {
                    matches!(
                        obs.signal,
                        LifecycleSignal::Compacting | LifecycleSignal::CompactionEnded { .. }
                    )
                })
    })
}

fn observes_subagent_lifecycle(
    adapter: &AgentDefinition,
    samples: &[ClassificationSample],
) -> bool {
    samples.iter().any(|sample| {
        sample.expected.class == AgentHookClass::Lifecycle
            && sample.event_name.to_ascii_lowercase().contains("subagent")
            && adapter
                .decode_hook(sample.event_name, &sample.payload)
                .expect("corpus payload decodes")
                .lifecycle()
                .is_some_and(|obs| {
                    obs.parent_agent_id.is_some()
                        && matches!(
                            obs.signal,
                            LifecycleSignal::SubagentStarted
                                | LifecycleSignal::SubagentStopped { .. }
                        )
                })
    })
}

fn native_event_classifies(
    adapter: &AgentDefinition,
    samples: &[ClassificationSample],
    native_events: &[&str],
    event_name: &str,
) -> bool {
    native_events.contains(&event_name)
        && samples.iter().any(|sample| {
            sample.event_name == event_name
                && adapter
                    .decode_hook(sample.event_name, &sample.payload)
                    .expect("corpus payload decodes")
                    .class()
                    == AgentHookClass::Lifecycle
        })
}
