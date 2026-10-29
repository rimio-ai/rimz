//! Root-session active-time ingress from normalized lifecycle hooks.

use tracing::warn;

use rimz::agents::spending::session_effort::{self, SessionEffortEvent, SessionEffortRecord};

use super::{AgentDefinition, HookOutput, LifecycleSignal, RecordedLifecycle, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveTimeOp {
    Progress,
    Pulse,
    Stop,
}

pub(super) fn record(
    store: &Store,
    agent: &AgentDefinition,
    decoded: &HookOutput,
    recorded: Option<&RecordedLifecycle>,
    agent_id: Option<&str>,
    event_name: &str,
) {
    let signal = recorded.map(|recorded| &recorded.observation.signal);
    let parent_agent_id =
        recorded.and_then(|recorded| recorded.observation.parent_agent_id.as_deref());
    let Some(op) = active_time_op(
        signal,
        parent_agent_id.is_some(),
        decoded.records_progress(),
        decoded.ends_session(),
    ) else {
        return;
    };
    let agent_id = match op {
        ActiveTimeOp::Pulse => parent_agent_id,
        ActiveTimeOp::Progress | ActiveTimeOp::Stop => agent_id,
    };
    let Some(agent_id) = agent_id else { return };
    let at = active_time_at(
        op,
        rimz::harness::hook_drain::frame_timestamp(),
        decoded.turn_error(),
    );
    apply(store, agent, op, agent_id, at, event_name);
}

/// Credit a side conversation's working time to the root session hosting it.
pub(super) fn record_side_conversation(
    store: &Store,
    agent: &AgentDefinition,
    signal: &LifecycleSignal,
    host_agent_id: &str,
    host_running: bool,
    event_name: &str,
) {
    let op = side_conversation_op(signal, host_running);
    let at = active_time_at(op, rimz::harness::hook_drain::frame_timestamp(), None);
    apply(store, agent, op, host_agent_id, at, event_name);
}

fn apply(
    store: &Store,
    agent: &AgentDefinition,
    op: ActiveTimeOp,
    agent_id: &str,
    at: jiff::Timestamp,
    event_name: &str,
) {
    let grace_secs = rimz::config::AttentionConfig::load_lenient()
        .active_grace_secs
        .get();
    let result = match op {
        ActiveTimeOp::Progress => rimz::store::active_time::record_progress(
            store.runtime_paths(),
            agent.spec().kind,
            agent_id,
            at,
            grace_secs,
        ),
        ActiveTimeOp::Pulse => rimz::store::active_time::record_pulse(
            store.runtime_paths(),
            agent.spec().kind,
            agent_id,
            at,
            grace_secs,
        )
        .map(|(_, released)| released),
        ActiveTimeOp::Stop => rimz::store::active_time::record_stop(
            store.runtime_paths(),
            agent.spec().kind,
            agent_id,
            at,
            grace_secs,
        ),
    };
    let released = match result {
        Ok(0) => return,
        Ok(released) => released,
        Err(err) => {
            warn!(
                agent = agent.spec().kind,
                event = %event_name,
                error = %err,
                "lifecycle: failed to update estimated active time",
            );
            return;
        }
    };
    if let Err(error) = session_effort::record(
        store.runtime_paths(),
        &SessionEffortRecord {
            kind: agent.spec().kind_id(),
            session: agent_id.into(),
            at,
            event: SessionEffortEvent::Active {
                active_ms: released,
            },
        },
    ) {
        warn!(
            agent = agent.spec().kind,
            event = %event_name,
            %error,
            "lifecycle: failed to append session effort"
        );
    }
}

fn active_time_op(
    signal: Option<&LifecycleSignal>,
    is_child: bool,
    records_progress: bool,
    ends_session: bool,
) -> Option<ActiveTimeOp> {
    if is_child {
        return matches!(signal, Some(LifecycleSignal::SubagentStopped { .. }))
            .then_some(ActiveTimeOp::Pulse);
    }
    match signal {
        Some(
            LifecycleSignal::TurnStarted { .. }
            | LifecycleSignal::ToolUsed { .. }
            | LifecycleSignal::Compacting
            | LifecycleSignal::CompactionEnded { failed: false, .. },
        ) => Some(ActiveTimeOp::Progress),
        Some(
            LifecycleSignal::AwaitingInput { .. }
            | LifecycleSignal::TurnEnded { .. }
            | LifecycleSignal::TurnInterrupted { .. }
            | LifecycleSignal::CompactionEnded { failed: true, .. }
            | LifecycleSignal::Ended
            | LifecycleSignal::Lost,
        ) => Some(ActiveTimeOp::Stop),
        // An ask close is no proof of activity; the next tool or turn resumes the clock.
        Some(
            LifecycleSignal::Registered
            | LifecycleSignal::AskResolved { .. }
            | LifecycleSignal::SubagentStarted
            | LifecycleSignal::SubagentStopped { .. },
        ) => None,
        None if ends_session => Some(ActiveTimeOp::Stop),
        None if records_progress => Some(ActiveTimeOp::Progress),
        None => None,
    }
}

/// A side turn opens or extends the host's span, and its end closes the span
/// only when the host's own turn is not running; everything else extends an
/// open span.
fn side_conversation_op(signal: &LifecycleSignal, host_running: bool) -> ActiveTimeOp {
    match signal {
        LifecycleSignal::TurnStarted { .. } => ActiveTimeOp::Progress,
        LifecycleSignal::TurnEnded { .. } | LifecycleSignal::TurnInterrupted { .. }
            if !host_running =>
        {
            ActiveTimeOp::Stop
        }
        _ => ActiveTimeOp::Pulse,
    }
}

fn active_time_at(
    op: ActiveTimeOp,
    now: jiff::Timestamp,
    turn_error: Option<&rimz::agents::AgentTurnError>,
) -> jiff::Timestamp {
    match op {
        ActiveTimeOp::Progress | ActiveTimeOp::Pulse => now,
        ActiveTimeOp::Stop => turn_error.map_or(now, |error| error.at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::AskKind;

    #[test]
    fn released_effort_is_appended_once_at_the_operation_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let id = rimz::ids::WorkspaceId::from_project_root(dir.path());
        let store = Store::open(
            rimz::StatePaths::under(id.clone(), dir.path()).unwrap(),
            rimz::RuntimePaths::under(id, dir.path()).unwrap(),
        )
        .unwrap();
        let agent = rimz::agents::definition_by_kind("claude").unwrap();
        for (op, second) in [
            (ActiveTimeOp::Progress, 0),
            (ActiveTimeOp::Progress, 10),
            (ActiveTimeOp::Stop, 20),
            (ActiveTimeOp::Stop, 20),
        ] {
            apply(
                &store,
                agent,
                op,
                "host",
                jiff::Timestamp::from_second(second).unwrap(),
                "test",
            );
        }
        let records = effort_records(&store);
        assert_eq!(records.len(), 1, "only nonzero releases append effort");
        assert_eq!(records[0]["session"], "host");
        assert_eq!(records[0]["event"], "active");
        assert_eq!(records[0]["active_ms"], 20_000);
        assert_eq!(records[0]["at"], "1970-01-01T00:00:20Z");
    }

    #[test]
    fn side_conversation_effort_is_released_to_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let id = rimz::ids::WorkspaceId::from_project_root(dir.path());
        let store = Store::open(
            rimz::StatePaths::under(id.clone(), dir.path()).unwrap(),
            rimz::RuntimePaths::under(id, dir.path()).unwrap(),
        )
        .unwrap();
        let agent = rimz::agents::definition_by_kind("claude").unwrap();
        for (signal, second) in [
            (LifecycleSignal::TurnStarted { turn_id: None }, 0),
            (
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    turn_id: None,
                },
                20,
            ),
        ] {
            let frame = rimz::store::ingress::HookIngress {
                boot_id: None,
                schema_version: "1".into(),
                event_id: rimz::ids::EventId::new(),
                ts: jiff::Timestamp::from_second(second).unwrap(),
                source: agent.spec().kind_id(),
                event: None,
                payload: String::new(),
                cwd: dir.path().to_owned(),
                hook_pid: std::process::id(),
                env: Default::default(),
            };
            rimz::harness::hook_drain::with_frame_env(
                &frame,
                rimz::harness::hook_drain::FrameAttempt::First,
                || {
                    record_side_conversation(&store, agent, &signal, "host", false, "test");
                },
            );
        }
        let records = effort_records(&store);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["session"], "host");
        assert_eq!(records[0]["active_ms"], 20_000);
    }

    fn effort_records(store: &Store) -> Vec<serde_json::Value> {
        let directory = store.runtime_paths().spending_history.effort_dir();
        std::fs::read_dir(directory)
            .into_iter()
            .flatten()
            .flatten()
            .flat_map(|entry| session_effort::read_segment(&entry.path()))
            .map(|record| serde_json::to_value(record).unwrap())
            .collect()
    }

    #[test]
    fn side_conversations_credit_their_host_without_closing_its_live_turn() {
        let started = LifecycleSignal::TurnStarted { turn_id: None };
        let ended = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        };
        let interrupted = LifecycleSignal::TurnInterrupted { turn_id: None };
        let compacting = LifecycleSignal::Compacting;
        for (signal, host_running, op) in [
            (&started, false, ActiveTimeOp::Progress),
            (&started, true, ActiveTimeOp::Progress),
            (&ended, false, ActiveTimeOp::Stop),
            (&ended, true, ActiveTimeOp::Pulse),
            (&interrupted, false, ActiveTimeOp::Stop),
            (&interrupted, true, ActiveTimeOp::Pulse),
            (&compacting, false, ActiveTimeOp::Pulse),
            (&compacting, true, ActiveTimeOp::Pulse),
        ] {
            assert_eq!(
                side_conversation_op(signal, host_running),
                op,
                "{signal:?} host_running={host_running}"
            );
        }
    }

    #[test]
    fn mapping_covers_every_lifecycle_signal_and_bare_progress() {
        let cases = [
            (LifecycleSignal::Registered, None),
            (
                LifecycleSignal::TurnStarted { turn_id: None },
                Some(ActiveTimeOp::Progress),
            ),
            (
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    turn_id: None,
                },
                Some(ActiveTimeOp::Stop),
            ),
            (
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: true,
                    turn_id: None,
                },
                Some(ActiveTimeOp::Stop),
            ),
            (
                LifecycleSignal::TurnInterrupted { turn_id: None },
                Some(ActiveTimeOp::Stop),
            ),
            (LifecycleSignal::SubagentStarted, None),
            (LifecycleSignal::SubagentStopped { errored: false }, None),
            (
                LifecycleSignal::ToolUsed {
                    mutates: false,
                    edits: false,
                    name: None,
                    native_key: None,
                    turn_id: None,
                },
                Some(ActiveTimeOp::Progress),
            ),
            (
                LifecycleSignal::AwaitingInput {
                    kind: AskKind::Question,
                    ask_id: None,
                    detail: None,
                    native_key: None,
                },
                Some(ActiveTimeOp::Stop),
            ),
            (LifecycleSignal::Compacting, Some(ActiveTimeOp::Progress)),
            (
                LifecycleSignal::CompactionEnded {
                    auto: None,
                    failed: false,
                },
                Some(ActiveTimeOp::Progress),
            ),
            (
                LifecycleSignal::CompactionEnded {
                    auto: Some(false),
                    failed: true,
                },
                Some(ActiveTimeOp::Stop),
            ),
            (LifecycleSignal::Ended, Some(ActiveTimeOp::Stop)),
            (LifecycleSignal::Lost, Some(ActiveTimeOp::Stop)),
        ];
        for (signal, expected) in cases {
            assert_eq!(
                active_time_op(Some(&signal), false, true, true),
                expected,
                "{}",
                signal.tag()
            );
        }
        assert_eq!(
            active_time_op(None, false, true, false),
            Some(ActiveTimeOp::Progress)
        );
        assert_eq!(
            active_time_op(None, false, false, true),
            Some(ActiveTimeOp::Stop)
        );
        assert_eq!(active_time_op(None, false, false, false), None);
    }

    #[test]
    fn child_mapping_pulses_only_on_subagent_stop() {
        assert_eq!(
            active_time_op(
                Some(&LifecycleSignal::SubagentStopped { errored: false }),
                true,
                false,
                false,
            ),
            Some(ActiveTimeOp::Pulse)
        );
        assert_eq!(
            active_time_op(Some(&LifecycleSignal::SubagentStarted), true, true, true,),
            None
        );
    }

    #[test]
    fn stop_uses_the_provider_error_boundary() {
        let now = jiff::Timestamp::from_second(1_000).unwrap();
        let error_at = jiff::Timestamp::from_second(900).unwrap();
        let error = rimz::agents::AgentTurnError {
            at: error_at,
            ..Default::default()
        };

        assert_eq!(
            active_time_at(ActiveTimeOp::Stop, now, Some(&error)),
            error_at
        );
        assert_eq!(active_time_at(ActiveTimeOp::Stop, now, None), now);
        assert_eq!(
            active_time_at(ActiveTimeOp::Progress, now, Some(&error)),
            now
        );
    }
}
