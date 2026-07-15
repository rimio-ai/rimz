use super::*;

#[test]
fn stalled_running_agent_recovers_when_activity_resumes() {
    // The stall escalation is self-healing: once the agent's next completed
    // tool touches the activity heartbeat, the fold readvances
    // `last_activity`, `is_stalled` goes false, and the row drops back out
    // of attention with no human action.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        // Silent past the stall window.
        .active_ago(default_stall_secs() + 60);

    // A fresh heartbeat lands (the agent's next tool completed).
    let touch = AgentActivity {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: "live-claude".into(),
        at: epoch(),
    };
    let snapshot = room(vec![session])
        .with_agent_activity(&[touch])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "a fresh heartbeat readvances last_activity, so the stalled row recovers"
    );
}

#[test]
fn configured_stall_window_controls_running_attention_escalation() {
    let project = |age_secs| {
        let session = agent("claude", "live-claude", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .in_pane("%1")
            .active_ago(age_secs);
        let mut snapshot = room(vec![session]);
        snapshot.attention.stalled_after_secs =
            std::num::NonZeroU32::new(120).expect("non-zero test window");
        snapshot.with_live_panes(vec![pane("%1", "node", "/repo/main")], None)
    };

    assert_eq!(
        project(119).worktree_groups[0].rows[0].status(),
        Some(AgentStatus::Running),
        "a running agent below the configured window stays live"
    );
    assert_eq!(
        project(120).worktree_groups[0].rows[0].status(),
        Some(AgentStatus::Failed),
        "a running agent at the configured window escalates to `!`"
    );
}

#[test]
fn quiet_parked_agent_settles_after_the_stall_window() {
    let project = |age_secs| {
        let session = agent("claude", "live-claude", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .in_pane("%1")
            .parked()
            .active_ago(age_secs);
        room(vec![session]).with_live_panes(vec![pane("%1", "node", "/repo/main")], None)
    };

    let fresh = project(default_stall_secs() - 1);
    let fresh_row = &fresh.worktree_groups[0].rows[0];
    assert_eq!(fresh_row.status(), Some(AgentStatus::Running));
    assert_eq!(fresh_row.phase(), TurnPhase::Parked);

    let quiet = project(default_stall_secs());
    let quiet_row = &quiet.worktree_groups[0].rows[0];
    assert_eq!(quiet_row.status(), Some(AgentStatus::Success));
    assert_eq!(quiet_row.phase(), TurnPhase::Idle);
}

// ── The precedence ladder, pinned as an ordering ─────────────────────────────
//
// docs/internals/agents/model.md commits to a strict order among the derived display
// states: a human-blocked `waiting` outranks them all, then a paused-class
// marker, then a failed marker, then the live-subagent exemption, then settled
// completion/interruption markers, then the stalled parked settle, then the
// stalled-running fallback (paused when the kind's window is spent, failed
// otherwise). The single-cause cases each prove one rung; this grid pins the
// error/stall order by stacking causes, and the turn_complete/turn_interrupted
// modules pin the settle rungs.
#[test]
fn displayed_status_precedence_ladder_holds() {
    for rung in displayed_status_rungs() {
        let mut agents = vec![rung.agent.in_pane("%1")];
        if rung.with_live_child {
            agents.push(child_state("root", "child-1", AgentStatus::Running, 5));
        }
        let snapshot = room(agents).with_live_panes_and_provider_capacities(
            vec![pane("%1", "node", "/repo/main")],
            None,
            &provider_capacity("claude", rung.budget_windows),
        );
        let row = row(&snapshot, "root");
        assert_eq!(
            row.status(),
            Some(rung.expect),
            "precedence rung: {}",
            rung.name
        );
        assert_eq!(
            row.turn_error_label().is_some(),
            rung.expect_error_label,
            "error-label rung: {}",
            rung.name
        );
    }
}

struct StatusRung {
    name: &'static str,
    agent: AgentState,
    with_live_child: bool,
    budget_windows: Vec<RateLimitWindow>,
    expect: AgentStatus,
    expect_error_label: bool,
}

fn displayed_status_rungs() -> Vec<StatusRung> {
    let stalled_secs = default_stall_secs() + 60;
    vec![
        StatusRung {
            name: "waiting outranks paused marker + child exemption + stall",
            agent: agent("claude", "root", AgentStatus::Waiting, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(spent_windows())
                .paused_turn_error(10, "You've hit your usage limit"),
            with_live_child: true,
            budget_windows: spent_windows(),
            expect: AgentStatus::Waiting,
            expect_error_label: false,
        },
        StatusRung {
            name: "paused marker beats child exemption + stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(spent_windows())
                .paused_turn_error(10, "You've hit your usage limit"),
            with_live_child: true,
            budget_windows: spent_windows(),
            expect: AgentStatus::Paused,
            expect_error_label: false,
        },
        StatusRung {
            name: "overloaded marker parks without budget data",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .overloaded_turn_error(10, "API Error: Overloaded"),
            with_live_child: false,
            budget_windows: Vec::new(),
            expect: AgentStatus::Paused,
            expect_error_label: false,
        },
        StatusRung {
            name: "rate-limit marker fails after reset",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(vec![window(100, -60)])
                .paused_turn_error(10, "You've hit your usage limit"),
            with_live_child: false,
            budget_windows: vec![unprojectable_spent_window(-60)],
            expect: AgentStatus::Failed,
            expect_error_label: true,
        },
        StatusRung {
            name: "failed marker beats child exemption + stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .turn_error(10, "API Error: Bad Request"),
            with_live_child: true,
            budget_windows: Vec::new(),
            expect: AgentStatus::Failed,
            expect_error_label: true,
        },
        StatusRung {
            name: "failed marker beats stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .turn_error(10, "API Error: Bad Request"),
            with_live_child: false,
            budget_windows: Vec::new(),
            expect: AgentStatus::Failed,
            expect_error_label: true,
        },
        StatusRung {
            name: "failed marker beats parked settle",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .parked()
                .active_ago(stalled_secs)
                .turn_error(10, "API Error: Bad Request"),
            with_live_child: false,
            budget_windows: Vec::new(),
            expect: AgentStatus::Failed,
            expect_error_label: true,
        },
        StatusRung {
            name: "parked settle beats stalled spent fallback",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .parked()
                .active_ago(stalled_secs)
                .limits(spent_windows()),
            with_live_child: false,
            budget_windows: spent_windows(),
            expect: AgentStatus::Success,
            expect_error_label: false,
        },
        StatusRung {
            name: "stalled spent fallback pauses",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(spent_windows()),
            with_live_child: false,
            budget_windows: spent_windows(),
            expect: AgentStatus::Paused,
            expect_error_label: false,
        },
        StatusRung {
            name: "stall is the backstop",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs),
            with_live_child: false,
            budget_windows: Vec::new(),
            expect: AgentStatus::Failed,
            expect_error_label: false,
        },
    ]
}

fn spent_windows() -> Vec<RateLimitWindow> {
    vec![window(100, 3_600)]
}
