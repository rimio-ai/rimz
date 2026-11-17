use super::*;

use crate::agents::AgentStatus;
use crate::ids::{AgentKind, MuxName, PaneId};
use crate::store::snapshot::SidebarSnapshot;
use crate::store::snapshot::testkit::{AgentStateFx, agent, ago, room};

/// A pane fixture with an explicit command and optional window name, so a
/// test can build daemon hosts, sidebars, and working shells across views.
fn pane_cmd(raw: &str, view: &str, command: &str, view_name: Option<&str>) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        session_name: "rimz-test".to_owned(),
        view_id: Some(view.to_owned()),
        view_kind: Some(crate::ids::ViewKind::Tab),
        view_name: view_name.map(str::to_owned),
        title: None,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some("/repo/main".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        hosted_agent_lineage: Vec::new(),
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

#[test]
fn only_daemon_view_classifies_view_sets() {
    for (label, panes, expected) in [
        (
            "daemon view only",
            vec![
                pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                pane_cmd(
                    "terminal_1",
                    "tab_0",
                    "claude remote-control --spawn worktree",
                    None,
                ),
                pane_cmd("terminal_2", "tab_0", "rimz codex app-server serve", None),
            ],
            true,
        ),
        (
            "working view exists",
            vec![
                pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                pane_cmd(
                    "terminal_1",
                    "tab_0",
                    "claude remote-control --spawn worktree",
                    None,
                ),
                pane_cmd("terminal_3", "tab_1", "rimz-sidebar", None),
                pane_cmd("terminal_4", "tab_1", "zsh", None),
            ],
            false,
        ),
        (
            "no daemon view",
            vec![
                pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                pane_cmd("terminal_1", "tab_0", "zsh", None),
            ],
            false,
        ),
        ("empty session", Vec::new(), false),
        (
            "sidebar-only limbo view ignored",
            vec![
                pane_cmd("terminal_0", "tab_0", "rimz-sidebar", None),
                pane_cmd("terminal_1", "tab_0", "rimz codex app-server serve", None),
                pane_cmd("terminal_3", "tab_1", "rimz-sidebar", None),
            ],
            true,
        ),
    ] {
        assert_eq!(
            SidebarSnapshot::only_daemon_view(&panes),
            expected,
            "{label}"
        );
    }
}

#[test]
fn card_admission_names_card_blockers() {
    let root_pane = pane_cmd("terminal_5", "tab_1", "claude", Some("rimzd"));
    let child_pane = pane_cmd("terminal_6", "tab_1", "claude", Some("rimzd"));
    let mut root = agent("claude", "root", AgentStatus::Running, 1_000);
    root.pane = Some(root_pane.clone());
    let mut child = agent("claude", "child", AgentStatus::Running, 1_001);
    child.pane = Some(child_pane.clone());
    child.parent_agent_id = Some(root.agent_id.clone());
    child.launch_depth = Some(1);
    let agents = [root, child];
    let bindings = PaneBindingIndex::new(&agents);
    let working = pane_cmd("terminal_1", "tab_0", "zsh", None);
    let unreadable = PaneRef {
        command: None,
        ..pane_cmd("terminal_2", "tab_0", "zsh", None)
    };
    for (label, pane, exclude, expected) in [
        ("working pane", working.clone(), None, true),
        (
            "excluded pane id",
            working.clone(),
            Some(working.pane_id.clone()),
            false,
        ),
        (
            "sidebar chrome",
            pane_cmd("terminal_3", "tab_0", "rimz-sidebar", None),
            None,
            false,
        ),
        (
            "remote-control host",
            pane_cmd("terminal_4", "tab_0", "rimz codex app-server serve", None),
            None,
            false,
        ),
        ("identityless pane", unreadable, None, true),
        (
            "daemon view without stamp",
            pane_cmd("terminal_7", "tab_1", "claude", Some("rimzd")),
            None,
            false,
        ),
        ("daemon view stamped root", root_pane.clone(), None, true),
        ("daemon view stamped child", child_pane, None, true),
        (
            "stamped host remains excluded",
            PaneRef {
                spawn_command: Some("claude remote-control".to_owned()),
                ..root_pane
            },
            None,
            false,
        ),
    ] {
        assert_eq!(
            pane_admits_card(&pane, exclude.as_ref(), &bindings),
            expected,
            "{label}"
        );
    }
}

#[test]
fn agent_stamp_tolerates_floor_to_exact_start_drift() {
    let floor: Timestamp = "2026-06-05T13:49:53Z".parse().unwrap();
    let exact: Timestamp = "2026-06-05T14:22:43Z".parse().unwrap();
    let stamped = PaneRef {
        pane_process_start: Some(floor),
        ..pane_cmd("terminal_1", "tab_0", "codex", None)
    };
    let live = PaneRef {
        pane_process_start: Some(exact),
        ..pane_cmd("terminal_1", "tab_0", "codex", None)
    };

    assert!(
        pane_start_matches_agent_stamp(&stamped, &live),
        "old floor-era agent stamps still attach to the now-exact live process"
    );
}

#[test]
fn stamped_lazy_agent_binds_with_carried_hosted_stamp_and_pidless_pane() {
    let agent = agent("codex", "sess-1", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let live = PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
        command: Some("git".to_owned()),
        pane_pid: None,
        hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
        hosted_agent_process_start: Some(ago(120)),
        ..pane_cmd("%1", "tab_0", "git", None)
    };

    assert!(stamped_agent_matches_live_pane(
        &agent,
        agent.pane.as_ref().expect("stamped pane"),
        &live,
    ));
}

#[test]
fn resumed_stamp_from_live_pane_root_binds_before_new_activity() {
    let mut agent = agent("codex", "sess-resumed", AgentStatus::Idle, 1)
        .worktree("/repo/main")
        .active_ago(120)
        .in_pane("%4");
    agent.pane.as_mut().expect("stamped pane").pane_pid = Some(84);
    agent.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "sess-resumed",
        84,
        None,
    ));
    let live = PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
        command: Some("codex".to_owned()),
        pane_pid: Some(84),
        pane_process_start: Some(ago(60)),
        hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
        hosted_agent_process_start: Some(ago(60)),
        ..pane_cmd("%4", "tab_0", "codex", None)
    };

    assert!(stamped_agent_matches_live_pane(
        &agent,
        agent.pane.as_ref().expect("stamped pane"),
        &live,
    ));
}

