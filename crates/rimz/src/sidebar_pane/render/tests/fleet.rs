use super::*;

fn body_filter_hit(hit: &HitRegion) -> Option<BodyFilter> {
    match &hit.target {
        HitTarget::BodyFilter(filter) => Some(*filter),
        _ => None,
    }
}

/// The cockpit summary leads with today's sessions (`◎ N`, under the name)
/// over the live-agent count (`¤ N`); the cockpit below it splits the
/// make-up at a fixed height — the left cluster (`? ! ⏸ ✓`, each glyph
/// spaced from its count, a zero reading `? 0`) and the live-capacity tail
/// (`⢿ ○`) — so the body never shifts as agents change state.
#[test]
fn fleet_header_is_fixed_and_splits_the_make_up() {
    // Borderless layout: row 0 is the name, row 1 a blank line, row 2 the `◎`
    // summary, row 3 the `¤` summary, row 4 the hairline rule. An empty room
    // reads `◎ 0` on row 2 with no make-up beneath, so the body never moves.
    let empty = snapshot_with(Vec::new());
    let empty_screen = snapshot_to_screen(&empty, 40, 15);
    assert!(
        empty_screen.lines().nth(2).unwrap().contains("◎ 0"),
        "{empty_screen}"
    );
    assert!(
        empty_screen.lines().nth(3).unwrap().contains("¤ 0"),
        "{empty_screen}"
    );

    let working = agent(
        "w",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("a"),
    );
    let mut reasoning = agent(
        "t",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("b"),
    );
    // The thinking head is a per-row animation, never a cockpit bucket: a
    // pre-edit turn still tallies as working.
    reasoning.phase = crate::agents::TurnPhase::Reasoning;
    let snapshot = snapshot_with(vec![working, reasoning]);
    let screen = snapshot_to_screen(&snapshot, 40, 15);
    // Row 2 is the `◎` summary, row 3 the `¤` summary; row 5 is the bucket
    // make-up (row 1 is the blank line, row 4 the hairline rule).
    assert!(screen.lines().nth(3).unwrap().contains("¤ 2"), "{screen}");
    let buckets = screen.lines().nth(5).unwrap();
    // The make-up shows the attention and parked/done buckets first, then the
    // live-capacity tail; both running agents tally into the one working (⢿)
    // bucket.
    assert!(buckets.contains("? 0"), "{buckets}");
    assert!(buckets.contains("! 0"), "{buckets}");
    // The paused glyph carries the U+FE0E text-presentation selector.
    assert!(buckets.contains("⏸\u{FE0E} 0"), "{buckets}");
    assert!(buckets.contains("✓ 0"), "{buckets}");
    assert!(buckets.contains("⢿ 2"), "{buckets}");
    assert!(buckets.contains("○ 0"), "{buckets}");
    let bucket_positions = [
        buckets.find("? 0").expect("waiting bucket"),
        buckets.find("! 0").expect("failed bucket"),
        buckets.find("⏸\u{FE0E} 0").expect("paused bucket"),
        buckets.find("✓ 0").expect("success bucket"),
        buckets.find("⢿ 2").expect("running bucket"),
        buckets.find("○ 0").expect("idle bucket"),
    ];
    assert!(
        bucket_positions.windows(2).all(|pair| pair[0] < pair[1]),
        "make-up order is ? ! ⏸ ✓ | ⢿ ○: {buckets}"
    );
    assert!(!buckets.contains('⠁'), "no thinking bucket: {buckets}");
    // The default selection lands on the first row, so its worktree reads as
    // one lane: the header gains the dotted seal and a `▎` lane spine.
    assert!(
        screen.lines().any(|line| line.contains("main")),
        "fleet header wrapped or shifted:\n{screen}"
    );
    assert!(
        screen.contains('▎'),
        "the selected worktree shows the lane spine:\n{screen}"
    );
}

