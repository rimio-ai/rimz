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

#[test]
fn registered_is_idle() {
    let t = step(None, &LifecycleSignal::Registered);
    assert_eq!(t.next.status, AgentStatus::Idle);
    assert_eq!(t.next.phase, TurnPhase::Idle);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn turn_started_opens_reasoning() {
    let prev = state(AgentStatus::Idle, TurnPhase::Idle, false);
    let t = step(Some(&prev), &LifecycleSignal::TurnStarted);
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(
        t.next.phase,
        TurnPhase::Reasoning,
        "a fresh turn opens in its reasoning phase"
    );
    assert_eq!(t.kind, TransitionKind::Normal);
    assert!(t.opened_turn);
}

#[test]
fn first_file_edit_moves_to_acting() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let t = step(Some(&prev), &tool(true));
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(
        t.next.phase,
        TurnPhase::Acting,
        "the turn's first edit flips it to working"
    );
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn non_editing_tool_keeps_reasoning() {
    // A shell command is work, but the turn has written nothing yet — the
    // reasoning phase carries forward until a real file edit.
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let t = step(Some(&prev), &tool(false));
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(t.next.phase, TurnPhase::Reasoning);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn acting_never_rearms_to_reasoning() {
    let prev = state(AgentStatus::Running, TurnPhase::Acting, false);
    let t = step(Some(&prev), &tool(false));
    assert_eq!(
        t.next.phase,
        TurnPhase::Acting,
        "a phase that left reasoning never re-arms mid-turn"
    );
}

#[test]
fn clean_turn_end_is_success_and_rests_phase() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let t = step(
        Some(&prev),
        &LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    );
    assert_eq!(t.next.status, AgentStatus::Success);
    assert_eq!(t.next.phase, TurnPhase::Idle);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn errored_turn_end_is_failed() {
    let prev = state(AgentStatus::Running, TurnPhase::Acting, false);
    let t = step(
        Some(&prev),
        &LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        },
    );
    assert_eq!(t.next.status, AgentStatus::Failed);
    assert_eq!(t.next.phase, TurnPhase::Idle);
}

#[test]
fn background_park_stays_running_in_parked_phase() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let t = step(
        Some(&prev),
        &LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
        },
    );
    // A designed edge — running, no log noise. The foreground reasoning is
    // done (the turn parked on background work), so the phase is the park.
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(t.next.phase, TurnPhase::Parked);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn errored_wins_over_background_park() {
    let prev = state(AgentStatus::Running, TurnPhase::Acting, false);
    let t = step(
        Some(&prev),
        &LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: true,
        },
    );
    assert_eq!(t.next.status, AgentStatus::Failed);
    assert_eq!(t.next.phase, TurnPhase::Idle);
}

#[test]
fn tool_after_park_resumes_acting() {
    // A parked turn that completes a tool is visibly back at work — the
    // background marker drops in favor of the working spinner.
    let prev = state(AgentStatus::Running, TurnPhase::Parked, false);
    let t = step(Some(&prev), &tool(false));
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(t.next.phase, TurnPhase::Acting);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn subagent_start_reasoning_stop_clean_is_success() {
    let start = step(None, &LifecycleSignal::SubagentStarted);
    assert_eq!(start.next.status, AgentStatus::Running);
    assert_eq!(
        start.next.phase,
        TurnPhase::Reasoning,
        "a child task opens reasoning too"
    );
    let stop = step(
        Some(&start.next),
        &LifecycleSignal::SubagentStopped { errored: false },
    );
    assert_eq!(stop.next.status, AgentStatus::Success);
    assert_eq!(stop.next.phase, TurnPhase::Idle);
}

#[test]
fn subagent_stop_errored_is_failed() {
    let start = step(None, &LifecycleSignal::SubagentStarted);
    let stop = step(
        Some(&start.next),
        &LifecycleSignal::SubagentStopped { errored: true },
    );
    assert_eq!(stop.next.status, AgentStatus::Failed);
    assert_eq!(stop.next.phase, TurnPhase::Idle);
}

#[test]
fn compacting_keeps_status_and_phase_and_sets_head() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let t = step(Some(&prev), &LifecycleSignal::Compacting);
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(
        t.next.phase,
        TurnPhase::Reasoning,
        "compaction preserves the turn phase"
    );
    assert!(t.next.compacting);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn compacting_preserves_a_parked_turn() {
    // Compaction is a head over the state, not a transition: an agent that
    // parked on background work is still parked when the head lifts.
    let prev = state(AgentStatus::Running, TurnPhase::Parked, false);
    let t = step(Some(&prev), &LifecycleSignal::Compacting);
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(t.next.phase, TurnPhase::Parked);
    assert!(t.next.compacting);
}

#[test]
fn compaction_ended_auto_resumes_running_from_idle() {
    let prev = state(AgentStatus::Idle, TurnPhase::Idle, true);
    let t = step(
        Some(&prev),
        &LifecycleSignal::CompactionEnded { auto: Some(true) },
    );
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(t.next.phase, TurnPhase::Idle);
    assert!(!t.next.compacting);
    assert!(t.compaction_closed);
    assert_eq!(
        t.kind,
        TransitionKind::Reconciled {
            from: AgentStatus::Idle,
            reason: "auto-compaction resumed a turn",
        }
    );
    assert!(t.opened_turn);
}

#[test]
fn compaction_ended_auto_keeps_running_and_carries_phase() {
    let prev = state(AgentStatus::Running, TurnPhase::Acting, true);
    let t = step(
        Some(&prev),
        &LifecycleSignal::CompactionEnded { auto: Some(true) },
    );
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(
        t.next.phase,
        TurnPhase::Acting,
        "an auto compact resumes the interrupted turn phase"
    );
    assert!(!t.next.compacting);
    assert!(t.compaction_closed);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn compaction_ended_auto_carries_reasoning_phase() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, true);
    let t = step(
        Some(&prev),
        &LifecycleSignal::CompactionEnded { auto: Some(true) },
    );
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(t.next.phase, TurnPhase::Reasoning);
    assert!(!t.next.compacting);
}

