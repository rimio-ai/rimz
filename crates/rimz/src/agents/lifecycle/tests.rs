use super::*;

use crate::agents::testkit::all_signals;

fn state(status: AgentStatus, phase: TurnPhase, compacting: bool) -> LifecycleState {
    LifecycleState {
        status,
        phase,
        compacting,
    }
}

fn tool(edits: bool) -> LifecycleSignal {
    LifecycleSignal::ToolUsed {
        mutates: true,
        edits,
    }
}

fn turn_end(errored: bool, parked_on_background: bool) -> LifecycleSignal {
    LifecycleSignal::TurnEnded {
        errored,
        parked_on_background,
    }
}

#[track_caller]
fn assert_next(
    label: &str,
    prev: Option<LifecycleState>,
    signal: LifecycleSignal,
    expected: LifecycleState,
) -> Transition {
    let transition = step(prev.as_ref(), &signal);
    assert_eq!(transition.next, expected, "{label}");
    transition
}

#[track_caller]
fn assert_legacy(wire: serde_json::Value, expected: LifecycleSignal) {
    assert_eq!(
        serde_json::from_value::<LifecycleSignal>(wire).unwrap(),
        expected
    );
}

#[test]
fn root_turn_edges_follow_the_contract() {
    let idle = state(AgentStatus::Idle, TurnPhase::Idle, false);
    let reasoning = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let acting = state(AgentStatus::Running, TurnPhase::Acting, false);

    assert_next("registration", None, LifecycleSignal::Registered, idle);
    let started = assert_next(
        "turn start",
        Some(idle),
        LifecycleSignal::TurnStarted,
        reasoning,
    );
    assert!(started.opened_turn);
    for (label, prev, signal, expected) in [
        ("non-editing work", reasoning, tool(false), reasoning),
        ("first edit", reasoning, tool(true), acting),
        ("no mid-turn re-arm", acting, tool(false), acting),
    ] {
        assert_next(label, Some(prev), signal, expected);
    }
    let reconciled = assert_next("resting tool", Some(idle), tool(true), acting);
    assert_eq!(
        (reconciled.kind, reconciled.opened_turn),
        (
            TransitionKind::Reconciled {
                from: AgentStatus::Idle,
                reason: "tool used outside a running turn",
            },
            true,
        )
    );
    for (label, errored, parked, expected) in [
        (
            "clean end",
            false,
            false,
            state(AgentStatus::Success, TurnPhase::Idle, false),
        ),
        (
            "background park",
            false,
            true,
            state(AgentStatus::Running, TurnPhase::Parked, false),
        ),
        (
            "error",
            true,
            false,
            state(AgentStatus::Failed, TurnPhase::Idle, false),
        ),
        (
            "error beats park",
            true,
            true,
            state(AgentStatus::Failed, TurnPhase::Idle, false),
        ),
    ] {
        assert_next(label, Some(reasoning), turn_end(errored, parked), expected);
    }

    for prior in [
        reasoning,
        state(AgentStatus::Waiting, TurnPhase::Idle, false),
        state(AgentStatus::Running, TurnPhase::Acting, true),
    ] {
        let interrupted = assert_next(
            "interrupted",
            Some(prior),
            LifecycleSignal::TurnInterrupted,
            idle,
        );
        assert_eq!(
            (interrupted.waiting_cleared, interrupted.compaction_closed),
            (prior.status == AgentStatus::Waiting, prior.compacting,)
        );
    }
}

#[test]
fn parked_wake_and_answered_prompt_preserve_boundary_facts() {
    let reasoning = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let parked = state(AgentStatus::Running, TurnPhase::Parked, false);
    let wake = assert_next(
        "parked wake",
        Some(parked),
        LifecycleSignal::TurnStarted,
        reasoning,
    );

    let prompt = assert_next(
        "live prompt",
        Some(reasoning),
        LifecycleSignal::TurnStarted,
        reasoning,
    );
    assert_eq!((wake.opened_turn, prompt.opened_turn), (false, true));

    let waiting = state(AgentStatus::Waiting, TurnPhase::Idle, false);
    let answer = assert_next(
        "answered prompt",
        Some(waiting),
        LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        },
        state(AgentStatus::Running, TurnPhase::Acting, false),
    );
    assert_eq!((answer.waiting_cleared, answer.opened_turn), (true, true));
}

