//! Semantic state → palette mapping, centralized so every command colors a
//! given state identically. Each helper matches on the typed enum, so adding or
//! renaming a variant is a compile error here rather than a silent fall-through
//! to the default tone in one command and a different tone in another.

use rimz::agents::AgentStatus;
use rimz::agents::TurnPhase;
use rimz::harness::run::RunStatus;
use rimz::message::MessageStatus;
use rimz::trust::TrustState;

use crate::cli::providers::ProviderStatus;

use super::palette;

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
        StateRole::Paused => palette::paused(),
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
        MessageStatus::TimedOut | MessageStatus::Errored | MessageStatus::Abandoned => {
            role(StateRole::Waiting)
        }
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

pub(crate) fn provider(status: ProviderStatus) -> anstyle::Style {
    match status {
        ProviderStatus::LoggedIn => role(StateRole::Success),
        ProviderStatus::LoggedOut => role(StateRole::Paused),
        ProviderStatus::Unavailable => role(StateRole::Unavailable),
    }
}
