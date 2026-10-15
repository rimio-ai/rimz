use super::*;

#[test]
fn picked_session_adopts_new_identity_or_revives_closed_identity() {
    for (kind, signal) in [("claude", "registered"), ("codex", "turn_started")] {
        for (recorded, old_pane) in [(false, None), (true, None), (true, Some("tmux:%3"))] {
            let mut events = Vec::new();
            if recorded {
                events.push(launch_event(
                    kind,
                    AgentLaunchPayload {
                        description: Some("saved history".into()),
                        launch_id: Some("launch_old".into()),
                        pane_id: old_pane.map(|pane| PaneId::parse(pane).unwrap()),
                        ..launch_payload("picked-id", "old-handle")
                    },
                ));
                let mut child = launch_payload("child-1", "child-handle");
                child.launch_id = Some("launch_child".into());
                child.pane_id = Some(PaneId::parse("tmux:%8").unwrap());
                child.launch.parent_agent_id = Some("launch_old".into());
                child.launch.launch_depth = Some(1);
                events.push(launch_event(kind, child));
                let mut peer = launch_payload("peer-1", "peer-handle");
                peer.launch_id = Some("launch_peer".into());
                peer.launch.launched_by = Some(Box::new(crate::agents::LaunchedBy {
                    kind: AgentKind::new_unchecked(kind),
                    agent_id: "launch_old".into(),
                }));
                events.push(launch_event(kind, peer));
                events.push(raw_lifecycle_at(
                    kind,
                    1,
                    json!({
                        "agent_id": "picked-id", "signal": {"signal": "ended"}
                    }),
                ));
            }
            events.push(launch_event(
                kind,
                AgentLaunchPayload {
                    pane_id: Some(PaneId::parse("tmux:%7").unwrap()),
                    launch_id: Some("launch_picker".into()),
                    prompt: None,
                    ..launch_payload("launch_picker", "new-handle")
                },
            ));
            events.push(raw_lifecycle_at(
                kind,
                2,
                json!({
                    "agent_id": "picked-id", "agent_name": "new-handle",
                    "pane_id": "tmux:%7", "signal": {"signal": signal}
                }),
            ));
            let agents = reduce_agent_states(&events);
            assert_eq!(
                agents.len(),
                if recorded { 3 } else { 1 },
                "{kind}, recorded={recorded}, old_pane={old_pane:?}"
            );
            let agent = agents
                .iter()
                .find(|agent| agent.agent_id == "picked-id")
                .unwrap();
            assert_eq!(agent.agent_id.as_str(), "picked-id");
            assert_eq!(
                crate::address::launch_row(
                    &agents,
                    &agent.kind,
                    &AgentSessionId::from("launch_picker"),
                )
                .map(|row| row.agent_id.as_str()),
                Some("picked-id"),
                "{kind}, recorded={recorded}"
            );
            assert_eq!(
                agent.name.as_deref(),
                Some(if recorded { "old-handle" } else { "new-handle" })
            );
            assert_eq!(agent.pane.as_ref().unwrap().pane_id.as_str(), "tmux:%7");
            assert!(agent.ended_at.is_none());
            if recorded {
                assert_eq!(agent.description.as_deref(), Some("saved history"));
                assert_eq!(
                    crate::address::launch_row(&agents, &agent.kind, &"launch_old".into())
                        .map(|row| row.agent_id.as_str()),
                    Some("picked-id")
                );
                let child = agents.iter().find(|row| row.agent_id == "child-1").unwrap();
                assert!(child.parent_is(agent));
                assert_eq!(
                    crate::address::launched_parent(&agents, child)
                        .map(|row| row.agent_id.as_str()),
                    Some("picked-id")
                );
                assert!(crate::address::launched_children(&agents, agent).contains(&child));
                let peer = agents.iter().find(|row| row.agent_id == "peer-1").unwrap();
                assert!(peer.launcher_is(agent));
                assert!(crate::address::launched_fleet(&agents, agent).contains(&peer));
            }
        }
    }
}

