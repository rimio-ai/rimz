use super::*;
use crate::sidebar_pane::render::PresentationToggles;

#[test]
fn card_head_draws_no_slot_digit_at_any_width() {
    let state = agent(
        "slot",
        "claude",
        AgentStatus::Waiting,
        Some("/repo/main"),
        Some("main"),
        Some("task"),
    );
    let mut snapshot = snapshot_with(vec![state]);
    snapshot
        .rows_mut()
        .next()
        .unwrap()
        .as_agent_mut()
        .unwrap()
        .delegated_cost_usd = Some(1.0);
    let mut plain = snapshot.clone();
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut plain.worktree_groups)[0].rows)
        [0]
    .pane = None;
    let head_text = |snapshot: &SidebarSnapshot, width: usize| {
        let lines = group_lines_at_width(snapshot, &Theme::fixed(false), usize::MAX, width);
        let head = lines
            .iter()
            .find(|line| line.spans.iter().any(|span| span.content == "claude"))
            .unwrap();
        assert!(
            head.spans.iter().all(|span| span.content != " 1"),
            "width={width}: {head:?}"
        );
        head.to_string()
    };
    for width in [17, 18, 19, 46, 60] {
        assert_eq!(
            head_text(&snapshot, width),
            head_text(&plain, width),
            "width={width}"
        );
    }
}

#[test]
fn stale_card_head_draws_no_slot_digit() {
    let mut state = agent(
        "stale",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    );
    state.last_activity = fixed_now() - Duration::from_secs(24 * 60 * 60);
    let mut snapshot = snapshot_with(vec![state]);
    let row = snapshot.rows_mut().next().unwrap();
    row.unread = false;
    row.last_activity = fixed_now() - Duration::from_secs(24 * 60 * 60);
    let lines = group_lines(&snapshot, &Theme::fixed(false), usize::MAX);
    let head = lines
        .iter()
        .find(|line| line.spans.iter().any(|span| span.content == "claude"))
        .unwrap();
    assert!(
        head.spans.iter().all(|span| span.content != " 1"),
        "{head:?}"
    );
}

#[test]
fn tier_move_marker_follows_the_model_at_exact_width_boundaries() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("route the work"),
    );
    codex.model = Some("astra".into());
    codex.tier = Some(
        serde_json::from_value(serde_json::json!({
            "tier": "senior", "model": "astra",
            "unbuildable": [{"model": "opus", "error": "enable model invocation"}]
        }))
        .unwrap(),
    );
    for set in ["unicode", "nerd_font"] {
        let mut snapshot = snapshot_with(vec![codex.clone()]);
        snapshot.theme.glyphs.set = Some(set.into());
        for width in [31, 32, 33, 45, 46, 47] {
            let rendered = snapshot_to_screen(&snapshot, width, 15);
            assert_eq!(
                rendered.contains("Astra ↪"),
                width >= 32,
                "{set}, {width}:\n{rendered}"
            );
            assert_eq!(
                rendered.contains('↪'),
                width >= 32,
                "{set}, {width}:\n{rendered}"
            );
        }
        if set == "unicode" {
            assert_snapshot("tier_move_card", snapshot_to_screen(&snapshot, 46, 15));
        }
    }
    codex.tier.as_mut().unwrap().unbuildable.clear();
    assert!(!snapshot_to_screen(&snapshot_with(vec![codex.clone()]), 46, 15).contains('↪'));
    codex
        .tier
        .as_mut()
        .unwrap()
        .unbuildable
        .push(crate::agents::TierUnbuildable {
            model: "opus".into(),
            error: "enable model invocation".into(),
        });
    codex.model = None;
    assert!(!snapshot_to_screen(&snapshot_with(vec![codex]), 46, 15).contains('↪'));
}

#[test]
fn idle_agent_card_lead_uses_soft_gray_when_unselected() {
    let idle = agent(
        "idle-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("resting"),
    );
    let snapshot = snapshot_with(vec![idle]);
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 1);
    let lead = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "○")
        .expect("idle card lead glyph renders as its own span");

    assert_eq!(lead.style, theme.body());
}

