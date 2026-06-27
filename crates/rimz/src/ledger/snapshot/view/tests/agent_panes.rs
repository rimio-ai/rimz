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

    assert_eq!(rows(&snapshot).len(), 1);
    assert_eq!(snapshot.agent_panes.len(), 1);
    assert_eq!(snapshot.agent_panes[0].kind.as_str(), "codex");
    assert_eq!(snapshot.agent_panes[0].agent_id, None);
    assert_eq!(snapshot.agent_panes[0].pane_id.raw(), "term1");
}

#[test]
fn floating_agent_pane_stays_addressable_without_room_row() {
    let mut floating = pane("term1", "codex", "/repo/main");
    floating.is_floating = true;
    let mut snapshot = room(
        Vec::new(),
        vec![paneless_codex("sess-1", "/repo/main", 1_000)],
    );
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![floating], None);

    assert!(rows(&snapshot).is_empty());
    assert_eq!(snapshot.agent_panes.len(), 1);
    assert_eq!(snapshot.agent_panes[0].kind.as_str(), "codex");
    assert_eq!(
        snapshot.agent_panes[0]
            .agent_id
            .as_ref()
            .map(|id| id.as_str()),
        Some("sess-1")
    );
    assert_eq!(snapshot.agent_panes[0].pane_id.raw(), "term1");

    let targets = crate::target::resolve_targets(&snapshot, "@codex", None, Some("main")).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].pane_id.raw(), "term1");
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

#[test]
fn stamped_lazy_agent_holds_pane_across_non_agent_child_commands() {
    for (label, command, expect_agent) in [
        ("own foreground still binds", "codex", true),
        ("child foreground still binds", "git status", true),
        ("different agent foreground rejects", "claude", false),
    ] {
        let codex = agent("codex", "sess-1", AgentStatus::Running, 1_000)
            .worktree("/repo/main")
            .in_pane("term1");
        let snapshot = room(Vec::new(), vec![codex])
            .with_live_panes(vec![pane("term1", command, "/repo/main")], None);

        let rows = rows(&snapshot);
        assert_eq!(rows.len(), 1, "{label}");
        assert_eq!(rows[0].is_agent(), expect_agent, "{label}");
        if expect_agent {
            assert_eq!(rows[0].id, "sess-1", "{label}");
            assert_eq!(snapshot.agent_panes.len(), 1, "{label}");
            assert_eq!(
                snapshot.agent_panes[0]
                    .agent_id
                    .as_ref()
                    .map(|id| id.as_str()),
                Some("sess-1"),
                "{label}",
            );
        } else {
            assert!(rows[0].is_process(), "{label}");
            assert_eq!(rows[0].name, "claude", "{label}");
            assert!(
                snapshot.agent_panes.is_empty(),
                "rejected bind must not stay addressable: {label}",
            );
        }
        assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "term1");
    }
}

#[test]
fn plain_shell_pane_is_not_an_agent_pane() {
    // Expanded mux listings may admit more visible terminal panes, including
    // floating shells. Only panes running a known agent command enter
    // agent_panes.
    let mut snapshot = room(Vec::new(), Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "zsh", "/repo/main")], None);

    assert!(
        snapshot.agent_panes.is_empty(),
        "a plain shell never becomes an addressable @agent"
    );
}
