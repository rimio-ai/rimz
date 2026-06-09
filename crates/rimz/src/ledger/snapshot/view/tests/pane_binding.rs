use super::*;

// ── Remote-control host filtering ────────────────────────────────────────────

#[test]
fn remote_control_host_pane_renders_no_row() {
    // A `claude remote-control` pane (Zellij reports the full command line)
    // is ambient infrastructure: it no longer renders as any row — its
    // presence surfaces as the provider dashboard's `⇅ rc` flag instead.
    // Only the shell pane beside it remains a row.
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(
        vec![
            pane("%1", "zsh", "/repo/main"),
            pane("%2", "claude remote-control --spawn worktree", "/repo/main"),
        ],
        None,
    );

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "only the shell pane is a row: {rows:?}");
    assert!(rows[0].is_process());
    assert_eq!(rows[0].name, "zsh");
    assert!(
        rows.iter().all(|row| row.name != "claude"),
        "the host pane must not produce a claude row: {rows:?}",
    );
}

#[test]
fn remote_control_host_pane_filtered_when_detected_by_view_name() {
    // tmux reports only the `claude` basename, but names the window — so the
    // view name marks the host, and that pane is filtered out the same way.
    let mut rc_pane = pane("%2", "claude", "/repo/main");
    rc_pane.view_name = Some(crate::remote_control::VIEW_NAME.to_owned());
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(vec![rc_pane], None);

    let rows = rows(&snapshot);
    assert!(
        rows.is_empty(),
        "a host-only pane set produces no rows: {rows:?}",
    );
}

// ── Pane binding: stamped ids, live overlays, one pane = one row ─────────────

#[test]
fn live_panes_add_process_rows_without_attention_counts() {
    let snapshot =
        room(Vec::new(), Vec::new()).with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_process());
    assert_eq!(row.name, "zsh");
    assert_eq!(row.status(), None);
    assert!(snapshot.worktree_groups[0].status_counts.is_empty());
}

fn script_ask_for_pane(pane: Option<PaneRef>) -> FeedItem {
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "approve deploy?",
        "deploy",
        "script",
    );
    item.pane = pane;
    item
}

#[test]
fn standalone_script_ask_renders_only_from_matching_frame_pane() {
    let stale_pane = PaneRef {
        view_id: Some("@stale".to_owned()),
        command: Some("old-deploy".to_owned()),
        cwd: Some("/old".to_owned()),
        ..pane("%7", "old-deploy", "/old")
    };
    let mut frame_pane = pane("%7", "deploy", "/repo/main");
    frame_pane.view_id = Some("@fresh".to_owned());
    frame_pane.is_focused = true;
    let item = script_ask_for_pane(Some(stale_pane));
    let request_id = item.request_id.clone();

    let snapshot = room(vec![item], Vec::new()).with_live_panes(vec![frame_pane.clone()], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "the ask owns the pane row slot");
    let row = rows[0];
    assert_eq!(row.request_id(), Some(&request_id));
    assert_eq!(row.task(), Some("approve deploy?"));
    assert_eq!(row.worktree_path.as_deref(), Some("/repo/main"));
    assert_eq!(row.pane.as_ref(), Some(&frame_pane));
}

#[test]
fn standalone_script_ask_without_matching_frame_pane_does_not_render() {
    for case in [
        (
            "without pane",
            script_ask_for_pane(None),
            vec![pane("%1", "zsh", "/repo/main")],
        ),
        (
            "absent pane",
            script_ask_for_pane(Some(pane("%7", "deploy", "/repo/main"))),
            vec![pane("%8", "zsh", "/repo/main")],
        ),
        (
            "reused pane id",
            script_ask_for_pane(Some(pane_started("%7", "/repo/main", ago(60)))),
            vec![pane_started("%7", "/repo/main", ago(5))],
        ),
    ] {
        let (label, item, panes) = case;
        let request_id = item.request_id.clone();

        let snapshot = room(vec![item], Vec::new()).with_live_panes(panes, None);

        let rows = rows(&snapshot);
        assert_eq!(rows.len(), 1, "{label}");
        assert!(
            rows.iter().all(|row| row.request_id() != Some(&request_id)),
            "{label}: unmatched asks remain metadata only"
        );
    }
}

#[test]
fn standalone_ask_on_an_agents_pane_folds_onto_the_agent_row() {
    // A script ask raised from inside an agent's pane (the agent shelling out
    // to `rimz feed ask`) folds onto the agent's row — identity and capability
    // line kept, the ask's waiting status and request taken — and outranks the
    // session's own pending agent-hook ask: the script blocks the pane's
    // foreground right now.
    let mut claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    claude.model = Some("opus-4".to_owned());
    let script_ask = script_ask_for_pane(Some(pane("%1", "claude", "/repo/main")));
    let request_id = script_ask.request_id.clone();
    let items = vec![
        agent_ask(FeedKind::Permission, "claude", "sess-a"),
        script_ask,
    ];

    let snapshot =
        room(items, vec![claude]).with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "one pane, one row: {rows:?}");
    let row = rows[0];
    assert!(row.is_agent());
    assert_eq!(row.id, "sess-a", "the agent keeps the row identity");
    assert_eq!(row.name, "claude");
    assert_eq!(
        row.model(),
        Some("opus-4"),
        "the capability line survives the fold"
    );
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(
        row.request_id(),
        Some(&request_id),
        "the pane-blocking script ask outranks the agent-hook ask"
    );
    assert_eq!(row.surface(), Some(Surface::Script));
}

