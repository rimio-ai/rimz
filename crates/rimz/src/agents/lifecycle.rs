//! The agent lifecycle state machine.
//!
//! This module is the single home for the directed transition graph that folds
//! a [`LifecycleSignal`] — the agent-agnostic *intent* an adapter reads off a
//! native hook event — onto the reduced [`LifecycleState`]. Both the snapshot
//! reducer (silently, on replay) and the hook ingestion path (with one-shot
//! anomaly logging) call the one pure [`step`] function, so the transition
//! table lives in exactly one place. See
//! [docs/internals/agents/agent.md](../../../../docs/internals/agents/agent.md).
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
    /// A session registered fresh (Claude/Codex `SessionStart` sources other
    /// than `compact`, Pi `session_start`).
    Registered,
    /// A user turn began (`UserPromptSubmit`). On a parked running row this
    /// resumes the same logical turn instead of opening a fresh boundary.
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
    /// Context compaction finished or was observed to have finished
    /// (Claude/Codex `PostCompact`, Claude/Codex `SessionStart` with
    /// `source = "compact"`, Pi `session_compact`). The transient compacting
    /// head lifts here when one is open. When the provider reports the trigger,
    /// automatic compaction resumes the interrupted turn and manual `/compact`
    /// rests to idle. A provider with no trigger bit preserves the prior state.
    CompactionEnded {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto: Option<bool>,
    },
    /// The session ended (Claude `SessionEnd`/`offline`). Handled as removal by
    /// the reducer's tombstone path, so it is never routed through [`step`];
    /// the variant exists only so an adapter can name the event.
    Ended,
}

impl LifecycleSignal {
    /// Whether this signal establishes a rollup identity when no prior row
    /// exists for the `(kind, agent_id)` key.
    pub const fn establishes_identity(self) -> bool {
        matches!(self, Self::Registered | Self::SubagentStarted)
    }

    /// Stable serde tag for runtime wakeups and diagnostics.
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::TurnStarted => "turn_started",
            Self::TurnEnded { .. } => "turn_ended",
            Self::SubagentStarted => "subagent_started",
            Self::SubagentStopped { .. } => "subagent_stopped",
            Self::ToolUsed { .. } => "tool_used",
            Self::Compacting => "compacting",
            Self::CompactionEnded { .. } => "compaction_ended",
            Self::Ended => "ended",
        }
    }
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
    /// paints the thinking head. Set by a turn (or child task) start,
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
/// this tag (it wants `next` and the transition facts); the ingestion path logs
/// `Reconciled`/`Ignored` once per fresh event under the
/// `rimz::agent::lifecycle` target.
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
    /// Countable compaction-bracket close: the prior state was compacting and
    /// this signal leaves it. Any non-`Compacting` signal closes an open
    /// bracket; an unbracketed close signal closes nothing.
    pub compaction_closed: bool,
    /// A turn boundary opened or re-opened. Explicit starts stamp a fresh
    /// prompt boundary except when a prompt wakes a parked running row and
    /// resumes the same logical turn; reconciled progress and auto-compaction
    /// resumes stamp only when they enter `Running` from a non-running prior
    /// state.
    pub opened_turn: bool,
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
            compaction_closed: false,
            opened_turn: false,
        };
    }

    // Compaction is the one signal that preserves the prior status; it is a
    // transient head, not a transition. Every other signal clears the head.
    let compacting = matches!(signal, LifecycleSignal::Compacting);
    let status = map_status(signal, prior_status, &mut kind);
    let phase = map_phase(signal, prior_phase, status);
    let compaction_closed = was_compacting && !compacting;
    let opened_turn = opened_turn(signal, prior_status, prior_phase, status);

    if matches!(signal, LifecycleSignal::CompactionEnded { .. })
        && !was_compacting
        && matches!(kind, TransitionKind::Normal)
        && prior_status == Some(status)
        && prior_phase == phase
    {
        kind = TransitionKind::Ignored {
            reason: "compaction end without an open bracket",
        };
    }

    Transition {
        next: LifecycleState {
            status,
            phase,
            compacting,
        },
        kind,
        compaction_closed,
        opened_turn,
    }
}

fn opened_turn(
    signal: &LifecycleSignal,
    prior_status: Option<AgentStatus>,
    prior_phase: TurnPhase,
    status: AgentStatus,
) -> bool {
    if matches!(signal, LifecycleSignal::TurnStarted)
        && prior_status == Some(AgentStatus::Running)
        && prior_phase == TurnPhase::Parked
    {
        return false;
    }
    matches!(
        signal,
        LifecycleSignal::TurnStarted | LifecycleSignal::SubagentStarted
    ) || (status == AgentStatus::Running
        && prior_status != Some(AgentStatus::Running)
        && matches!(
            signal,
            LifecycleSignal::ToolUsed { .. }
                | LifecycleSignal::CompactionEnded { auto: Some(true) }
        ))
}

fn map_status(
    signal: &LifecycleSignal,
    prior_status: Option<AgentStatus>,
    kind: &mut TransitionKind,
) -> AgentStatus {
    match signal {
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
                    *kind = TransitionKind::Reconciled {
                        from: resting,
                        reason: "tool used outside a running turn",
                    };
                    AgentStatus::Running
                }
                Some(other) => other,
                None => {
                    *kind = TransitionKind::Reconciled {
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
                *kind = TransitionKind::Reconciled {
                    from: resting,
                    reason: "auto-compaction resumed a turn",
                };
                AgentStatus::Running
            }
            Some(other) => other,
            None => {
                *kind = TransitionKind::Reconciled {
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
    }
}

fn map_phase(signal: &LifecycleSignal, prior_phase: TurnPhase, status: AgentStatus) -> TurnPhase {
    // The turn phase: a fresh turn (or child task) opens reasoning; its first
    // file edit moves it to acting; a clean end parking on background work
    // parks it; any other boundary rests it. Compaction preserves the phase
    // like it preserves the status.
    let phase = match signal {
        LifecycleSignal::TurnStarted | LifecycleSignal::SubagentStarted => TurnPhase::Reasoning,
        // A shell command during the reasoning phase is work, but the turn has
        // still written nothing — the thinking head carries forward. Anywhere else a
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
    if status == AgentStatus::Running {
        phase
    } else {
        TurnPhase::Idle
    }
}

#[cfg(test)]
mod tests;
