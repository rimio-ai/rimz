//! Registry-wide adapter conformance tests.
//!
//! The adapter registry is the production dispatch surface; these tests use the
//! same table so a new adapter inherits the classification/render contracts
//! without adding a second hand-maintained per-agent switch.

use std::fs;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde_json::json;

use super::lifecycle::{LifecycleSignal, LifecycleSignalKind, TurnPhase};
use super::{
    ADAPTERS, AgentAdapter, AgentErr, AgentHookClass, ClassificationSample, ConcernCoverage,
    HookCoverage, IntegrationConcern, PriceBook, SpendFixture, SpendFixtureBody,
};
use crate::agents::AgentStatus;
use crate::feed::{FeedKind, Resolution, ResolutionMethod, Surface};
use crate::ledger::snapshot::{AgentCard, RowCard, SidebarRow, fold_ask_onto_row};

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
fn classify_render_closure() {
    let resolution = Resolution::new(
        json!({ "choice": "allow", "updatedInput": {} }),
        ResolutionMethod::HookBridge,
    );
    for adapter in ADAPTERS {
        let kind = adapter.descriptor().kind;
        for feed_kind in producible_feed_kinds(&corpus(*adapter)) {
            let item = super::testkit::feed_item(feed_kind, kind);
            match adapter.render_decision(&item, &resolution) {
                Ok(_) => {}
                Err(AgentErr::Render { reason, .. })
                    if reason.starts_with("unsupported feed kind") =>
                {
                    panic!("{kind} classifies {feed_kind:?} but cannot render it")
                }
                Err(err) => panic!(
                    "{kind} render for classified {feed_kind:?} failed with non-closure error: {err}"
                ),
            }
        }
    }
}

#[test]
fn installed_events_are_covered_by_the_corpus_and_classify_to_a_channel() {
    for adapter in ADAPTERS {
        let kind = adapter.descriptor().kind;
        let samples = corpus(*adapter);
        let installed_events = adapter.installed_hook_events();
        assert!(
            !installed_events.is_empty(),
            "{kind} adapter must declare installed hook events for conformance"
        );
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
        let feed_kinds = producible_feed_kinds(&samples);
        let has_blocking = samples
            .iter()
            .any(|sample| sample.expected.class == AgentHookClass::BlockingFeed);

        assert_eq!(
            capabilities.blocking_feed, has_blocking,
            "{kind} blocking_feed capability must match the declared corpus"
        );

        // Subagent honesty (capability ⟹ an observed subagent lifecycle sample) is
        // enforced by `coverage_is_complete_and_honest`'s Subagents arm.

        if !capabilities.native_ask_ui {
            assert!(
                !feed_kinds
                    .iter()
                    .any(|kind| matches!(kind, FeedKind::PlanApproval | FeedKind::Question)),
                "{kind} declares no native ask UI but classifies native ask feed kinds"
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
        assert_hook_matches_concern(
            *adapter,
            LifecycleSignalKind::Compacting,
            IntegrationConcern::Compaction,
        );
        assert_hook_matches_concern(
            *adapter,
            LifecycleSignalKind::CompactionEnded,
            IntegrationConcern::Compaction,
        );
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
fn pending_ask_projects_to_waiting() {
    for adapter in ADAPTERS {
        let kind = adapter.descriptor().kind;
        for feed_kind in producible_feed_kinds(&corpus(*adapter)) {
            let mut row = agent_row(kind);
            let item = super::testkit::feed_item(feed_kind, kind);

            fold_ask_onto_row(&mut row, &item);

            assert_eq!(
                row.status(),
                Some(AgentStatus::Waiting),
                "{kind} pending {feed_kind:?} should project to waiting"
            );
            assert_eq!(row.phase(), TurnPhase::Idle, "{kind} waiting phase");
            assert_eq!(
                row.request_id(),
                Some(&item.request_id),
                "{kind} waiting row carries request id"
            );
            assert_eq!(
                row.surface(),
                Some(Surface::Bridge),
                "{kind} waiting row carries ask surface"
            );
        }
    }
}

fn corpus(adapter: &dyn AgentAdapter) -> Vec<ClassificationSample> {
    let samples = adapter.classification_corpus();
    assert!(
        !samples.is_empty(),
        "{} adapter must declare a conformance corpus",
        adapter.descriptor().kind
    );
    samples
}

fn producible_feed_kinds(samples: &[ClassificationSample]) -> Vec<FeedKind> {
    let mut kinds = Vec::new();
    for sample in samples {
        if let Some(kind) = sample.expected.feed_kind
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
    // `Partial` and `Unsupported` both assert no native signal carries the
    // concern, so both read as `!wired` here: the equality below forbids
    // declaring either when a native backing actually exists (that must be
    // `Wired`). Whether a partial's derivation truly reconstructs the behaviour
    // is editorial, like the via/reason text, and is not checked mechanically.
    let wired = coverage.is_wired();
    match concern {
        IntegrationConcern::TurnLifecycle => assert_eq!(
            wired,
            observes_turn_lifecycle(adapter, samples),
            "{kind} TurnLifecycle coverage must match observed lifecycle samples"
        ),
        IntegrationConcern::Permission => assert_eq!(
            wired,
            has_feed_kind(samples, FeedKind::Permission),
            "{kind} Permission coverage must match classified permission feed samples"
        ),
        IntegrationConcern::PlanApproval => assert_eq!(
            wired,
            has_feed_kind(samples, FeedKind::PlanApproval)
                || has_blocking_tool_kind(descriptor, FeedKind::PlanApproval),
            "{kind} PlanApproval coverage must match blocking plan feed/tool classification"
        ),
        IntegrationConcern::UserQuestion => assert_eq!(
            wired,
            has_feed_kind(samples, FeedKind::Question)
                || has_blocking_tool_kind(descriptor, FeedKind::Question),
            "{kind} UserQuestion coverage must match blocking question feed/tool classification"
        ),
        IntegrationConcern::Compaction => assert_eq!(
            wired,
            observes_compaction(adapter, samples),
            "{kind} Compaction coverage must match observed compaction lifecycle samples"
        ),
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
        IntegrationConcern::IdleNotification => assert_eq!(
            wired,
            installed_event_classifies(adapter, samples, installed_events, "Notification"),
            "{kind} IdleNotification coverage must match an installed Notification event"
        ),
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
            wired, descriptor.capabilities.context_usage,
            "{kind} ContextUsage coverage must match the context_usage capability"
        ),
        IntegrationConcern::RealtimeCost => assert_eq!(
            !matches!(coverage, ConcernCoverage::Unsupported { .. }),
            realtime_cost_from_fixture(adapter),
            "{kind} RealtimeCost coverage must match session_cost_usd fixture output"
        ),
        IntegrationConcern::AccountSpend => assert_eq!(
            wired, descriptor.capabilities.account_spend,
            "{kind} AccountSpend coverage must match the account_spend capability"
        ),
    }
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
                        && adapter
                            .observe_lifecycle(sample.event_name, &sample.payload)
                            .is_some_and(|obs| obs.signal.kind() == signal_kind)
                }),
                "{kind} {signal_kind:?} native event {event} must produce the declared lifecycle signal in the corpus"
            );
        }
        HookCoverage::Derived { .. } | HookCoverage::Absent { .. } => {
            assert!(
                !samples.iter().any(|sample| {
                    adapter
                        .observe_lifecycle(sample.event_name, &sample.payload)
                        .is_some_and(|obs| obs.signal.kind() == signal_kind)
                }),
                "{kind} {signal_kind:?} is declared non-native but a corpus sample produces it"
            );
        }
    }
}

