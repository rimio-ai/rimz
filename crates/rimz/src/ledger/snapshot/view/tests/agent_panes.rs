//! `SidebarSnapshot::agent_panes` — the uncapped, binding-site source that
//! command resolution (`steer`) reads. It lists every live agent pane (bound and
//! lazy), carries the producer-bound pane for a cwd-bound session, survives row
//! capping, and never admits a standalone ask that merely shares the agent-card
//! shape.

use super::*;

#[test]
fn cwd_bound_session_lists_its_producer_bound_pane() {
    // A daemon-routed codex carries no stamped pane; the fold binds it to the live
    // pane by cwd, and agent_panes carries that pane so steer can reach it — even
    // though the rollup session itself still has no pane of its own.
    let mut snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    );
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    assert_eq!(snapshot.agent_panes.len(), 1);
    let bound = &snapshot.agent_panes[0];
    assert_eq!(bound.kind.as_str(), "codex");
    assert_eq!(
        bound.agent_id.as_ref().map(|id| id.as_str()),
        Some("sess-1")
    );
    assert_eq!(bound.pane_id.raw(), "term1");
    assert!(
        snapshot.agents[0].pane.is_none(),
        "the rollup session it came from carries no pane of its own"
    );
}

#[test]
fn lazy_pane_lists_without_a_session() {
    // An unprompted wired codex lists as a lazy pane: kind and pane, no session.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    assert_eq!(snapshot.agent_panes.len(), 1);
    assert_eq!(snapshot.agent_panes[0].kind.as_str(), "codex");
    assert_eq!(snapshot.agent_panes[0].agent_id, None);
    assert_eq!(snapshot.agent_panes[0].pane_id.raw(), "term1");
}

#[test]
fn agent_panes_are_uncapped() {
    // Worktree rows cap at WORKTREE_ROW_CAP idle rows, but agent_panes lists every
    // live agent pane so a command never misses one hidden behind `+K more`.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let panes: Vec<_> = (0..9)
        .map(|i| pane(&format!("term{i}"), "codex", "/repo/main"))
        .collect();
    let snapshot = snapshot.with_live_panes(panes, None);

    assert_eq!(
        snapshot.agent_panes.len(),
        9,
        "every codex pane lists, uncapped"
    );
    let rendered: usize = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.rows.len())
        .sum();
    assert!(
        rendered < 9,
        "rendered rows are capped below the pane count: {rendered}"
    );
}

#[test]
fn standalone_ask_is_not_an_agent_pane() {
    // A pending ask on a shell pane renders an agent-shaped attention row whose
    // name is its source — but it is not a live agent, so it never enters
    // agent_panes and `@codex` can never steer that shell pane.
    let shell = pane("%shell", "zsh", "/repo/main");
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "Should I proceed?",
        // A source that collides with an agent kind name — the trap the old
        // row-shape harvest fell into.
        "codex",
        // Not `agent-hook`, so it renders as a standalone attention row.
        "cli",
    );
    item.worktree_path = Some("/repo/main".to_owned());
    item.pane = Some(shell.clone());

    let snapshot = room(vec![item], Vec::new()).with_live_panes(vec![shell], None);

    let row_names: Vec<&str> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .map(|row| row.name.as_str())
        .collect();
    assert!(
        row_names.contains(&"codex"),
        "the standalone ask renders a codex-named agent row: {row_names:?}"
    );
    assert!(
        snapshot.agent_panes.is_empty(),
        "a standalone ask never enters agent_panes"
    );
}