#[test]
fn repeated_picker_revives_keep_children_and_exact_resume_keeps_prior_ids() {
    for (kind, signal) in [("claude", "registered"), ("codex", "turn_started")] {
        let mut events = vec![launch_event(
            kind,
            AgentLaunchPayload {
                launch_id: Some("launch_old".into()),
                pane_id: Some(PaneId::parse("tmux:%3").unwrap()),
                ..launch_payload("picked-id", "old-handle")
            },
        )];
        for (prior, next, pane, child_id, child_name) in [
            (
                "launch_old",
                "launch_p1",
                "tmux:%7",
                "child-1",
                "first-child",
            ),
            (
                "launch_p1",
                "launch_p2",
                "tmux:%9",
                "child-2",
                "second-child",
            ),
        ] {
            let mut child = launch_payload(child_id, child_name);
            child.launch.parent_agent_id = Some(prior.into());
            child.launch.launch_depth = Some(1);
            child.pane_id = Some(PaneId::parse("tmux:%8").unwrap());
            events.push(launch_event(kind, child));
            events.push(raw_lifecycle_at(
                kind,
                1,
                json!({"agent_id": "picked-id", "signal": {"signal": "ended"}}),
            ));
            events.push(launch_event(
                kind,
                AgentLaunchPayload {
                    launch_id: Some(next.into()),
                    pane_id: Some(PaneId::parse(pane).unwrap()),
                    prompt: None,
                    ..launch_payload(next, "new-handle")
                },
            ));
            events.push(raw_lifecycle_at(kind, 2, json!({"agent_id": "picked-id", "agent_name": "new-handle", "pane_id": pane, "signal": {"signal": signal}})));
        }
        let agents = reduce_agent_states(&events);
        assert_eq!(agents.len(), 3, "{kind}: no provisional duplicates");
        let revived = agents
            .iter()
            .find(|row| row.agent_id == "picked-id")
            .unwrap();
        assert_eq!(revived.launch_id.as_deref(), Some("launch_p2"));
        assert_eq!(
            revived.prior_launch_ids,
            vec![AgentSessionId::from("launch_old"), "launch_p1".into()]
        );
        assert_eq!(crate::address::launched_children(&agents, revived).len(), 2);
        for child in agents.iter().filter(|row| row.is_launched_child()) {
            assert!(child.parent_is(revived));
            assert_eq!(
                crate::address::launched_parent(&agents, child),
                Some(revived)
            );
        }
        events.push(attach_event(
            kind,
            "picked-id",
            Some("launch_p2"),
            "tmux:%10",
            None,
            42,
        ));
        let attached = reduce_agent_states(&events);
        let attached = attached
            .iter()
            .find(|row| row.agent_id == "picked-id")
            .unwrap();
        assert_eq!(attached.launch_id, revived.launch_id);
        assert_eq!(attached.prior_launch_ids, revived.prior_launch_ids);
    }
}

#[test]
fn launch_and_attach_rebindings_preserve_prior_ids() {
    let mut events = vec![launch_event(
        "codex",
        AgentLaunchPayload {
            launch_id: Some("launch_old".into()),
            ..launch_payload("session", "coder")
        },
    )];
    events.push(attach_event(
        "codex",
        "session",
        Some("launch_attach"),
        "tmux:%4",
        None,
        42,
    ));
    events.push(launch_event(
        "codex",
        AgentLaunchPayload {
            launch_id: Some("launch_new".into()),
            ..launch_payload("session", "coder")
        },
    ));
    let agents = reduce_agent_states(&events);
    assert_eq!(agents[0].launch_id.as_deref(), Some("launch_new"));
    assert_eq!(
        agents[0].prior_launch_ids,
        vec![AgentSessionId::from("launch_old"), "launch_attach".into()]
    );
}

#[test]
fn successor_inherits_prior_launch_ids() {
    let mut events = resumed_fork_events("tmux:%1");
    events.insert(
        2,
        attach_event(
            "codex",
            "primary",
            Some("launch_previous"),
            "tmux:%1",
            None,
            41,
        ),
    );
    let agents = reduce_agent_states(&events);
    let successor = agents
        .iter()
        .find(|agent| agent.agent_id == "side-thread")
        .unwrap();
    assert_eq!(
        successor.prior_launch_ids,
        vec![AgentSessionId::from("launch_previous")]
    );
}

#[test]
fn live_successor_keeps_its_launch_after_predecessor_picker_revive() {
    let mut events = resumed_fork_events("tmux:%1");
    events.push(launch_event(
        "codex",
        AgentLaunchPayload {
            pane_id: Some(PaneId::parse("tmux:%7").unwrap()),
            launch_id: Some("launch_picker".into()),
            prompt: None,
            ..launch_payload("launch_picker", "picker-card")
        },
    ));
    events.push(raw_lifecycle(
        "codex",
        json!({
            "agent_id": "primary", "pane_id": "tmux:%7",
            "signal": {"signal": "turn_started"}
        }),
    ));
    for (index, event) in events.iter_mut().enumerate().skip(6) {
        event.timestamp = Timestamp::from_second(epoch().as_second() + index as i64).unwrap();
    }
    let agents = reduce_agent_states(&events);
    let primary = agents.iter().find(|row| row.agent_id == "primary").unwrap();
    let fork = agents
        .iter()
        .find(|row| row.agent_id == "side-thread")
        .unwrap();
    assert!(primary.ended_at.is_none() && primary.holds_open_turn());
    assert_eq!(
        primary.prior_launch_ids,
        vec![AgentSessionId::from("launch_coder")]
    );
    let codex = AgentKind::new_unchecked("codex");
    assert_eq!(
        crate::address::launch_row(&agents, &codex, &"launch_coder".into())
            .map(|row| row.agent_id.as_str()),
        Some("side-thread")
    );
    assert!(crate::address::is_launch_row(&agents, fork));
    assert!(crate::address::is_launch_row(&agents, primary));
}

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
