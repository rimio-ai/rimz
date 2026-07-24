use super::*;

fn incident_lifecycle(
    secs_ago: u64,
    event_name: &str,
    agent_id: &str,
    signal: LifecycleSignal,
    configure: impl FnOnce(&mut crate::agents::AgentLifecycleObservation),
) -> crate::store::event::EventEnvelope {
    let mut observation =
        crate::agents::AgentLifecycleObservation::new(Some(agent_id.into()), signal);
    observation.pane_id = Some(crate::ids::PaneId::parse("tmux:%1").unwrap());
    observation.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        "codex",
        4242,
        Some("process-start".to_owned()),
    ));
    observation.worktree_path = Some("/repo/main".to_owned());
    configure(&mut observation);
    let mut event = crate::store::event::EventEnvelope::agent_lifecycle(
        workspace(),
        "session",
        "codex",
        event_name,
        &observation,
    );
    event.timestamp = epoch() - std::time::Duration::from_secs(secs_ago);
    event
}

fn compact_incident(with_side_fork: bool) -> SidebarSnapshot {
    let mut events = vec![incident_lifecycle(
        4,
        "Stop",
        "predecessor",
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
        },
        |observation| observation.origin = Some(crate::agents::SessionOrigin::Fresh),
    )];
    if with_side_fork {
        events.push(incident_lifecycle(
            3,
            "SessionStart",
            "side",
            LifecycleSignal::Registered,
            |observation| observation.origin = Some(crate::agents::SessionOrigin::Forked),
        ));
    }
    events.push(incident_lifecycle(
        2,
        "SessionStart",
        "continuation",
        LifecycleSignal::CompactionEnded { auto: None },
        |observation| {
            observation.origin = Some(crate::agents::SessionOrigin::Forked);
            observation.compacted_from = Some("predecessor".into());
        },
    ));
    events.push(incident_lifecycle(
        1,
        "UserPromptSubmit",
        "continuation",
        LifecycleSignal::TurnStarted,
        |observation| {
            observation.prompt = Some("continue".to_owned());
            observation.usage.total_tokens = Some(123);
        },
    ));

    let mut snapshot = room(reduce_agent_states(&events));
    snapshot.reap_stale_sessions();
    snapshot.with_live_panes(vec![pane("%1", "codex", "/repo/main")], None)
}

#[test]
fn codex_compaction_fork_promotes_the_continuation() {
    let snapshot = compact_incident(false);

    assert_eq!(snapshot.agents.len(), 1);
    let continuation = rollup_agent(&snapshot, "continuation");
    assert_eq!(continuation.status, AgentStatus::Running);
    assert_eq!(continuation.prompt.as_deref(), Some("continue"));
    assert_eq!(continuation.usage.total_tokens, Some(123));
    assert_eq!(rows(&snapshot).len(), 1);
    assert_eq!(rows(&snapshot)[0].id, "continuation");
}

#[test]
fn codex_compaction_continuation_keeps_primacy_over_an_older_side_fork() {
    let snapshot = compact_incident(true);

    assert_eq!(snapshot.agents.len(), 2, "the side fork remains durable");
    assert_eq!(rows(&snapshot).len(), 1);
    assert_eq!(rows(&snapshot)[0].id, "continuation");
}

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
        .compacting_ago(crate::agents::COMPACTING_WINDOW_SECS - 1);
    let stale = agent("claude", "compacted-long-ago", AgentStatus::Idle, 1_100)
        .worktree("/repo/main")
        .compacting_ago(crate::agents::COMPACTING_WINDOW_SECS + 1);

    let snapshot = room_with_agent_panes(vec![fresh, inside, stale]);
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
    assert_eq!(
        after_stop[0].compaction_count, 1,
        "the display head can expire, but the next signal still closes and counts the bracket"
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

    let snapshot = room_with_agent_panes(vec![auto]);
    assert_eq!(
        row(&snapshot, "auto").status(),
        Some(AgentStatus::Running),
        "spent-account projection does not park an auto-resumed row without a pause marker"
    );
    let snapshot = room_with_agent_panes(vec![manual]);
    assert_eq!(
        row(&snapshot, "manual").status(),
        Some(AgentStatus::Idle),
        "manual compaction rests the row, so it is stall-exempt"
    );
}
