use super::*;

#[test]
fn liveness_drops_dead_agent_pid_from_rollup() {
    let mut codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).branch("main");
    codex.agent_pid = Some(424_242);
    codex.agent_process_start = Some("12345".to_owned());

    let mut snapshot = room(Vec::new(), vec![codex]);
    assert_eq!(snapshot.agents.len(), 1);
    assert!(snapshot.worktree_groups.is_empty());

    snapshot.drop_dead_agents_with(|pid, start| {
        assert_eq!(pid, 424_242);
        assert_eq!(start, Some("12345"));
        false
    });

    assert!(snapshot.agents.is_empty());
    assert!(snapshot.worktree_groups.is_empty());
}

/// Build a single-agent rollup at the epoch, run the reap, and return the
/// surviving agent ids. Fixture timestamps are epoch offsets, so the TTL
/// rules are exercised deterministically.
fn reap_survivors(agents: Vec<AgentState>) -> Vec<String> {
    let mut snapshot = room(Vec::new(), agents);
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
            label: "pidless stale session drops, recent and pid-bearing survive",
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
                pidful.agent_pid = Some(4242);
                vec![stale, recent, pidful]
            },
            expected: vec!["pidful", "recent"],
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
            label: "distinct stamped panes are concurrent live sessions",
            agents: {
                let mut older = agent("claude", "older", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .in_pane("%1")
                    .active_ago(120);
                older.agent_pid = Some(111);
                let mut newer = agent("claude", "newer", AgentStatus::Running, 0)
                    .worktree("/repo/a")
                    .branch("main")
                    .in_pane("%2")
                    .active_ago(60);
                newer.agent_pid = Some(222);
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
