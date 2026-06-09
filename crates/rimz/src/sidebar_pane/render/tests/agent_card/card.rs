use super::*;

#[test]
fn render_agent_capability_and_window() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Failed,
        Some("/repo/feature-migration"),
        Some("feature-migration"),
        Some("db migrate"),
    );
    claude.model = Some("Opus".to_owned());
    claude.effort = Some("xhigh".to_owned());
    // The hook-derived window renders as the identity line's `1M` token.
    claude.context_window = Some(1_000_000);
    claude.last_activity = fixed_now() - Duration::from_secs(4 * 60);
    let snapshot = snapshot_with(Vec::new(), vec![claude]);

    assert_snapshot("agent_capability", snapshot_to_screen(&snapshot, 34, 12));
}
#[test]
fn render_enriched_selected_agent_card() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/feature-migration"),
        Some("feature-migration"),
        Some("db migrate"),
    );
    // Transcript scalars are the coarse fallback; the statusline enriches the
    // display name (`Opus` → `Opus 4.8`). Effort stays with the hook-derived
    // configured value (`xhigh`) even when the statusline reports a capped
    // model-effective level (`high`).
    claude.model = Some("Opus".to_owned());
    claude.effort = Some("xhigh".to_owned());
    claude.context_pct = Some(38);
    claude.total_tokens = Some(12_400);
    claude.todo_done = Some(3);
    claude.todo_total = Some(5);
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(Vec::new(), vec![claude]);
    snapshot.worktree_groups[0].diff_added = Some(127);
    snapshot.worktree_groups[0].diff_removed = Some(43);
    snapshot.worktree_groups[0].commits_ahead = Some(3);
    snapshot.worktree_groups[0].commits_behind = Some(1);
    snapshot.worktree_groups[0].trunk = Some("main".to_owned());
    snapshot.worktree_groups[0].clean = Some(true);

    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            help_visible: false,
            animation_phase: 0,
            line_map: Vec::new(),
            ..Default::default()
        },
        54,
        14,
    );

    // The worktree's git story sits on the group header: the ⇡/⇣ commit
    // delta leads the worktree-total diff. Clean but carrying work: the
    // landed markers need a zero diff too, so the cluster stays.
    assert!(rendered.contains("⇡3 ⇣1  +127 -43"), "header:\n{rendered}");
    assert!(
        !rendered.contains('≡') && !rendered.contains("✓ main"),
        "a work-carrying worktree wears no landed marker:\n{rendered}"
    );
    // Line 1 carries identity + capability + cost; line 2 is the session
    // name; the model display name sheds its window qualifier (`Opus 4.8
    // (1M context)` → `Opus 4.8`) — the dedicated window token (the
    // statusline's 200k reading) carries the figure.
    assert!(rendered.contains("Opus 4.8"));
    assert!(!rendered.contains("(1M"));
    assert!(!rendered.contains("context"));
    assert!(rendered.contains("xhigh"), "effort:\n{rendered}");
    assert!(rendered.contains("· 200k"), "window token:\n{rendered}");
    // Per-row cost now reads at full cent resolution, like every other spend.
    assert!(rendered.contains("$1.27"));
    // Line 2 is the full-width description; todo dots inline at L2.
    assert!(rendered.contains("ledger refactor"));
    assert!(rendered.contains("●●●○○ 3/5"));
    // The context bar carries the `▣` label and the percent used as its
    // value (always — the window size moved to the token line below); the
    // fill carries the same reading.
    assert!(rendered.contains("▣ "));
    // The account-scoped 5h/7d budgets are gone from the row — they live in
    // the provider dashboard now.
    assert!(!rendered.contains("5h↻"));
    assert!(!rendered.contains("7d↻"));
    // The card carries the context line at rest: ▤ the filled window
    // (input + cache-write + cache-read — the ▣ meter's numerator, so the
    // 38.2% above and this 76k are one measurement), a · seam, then the
    // latest call's composition ordered by how the window filled — ◌
    // cache read, ◍ cache write, ↘ fresh input, ↗ output. The ◇ totals
    // stay the cockpit/ledger vocabulary; the window size no longer rides
    // this line.
    assert!(
        rendered.contains("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k"),
        "context line:\n{rendered}"
    );
    assert!(!rendered.contains('◇'), "no fleet total on the card");
    assert!(
        !rendered.contains("ctx"),
        "window size left the token line:\n{rendered}"
    );
    assert_snapshot("enriched_selected_agent_card", rendered);
}
#[test]
fn render_api_error_dead_turn_card() {
    // A turn that died on a provider API error fires no Stop hook; the
    // projection escalates the row to the attention `!` and line 2 quotes
    // the upstream error text (dim) instead of the task fall-through, so
    // the card says why without a jump.
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.last_activity = fixed_now() - Duration::from_secs(60);
    let mut context = claude_context(fixed_now());
    context.turn_error = Some(AgentTurnError {
        class: TurnErrorClass::Failed,
        at: fixed_now() - Duration::from_secs(10),
        label: Some("API Error: Server Error".to_owned()),
    });
    claude.context = Some(context);
    let snapshot = snapshot_with(Vec::new(), vec![claude]);

    let rendered = snapshot_to_screen(&snapshot, 54, 14);

    assert!(
        rendered.contains("! claude"),
        "the dead turn escalates to the attention glyph:\n{rendered}"
    );
    assert!(
        rendered.contains("API Error: Server Error"),
        "line 2 quotes the upstream error text:\n{rendered}"
    );
    assert!(
        !rendered.contains("ledger refactor"),
        "the reason takes the line over the session-name fall-through:\n{rendered}"
    );
    assert_snapshot("api_error_dead_turn_card", rendered);
}
#[test]
fn render_omits_history_sections() {
    let workspace = fixed_workspace();
    let mut answered = FeedItem::new(
        workspace.clone(),
        Surface::Script,
        FeedKind::Question,
        "Deploy staging?",
        "deploy.sh",
        "cli",
    );
    answered.status = FeedStatus::Resolved;
    let event = EventEnvelope::new(
        workspace.clone(),
        "rimz-test",
        "rimz",
        "cli",
        "event.emit",
        json!({ "kind": "build.started", "title": "Building web" }),
    );
    let mut snapshot = SidebarSnapshot::build_with_carryover(
        workspace,
        vec![answered],
        vec![event],
        vec![],
        Timestamp::now(),
    );
    snapshot.display_name = "query-engine".to_owned();
    let rendered = snapshot_to_screen(&snapshot, 38, 10);

    assert!(!rendered.contains("all clear"));
    assert!(!rendered.contains("Recent activity"));
    assert!(!rendered.contains("Recently answered"));
}