#[test]
fn standalone_bridge_ask_renders_its_resolver_from_the_frame() {
    let mut item = FeedItem::new(
        workspace(),
        Surface::Bridge,
        FeedKind::Permission,
        "approve deploy?",
        "deploy",
        "script",
    );
    item.pane = Some(pane("%7", "deploy", "/repo/main"));
    item.chain_active_resolver = Some(crate::ids::ResolverId::new_unchecked("auto-approver"));
    let request_id = item.request_id.clone();

    let snapshot = room(vec![item], Vec::new())
        .with_live_panes(vec![pane("%7", "deploy", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "the bridge ask owns the pane row slot");
    let row = rows[0];
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.request_id(), Some(&request_id));
    assert_eq!(
        row.resolver()
            .as_ref()
            .map(|resolver| resolver.resolver_id.as_str()),
        Some("auto-approver"),
        "a frame-admitted bridge ask carries its active resolver"
    );
}

#[test]
fn standalone_ask_on_a_wired_idle_lazy_pane_folds_onto_the_idle_row() {
    let item = script_ask_for_pane(Some(pane("term1", "codex", "/repo/main")));
    let request_id = item.request_id.clone();
    let mut snapshot = room(vec![item], Vec::new());
    snapshot.wired_lazy_kinds = vec!["codex".to_owned()];

    let snapshot = snapshot.with_live_panes(vec![pane("term1", "codex", "/repo/main")], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "one pane, one row: {rows:?}");
    let row = rows[0];
    assert!(row.is_agent());
    assert_eq!(
        row.name, "codex",
        "the idle lazy identity survives the fold"
    );
    assert_eq!(row.id, "tmux:term1");
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.request_id(), Some(&request_id));
}

