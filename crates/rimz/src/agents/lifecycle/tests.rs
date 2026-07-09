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

fn assert_step(
    name: &str,
    prev: Option<LifecycleState>,
    signal: LifecycleSignal,
    next: LifecycleState,
    kind: TransitionKind,
    compaction_closed: bool,
    opened_turn: bool,
) {
    let transition = step(prev.as_ref(), &signal);
    assert_eq!(transition.next, next, "{name}: next");
    assert_eq!(transition.kind, kind, "{name}: kind");
    assert_eq!(
        transition.compaction_closed, compaction_closed,
        "{name}: compaction_closed"
    );
    assert_eq!(transition.opened_turn, opened_turn, "{name}: opened_turn");
}

#[test]
fn core_turn_and_subagent_edges_follow_the_contract() {
    let idle = state(AgentStatus::Idle, TurnPhase::Idle, false);
    let reasoning = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let acting = state(AgentStatus::Running, TurnPhase::Acting, false);
    let parked = state(AgentStatus::Running, TurnPhase::Parked, false);

    for (name, prev, signal, next, kind, opened_turn) in [
        (
            "registration starts idle",
            None,
            LifecycleSignal::Registered,
            idle,
            TransitionKind::Normal,
            false,
        ),
        (
            "turn starts reasoning",
            Some(idle),
            LifecycleSignal::TurnStarted,
            reasoning,
            TransitionKind::Normal,
            true,
        ),
        (
            "non-editing tool keeps reasoning",
            Some(reasoning),
            tool(false),
            reasoning,
            TransitionKind::Normal,
            false,
        ),
        (
            "first file edit moves to acting",
            Some(reasoning),
            tool(true),
            acting,
            TransitionKind::Normal,
            false,
        ),
        (
            "acting does not re-arm reasoning",
            Some(acting),
            tool(false),
            acting,
            TransitionKind::Normal,
            false,
        ),
        (
            "clean turn end succeeds",
            Some(reasoning),
            turn_end(false, false),
            state(AgentStatus::Success, TurnPhase::Idle, false),
            TransitionKind::Normal,
            false,
        ),
        (
            "errored turn end fails",
            Some(acting),
            turn_end(true, false),
            state(AgentStatus::Failed, TurnPhase::Idle, false),
            TransitionKind::Normal,
            false,
        ),
        (
            "background park stays running",
            Some(reasoning),
            turn_end(false, true),
            parked,
            TransitionKind::Normal,
            false,
        ),
        (
            "tool after park resumes acting",
            Some(parked),
            tool(false),
            acting,
            TransitionKind::Normal,
            false,
        ),
        (
            "wake after park resumes the turn",
            Some(parked),
            LifecycleSignal::TurnStarted,
            reasoning,
            TransitionKind::Normal,
            false,
        ),
        (
            "prompt on a live turn still re-stamps",
            Some(reasoning),
            LifecycleSignal::TurnStarted,
            reasoning,
            TransitionKind::Normal,
            true,
        ),
        (
            "subagent starts reasoning",
            None,
            LifecycleSignal::SubagentStarted,
            reasoning,
            TransitionKind::Normal,
            true,
        ),
        (
            "subagent stops clean",
            Some(reasoning),
            LifecycleSignal::SubagentStopped { errored: false },
            state(AgentStatus::Success, TurnPhase::Idle, false),
            TransitionKind::Normal,
            false,
        ),
        (
            "subagent stop can fail",
            Some(reasoning),
            LifecycleSignal::SubagentStopped { errored: true },
            state(AgentStatus::Failed, TurnPhase::Idle, false),
            TransitionKind::Normal,
            false,
        ),
        (
            "tool while resting reconciles to running",
            Some(idle),
            tool(true),
            acting,
            TransitionKind::Reconciled {
                from: AgentStatus::Idle,
                reason: "tool used outside a running turn",
            },
            true,
        ),
        (
            "non-mutating tool clears waiting",
            Some(state(AgentStatus::Waiting, TurnPhase::Idle, false)),
            LifecycleSignal::ToolUsed {
                mutates: false,
                edits: false,
            },
            state(AgentStatus::Running, TurnPhase::Acting, false),
            TransitionKind::Normal,
            true,
        ),
    ] {
        assert_step(name, prev, signal, next, kind, false, opened_turn);
    }

    let waiting_resume = step(
        Some(&state(AgentStatus::Waiting, TurnPhase::Idle, false)),
        &LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        },
    );
    assert!(waiting_resume.waiting_cleared);

    let ended = step(Some(&reasoning), &LifecycleSignal::Ended);
    assert_eq!(ended.next, reasoning);
    assert!(matches!(ended.kind, TransitionKind::Ignored { .. }));
    assert!(!ended.compaction_closed);
    assert!(!ended.opened_turn);

    let lost = step(Some(&reasoning), &LifecycleSignal::Lost);
    assert_eq!(lost.next, reasoning);
    assert!(matches!(lost.kind, TransitionKind::Ignored { .. }));
    assert!(!lost.compaction_closed);
    assert!(!lost.opened_turn);
}