#[test]
fn shell_root_stamp_keeps_clock_guard_when_agent_owner_is_child() {
    let mut agent = agent("codex", "sess-retired", AgentStatus::Idle, 1)
        .worktree("/repo/main")
        .active_ago(120)
        .in_pane("%4");
    agent.pane.as_mut().expect("stamped pane").pane_pid = Some(84);
    agent.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "sess-retired",
        85,
        None,
    ));
    let live = PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
        command: Some("zsh".to_owned()),
        pane_pid: Some(84),
        pane_process_start: Some(ago(60)),
        hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
        hosted_agent_process_start: Some(ago(60)),
        ..pane_cmd("%4", "tab_0", "zsh", None)
    };

    assert!(!stamped_agent_matches_live_pane(
        &agent,
        agent.pane.as_ref().expect("stamped pane"),
        &live,
    ));
}

#[test]
fn shell_launched_lazy_stamp_binds_by_its_recorded_chain_before_its_first_turn() {
    for (label, ancestors, binds) in [
        ("the chain holds the pane root", vec![84, 12], true),
        ("the chain misses the pane root", vec![91, 12], false),
        ("no chain recorded", Vec::new(), false),
    ] {
        let mut launched = agent("codex", "launch_relaunched", AgentStatus::Idle, 1)
            .worktree("/repo/main")
            .active_ago(10)
            .in_pane("%4");
        launched.pane.as_mut().expect("stamped pane").pane_pid = Some(85);
        launched.runtime_owner = Some(owner("launch_relaunched", 85, &ancestors));
        let live = PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
            pane_pid: Some(84),
            pane_process_start: Some(ago(5)),
            hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
            hosted_agent_process_start: Some(ago(5)),
            ..pane_cmd("%4", "tab_0", "zsh", None)
        };
        let agents = [launched];

        let bound = PaneBindingIndex::new(&agents).stamped_agent(&live);
        assert_eq!(
            bound.map(|agent| agent.agent_id.as_str()),
            binds.then_some("launch_relaunched"),
            "{label}"
        );
    }
}