#[test]
fn cockpit_reads_workspace_tally_while_store_reads_global_tally() {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.value_tally = Some(bottom_tally());
    let headline = crate::SpendWindow {
        usd: 1.23,
        tokens: 30_000,
        input: 20_000,
        output: 10_000,
        cache_read: 5_000,
        sessions: 2,
        ..Default::default()
    };
    snapshot.workspace_value_tally = Some(crate::SpendTally {
        headline,
        week: headline,
        month: headline,
        year: headline,
    });

    let rendered = snapshot_to_screen(&snapshot, 60, 14);
    let summary = rendered.lines().nth(2).unwrap();
    let spend = rendered.lines().nth(3).unwrap();

    assert!(summary.contains("◎ 2"), "scoped sessions:\n{rendered}");
    assert!(summary.contains("30k"), "scoped tokens:\n{rendered}");
    assert!(
        spend.contains("$1.23"),
        "scoped headline spend:\n{rendered}"
    );
    assert!(
        rendered.contains("$12.34") && rendered.contains("$56.78"),
        "global store week/month stay visible:\n{rendered}"
    );
}

#[test]
fn cockpit_healthy_daily_cap_stays_quiet_on_headline_spend() {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.today_spend_live_usd = Some(1.25);
    snapshot.today_spend_epoch_secs = Some(10);
    snapshot.fleet_day_spend_usd = Some(41.20);
    snapshot.fleet_day_spend_epoch_secs = Some(20);
    snapshot.fleet_budget = Some(crate::DailyBudgetView {
        cap_usd: 50.0,
        spend_usd: 41.20,
        parked: false,
    });

    let rendered = snapshot_to_screen(&snapshot, 60, 14);
    let spend = rendered.lines().nth(3).expect("cockpit spend row");
    assert!(spend.contains("$1.25"), "{rendered}");
    assert!(
        !spend.contains(" of "),
        "healthy cap stays quiet: {rendered}"
    );
}

