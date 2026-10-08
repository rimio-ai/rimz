//! Semantic state → palette mapping, centralized so every command colors a
//! given state identically. Each helper matches on the typed enum, so adding or
//! renaming a variant is a compile error here rather than a silent fall-through
//! to the default tone in one command and a different tone in another.

use rimz::agents::AccountStatus;
use rimz::agents::AgentStatus;
use rimz::agents::TurnPhase;
use rimz::agents::account::ProviderStatus;
use rimz::store::message::MessageStatus;
use rimz::store::run::RunStatus;
use rimz::trust::TrustState;

use super::palette;

/// A shared language server's registry state. Returns the role because verdict glyphs and doctor's health tally both start from it.
pub(crate) fn lsp(state: &rimz::lsp::registry::State) -> StateRole {
    use rimz::lsp::registry::{State, StopReason};
    match state {
        State::Ready => StateRole::Success,
        State::Starting | State::Indexing => StateRole::Working,
        State::Dormant { reason, .. } => match reason {
            Some(StopReason::Crashed) => StateRole::Failed,
            Some(StopReason::MemoryPressure) => StateRole::Waiting,
            None
            | Some(
                StopReason::Idle
                | StopReason::Evicted
                | StopReason::TeamDone
                | StopReason::StoppedByHand
                | StopReason::Released
                | StopReason::NeverLeased
                | StopReason::CheckoutRemoved,
            ) => StateRole::Neutral,
        },
        State::Stopped { .. } => StateRole::Neutral,
    }
}

/// The row order both LSP tables share: running servers first, then those
/// needing attention, then the rest, ties broken by checkout and server.
pub(crate) fn lsp_order(entry: &rimz::lsp::registry::Entry) -> (u8, &std::path::Path, &str) {
    let rank = match lsp(&entry.state) {
        StateRole::Success | StateRole::Working => 0,
        StateRole::Failed | StateRole::Waiting | StateRole::Paused | StateRole::Unavailable => 1,
        StateRole::Neutral => 2,
    };
    (rank, &entry.root, &entry.server)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StateRole {
    Success,
    Working,
    Waiting,
    Paused,
    Failed,
    Unavailable,
    Neutral,
}

pub(crate) fn role(role: StateRole) -> anstyle::Style {
    match role {
        StateRole::Success => palette::good(),
        StateRole::Working => palette::cool(),
        StateRole::Waiting => palette::warn(),
        StateRole::Paused => palette::warn(),
        StateRole::Failed | StateRole::Unavailable => palette::alarm(),
        StateRole::Neutral => palette::muted(),
    }
}

/// An agent's lifecycle status, refined by its turn phase: a reasoning agent
/// reads cool, an acting one healthy-green.
pub(crate) fn agent(status: AgentStatus, phase: TurnPhase) -> anstyle::Style {
    match status {
        AgentStatus::Running if phase == TurnPhase::Reasoning => role(StateRole::Working),
        AgentStatus::Running | AgentStatus::Success => role(StateRole::Success),
        AgentStatus::Idle => role(StateRole::Neutral),
        AgentStatus::Sleeping => role(StateRole::Working),
        AgentStatus::Waiting => role(StateRole::Waiting),
        AgentStatus::Paused => role(StateRole::Paused),
        AgentStatus::Failed => role(StateRole::Failed),
    }
}

/// A supervised run's terminal/working status.
pub(crate) fn run(status: RunStatus) -> anstyle::Style {
    match status {
        RunStatus::Completed => role(StateRole::Success),
        RunStatus::Running | RunStatus::Pending => role(StateRole::Working),
        RunStatus::Failed
        | RunStatus::VerifyFailed
        | RunStatus::TimedOut
        | RunStatus::BudgetExceeded => role(StateRole::Failed),
        RunStatus::Canceled => role(StateRole::Neutral),
    }
}

/// A queued message's delivery status.
pub(crate) fn message(status: MessageStatus) -> anstyle::Style {
    match status {
        MessageStatus::Delivered => role(StateRole::Success),
        MessageStatus::Queued | MessageStatus::Claimed | MessageStatus::Sent => {
            role(StateRole::Working)
        }
        MessageStatus::TimedOut
        | MessageStatus::Errored
        | MessageStatus::Abandoned
        | MessageStatus::Expired => role(StateRole::Waiting),
        MessageStatus::Canceled | MessageStatus::Archived => role(StateRole::Neutral),
    }
}

/// A project's executable-surface trust state. `Stale` reads as an alarm: the
/// surface drifted since the grant, the one state worth a second look.
pub(crate) fn trust(state: TrustState) -> anstyle::Style {
    match state {
        TrustState::Trusted => role(StateRole::Success),
        TrustState::Stale => role(StateRole::Failed),
        TrustState::Untrusted => role(StateRole::Waiting),
        TrustState::NoConfig => role(StateRole::Neutral),
    }
}

/// Whether a room can launch into an account; every problem reads as a warning.
pub(crate) fn account(status: AccountStatus) -> anstyle::Style {
    match status {
        AccountStatus::Ready => role(StateRole::Success),
        AccountStatus::HomeMissing
        | AccountStatus::HooksMissing
        | AccountStatus::HooksUntrusted
        | AccountStatus::LoggedOut
        | AccountStatus::Unavailable => role(StateRole::Waiting),
    }
}

pub(crate) fn provider(status: ProviderStatus) -> anstyle::Style {
    match status {
        ProviderStatus::LoggedIn => role(StateRole::Success),
        ProviderStatus::LoggedOut => role(StateRole::Paused),
        ProviderStatus::Unavailable => role(StateRole::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::lsp::registry::{State, StopReason};

    #[test]
    fn lsp_roles_cover_states_and_stop_reasons() {
        for (state, expected) in [
            (State::Ready, StateRole::Success),
            (State::Starting, StateRole::Working),
            (State::Indexing, StateRole::Working),
            (
                State::Dormant {
                    since_ms: 0,
                    reason: None,
                },
                StateRole::Neutral,
            ),
            (
                State::Stopped {
                    at_ms: 0,
                    reason: StopReason::Released,
                },
                StateRole::Neutral,
            ),
        ] {
            assert_eq!(lsp(&state), expected, "{state:?}");
        }
        for (reason, expected) in [
            (StopReason::Crashed, StateRole::Failed),
            (StopReason::MemoryPressure, StateRole::Waiting),
            (StopReason::Idle, StateRole::Neutral),
            (StopReason::Evicted, StateRole::Neutral),
            (StopReason::TeamDone, StateRole::Neutral),
            (StopReason::StoppedByHand, StateRole::Neutral),
            (StopReason::Released, StateRole::Neutral),
            (StopReason::NeverLeased, StateRole::Neutral),
            (StopReason::CheckoutRemoved, StateRole::Neutral),
        ] {
            assert_eq!(
                lsp(&State::Dormant {
                    since_ms: 0,
                    reason: Some(reason)
                }),
                expected,
                "{reason}"
            );
        }
    }
}
