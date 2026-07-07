use super::*;
use crate::agents::SessionOrigin;

fn daemon_codex(id: &str, worktree: &str, owner_pid: u32) -> AgentState {
    let mut codex = paneless_codex(id, worktree, 1_000);
    codex.runtime_owner = Some(RuntimeOwner::new(
        RuntimeOwnerKind::Agent,
        id,
        owner_pid,
        None,
    ));
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
        Case {
            label: "daemon owner kind is authoritative for new records",
            daemon_pids: vec![7],
            loaded: Some(Vec::new()),
            agents: {
                let mut daemon = daemon_codex("t-gone", "/repo/b", 99);
                daemon.runtime_owner = Some(RuntimeOwner::new(
                    RuntimeOwnerKind::Daemon,
                    "t-gone",
                    99,
                    None,
                ));
                vec![daemon]
            },
            expected: Vec::new(),
        },
        Case {
            label: "subagents and other kinds are not daemon-mode codex",
            daemon_pids: vec![7],
            loaded: Some(Vec::new()),
            agents: {
                let mut sub = daemon_codex("sub-1", "/repo/a", 7);
                sub.parent_agent_id = Some("root-1".into());
                let mut claude = daemon_codex("claude-1", "/repo/c", 7);
                claude.kind = AgentKind::new_unchecked("claude");
                vec![sub, claude]
            },
            expected: vec!["claude-1", "sub-1"],
        },
    ] {
        let daemon_pids = case.daemon_pids.into_iter().collect::<BTreeSet<_>>();
        let loaded = case
            .loaded
            .map(|ids| ids.into_iter().map(str::to_owned).collect::<BTreeSet<_>>());
        let mut snapshot = room(Vec::new(), case.agents);
        snapshot.reap_runtime(crate::store::snapshot::RuntimeReapInputs {
            daemon_pids: &daemon_pids,
            loaded: loaded.as_ref(),
            frame_panes: None,
            exclude_pane: None,
        });
        assert_eq!(rollup_ids(&snapshot), case.expected, "{}", case.label);
    }
}

#[test]
fn daemon_session_reap_handles_stamped_pane_liveness() {
    struct Case {
        label: &'static str,
        agent: AgentState,
        frame_panes: Option<Vec<PaneRef>>,
        expected: Vec<&'static str>,
    }

    for case in [
        Case {
            label: "dead stamped pane is reaped when absent from live frame",
            agent: daemon_codex("t-gone", "/repo/a", 7).in_pane("%dead"),
            frame_panes: Some(vec![pane("%other", "codex", "/repo/a")]),
            expected: Vec::new(),
        },
        Case {
            label: "live stamped pane keeps absent loaded thread",
            agent: daemon_codex("t-gone", "/repo/a", 7).in_pane("%live"),
            frame_panes: Some(vec![pane("%live", "codex", "/repo/a")]),
            expected: vec!["t-gone"],
        },
        Case {
            label: "absent frame keeps stamped daemon session",
            agent: daemon_codex("t-gone", "/repo/a", 7).in_pane("%dead"),
            frame_panes: None,
            expected: vec!["t-gone"],
        },
        Case {
            label: "paneless absent loaded thread still reaps",
            agent: daemon_codex("t-gone", "/repo/a", 7),
            frame_panes: Some(vec![pane("%other", "codex", "/repo/a")]),
            expected: Vec::new(),
        },
    ] {
        let daemon_pids = BTreeSet::from([7]);
        let loaded = BTreeSet::new();
        let mut snapshot = room(Vec::new(), vec![case.agent]);
        snapshot.reap_runtime(crate::store::snapshot::RuntimeReapInputs {
            daemon_pids: &daemon_pids,
            loaded: Some(&loaded),
            frame_panes: case.frame_panes.as_deref(),
            exclude_pane: None,
        });
        assert_eq!(rollup_ids(&snapshot), case.expected, "{}", case.label);
    }
}