#[test]
fn commandless_unbound_pane_folds_no_row() {
    // A pane whose command is still unknown after frame rotation — mid-birth,
    // or a raced first read — is presence without identity: it folds no row
    // rather than an anonymous `process` under `external`.
    let raced = PaneRef {
        command: None,
        cwd: None,
        ..pane("%1", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(vec![raced], None);

    let rows = rows(&snapshot);
    assert!(
        rows.is_empty(),
        "a command-less pane renders no row: {rows:?}"
    );
}

#[test]
fn spawn_only_unbound_pane_still_renders() {
    // Regression for Zellij topology/CLI source races: a frame with no
    // foreground command but a stable spawn command remains a known pane.
    let raced = PaneRef {
        command: None,
        spawn_command: Some("rimz agents exec codex --worktree-path /repo/main".to_owned()),
        cwd: None,
        ..pane("%1", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new()).with_live_panes(vec![raced], None);

    let rows = rows(&snapshot);
    assert_eq!(
        rows.len(),
        1,
        "spawn identity keeps the row visible: {rows:?}"
    );
    assert_eq!(rows[0].name, "codex");
}

#[test]
fn commandless_pane_with_agent_still_renders_agent_row() {
    // Agent rows bind by stamped pane id, never by command, so a raced read
    // that drops the command never demotes or hides the agent's row.
    let claude = agent("claude", "sess-a", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let raced = PaneRef {
        command: None,
        ..pane("%1", "claude", "/repo/main")
    };
    let snapshot = room(Vec::new(), vec![claude]).with_live_panes(vec![raced], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "the stamped agent row survives: {rows:?}");
    assert!(rows[0].is_agent());
}

#[test]
fn commandless_pane_does_not_form_empty_external_group() {
    // The raced read that drops a command usually drops the cwd too; the
    // filtered pane must not mint a stray `external` header on its way out.
    let root = "/repo/rimz";
    let raced = PaneRef {
        command: None,
        cwd: None,
        ..pane("%2", "x", "")
    };
    let snapshot = room(Vec::new(), Vec::new())
        .with_project_root(Some(PathBuf::from(root)))
        .with_live_panes(vec![pane("%1", "zsh", root), raced], None);

    assert_eq!(
        snapshot.worktree_groups.len(),
        1,
        "no external group for the filtered pane: {:?}",
        snapshot.worktree_groups,
    );
    assert_eq!(snapshot.worktree_groups[0].label, "rimz");
}

#[test]
fn commandless_pane_keeps_known_process_rows() {
    // The guard is per-pane: a sibling whose command read succeeded keeps
    // its named process row.
    let raced = PaneRef {
        command: None,
        ..pane("%2", "x", "/repo/main")
    };
    let snapshot = room(Vec::new(), Vec::new())
        .with_live_panes(vec![pane("%1", "zsh", "/repo/main"), raced], None);

    let rows = rows(&snapshot);
    assert_eq!(rows.len(), 1, "only the named pane is a row: {rows:?}");
    assert_eq!(rows[0].name, "zsh");
}

#[test]
fn live_panes_overlay_matching_agent_rows() {
    let codex = agent("codex", "sess-1", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .branch("main")
        .in_pane("%1");
    let snapshot = room(Vec::new(), vec![codex])
        .with_live_panes(vec![pane("%1", "codex", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert_eq!(snapshot.worktree_groups[0].rows.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_agent());
    assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
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
fn live_panes_do_not_render_unmatched_ledger_agents() {
    let codex = agent("codex", "sess-1", AgentStatus::Running, 1_000).worktree("/repo/main");

    let snapshot =
        room(Vec::new(), vec![codex]).with_live_panes(vec![pane("%1", "zsh", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .all(|row| !row.is_agent()),
        "non-attention agent rows must come from live pane presence"
    );
    assert!(
        snapshot.worktree_groups[0]
            .rows
            .iter()
            .any(|row| row.is_process() && row.name == "zsh"),
        "the live shell pane remains a process row"
    );
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
fn live_panes_keep_agent_attention_with_process() {
    let item = agent_ask(FeedKind::Permission, "claude", "live-claude");
    // The ask's session is live in the rollup, so it binds to that
    // session's pane and renders as attention.
    let session = agent("claude", "live-claude", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");

    // The pane runs under a `node` wrapper, not a `claude` foreground — the
    // bind is by the session's stamped pane id, so the command is moot.
    let snapshot = room(vec![item], vec![session])
        .with_live_panes(vec![pane("%1", "node", "/repo/main")], None);

    assert_eq!(snapshot.worktree_groups.len(), 1);
    let row = &snapshot.worktree_groups[0].rows[0];
    assert!(row.is_agent());
    assert_eq!(row.name, "claude");
    assert_eq!(row.status(), Some(AgentStatus::Waiting));
    assert_eq!(row.pane.as_ref().unwrap().pane_id.raw(), "%1");
}

#[test]
fn pending_agent_ask_preserves_session_prompt_description() {
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
fn answered_native_ui_ask_without_panes_stays_metadata_only() {
    // With no live frame, the rollup carries the pending ask as metadata but
    // emits no row. The pane-backed path above owns the moved-past display
    // recovery.
    let mut item = agent_ask(FeedKind::Question, "claude", "live-claude");
    item.updated_at = ago(600);
    let session =
        agent("claude", "live-claude", AgentStatus::Running, 2_000).worktree("/repo/main");

    let snapshot = room(vec![item], vec![session]);

    assert_eq!(snapshot.needs_attention.len(), 1);
    assert!(snapshot.worktree_groups.is_empty());
}

#[test]
fn two_same_kind_agents_bind_to_their_stamped_panes() {
    // Two claude sessions in one worktree are indistinguishable by name and
    // cwd alone; binding is by the hook-stamped pane id, so each session
    // lands on exactly its own pane instead of cross-wiring the rows.
    let older = agent("claude", "sess-a", AgentStatus::Idle, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    let newer = agent("claude", "sess-b", AgentStatus::Running, 2_000)
        .worktree("/repo/main")
        .in_pane("%2");

    let snapshot = room(Vec::new(), vec![older, newer]).with_live_panes(
        vec![
            pane("%1", "claude", "/repo/main"),
            pane("%2", "claude", "/repo/main"),
        ],
        None,
    );

    assert_eq!(snapshot.worktree_groups.len(), 1);
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
fn subagent_never_steals_its_parents_pane() {
    // A subagent runs in its parent's pane, so its lifecycle hooks stamp the
    // parent's pane id — parent and child both claim `%1`. The child here is
    // strictly more recently active than the parked parent, which would let
    // `max_by_key(last_activity)` bind the pane to the child. Panes bind root
    // agents only: `%1` stays the parent's row and the child nests under it.
    let parent = agent("claude", "sess-root", AgentStatus::Running, 1_000)
        .worktree("/repo/main")
        .in_pane("%1");
    // Newer activity than the parent (5s ago vs ~99s ago) — the flip trigger.
    let child = child_state("sess-root", "child-1", AgentStatus::Running, 5)
        .worktree("/repo/main")
        .in_pane("%1");

    let snapshot = room(Vec::new(), vec![parent, child])
        .with_live_panes(vec![pane("%1", "claude", "/repo/main")], None);

    let rows = &snapshot.worktree_groups[0].rows;
    assert_eq!(rows.len(), 1, "one pane binds exactly one top-level row");
    assert_eq!(
        rows[0].id, "sess-root",
        "the pane binds the root, not the child"
    );
    assert_eq!(rows[0].pane.as_ref().unwrap().pane_id.raw(), "%1");
    assert_eq!(
        rows[0].sub_agents().len(),
        1,
        "the child nests under the parent"
    );
    assert_eq!(rows[0].sub_agents()[0].id, "child-1");
    assert_eq!(rows[0].sub_agents()[0].name, "Explore");
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
