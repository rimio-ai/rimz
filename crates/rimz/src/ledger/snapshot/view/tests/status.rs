use super::*;

// ── The displayed-status ladder: stall, rate-limit park, turn death ──────────

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
fn idle_agent_in_spent_account_is_not_paused() {
    // A spent account reading is budget display, not a row-wide park. Calm
    // agents stay calm, and a running agent that is still inside the stall
    // window keeps working.
    let reporter = agent("claude", "sess-spent", AgentStatus::Success, 1_000)
        .worktree("/repo/main")
        .limits(vec![window(100, 3_600)]);
    let fresh = agent("claude", "sess-fresh", AgentStatus::Idle, 1_100).worktree("/repo/main");
    let working = agent("claude", "sess-busy", AgentStatus::Running, 1_200).worktree("/repo/main");

    let snapshot = room_with_agent_panes(Vec::new(), vec![reporter, fresh, working]);
    assert_eq!(
        row(&snapshot, "sess-spent").status(),
        Some(AgentStatus::Success)
    );
    assert_eq!(
        row(&snapshot, "sess-fresh").status(),
        Some(AgentStatus::Idle),
        "a fresh idle session does not inherit an account-wide park"
    );
    assert_eq!(
        row(&snapshot, "sess-busy").status(),
        Some(AgentStatus::Running),
        "a live running session is not paused until it stalls or carries a marker"
    );
}

#[test]
fn a_window_spent_but_already_reset_does_not_park() {
    // A spent reading whose reset has passed is stale, not limiting — the
    // budget has refilled, so a resting agent reads idle, not parked.
    let idle = agent("claude", "sess-1", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .limits(vec![window(100, -60)]);

    let snapshot = room_with_agent_panes(Vec::new(), vec![idle]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status(),
        Some(AgentStatus::Idle),
        "a passed reset means the budget refilled — not paused"
    );
}

#[test]
fn running_parent_with_a_live_subagent_waits_instead_of_stalling() {
    // A running parent that has delegated to a live child shows no heartbeat
    // of its own, so the stall window would falsely escalate it. The
    // delegated-wait exemption keeps it `running` while a child runs; the
    // renderer paints the waiting-on-subagents head from `sub_agents`.
    let parent = agent("claude", "root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        // Silent past the stall window — its heartbeat is quiet because the
        // work is the child's, not a wedge.
        .active_ago(default_stall_secs() + 60);
    let child = child_state("root", "child-1", AgentStatus::Running, 5);

    let snapshot = room(Vec::new(), vec![parent, child])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "a parent delegating to a live child is waiting on it, not stalled"
    );
    assert!(
        row.sub_agents()
            .iter()
            .any(|child| child.status == AgentStatus::Running),
        "the live child is nested so the renderer can paint the wait head"
    );
}

#[test]
fn api_error_turn_escalates_running_to_attention() {
    // A turn that died on a provider API error fires no Stop hook, so the
    // rollup keeps `running` — but the transcript marker postdates the
    // agent's own activity, and the projection escalates at once. The
    // headline: the agent is *inside* the stall window (silent only a
    // minute), so this beats the configured backstop.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .turn_error(10, "API Error: Server Error");

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Failed),
        "the explicit death certificate escalates without waiting out the stall window"
    );
    assert_eq!(
        row.turn_error_label(),
        Some("API Error: Server Error"),
        "the row carries the upstream error text for the card's line 2"
    );
    assert!(
        snapshot.worktree_groups[0]
            .status_counts
            .iter()
            .any(|count| count.status == AgentStatus::Failed && count.count == 1),
        "the dead turn counts in the attention tally"
    );
    let rolled_up = snapshot
        .agents
        .iter()
        .find(|a| a.agent_id == "live-claude")
        .expect("agent in rollup");
    assert_eq!(
        rolled_up.status,
        AgentStatus::Running,
        "the rollup keeps the agent-owned status; only the display row escalates"
    );
}

#[test]
fn api_error_self_clears_when_activity_resumes() {
    // Any newer hook event (a prompt, a resume, a rewind) advances
    // `last_activity` past the stale marker and the escalation drops with
    // no human action — the self-clear guard.
    let session = agent("claude", "live-claude", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(30)
        .overloaded_turn_error(120, "API Error: Overloaded");

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "activity newer than the marker means the session moved on"
    );
    assert!(
        row.turn_error_label().is_none(),
        "a cleared escalation leaves no stale reason label"
    );
}