#[test]
fn selected_default_idle_agent_card_lead_stays_colorless() {
    let idle = agent(
        "idle-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("resting"),
    );
    let snapshot = snapshot_with(vec![idle]);
    let theme = Theme::fixed(false);
    let lines = group_lines(&snapshot, &theme, 0);
    let lead = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "○")
        .expect("idle card lead glyph renders as its own span");

    // The selected idle lead keeps no foreground tint — it stays colorless — while
    // the selection band lays its dark fill behind every cell of the card: at
    // indexed depth the band recesses one xterm cell below the panel (gray 235 →
    // 234), the cube's carry of the truecolor sub-cell recess.
    assert_eq!(lead.style.fg, None);
    assert_eq!(lead.style.bg, Some(Color::Indexed(234)));
}

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
    claude.usage.context_window = Some(1_000_000);
    claude.last_activity = fixed_now() - Duration::from_secs(4 * 60);
    let snapshot = snapshot_with(vec![claude]);

    let rendered = snapshot_to_screen(&snapshot, 34, 15);

    assert!(
        rendered.contains("! claude"),
        "a failed agent leads with the attention glyph:\n{rendered}"
    );
    assert!(
        rendered.contains("Opus · 1M"),
        "the identity line carries the capability and hook-derived window token:\n{rendered}"
    );
}

#[test]
fn render_cursor_normalized_model_metadata_once() {
    let mut cursor = agent(
        "cursor-1",
        "cursor",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("normalize model metadata"),
    );
    let mut context = claude_context(fixed_now());
    context.source = "cursor".to_owned();
    context.model_id = Some("auto".to_owned());
    context.model_display_name = Some("GPT-5.6 Sol".to_owned());
    context.effort = Some("medium".to_owned());
    context.tokens.as_mut().unwrap().context_window_size = Some(200_000);
    cursor.context = Some(context);

    let rendered = snapshot_to_screen(&snapshot_with(vec![cursor]), 54, 15);

    assert!(
        rendered.contains("GPT Sol 5.6 · medium · 200k"),
        "the identity line separates Cursor's normalized capabilities:\n{rendered}"
    );
    assert!(
        !rendered.contains("272K") && !rendered.contains("272k"),
        "the nominal selector window does not leak into the card:\n{rendered}"
    );
}

#[test]
fn cursor_idle_session_name_keeps_the_fresh_card_thin() {
    let mut cursor = agent(
        "cursor-1",
        "cursor",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    let mut context = codex_context(fixed_now());
    context.source = "cursor".to_owned();
    context.session_name = Some("provider-owned pre-prompt title".to_owned());
    context.model_id = None;
    context.model_display_name = None;
    context.effort = None;
    context.rate_limits = None;
    cursor.context = Some(context);
    let theme = Theme::fixed(true);

    let before_prompt = line_texts(&group_lines(
        &snapshot_with(vec![cursor.clone()]),
        &theme,
        0,
    ));
    assert_eq!(
        before_prompt.len(),
        2,
        "a provider-owned session title cannot expand a fresh card:\n{}",
        before_prompt.join("\n")
    );
    assert!(
        before_prompt
            .iter()
            .all(|line| !line.contains("provider-owned pre-prompt title")),
        "pre-prompt presentation text stays off the card:\n{}",
        before_prompt.join("\n")
    );

    cursor.prompt = Some("first real prompt".to_owned());
    let after_prompt = line_texts(&group_lines(&snapshot_with(vec![cursor]), &theme, 0));
    assert!(
        after_prompt
            .iter()
            .any(|line| line.contains("provider-owned pre-prompt title")),
        "durable prompt evidence enables the normal session-name precedence:\n{}",
        after_prompt.join("\n")
    );
}

#[test]
fn render_agent_capability_uses_descriptor_default_window() {
    let mut kimi = agent(
        "kimi-1",
        "kimi",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("resting"),
    );
    kimi.model = Some("kimi-k2.5".to_owned());
    kimi.effort = Some("high".to_owned());
    assert!(kimi.usage.context_window.is_none());
    assert!(kimi.context.is_none());
    let snapshot = snapshot_with(vec![kimi]);

    let rendered = snapshot_to_screen(&snapshot, 44, 15);

    assert!(
        rendered.contains("Kimi K2.5 · 262k"),
        "the identity line falls back to the Kimi definition window:\n{rendered}"
    );
}

#[test]
fn reasoning_configuration_uses_live_then_carried_then_thinking() {
    let render = |live_effort: Option<&str>, carried: Option<&str>, thinking, width| {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Idle,
            Some("/repo/main"),
            Some("main"),
            Some("reasoning config"),
        );
        claude.model = Some("Opus".to_owned());
        claude.effort = carried.map(ToOwned::to_owned);
        let mut context = claude_context(fixed_now());
        context.model_display_name = Some("Opus".to_owned());
        context.effort = live_effort.map(ToOwned::to_owned);
        context.thinking_enabled = thinking;
        context.cost = None;
        context.tokens = None;
        claude.context = Some(context);
        snapshot_to_screen(&snapshot_with(vec![claude]), width, 15)
    };

    let live = render(Some("medium"), Some("low"), Some(true), 54);
    assert!(
        live.contains("Opus 4.8 · medium"),
        "live effort wins:\n{live}"
    );
    assert!(!live.contains(" · low") && !live.contains("thinking"));

    let carried = render(None, Some("high"), Some(true), 54);
    assert!(
        carried.contains("Opus 4.8 · high"),
        "carried effort precedes thinking:\n{carried}"
    );
    assert!(!carried.contains("thinking"));

    let empty_live = render(Some(""), Some("high"), Some(false), 54);
    assert!(
        empty_live.contains("Opus 4.8 · high"),
        "an empty live value does not suppress carried effort:\n{empty_live}"
    );

    let thinking = render(None, None, Some(true), 54);
    assert!(
        thinking.contains("Opus 4.8 · thinking"),
        "thinking is the final fallback:\n{thinking}"
    );

    for disabled in [Some(false), None] {
        let rendered = render(None, None, disabled, 54);
        assert!(rendered.contains("Opus"));
        assert!(!rendered.contains("thinking"), "disabled:\n{rendered}");
    }
}