#[test]
fn compaction_ended_auto_leaves_attention_status() {
    let prev = state(AgentStatus::Failed, TurnPhase::Idle, true);
    let t = step(
        Some(&prev),
        &LifecycleSignal::CompactionEnded { auto: Some(true) },
    );
    assert_eq!(t.next.status, AgentStatus::Failed);
    assert_eq!(t.next.phase, TurnPhase::Idle);
    assert!(!t.next.compacting);
    assert!(t.compaction_closed);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn compaction_ended_manual_rests_to_idle() {
    let prev = state(AgentStatus::Running, TurnPhase::Acting, true);
    let t = step(
        Some(&prev),
        &LifecycleSignal::CompactionEnded { auto: Some(false) },
    );
    assert_eq!(t.next.status, AgentStatus::Idle);
    assert_eq!(t.next.phase, TurnPhase::Idle);
    assert!(!t.next.compacting);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn compaction_ended_unknown_preserves_state_and_phase() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, true);
    let t = step(
        Some(&prev),
        &LifecycleSignal::CompactionEnded { auto: None },
    );
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(
        t.next.phase,
        TurnPhase::Reasoning,
        "a provider without the manual/auto bit only clears the head"
    );
    assert!(!t.next.compacting);
    assert!(t.compaction_closed);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn compaction_ended_clears_compacting_head() {
    for auto in [None, Some(false), Some(true)] {
        let prev = state(AgentStatus::Running, TurnPhase::Reasoning, true);
        let t = step(Some(&prev), &LifecycleSignal::CompactionEnded { auto });
        assert!(!t.next.compacting, "{auto:?}");
        assert!(t.compaction_closed, "{auto:?}");
    }
}

#[test]
fn any_signal_clears_compacting_head() {
    let prev = state(AgentStatus::Running, TurnPhase::Acting, true);
    let t = step(Some(&prev), &LifecycleSignal::TurnStarted);
    assert!(!t.next.compacting);
    assert!(t.compaction_closed);
}

#[test]
fn unbracketed_compaction_end_is_ignored_when_it_changes_nothing() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let t = step(
        Some(&prev),
        &LifecycleSignal::CompactionEnded { auto: None },
    );
    assert_eq!(t.next, prev);
    assert!(!t.compaction_closed);
    assert_eq!(
        t.kind,
        TransitionKind::Ignored {
            reason: "compaction end without an open bracket",
        }
    );
}

#[test]
fn unbracketed_manual_compaction_end_applies_without_counting() {
    let prev = state(AgentStatus::Running, TurnPhase::Acting, false);
    let t = step(
        Some(&prev),
        &LifecycleSignal::CompactionEnded { auto: Some(false) },
    );
    assert_eq!(t.next.status, AgentStatus::Idle);
    assert_eq!(t.next.phase, TurnPhase::Idle);
    assert!(!t.compaction_closed);
    assert_eq!(t.kind, TransitionKind::Normal);
}

#[test]
fn compacting_while_compacting_restamps_without_closing() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, true);
    let t = step(Some(&prev), &LifecycleSignal::Compacting);
    assert!(t.next.compacting);
    assert!(!t.compaction_closed);
}

#[test]
fn ended_carries_open_compaction_unclosed() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, true);
    let t = step(Some(&prev), &LifecycleSignal::Ended);
    assert!(t.next.compacting);
    assert!(!t.compaction_closed);
    assert!(matches!(t.kind, TransitionKind::Ignored { .. }));
}

