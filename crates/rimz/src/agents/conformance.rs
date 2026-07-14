//! Registry-wide adapter conformance tests.
//!
//! The adapter registry is the production dispatch surface; these tests use the
//! same table so a new adapter inherits the classification/render contracts
//! without adding a second hand-maintained per-agent switch.

use std::fs;
use std::path::{Path, PathBuf};

use super::lifecycle::{LifecycleSignal, LifecycleSignalKind, LifecycleState, TurnPhase, step};
use super::{
    ADAPTERS, AgentAdapter, AgentHookClass, AskReply, ClassificationSample, ConcernCoverage,
    DerivedAskFixture, HookCoverage, IntegrationConcern, LaunchPreset, PresetArgMatcher,
    PresetField, PriceBook, SpendFixture, SpendFixtureBody,
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
    for adapter in ADAPTERS {
        for (field, value) in fields {
            let preset = preset_for_field(field, value);
            match adapter.render_preset(&preset) {
                Ok(argv) => {
                    let matcher = adapter.preset_arg_matcher(field).unwrap_or_else(|| {
                        panic!(
                            "{} renders {field:?} without declaring its argv matcher",
                            adapter.descriptor().kind
                        )
                    });
                    assert!(
                        matcher_consumes(&matcher, &argv),
                        "{} {field:?} matcher {matcher:?} does not consume {argv:?}",
                        adapter.descriptor().kind
                    );
                }
                Err(_) => assert!(
                    adapter.preset_arg_matcher(field).is_none(),
                    "{} declares a matcher for unsupported {field:?}",
                    adapter.descriptor().kind
                ),
            }
        }
    }
}

fn preset_for_field(field: PresetField, value: &str) -> LaunchPreset {
    let mut preset = LaunchPreset::default();
    match field {
        PresetField::Model => preset.model = Some(value.to_owned()),
        PresetField::Effort => preset.effort = Some(value.to_owned()),
        PresetField::SystemPromptFile => preset.system_prompt_file = Some(value.into()),
        PresetField::AppendSystemPromptFile => {
            preset.append_system_prompt_file = Some(value.into());
        }
    }
    preset
}

fn matcher_consumes(matcher: &PresetArgMatcher, argv: &[String]) -> bool {
    match matcher {
        PresetArgMatcher::Flag(flags) => match argv {
            [flag, _value] => flags.contains(flag),
            [joined] => flags.iter().any(|flag| {
                joined
                    .strip_prefix(flag)
                    .is_some_and(|rest| rest.starts_with('='))
            }),
            _ => false,
        },
        PresetArgMatcher::ConfigKey { flags, key } => match argv {
            [flag, value] => flags.contains(flag) && value.starts_with(&format!("{key}=")),
            [joined] => flags.iter().any(|flag| {
                joined
                    .strip_prefix(flag)
                    .and_then(|rest| rest.strip_prefix('='))
                    .is_some_and(|value| value.starts_with(&format!("{key}=")))
            }),
            _ => false,
        },
    }
}

#[test]
fn classify_matches_corpus() {
    for adapter in ADAPTERS {
        let kind = adapter.descriptor().kind;
        let samples = corpus(*adapter);
        for sample in samples {
            let actual = adapter.classify_hook(sample.event_name, &sample.payload);
            assert_eq!(
                actual, sample.expected,
                "{kind} classification sample {}",
                sample.event_name
            );
        }
    }
}