#[test]
fn host_pane_roots_are_dropped_only_when_frame_is_present() {
    let host_root = agent("claude", "host-root", AgentStatus::Running, 1_000)
        .worktree("/repo/daemon")
        .in_pane("%host");
    let mut host_child = agent("claude", "host-child", AgentStatus::Running, 1_001)
        .worktree("/repo/daemon")
        .in_pane("%host");
    host_child.parent_agent_id = Some("host-root".into());
    let normal = agent("claude", "normal", AgentStatus::Running, 1_002)
        .worktree("/repo/main")
        .in_pane("%work");
    let mut host_pane = pane("%host", "claude", "/repo/daemon");
    host_pane.spawn_command = Some("claude remote-control --spawn worktree".to_owned());
    let work_pane = pane("%work", "claude", "/repo/main");
    let mut snapshot = room(
        Vec::new(),
        vec![host_root.clone(), host_child.clone(), normal.clone()],
    );

    snapshot.reap_runtime(crate::store::snapshot::RuntimeReapInputs {
        daemon_pids: &BTreeSet::new(),
        loaded: None,
        frame_panes: Some(&[host_pane, work_pane]),
        exclude_pane: None,
    });

    assert_eq!(rollup_ids(&snapshot), vec!["normal"]);

    let mut snapshot = room(Vec::new(), vec![host_root, host_child, normal]);
    snapshot.reap_runtime(crate::store::snapshot::RuntimeReapInputs {
        daemon_pids: &BTreeSet::new(),
        loaded: None,
        frame_panes: None,
        exclude_pane: None,
    });

    assert_eq!(
        rollup_ids(&snapshot),
        vec!["host-child", "host-root", "normal"]
    );
}

#[test]
fn cleared_codex_session_reap_handles_lineage_and_scope_edges() {
    struct Case {
        label: &'static str,
        agents: Vec<AgentState>,
        live_panes: Vec<PaneRef>,
        expected: Vec<&'static str>,
    }

    let same_pane_pair = || {
        let mut old = paneless_codex("old", "/repo/a", 1_000)
            .branch("main")
            .in_pane("%1")
            .active_ago(120);
        old.origin = Some(SessionOrigin::Fresh);
        let mut new = paneless_codex("new", "/repo/a", 2_000)
            .branch("main")
            .in_pane("%1")
            .active_ago(5);
        new.origin = Some(SessionOrigin::Fresh);
        vec![old, new]
    };

    let fresh = |mut agent: AgentState| {
        agent.origin = Some(SessionOrigin::Fresh);
        agent
    };

    for case in [
        Case {
            label: "fresh same-pane replacement drops the prior session",
            agents: same_pane_pair(),
            live_panes: vec![pane("%1", "codex", "/repo/a")],
            expected: vec!["new"],
        },
        Case {
            label: "fork or unknown lineage keeps both same-pane sessions",
            agents: {
                let mut agents = same_pane_pair();
                agents[1].origin = Some(SessionOrigin::Forked);
                agents
            },
            live_panes: vec![pane("%1", "codex", "/repo/a")],
            expected: vec!["new", "old"],
        },
        Case {
            label: "different worktree keeps both sessions",
            agents: vec![
                fresh(
                    paneless_codex("old", "/repo/a", 1_000)
                        .in_pane("%1")
                        .active_ago(120),
                ),
                fresh(
                    paneless_codex("new", "/repo/b", 2_000)
                        .in_pane("%1")
                        .active_ago(5),
                ),
            ],
            live_panes: vec![pane("%1", "codex", "/repo/a")],
            expected: vec!["new", "old"],
        },
        Case {
            label: "distinct panes keep both sessions",
            agents: vec![
                fresh(
                    paneless_codex("old", "/repo/a", 1_000)
                        .in_pane("%1")
                        .active_ago(120),
                ),
                fresh(
                    paneless_codex("new", "/repo/a", 2_000)
                        .in_pane("%2")
                        .active_ago(5),
                ),
            ],
            live_panes: vec![
                pane("%1", "codex", "/repo/a"),
                pane("%2", "codex", "/repo/a"),
            ],
            expected: vec!["new", "old"],
        },
        Case {
            label: "non-codex live pane keeps both sessions",
            agents: vec![
                fresh(
                    paneless_codex("old", "/repo/a", 1_000)
                        .branch("main")
                        .in_pane("%1")
                        .active_ago(120),
                ),
                fresh(
                    paneless_codex("new", "/repo/a", 2_000)
                        .branch("main")
                        .in_pane("%1")
                        .active_ago(5),
                ),
            ],
            live_panes: vec![pane("%1", "zsh", "/repo/a")],
            expected: vec!["new", "old"],
        },
    ] {
        let mut snapshot = room(Vec::new(), case.agents);
        snapshot.reap_runtime(crate::store::snapshot::RuntimeReapInputs {
            daemon_pids: &BTreeSet::new(),
            loaded: None,
            frame_panes: Some(&case.live_panes),
            exclude_pane: None,
        });
        assert_eq!(rollup_ids(&snapshot), case.expected, "{}", case.label);
    }
}

// ── Ranking, caps, and bucket order ──────────────────────────────────────────