#[test]
fn cockpit_tripped_daily_cap_uses_day_spend_and_names_the_cap() {
    let mut snapshot = snapshot_with(Vec::new());
    snapshot.today_spend_live_usd = Some(1.25);
    snapshot.today_spend_epoch_secs = Some(10);
    snapshot.fleet_day_spend_usd = Some(41.20);
    snapshot.fleet_day_spend_epoch_secs = Some(20);
    snapshot.fleet_budget = Some(crate::DailyBudgetView {
        cap_usd: 50.0,
        spend_usd: 41.20,
        parked: true,
    });

    let rendered = snapshot_to_screen(&snapshot, 60, 14);
    let spend = rendered.lines().nth(3).expect("cockpit spend row");
    assert!(spend.contains("$41.20 of $50/day"), "{rendered}");
}
/// The read cockpit `?`/`!` buckets hold their fixed semantic tone at rest —
/// `?` yellow — regardless of how old the oldest contributing row is.
#[test]
fn attention_bucket_holds_a_fixed_tone() {
    let theme = Theme::fixed(false);
    let bucket_fg = |idle_secs: u64| {
        let mut waiting = agent(
            "w",
            "claude",
            AgentStatus::Waiting,
            Some("/repo/main"),
            Some("main"),
            Some("a"),
        );
        waiting.last_activity = fixed_now() - Duration::from_secs(idle_secs);
        let snapshot = snapshot_with(vec![waiting]);
        fleet_header_lines(
            &theme,
            &snapshot.worktree_groups,
            snapshot.now,
            None,
            0,
            60,
            None,
        )
        .0
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.contains('?'))
        .map(|span| span.style.fg)
        .expect("the make-up line carries the ? bucket")
    };
    for idle_secs in [5 * 60, 40 * 60, 2 * 60 * 60] {
        assert_eq!(
            bucket_fg(idle_secs),
            theme.warn(Modifier::empty()).fg,
            "the ? bucket holds yellow at {idle_secs}s, no age slide",
        );
    }
}
/// State glyphs keep their cockpit tier. A zero bucket keeps its resting style
/// and rests only its count at the soft stat tier; idle's glyph and count read
/// at that same soft tier, while the colored states keep their semantic tone.
#[test]
fn state_glyphs_keep_their_cockpit_tier() {
    let theme = Theme::fixed(false);
    // A room with one working agent: every bucket but `⢿` is zero.
    let working = agent(
        "w",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("a"),
    );
    let snapshot = snapshot_with(vec![working]);
    let lines = fleet_header_lines(
        &theme,
        &snapshot.worktree_groups,
        snapshot.now,
        None,
        0,
        60,
        None,
    )
    .0;
    let spans: Vec<_> = lines.iter().flat_map(|line| line.spans.iter()).collect();
    let glyph_style = |glyph: &str| {
        spans
            .iter()
            .find(|span| span.content.as_ref() == glyph)
            .map(|span| span.style)
            .unwrap_or_else(|| panic!("a zero bucket splits the {glyph} glyph from its count"))
    };
    assert_eq!(
        glyph_style("?"),
        theme.warn(Modifier::empty()),
        "a zero ? bucket rests at its yellow floor, unbolded"
    );
    assert_eq!(
        glyph_style("✓"),
        theme.good(Modifier::empty()),
        "a zero ✓ bucket keeps the quiet success tone at full strength"
    );
    assert_eq!(
        glyph_style("○"),
        theme.body(),
        "a zero idle bucket carries the soft stat gray"
    );
    let zero_counts: Vec<_> = spans
        .iter()
        .filter(|span| span.content.as_ref() == " 0")
        .collect();
    assert!(!zero_counts.is_empty(), "zero buckets render their counts");
    assert!(
        zero_counts.iter().all(|span| span.style == theme.body()),
        "zero counts rest at the soft stat tier"
    );
    assert_eq!(labels::status_style(&theme, AgentStatus::Idle).fg, None);
    assert_eq!(
        labels::status_style(&theme, AgentStatus::Success),
        theme.good(Modifier::empty()),
        "success keeps the quiet success tone at full strength"
    );
}
/// With color, a make-up pick changes fill and weight only: whichever bucket
/// is active, the line renders glyph-for-glyph identical text and every hit
/// covers the same `glyph count` footprint, so a filter change moves no cells.
#[test]
fn make_up_filter_keeps_every_glyph_still_across_picks() {
    let theme = Theme::fixed(false);
    let snapshot = make_up_snapshot();
    let compose = |filter| {
        fleet_header_lines(
            &theme,
            &snapshot.worktree_groups,
            snapshot.now,
            filter,
            0,
            38,
            None,
        )
    };
    let (resting_lines, resting_hits) = compose(None);
    let (failed_lines, failed_hits) = compose(Some(BodyFilter::Status(AgentStatus::Failed)));
    let (running_lines, running_hits) = compose(Some(BodyFilter::Status(AgentStatus::Running)));
    let resting = make_up_text(&resting_lines);
    let failed = make_up_text(&failed_lines);
    let running = make_up_text(&running_lines);
    assert_eq!(resting, failed, "the failed pick moves no glyphs");
    assert_eq!(resting, running, "the running pick moves no glyphs");
    assert_eq!(
        resting_hits, failed_hits,
        "the failed pick keeps click targets fixed"
    );
    assert_eq!(
        resting_hits, running_hits,
        "the running pick keeps click targets fixed"
    );
    assert!(
        [resting.as_str(), failed.as_str(), running.as_str()]
            .iter()
            .all(|text| !text.contains('┤')),
        "with color, no caps paint:\n{resting}\n{failed}\n{running}"
    );
    for (lines, hits, status, expected) in [
        (&failed_lines, &failed_hits, AgentStatus::Failed, "! 1"),
        (&running_lines, &running_hits, AgentStatus::Running, "⢿ 1"),
    ] {
        let text = make_up_text(lines);
        let hit = hits
            .iter()
            .find(|hit| body_filter_hit(hit) == Some(BodyFilter::Status(status)))
            .expect("active bucket keeps its hit");
        let footprint = text_cell_range(&text, hit.columns.start, hit.columns.end);
        assert_eq!(footprint, expected, "hit covers the fixed bucket");
    }
}
/// Under `NO_COLOR` the chip fill drops, so reverse-video marks the same fixed
/// `glyph count` cells instead of adding caps that would move the bucket text.
#[test]
fn make_up_filter_no_color_marks_the_fixed_bucket_cells() {
    let theme = Theme::fixed(true);
    let snapshot = make_up_snapshot();
    let (resting_lines, resting_hits) = fleet_header_lines(
        &theme,
        &snapshot.worktree_groups,
        snapshot.now,
        None,
        0,
        38,
        None,
    );
    let (lines, hits) = fleet_header_lines(
        &theme,
        &snapshot.worktree_groups,
        snapshot.now,
        Some(BodyFilter::Status(AgentStatus::Failed)),
        0,
        38,
        None,
    );
    assert_eq!(resting_hits, hits, "the pick keeps click targets fixed");
    let text = make_up_text(&lines);
    assert_eq!(
        make_up_text(&resting_lines),
        text,
        "the pick moves no glyphs under NO_COLOR"
    );
    assert!(
        !text.contains('┤'),
        "caps would move the bucket text:\n{text}"
    );
    let hit = hits
        .iter()
        .find(|hit| body_filter_hit(hit) == Some(BodyFilter::Status(AgentStatus::Failed)))
        .expect("the picked bucket keeps its hit");
    let footprint = text_cell_range(&text, hit.columns.start, hit.columns.end);
    assert_eq!(footprint, "! 1", "the hit covers the fixed bucket");
    let active = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "! 1")
        .expect("the active bucket stays one span");
    assert!(
        active.style.add_modifier.contains(Modifier::REVERSED),
        "NO_COLOR marks the active fixed cells by modifier"
    );
}

