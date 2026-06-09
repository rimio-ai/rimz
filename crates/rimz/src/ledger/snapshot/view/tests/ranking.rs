use super::*;

// ── The daemon-session reap (Codex app-server loaded-thread set) ─────────────

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

#[test]
fn calm_tail_cap_never_hides_attention_rows() {
    let mut agents = (0..8)
        .map(|i| {
            agent_in(
                &format!("sess-{i}"),
                "/repo/main",
                AgentStatus::Running,
                1_000 + i,
            )
        })
        .collect::<Vec<_>>();
    agents.push(agent_in("failed", "/repo/main", AgentStatus::Failed, 2_000));

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.status() == Some(AgentStatus::Failed)),
        "attention rows remain visible past the calm-row cap"
    );
    assert!(snapshot.worktree_groups[0].hidden_count > 0);
}

#[test]
fn calm_tail_cap_never_hides_focused_rows() {
    let agents = (0..8)
        .map(|i| {
            let mut agent = agent_in(
                &format!("sess-{i}"),
                "/repo/main",
                AgentStatus::Running,
                1_000 + i,
            );
            if i == 0 {
                agent.pane = Some(PaneRef {
                    is_focused: true,
                    ..pane("%99", "codex", "/repo/main")
                });
            }
            agent
        })
        .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.id == "sess-0"),
        "the focused running pane remains visible even past the calm-row cap"
    );
    assert!(snapshot.worktree_groups[0].hidden_count > 0);
}

#[test]
fn bucket_order_puts_attention_first_and_idle_last() {
    // Scrambled input proves the sort, not the insertion order.
    let agents = [
        AgentStatus::Running,
        AgentStatus::Success,
        AgentStatus::Idle,
        AgentStatus::Paused,
        AgentStatus::Failed,
        AgentStatus::Waiting,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, status)| agent_in(&format!("sess-{i}"), "/repo/main", status, 1_000 + i as i64))
    .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.status())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            Some(AgentStatus::Waiting),
            Some(AgentStatus::Failed),
            Some(AgentStatus::Paused),
            Some(AgentStatus::Success),
            Some(AgentStatus::Running),
            Some(AgentStatus::Idle),
        ],
        "attention leads; parked idle agents sink to the bottom of the group"
    );

    let counts = snapshot.worktree_groups[0]
        .status_counts
        .iter()
        .map(|count| count.status)
        .collect::<Vec<_>>();
    assert_eq!(
        counts,
        vec![
            AgentStatus::Waiting,
            AgentStatus::Failed,
            AgentStatus::Paused,
            AgentStatus::Success,
            AgentStatus::Running,
            AgentStatus::Idle,
        ],
        "status tallies stay in cockpit make-up order"
    );
}

#[test]
fn calm_bucket_holds_stable_spawn_order() {
    // Idle agents with distinct spawn times (and one with no pane). The
    // bucket holds spawn order — oldest first — regardless of activity.
    let specs: [(&str, Option<i64>); 4] = [
        ("late", Some(100)),
        ("nopane", None),
        ("early", Some(300)),
        ("mid", Some(200)),
    ];
    let agents = specs
        .into_iter()
        .enumerate()
        .map(|(i, (id, ago_secs))| {
            let mut agent = agent_in(id, "/repo/main", AgentStatus::Idle, 1_000 + i as i64);
            agent.pane =
                ago_secs.map(|secs| pane_started(&format!("%{i}"), "/repo/main", ago(secs)));
            agent
        })
        .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    // Oldest pane first; the paneless row keys on its `registered_at` — newer
    // than every pane start here — and falls to the bucket tail.
    assert_eq!(order, vec!["early", "mid", "late", "nopane"]);
}

#[test]
fn new_idle_agent_appends_below_calm_work() {
    // A brand-new agent registers idle, so wherever the snapshot catches it —
    // before or after its first prompt — it never lands above finished or
    // working agents: idle is the calm region's bottom bucket.
    let mut done = agent_in("done", "/repo/main", AgentStatus::Success, 1_000);
    done.pane = Some(pane_started("%0", "/repo/main", ago(600)));
    let mut work = agent_in("work", "/repo/main", AgentStatus::Running, 1_001);
    work.pane = Some(pane_started("%1", "/repo/main", ago(500)));
    let mut fresh = agent_in("fresh", "/repo/main", AgentStatus::Idle, 1_002);
    fresh.pane = Some(pane_started("%2", "/repo/main", ago(5)));

    let snapshot = room_with_agent_panes(Vec::new(), vec![fresh, work, done]);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["done", "work", "fresh"],
        "the new idle card appends at the bottom of the calm region"
    );
}

