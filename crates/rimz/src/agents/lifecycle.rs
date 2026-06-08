//! The agent lifecycle state machine.
//!
//! This module is the single home for the directed transition graph that folds
//! a [`LifecycleSignal`] — the agent-agnostic *intent* an adapter reads off a
//! native hook event — onto the reduced [`LifecycleState`]. Both the snapshot
//! reducer (silently, on replay) and the hook ingestion path (with one-shot
//! anomaly logging) call the one pure [`step`] function, so the transition
//! table lives in exactly one place. See
//! [docs/internals/agent.md](../../../../docs/internals/agent.md).
//!
//! `step` is reused identically for a root agent and a subagent: the two levels
//! differ only in which signals are legal and how the entity is keyed, never in
//! the transition itself. The function is pure and total — an unexpected
//! `(state, signal)` pair never panics. It takes the signal's natural edge (the
//! agent is authoritative about its own activity) and tags the result
//! [`TransitionKind::Reconciled`] so the caller can log the drift rather than
//! silently absorbing it.

use serde::{Deserialize, Serialize};

use crate::feed::AgentStatus;

/// The agent-agnostic intent each native lifecycle event carries. An adapter's
/// `observe_lifecycle` maps a native event onto exactly one of these (plus the
/// enrichment); it no longer decides an [`AgentStatus`].
///
/// Wire format is internally tagged on `signal` (snake_case), e.g.
/// `{"signal":"turn_ended","errored":false,"parked_on_background":false}`. It
/// rides the `agent.lifecycle` event params in place of the legacy bare status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum LifecycleSignal {
    /// A session registered fresh (Claude `SessionStart`, Codex first turn).
    Registered,
    /// A user turn began (`UserPromptSubmit`).
    TurnStarted,
    /// A turn ended. `errored` always wins (the failure is the attention
    /// signal); a clean end with `parked_on_background` is the main thread
    /// parking on still-in-flight work, not a true turn end, so it stays
    /// running rather than painting a false success.
    TurnEnded {
        errored: bool,
        parked_on_background: bool,
    },
    /// A subagent began (`SubagentStart`) — only ever observed for a child
    /// entity, keyed by its own child id.
    SubagentStarted,
    /// A subagent finished (`SubagentStop`) — the child entity resolves to
    /// failed when `errored`, else success. Defaulted so a `subagent_stopped`
    /// event written before the bit existed still replays.
    SubagentStopped {
        #[serde(default)]
        errored: bool,
    },
    /// A tool completed (`PostToolUse`). Adapters emit this only for a
    /// *mutating* tool, so it doubles as proof the agent is doing real work —
    /// which is why it can reconcile a rollup that wrongly thinks the agent is
    /// resting. `edits` marks the file-editing subset (Claude `Edit`/`Write`/…,
    /// Codex `apply_patch`): the first edit of a turn moves it from reasoning
    /// to acting. Defaulted so a `tool_used` event written before the bit
    /// existed still replays.
    ToolUsed {
        mutates: bool,
        #[serde(default)]
        edits: bool,
    },
    /// The agent began compacting its context window (Claude `PreCompact`,
    /// Codex `PreCompact`). A transient head, not a status change.
    Compacting,
    /// Context compaction finished (Claude/Codex `PostCompact`, Pi
    /// `session_compact`). The transient compacting head lifts here. When the
    /// provider reports the trigger, automatic compaction resumes the
    /// interrupted turn and manual `/compact` rests to idle. A provider with no
    /// trigger bit clears the head and preserves the prior state.
    CompactionEnded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto: Option<bool>,
    },
    /// The session ended (Claude `SessionEnd`/`offline`). Handled as removal by
    /// the reducer's tombstone path, so it is never routed through [`step`];
    /// the variant exists only so an adapter can name the event.
    Ended,
}

