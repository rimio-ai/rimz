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
    let snapshot = room(Vec::new(), vec![session])
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
fn stalled_running_agent_escalates_to_attention() {
    // A running agent that records no activity past the stall window is
    // likely wedged; the displayed row escalates to the attention bucket
    // (`!`) and the rollup keeps the true `running` status.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(default_stall_secs() + 60);

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "a long-silent running agent escalates to the attention bucket"
    );
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Failed && count.count == 1),
        "the stalled agent counts in the attention tally"
    );
    let rolled_up = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "live-claude")
        .expect("agent in rollup");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the true running status; only the display row escalates"
    );
}

#[test]
fn configured_stall_window_controls_running_attention_escalation() {
    let project = |age_secs| {
        let session = agent("claude", "live-claude", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .in_pane("%1")
            .active_ago(age_secs);
        let mut snapshot = room(Vec::new(), vec![session]);
        snapshot.sidebar.attention.stalled_after_secs =
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
fn displayed_status_precedence_ladder_holds() {
    let spent = || vec![window(100, 3_600)];
    let stalled_secs = default_stall_secs() + 60;

    struct Rung {
        name: &'static str,
        agent: AgentState,
        with_live_child: bool,
        expect: AgentStatus,
        expect_error_label: bool,
    }
    let rungs = [
        // The top of the ladder: a human-blocked ask outranks every derived
        // state at once — the human is the blocker, not the provider or a
        // wedged turn, so no projection may repaint the row out from under
        // the pending decision.
        Rung {
            name: "waiting outranks paused marker + child exemption + stall",
            agent: agent("claude", "root", AgentStatus::Waiting, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(spent())
                .paused_turn_error(10, "You've hit your usage limit"),
            with_live_child: true,
            expect: AgentStatus::Waiting,
            expect_error_label: false,
        },
        // Every derived cause at once: the per-agent pause marker wins over
        // child exemption and stall.
        Rung {
            name: "paused marker beats child exemption + stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(spent())
                .paused_turn_error(10, "You've hit your usage limit"),
            with_live_child: true,
            expect: AgentStatus::Paused,
            expect_error_label: false,
        },
        // Provider overload has no reset window; the marker parks until the next
        // hook event self-clears it.
        Rung {
            name: "overloaded marker parks without budget data",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(60)
                .overloaded_turn_error(10, "API Error: Overloaded"),
            with_live_child: false,
            expect: AgentStatus::Paused,
            expect_error_label: false,
        },
        // The rate-limit wait has passed: a still-dead paused row becomes an
        // actionable failure so the user can resume it.
        Rung {
            name: "rate-limit marker fails after reset",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(vec![window(100, -60)])
                .paused_turn_error(10, "You've hit your usage limit"),
            with_live_child: false,
            expect: AgentStatus::Failed,
            expect_error_label: true,
        },
        // Without a pause marker, the exemption decides next: a live child
        // holds the row at running over both the marker and the stall.
        Rung {
            name: "child exemption beats marker + stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .turn_error(10, "API Error: Server Error"),
            with_live_child: true,
            expect: AgentStatus::Running,
            expect_error_label: false,
        },
        // No pause, no child: the explicit failed marker beats the stall window.
        Rung {
            name: "failed marker beats stall",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .turn_error(10, "API Error: Server Error"),
            with_live_child: false,
            expect: AgentStatus::Failed,
            expect_error_label: true,
        },
        // A stalled running turn with a spent account is the fallback pause.
        Rung {
            name: "stalled spent fallback pauses",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs)
                .limits(spent()),
            with_live_child: false,
            expect: AgentStatus::Paused,
            expect_error_label: false,
        },
        // Nothing above holds: the stall backstop escalates on its own.
        Rung {
            name: "stall is the backstop",
            agent: agent("claude", "root", AgentStatus::Running, 0)
                .worktree("/repo/main")
                .active_ago(stalled_secs),
            with_live_child: false,
            expect: AgentStatus::Failed,
            expect_error_label: false,
        },
    ];

    for rung in rungs {
        let mut agents = vec![rung.agent.in_pane("%1")];
        if rung.with_live_child {
            agents.push(child_state("root", "child-1", AgentStatus::Running, 5));
        }
        let snapshot =
            room(Vec::new(), agents).with_live_panes(vec![pane("%1", "node", "/repo/main")], None);
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
