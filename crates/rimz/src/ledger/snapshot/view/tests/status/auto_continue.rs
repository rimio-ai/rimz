use super::*;

use std::time::Duration;

use jiff::Timestamp;

use crate::agents::{ResumeArm, resume_park};

/// The reset deadline the producer would durably arm for one agent this frame, or
/// `None` when there is nothing to arm. Mirrors what `sidebar::enrich`
/// auto-continue records while a park is fresh, before the live reading turns over.
fn arm(agent: &AgentState) -> Option<ResumeArm> {
    resume_park(agent, epoch())
}

/// The reset deadline `resets_in_secs` after the fixed epoch — the value `window`
/// stamps onto a window's `resets_at`.
fn deadline(resets_in_secs: i64) -> Timestamp {
    Timestamp::from_second(epoch().as_second() + resets_in_secs).expect("valid test timestamp")
}

fn ago(secs: u64) -> Timestamp {
    epoch() - Duration::from_secs(secs)
}

#[test]
fn arms_a_rate_limit_park_at_its_window_reset() {
    let parked = agent("claude", "limited", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, 3_600)])
        .paused_turn_error(10, "You've hit your usage limit");
    assert_eq!(
        arm(&parked),
        Some(ResumeArm::RateLimit {
            deadline: deadline(3_600)
        })
    );
}

#[test]
fn legacy_session_limit_marker_arms_a_rate_limit_park() {
    let parked = agent("claude", "limited", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, 3_600)])
        .turn_error(10, "You've hit your session limit · resets 10:50am (UTC)");
    assert_eq!(
        arm(&parked),
        Some(ResumeArm::RateLimit {
            deadline: deadline(3_600)
        })
    );
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
    assert_eq!(
        arm(&parked),
        Some(ResumeArm::RateLimit {
            deadline: deadline(86_400)
        })
    );
}

#[test]
fn arms_after_reset_when_the_park_marker_is_still_active() {
    // A producer can miss the pre-reset frame during a reload or elder change.
    // While the same parked turn-error marker remains active, a spent window that
    // has crossed its reset recreates a due arm so live auto-continue still fires.
    let parked = agent("claude", "limited", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, -60)])
        .paused_turn_error(10, "You've hit your usage limit");
    assert_eq!(
        arm(&parked),
        Some(ResumeArm::RateLimit {
            deadline: deadline(-60)
        })
    );
}

#[test]
fn does_not_recreate_an_arm_from_a_refilled_reading() {
    // Once the agent reports a non-spent window, there is no spent reset
    // certificate left to distinguish a missed park from a stale marker.
    let parked = agent("claude", "limited", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(20, -60)])
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
fn arms_an_overloaded_park() {
    let parked = agent("claude", "busy", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, 3_600)])
        .overloaded_turn_error(10, "API Error: Overloaded");
    assert_eq!(
        arm(&parked),
        Some(ResumeArm::Overloaded {
            overloaded_at: ago(10)
        })
    );
}

#[test]
fn overloaded_arm_ignores_spent_windows() {
    // `overloaded` carries no local reset window, so a reset budget reading does
    // not suppress the retry-backed arm.
    let parked = agent("claude", "busy", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .limits(vec![window(100, -60)])
        .overloaded_turn_error(10, "API Error: Overloaded");
    assert_eq!(
        arm(&parked),
        Some(ResumeArm::Overloaded {
            overloaded_at: ago(10)
        })
    );
}

#[test]
fn does_not_arm_a_failed_park() {
    let failed = agent("claude", "failed", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .turn_error(10, "API Error: Bad Request");
    assert_eq!(arm(&failed), None);
}

#[test]
fn arms_a_server_error_park() {
    let temporary_500 = concat!(
        "API Error: 500 Internal server error. ",
        "This is a server-side issue, usually temporary — try again in a moment."
    );
    let parked = agent("claude", "busy", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .active_ago(60)
        .overloaded_turn_error(10, temporary_500);
    assert_eq!(
        arm(&parked),
        Some(ResumeArm::Overloaded {
            overloaded_at: ago(10)
        })
    );
}