#[test]
fn selected_idle_filter_preserves_soft_gray_with_reverse_video() {
    let theme = Theme::fixed(false);
    let idle = agent(
        "idle-1",
        "codex",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("resting"),
    );
    let snapshot = snapshot_with(vec![idle]);
    let (lines, _) = fleet_header_lines(
        &theme,
        &snapshot.worktree_groups,
        snapshot.now,
        Some(BodyFilter::Status(AgentStatus::Idle)),
        0,
        38,
        None,
    );
    let active = lines[0]
        .spans
        .iter()
        .find(|span| span.content.as_ref() == "○ 1")
        .expect("the selected idle bucket stays one span");
    assert_eq!(active.style.fg, theme.body().fg);
    assert!(
        active.style.add_modifier.contains(Modifier::REVERSED)
            && active.style.add_modifier.contains(Modifier::BOLD),
        "selected idle uses soft gray plus weight and reverse video"
    );
}

/// A zero bucket emits no hit — inert, as if not a tab — and every emitted
/// hit's column range covers exactly its bucket's `glyph count` text, in the
/// left cluster (content-absolute) and the right (offset to wherever
/// `pin_right` landed it) alike.
#[test]
fn make_up_zero_buckets_emit_no_hit_and_hits_cover_their_text() {
    let theme = Theme::fixed(false);
    let snapshot = make_up_snapshot();
    let (lines, hits) = fleet_header_lines(
        &theme,
        &snapshot.worktree_groups,
        snapshot.now,
        None,
        0,
        38,
        None,
    );
    let text = make_up_text(&lines);
    assert_eq!(
        hits.iter().filter_map(body_filter_hit).collect::<Vec<_>>(),
        vec![
            BodyFilter::Status(AgentStatus::Failed),
            BodyFilter::Status(AgentStatus::Running),
        ],
        "only the non-zero buckets are tabs"
    );
    for hit in &hits {
        let footprint = text_cell_range(&text, hit.columns.start, hit.columns.end);
        let Some(BodyFilter::Status(status)) = body_filter_hit(hit) else {
            panic!("fleet line emits only status buckets");
        };
        assert_eq!(
            footprint,
            format!("{} 1", labels::status_glyph(&theme, status)),
            "the hit sits exactly on its bucket:\n{text}"
        );
    }
}
/// A bucket the width clips keeps no hit — dropped whole rather than left
/// aimed past the visible edge, the tab rail's drop-whole-tab rule.
#[test]
fn make_up_clipped_bucket_drops_its_hit() {
    let theme = Theme::fixed(false);
    let snapshot = make_up_snapshot();
    // 18 columns forces the left-packed fallback and clips the live-capacity tail, so
    // the right-cluster `⢿ 1` bucket falls past the edge.
    let (_, hits) = fleet_header_lines(
        &theme,
        &snapshot.worktree_groups,
        snapshot.now,
        None,
        0,
        18,
        None,
    );
    assert!(
        hits.iter().all(|hit| usize::from(hit.columns.end) <= 18),
        "no hit points past the visible edge: {hits:?}"
    );
    assert_eq!(
        hits.iter().filter_map(body_filter_hit).collect::<Vec<_>>(),
        vec![BodyFilter::Status(AgentStatus::Failed)],
        "the clipped working bucket keeps no hit"
    );
}

