use super::*;

use std::time::Duration;

use jiff::Timestamp;

use crate::agents::{AccountBudget, ResumeArm, TurnErrorClass, resume_park};

/// The reset deadline the producer would durably arm for one agent this frame, or
/// `None` when there is nothing to arm. Mirrors what `sidebar::enrich`
/// auto-continue records while a park is fresh, before the live reading turns over.
fn arm(agent: &AgentState, budget: Option<&AccountBudget>) -> Option<ResumeArm> {
    resume_park(agent, budget, epoch())
}

fn budget(windows: Vec<RateLimitWindow>) -> AccountBudget {
    AccountBudget { windows }
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
fn limit_parks_arm_at_the_fused_budget_reset() {
    for (label, agent, budget, expected) in [
        (
            "rate-limit park",
            agent("claude", "limited", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .paused_turn_error(10, "You've hit your usage limit"),
            Some(budget(vec![window(100, 3_600)])),
            Some(ResumeArm::RateLimit {
                deadline: deadline(3_600),
            }),
        ),
        (
            "spend-limit park",
            agent("claude", "limited", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .spend_limit_turn_error(10, "You've hit your monthly spend limit."),
            Some(budget(vec![window(100, 3_600)])),
            Some(ResumeArm::RateLimit {
                deadline: deadline(3_600),
            }),
        ),
        (
            "legacy session-limit marker",
            agent("claude", "limited", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .turn_error(10, "You've hit your session limit · resets 10:50am (UTC)"),
            Some(budget(vec![window(100, 3_600)])),
            Some(ResumeArm::RateLimit {
                deadline: deadline(3_600),
            }),
        ),
        // The turn may resume only once every spent window has reset, so the deadline
        // is the furthest reset — here the 7d window, not the 5h.
        (
            "latest spent-window reset",
            agent("claude", "limited", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .paused_turn_error(10, "You've hit your usage limit"),
            Some(budget(vec![window(100, 3_600), window(100, 86_400)])),
            Some(ResumeArm::RateLimit {
                deadline: deadline(86_400),
            }),
        ),
    ] {
        assert_eq!(arm(&agent, budget.as_ref()), expected, "{label}");
    }
}

#[test]
fn overload_class_parks_arm_on_retry_backoff() {
    let temporary_500 = concat!(
        "API Error: 500 Internal server error. ",
        "This is a server-side issue, usually temporary — try again in a moment."
    );
    const STALL_LABEL: &str =
        "API Error: Response stalled mid-stream. The response above may be incomplete.";

    for (label, agent, budget, expected) in [
        (
            "overloaded park",
            agent("claude", "busy", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .limits(vec![window(100, 3_600)])
                .overloaded_turn_error(10, "API Error: Overloaded"),
            Some(budget(vec![window(100, 3_600)])),
            Some(ResumeArm::Overloaded {
                overloaded_at: ago(10),
            }),
        ),
        // `overloaded` carries no local reset window, so a reset budget reading does
        // not suppress the retry-backed arm.
        (
            "overloaded park ignores spent windows",
            agent("claude", "busy", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .limits(vec![window(100, -60)])
                .overloaded_turn_error(10, "API Error: Overloaded"),
            Some(budget(vec![window(0, 3_600)])),
            Some(ResumeArm::Overloaded {
                overloaded_at: ago(10),
            }),
        ),
        (
            "server-error park",
            agent("claude", "busy", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .overloaded_turn_error(10, temporary_500),
            None,
            Some(ResumeArm::Overloaded {
                overloaded_at: ago(10),
            }),
        ),
        (
            "stalled-stream park",
            agent("claude", "busy", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .turn_error_class(
                    10,
                    STALL_LABEL,
                    TurnErrorClass::classify_label(Some(STALL_LABEL)),
                ),
            None,
            Some(ResumeArm::Overloaded {
                overloaded_at: ago(10),
            }),
        ),
    ] {
        assert_eq!(arm(&agent, budget.as_ref()), expected, "{label}");
    }
}

#[test]
fn does_not_arm_without_a_spent_reset_certificate() {
    for (label, agent, budget, expected) in [
        (
            "spend limit with no window",
            agent("claude", "limited", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .spend_limit_turn_error(10, "You've hit your monthly spend limit."),
            None,
            None,
        ),
        // The fused account budget is the decision input. A paused agent can keep a
        // frozen per-session 100% reading, but once the account bar has recovered
        // there is no new reset deadline to arm.
        (
            "recovered fused budget",
            agent("claude", "limited", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .limits(vec![window(100, -60)])
                .paused_turn_error(10, "You've hit your usage limit"),
            Some(budget(vec![window(0, 3_600)])),
            None,
        ),
        // Once the agent reports a non-spent window, there is no spent reset
        // certificate left to distinguish a missed park from a stale marker.
        (
            "refilled reading",
            agent("claude", "limited", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .limits(vec![window(20, -60)])
                .paused_turn_error(10, "You've hit your usage limit"),
            Some(budget(vec![window(20, -60)])),
            None,
        ),
        // A kind whose budget is spent but whose agent never stopped on a limit keeps
        // working; there is no park to resume.
        (
            "calm agent with spent window",
            agent("claude", "calm", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(5)
                .limits(vec![window(100, 3_600)]),
            Some(budget(vec![window(100, 3_600)])),
            None,
        ),
        (
            "failed park",
            agent("claude", "failed", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .turn_error(10, "API Error: Bad Request"),
            Some(budget(vec![window(100, 3_600)])),
            None,
        ),
    ] {
        assert_eq!(arm(&agent, budget.as_ref()), expected, "{label}");
    }
}