#[test]
fn carried_stamp_without_live_root_identity_keeps_clock_guard() {
    for pane_pid in [None, Some(83)] {
        let mut agent = agent("codex", "sess-stale", AgentStatus::Idle, 1)
            .worktree("/repo/main")
            .active_ago(120)
            .in_pane("%4");
        agent.pane.as_mut().expect("stamped pane").pane_pid = pane_pid;
        let live = PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
            command: Some("codex".to_owned()),
            pane_pid: Some(84),
            pane_process_start: Some(ago(60)),
            hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
            hosted_agent_process_start: Some(ago(60)),
            ..pane_cmd("%4", "tab_0", "codex", None)
        };

        assert!(!stamped_agent_matches_live_pane(
            &agent,
            agent.pane.as_ref().expect("stamped pane"),
            &live,
        ));
    }
}

#[test]
fn live_root_stamp_still_rejects_a_different_hosted_kind() {
    let mut agent = agent("codex", "sess-resumed", AgentStatus::Idle, 1)
        .worktree("/repo/main")
        .active_ago(120)
        .in_pane("%4");
    agent.pane.as_mut().expect("stamped pane").pane_pid = Some(84);
    agent.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "sess-resumed",
        84,
        None,
    ));
    let live = PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, "%4"),
        command: Some("node".to_owned()),
        pane_pid: Some(84),
        pane_process_start: Some(ago(60)),
        hosted_agent_kind: Some(AgentKind::new_unchecked("claude")),
        hosted_agent_process_start: Some(ago(60)),
        ..pane_cmd("%4", "tab_0", "node", None)
    };

    assert!(!stamped_agent_matches_live_pane(
        &agent,
        agent.pane.as_ref().expect("stamped pane"),
        &live,
    ));
}

fn owner(id: &str, pid: u32, ancestors: &[u32]) -> crate::pane::RuntimeOwner {
    crate::pane::RuntimeOwner {
        ancestors: ancestors.to_vec(),
        ..crate::pane::RuntimeOwner::new(RuntimeOwnerKind::Agent, id, pid, None)
    }
}

/// A live tmux pane rooted at `root`, started 60 s before the epoch, running
/// `command` and hosting no classified process.
fn rooted(raw: &str, root: Option<u32>, command: &str) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, raw),
        pane_pid: root,
        pane_process_start: Some(ago(60)),
        ..pane_cmd(raw, "tab_0", command, None)
    }
}

fn binds(agent: &AgentState, live: &PaneRef) -> bool {
    stamped_agent_matches_live_pane(agent, agent.pane.as_ref().expect("stamped pane"), live)
}

#[test]
fn a_proven_owner_binds_without_clock_program_or_start_evidence() {
    for kind in ["claude", "codex"] {
        let other = if kind == "claude" { "codex" } else { "claude" };
        let mut stale = agent(kind, "sess", AgentStatus::Idle, 1)
            .worktree("/repo/main")
            .active_ago(120)
            .in_pane("%4");
        stale
            .pane
            .as_mut()
            .expect("stamped pane")
            .pane_process_start = Some(ago(10));
        let live = PaneRef {
            hosted_agent_process_start: Some(ago(30)),
            ..rooted("%4", Some(84), "zsh")
        };
        for (label, recorded) in [
            ("the chain holds the root", owner("sess", 90, &[85, 84, 12])),
            ("the owner is the root", owner("sess", 84, &[])),
        ] {
            stale.runtime_owner = Some(recorded);
            assert!(binds(&stale, &live), "{kind}: {label}");
            let tenanted = PaneRef {
                hosted_agent_kind: Some(AgentKind::new_unchecked(other)),
                ..live.clone()
            };
            assert!(
                !binds(&stale, &tenanted),
                "{kind}: {label}, but the pane hosts {other}"
            );
        }
    }
}

