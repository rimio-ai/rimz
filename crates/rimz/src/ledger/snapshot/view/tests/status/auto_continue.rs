use super::*;

use std::time::Duration;

use jiff::Timestamp;

use crate::feed::rate_limit_resume_arm;

/// The reset deadline the producer would durably arm for one agent this frame, or
/// `None` when there is nothing to arm. Mirrors what `sidebar::enrich`
/// auto-continue records while a park is fresh, before the live reading turns over.
fn arm(agent: &AgentState) -> Option<Timestamp> {
    rate_limit_resume_arm(agent, epoch())
}

/// The reset deadline `resets_in_secs` after the fixed epoch — the value `window`
/// stamps onto a window's `resets_at`.
fn deadline(resets_in_secs: i64) -> Timestamp {
    epoch() + Duration::from_secs(resets_in_secs as u64)
}

#[test]
fn arms_a_rate_limit_park_at_its_window_reset() {
    let parked = agent("claude", "limited", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, 3_600)])
        .paused_turn_error(10, "You've hit your usage limit");
    assert_eq!(arm(&parked), Some(deadline(3_600)));
}

#[test]
fn arms_for_the_latest_of_several_spent_windows() {
    // The turn may resume only once every spent window has reset, so the deadline
    // is the furthest reset — here the 7d window, not the 5h.
    let parked = agent("claude", "limited", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, 3_600), window(100, 86_400)])
        .paused_turn_error(10, "You've hit your usage limit");
    assert_eq!(arm(&parked), Some(deadline(86_400)));
}

#[test]
fn does_not_arm_once_the_only_window_has_reset() {
    // A window already past its reset is no longer spent: there is nothing fresh to
    // arm. The producer fires off the deadline it recorded earlier, not this frame.
    let parked = agent("claude", "limited", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, -60)])
        .paused_turn_error(10, "You've hit your usage limit");
    assert_eq!(arm(&parked), None);
}

#[test]
fn does_not_arm_a_calm_agent_with_a_spent_window() {
    // A kind whose budget is spent but whose agent never stopped on a limit keeps
    // working; there is no park to resume.
    let calm = agent("claude", "calm", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(5)
        .limits(vec![window(100, 3_600)]);
    assert_eq!(arm(&calm), None);
}

#[test]
fn does_not_arm_an_overloaded_park() {
    // `overloaded` carries no local reset window — it recovers on a provider retry,
    // not a window clock — so a spent budget never arms it.
    let parked = agent("claude", "busy", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, 3_600)])
        .overloaded_turn_error(10, "API Error: Overloaded");
    assert_eq!(arm(&parked), None);
}
