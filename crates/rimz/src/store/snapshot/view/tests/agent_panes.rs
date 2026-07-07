//! `SidebarSnapshot::agent_panes` — the uncapped, binding-site source that
//! command resolution (`message --steer`) reads. It lists every live agent pane (bound and
//! lazy), carries the producer-bound pane for a cwd-bound session, survives row
//! capping, and never admits a standalone ask that merely shares the agent-card
//! shape.

use super::*;

#[test]
fn cwd_bound_session_lists_its_producer_bound_pane() {
    // A daemon-routed codex carries no stamped pane; the fold binds it to the live
    // pane by cwd, and agent_panes carries that pane so message can reach it — even
    // though the rollup session itself still has no pane of its own.
    let mut snapshot = room(vec![paneless_codex("sess-1", "/repo/main", 1_000)]);
    snapshot.wired_kinds = vec!["codex".to_owned()];
    let mut live = pane("term1", "codex", "/repo/main");
    live.pane_pid = Some(12_345);
    let snapshot = snapshot.with_live_panes(vec![live], None);

    assert_eq!(snapshot.agent_panes.len(), 1);
    let bound = &snapshot.agent_panes[0];
    assert_eq!(bound.kind.as_str(), "codex");
    assert_eq!(
        bound.agent_id.as_ref().map(|id| id.as_str()),
        Some("sess-1")
    );
    assert_eq!(bound.pane_id.raw(), "term1");
    assert_eq!(bound.pane_pid, Some(12_345));
    assert!(
        snapshot.agents[0].pane.is_none(),
        "the rollup session it came from carries no pane of its own"
    );
}

#[test]
fn cwd_bound_session_binds_wrapper_pane_hosting_agent_process() {
    let mut live = pane("term1", "chezmoi cd", "/repo/main");
    live.hosted_agent_kind = Some(crate::ids::AgentKind::new_unchecked("codex"));
    live.hosted_agent_process_start = Some(ago(60));
    let mut snapshot = room(vec![paneless_codex("sess-1", "/repo/main", 1_000)]);
    snapshot.wired_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![live], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].is_agent());
    assert_eq!(rows[0].id, "sess-1");
    assert_eq!(snapshot.agent_panes.len(), 1);
    assert_eq!(snapshot.agent_panes[0].pane_id.raw(), "term1");
}

#[test]
fn lazy_pane_lists_without_a_session() {
    // An unprompted wired codex lists as an agent pane: kind and pane, no session.
    let mut snapshot = room(Vec::new());
    snapshot.wired_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    assert_eq!(rows(&snapshot).len(), 1);
    assert_eq!(snapshot.agent_panes.len(), 1);
    assert_eq!(snapshot.agent_panes[0].kind.as_str(), "codex");
    assert_eq!(snapshot.agent_panes[0].agent_id, None);
    assert_eq!(snapshot.agent_panes[0].pane_id.raw(), "term1");
}

#[test]
fn claude_pane_lists_without_a_session() {
    let mut snapshot = room(Vec::new());
    snapshot.wired_kinds = vec!["claude".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "claude", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "claude");
    assert_eq!(rows[0].status(), Some(AgentStatus::Idle));
    assert_eq!(snapshot.agent_panes.len(), 1);
    assert_eq!(snapshot.agent_panes[0].kind.as_str(), "claude");
    assert_eq!(snapshot.agent_panes[0].agent_id, None);
    assert_eq!(snapshot.agent_panes[0].pane_id.raw(), "term1");
}

#[test]
fn floating_agent_pane_stays_addressable_without_room_row() {
    let mut floating = pane("term1", "codex", "/repo/main");
    floating.is_floating = true;
    let mut snapshot = room(vec![paneless_codex("sess-1", "/repo/main", 1_000)]);
    snapshot.wired_kinds = vec!["codex".to_owned()];
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

    let targets =
        crate::harness::target::resolve_targets(&snapshot, "@codex", None, Some("main")).unwrap();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].pane_id.raw(), "term1");
}

#[test]
fn agent_panes_are_uncapped() {
    // The sidebar snapshot and agent_panes both keep every live agent pane so a
    // command never misses one hidden by the renderer's `+K more` cap.
    let mut snapshot = room(Vec::new());
    snapshot.wired_kinds = vec!["codex".to_owned()];
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
    assert_eq!(rendered, 9, "snapshot rows stay uncapped: {rendered}");
}

#[test]
fn stamped_lazy_agent_holds_pane_across_non_agent_child_commands() {
    for (label, command, hosted_kind, expect_agent) in [
        ("own foreground still binds", "codex", Some("codex"), true),
        (
            "child foreground still binds",
            "git status",
            Some("codex"),
            true,
        ),
        ("quit shell demotes", "zsh", None, false),
        ("different agent foreground rejects", "claude", None, false),
    ] {
        let codex = agent("codex", "sess-1", AgentStatus::Running, 1_000)
            .worktree("/repo/main")
            .in_pane("term1");
        let mut live = pane("term1", command, "/repo/main");
        if let Some(kind) = hosted_kind {
            live.hosted_agent_kind = Some(crate::ids::AgentKind::new_unchecked(kind));
            live.hosted_agent_process_start = Some(ago(600));
        }
        let snapshot = room(vec![codex]).with_live_panes(vec![live], None);

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
            assert_eq!(
                rows[0].name,
                command.split_whitespace().next().unwrap(),
                "{label}"
            );
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
    let mut snapshot = room(Vec::new());
    snapshot.wired_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![pane("term1", "zsh", "/repo/main")], None);

    assert!(
        snapshot.agent_panes.is_empty(),
        "a plain shell never becomes an addressable @agent"
    );
}
