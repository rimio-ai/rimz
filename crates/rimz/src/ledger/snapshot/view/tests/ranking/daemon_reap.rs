use super::*;

fn daemon_codex(id: &str, worktree: &str, owner_pid: u32) -> AgentState {
    let mut codex = paneless_codex(id, worktree, 1_000);
    codex.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        id,
        owner_pid,
        None,
    ));
    codex.agent_pid = Some(owner_pid);
    codex
}

fn rollup_ids(snapshot: &SidebarSnapshot) -> Vec<String> {
    let mut ids: Vec<String> = snapshot
        .agents
        .iter()
        .map(|a| a.agent_id.to_string())
        .collect();
    ids.sort();
    ids
}

#[test]
fn daemon_session_reap_handles_loaded_set_edges() {
    struct Case {
        label: &'static str,
        daemon_pids: Vec<u32>,
        loaded: Option<Vec<&'static str>>,
        agents: Vec<AgentState>,
        expected: Vec<&'static str>,
    }

    for case in [
        Case {
            label: "absent daemon session is reaped",
            daemon_pids: vec![7],
            loaded: Some(vec!["t-live"]),
            agents: vec![
                daemon_codex("t-live", "/repo/a", 7),
                daemon_codex("t-gone", "/repo/b", 7),
            ],
            expected: vec!["t-live"],
        },
        Case {
            label: "unknown loaded set keeps every session",
            daemon_pids: vec![7],
            loaded: None,
            agents: vec![daemon_codex("t-gone", "/repo/b", 7)],
            expected: vec!["t-gone"],
        },
        Case {
            label: "empty daemon pids keeps every session",
            daemon_pids: Vec::new(),
            loaded: Some(Vec::new()),
            agents: vec![daemon_codex("t-gone", "/repo/b", 7)],
            expected: vec!["t-gone"],
        },
        Case {
            label: "standalone codex is not reaped by loaded set",
            daemon_pids: vec![7],
            loaded: Some(Vec::new()),
            agents: vec![daemon_codex("t-standalone", "/repo/b", 99)],
            expected: vec!["t-standalone"],
        },
    ] {
        let daemon_pids = case.daemon_pids.into_iter().collect::<BTreeSet<_>>();
        let loaded = case
            .loaded
            .map(|ids| ids.into_iter().map(str::to_owned).collect::<BTreeSet<_>>());
        let mut snapshot = room(Vec::new(), case.agents);
        snapshot.drop_dead_daemon_sessions(&daemon_pids, loaded.as_ref());
        assert_eq!(rollup_ids(&snapshot), case.expected, "{}", case.label);
    }
}

#[test]
fn daemon_filter_spares_subagents_and_other_kinds() {
    // A codex subagent id is never a root thread, and a non-codex agent is never
    // daemon-mode — neither is reaped even sharing the daemon pid and absent from
    // the loaded set.
    let daemon_pids = BTreeSet::from([7]);
    let loaded = BTreeSet::new();
    let mut sub = daemon_codex("sub-1", "/repo/a", 7);
    sub.parent_agent_id = Some("root-1".into());
    let mut claude = daemon_codex("claude-1", "/repo/c", 7);
    claude.kind = AgentKind::new_unchecked("claude");
    let mut snapshot = room(Vec::new(), vec![sub, claude]);
    snapshot.drop_dead_daemon_sessions(&daemon_pids, Some(&loaded));
    assert_eq!(rollup_ids(&snapshot), vec!["claude-1", "sub-1"]);
}

// ── Ranking, caps, and bucket order ──────────────────────────────────────────
