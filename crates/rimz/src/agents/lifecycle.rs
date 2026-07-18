//! The agent lifecycle state machine.
//!
//! This module is the single home for the directed transition graph that folds
//! a [`LifecycleSignal`] — the agent-agnostic *intent* an adapter reads off a
//! native hook event — onto the reduced [`LifecycleState`]. Both the snapshot
//! reducer (silently, on replay) and the hook ingestion path (with one-shot
//! anomaly logging) call the one pure [`step`] function, so the transition
//! table lives in exactly one place. See
//! [docs/internals/agents/model.md](../../../../docs/internals/agents/model.md).
//!
//! `step` is reused identically for a root agent and a subagent: the two levels
//! differ only in which signals are legal and how the entity is keyed, never in
//! the transition itself. The function is pure and total — an unexpected
//! `(state, signal)` pair never panics. It takes the signal's natural edge (the
//! agent is authoritative about its own activity) and tags the result
//! [`TransitionKind::Reconciled`] so the caller can log the drift rather than
//! silently absorbing it.

use serde::{Deserialize, Serialize};

use crate::agents::AgentStatus;
use crate::ids::AskId;

macro_rules! lifecycle_signal_kinds {
    ($($variant:ident => $label:literal),+ $(,)?) => {
        /// Data-less lifecycle signal kinds every adapter declares explicitly.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum LifecycleSignalKind {
            $($variant),+
        }

        impl LifecycleSignalKind {
            pub const ALL: [Self; lifecycle_signal_kinds!(@count $($variant),+)] = [
                $(Self::$variant),+
            ];

            pub const fn short_label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }
        }
    };
    (@count $($variant:ident),+ $(,)?) => {
        <[()]>::len(&[$(lifecycle_signal_kinds!(@unit $variant)),+])
    };
    (@unit $variant:ident) => {
        ()
    };
}

lifecycle_signal_kinds! {
    Registered => "registered",
    TurnStarted => "turn_started",
    TurnEnded => "turn_ended",
    ToolUsed => "tool_used",
    AwaitingInput => "awaiting_input",
    SubagentStarted => "subagent_started",
    SubagentStopped => "subagent_stopped",
    Compacting => "compacting",
    CompactionEnded => "compaction_ended",
    Ended => "ended",
    Lost => "lost",
}

/// Which native prompt is blocking the agent's own UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    Permission,
    PlanApproval,
    Question,
}

impl AskKind {
    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::PlanApproval => "plan approval",
            Self::Question => "question",
        }
    }
}

/// The agent-agnostic intent each native lifecycle event carries. An adapter's
/// `decode_hook` maps a native event onto exactly one of these (plus the
/// enrichment); it no longer decides an [`AgentStatus`].
///
/// Wire format is internally tagged on `signal` (snake_case), e.g.
/// `{"signal":"turn_ended","errored":false,"parked_on_background":false}`. It
/// rides the `agent.lifecycle` event params in place of the legacy bare status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// A turn was canceled by the user or provider. This closes the turn
    /// without reporting either success or failure and leaves the session idle.
    TurnInterrupted,
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
    /// A tool completed (`PostToolUse`) or non-blocking tool started
    /// (`PreToolUse`). Adapters emit this for every completed tool; `mutates`,
    /// not emission, marks proof of real work durable enough to record without
    /// a state change. `edits` marks the file-editing subset (Claude
    /// `Edit`/`Write`/…, Codex `apply_patch`): the first edit of a turn moves
    /// it from reasoning to acting. Defaulted so a `tool_used` event written
    /// before the bit existed still replays.
    ToolUsed {
        mutates: bool,
        #[serde(default)]
        edits: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_key: Option<String>,
    },
    /// The agent opened a native blocking prompt and is waiting for input in
    /// its own pane. Hook ingestion mints `ask_id`; adapters leave it absent.
    AwaitingInput {
        kind: AskKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ask_id: Option<AskId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native_key: Option<String>,
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
    /// The session ended (Claude `SessionEnd`/`offline`). The reducer stamps
    /// the durable row while [`step`] preserves its last lifecycle state.
    Ended,
    /// The agent's pane disappeared because its mux session died. Retained so
    /// old `rimz.agent-lost` records remain parseable during log replay.
    Lost,
}