#[test]
fn reasoning_configuration_keeps_existing_width_degradation() {
    let render = |width| {
        let mut claude = agent(
            "claude-1",
            "claude",
            AgentStatus::Idle,
            Some("/repo/main"),
            Some("main"),
            Some("reasoning config"),
        );
        claude.model = Some("Opus".to_owned());
        let mut context = claude_context(fixed_now());
        context.model_display_name = Some("Opus".to_owned());
        context.effort = None;
        context.thinking_enabled = Some(true);
        context.cost = None;
        context.tokens = None;
        claude.context = Some(context);
        snapshot_to_screen(&snapshot_with(vec![claude]), width, 15)
    };

    let medium = render(40);
    assert!(medium.contains("Opus"), "medium keeps the model:\n{medium}");
    assert!(
        !medium.contains("thinking"),
        "medium drops reasoning configuration:\n{medium}"
    );

    let narrow = render(28);
    assert!(
        !narrow.contains("Opus"),
        "narrow drops capability:\n{narrow}"
    );
    assert!(!narrow.contains("thinking"));
}

#[test]
fn blank_idle_agent_renders_single_line() {
    let idle = agent(
        "idle-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    let snapshot = snapshot_with(vec![idle]);
    let theme = Theme::fixed(true);
    let card_lines = line_texts(&group_lines(&snapshot, &theme, usize::MAX))
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();

    assert_eq!(
        card_lines.len(),
        1,
        "blank idle card renders identity only:\n{}",
        card_lines.join("\n")
    );
    assert!(
        card_lines[0].contains("claude"),
        "identity line carries the agent name:\n{}",
        card_lines.join("\n")
    );
    assert!(
        !card_lines[0].contains("...") && !card_lines[0].contains('—'),
        "blank idle card carries no placeholder description:\n{}",
        card_lines.join("\n")
    );
}

#[test]
fn selected_blank_idle_agent_stays_single_line() {
    let idle = agent(
        "idle-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    let snapshot = snapshot_with(vec![idle]);
    let theme = Theme::fixed(true);

    let selected = line_texts(&group_lines(&snapshot, &theme, 0));
    assert_eq!(selected.len(), 2, "{}", selected.join("\n"));
    assert!(selected[1].contains("claude"), "{selected:?}");
}

#[test]
fn unselected_blank_idle_agent_stays_single_line() {
    let idle = agent(
        "idle-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    let snapshot = snapshot_with(vec![idle]);
    let theme = Theme::fixed(true);

    let card_lines = line_texts(&group_lines(&snapshot, &theme, 99))
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();

    assert_eq!(card_lines.len(), 1, "{card_lines:?}");
    assert!(
        card_lines.iter().all(|line| !line.contains('▢')),
        "unselected blank idle card keeps the thin shape:\n{}",
        card_lines.join("\n")
    );
}

#[test]
fn described_unprompted_idle_agent_keeps_two_lines_at_every_reach() {
    let mut idle = agent(
        "idle-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    idle.description = Some("review the API".to_owned());
    let snapshot = snapshot_with(vec![idle]);
    let theme = Theme::fixed(true);

    for selected_index in [0, usize::MAX] {
        let card_lines = line_texts(&group_lines(&snapshot, &theme, selected_index))
            .into_iter()
            .skip(1)
            .collect::<Vec<_>>();

        assert_eq!(card_lines.len(), 2, "{}", card_lines.join("\n"));
        assert!(card_lines[1].contains("review the API"), "{card_lines:?}");
        assert!(
            !card_lines.iter().any(|line| line.contains('▢')),
            "{card_lines:?}"
        );
        assert!(
            card_lines
                .iter()
                .all(|line| !line.contains('▤') && !line.contains(".  "))
        );
    }
}

#[test]
fn running_agent_without_enrichment_keeps_full_placeholder_shape() {
    let running = agent(
        "running-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("implement the fix"),
    );
    let snapshot = snapshot_with(vec![running]);
    let theme = Theme::fixed(true);

    let card_lines = line_texts(&group_lines(&snapshot, &theme, usize::MAX))
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();

    assert_eq!(card_lines.len(), 3, "{}", card_lines.join("\n"));
    assert!(
        card_lines[1].contains("implement the fix"),
        "{card_lines:?}"
    );
    assert!(
        card_lines[2].contains('▢') && card_lines[2].contains("0%"),
        "{card_lines:?}"
    );
    assert!(
        !card_lines.iter().any(|line| line.contains('▤')),
        "{card_lines:?}"
    );
}

#[test]
fn idle_agent_with_submitted_prompt_keeps_full_placeholder_shape() {
    let mut idle = agent(
        "idle-1",
        "codex",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    idle.prompt = Some("first real prompt".to_owned());
    let snapshot = snapshot_with(vec![idle]);
    let theme = Theme::fixed(true);

    let card_lines = line_texts(&group_lines(&snapshot, &theme, usize::MAX))
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();

    assert_eq!(card_lines.len(), 3, "{}", card_lines.join("\n"));
    assert!(
        card_lines[1].contains("first real prompt"),
        "{card_lines:?}"
    );
    assert!(
        card_lines[2].contains('▢') && card_lines[2].contains("0%"),
        "{card_lines:?}"
    );
    assert!(
        !card_lines.iter().any(|line| line.contains('▤')),
        "{card_lines:?}"
    );
}

#[test]
fn selected_idle_agent_with_history_keeps_existing_shape() {
    let mut idle = agent(
        "idle-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        None,
    );
    idle.usage.context_pct = Some(0);
    idle.usage.total_tokens = Some(1);
    let snapshot = snapshot_with(vec![idle]);
    let theme = Theme::fixed(true);

    let selected = line_texts(&group_lines(&snapshot, &theme, 0));

    assert!(
        selected.iter().all(|line| !line.contains(".  ")),
        "idle cards with history do not get the first-prompt affordance:\n{}",
        selected.join("\n")
    );
    assert!(
        selected.iter().any(|line| line.contains('▢')),
        "idle cards with history keep their existing context bar:\n{}",
        selected.join("\n")
    );
}

#[test]
fn idle_agent_omits_window_token() {
    let mk = |status| {
        let mut claude = agent(
            "claude-1",
            "claude",
            status,
            Some("/repo/main"),
            Some("main"),
            Some("resting"),
        );
        claude.model = Some("Opus".to_owned());
        claude.usage.context_window = Some(200_000);
        claude
    };
    let theme = Theme::fixed(true);

    let idle = snapshot_with(vec![mk(AgentStatus::Idle)]);
    let idle_rendered = line_texts(&group_lines(&idle, &theme, usize::MAX)).join("\n");
    assert!(
        idle_rendered.contains("Opus"),
        "idle agent still renders the model:\n{idle_rendered}"
    );
    assert!(
        !idle_rendered.contains("200k"),
        "idle agent drops the window token:\n{idle_rendered}"
    );

    let running = snapshot_with(vec![mk(AgentStatus::Running)]);
    let running_rendered = line_texts(&group_lines(&running, &theme, usize::MAX)).join("\n");
    assert!(
        running_rendered.contains("Opus · 200k"),
        "non-idle agent keeps the window token:\n{running_rendered}"
    );
}

#[test]
fn capability_cluster_requires_a_resolved_model() {
    // The window is the model's window and effort configures that model, so with
    // no model resolved yet (a Codex session before its app-server context
    // refresh) the whole `· model · effort · window` cluster drops. The card
    // shows just the handle.
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("add tests"),
    );
    assert!(codex.model.is_none());
    codex.effort = Some("xhigh".to_owned());
    codex.usage.context_window = Some(272_000);
    let snapshot = snapshot_with(vec![codex]);

    let rendered = snapshot_to_screen(&snapshot, 44, 15);

    assert!(
        rendered.contains("codex"),
        "the handle still renders:\n{rendered}"
    );
    assert!(
        !rendered.contains("272k"),
        "a model-less window token drops:\n{rendered}"
    );
    assert!(
        !rendered.contains("xhigh"),
        "a model-less effort token drops:\n{rendered}"
    );
}

#[test]
fn render_agent_handle_as_card_identity() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Idle,
        Some("/repo/main"),
        Some("main"),
        Some("plan the change"),
    );
    claude.role = Some("planner".to_owned());
    claude.model = Some("Opus".to_owned());
    let snapshot = snapshot_with(vec![claude]);

    let rendered = snapshot_to_screen(&snapshot, 34, 15);

    assert!(
        rendered.contains("○ planner"),
        "the card renders the team role as the identity text:\n{rendered}"
    );
    assert!(
        !rendered.contains("○ claude"),
        "the provider kind stays off the identity text when a handle exists:\n{rendered}"
    );
    assert_snapshot("agent_handle_identity", rendered);
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
    // display name (`Opus` → `Opus 4.8`) and publishes the live effective
    // effort (`high`) over the launch scalar (`xhigh`).
    claude.model = Some("Opus".to_owned());
    claude.effort = Some("xhigh".to_owned());
    claude.usage.context_pct = Some(38);
    claude.usage.total_tokens = Some(12_400);
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(vec![claude]);
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].diff_added = Some(127);
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].diff_removed = Some(43);
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].commits_ahead = Some(3);
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].commits_behind = Some(1);
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].trunk = Some("main".to_owned());
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].clean = Some(true);

    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &ViewState {
            selected_index: Some(0),
            animation_phase: 0,
            toggles: PresentationToggles {
                help_visible: false,
                ..Default::default()
            },
            ..Default::default()
        },
        &UiState::default(),
        54,
        17,
    );

    // The worktree's git story sits on the group header: the ⇡/⇣ commit
    // delta leads the worktree-total diff. This fixture has no landed verdict,
    // so the cluster stays.
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
    assert!(rendered.contains("high"), "effort:\n{rendered}");
    assert!(
        !rendered.contains("xhigh"),
        "launch effort should not render:\n{rendered}"
    );
    assert!(rendered.contains("· 200k"), "window token:\n{rendered}");
    // Per-row cost now reads at full cent resolution, like every other spend.
    assert!(rendered.contains("$1.27"));
    // Line 2 is the full-width description.
    assert!(rendered.contains("store refactor"));
    // The context bar carries the `▣` label and the percent used as its
    // value (the window size is on the identity line); the
    // fill carries the same reading.
    assert!(rendered.contains("▣ "));
    // The account-scoped 5h/7d budgets are gone from the row — they live in
    // the provider dashboard now.
    assert!(!rendered.contains("5h↻"));
    assert!(!rendered.contains("7d↻"));
    // The selected card carries the context line: ▤ the filled window
    // (input + cache-write + cache-read — the ▣ meter's numerator, so the
    // 38.2% above and this 76k are one measurement), a · seam, then the
    // latest call's composition ordered by how the window filled — ◌
    // cache read, ◍ cache write, ↘ fresh input, ↗ output. The ◇ totals
    // stay the cockpit/store vocabulary; the window size no longer rides
    // this line.
    assert!(
        rendered.contains("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k"),
        "context line:\n{rendered}"
    );
    let context_line = rendered
        .lines()
        .find(|line| line.contains("▤ 76k"))
        .unwrap_or_else(|| panic!("context line rendered:\n{rendered}"));
    assert!(!context_line.contains('◇'), "no fleet total on the card");
    assert!(
        !rendered.contains("ctx"),
        "window size left the token line:\n{rendered}"
    );
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
        model: None,
        ended_turn: false,
        class: TurnErrorClass::Failed,
        at: fixed_now() - Duration::from_secs(10),
        label: Some("API Error: Bad Request".to_owned()),
        refill_at: None,
    });
    claude.context = Some(context);
    let snapshot = snapshot_with(vec![claude]);

    let rendered = snapshot_to_screen(&snapshot, 54, 17);

    assert!(
        rendered.contains("! claude"),
        "the dead turn escalates to the attention glyph:\n{rendered}"
    );
    assert!(
        rendered.contains("API Error: Bad Request"),
        "line 2 quotes the upstream error text:\n{rendered}"
    );
    assert!(
        !rendered.contains("store refactor"),
        "the reason takes the line over the session-name fall-through:\n{rendered}"
    );
    assert_snapshot("api_error_dead_turn_card", rendered);
}
#[test]
fn render_omits_history_sections() {
    let workspace = fixed_workspace();
    let mut snapshot =
        SidebarSnapshot::build_with_carryover(workspace, Vec::new(), vec![], fixed_now());
    snapshot.display_name = "query-engine".to_owned();
    let rendered = snapshot_to_screen(&snapshot, 38, 10);

    assert!(!rendered.contains("all clear"));
    assert!(!rendered.contains("Recent activity"));
    assert!(!rendered.contains("Recently answered"));
}