#[test]
fn a_disproven_owner_never_binds_though_kind_and_clocks_agree() {
    for kind in ["claude", "codex"] {
        let mut elsewhere = agent(kind, "sess-elsewhere", AgentStatus::Idle, 1)
            .worktree("/repo/main")
            .active_ago(5)
            .in_pane("%4");
        elsewhere.runtime_owner = Some(owner("sess-elsewhere", 90, &[85, 80]));
        let live = rooted("%4", Some(84), kind);
        assert!(!binds(&elsewhere, &live), "{kind}");

        let sibling = rooted("%5", Some(95), kind);
        let snapshot = room(vec![elsewhere]).with_live_panes(vec![live, sibling], None);
        assert!(
            snapshot.rows().all(|row| row.id != "sess-elsewhere"),
            "{kind}: neither pane shows the disproven card"
        );
        assert!(
            snapshot.agent_panes.iter().all(|bound| bound
                .agent_id
                .as_ref()
                .is_none_or(|id| id.as_str() != "sess-elsewhere")),
            "{kind}: the row is paired with no pane by cwd"
        );
        let shown = snapshot
            .rows()
            .map(|row| (row.is_agent(), row.is_process()))
            .collect::<Vec<_>>();
        assert_eq!(shown.len(), 2, "{kind}: {shown:?}");
        assert!(
            shown.iter().all(|(agent, process)| *agent || *process),
            "{kind}: each pane shows an unnamed agent row or a process row: {shown:?}"
        );
    }
}

#[test]
fn an_unknown_owner_binds_an_eager_kind_only_with_its_program_and_fresh_clocks() {
    let daemon = crate::pane::RuntimeOwner::new(RuntimeOwnerKind::Daemon, "sess", 84, None);
    for (label, recorded, root) in [
        ("no owner", None, Some(84)),
        ("a daemon owner", Some(daemon), Some(84)),
        (
            "another subject",
            Some(owner("sess-other", 84, &[])),
            Some(84),
        ),
        ("an empty chain", Some(owner("sess", 90, &[])), Some(84)),
        (
            "a pane without a root pid",
            Some(owner("sess", 90, &[84])),
            None,
        ),
    ] {
        let fresh = || {
            let mut row = agent("claude", "sess", AgentStatus::Idle, 1)
                .worktree("/repo/main")
                .active_ago(5)
                .in_pane("%4");
            row.runtime_owner.clone_from(&recorded);
            row
        };
        assert!(binds(&fresh(), &rooted("%4", root, "claude")), "{label}");
        assert!(
            !binds(&fresh(), &rooted("%4", root, "zsh")),
            "{label}: no program"
        );
        assert!(
            !binds(&fresh(), &rooted("%4", root, "codex")),
            "{label}: another program"
        );
        let older = fresh().active_ago(120);
        assert!(
            !binds(&older, &rooted("%4", root, "claude")),
            "{label}: activity older than the pane start"
        );
        let behind_hosted = PaneRef {
            pane_process_start: Some(ago(200)),
            hosted_agent_kind: Some(AgentKind::new_unchecked("claude")),
            hosted_agent_process_start: Some(ago(60)),
            ..rooted("%4", root, "claude")
        };
        assert!(
            !binds(&older, &behind_hosted),
            "{label}: activity older than the hosted process start"
        );
    }
}

#[test]
fn every_built_in_kind_is_proven_and_disproven_by_its_owner() {
    for definition in crate::agents::all_definitions() {
        let kind = definition.spec().kind;
        let mut row = agent(kind, "sess", AgentStatus::Idle, 1)
            .worktree("/repo/main")
            .active_ago(120)
            .in_pane("%4");
        row.runtime_owner = Some(owner("sess", 90, &[84, 12]));
        assert!(binds(&row, &rooted("%4", Some(84), "zsh")), "{kind} proven");
        row.runtime_owner = Some(owner("sess", 90, &[85, 12]));
        assert!(
            !binds(&row, &rooted("%4", Some(84), "zsh")),
            "{kind} disproven"
        );
    }
}

#[test]
fn an_ended_row_whose_owner_is_the_root_does_not_bind_a_shell() {
    let mut ended = agent("claude", "sess-ended", AgentStatus::Idle, 1)
        .worktree("/repo/main")
        .active_ago(5)
        .in_pane("%4");
    ended.runtime_owner = Some(owner("sess-ended", 84, &[12]));
    let live = rooted("%4", Some(84), "zsh");
    assert!(binds(&ended, &live), "the unended row is proven");
    ended.ended_at = Some(ago(1));
    assert!(!binds(&ended, &live));
}