#[test]
fn subagent_and_terminal_edges_follow_the_contract() {
    let reasoning = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let started = assert_next(
        "child start",
        None,
        LifecycleSignal::SubagentStarted,
        reasoning,
    );
    assert!(started.opened_turn);

    for (label, errored, expected) in [
        ("clean child", false, AgentStatus::Success),
        ("errored child", true, AgentStatus::Failed),
    ] {
        assert_next(
            label,
            Some(reasoning),
            LifecycleSignal::SubagentStopped { errored },
            state(expected, TurnPhase::Idle, false),
        );
    }
    for (label, signal, reason) in [
        (
            "ended",
            LifecycleSignal::Ended,
            "session ended (handled as removal)",
        ),
        (
            "lost",
            LifecycleSignal::Lost,
            "session lost (legacy replay marker)",
        ),
    ] {
        let transition = assert_next(label, Some(reasoning), signal, reasoning);
        assert_eq!(
            transition.kind,
            TransitionKind::Ignored { reason },
            "{label}"
        );
    }
}

#[test]
fn compaction_bracket_follows_trigger_and_counts_one_close() {
    let reasoning = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let acting = state(AgentStatus::Running, TurnPhase::Acting, false);
    let opened = assert_next(
        "open",
        Some(acting),
        LifecycleSignal::Compacting,
        state(AgentStatus::Running, TurnPhase::Acting, true),
    );
    assert!(!opened.compaction_closed);

    for (label, prev, auto, expected) in [
        (
            "automatic",
            state(AgentStatus::Running, TurnPhase::Acting, true),
            Some(true),
            acting,
        ),
        (
            "manual",
            state(AgentStatus::Running, TurnPhase::Acting, true),
            Some(false),
            state(AgentStatus::Idle, TurnPhase::Idle, false),
        ),
        (
            "unknown",
            state(AgentStatus::Running, TurnPhase::Reasoning, true),
            None,
            reasoning,
        ),
        (
            "attention",
            state(AgentStatus::Failed, TurnPhase::Idle, true),
            Some(true),
            state(AgentStatus::Failed, TurnPhase::Idle, false),
        ),
    ] {
        let transition = assert_next(
            label,
            Some(prev),
            LifecycleSignal::CompactionEnded { auto },
            expected,
        );
        assert!(transition.compaction_closed, "{label}");
    }

    let resumed = assert_next(
        "auto from idle",
        Some(state(AgentStatus::Idle, TurnPhase::Idle, true)),
        LifecycleSignal::CompactionEnded { auto: Some(true) },
        reasoning,
    );
    assert_eq!(
        (resumed.kind, resumed.compaction_closed, resumed.opened_turn,),
        (
            TransitionKind::Reconciled {
                from: AgentStatus::Idle,
                reason: "auto-compaction resumed a turn"
            },
            true,
            true,
        )
    );

    let ignored = assert_next(
        "unbracketed unknown",
        Some(reasoning),
        LifecycleSignal::CompactionEnded { auto: None },
        reasoning,
    );
    assert_eq!(
        (ignored.kind, ignored.compaction_closed),
        (
            TransitionKind::Ignored {
                reason: "compaction end without an open bracket"
            },
            false,
        )
    );
    let manual = assert_next(
        "unbracketed manual",
        Some(acting),
        LifecycleSignal::CompactionEnded { auto: Some(false) },
        state(AgentStatus::Idle, TurnPhase::Idle, false),
    );
    assert!(!manual.compaction_closed);

    let ordinary = assert_next(
        "ordinary close",
        Some(state(AgentStatus::Running, TurnPhase::Acting, true)),
        LifecycleSignal::TurnStarted,
        reasoning,
    );
    assert_eq!(
        (ordinary.compaction_closed, ordinary.opened_turn),
        (true, true)
    );
}