#[test]
fn paneless_calm_rows_order_by_registration_not_label() {
    // Zellij reports no pane process start, so calm rows there fall back to
    // the durable `registered_at` spawn key — never a label: the older session
    // leads even though its kind name sorts after its sibling's.
    let mut older = agent("codex", "older", AgentStatus::Idle, 1_000).worktree("/repo/main");
    older.pane = Some(pane("%0", "codex", "/repo/main"));
    let mut newer = agent("claude", "newer", AgentStatus::Idle, 9_000).worktree("/repo/main");
    newer.pane = Some(pane("%1", "claude", "/repo/main"));

    let snapshot = room_with_agent_panes(Vec::new(), vec![newer, older]);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec!["older", "newer"],
        "spawn order holds without a pane start; the label never reorders calm rows"
    );
}

#[test]
fn cap_trims_idle_before_running() {
    // Idle ranks last among agents, so the per-worktree cap's calm trim eats
    // the parked idle tail first and a working agent stays visible longer.
    let mut agents = Vec::new();
    for i in 0..4 {
        agents.push(agent_in(
            &format!("run-{i}"),
            "/repo/main",
            AgentStatus::Running,
            1_000 + i,
        ));
    }
    for i in 0..4 {
        agents.push(agent_in(
            &format!("idle-{i}"),
            "/repo/main",
            AgentStatus::Idle,
            2_000 + i,
        ));
    }

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let visible = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    assert!(
        (0..4).all(|i| visible.contains(&format!("run-{i}"))),
        "every running agent stays visible; only the idle tail trims: {visible:?}"
    );
    assert_eq!(snapshot.worktree_groups[0].hidden_count, 2);
}

#[test]
fn calm_groups_hold_order_through_member_status_churn() {
    // Calm worktree groups never leapfrog just because a member's calm status
    // flipped: the group tier collapses success/running/idle to one rank, so
    // the stable earliest-pane order decides until genuine attention arises.
    let build = |a_status: AgentStatus, b_status: AgentStatus| {
        let mut a = agent_in("sess-a", "/repo/a", a_status, 1_000);
        a.pane = Some(pane_started("%0", "/repo/a", ago(600)));
        let mut b = agent_in("sess-b", "/repo/b", b_status, 1_001);
        b.pane = Some(pane_started("%1", "/repo/b", ago(500)));
        room_with_agent_panes(Vec::new(), vec![a, b])
    };

    let groups = |snapshot: &SidebarSnapshot| {
        snapshot
            .worktree_groups
            .iter()
            .map(|group| group.label.clone())
            .collect::<Vec<_>>()
    };

    let before = build(AgentStatus::Running, AgentStatus::Success);
    // b's agent finishing a turn while a's keeps working reorders nothing.
    let after = build(AgentStatus::Idle, AgentStatus::Running);
    assert_eq!(groups(&before), groups(&after));
    assert_eq!(groups(&before), vec!["a", "b"]);

    // Genuine attention still floats its group to the top.
    let blocked = build(AgentStatus::Running, AgentStatus::Waiting);
    assert_eq!(groups(&blocked), vec!["b", "a"]);
}

#[test]
fn paneless_calm_groups_order_by_registration_not_label() {
    // Same fallback at the group tier as within a bucket: without a pane
    // start (Zellij), same-tier groups key on their earliest member's
    // `registered_at` — the worktree you opened first stays first, whatever
    // its label.
    let mut older = agent_in("sess-b", "/repo/b", AgentStatus::Idle, 1_000);
    older.pane = Some(pane("%0", "node", "/repo/b"));
    let mut newer = agent_in("sess-a", "/repo/a", AgentStatus::Idle, 9_000);
    newer.pane = Some(pane("%1", "node", "/repo/a"));

    let snapshot = room_with_agent_panes(Vec::new(), vec![newer, older]);

    let groups = snapshot
        .worktree_groups
        .iter()
        .map(|group| group.label.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        groups,
        vec!["b", "a"],
        "group spawn order holds without pane starts; the label never reorders calm groups"
    );
}