#[test]
fn a_dead_owners_child_kept_for_display_is_judged_unknown() {
    let mut parent = agent("claude", "root", AgentStatus::Idle, 1)
        .worktree("/repo/main")
        .active_ago(5)
        .in_pane("%1");
    parent.launch_id = Some("launch_root".into());
    let mut child = agent("claude", "child", AgentStatus::Idle, 1)
        .worktree("/repo/main")
        .active_ago(5)
        .in_pane("%4");
    child.parent_agent_id = parent.launch_id.clone();
    child.launch_depth = Some(1);
    child.runtime_owner = Some(owner("child", u32::MAX, &[84, 12]));
    let live = rooted("%4", Some(84), "zsh");

    let raw = [parent, child];
    assert!(
        PaneBindingIndex::new(&raw)
            .stamped_launched_child(&live)
            .is_some(),
        "the raw row's chain proves the pane"
    );
    let projected =
        crate::store::runtime::RuntimeProjection::runtime_from_refs(raw.iter(), Vec::new()).agents;
    let kept = projected
        .iter()
        .find(|row| row.agent_id == "child")
        .expect("the dead owner's child is kept for display");
    assert!(
        kept.runtime_owner
            .as_ref()
            .is_some_and(|owner| owner.ancestors.is_empty())
    );
    assert!(
        PaneBindingIndex::new(&projected)
            .stamped_launched_child(&live)
            .is_none(),
        "a kept dead owner carries no chain, so the shell pane does not bind"
    );
}

/// The tenant rule of `crates/rimz/src/sidebar/AGENTS.md`: each row a pane can
/// show between its shell row and its bound card, read for an Unknown eager
/// row (no owner) and a Proven one (the owner's chain holds the root).
#[test]
fn tenant_rows_between_shell_and_card_follow_the_verdict() {
    let zellij = PaneRef {
        pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_4"),
        ..rooted("%4", Some(84), "claude")
    };
    let plumbing = PaneRef {
        spawn_command: Some("rimz agents exec claude --worktree-path /repo/main".to_owned()),
        ..rooted("%4", Some(84), "rimz")
    };
    for (label, live, unknown_binds) in [
        (
            "the pulled process row",
            rooted("%4", Some(84), "zsh"),
            false,
        ),
        ("launch plumbing", plumbing, true),
        ("Zellij's unbound agent row", zellij, true),
    ] {
        for (verdict, recorded, expect_card) in [
            ("unknown", None, unknown_binds),
            ("proven", Some(owner("sess", 90, &[84, 12])), true),
            ("disproven", Some(owner("sess", 90, &[85, 12])), false),
        ] {
            let mut row = agent("claude", "sess", AgentStatus::Idle, 1)
                .worktree("/repo/main")
                .active_ago(5);
            row.pane = Some(PaneRef::from_id(live.pane_id.clone()));
            row.runtime_owner = recorded;
            let snapshot = room(vec![row]).with_live_panes(vec![live.clone()], None);
            let shown = snapshot
                .rows()
                .find(|shown| {
                    shown
                        .pane
                        .as_ref()
                        .is_some_and(|p| p.pane_id == live.pane_id)
                })
                .expect("the pane shows a row");
            assert_eq!(shown.id == "sess", expect_card, "{label}, {verdict}");
            if !expect_card {
                assert!(
                    shown.is_agent() || shown.is_process(),
                    "{label}, {verdict}: an unnamed agent row or a process row"
                );
            }
        }
    }

    // A `CommandChanged` overlay rebuilds the row from the event's command
    // alone, whatever the verdict; the next pull judges the card again.
    for (verdict, recorded, command) in [
        ("unknown", None, "claude"),
        ("proven", Some(owner("sess", 90, &[84, 12])), "zsh"),
    ] {
        let live = rooted("%4", Some(84), command);
        let mut row = agent("claude", "sess", AgentStatus::Idle, 1)
            .worktree("/repo/main")
            .active_ago(5)
            .in_pane("%4");
        row.runtime_owner = recorded;
        let mut snapshot = room(vec![row]).with_live_panes(vec![live.clone()], None);
        assert!(snapshot.rows().any(|shown| shown.id == "sess"), "{verdict}");
        snapshot.overlay_pane_command(&live.pane_id, "vim");
        let shown = snapshot
            .rows()
            .find(|shown| {
                shown
                    .pane
                    .as_ref()
                    .is_some_and(|p| p.pane_id == live.pane_id)
            })
            .expect("the pane keeps a row");
        assert!(
            shown.id != "sess" && shown.is_process(),
            "{verdict}: the overlay shows the new command's process row"
        );
    }
}