#[test]
fn unread_result_card_rests_on_a_uniform_unread_wash() {
    let mut done = agent(
        "done-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("shipped"),
    );
    done.last_activity = fixed_now() - Duration::from_secs(60);
    let mut snapshot = snapshot_with(vec![done]);
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .unread = true;
    // Truecolor: the wash is a fine sub-cell tint above the panel.
    let theme = super::super::truecolor_sidebar_theme();
    let wash = theme
        .unread_wash()
        .expect("a finished card washes at truecolor");

    // Nothing selected (index out of range), so this unread result is not the
    // selection: the whole card grounds on its uniform unread wash.
    let lines = group_lines(&snapshot, &theme, 99);
    let name = lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "claude")
        .expect("the unread card name renders");
    assert_eq!(
        name.style.bg,
        Some(wash),
        "the unread result rests on the uniform unread wash, not bare ground"
    );
    assert_ne!(
        Some(wash),
        theme.selection_band(),
        "the wash is its own panel, a lighter tint of the selection blue"
    );

    // The look clears on read: a read result carries no wash.
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .unread = false;
    let read = group_lines(&snapshot, &theme, 99);
    let read_name = read
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == "claude")
        .expect("the read card name renders");
    assert_eq!(
        read_name.style.bg, None,
        "a read result rests on no wash — the cue clears on the look"
    );
}

