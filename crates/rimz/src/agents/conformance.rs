//! Registry-wide adapter conformance tests.
//!
//! The adapter registry is the production dispatch surface; these tests use the
//! same table so a new adapter inherits the classification/render contracts
//! without adding a second hand-maintained per-agent switch.

use jiff::Timestamp;
use serde_json::json;

use super::lifecycle::{LifecycleSignal, TurnPhase};
use super::{ADAPTERS, AgentAdapter, AgentErr, AgentHookClass, ClassificationSample};
use crate::feed::{AgentStatus, FeedKind, Resolution, ResolutionMethod, Surface};
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

        if capabilities.subagents {
            assert!(
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
                }),
                "{kind} declares subagents but has no observed subagent lifecycle sample"
            );
        }

        if !capabilities.native_ask_ui {
            assert!(
                !feed_kinds
                    .iter()
                    .any(|kind| matches!(kind, FeedKind::PlanApproval | FeedKind::Question)),
                "{kind} declares no native ask UI but classifies native ask feed kinds"
            );
        }
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

fn agent_row(kind: &str) -> SidebarRow {
    SidebarRow {
        id: "agent-hook".to_owned(),
        name: kind.to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
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