/// The shape of the running turn — the orthogonal axis next to
/// [`AgentStatus`]. One typed value where three independent bools (`thinking`,
/// `parked_on_background`, and the reducer's separate parked derivation) used
/// to live, so the illegal combinations (parked + thinking, a resting agent
/// mid-phase) are unrepresentable. Meaningful only while `status == Running`;
/// every other status carries [`TurnPhase::Idle`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnPhase {
    /// Resting — the phase of every non-running status.
    #[default]
    Idle,
    /// The turn's pre-edit opening: reading, searching, reasoning. The sidebar
    /// paints the thinking sparkle. Set by a turn (or child task) start,
    /// carried through non-editing tools.
    Reasoning,
    /// The turn has begun mutating the workspace — its first file-editing tool
    /// completed. The sidebar paints the working spinner. A phase that left
    /// reasoning never re-arms mid-turn.
    Acting,
    /// The main thread parked on still-in-flight background work after a clean
    /// turn end (Claude Code v2.1.145+). The agent stays running; the sidebar
    /// paints a secondary "background" marker instead of a false success.
    Parked,
}

/// The small reduced lifecycle state [`step`] owns — and the only fields it
/// writes. Everything else on the agent rollup (identity, task, prompt, model,
/// gauges, worktree, parent link, timestamps) is governed by the reducer's
/// field lifetimes, untouched by the state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleState {
    pub status: AgentStatus,
    /// The running turn's shape; [`TurnPhase::Idle`] outside `Running`.
    pub phase: TurnPhase,
    /// Whether the agent is mid-compaction — a transient head the sidebar
    /// paints over any base status, cleared by the next non-compaction signal.
    /// Orthogonal to `phase`: compaction preserves the turn shape it interrupts.
    pub compacting: bool,
}

/// How [`step`] classifies a transition, for observability. The reducer ignores
/// this (it only wants `next`); the ingestion path logs `Reconciled`/`Ignored`
/// once per fresh event under the `rimz::agent::lifecycle` target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransitionKind {
    /// An expected edge in the documented graph.
    Normal,
    /// The pair was a contradiction or otherwise unexpected; the machine took
    /// the safe edge (follow the signal) and records what it overrode and why.
    Reconciled {
        from: AgentStatus,
        reason: &'static str,
    },
    /// A no-op signal for this state — recorded so a flood of them is visible.
    Ignored { reason: &'static str },
}

/// The outcome of one step: the next reduced state and how it was reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub next: LifecycleState,
    pub kind: TransitionKind,
}