#[test]
fn make_up_buckets_pulse_only_while_unread() {
    let theme = Theme::fixed(false);
    let mut snapshot = make_up_snapshot();
    let bucket_style = |snapshot: &SidebarSnapshot, content: &str, animation_phase| {
        fleet_header_lines(
            &theme,
            &snapshot.worktree_groups,
            snapshot.now,
            None,
            animation_phase,
            38,
            lead_unread(&snapshot.worktree_groups).map(|(_, status)| status),
        )
        .0
        .into_iter()
        .flat_map(|line| line.spans)
        .find(|span| span.content.as_ref() == content)
        .unwrap_or_else(|| panic!("{content} bucket"))
        .style
    };
    let read: Vec<_> = (0..32)
        .map(|phase| bucket_style(&snapshot, "! 1", phase))
        .collect();
    assert!(read.iter().all(|style| style.add_modifier.is_empty()));
    assert!(
        read.iter().all(|style| style.fg == read[0].fg),
        "read buckets stay static phase to phase"
    );

    snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
        .find(|row| row.status() == Some(AgentStatus::Failed))
        .expect("failed row")
        .unread = true;
    let unread: Vec<_> = (0..32)
        .map(|phase| bucket_style(&snapshot, "! 1", phase))
        .collect();
    // At indexed depth the unread effect (the default shimmer here) rides weight,
    // not color: the moving beam bolds the cell as it passes, leaves it plain
    // otherwise, never dims, and holds the bucket's tone throughout.
    assert!(
        unread
            .iter()
            .any(|style| style.add_modifier == Modifier::BOLD),
        "an unread bucket bolds as the beam passes"
    );
    assert!(
        unread.iter().any(|style| style.add_modifier.is_empty()),
        "and rests plain between passes — a weight cue, not constant bold"
    );
    assert!(
        unread
            .iter()
            .all(|style| !style.add_modifier.contains(Modifier::DIM))
    );
    assert!(
        unread.iter().all(|style| style.fg == unread[0].fg),
        "the indexed fallback keeps the bucket's tone — the cue is weight"
    );

    let mut success = agent(
        "done",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("finished"),
    );
    success.last_activity = fixed_now() - Duration::from_secs(5 * 60);
    let mut success_snapshot = snapshot_with(vec![success]);
    let read_success: Vec<_> = (0..32)
        .map(|phase| bucket_style(&success_snapshot, "✓ 1", phase))
        .collect();
    assert!(
        read_success
            .iter()
            .all(|style| style == &labels::status_rest_style(&theme, AgentStatus::Success))
    );

    success_snapshot.worktree_groups[0].rows[0].unread = true;
    let unread_success: Vec<_> = (0..32)
        .map(|phase| bucket_style(&success_snapshot, "✓ 1", phase))
        .collect();
    // An unread result is a look, not an act, so the success bucket never leads —
    // it settles to the steady bright crest: held bold throughout, the success
    // tone constant, the moving beam reserved for the lead actionable bucket.
    assert!(
        unread_success
            .iter()
            .all(|style| style.add_modifier == Modifier::BOLD),
        "unread success holds a steady bold crest, never resting plain"
    );
    assert!(
        unread_success
            .iter()
            .all(|style| style.fg == unread_success[0].fg),
        "the success tone holds steady — bright, not a moving beam"
    );

    let mut running_snapshot = make_up_snapshot();
    running_snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
        .find(|row| row.status() == Some(AgentStatus::Running))
        .expect("running row")
        .unread = true;
    let unread_running: Vec<_> = (0..32)
        .map(|phase| bucket_style(&running_snapshot, "⢿ 1", phase))
        .collect();
    assert!(
        unread_running
            .iter()
            .all(|style| style.add_modifier == Modifier::BOLD),
        "recovered unread running rows hold a steady crest in the working bucket"
    );

    let mut idle = agent(
        "idle",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("idle"),
    );
    idle.last_activity = fixed_now() - Duration::from_secs(5 * 60);
    let mut idle_snapshot = snapshot_with(vec![idle]);
    idle_snapshot.worktree_groups[0].rows[0].unread = true;
    let unread_idle: Vec<_> = (0..32)
        .map(|phase| bucket_style(&idle_snapshot, "○ 1", phase))
        .collect();
    assert!(
        unread_idle
            .iter()
            .all(|style| style.add_modifier == Modifier::BOLD),
        "recovered unread idle rows hold a steady crest in the idle bucket"
    );
}