impl LifecycleSignal {
    /// Data-less kind for matrix rows and descriptor conformance.
    pub const fn kind(&self) -> LifecycleSignalKind {
        match self {
            Self::Registered => LifecycleSignalKind::Registered,
            Self::TurnStarted => LifecycleSignalKind::TurnStarted,
            Self::TurnEnded { .. } | Self::TurnInterrupted => LifecycleSignalKind::TurnEnded,
            Self::SubagentStarted => LifecycleSignalKind::SubagentStarted,
            Self::SubagentStopped { .. } => LifecycleSignalKind::SubagentStopped,
            Self::ToolUsed { .. } => LifecycleSignalKind::ToolUsed,
            Self::AwaitingInput { .. } => LifecycleSignalKind::AwaitingInput,
            Self::Compacting => LifecycleSignalKind::Compacting,
            Self::CompactionEnded { .. } => LifecycleSignalKind::CompactionEnded,
            Self::Ended => LifecycleSignalKind::Ended,
            Self::Lost => LifecycleSignalKind::Lost,
        }
    }

    /// Whether this signal establishes a rollup identity when no prior row
    /// exists for the `(kind, agent_id)` key.
    pub const fn establishes_identity(&self) -> bool {
        matches!(self, Self::Registered | Self::SubagentStarted)
    }

    /// Stable serde tag for runtime wakeups and diagnostics.
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Registered => "registered",
            Self::TurnStarted => "turn_started",
            Self::TurnEnded { .. } => "turn_ended",
            Self::TurnInterrupted => "turn_interrupted",
            Self::SubagentStarted => "subagent_started",
            Self::SubagentStopped { .. } => "subagent_stopped",
            Self::ToolUsed { .. } => "tool_used",
            Self::AwaitingInput { .. } => "awaiting_input",
            Self::Compacting => "compacting",
            Self::CompactionEnded { .. } => "compaction_ended",
            Self::Ended => "ended",
            Self::Lost => "lost",
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

impl TurnPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Reasoning => "reasoning",
            Self::Acting => "acting",
            Self::Parked => "parked",
        }
    }
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
    /// A waiting state was durably cleared without otherwise changing status.
    /// Blocking prompts normally resume through their own non-mutating
    /// PostToolUse answer edge, with the next PreToolUse as a backstop;
    /// recording that no-op is what clears `waiting_since` on replay.
    pub waiting_cleared: bool,
    /// A turn boundary opened or re-opened. Explicit starts stamp a fresh
    /// prompt boundary except when a prompt wakes a parked running row and
    /// resumes the same logical turn. An answered ask also resumes the same
    /// prompt boundary; other reconciled progress and auto-compaction resumes
    /// stamp only when they enter `Running` from a non-running prior state.
    pub opened_turn: bool,
}

/// Fold one [`LifecycleSignal`] onto the prior [`LifecycleState`]. Pure and
/// total: any `(prev, signal)` pair returns a `Transition` and never panics.
pub fn step(
    prev: Option<&LifecycleState>,
    open_ask_key: Option<&str>,
    signal: &LifecycleSignal,
) -> Transition {
    let prior_status = prev.map(|p| p.status);
    let prior_phase = prev.map(|p| p.phase).unwrap_or_default();
    let was_compacting = prev.is_some_and(|p| p.compacting);
    let mut kind = TransitionKind::Normal;

    // `Ended` stamps row state in the reducer and `Lost` is kept for
    // backward-compatible log replay. Both preserve the lifecycle state and
    // report an ignored transition.
    if matches!(signal, LifecycleSignal::Ended | LifecycleSignal::Lost) {
        return Transition {
            next: LifecycleState {
                status: prior_status.unwrap_or(AgentStatus::Idle),
                phase: prior_phase,
                compacting: was_compacting,
            },
            kind: TransitionKind::Ignored {
                reason: match signal {
                    LifecycleSignal::Ended => "session ended (handled as removal)",
                    LifecycleSignal::Lost => "session lost (legacy replay marker)",
                    _ => unreachable!("guarded above"),
                },
            },
            compaction_closed: false,
            waiting_cleared: false,
            opened_turn: false,
        };
    }

    // Compaction is the one signal that preserves the prior status; it is a
    // transient head, not a transition. Every other signal clears the head.
    let compacting = matches!(signal, LifecycleSignal::Compacting);
    let status = map_status(signal, prior_status, open_ask_key, &mut kind);
    let phase = map_phase(signal, prior_phase, status);
    let compaction_closed = was_compacting && !compacting;
    let waiting_cleared =
        prior_status == Some(AgentStatus::Waiting) && status != AgentStatus::Waiting;
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
        waiting_cleared,
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
    matches!(signal, LifecycleSignal::TurnStarted)
        || (matches!(signal, LifecycleSignal::SubagentStarted) && status == AgentStatus::Running)
        || (status == AgentStatus::Running
            && !matches!(
                prior_status,
                Some(AgentStatus::Running | AgentStatus::Waiting)
            )
            && matches!(
                signal,
                LifecycleSignal::ToolUsed { .. }
                    | LifecycleSignal::CompactionEnded { auto: Some(true) }
            ))
}

