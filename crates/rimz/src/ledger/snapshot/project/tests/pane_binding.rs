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
fn rebirth_boundary_unstamps_prior_panes_in_log_order() {
    // A mux rebirth renumbers panes from zero, so the boundary must clear
    // every stamp recorded before it — while a stamp recorded after it is the
    // new incarnation's and stays, even on the very same reused pane id.
    let workspace = project_workspace();
    let boundary = EventEnvelope::session_rebirth(workspace.clone(), "session");

    let agents = reduce_agent_states(&[
        stamped_start(&workspace, "sess-old", "zellij:terminal_6"),
        boundary.clone(),
        stamped_start(&workspace, "sess-new", "zellij:terminal_6"),
    ]);
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
}

#[test]
fn rebirth_boundary_folds_identically_split_or_whole() {
    // The seeded-resume property the incremental fold stands on holds across
    // the boundary: folding the tail (boundary included) onto the prefix's map
    // equals folding the whole log from scratch.
    let workspace = project_workspace();
    let events = vec![
        stamped_start(&workspace, "sess-old", "zellij:terminal_6"),
        EventEnvelope::session_rebirth(workspace.clone(), "session"),
        stamped_start(&workspace, "sess-new", "zellij:terminal_6"),
    ];

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