#[test]
fn compaction_edges_keep_the_head_orthogonal_to_status_and_phase() {
    let running_reasoning = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let running_acting = state(AgentStatus::Running, TurnPhase::Acting, false);
    let running_parked = state(AgentStatus::Running, TurnPhase::Parked, false);

    for (name, prev, signal, next, kind, compaction_closed, opened_turn) in [
        (
            "compacting preserves reasoning",
            Some(running_reasoning),
            LifecycleSignal::Compacting,
            state(AgentStatus::Running, TurnPhase::Reasoning, true),
            TransitionKind::Normal,
            false,
            false,
        ),
        (
            "compacting preserves parked",
            Some(running_parked),
            LifecycleSignal::Compacting,
            state(AgentStatus::Running, TurnPhase::Parked, true),
            TransitionKind::Normal,
            false,
            false,
        ),
        (
            "auto compaction resumes from idle",
            Some(state(AgentStatus::Idle, TurnPhase::Idle, true)),
            LifecycleSignal::CompactionEnded { auto: Some(true) },
            state(AgentStatus::Running, TurnPhase::Reasoning, false),
            TransitionKind::Reconciled {
                from: AgentStatus::Idle,
                reason: "auto-compaction resumed a turn",
            },
            true,
            true,
        ),
        (
            "auto compaction keeps running phase",
            Some(state(AgentStatus::Running, TurnPhase::Acting, true)),
            LifecycleSignal::CompactionEnded { auto: Some(true) },
            running_acting,
            TransitionKind::Normal,
            true,
            false,
        ),
        (
            "auto compaction leaves attention status",
            Some(state(AgentStatus::Failed, TurnPhase::Idle, true)),
            LifecycleSignal::CompactionEnded { auto: Some(true) },
            state(AgentStatus::Failed, TurnPhase::Idle, false),
            TransitionKind::Normal,
            true,
            false,
        ),
        (
            "manual compaction rests",
            Some(state(AgentStatus::Running, TurnPhase::Acting, true)),
            LifecycleSignal::CompactionEnded { auto: Some(false) },
            state(AgentStatus::Idle, TurnPhase::Idle, false),
            TransitionKind::Normal,
            true,
            false,
        ),
        (
            "unknown compaction end only clears head",
            Some(state(AgentStatus::Running, TurnPhase::Reasoning, true)),
            LifecycleSignal::CompactionEnded { auto: None },
            running_reasoning,
            TransitionKind::Normal,
            true,
            false,
        ),
        (
            "unbracketed unknown close is ignored",
            Some(running_reasoning),
            LifecycleSignal::CompactionEnded { auto: None },
            running_reasoning,
            TransitionKind::Ignored {
                reason: "compaction end without an open bracket",
            },
            false,
            false,
        ),
        (
            "unbracketed manual close still applies",
            Some(running_acting),
            LifecycleSignal::CompactionEnded { auto: Some(false) },
            state(AgentStatus::Idle, TurnPhase::Idle, false),
            TransitionKind::Normal,
            false,
            false,
        ),
        (
            "next non-compaction signal closes open head",
            Some(state(AgentStatus::Running, TurnPhase::Acting, true)),
            LifecycleSignal::TurnStarted,
            running_reasoning,
            TransitionKind::Normal,
            true,
            true,
        ),
        (
            "parked wake closes open head without re-stamping",
            Some(state(AgentStatus::Running, TurnPhase::Parked, true)),
            LifecycleSignal::TurnStarted,
            running_reasoning,
            TransitionKind::Normal,
            true,
            false,
        ),
        (
            "compacting while compacting does not close",
            Some(state(AgentStatus::Running, TurnPhase::Reasoning, true)),
            LifecycleSignal::Compacting,
            state(AgentStatus::Running, TurnPhase::Reasoning, true),
            TransitionKind::Normal,
            false,
            false,
        ),
    ] {
        assert_step(
            name,
            prev,
            signal,
            next,
            kind,
            compaction_closed,
            opened_turn,
        );
    }
}

