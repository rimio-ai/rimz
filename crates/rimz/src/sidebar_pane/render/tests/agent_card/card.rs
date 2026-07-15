use super::*;

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
    // The hook-derived window renders as the identity line's `1m` token.
    claude.context_window = Some(1_000_000);
    claude.last_activity = fixed_now() - Duration::from_secs(4 * 60);
    let snapshot = snapshot_with(vec![claude]);

    let rendered = snapshot_to_screen(&snapshot, 34, 15);

    assert!(
        rendered.contains("! claude"),
        "a failed agent leads with the attention glyph:\n{rendered}"
    );
    assert!(
        rendered.contains("Opus · 1m"),
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
        rendered.contains("GPT 5.6 Sol · medium · 200k"),
        "the identity line separates Cursor's normalized capabilities:\n{rendered}"
    );
    assert!(
        !rendered.contains("272K") && !rendered.contains("272k"),
        "the nominal selector window does not leak into the card:\n{rendered}"
    );
}

#[test]
fn cursor_idle_session_name_yields_to_first_prompt_affordance() {
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
    assert!(
        before_prompt.iter().any(|line| line.contains(".  ")),
        "a provider-owned session title cannot displace the compose affordance:\n{}",
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
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("resting"),
    );
    codex.model = Some("gpt-5.5-codex".to_owned());
    codex.effort = Some("xhigh".to_owned());
    assert!(codex.context_window.is_none());
    assert!(codex.context.is_none());
    let snapshot = snapshot_with(vec![codex]);

    let rendered = snapshot_to_screen(&snapshot, 44, 15);

    assert!(
        rendered.contains("GPT 5.5 Codex · 272k"),
        "the identity line falls back to the Codex descriptor window:\n{rendered}"
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
    assert!(live.contains("Opus · medium"), "live effort wins:\n{live}");
    assert!(!live.contains(" · low") && !live.contains("thinking"));

    let carried = render(None, Some("high"), Some(true), 54);
    assert!(
        carried.contains("Opus · high"),
        "carried effort precedes thinking:\n{carried}"
    );
    assert!(!carried.contains("thinking"));

    let empty_live = render(Some(""), Some("high"), Some(false), 54);
    assert!(
        empty_live.contains("Opus · high"),
        "an empty live value does not suppress carried effort:\n{empty_live}"
    );

    let thinking = render(None, None, Some(true), 54);
    assert!(
        thinking.contains("Opus · thinking"),
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
fn selected_blank_idle_agent_opens_compose_affordance() {
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

    assert!(
        selected.iter().any(|line| line.contains(".  ")),
        "phase-0 compose placeholder renders on the selected blank idle card:\n{}",
        selected.join("\n")
    );
    assert!(
        selected
            .iter()
            .any(|line| line.contains('▢') && line.contains("0%")),
        "selected blank idle card renders the empty context bar:\n{}",
        selected.join("\n")
    );
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
fn described_unprompted_idle_agent_stays_fresh_and_two_lines() {
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
            card_lines
                .iter()
                .all(|line| !line.contains('▢') && !line.contains('▤') && !line.contains(".  ")),
            "{card_lines:?}"
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

    assert_eq!(card_lines.len(), 4, "{}", card_lines.join("\n"));
    assert!(
        card_lines[1].contains("implement the fix"),
        "{card_lines:?}"
    );
    assert!(
        card_lines[2].contains('▢') && card_lines[2].contains("0%"),
        "{card_lines:?}"
    );
    assert!(card_lines[3].contains("▤ 0"), "{card_lines:?}");
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

    assert_eq!(card_lines.len(), 4, "{}", card_lines.join("\n"));
    assert!(
        card_lines[1].contains("first real prompt"),
        "{card_lines:?}"
    );
    assert!(
        card_lines[2].contains('▢') && card_lines[2].contains("0%"),
        "{card_lines:?}"
    );
    assert!(card_lines[3].contains("▤ 0"), "{card_lines:?}");
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
    idle.context_pct = Some(0);
    idle.total_tokens = Some(1);
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
        claude.context_window = Some(200_000);
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
    codex.context_window = Some(272_000);
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
    claude.context_pct = Some(38);
    claude.total_tokens = Some(12_400);
    claude.context = Some(claude_context(fixed_now()));
    let mut snapshot = snapshot_with(vec![claude]);
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
            ..Default::default()
        },
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
        class: TurnErrorClass::Failed,
        at: fixed_now() - Duration::from_secs(10),
        label: Some("API Error: Bad Request".to_owned()),
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
    snapshot.worktree_groups[0].rows[0].unread = true;
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
    snapshot.worktree_groups[0].rows[0].unread = false;
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