#[test]
fn installed_events_are_covered_by_the_corpus_and_classify_to_a_channel() {
    for adapter in ADAPTERS {
        let kind = adapter.descriptor().kind;
        let samples = corpus(*adapter);
        let installed_events = adapter.installed_hook_events();
        assert_eq!(
            !installed_events.is_empty(),
            adapter.descriptor().capabilities.hook_install,
            "{kind} installed hook events must match hook-install capability"
        );
        if installed_events.is_empty() {
            assert!(
                samples.is_empty(),
                "{kind} adapter without installed hooks must not claim a native classification corpus"
            );
        }
        for event in installed_events {
            assert!(
                samples.iter().any(|sample| sample.event_name == event),
                "{kind} installed event {event} has no classification corpus sample"
            );
        }
        for sample in samples {
            assert_ne!(
                sample.expected.class,
                AgentHookClass::Unknown,
                "{kind} corpus sample {} must expect a real channel",
                sample.event_name
            );
            assert_ne!(
                adapter
                    .classify_hook(sample.event_name, &sample.payload)
                    .class,
                AgentHookClass::Unknown,
                "{kind} installed sample {} classified as unknown",
                sample.event_name
            );
        }
    }
}

#[test]
fn capability_honesty() {
    for adapter in ADAPTERS {
        let kind = adapter.descriptor().kind;
        let capabilities = adapter.descriptor().capabilities;
        let samples = corpus(*adapter);
        let ask_kinds = producible_ask_kinds(&samples);
        let has_blocking = samples
            .iter()
            .any(|sample| sample.expected.class == AgentHookClass::AwaitingUser);

        assert_eq!(
            capabilities.blocking_asks, has_blocking,
            "{kind} blocking_asks capability must match the declared corpus"
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

        if capabilities.realtime_usage.covers_account_while_live
            || capabilities.realtime_usage.windows_defer_to_fresh_realtime
        {
            assert!(
                capabilities.rich_context,
                "{kind} realtime account-usage channel requires rich_context"
            );
        }

        assert_eq!(
            capabilities.local_session_discovery,
            adapter.local_session_fixture().is_some(),
            "{kind} local-session discovery requires fixture-backed behavior"
        );
        if let Some(observation) = adapter.local_session_fixture() {
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
    for adapter in ADAPTERS {
        let descriptor = adapter.descriptor();
        let kind = descriptor.kind;
        let samples = corpus(*adapter);
        let installed_events = adapter.installed_hook_events();

        assert_eq!(
            descriptor.coverage.len(),
            IntegrationConcern::ALL.len(),
            "{kind} coverage must list every integration concern exactly once"
        );

        for concern in IntegrationConcern::ALL {
            let matching: Vec<_> = descriptor
                .coverage
                .iter()
                .filter(|(declared, _)| *declared == concern)
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "{kind} coverage must list {concern:?} exactly once"
            );
            let &(_, coverage) = matching[0];
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
            assert_coverage_honest(*adapter, &samples, &installed_events, concern, coverage);
        }
    }
}

#[test]
fn lifecycle_hooks_are_complete_and_honest() {
    for adapter in ADAPTERS {
        let descriptor = adapter.descriptor();
        let kind = descriptor.kind;
        let samples = corpus(*adapter);
        let installed_events = adapter.installed_hook_events();

        assert_eq!(
            descriptor.lifecycle_hooks.len(),
            LifecycleSignalKind::ALL.len(),
            "{kind} lifecycle_hooks must list every lifecycle signal exactly once"
        );

        for signal_kind in LifecycleSignalKind::ALL {
            let matching: Vec<_> = descriptor
                .lifecycle_hooks
                .iter()
                .filter(|(declared, _)| *declared == signal_kind)
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "{kind} lifecycle_hooks must list {signal_kind:?} exactly once"
            );
            let &(_, coverage) = matching[0];
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
                *adapter,
                &samples,
                &installed_events,
                signal_kind,
                coverage,
            );
        }

        assert_hook_matches_concern(
            *adapter,
            LifecycleSignalKind::Ended,
            IntegrationConcern::SessionEnd,
        );
        assert_hook_matches_concern(
            *adapter,
            LifecycleSignalKind::SubagentStarted,
            IntegrationConcern::Subagents,
        );
        assert_hook_matches_concern(
            *adapter,
            LifecycleSignalKind::SubagentStopped,
            IntegrationConcern::Subagents,
        );
        assert_compaction_hooks_match_concern(*adapter);
    }
}