#[test]
fn all_state_signal_pairs_preserve_machine_invariants() {
    let statuses = [
        AgentStatus::Running,
        AgentStatus::Waiting,
        AgentStatus::Idle,
        AgentStatus::Success,
        AgentStatus::Failed,
        AgentStatus::Paused,
    ];
    let phases = [
        TurnPhase::Idle,
        TurnPhase::Reasoning,
        TurnPhase::Acting,
        TurnPhase::Parked,
    ];
    let states = [None]
        .into_iter()
        .chain(statuses.into_iter().flat_map(|status| {
            phases.into_iter().flat_map(move |phase| {
                [false, true].map(move |compacting| Some(state(status, phase, compacting)))
            })
        }));
    let signals = all_signals();

    for prev in states {
        for signal in &signals {
            let transition = step(prev.as_ref(), signal);
            if !matches!(signal, LifecycleSignal::Ended | LifecycleSignal::Lost)
                && transition.next.status != AgentStatus::Running
            {
                assert_eq!(
                    transition.next.phase,
                    TurnPhase::Idle,
                    "{prev:?} + {signal:?}"
                );
            }
            if prev.is_none_or(|state| !state.compacting) {
                assert!(!transition.compaction_closed, "{prev:?} + {signal:?}");
            }
        }
    }
}

#[test]
fn lifecycle_wire_tags_and_legacy_defaults_are_stable() {
    let signals = [
        (LifecycleSignal::Registered, "registered"),
        (LifecycleSignal::TurnStarted, "turn_started"),
        (turn_end(false, true), "turn_ended"),
        (LifecycleSignal::TurnInterrupted, "turn_interrupted"),
        (tool(true), "tool_used"),
        (
            LifecycleSignal::AwaitingInput {
                kind: AskKind::Question,
                ask_id: None,
                detail: None,
            },
            "awaiting_input",
        ),
        (LifecycleSignal::SubagentStarted, "subagent_started"),
        (
            LifecycleSignal::SubagentStopped { errored: true },
            "subagent_stopped",
        ),
        (LifecycleSignal::Compacting, "compacting"),
        (
            LifecycleSignal::CompactionEnded { auto: Some(true) },
            "compaction_ended",
        ),
        (LifecycleSignal::Ended, "ended"),
        (LifecycleSignal::Lost, "lost"),
    ];
    for (signal, tag) in signals {
        let wire = serde_json::to_value(&signal).unwrap();
        assert_eq!(
            (
                wire["signal"].as_str(),
                signal.tag(),
                serde_json::from_value::<LifecycleSignal>(wire.clone()).unwrap(),
            ),
            (Some(tag), tag, signal)
        );
    }

    for (phase, wire) in [
        (TurnPhase::Idle, "idle"),
        (TurnPhase::Reasoning, "reasoning"),
        (TurnPhase::Acting, "acting"),
        (TurnPhase::Parked, "parked"),
    ] {
        assert_eq!(
            (
                serde_json::to_value(phase).unwrap(),
                serde_json::from_value::<TurnPhase>(serde_json::json!(wire)).unwrap(),
            ),
            (serde_json::json!(wire), phase)
        );
    }

    assert_legacy(
        serde_json::json!({ "signal": "tool_used", "mutates": true }),
        tool(false),
    );
    assert_legacy(
        serde_json::json!({ "signal": "subagent_stopped" }),
        LifecycleSignal::SubagentStopped { errored: false },
    );
    assert_legacy(
        serde_json::json!({ "signal": "compaction_ended" }),
        LifecycleSignal::CompactionEnded { auto: None },
    );
    assert_eq!(
        serde_json::to_value(LifecycleSignal::CompactionEnded { auto: None }).unwrap(),
        serde_json::json!({ "signal": "compaction_ended" })
    );
}