/// Fold one [`LifecycleSignal`] onto the prior [`LifecycleState`]. Pure and
/// total: any `(prev, signal)` pair returns a `Transition` and never panics.
pub fn step(prev: Option<&LifecycleState>, signal: &LifecycleSignal) -> Transition {
    let prior_status = prev.map(|p| p.status);
    let prior_phase = prev.map(|p| p.phase).unwrap_or_default();
    let was_compacting = prev.is_some_and(|p| p.compacting);
    let mut kind = TransitionKind::Normal;

    // `Ended` is handled as removal upstream and should never be stepped; if it
    // reaches here, keep prior state intact and flag the no-op.
    if matches!(signal, LifecycleSignal::Ended) {
        return Transition {
            next: LifecycleState {
                status: prior_status.unwrap_or(AgentStatus::Idle),
                phase: prior_phase,
                compacting: was_compacting,
            },
            kind: TransitionKind::Ignored {
                reason: "session ended (handled as removal)",
            },
        };
    }

    // Compaction is the one signal that preserves the prior status; it is a
    // transient head, not a transition. Every other signal clears the head.
    let compacting = matches!(signal, LifecycleSignal::Compacting);

    let status = match signal {
        LifecycleSignal::Registered => AgentStatus::Idle,
        LifecycleSignal::TurnStarted | LifecycleSignal::SubagentStarted => AgentStatus::Running,
        // A finished child resolves to a terminal verdict, so the parent's
        // expanded list reads `✓`/`!` instead of a resting `○`.
        LifecycleSignal::SubagentStopped { errored } => {
            if *errored {
                AgentStatus::Failed
            } else {
                AgentStatus::Success
            }
        }
        LifecycleSignal::TurnEnded {
            errored,
            parked_on_background,
        } => {
            if *errored {
                AgentStatus::Failed
            } else if *parked_on_background {
                // The main thread parked on background work — not a turn end, so
                // the row stays live. A designed edge, tagged `Normal`.
                AgentStatus::Running
            } else {
                AgentStatus::Success
            }
        }
        LifecycleSignal::ToolUsed { .. } => {
            // A completed tool proves the agent is working; if the rollup thinks
            // it is resting (or it is unknown), reconcile to running. Attention
            // states (anything not resting) are left alone.
            match prior_status {
                Some(AgentStatus::Running) => AgentStatus::Running,
                Some(resting @ (AgentStatus::Idle | AgentStatus::Success)) => {
                    kind = TransitionKind::Reconciled {
                        from: resting,
                        reason: "tool used outside a running turn",
                    };
                    AgentStatus::Running
                }
                Some(other) => other,
                None => {
                    kind = TransitionKind::Reconciled {
                        from: AgentStatus::Idle,
                        reason: "tool used before session registered",
                    };
                    AgentStatus::Running
                }
            }
        }
        // Status preserved; only the head is stamped.
        LifecycleSignal::Compacting => prior_status.unwrap_or(AgentStatus::Idle),
        LifecycleSignal::CompactionEnded { auto: Some(true) } => match prior_status {
            Some(AgentStatus::Running) => AgentStatus::Running,
            Some(resting @ (AgentStatus::Idle | AgentStatus::Success)) => {
                kind = TransitionKind::Reconciled {
                    from: resting,
                    reason: "auto-compaction resumed a turn",
                };
                AgentStatus::Running
            }
            Some(other) => other,
            None => {
                kind = TransitionKind::Reconciled {
                    from: AgentStatus::Idle,
                    reason: "auto-compaction resumed a turn",
                };
                AgentStatus::Running
            }
        },
        LifecycleSignal::CompactionEnded { auto: Some(false) } => AgentStatus::Idle,
        LifecycleSignal::CompactionEnded { auto: None } => {
            prior_status.unwrap_or(AgentStatus::Idle)
        }
        // Handled above.
        LifecycleSignal::Ended => unreachable!("Ended returns early"),
    };

    // The turn phase: a fresh turn (or child task) opens reasoning; its first
    // file edit moves it to acting; a clean end parking on background work
    // parks it; any other boundary rests it. Compaction preserves the phase
    // like it preserves the status.
    let phase = match signal {
        LifecycleSignal::TurnStarted | LifecycleSignal::SubagentStarted => TurnPhase::Reasoning,
        // A shell command during the reasoning phase is work, but the turn has
        // still written nothing — the sparkle carries forward. Anywhere else a
        // completed tool is visible work (acting): a phase that left reasoning
        // never re-arms mid-turn, and a parked turn that runs a tool is
        // visibly back at work.
        LifecycleSignal::ToolUsed { edits, .. } => {
            if !edits && prior_phase == TurnPhase::Reasoning {
                TurnPhase::Reasoning
            } else {
                TurnPhase::Acting
            }
        }
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
        } => TurnPhase::Parked,
        LifecycleSignal::Compacting => prior_phase,
        LifecycleSignal::CompactionEnded {
            auto: Some(true) | None,
        } => prior_phase,
        LifecycleSignal::CompactionEnded { auto: Some(false) } => TurnPhase::Idle,
        LifecycleSignal::Registered
        | LifecycleSignal::TurnEnded { .. }
        | LifecycleSignal::SubagentStopped { .. } => TurnPhase::Idle,
        // Handled above.
        LifecycleSignal::Ended => unreachable!("Ended returns early"),
    };
    // The phase axis exists only inside a running turn — a resting or
    // attention status always reads `Idle`, by construction.
    let phase = if status == AgentStatus::Running {
        phase
    } else {
        TurnPhase::Idle
    };

    Transition {
        next: LifecycleState {
            status,
            phase,
            compacting,
        },
        kind,
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(
            t.kind,
            TransitionKind::Reconciled {
                from: AgentStatus::Idle,
                reason: "auto-compaction resumed a turn",
            }
        );
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
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn compaction_ended_clears_compacting_head() {
        for auto in [None, Some(false), Some(true)] {
            let prev = state(AgentStatus::Running, TurnPhase::Reasoning, true);
            let t = step(Some(&prev), &LifecycleSignal::CompactionEnded { auto });
            assert!(!t.next.compacting, "{auto:?}");
        }
    }

    #[test]
    fn any_signal_clears_compacting_head() {
        let prev = state(AgentStatus::Running, TurnPhase::Acting, true);
        let t = step(Some(&prev), &LifecycleSignal::TurnStarted);
        assert!(!t.next.compacting);
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
}
