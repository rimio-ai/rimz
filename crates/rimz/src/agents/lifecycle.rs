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
use serde_json::Value;

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
    /// A subagent finished (`SubagentStop`) — child entity returns to idle.
    SubagentStopped,
    /// A tool completed (`PostToolUse`). Adapters emit this only for a
    /// *mutating* tool, so it doubles as proof the agent is doing real work —
    /// which is why it can reconcile a rollup that wrongly thinks the agent is
    /// resting. `edits` marks the file-editing subset (Claude `Edit`/`Write`/…,
    /// Codex `apply_patch`): the first edit of a turn ends the thinking head.
    /// Defaulted so a `tool_used` event written before the bit existed still
    /// replays.
    ToolUsed {
        mutates: bool,
        #[serde(default)]
        edits: bool,
    },
    /// The agent began compacting its context window (Claude `PreCompact`,
    /// Codex `SessionStart:compact`). A transient head, not a status change.
    Compacting,
    /// The session ended (Claude `SessionEnd`/`offline`). Handled as removal by
    /// the reducer's tombstone path, so it is never routed through [`step`];
    /// the variant exists only so an adapter can name the event.
    Ended,
}

/// The small reduced lifecycle state [`step`] owns — and the only fields it
/// writes. Everything else on the agent rollup (identity, task, prompt, model,
/// gauges, worktree, parent link, timestamps) is governed by the reducer's
/// field lifetimes, untouched by the state machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleState {
    pub status: AgentStatus,
    /// Whether the running turn is still in its pre-edit reasoning phase — a
    /// transient head the sidebar paints as the thinking sparkle. Set by a turn
    /// (or subagent) start, cleared by the turn's first file-editing tool or
    /// its end; only ever rendered while `status == Running`.
    pub thinking: bool,
    /// Whether the agent is mid-compaction — a transient head the sidebar
    /// paints, cleared by the next non-compaction signal.
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
    let was_compacting = prev.is_some_and(|p| p.compacting);
    let was_thinking = prev.is_some_and(|p| p.thinking);
    let mut kind = TransitionKind::Normal;

    // `Ended` is handled as removal upstream and should never be stepped; if it
    // reaches here, keep prior state intact and flag the no-op.
    if matches!(signal, LifecycleSignal::Ended) {
        return Transition {
            next: LifecycleState {
                status: prior_status.unwrap_or(AgentStatus::Idle),
                thinking: was_thinking,
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

    // The thinking head: a fresh turn (or child task) opens in its reasoning
    // phase; the turn's first file-editing tool ends it; any turn boundary
    // clears it. A non-editing mutating tool (a shell command) is work, but the
    // turn has still written nothing — the head carries forward. Compaction
    // preserves it like it preserves the status.
    let thinking = match signal {
        LifecycleSignal::TurnStarted | LifecycleSignal::SubagentStarted => true,
        LifecycleSignal::ToolUsed { edits, .. } => was_thinking && !edits,
        LifecycleSignal::Compacting => was_thinking,
        LifecycleSignal::Registered
        | LifecycleSignal::TurnEnded { .. }
        | LifecycleSignal::SubagentStopped => false,
        // Handled above.
        LifecycleSignal::Ended => unreachable!("Ended returns early"),
    };

    let status = match signal {
        LifecycleSignal::Registered => AgentStatus::Idle,
        LifecycleSignal::TurnStarted | LifecycleSignal::SubagentStarted => AgentStatus::Running,
        LifecycleSignal::SubagentStopped => AgentStatus::Idle,
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
        // Handled above.
        LifecycleSignal::Ended => unreachable!("Ended returns early"),
    };

    Transition {
        next: LifecycleState {
            status,
            thinking,
            compacting,
        },
        kind,
    }
}

/// Decode the lifecycle signal an `agent.lifecycle` event carries. A current
/// (schema v2+) event stores an explicit `signal`; a legacy (v1) event stored a
/// bare `status` plus a `compacting` flag, which this reconstructs into the
/// closest signal so an on-disk event log replays without a rewrite. Deletable
/// once no v1 log can still be active.
pub(crate) fn signal_from_event_params(params: &Value, event_name: &str) -> LifecycleSignal {
    if let Some(signal) = params
        .get("signal")
        .and_then(|value| LifecycleSignal::deserialize(value).ok())
    {
        return signal;
    }
    // Legacy reconstruction.
    if params
        .get("compacting")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return LifecycleSignal::Compacting;
    }
    let status = params.get("status").and_then(Value::as_str);
    match (event_name, status) {
        ("SessionEnd", _) | (_, Some("offline")) => LifecycleSignal::Ended,
        ("SessionStart", _) => LifecycleSignal::Registered,
        ("UserPromptSubmit", _) => LifecycleSignal::TurnStarted,
        ("SubagentStart", _) => LifecycleSignal::SubagentStarted,
        ("SubagentStop", _) => LifecycleSignal::SubagentStopped,
        ("Stop", status) => LifecycleSignal::TurnEnded {
            errored: status == Some("failed"),
            parked_on_background: status == Some("running"),
        },
        (_, Some("idle")) => LifecycleSignal::Registered,
        (_, Some("success")) => LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
        (_, Some("failed")) => LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
        },
        // A bare legacy `running` (e.g. a recorded tool event) keeps the agent
        // running without claiming a turn boundary.
        _ => LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: AgentStatus, thinking: bool, compacting: bool) -> LifecycleState {
        LifecycleState {
            status,
            thinking,
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
        assert!(!t.next.thinking);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn turn_started_is_running_and_thinking() {
        let prev = state(AgentStatus::Idle, false, false);
        let t = step(Some(&prev), &LifecycleSignal::TurnStarted);
        assert_eq!(t.next.status, AgentStatus::Running);
        assert!(t.next.thinking, "a fresh turn opens in its reasoning phase");
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn first_file_edit_ends_thinking() {
        let prev = state(AgentStatus::Running, true, false);
        let t = step(Some(&prev), &tool(true));
        assert_eq!(t.next.status, AgentStatus::Running);
        assert!(
            !t.next.thinking,
            "the turn's first edit flips it to working"
        );
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn non_editing_tool_keeps_thinking() {
        // A shell command is work, but the turn has written nothing yet — the
        // thinking head carries forward until a real file edit.
        let prev = state(AgentStatus::Running, true, false);
        let t = step(Some(&prev), &tool(false));
        assert_eq!(t.next.status, AgentStatus::Running);
        assert!(t.next.thinking);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn thinking_stays_cleared_for_the_rest_of_the_turn() {
        let prev = state(AgentStatus::Running, false, false);
        let t = step(Some(&prev), &tool(false));
        assert!(!t.next.thinking, "a cleared head never re-arms mid-turn");
    }

    #[test]
    fn clean_turn_end_is_success_and_clears_thinking() {
        let prev = state(AgentStatus::Running, true, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
        );
        assert_eq!(t.next.status, AgentStatus::Success);
        assert!(!t.next.thinking);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn errored_turn_end_is_failed() {
        let prev = state(AgentStatus::Running, false, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        );
        assert_eq!(t.next.status, AgentStatus::Failed);
    }

    #[test]
    fn background_park_stays_running_normal_without_thinking() {
        let prev = state(AgentStatus::Running, true, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: true,
            },
        );
        // A designed edge — running, no log noise. The foreground reasoning is
        // done (the turn parked on background work), so the thinking head drops.
        assert_eq!(t.next.status, AgentStatus::Running);
        assert!(!t.next.thinking);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn errored_wins_over_background_park() {
        let prev = state(AgentStatus::Running, false, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: true,
            },
        );
        assert_eq!(t.next.status, AgentStatus::Failed);
    }

    #[test]
    fn subagent_start_running_thinking_stop_idle() {
        let start = step(None, &LifecycleSignal::SubagentStarted);
        assert_eq!(start.next.status, AgentStatus::Running);
        assert!(start.next.thinking, "a child task opens reasoning too");
        let stop = step(Some(&start.next), &LifecycleSignal::SubagentStopped);
        assert_eq!(stop.next.status, AgentStatus::Idle);
        assert!(!stop.next.thinking);
    }

    #[test]
    fn compacting_keeps_status_and_thinking_and_sets_head() {
        let prev = state(AgentStatus::Running, true, false);
        let t = step(Some(&prev), &LifecycleSignal::Compacting);
        assert_eq!(t.next.status, AgentStatus::Running);
        assert!(t.next.thinking, "compaction preserves the turn phase");
        assert!(t.next.compacting);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn any_signal_clears_compacting_head() {
        let prev = state(AgentStatus::Running, false, true);
        let t = step(Some(&prev), &LifecycleSignal::TurnStarted);
        assert!(!t.next.compacting);
    }

    #[test]
    fn tool_while_resting_reconciles_to_running() {
        let prev = state(AgentStatus::Idle, false, false);
        let t = step(Some(&prev), &tool(true));
        assert_eq!(t.next.status, AgentStatus::Running);
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
        let prev = state(AgentStatus::Running, true, false);
        let t = step(Some(&prev), &LifecycleSignal::Ended);
        assert_eq!(t.next.status, AgentStatus::Running);
        assert!(t.next.thinking);
        assert!(matches!(t.kind, TransitionKind::Ignored { .. }));
    }

    #[test]
    fn none_prev_never_panics_for_any_signal() {
        for signal in [
            LifecycleSignal::Registered,
            LifecycleSignal::TurnStarted,
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            LifecycleSignal::SubagentStarted,
            LifecycleSignal::SubagentStopped,
            tool(true),
            LifecycleSignal::Compacting,
            LifecycleSignal::Ended,
        ] {
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
    fn decodes_an_explicit_signal_from_params() {
        let params = serde_json::json!({
            "signal": { "signal": "turn_ended", "errored": true, "parked_on_background": false },
            "status": "running",
        });
        assert_eq!(
            signal_from_event_params(&params, "Stop"),
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
        );
    }

    #[test]
    fn reconstructs_signal_from_legacy_status_params() {
        let cases = [
            (
                serde_json::json!({ "status": "idle" }),
                "SessionStart",
                LifecycleSignal::Registered,
            ),
            (
                serde_json::json!({ "status": "running" }),
                "UserPromptSubmit",
                LifecycleSignal::TurnStarted,
            ),
            (
                serde_json::json!({ "status": "running" }),
                "SubagentStart",
                LifecycleSignal::SubagentStarted,
            ),
            (
                serde_json::json!({ "status": "success" }),
                "Stop",
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                },
            ),
            (
                serde_json::json!({ "status": "running" }),
                "Stop",
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: true,
                },
            ),
            (
                serde_json::json!({ "status": "running", "compacting": true }),
                "PreCompact",
                LifecycleSignal::Compacting,
            ),
        ];
        for (params, event, expected) in cases {
            assert_eq!(
                signal_from_event_params(&params, event),
                expected,
                "{event}"
            );
        }
    }
}
