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

use crate::feed::{AgentStatus, PermissionPosture};

/// The agent-agnostic intent each native lifecycle event carries. An adapter's
/// `observe_lifecycle` maps a native event onto exactly one of these (plus the
/// posture sample and enrichment); it no longer decides an [`AgentStatus`].
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
    /// which is why it can reconcile a stale `plan` posture (plan mode is
    /// read-only) or a rollup that wrongly thinks the agent is resting.
    ToolUsed { mutates: bool },
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
    /// The permission slider, sticky: the latest sample wins and a signal that
    /// carries none keeps the prior value.
    pub posture: PermissionPosture,
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
///
/// `posture_sample` is the slider the originating event reported, if any
/// (`None` means the event named no slider, so the prior posture carries
/// forward). Posture is resolved once, up front, and a reconciliation edge may
/// still override it (a mutating tool proves the agent left read-only plan
/// mode).
pub fn step(
    prev: Option<&LifecycleState>,
    signal: &LifecycleSignal,
    posture_sample: Option<PermissionPosture>,
) -> Transition {
    let prior_status = prev.map(|p| p.status);
    let was_compacting = prev.is_some_and(|p| p.compacting);

    // The slider is sticky: latest sample wins, else carry forward, else the
    // omitted baseline.
    let mut posture = posture_sample
        .or_else(|| prev.map(|p| p.posture))
        .unwrap_or(PermissionPosture::Default);
    let mut kind = TransitionKind::Normal;

    // `Ended` is handled as removal upstream and should never be stepped; if it
    // reaches here, keep prior state intact and flag the no-op.
    if matches!(signal, LifecycleSignal::Ended) {
        return Transition {
            next: LifecycleState {
                status: prior_status.unwrap_or(AgentStatus::Idle),
                posture,
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
        LifecycleSignal::ToolUsed { mutates } => {
            // A mutating tool while the slider still reads `plan` is impossible —
            // plan mode is read-only — so the agent left it without reporting a
            // new posture. Reconcile off plan and let the caller log it. This is
            // the "shows thinking while editing in auto mode" fix.
            if *mutates && posture == PermissionPosture::Plan {
                posture = PermissionPosture::Auto;
                kind = TransitionKind::Reconciled {
                    from: prior_status.unwrap_or(AgentStatus::Running),
                    reason: "mutating tool while plan posture",
                };
            }
            // A completed tool proves the agent is working; if the rollup thinks
            // it is resting (or it is unknown), reconcile to running. Attention
            // states (anything not resting) are left alone.
            match prior_status {
                Some(AgentStatus::Running) => AgentStatus::Running,
                Some(resting @ (AgentStatus::Idle | AgentStatus::Success)) => {
                    if matches!(kind, TransitionKind::Normal) {
                        kind = TransitionKind::Reconciled {
                            from: resting,
                            reason: "tool used outside a running turn",
                        };
                    }
                    AgentStatus::Running
                }
                Some(other) => other,
                None => {
                    if matches!(kind, TransitionKind::Normal) {
                        kind = TransitionKind::Reconciled {
                            from: AgentStatus::Idle,
                            reason: "tool used before session registered",
                        };
                    }
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
            posture,
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
        _ => LifecycleSignal::ToolUsed { mutates: false },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(status: AgentStatus, posture: PermissionPosture, compacting: bool) -> LifecycleState {
        LifecycleState {
            status,
            posture,
            compacting,
        }
    }

    #[test]
    fn registered_is_idle() {
        let t = step(None, &LifecycleSignal::Registered, None);
        assert_eq!(t.next.status, AgentStatus::Idle);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn turn_started_is_running() {
        let prev = state(AgentStatus::Idle, PermissionPosture::Default, false);
        let t = step(Some(&prev), &LifecycleSignal::TurnStarted, None);
        assert_eq!(t.next.status, AgentStatus::Running);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn clean_turn_end_is_success() {
        let prev = state(AgentStatus::Running, PermissionPosture::Default, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
            },
            None,
        );
        assert_eq!(t.next.status, AgentStatus::Success);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn errored_turn_end_is_failed() {
        let prev = state(AgentStatus::Running, PermissionPosture::Default, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
            },
            None,
        );
        assert_eq!(t.next.status, AgentStatus::Failed);
    }

    #[test]
    fn background_park_stays_running_normal() {
        let prev = state(AgentStatus::Running, PermissionPosture::Default, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: true,
            },
            None,
        );
        // A designed edge — running, no log noise.
        assert_eq!(t.next.status, AgentStatus::Running);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn errored_wins_over_background_park() {
        let prev = state(AgentStatus::Running, PermissionPosture::Default, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: true,
            },
            None,
        );
        assert_eq!(t.next.status, AgentStatus::Failed);
    }

    #[test]
    fn subagent_start_running_stop_idle() {
        let start = step(None, &LifecycleSignal::SubagentStarted, None);
        assert_eq!(start.next.status, AgentStatus::Running);
        let stop = step(Some(&start.next), &LifecycleSignal::SubagentStopped, None);
        assert_eq!(stop.next.status, AgentStatus::Idle);
    }

    #[test]
    fn compacting_keeps_status_and_sets_head() {
        let prev = state(AgentStatus::Running, PermissionPosture::Default, false);
        let t = step(Some(&prev), &LifecycleSignal::Compacting, None);
        assert_eq!(t.next.status, AgentStatus::Running);
        assert!(t.next.compacting);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn any_signal_clears_compacting_head() {
        let prev = state(AgentStatus::Running, PermissionPosture::Default, true);
        let t = step(Some(&prev), &LifecycleSignal::TurnStarted, None);
        assert!(!t.next.compacting);
    }

    #[test]
    fn mutating_tool_in_plan_reconciles_posture_off_plan() {
        let prev = state(AgentStatus::Running, PermissionPosture::Plan, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::ToolUsed { mutates: true },
            None,
        );
        assert_eq!(t.next.posture, PermissionPosture::Auto);
        assert_eq!(t.next.status, AgentStatus::Running);
        assert_eq!(
            t.kind,
            TransitionKind::Reconciled {
                from: AgentStatus::Running,
                reason: "mutating tool while plan posture",
            }
        );
    }

    #[test]
    fn nonmutating_tool_leaves_plan_posture_intact() {
        // The legacy shim can produce a non-mutating ToolUsed; it must not flip
        // a genuine plan posture.
        let prev = state(AgentStatus::Running, PermissionPosture::Plan, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::ToolUsed { mutates: false },
            None,
        );
        assert_eq!(t.next.posture, PermissionPosture::Plan);
        assert_eq!(t.kind, TransitionKind::Normal);
    }

    #[test]
    fn tool_while_resting_reconciles_to_running() {
        let prev = state(AgentStatus::Idle, PermissionPosture::Default, false);
        let t = step(
            Some(&prev),
            &LifecycleSignal::ToolUsed { mutates: true },
            None,
        );
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
    fn posture_sample_wins_then_carries_forward() {
        // A sample sets it...
        let t = step(
            None,
            &LifecycleSignal::Registered,
            Some(PermissionPosture::Yolo),
        );
        assert_eq!(t.next.posture, PermissionPosture::Yolo);
        // ...and a later event that names no slider keeps it.
        let t2 = step(Some(&t.next), &LifecycleSignal::TurnStarted, None);
        assert_eq!(t2.next.posture, PermissionPosture::Yolo);
    }

    #[test]
    fn ended_is_ignored_and_preserves_state() {
        let prev = state(AgentStatus::Running, PermissionPosture::Auto, false);
        let t = step(Some(&prev), &LifecycleSignal::Ended, None);
        assert_eq!(t.next.status, AgentStatus::Running);
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
            LifecycleSignal::ToolUsed { mutates: true },
            LifecycleSignal::Compacting,
            LifecycleSignal::Ended,
        ] {
            let _ = step(None, &signal, None);
        }
    }

    #[test]
    fn signal_round_trips_through_json() {
        let signal = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: true,
        };
        let wire = serde_json::to_value(signal).unwrap();
        assert_eq!(wire["signal"], "turn_ended");
        let back: LifecycleSignal = serde_json::from_value(wire).unwrap();
        assert_eq!(signal, back);
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