#[test]
fn render_cockpit_unread_count() {
    let mut snapshot = make_up_snapshot();
    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Open);
    snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
        .find(|row| row.status() == Some(AgentStatus::Failed))
        .expect("failed row")
        .unread = true;

    let screen = snapshot_to_screen(&snapshot, 38, 20);
    assert!(
        screen.lines().any(|line| line.contains("¤ 2 (1) ⑃ 1")),
        "live-agent summary carries unread and open-PR counts in order:\n{screen}"
    );
}

#[test]
fn render_cockpit_counts_open_prs_including_finished_lanes() {
    let mut snapshot = make_up_snapshot();
    let mut merged = snapshot.worktree_groups[0].clone();
    merged.key.push_str("-merged");
    merged.label.push_str("-merged");
    merged.pr_state = Some(crate::WorktreePrState::Merged);
    merged.pr_number = Some(303);
    snapshot.worktree_groups[0].pr_state = Some(crate::WorktreePrState::Open);
    snapshot.worktree_groups[0].pr_number = Some(101);
    snapshot.worktree_groups[0].finished = true;
    snapshot.worktree_groups[1].pr_state = Some(crate::WorktreePrState::Open);
    snapshot.worktree_groups[1].pr_number = Some(202);
    snapshot.worktree_groups.push(merged);

    let screen = snapshot_to_screen(&snapshot, 38, 20);
    let spend = screen
        .lines()
        .find(|line| line.contains('¤'))
        .expect("cockpit spend line");
    assert!(spend.contains("⑃ 2"), "two open lane PRs count:\n{screen}");
    assert!(
        !spend.contains('#'),
        "the aggregate omits lane PR numbers:\n{screen}"
    );
}

#[test]
fn render_cockpit_hides_zero_open_prs() {
    let screen = snapshot_to_screen(&make_up_snapshot(), 38, 20);
    let spend = screen
        .lines()
        .find(|line| line.contains('¤'))
        .expect("cockpit spend line");
    assert!(!spend.contains('⑃'), "zero open PRs stay hidden:\n{screen}");
}

/// The `↑ N need you` jump banner appears when the lead leaves the viewport and
/// stays hidden while scroll-to-top already shows it.
#[test]
fn unread_jump_banner_shows_only_when_the_lead_is_scrolled_off() {
    let snapshot = overflowing_fleet_with_unread_lead();

    let screen = snapshot_to_screen(&snapshot, 38, 20);
    assert!(
        !screen.contains("need you"),
        "banner stays hidden when the top-ranked lead is visible:\n{screen}"
    );

    let ui = UiState {
        scroll_offset: 99,
        manual_scroll: Some(ManualScroll {
            selection_at_start: None,
        }),
        ..UiState::default()
    };
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 38, 20);
    assert!(
        screen.contains("↑ 1 need you"),
        "banner appears when wheel-pinned past the lead:\n{screen}"
    );
}