fn assert_hook_matches_concern(
    adapter: &dyn AgentAdapter,
    signal_kind: LifecycleSignalKind,
    concern: IntegrationConcern,
) {
    let kind = adapter.descriptor().kind;
    let hook = hook_coverage_for(adapter, signal_kind);
    let concern_coverage = coverage_for(adapter, concern);
    let matches = matches!(
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
    );
    assert!(
        matches,
        "{kind} {signal_kind:?} hook coverage must agree with {concern:?} concern coverage"
    );
}

fn realtime_cost_from_fixture(adapter: &dyn AgentAdapter) -> bool {
    let Some(fixture) = adapter.spend_fixture() else {
        return false;
    };
    let dir = tempfile::TempDir::new().expect("spend fixture tempdir");
    let path = materialize_spend_fixture(dir.path(), &fixture);
    super::spending::session_cost_usd(adapter, fixture.session_id, &path, &PriceBook::embedded())
        .and_then(|cost| cost.total_cost_usd)
        .is_some_and(|cost| cost > 0.0)
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

fn has_feed_kind(samples: &[ClassificationSample], feed_kind: FeedKind) -> bool {
    samples
        .iter()
        .any(|sample| sample.expected.feed_kind == Some(feed_kind))
}

fn has_blocking_tool_kind(descriptor: &super::AgentDescriptor, feed_kind: FeedKind) -> bool {
    descriptor
        .tools
        .blocking
        .iter()
        .any(|(_, kind)| *kind == feed_kind)
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
                    != AgentHookClass::Unknown
        })
}

fn agent_row(kind: &str) -> SidebarRow {
    SidebarRow {
        id: "agent-hook".to_owned(),
        name: kind.to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        last_activity: Timestamp::from_second(1).unwrap(),
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(AgentStatus::Running),
            phase: TurnPhase::Reasoning,
            ..AgentCard::default()
        })),
    }
}