#[test]
fn no_signal_closes_without_an_open_bracket() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    for signal in all_signals() {
        let t = step(Some(&prev), &signal);
        assert!(
            !t.compaction_closed,
            "{signal:?} must not close an absent bracket"
        );
    }
}

#[test]
fn tool_while_resting_reconciles_to_running() {
    let prev = state(AgentStatus::Idle, TurnPhase::Idle, false);
    let t = step(Some(&prev), &tool(true));
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(t.next.phase, TurnPhase::Acting);
    assert_eq!(
        t.kind,
        TransitionKind::Reconciled {
            from: AgentStatus::Idle,
            reason: "tool used outside a running turn",
        }
    );
    assert!(t.opened_turn);
}

#[test]
fn opened_turn_tracks_explicit_reconcile_and_auto_resume_edges() {
    let running = state(AgentStatus::Running, TurnPhase::Acting, false);
    assert!(step(Some(&running), &LifecycleSignal::TurnStarted).opened_turn);

    let idle = state(AgentStatus::Idle, TurnPhase::Idle, false);
    assert!(step(Some(&idle), &tool(false)).opened_turn);
    assert!(
        step(
            Some(&idle),
            &LifecycleSignal::CompactionEnded { auto: Some(true) },
        )
        .opened_turn
    );

    assert!(!step(Some(&running), &tool(false)).opened_turn);
    assert!(
        !step(
            Some(&running),
            &LifecycleSignal::CompactionEnded { auto: Some(true) },
        )
        .opened_turn
    );
}

#[test]
fn ended_is_ignored_and_preserves_state() {
    let prev = state(AgentStatus::Running, TurnPhase::Reasoning, false);
    let t = step(Some(&prev), &LifecycleSignal::Ended);
    assert_eq!(t.next.status, AgentStatus::Running);
    assert_eq!(t.next.phase, TurnPhase::Reasoning);
    assert!(matches!(t.kind, TransitionKind::Ignored { .. }));
}

/// Every signal stepped from every reachable `(status, phase)` pair: the
/// machine is total, and a non-running result never carries a phase.
#[test]
fn resting_status_never_carries_a_phase() {
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
    for status in statuses {
        for phase in phases {
            for compacting in [false, true] {
                let prev = state(status, phase, compacting);
                for signal in all_signals() {
                    let t = step(Some(&prev), &signal);
                    // `Ended` is the explicit no-op carry; everything else
                    // upholds the invariant by construction.
                    if !matches!(signal, LifecycleSignal::Ended)
                        && t.next.status != AgentStatus::Running
                    {
                        assert_eq!(
                            t.next.phase,
                            TurnPhase::Idle,
                            "{status:?}/{phase:?} + {signal:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn none_prev_never_panics_for_any_signal() {
    for signal in all_signals() {
        let _ = step(None, &signal);
    }
}

#[test]
fn signal_round_trips_through_json() {
    let signal = LifecycleSignal::ToolUsed {
        mutates: true,
        edits: true,
    };
    let wire = serde_json::to_value(signal).unwrap();
    assert_eq!(wire["signal"], "tool_used");
    assert_eq!(wire["edits"], true);
    let back: LifecycleSignal = serde_json::from_value(wire).unwrap();
    assert_eq!(signal, back);
}

#[test]
fn signal_tags_match_json_discriminants() {
    for signal in all_signals() {
        let wire = serde_json::to_value(signal).unwrap();
        assert_eq!(wire["signal"], signal.tag(), "{signal:?}");
    }
}

#[test]
fn turn_phase_round_trips_through_json() {
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
}

#[test]
fn tool_used_without_edits_bit_still_deserializes() {
    // Events written before the `edits` bit existed carry only `mutates`;
    // the missing field defaults to false so an old log replays.
    let wire = serde_json::json!({ "signal": "tool_used", "mutates": true });
    let signal: LifecycleSignal = serde_json::from_value(wire).unwrap();
    assert_eq!(
        signal,
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: false,
        }
    );
}

#[test]
fn subagent_stopped_without_errored_bit_still_deserializes() {
    // Events written before the `errored` bit existed carry the bare tag;
    // the missing field defaults to false so an old log replays as clean.
    let wire = serde_json::json!({ "signal": "subagent_stopped" });
    let signal: LifecycleSignal = serde_json::from_value(wire).unwrap();
    assert_eq!(signal, LifecycleSignal::SubagentStopped { errored: false });
}

#[test]
fn compaction_ended_without_auto_bit_still_deserializes() {
    let wire = serde_json::json!({ "signal": "compaction_ended" });
    let signal: LifecycleSignal = serde_json::from_value(wire).unwrap();
    assert_eq!(signal, LifecycleSignal::CompactionEnded { auto: None });
    let encoded = serde_json::to_value(signal).unwrap();
    assert_eq!(encoded, serde_json::json!({ "signal": "compaction_ended" }));
}