/// A make-up bucket click narrows the body to that status: only the `!` card
/// remains — the running agent's worktree is skipped whole (header included),
/// the status-less process tail drops, and the `+K more` line is suppressed —
/// while the cockpit make-up keeps the full-fleet counts (`⢿` still reads 1).
/// With color on, the picked bucket is a chip — fill and weight, never a
/// glyph — so the make-up line's text matches the resting frame exactly.
#[test]
fn render_make_up_filter_narrows_the_body() {
    let mut snapshot = make_up_snapshot();
    let main_group = snapshot
        .worktree_groups
        .iter_mut()
        .find(|group| group.label == "main")
        .expect("the fixture groups by worktree");
    main_group.rows.push(crate::SidebarRow {
        id: "%9".to_owned(),
        name: "zsh".to_owned(),
        pane: Some(pane("%9", "zsh", "/home/me/query-engine")),
        worktree_path: Some("/home/me/query-engine".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: fixed_now(),
        card: crate::RowCard::Process(crate::ProcessCard::default()),
    });
    let ui = UiState {
        make_up_filter: Some(BodyFilter::Status(crate::agents::AgentStatus::Failed)),
        ..Default::default()
    };
    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 38, 20);
    assert!(
        screen.contains("! claude"),
        "the failed card is the body:\n{screen}"
    );
    assert!(
        !screen.contains("codex") && !screen.contains("feature-migration"),
        "a group with no matching row is skipped whole:\n{screen}"
    );
    assert!(
        !screen.contains("zsh"),
        "a status-less process row drops under a filter:\n{screen}"
    );
    assert!(
        !screen.contains("more"),
        "the producer-capped +K line is suppressed under a filter:\n{screen}"
    );
    assert!(
        screen.contains("⢿ 1"),
        "the make-up still counts the full fleet:\n{screen}"
    );
    assert_snapshot("make_up_filter_failed", screen);
}

#[test]
fn render_unread_filter_narrows_the_body() {
    let mut snapshot = make_up_snapshot();
    let failed = snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
        .find(|row| row.status() == Some(AgentStatus::Failed))
        .expect("failed row");
    failed.unread = true;
    let ui = UiState {
        make_up_filter: Some(BodyFilter::Unread),
        ..Default::default()
    };

    let screen = snapshot_to_screen_with_alert_and_ui(&snapshot, None, &ui, 38, 20);
    assert!(
        screen.contains("! claude"),
        "the unread failed card is the body:\n{screen}"
    );
    assert!(
        !screen.contains("codex") && !screen.contains("feature-migration"),
        "read rows and groups with no unread row are skipped whole:\n{screen}"
    );
    assert!(
        screen.contains("⢿ 1"),
        "the make-up still counts the full fleet:\n{screen}"
    );
}

/// A compacting agent counts as **working** (`⢿`) in the cockpit — the
/// compaction pulse, like the thinking head, is a per-row head and never
/// a bucket.
#[test]
fn compacting_agent_counts_as_working() {
    let mut compacting = agent(
        "c",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("t"),
    );
    compacting.compacting_since = Some(fixed_now());
    compacting.phase = crate::agents::TurnPhase::Reasoning;
    let snapshot = snapshot_with(vec![compacting]);
    let screen = snapshot_to_screen(&snapshot, 40, 12);
    // Row 5 is the make-up: name(0), blank(1), `¤`(2), `◎`(3), hairline(4).
    let buckets = screen.lines().nth(5).unwrap();
    assert!(
        buckets.contains("⢿ 1"),
        "compacting counts as working: {buckets}"
    );
    assert!(!buckets.contains('⠁'), "no thinking bucket: {buckets}");
}
