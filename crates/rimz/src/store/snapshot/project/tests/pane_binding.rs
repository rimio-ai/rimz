use super::*;

#[test]
fn lifecycle_reduces_pane_id_and_carries_it_forward() {
    // The hook stamps the mux pane id on every lifecycle event so the
    // reducer can bind each agent to its own pane. A later event that omits
    // pane_id must not unbind the agent.
    let start = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": "sess-1",
            "signal": { "signal": "registered" },
            "pane_id": "tmux:%7",
        }),
    );
    let prompt = raw_lifecycle(
        "claude",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
        }),
    );

    let agents = reduce_agent_states(&[start, prompt]);
    assert_eq!(agents.len(), 1);
    let bound = agents[0].pane.as_ref().expect("pane carries forward");
    assert_eq!(bound.pane_id.raw(), "%7");
}

#[test]
fn rebirth_boundary_unstamps_prior_panes_and_resumes_split_or_whole() {
    // A mux rebirth renumbers panes from zero, so the boundary must clear
    // every stamp recorded before it — while a stamp recorded after it is the
    // new incarnation's and stays, even on the very same reused pane id.
    let workspace = workspace();
    let boundary = EventEnvelope::session_rebirth(workspace.clone(), "session");

    let events = vec![
        stamped_start(&workspace, "sess-old", "zellij:terminal_6"),
        boundary.clone(),
        stamped_start(&workspace, "sess-new", "zellij:terminal_6"),
    ];
    let agents = reduce_agent_states(&events);
    let by_id = |id: &str| {
        agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == id)
            .expect("session folded")
    };
    assert!(
        by_id("sess-old").pane.is_none(),
        "a stamp recorded before the boundary names a dead pane id and clears"
    );
    assert_eq!(
        by_id("sess-new")
            .pane
            .as_ref()
            .expect("fresh stamp")
            .pane_id
            .raw(),
        "terminal_6",
        "the reborn incarnation's stamp on the reused id stays"
    );

    // The cleared session re-stamps the moment a later event names a pane.
    let restamped = reduce_agent_states(&[
        stamped_start(&workspace, "sess-old", "zellij:terminal_6"),
        boundary,
        stamped_start(&workspace, "sess-old", "zellij:terminal_9"),
    ]);
    assert_eq!(
        restamped[0]
            .pane
            .as_ref()
            .expect("re-stamped")
            .pane_id
            .raw(),
        "terminal_9"
    );

    let whole = reduce_agent_states_seeded(BTreeMap::new(), &events);
    let prefix = reduce_agent_states_seeded(BTreeMap::new(), &events[..1]);
    let split = reduce_agent_states_seeded(prefix, &events[1..]);

    assert_eq!(whole, split);
}

fn stamped_start(workspace: &WorkspaceId, agent_id: &str, pane: &str) -> EventEnvelope {
    raw_lifecycle_in(
        workspace,
        "codex",
        serde_json::json!({
            "event_name": "SessionStart",
            "agent_id": agent_id,
            "signal": { "signal": "registered" },
            "pane_id": pane,
        }),
    )
}

#[test]
fn lifecycle_pane_stamp_carries_full_anchor_and_survives_bare_id_events() {
    let mut stamp = crate::pane::PaneRef::from_id(PaneId::parse("tmux:%7").expect("pane id"));
    stamp.session_name = "session".to_owned();
    stamp.view_id = Some("@3".to_owned());
    stamp.cwd = Some("/repo/main".to_owned());
    stamp.pane_pid = Some(42);
    stamp.pane_process_start = Some(Timestamp::from_second(1_750_000_001).unwrap());

    let mut start = crate::agents::AgentLifecycleObservation::new(
        Some("sess-1".into()),
        crate::agents::lifecycle::LifecycleSignal::Registered,
    );
    start.pane_id = Some(stamp.pane_id.clone());
    start.pane_stamp = Some(stamp);
    let start =
        EventEnvelope::agent_lifecycle(workspace(), "session", "codex", "SessionStart", &start);
    let prompt = raw_lifecycle(
        "codex",
        serde_json::json!({
            "event_name": "UserPromptSubmit",
            "agent_id": "sess-1",
            "signal": { "signal": "turn_started" },
            "pane_id": "tmux:%7",
        }),
    );

    let agents = reduce_agent_states(&[start, prompt]);
    let pane = agents[0].pane.as_ref().expect("pane anchor");
    assert_eq!(pane.pane_id.raw(), "%7");
    assert_eq!(pane.view_id.as_deref(), Some("@3"));
    assert_eq!(pane.cwd.as_deref(), Some("/repo/main"));
    assert_eq!(pane.pane_pid, Some(42));
    assert!(
        pane.pane_process_start.is_some(),
        "later bare pane_id event must not downgrade an enriched stamp"
    );
}

#[test]
fn later_daemon_owner_does_not_clobber_pane_process_owner() {
    let owner_event = |kind: &str, pid: u32| {
        raw_lifecycle(
            "codex",
            serde_json::json!({
                "event_name": "SessionStart",
                "agent_id": "sess-1",
                "signal": { "signal": "registered" },
                "agent_pid": pid,
                "runtime_owner": {
                    "kind": kind,
                    "subject_id": "sess-1",
                    "pid": pid,
                },
            }),
        )
    };

    let agents = reduce_agent_states(&[owner_event("agent", 42), owner_event("daemon", 77)]);

    let owner = agents[0].runtime_owner.as_ref().expect("runtime owner");
    assert_eq!(owner.kind, crate::pane::RuntimeOwnerKind::Agent);
    assert_eq!(owner.pid, 42);
}
