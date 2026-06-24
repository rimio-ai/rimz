//! Semantic state → palette mapping, centralized so every command colors a
//! given state identically. Each helper matches on the typed enum, so adding or
//! renaming a variant is a compile error here rather than a silent fall-through
//! to the default tone in one command and a different tone in another.

use rimz::agents::AgentStatus;
use rimz::agents::TurnPhase;
use rimz::feed::FeedStatus;
use rimz::message::MessageStatus;
use rimz::run::RunStatus;
use rimz::trust::TrustState;

use super::palette;

/// An agent's lifecycle status, refined by its turn phase: a reasoning agent
/// reads cool, an acting one healthy-green.
pub(crate) fn agent(status: AgentStatus, phase: TurnPhase) -> anstyle::Style {
    match status {
        AgentStatus::Running if phase == TurnPhase::Reasoning => palette::COOL,
        AgentStatus::Running | AgentStatus::Success => palette::GOOD,
        AgentStatus::Idle => palette::MUTED,
        AgentStatus::Waiting | AgentStatus::Paused => palette::WARN,
        AgentStatus::Failed => palette::ALARM,
    }
}

/// A supervised run's terminal/working status.
pub(crate) fn run(status: RunStatus) -> anstyle::Style {
    match status {
        RunStatus::Completed => palette::GOOD,
        RunStatus::Running | RunStatus::Pending => palette::COOL,
        RunStatus::Failed | RunStatus::TimedOut => palette::ALARM,
        RunStatus::Canceled => palette::MUTED,
    }
}

/// A queued message's delivery status.
pub(crate) fn message(status: MessageStatus) -> anstyle::Style {
    match status {
        MessageStatus::Delivered => palette::GOOD,
        MessageStatus::Created
        | MessageStatus::Queued
        | MessageStatus::Claimed
        | MessageStatus::Sent => palette::COOL,
        MessageStatus::TimedOut | MessageStatus::Errored | MessageStatus::Abandoned => {
            palette::WARN
        }
        MessageStatus::Removed => palette::MUTED,
    }
}

/// A feed item's resolution status.
pub(crate) fn feed(status: FeedStatus) -> anstyle::Style {
    match status {
        FeedStatus::Resolved => palette::GOOD,
        FeedStatus::Pending => palette::COOL,
        FeedStatus::TimedOut => palette::WARN,
        FeedStatus::Abandoned => palette::MUTED,
    }
}

/// A project's executable-surface trust state. `Stale` reads as an alarm: the
/// surface drifted since the grant, the one state worth a second look.
pub(crate) fn trust(state: TrustState) -> anstyle::Style {
    match state {
        TrustState::Trusted => palette::GOOD,
        TrustState::Stale => palette::ALARM,
        TrustState::Untrusted => palette::WARN,
        TrustState::NoConfig => palette::MUTED,
    }
}