#[test]
fn state_machine_is_total_and_keeps_phase_axis_valid() {
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
    let signals = all_signals();

    for signal in &signals {
        let _ = step(None, signal);
    }

    for status in statuses {
        for phase in phases {
            for compacting in [false, true] {
                let prev = state(status, phase, compacting);
                for signal in &signals {
                    let transition = step(Some(&prev), signal);
                    if !matches!(signal, LifecycleSignal::Ended | LifecycleSignal::Lost)
                        && transition.next.status != AgentStatus::Running
                    {
                        assert_eq!(
                            transition.next.phase,
                            TurnPhase::Idle,
                            "{status:?}/{phase:?}/{compacting} + {signal:?}"
                        );
                    }
                    if !compacting {
                        assert!(
                            !transition.compaction_closed,
                            "{signal:?} must not close an absent bracket"
                        );
                    }
                    if matches!(signal, LifecycleSignal::TurnStarted) {
                        assert_eq!(
                            transition.opened_turn,
                            !(status == AgentStatus::Running && phase == TurnPhase::Parked),
                            "{status:?}/{phase:?}/{compacting} + {signal:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn lifecycle_wire_tags_and_legacy_defaults_are_stable() {
    for signal in all_signals() {
        let wire = serde_json::to_value(&signal).unwrap();
        assert_eq!(wire["signal"], signal.tag(), "{signal:?}");
        let back: LifecycleSignal = serde_json::from_value(wire).unwrap();
        assert_eq!(signal, back);
    }

    for phase in [
        TurnPhase::Idle,
        TurnPhase::Reasoning,
        TurnPhase::Acting,
        TurnPhase::Parked,
    ] {
        let wire = serde_json::to_value(phase).unwrap();
        let back: TurnPhase = serde_json::from_value(wire).unwrap();
        assert_eq!(phase, back);
    }
    assert_eq!(
        serde_json::to_value(TurnPhase::Reasoning).unwrap(),
        serde_json::json!("reasoning"),
    );

    let tool_without_edits: LifecycleSignal =
        serde_json::from_value(serde_json::json!({ "signal": "tool_used", "mutates": true }))
            .unwrap();
    assert_eq!(
        tool_without_edits,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
        }
    );

    let subagent_without_error: LifecycleSignal =
        serde_json::from_value(serde_json::json!({ "signal": "subagent_stopped" })).unwrap();
    assert_eq!(
        subagent_without_error,
        LifecycleSignal::SubagentStopped { errored: false }
    );

    let compaction_without_auto: LifecycleSignal =
        serde_json::from_value(serde_json::json!({ "signal": "compaction_ended" })).unwrap();
    assert_eq!(
        compaction_without_auto,
        LifecycleSignal::CompactionEnded { auto: None }
    );
    assert_eq!(
        serde_json::to_value(compaction_without_auto).unwrap(),
        serde_json::json!({ "signal": "compaction_ended" })
    );
}
