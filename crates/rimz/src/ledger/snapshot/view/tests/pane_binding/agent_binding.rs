use super::*;

#[test]
fn live_panes_overlay_only_matching_agent_rows() {
    for (label, stamped_pane, pane_command, expect_agent) in [
        (
            "stamped agent overlays matching pane",
            Some("%1"),
            "codex",
            true,
        ),
        (
            "unmatched ledger agent leaves process row",
            None,
            "zsh",
            false,
        ),
    ] {
        let mut codex =
            agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");
        if let Some(raw_pane) = stamped_pane {
            codex = codex.branch("main").in_pane(raw_pane);
        }
        let snapshot = room(Vec::new(), vec![codex])
            .with_live_panes(vec![pane("%1", pane_command, "/repo/main")], None);

        assert_eq!(snapshot.worktree_groups.len(), 1, "{label}");
        assert_eq!(snapshot.worktree_groups[0].rows.len(), 1, "{label}");
        let row = &snapshot.worktree_groups[0].rows[0];
        assert_eq!(row.is_agent(), expect_agent, "{label}");
        assert_eq!(row.is_process(), !expect_agent, "{label}");
        assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1", "{label}");
        if expect_agent {
            assert_eq!(row.id, "sess-1", "{label}");
        } else {
            assert_eq!(row.name, "zsh", "{label}");
        }
    }
}

#[test]
fn stamped_codex_returned_to_shell_renders_process_row() {
    // Codex records lifecycle through the shared app-server daemon, so the
    // session can remain live after the in-pane CLI exits. When the same pane id
    // now reports a shell foreground, the old Codex card must not stay attached.
    let codex = agent("codex", "sess-1", AgentStatus::Success, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let snapshot =
        room(Vec::new(), vec![codex]).with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "zsh");
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn live_panes_suppress_stale_agent_attention_without_process() {
    let item = agent_ask(FeedKind::Permission, "claude", "stale-claude");

    let snapshot = room(vec![item], Vec::new()).with_live_panes(
        vec![
            pane("%0", "rimz-sidebar", "/repo/main"),
            pane("%1", "zsh", "/repo/main"),
        ],
        None,
    );

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .all(|row| row.is_process() && row.name == "zsh"),
        "a stale agent prompt must not claim the sidebar pane or outlive its agent process: {:?}",
        snapshot.worktree_groups[0].rows,
    );
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

#[test]
fn pending_agent_ask_folds_by_stamped_pane_and_preserves_session_description() {
    let item = agent_ask(FeedKind::Permission, "claude", "live-claude");
    let request_id = item.request_id.clone();
    let mut session = agent("claude", "live-claude", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    session.prompt = Some("read architecture docs and map agent state".to_owned());

    let snapshot = room(vec![item], vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    let agent = row.as_agent().expect("ask folds onto the agent card");
    assert_eq!(row.name, "claude");
    assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert_eq!(
        row.status(),
        Some(AgentStatus::Waiting),
        "the ask still marks the row as waiting"
    );
    assert_eq!(row.request_id(), Some(&request_id));
    assert_eq!(
        agent.task.as_deref(),
        None,
        "ask kind is not an activity task"
    );
    assert_eq!(
        agent.prompt.as_deref(),
        Some("read architecture docs and map agent state"),
        "the prompt remains the card's fallback description"
    );
}

#[test]
fn answered_native_ui_ask_returns_to_running() {
    // The live bug: a native_ui ask is answered in the agent's own UI and
    // the agent keeps working the same turn. The ask stays pending in the
    // ledger, but the activity heartbeat has advanced `last_activity` past
    // the ask, so the row must read `running`, not stay folded to `waiting`.
    let mut item = agent_ask(FeedKind::Question, "claude", "live-claude");
    // Ask raised long before the agent's recent activity.
    item.updated_at = ago(600);

    // The agent recorded progress after the ask — it has un-blocked and
    // moved on.
    let session = agent("claude", "live-claude", AgentStatus::Running, 2_000)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(vec![item], vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_agent());
    assert_eq!(
        row.status(),
        Some(AgentStatus::Running),
        "an answered ask the agent moved past must not pin the row to waiting"
    );
}

#[test]
fn agent_binds_only_by_stamped_pane_id() {
    // The pane-keyed invariant: an agent stamped `%2`, but only `%1` is
    // live. `%1`'s command and cwd both match the agent — under the old
    // command/cwd fallback it would have bound. Stamped-id binding refuses
    // it, so `%1` stays a process row and the agent simply does not render.
    let claude = agent("claude", "sess-1", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%2");

    let snapshot = room(Vec::new(), vec![claude])
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_process());
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn each_live_pane_yields_exactly_one_row() {
    // One pane = one row, by construction: every live pane produces exactly
    // one row — agent or process — and no pane id is ever duplicated.
    let stamped = |id, raw| {
        agent("claude", id, AgentStatus::Running, 1_000)
            .worktree("/repo/main")
            .in_pane(raw)
    };

    let snapshot = room(
        Vec::new(),
        vec![stamped("sess-a", "%1"), stamped("sess-b", "%2")],
    )
    .with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "claude", "/repo/main"),
            pane("%3", "zsh", "/repo/main"),
        ],
        None,
    );

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 3, "three panes render three rows: {rows:?}");
    let mut pane_ids: Vec<&str> = rows
        .iter()
        .map(|row| row.pane.as_ref().unwrap().pane_id.raw())
        .collect();
    pane_ids.sort_unstable();
    assert_eq!(pane_ids, vec!["%1", "%2", "%3"], "no pane id is duplicated");
    let agents = rows.iter().filter(|row| row.is_agent()).count();
    assert_eq!(agents, 2, "the two stamped panes bound their agents");
    assert_eq!(
        row(&snapshot, "sess-a")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%1"
    );
    assert_eq!(
        row(&snapshot, "sess-b")
            .pane
            .as_ref()
            .unwrap()
            .pane_id
            .raw(),
        "%2"
    );
}

#[test]
fn live_agent_and_process_rows_are_pane_backed() {
    // In a live-pane fold, every visible top-level row is jumpable: agent
    // rows and process rows both carry a pane. A subagent that shares its
    // parent's pane nests in the parent card instead of becoming a second
    // top-level row with the same pane.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(Vec::new(), vec![parent, child]).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "zsh", "/repo/main"),
        ],
        None,
    );

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 2, "root agent + process pane render two rows");
    assert!(
        rows.iter().all(|row| row.pane.is_some()),
        "every visible live-pane row has a pane: {rows:?}",
    );
    assert!(
        rows.iter().all(|row| row.id != "child-1"),
        "the subagent is not a top-level row",
    );
    let parent = rows
        .iter()
        .find(|row| row.id == "sess-root")
        .expect("parent row present");
    assert_eq!(parent.sub_agents().len(), 1);
    assert_eq!(parent.sub_agents()[0].id, "child-1");
}
