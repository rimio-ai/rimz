use super::*;
use crate::agents::SessionOrigin;

/// Build a single-agent rollup at the epoch, run the reap, and return the
/// surviving agent ids. Fixture timestamps are epoch offsets, so the TTL
/// rules are exercised deterministically.
fn reap_survivors(agents: Vec<AgentState>) -> Vec<String> {
    let mut snapshot = room(agents);
    snapshot.reap_stale_sessions();
    let mut ids: Vec<String> = snapshot
        .agents
        .iter()
        .map(|a| a.agent_id.to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn root_session_reaper_drops_only_unprovable_ghosts() {
    struct Case {
        label: &'static str,
        agents: Vec<AgentState>,
        expected: Vec<&'static str>,
    }

    for case in [
        Case {
            label: "ownerless stale session drops, recent and owned survive",
            agents: {
                let stale = agent("claude", "stale", AgentStatus::Idle, 0)
                    .worktree("/repo/stale")
                    .active_ago(GHOST_SESSION_TTL_SECS + 60);
                let recent = agent("claude", "recent", AgentStatus::Idle, 0)
                    .worktree("/repo/recent")
                    .active_ago(60);
                let mut pidful = agent("codex", "pidful", AgentStatus::Idle, 0)
                    .worktree("/repo/pidful")
                    .active_ago(GHOST_SESSION_TTL_SECS * 10);
                pidful.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "pidful",
                    4242,
                    None,
                ));
                vec![stale, recent, pidful]
            },
            expected: vec!["pidful", "recent"],
        },
        Case {
            label: "daemon-owned stale session is pidless but pane owner survives",
            agents: {
                let mut stale_daemon = agent("codex", "stale-daemon", AgentStatus::Idle, 0)
                    .worktree("/repo/stale-daemon")
                    .active_ago(GHOST_SESSION_TTL_SECS + 60);
                stale_daemon.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Daemon,
                    "stale-daemon",
                    77,
                    None,
                ));
                let mut recent_daemon = agent("codex", "recent-daemon", AgentStatus::Idle, 0)
                    .worktree("/repo/recent-daemon")
                    .active_ago(60);
                recent_daemon.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Daemon,
                    "recent-daemon",
                    77,
                    None,
                ));
                let mut pane_owner = agent("codex", "pane-owner", AgentStatus::Idle, 0)
                    .worktree("/repo/pane-owner")
                    .in_pane("%9")
                    .active_ago(GHOST_SESSION_TTL_SECS * 10);
                pane_owner.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "pane-owner",
                    88,
                    None,
                ));
                vec![stale_daemon, recent_daemon, pane_owner]
            },
            expected: vec!["pane-owner", "recent-daemon"],
        },
        Case {
            label: "paneless same path and branch collapses to newest",
            agents: vec![
                agent("codex", "older", AgentStatus::Idle, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .active_ago(120),
                agent("codex", "newer", AgentStatus::Idle, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .active_ago(60),
            ],
            expected: vec!["newer"],
        },
        Case {
            label: "newer distinct pane does not prove paneless older stale",
            agents: vec![
                agent("codex", "older", AgentStatus::Idle, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .active_ago(120),
                agent("codex", "newer", AgentStatus::Idle, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .in_pane("%2")
                    .active_ago(60),
            ],
            expected: vec!["newer", "older"],
        },
        Case {
            label: "relaunch reusing a pane collapses across a branch checkout",
            agents: {
                let mut older = agent("claude", "older", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .in_pane("%1")
                    .active_ago(120);
                older.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "older",
                    111,
                    None,
                ));
                let mut newer = agent("claude", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .branch("feature")
                    .in_pane("%1")
                    .active_ago(60);
                newer.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "newer",
                    222,
                    None,
                ));
                vec![older, newer]
            },
            expected: vec!["newer"],
        },
        Case {
            label: "distinct stamped panes are concurrent live sessions",
            agents: {
                let mut older = agent("claude", "older", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .in_pane("%1")
                    .active_ago(120);
                older.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "older",
                    111,
                    None,
                ));
                let mut newer = agent("claude", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .in_pane("%2")
                    .active_ago(60);
                newer.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "newer",
                    222,
                    None,
                ));
                vec![older, newer]
            },
            expected: vec!["newer", "older"],
        },
        Case {
            label: "fresh same-pane replacement drops the prior session but keeps forks",
            agents: {
                let mut older = agent("codex", "older", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(120);
                older.origin = Some(SessionOrigin::Fresh);
                let mut fork = agent("codex", "fork", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(90);
                fork.origin = Some(SessionOrigin::Forked);
                let mut newer = agent("codex", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(60);
                newer.origin = Some(SessionOrigin::Fresh);
                vec![older, fork, newer]
            },
            expected: vec!["fork", "newer"],
        },
        Case {
            label: "antigravity same-process conversation switch drops the prior session",
            agents: {
                let mut older = agent("antigravity", "older", AgentStatus::Success, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(120);
                older.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "older",
                    9_999,
                    Some("process-a".to_owned()),
                ));
                let mut newer = agent("antigravity", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(60);
                newer.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "newer",
                    9_999,
                    Some("process-a".to_owned()),
                ));
                vec![older, newer]
            },
            expected: vec!["newer"],
        },
        Case {
            label: "antigravity process identity mismatch keeps both conversations",
            agents: {
                let mut older = agent("antigravity", "older", AgentStatus::Success, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(120);
                older.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "older",
                    9_999,
                    Some("process-a".to_owned()),
                ));
                let mut newer = agent("antigravity", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(60);
                newer.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "newer",
                    9_999,
                    Some("process-b".to_owned()),
                ));
                vec![older, newer]
            },
            expected: vec!["newer", "older"],
        },
        Case {
            label: "antigravity pane incarnation mismatch keeps both conversations",
            agents: {
                let mut older = agent("antigravity", "older", AgentStatus::Success, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(120);
                older.pane.as_mut().unwrap().pane_process_start = Some(ago(600));
                older.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "older",
                    9_999,
                    Some("process-a".to_owned()),
                ));
                let mut newer = agent("antigravity", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(60);
                newer.pane.as_mut().unwrap().pane_process_start = Some(ago(300));
                newer.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Agent,
                    "newer",
                    9_999,
                    Some("process-a".to_owned()),
                ));
                vec![older, newer]
            },
            expected: vec!["newer", "older"],
        },
        Case {
            label: "unknown older lineage keeps both same-pane sessions",
            agents: {
                let older = agent("codex", "older", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(120);
                let mut newer = agent("codex", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(60);
                newer.origin = Some(SessionOrigin::Fresh);
                vec![older, newer]
            },
            expected: vec!["newer", "older"],
        },
        Case {
            label: "unknown newer lineage keeps both same-pane sessions",
            agents: {
                let mut older = agent("codex", "older", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(120);
                older.origin = Some(SessionOrigin::Fresh);
                let newer = agent("codex", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(60);
                vec![older, newer]
            },
            expected: vec!["newer", "older"],
        },
        Case {
            label: "fresh sessions on distinct panes are concurrent",
            agents: {
                let mut older = agent("codex", "older", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%1")
                    .active_ago(120);
                older.origin = Some(SessionOrigin::Fresh);
                let mut newer = agent("codex", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .in_pane("%2")
                    .active_ago(60);
                newer.origin = Some(SessionOrigin::Fresh);
                vec![older, newer]
            },
            expected: vec!["newer", "older"],
        },
        Case {
            label: "paneless fresh root does not yield to a stamped fresh root",
            agents: {
                let mut older = agent("codex", "older", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .active_ago(120);
                older.origin = Some(SessionOrigin::Fresh);
                let mut newer = agent("codex", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .in_pane("%1")
                    .active_ago(60);
                newer.origin = Some(SessionOrigin::Fresh);
                vec![older, newer]
            },
            expected: vec!["newer", "older"],
        },
    ] {
        assert_eq!(
            reap_survivors(case.agents),
            case.expected
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            "{}",
            case.label
        );
    }
}

#[test]
fn reaper_never_drops_a_subagent() {
    let parent = agent("claude", "sess-root", AgentStatus::Running, 0);
    // A pidless idle child well past the ghost TTL, plus a same-type sibling
    // that would "supersede" it under the root rule — both survive, because
    // children are exempt and leave only when the parent does.
    let old_child = child_state(
        "sess-root",
        "child-old",
        AgentStatus::Idle,
        GHOST_SESSION_TTL_SECS + 600,
    );
    let new_child = child_state("sess-root", "child-new", AgentStatus::Running, 5);
    assert_eq!(
        reap_survivors(vec![parent, old_child, new_child]),
        vec![
            "child-new".to_owned(),
            "child-old".to_owned(),
            "sess-root".to_owned()
        ],
    );
}