fn map_status(
    signal: &LifecycleSignal,
    prior_status: Option<AgentStatus>,
    open_ask_key: Option<&str>,
    kind: &mut TransitionKind,
) -> AgentStatus {
    match signal {
        LifecycleSignal::Registered => AgentStatus::Idle,
        LifecycleSignal::TurnStarted => AgentStatus::Running,
        LifecycleSignal::SubagentStarted => match prior_status {
            Some(terminal @ (AgentStatus::Success | AgentStatus::Failed)) => {
                *kind = TransitionKind::Ignored {
                    reason: "subagent start after a terminal stop",
                };
                terminal
            }
            _ => AgentStatus::Running,
        },
        LifecycleSignal::AwaitingInput { .. } => AgentStatus::Waiting,
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
        LifecycleSignal::TurnInterrupted => AgentStatus::Idle,
        LifecycleSignal::ToolUsed { native_key, .. } => {
            // A completed tool proves the agent is working; if the rollup thinks
            // it is resting (or it is unknown), reconcile to running. Attention
            // states (anything not resting) are left alone.
            match prior_status {
                Some(AgentStatus::Waiting)
                    if open_ask_key.is_some()
                        && native_key.as_deref().is_some()
                        && open_ask_key != native_key.as_deref() =>
                {
                    *kind = TransitionKind::Ignored {
                        reason: "sibling tool completed while a keyed ask is open",
                    };
                    AgentStatus::Waiting
                }
                Some(AgentStatus::Running | AgentStatus::Waiting) => AgentStatus::Running,
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
            Some(AgentStatus::Running | AgentStatus::Waiting) => AgentStatus::Running,
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
        LifecycleSignal::CompactionEnded { auto: Some(false) }
            if prior_status == Some(AgentStatus::Waiting) =>
        {
            AgentStatus::Running
        }
        LifecycleSignal::CompactionEnded { auto: Some(false) } => AgentStatus::Idle,
        LifecycleSignal::CompactionEnded { auto: None } => {
            if prior_status == Some(AgentStatus::Waiting) {
                AgentStatus::Running
            } else {
                prior_status.unwrap_or(AgentStatus::Idle)
            }
        }
        // Handled above.
        LifecycleSignal::Ended | LifecycleSignal::Lost => {
            unreachable!("terminal side-channel signals return early")
        }
    }
}

fn map_phase(signal: &LifecycleSignal, prior_phase: TurnPhase, status: AgentStatus) -> TurnPhase {
    // The turn phase: a fresh turn (or child task) opens reasoning; its first
    // file edit moves it to acting; a clean end parking on background work
    // parks it; any other boundary rests it. Compaction preserves the phase
    // like it preserves the status.
    let phase = match signal {
        LifecycleSignal::TurnStarted | LifecycleSignal::SubagentStarted => TurnPhase::Reasoning,
        LifecycleSignal::AwaitingInput { .. } => TurnPhase::Idle,
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
        | LifecycleSignal::TurnInterrupted
        | LifecycleSignal::SubagentStopped { .. } => TurnPhase::Idle,
        // Handled above.
        LifecycleSignal::Ended | LifecycleSignal::Lost => {
            unreachable!("terminal side-channel signals return early")
        }
    };
    // The phase axis exists only inside a running turn — a resting or
    // attention status always reads `Idle`, by construction.
    if status == AgentStatus::Running {
        if phase == TurnPhase::Idle {
            TurnPhase::Reasoning
        } else {
            phase
        }
    } else {
        TurnPhase::Idle
    }
}

#[cfg(test)]
mod tests;