#[test]
fn api_error_does_not_override_waiting() {
    // A human-blocked ask outranks every derived state, the dead-turn
    // escalation included.
    let session = agent("claude", "live-claude", AgentStatus::Waiting, 0)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .paused_turn_error(10, "You've hit your usage limit");

    let snapshot = room(Vec::new(), vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert!(row.turn_error_label().is_none());
}

#[test]
fn dead_parent_with_live_child_keeps_running() {
    // The delegated-wait exemption wins: a live child's heartbeats are the
    // parent's work, so a failed parent marker never escalates over it. If
    // the children also die, the stall window remains the backstop.
    let parent = agent("claude", "root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1")
        .active_ago(60)
        .turn_error(10, "API Error: Server Error");
    let child = child_state("root", "child-1", AgentStatus::Running, 5);

    let snapshot = room(Vec::new(), vec![parent, child])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert_eq!(row.status(), Some(AgentStatus::Running));
    assert!(row.turn_error_label().is_none());
}

// ── The precedence ladder, pinned as an ordering ─────────────────────────────
//
// docs/internals/agent.md commits to a strict order among the derived display
// states: a human-blocked `waiting` outranks them all, then a paused-class
// marker, then the live-subagent exemption, then a failed marker, then the
// stalled-running fallback (paused when the kind's window is spent, failed
// otherwise). The single-cause cases above each prove one rung; this grid pins
// the order by stacking causes.

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

#[test]
fn fallback_paused_predicate() {
    let stalled_spent = agent("claude", "stalled-spent", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .limits(vec![window(100, 3_600)])
        .active_ago(default_stall_secs() + 60);
    let stalled_fresh = agent("codex", "stalled-fresh", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .limits(vec![window(80, 3_600)])
        .active_ago(default_stall_secs() + 60);
    let active_spent = agent("claude", "active-spent", AgentStatus::Running, 0)
        .worktree("/repo/main")
        .limits(vec![window(100, 3_600)])
        .active_ago(60);

    let snapshot =
        room_with_agent_panes(Vec::new(), vec![stalled_spent, stalled_fresh, active_spent]);
    assert_eq!(
        row(&snapshot, "stalled-spent").status(),
        Some(AgentStatus::Paused),
        "stalled running plus a spent window pauses"
    );
    assert_eq!(
        row(&snapshot, "stalled-fresh").status(),
        Some(AgentStatus::Failed),
        "stalled running with budget data below the cap fails"
    );
    assert_eq!(
        row(&snapshot, "active-spent").status(),
        Some(AgentStatus::Running),
        "spent budget alone does not pause an active turn"
    );
}

#[test]
fn paused_rate_limit_marker_lifts_only_after_every_spent_window_resets() {
    for case in [
        (
            "spent window parks the affected agent",
            vec![window(100, 3_600)],
            AgentStatus::Paused,
            None,
        ),
        (
            "after reset the still-dead turn becomes resumable failure",
            vec![window(100, -60)],
            AgentStatus::Failed,
            Some("You've hit your usage limit"),
        ),
        (
            "reset short window waits for longer spent window",
            vec![window(100, -60), window(100, 86_400)],
            AgentStatus::Paused,
            None,
        ),
    ] {
        let (label, windows, expected_status, expected_error_label) = case;
        let session = agent("claude", "limited-dead", AgentStatus::Running, 0)
            .worktree("/repo/main")
            .active_ago(60)
            .limits(windows)
            .paused_turn_error(10, "You've hit your usage limit");

        let snapshot = room_with_agent_panes(Vec::new(), vec![session]);
        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.status(), Some(expected_status), "{label}");
        assert_eq!(row.turn_error_label(), expected_error_label, "{label}");
    }
}

#[test]
fn running_parent_with_live_child_in_spent_account_parks() {
    // Children share the parent's provider turn. A spent account alone does not
    // park the parent, but an explicit paused marker says the provider stopped
    // the turn and outranks the delegated-wait exemption.
    let parent = agent("claude", "root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .limits(vec![window(100, 3_600)])
        .paused_turn_error(10, "You've hit your usage limit");
    let child = child_state("root", "child-1", AgentStatus::Running, 5);

    let snapshot = room_with_agent_panes(Vec::new(), vec![parent, child]);
    assert_eq!(
        snapshot.worktree_groups[0].rows[0].status(),
        Some(AgentStatus::Paused),
        "the pause marker parks the delegating parent"
    );
}

// ── Compaction: the transient head and its crash backstop ────────────────────

#[test]
fn compacting_marker_lights_the_head_then_expires() {
    // A fresh compaction marker pulses the head; one older than the window
    // has expired (the crash backstop), so the head returns to its base.
    // The boundary is exact: one second inside the window still pulses, one
    // second past it has expired — a crash mid-compact can never pulse the
    // head forever.
    let fresh = agent("claude", "compacting-now", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .compacting_ago(0);
    let inside = agent("claude", "compacting-inside", AgentStatus::Running, 1_050)
        .worktree("/repo/main")
        .compacting_ago(crate::feed::COMPACTING_WINDOW_SECS - 1);
    let stale = agent("claude", "compacted-long-ago", AgentStatus::Idle, 1_100)
        .worktree("/repo/main")
        .compacting_ago(crate::feed::COMPACTING_WINDOW_SECS + 1);

    let snapshot = room_with_agent_panes(Vec::new(), vec![fresh, inside, stale]);
    assert!(
        row(&snapshot, "compacting-now").compacting(),
        "a fresh marker pulses"
    );
    assert!(
        row(&snapshot, "compacting-inside").compacting(),
        "a marker one second inside the window still pulses"
    );
    assert!(
        !row(&snapshot, "compacted-long-ago").compacting(),
        "a marker past the window has expired"
    );
}

#[test]
fn compaction_event_stamps_then_a_later_event_clears_the_marker() {
    // The reducer treats a `compacting` event as a transient: it stamps
    // `compacting_since` and keeps the prior status (not a transition); the
    // next lifecycle event means compaction is done and clears the marker.
    let ws = workspace();
    let prompt = lifecycle_at(
        &ws,
        "claude",
        "UserPromptSubmit",
        "sess-1",
        LifecycleSignal::TurnStarted,
    );
    let compact = lifecycle_at(
        &ws,
        "claude",
        "PreCompact",
        "sess-1",
        LifecycleSignal::Compacting,
    );
    let after_compact = reduce_agent_states(&[prompt.clone(), compact.clone()]);
    assert!(
        after_compact[0].compacting_since.is_some(),
        "the compaction marker is stamped"
    );
    assert_eq!(
        after_compact[0].status,
        AgentStatus::Running,
        "compaction keeps the prior status — it is not a transition"
    );

    let stop = lifecycle_at(
        &ws,
        "claude",
        "Stop",
        "sess-1",
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
    );
    let after_stop = reduce_agent_states(&[prompt, compact, stop]);
    assert!(
        after_stop[0].compacting_since.is_none(),
        "a later lifecycle event clears a missed terminator"
    );
    assert_eq!(after_stop[0].status, AgentStatus::Success);
}

#[test]
fn compacting_head_clears_on_post_compact() {
    let ws = workspace();
    let prompt = lifecycle_at(
        &ws,
        "codex",
        "UserPromptSubmit",
        "sess-1",
        LifecycleSignal::TurnStarted,
    );
    let compact = lifecycle_at(
        &ws,
        "codex",
        "PreCompact",
        "sess-1",
        LifecycleSignal::Compacting,
    );
    let post = lifecycle_at(
        &ws,
        "codex",
        "PostCompact",
        "sess-1",
        LifecycleSignal::CompactionEnded { auto: Some(true) },
    );

    let after_post = reduce_agent_states(&[prompt, compact, post]);
    assert!(
        after_post[0].compacting_since.is_none(),
        "the explicit trailing hook clears the marker"
    );
    let snapshot = room_with_agent_panes(Vec::new(), after_post);
    assert!(
        !row(&snapshot, "sess-1").compacting(),
        "projection has no head left to paint"
    );
}

#[test]
fn compaction_end_stays_orthogonal_to_display_status() {
    let ws = workspace();
    let auto = reduce_agent_states(&[
        lifecycle_at(
            &ws,
            "codex",
            "UserPromptSubmit",
            "auto",
            LifecycleSignal::TurnStarted,
        ),
        lifecycle_at(
            &ws,
            "codex",
            "PreCompact",
            "auto",
            LifecycleSignal::Compacting,
        ),
        lifecycle_at(
            &ws,
            "codex",
            "PostCompact",
            "auto",
            LifecycleSignal::CompactionEnded { auto: Some(true) },
        ),
    ])
    .remove(0)
    .worktree("/repo/main")
    .limits(vec![window(100, 3_600)]);
    let manual = reduce_agent_states(&[
        lifecycle_at(
            &ws,
            "codex",
            "UserPromptSubmit",
            "manual",
            LifecycleSignal::TurnStarted,
        ),
        lifecycle_at(
            &ws,
            "codex",
            "PreCompact",
            "manual",
            LifecycleSignal::Compacting,
        ),
        lifecycle_at(
            &ws,
            "codex",
            "PostCompact",
            "manual",
            LifecycleSignal::CompactionEnded { auto: Some(false) },
        ),
    ])
    .remove(0)
    .worktree("/repo/main")
    .active_ago(default_stall_secs() + 10);

    let snapshot = room_with_agent_panes(Vec::new(), vec![auto]);
    assert_eq!(
        row(&snapshot, "auto").status(),
        Some(AgentStatus::Running),
        "spent-account projection does not park an auto-resumed row without a pause marker"
    );
    let snapshot = room_with_agent_panes(Vec::new(), vec![manual]);
    assert_eq!(
        row(&snapshot, "manual").status(),
        Some(AgentStatus::Idle),
        "manual compaction rests the row, so it is stall-exempt"
    );
}