#[test]
fn realtime_cost_matches_coverage() {
    for adapter in ADAPTERS {
        let kind = adapter.descriptor().kind;
        let coverage = coverage_for(*adapter, IntegrationConcern::RealtimeCost);
        assert_eq!(
            !matches!(coverage, ConcernCoverage::Unsupported { .. }),
            realtime_cost_from_fixture(*adapter),
            "{kind} RealtimeCost coverage must match session_cost_usd fixture output"
        );
    }
}

#[test]
fn awaiting_input_projects_to_waiting() {
    for adapter in ADAPTERS {
        let kind = adapter.descriptor().kind;
        for ask_kind in producible_ask_kinds(&corpus(*adapter)) {
            let prior = LifecycleState {
                status: AgentStatus::Running,
                phase: TurnPhase::Reasoning,
                compacting: false,
            };
            let transition = step(
                Some(&prior),
                &LifecycleSignal::AwaitingInput {
                    kind: ask_kind,
                    ask_id: None,
                    detail: None,
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
    let loaded = super::plugin::load_from_root(root.path());
    assert!(loaded.errors.is_empty(), "{:?}", loaded.errors);
    let adapter = loaded.adapters[0];
    let descriptor = adapter.descriptor();
    let samples = corpus(adapter);
    let installed_events = adapter.installed_hook_events();

    assert_eq!(descriptor.coverage.len(), IntegrationConcern::ALL.len());
    for concern in IntegrationConcern::ALL {
        let coverage = coverage_for(adapter, concern);
        assert_coverage_honest(adapter, &samples, &installed_events, concern, coverage);
    }

    assert_eq!(
        descriptor.lifecycle_hooks.len(),
        LifecycleSignalKind::ALL.len()
    );
    for signal_kind in LifecycleSignalKind::ALL {
        let coverage = hook_coverage_for(adapter, signal_kind);
        assert_lifecycle_hook_honest(adapter, &samples, &installed_events, signal_kind, coverage);
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

fn corpus(adapter: &dyn AgentAdapter) -> Vec<ClassificationSample> {
    adapter.classification_corpus()
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
    adapter: &dyn AgentAdapter,
    samples: &[ClassificationSample],
    installed_events: &[&str],
    concern: IntegrationConcern,
    coverage: ConcernCoverage,
) {
    let descriptor = adapter.descriptor();
    let kind = descriptor.kind;
    // `Partial` and `Unsupported` generally assert no native signal carries
    // the concern. Compaction is the multi-signal exception: a partial can
    // combine a native opening edge with a derived close, cross-checked below.
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
                || has_blocking_tool_kind(descriptor, AskKind::PlanApproval)
                || derived_ask_kind(adapter) == Some(AskKind::PlanApproval),
            "{kind} PlanApproval coverage must match blocking plan ask/tool classification"
        ),
        IntegrationConcern::UserQuestion => assert_eq!(
            wired,
            has_ask_kind(samples, AskKind::Question)
                || has_blocking_tool_kind(descriptor, AskKind::Question),
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
            assert_eq!(
                wired, descriptor.capabilities.subagents,
                "{kind} Subagents coverage must match the subagents capability"
            );
            if wired {
                assert!(
                    observes_subagent_lifecycle(adapter, samples),
                    "{kind} declares subagents but has no observed subagent lifecycle sample"
                );
            }
        }
        IntegrationConcern::BackgroundParking => assert_eq!(
            wired, descriptor.capabilities.background_tasks,
            "{kind} BackgroundParking coverage must match the background_tasks capability"
        ),
        IntegrationConcern::SessionEnd => assert_eq!(
            wired,
            installed_events
                .iter()
                .any(|event| adapter.ends_session(event)),
            "{kind} SessionEnd coverage must match an installed session-ending event"
        ),
        IntegrationConcern::IdleNotification => {
            if wired {
                assert!(
                    installed_event_classifies(adapter, samples, installed_events, "Notification"),
                    "{kind} wired IdleNotification coverage requires an installed Notification event"
                );
            }
        }
        IntegrationConcern::RichContext => assert_eq!(
            wired, descriptor.capabilities.rich_context,
            "{kind} RichContext coverage must match the rich_context capability"
        ),
        IntegrationConcern::HookInstall => assert_eq!(
            wired, descriptor.capabilities.hook_install,
            "{kind} HookInstall coverage must match the hook_install capability"
        ),
        IntegrationConcern::RemoteControl => assert_eq!(
            wired,
            descriptor.capabilities.remote_control.pane_sessions
                || descriptor.capabilities.remote_control.background_sessions,
            "{kind} RemoteControl coverage must match remote-control capabilities"
        ),
        IntegrationConcern::ContextUsage => assert_eq!(
            !matches!(coverage, ConcernCoverage::Unsupported { .. }),
            descriptor.capabilities.context_usage,
            "{kind} ContextUsage coverage must match the context_usage capability"
        ),
        IntegrationConcern::RealtimeCost => assert_eq!(
            !matches!(coverage, ConcernCoverage::Unsupported { .. }),
            realtime_cost_from_fixture(adapter),
            "{kind} RealtimeCost coverage must match session_cost_usd fixture output"
        ),
        IntegrationConcern::AccountSpend => assert!(
            !descriptor.has_authoritative_account_spend() || descriptor.capabilities.account_spend,
            "{kind} wired AccountSpend coverage requires the account_spend capability"
        ),
    }
}

fn has_answer_plan(adapter: &dyn AgentAdapter) -> bool {
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

fn coverage_for(adapter: &dyn AgentAdapter, concern: IntegrationConcern) -> ConcernCoverage {
    adapter
        .descriptor()
        .coverage
        .iter()
        .find(|(declared, _)| *declared == concern)
        .map(|(_, coverage)| *coverage)
        .unwrap_or(ConcernCoverage::Unsupported {
            reason: "coverage row missing",
        })
}

fn hook_coverage_for(adapter: &dyn AgentAdapter, signal_kind: LifecycleSignalKind) -> HookCoverage {
    adapter
        .descriptor()
        .lifecycle_hooks
        .iter()
        .find(|(declared, _)| *declared == signal_kind)
        .map(|(_, coverage)| *coverage)
        .unwrap_or(HookCoverage::Absent {
            reason: "lifecycle hook row missing",
        })
}

fn assert_lifecycle_hook_honest(
    adapter: &dyn AgentAdapter,
    samples: &[ClassificationSample],
    installed_events: &[&str],
    signal_kind: LifecycleSignalKind,
    coverage: HookCoverage,
) {
    let kind = adapter.descriptor().kind;
    match coverage {
        HookCoverage::Native { event } => {
            assert!(
                installed_events.contains(&event),
                "{kind} {signal_kind:?} native event {event} must be installed"
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
    adapter: &dyn AgentAdapter,
    sample: &ClassificationSample,
    signal_kind: LifecycleSignalKind,
) -> bool {
    if signal_kind == LifecycleSignalKind::AwaitingInput {
        return adapter.descriptor().capabilities.native_ask_ui
            && adapter
                .classify_hook(sample.event_name, &sample.payload)
                .class
                == AgentHookClass::AwaitingUser;
    }
    adapter
        .observe_lifecycle(sample.event_name, &sample.payload)
        .is_some_and(|observation| observation.signal.kind() == signal_kind)
}

fn assert_hook_matches_concern(
    adapter: &dyn AgentAdapter,
    signal_kind: LifecycleSignalKind,
    concern: IntegrationConcern,
) {
    let kind = adapter.descriptor().kind;
    let hook = hook_coverage_for(adapter, signal_kind);
    let concern_coverage = coverage_for(adapter, concern);
    let matches = if concern == IntegrationConcern::Compaction {
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

fn assert_compaction_hooks_match_concern(adapter: &dyn AgentAdapter) {
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
        adapter.descriptor().kind
    );
}

fn realtime_cost_from_fixture(adapter: &dyn AgentAdapter) -> bool {
    let prices = PriceBook::embedded();
    if let Some(fixture) = adapter.context_cost_fixture() {
        return adapter
            .context_cost(&fixture.payload, &prices)
            .and_then(|cost| cost.total_cost_usd)
            .is_some_and(|cost| cost > 0.0);
    }
    if let Some(fixture) = adapter.turn_cost_fixture() {
        return adapter
            .price_turn_locally(fixture.event_name, &fixture.payload, &prices)
            .is_some_and(|cost| cost.cost_usd > 0.0);
    }
    adapter.spend_fixture().is_some_and(|fixture| {
        let dir = tempfile::TempDir::new().expect("spend fixture tempdir");
        let path = materialize_spend_fixture(dir.path(), &fixture);
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

fn derived_ask_kind(adapter: &dyn AgentAdapter) -> Option<AskKind> {
    let fixture = adapter.derived_ask_fixture()?;
    let dir = tempfile::TempDir::new().expect("derived ask fixture tempdir");
    let path = materialize_derived_ask_fixture(dir.path(), &fixture);
    let mut payload = fixture.payload;
    payload.as_object_mut()?.insert(
        "transcript_path".to_owned(),
        serde_json::json!(path.to_string_lossy()),
    );
    let observed = adapter
        .observe_lifecycle(fixture.event_name, &payload)
        .and_then(|observation| match observation.signal {
            LifecycleSignal::AwaitingInput { kind, .. } => Some(kind),
            _ => None,
        });
    assert_eq!(
        observed,
        Some(fixture.expected_kind),
        "{} derived ask fixture must produce its declared kind",
        adapter.descriptor().kind
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

fn has_blocking_tool_kind(descriptor: &super::AgentDescriptor, ask_kind: AskKind) -> bool {
    descriptor
        .tools
        .blocking
        .iter()
        .any(|(_, kind)| *kind == ask_kind)
}

fn observes_turn_lifecycle(adapter: &dyn AgentAdapter, samples: &[ClassificationSample]) -> bool {
    samples.iter().any(|sample| {
        sample.expected.class == AgentHookClass::Lifecycle
            && adapter
                .observe_lifecycle(sample.event_name, &sample.payload)
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

fn observes_compaction(adapter: &dyn AgentAdapter, samples: &[ClassificationSample]) -> bool {
    samples.iter().any(|sample| {
        sample.expected.class == AgentHookClass::Lifecycle
            && adapter
                .observe_lifecycle(sample.event_name, &sample.payload)
                .is_some_and(|obs| {
                    matches!(
                        obs.signal,
                        LifecycleSignal::Compacting | LifecycleSignal::CompactionEnded { .. }
                    )
                })
    })
}

fn observes_subagent_lifecycle(
    adapter: &dyn AgentAdapter,
    samples: &[ClassificationSample],
) -> bool {
    samples.iter().any(|sample| {
        sample.expected.class == AgentHookClass::Lifecycle
            && sample.event_name.contains("Subagent")
            && adapter
                .observe_lifecycle(sample.event_name, &sample.payload)
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

fn installed_event_classifies(
    adapter: &dyn AgentAdapter,
    samples: &[ClassificationSample],
    installed_events: &[&str],
    event_name: &str,
) -> bool {
    installed_events.contains(&event_name)
        && samples.iter().any(|sample| {
            sample.event_name == event_name
                && adapter
                    .classify_hook(sample.event_name, &sample.payload)
                    .class
                    == AgentHookClass::Lifecycle
        })
}