#[test]
fn attention_bucket_sorts_longest_overdue_first() {
    // Scrambled input; a higher rank means more recent activity.
    let agents = vec![
        ("wait-new", AgentStatus::Waiting, 9_000),
        ("wait-old", AgentStatus::Waiting, 1_000),
        ("fail-new", AgentStatus::Failed, 8_000),
        ("fail-old", AgentStatus::Failed, 2_000),
    ]
    .into_iter()
    .map(|(id, status, rank)| agent_in(id, "/repo/main", status, rank))
    .collect::<Vec<_>>();

    let snapshot = room_with_agent_panes(Vec::new(), agents);

    let order = snapshot.worktree_groups[0]
        .rows
        .iter()
        .map(|row| row.id.clone())
        .collect::<Vec<_>>();
    // Waiting leads failed; within each, the longest-overdue (oldest activity) rises.
    assert_eq!(order, vec!["wait-old", "wait-new", "fail-old", "fail-new"]);
}

// ── Process liveness and the session reaper ──────────────────────────────────

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
fn reap_drops_pidless_session_past_ttl_but_keeps_recent_and_pidful() {
    let stale = agent("claude", "stale", AgentStatus::Idle, 0)
        .worktree("/repo/stale")
        .active_ago(GHOST_SESSION_TTL_SECS + 60);
    let recent = agent("claude", "recent", AgentStatus::Idle, 0)
        .worktree("/repo/recent")
        .active_ago(60);
    // Old but pid-bearing: TTL reaping is for pidless ghosts only.
    let mut pidful = agent("codex", "pidful", AgentStatus::Idle, 0)
        .worktree("/repo/pidful")
        .active_ago(GHOST_SESSION_TTL_SECS * 10);
    pidful.agent_pid = Some(4242);

    assert_eq!(
        reap_survivors(vec![stale, recent, pidful]),
        vec!["pidful".to_owned(), "recent".to_owned()],
        "only the pidless, past-TTL ghost is reaped"
    );
}

#[test]
fn reap_collapses_superseded_paneless_session_to_the_newest() {
    let older = agent("codex", "older", AgentStatus::Idle, 0)
        .worktree("/repo/a")
        .branch("main")
        .active_ago(120);
    let newer = agent("codex", "newer", AgentStatus::Idle, 0)
        .worktree("/repo/a")
        .branch("main")
        .active_ago(60);

    assert_eq!(
        reap_survivors(vec![older, newer]),
        vec!["newer".to_owned()],
        "the older paneless session on the same path+branch is reaped"
    );
}

#[test]
fn reap_keeps_paneless_older_when_newer_has_distinct_stamped_pane() {
    // A recovered focused-pane stamp on the newer daemon-routed Codex session
    // proves only where the newer session lives. The older paneless session may
    // still bind another same-cwd live pane at projection time, so the reaper
    // must not collapse it as an indistinguishable duplicate.
    let older = agent("codex", "older", AgentStatus::Idle, 0)
        .worktree("/repo/a")
        .branch("main")
        .active_ago(120);
    let newer = agent("codex", "newer", AgentStatus::Idle, 0)
        .worktree("/repo/a")
        .branch("main")
        .in_pane("%2")
        .active_ago(60);

    assert_eq!(
        reap_survivors(vec![older, newer]),
        vec!["newer".to_owned(), "older".to_owned()],
        "a newer distinct pane does not prove the paneless older session is stale"
    );
}

#[test]
fn reap_keeps_concurrent_agents_each_holding_a_distinct_pane() {
    // The one-pane-one-row safety property: two same-branch agents in
    // distinct panes are both live and must both survive supersession.
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

    assert_eq!(
        reap_survivors(vec![older, newer]),
        vec!["newer".to_owned(), "older".to_owned()],
        "an agent holding its own distinct pane is never reaped"
    );
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