#[test]
fn card_dims_series_word_in_brand_colour_and_drops_child_words_right_to_left_when_narrow() {
    let mut pi = agent(
        "pi-1",
        "pi",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("run gemini"),
    );
    pi.model = Some("gemini-3.8-flash-lite".to_owned());
    pi.effort = Some("high".to_owned());
    pi.usage.context_window = Some(200_000);
    let mut pi_ctx = AgentContext::new("pi", fixed_now());
    pi_ctx.cost = Some(AgentCost {
        total_cost_usd: Some(1.20),
        ..Default::default()
    });
    pi.context = Some(pi_ctx);
    let mut pi_deepseek = agent(
        "pi-2",
        "pi",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("run deepseek"),
    );
    pi_deepseek.model = Some("deepseek-v4-pro".to_owned());
    let mut agy = agent(
        "agy-1",
        "antigravity",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("run home gemini"),
    );
    agy.model = Some("gemini-4-neon".to_owned());
    let mut snapshot = snapshot_with(vec![pi, pi_deepseek, agy]);
    snapshot
        .rows_mut()
        .next()
        .unwrap()
        .as_agent_mut()
        .unwrap()
        .handle = Some("pi-researcher".into());

    let theme = super::super::truecolor_sidebar_theme();
    let wide = snapshot_to_screen(&snapshot, 64, 24);
    assert!(
        wide.contains("pi-researcher · Gemini 3.8 Flash Lite · high · 200k"),
        "wide Pi card shows the Gemini series word:\n{wide}"
    );
    assert!(
        wide.contains("pi · DeepSeek V4 Pro"),
        "wide Pi card shows the DeepSeek series word:\n{wide}"
    );
    assert!(
        wide.contains("antigravity · Neon 4"),
        "the home provider hides its series where a line name leads:\n{wide}"
    );

    // Span styling on the wide card: `Gemini` wears Antigravity's brand
    // colour dimmed under Pi (Antigravity's own card hides it); `DeepSeek` has no
    // declaring adapter so it wears plain dim (`theme.muted()`).
    let lines = group_lines_at_width(&snapshot, &theme, 99, 64);
    let spans: Vec<_> = lines.iter().flat_map(|line| line.spans.iter()).collect();
    let gemini: Vec<_> = spans
        .iter()
        .filter(|span| span.content.as_ref() == "Gemini")
        .collect();
    assert_eq!(gemini.len(), 1, "one `Gemini` series span on the Pi card");
    for span in gemini {
        assert_eq!(
            span.style.fg,
            Some(theme.provider_brand_tone("antigravity")),
            "Gemini series word uses Antigravity brand colour"
        );
        assert!(
            span.style
                .add_modifier
                .contains(ratatui::style::Modifier::DIM),
            "Gemini series word is dimmed"
        );
    }
    let deepseek = spans
        .iter()
        .find(|span| span.content.as_ref() == "DeepSeek")
        .expect("wide card renders the `DeepSeek` series span");
    assert_eq!(
        deepseek.style,
        theme.muted(),
        "undeclared series uses plain dim"
    );

    // Child words drop right to left at each exact drop width and one column
    // either side: `◐ pi-researcher` (15) + ` · Gemini 3.8 Flash Lite · high ·
    // 200k` (38) + ` $1.20` (6) = 59 content cells (pane width 61); without
    // ` Lite` 54 (pane 56); without ` Flash` 48 (pane 50). Below that the line
    // is cut at the pane edge, never shedding the series word or the core.
    for (width, shown) in [
        (62, "Gemini 3.8 Flash Lite · high · 200k"),
        (61, "Gemini 3.8 Flash Lite · high · 200k"),
        (60, "Gemini 3.8 Flash · high · 200k"),
        (57, "Gemini 3.8 Flash · high · 200k"),
        (56, "Gemini 3.8 Flash · high · 200k"),
        (55, "Gemini 3.8 · high · 200k"),
        (51, "Gemini 3.8 · high · 200k"),
        (50, "Gemini 3.8 · high · 200k"),
        (49, "Gemini 3.8 · high · 20"),
    ] {
        let lines = group_lines_at_width(&snapshot, &theme, usize::MAX, width);
        let text = lines
            .iter()
            .find(|line| line.to_string().contains("pi-researcher"))
            .unwrap()
            .to_string();
        assert!(
            text.contains(&format!("pi-researcher · {shown}"))
                && text.trim_end().ends_with("$1.20"),
            "width {width} shows `{shown}`: {text}"
        );
    }

    // Fast card: child words drop before the name would be ellipsized beside
    // the bolt (`◐ pi-researcher` 15 + ` · Gemini 3.8 Flash Lite ϟ` 26 +
    // ` $1.20` 6 = 47 content cells, pane width 49; without ` Lite` pane 44,
    // without ` Flash` pane 38), and the bolt stays when the core is cut.
    let mut fast_pi = agent(
        "pi-fast",
        "pi",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("run fast gemini"),
    );
    fast_pi.model = Some("gemini-3.8-flash-lite".to_owned());
    let mut fast_ctx = AgentContext::new("pi", fixed_now());
    fast_ctx.cost = Some(AgentCost {
        total_cost_usd: Some(1.20),
        ..Default::default()
    });
    fast_pi.context = Some(fast_ctx);
    let mut fast_snapshot = snapshot_with(vec![fast_pi]);
    {
        let card = fast_snapshot
            .rows_mut()
            .next()
            .unwrap()
            .as_agent_mut()
            .unwrap();
        card.handle = Some("pi-researcher".into());
        card.fast = true;
    }
    let bolt = theme.glyph(GlyphRole::CardFast);
    for (width, shown) in [
        (50, "Gemini 3.8 Flash Lite"),
        (49, "Gemini 3.8 Flash Lite"),
        (48, "Gemini 3.8 Flash"),
        (45, "Gemini 3.8 Flash"),
        (44, "Gemini 3.8 Flash"),
        (43, "Gemini 3.8"),
        (39, "Gemini 3.8"),
        (38, "Gemini 3.8"),
        (37, "Gemini 3…"),
    ] {
        let lines = group_lines_at_width(&fast_snapshot, &theme, usize::MAX, width);
        let text = lines
            .iter()
            .find(|line| line.to_string().contains("pi-researcher"))
            .unwrap()
            .to_string();
        assert!(
            text.contains(&format!("pi-researcher · {shown} {bolt}"))
                && text.trim_end().ends_with("$1.20"),
            "fast card width {width} shows `{shown}` and the bolt: {text}"
        );
    }

    // A fast card whose name after the series is empty (`grok-fast` reads
    // `Grok`) keeps its bolt at the exact width it fits (`◐ pi-grok-researcher`
    // 20 + ` · Grok ϟ` 9 + ` $1.20` 6 = 35 content cells, pane width 37) and
    // drops it one column narrower.
    let mut grok = agent(
        "pi-grok",
        "pi",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("run fast grok"),
    );
    grok.model = Some("grok-fast".to_owned());
    let mut grok_ctx = AgentContext::new("pi", fixed_now());
    grok_ctx.cost = Some(AgentCost {
        total_cost_usd: Some(1.20),
        ..Default::default()
    });
    grok.context = Some(grok_ctx);
    let mut grok_snapshot = snapshot_with(vec![grok]);
    {
        let card = grok_snapshot
            .rows_mut()
            .next()
            .unwrap()
            .as_agent_mut()
            .unwrap();
        card.handle = Some("pi-grok-researcher".into());
        card.fast = true;
    }
    for (width, bolted) in [(38, true), (37, true), (36, false)] {
        let lines = group_lines_at_width(&grok_snapshot, &theme, usize::MAX, width);
        let text = lines
            .iter()
            .find(|line| line.to_string().contains("pi-grok-researcher"))
            .unwrap()
            .to_string();
        assert!(
            text.contains("pi-grok-researcher · Grok")
                && text.contains(bolt) == bolted
                && text.trim_end().ends_with("$1.20"),
            "fast Grok card width {width} shows the bolt: {bolted}: {text}"
        );
    }

    // A versionless name keeps every word after the series a child, so
    // `Pro` drops whole (`◐ pi-researcher` 15 + ` · Gemini Pro` 13 + ` $1.20`
    // 6 = 34 content cells, pane width 36), never cut to `Gemini P`.
    let mut versionless = agent(
        "pi-pro",
        "pi",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("run gemini pro"),
    );
    versionless.model = Some("gemini-pro-latest".to_owned());
    let mut versionless_ctx = AgentContext::new("pi", fixed_now());
    versionless_ctx.cost = Some(AgentCost {
        total_cost_usd: Some(1.20),
        ..Default::default()
    });
    versionless.context = Some(versionless_ctx);
    let mut versionless_snapshot = snapshot_with(vec![versionless]);
    versionless_snapshot
        .rows_mut()
        .next()
        .unwrap()
        .as_agent_mut()
        .unwrap()
        .handle = Some("pi-researcher".into());
    for (width, shown) in [(37, "Gemini Pro"), (36, "Gemini Pro"), (35, "Gemini")] {
        let lines = group_lines_at_width(&versionless_snapshot, &theme, usize::MAX, width);
        let text = lines
            .iter()
            .find(|line| line.to_string().contains("pi-researcher"))
            .unwrap()
            .to_string();
        let after = text.split_once("pi-researcher · ").map(|(_, rest)| rest);
        assert!(
            after.is_some_and(|rest| rest
                .trim_end()
                .strip_suffix("$1.20")
                .is_some_and(|name| name.trim_end() == shown)),
            "versionless card width {width} shows `{shown}`: {text}"
        );
    }

    // Compaction point + capacity: ` Flash` drops before `835k/1M` would
    // lose `/1M` (`⣾ opencode-lead` 15 + ` · Gemini 4 Flash · 835k/1M` 27 +
    // ` $1.20` 6 = 48 content cells, pane width 50).
    let mut compact_opencode = agent(
        "opencode-compact",
        "opencode",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("run compact gemini"),
    );
    compact_opencode.model = Some("gemini-4-flash".to_owned());
    compact_opencode.usage.context_window = Some(983_010);
    let mut compact_ctx = AgentContext::new("opencode", fixed_now());
    compact_ctx.cost = Some(AgentCost {
        total_cost_usd: Some(1.20),
        ..Default::default()
    });
    compact_opencode.context = Some(compact_ctx);
    let mut compact_snapshot = snapshot_with(vec![compact_opencode]);
    {
        let card = compact_snapshot
            .rows_mut()
            .next()
            .unwrap()
            .as_agent_mut()
            .unwrap();
        card.handle = Some("opencode-lead".into());
        card.usage.context_window = Some(983_010);
        card.usage.context_capacity = Some(1_048_576);
    }
    for (width, shown) in [
        (51, "Gemini 4 Flash · 835k/1M"),
        (50, "Gemini 4 Flash · 835k/1M"),
        (49, "Gemini 4 · 835k/1M"),
    ] {
        let lines = group_lines_at_width(&compact_snapshot, &theme, usize::MAX, width);
        let text = lines
            .iter()
            .find(|line| line.to_string().contains("opencode-lead"))
            .unwrap()
            .to_string();
        assert!(
            text.contains(&format!("opencode-lead · {shown}"))
                && text.trim_end().ends_with("$1.20"),
            "compaction card width {width} shows `{shown}`: {text}"
        );
    }
}
