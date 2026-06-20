use super::*;

pub(super) fn sidebar_fixture_snapshot(
    state: SidebarFixtureState,
) -> Result<rimz::SidebarSnapshot> {
    let now = fixture_now()?;
    let workspace_id = "ws_0123456789abcdef01234567".parse::<WorkspaceId>()?;
    let mut snapshot = rimz::SidebarSnapshot {
        workspace_id,
        display_name: "query-engine".to_owned(),
        generated_at: now,
        panes_produced_at_ms: Some(1_781_009_600_000),
        panes_observed_at_ms: None,
        focus_contested_panes: Vec::new(),
        truth_degraded: None,
        now,
        worktree_groups: Vec::new(),
        needs_attention: Vec::new(),
        resolver_working: Vec::new(),
        agents: Vec::new(),
        wired_lazy_kinds: vec!["codex".to_owned()],
        lazy_agent_default_models: std::collections::BTreeMap::new(),
        agent_panes: Vec::new(),
        own_view: None,
        only_daemon_view_remains: false,
        project_root: Some(PathBuf::from("/srv/code/query-engine")),
        worktree_roots: vec![PathBuf::from("/srv/code/query-engine")],
        worktree_home: None,
        root_class: rimz::workspace::RootClass::Repo,
        sidebar: rimz::config::SidebarConfig::default(),
        theme: rimz::config::ThemeConfig::default(),
        pets: rimz::config::PetsConfig::default(),
        attention: rimz::config::AttentionConfig::default(),
        providers: Vec::new(),
        value_tally: None,
        workspace_value_tally: None,
        today_spend_live_usd: None,
        link: None,
        reflects_log: None,
    };
    snapshot.theme.scheme = Some("TokyoNight Night".to_owned());

    match state {
        SidebarFixtureState::Empty => {}
        SidebarFixtureState::Fleet => add_fleet_fixture(&mut snapshot, now),
        SidebarFixtureState::Provider => {
            add_fleet_fixture(&mut snapshot, now);
            add_provider_fixture(&mut snapshot, now);
        }
    }
    Ok(snapshot)
}

fn fixture_now() -> Result<jiff::Timestamp> {
    "2026-06-09T12:00:00Z"
        .parse()
        .context("parsing sidebar fixture timestamp")
}

fn add_fleet_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    let claude = agent_row(
        "agent:claude:auth",
        "claude",
        "terminal_21",
        "/srv/code/query-engine",
        "feature/auth-router",
        rimz::feed::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "port auth watcher",
        "Opus 4.1",
        Some((67, 200_000, 128_400)),
        now,
    );
    let codex = agent_row(
        "agent:codex:pricing",
        "codex",
        "terminal_22",
        "/srv/code/query-engine/.rimz/worktrees/pricing",
        "pricing-refresh",
        rimz::feed::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve pricing cache write",
        "GPT-5.1-Codex",
        Some((41, 272_000, 96_200)),
        now,
    );
    let pi = agent_row(
        "agent:pi:mux",
        "pi",
        "terminal_23",
        "/srv/code/query-engine/.rimz/worktrees/mux",
        "zellij-health",
        rimz::feed::AgentStatus::Failed,
        rimz::agents::TurnPhase::Idle,
        "debug zellij health probe",
        "Pi",
        None,
        now,
    );
    let process = rimz::SidebarRow {
        id: "process:cargo-nextest".to_owned(),
        name: "cargo nextest".to_owned(),
        pane: Some(pane_ref(
            "terminal_24",
            "cargo nextest run",
            "/srv/code/query-engine",
            false,
        )),
        worktree_path: Some("/srv/code/query-engine".to_owned()),
        worktree_branch: Some("main".to_owned()),
        unread: false,
        inactive: false,
        last_activity: now,
        card: rimz::RowCard::Process(rimz::ProcessCard {
            state: rimz::ProcessState::Busy,
            command_detail: Some("integration::backend::zellij".to_owned()),
            cpu_pct: Some(37),
            rss_kb: Some(412_000),
            ..rimz::ProcessCard::default()
        }),
    };

    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![
                status_count(rimz::feed::AgentStatus::Running, 1),
                status_count(rimz::feed::AgentStatus::Waiting, 1),
            ],
            rows: vec![claude, codex, process],
            hidden_count: 0,
            diff_added: Some(182),
            diff_removed: Some(47),
            commits_ahead: Some(3),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/mux".to_owned(),
            label: "zellij-health".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![status_count(rimz::feed::AgentStatus::Failed, 1)],
            rows: vec![pi],
            hidden_count: 0,
            diff_added: Some(14),
            diff_removed: Some(3),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
        },
    ];
    snapshot.value_tally = Some(spend_tally(9.42, 712_000, 8));
    snapshot.workspace_value_tally = Some(spend_tally(6.84, 481_000, 5));
    snapshot.today_spend_live_usd = Some(10.08);
}

#[allow(clippy::too_many_arguments)]
fn agent_row(
    id: &str,
    name: &str,
    pane_raw: &str,
    cwd: &str,
    branch: &str,
    status: rimz::feed::AgentStatus,
    phase: rimz::agents::TurnPhase,
    task: &str,
    model: &str,
    context: Option<(u8, u64, u64)>,
    now: jiff::Timestamp,
) -> rimz::SidebarRow {
    let (context_pct, context_window, total_tokens) = context
        .map_or((None, None, None), |(pct, window, total)| {
            (Some(pct), Some(window), Some(total))
        });
    let mut card = rimz::AgentCard {
        status: Some(status),
        phase,
        surface: Some(rimz::Surface::NativeUi),
        task: Some(task.to_owned()),
        model: Some(model.to_owned()),
        context_pct,
        context_window,
        total_tokens,
        cache_read_input_tokens: Some(total_tokens.unwrap_or_default() / 3),
        fresh_input_tokens: Some(total_tokens.unwrap_or_default() / 5),
        output_tokens: Some(total_tokens.unwrap_or_default() / 8),
        todo_done: Some(4),
        todo_total: Some(7),
        context_severity: context_pct.map(|pct| {
            rimz::feed::ContextSeverity::classify(
                pct,
                total_tokens,
                &rimz::config::ContextMeterConfig::default(),
            )
        }),
        registered_at: Some(now),
        ..rimz::AgentCard::default()
    };
    if status == rimz::feed::AgentStatus::Failed {
        card.turn_error_label = Some("API error".to_owned());
        card.todo_done = Some(2);
        card.todo_total = Some(5);
    }
    if status == rimz::feed::AgentStatus::Running {
        card.sub_agents = vec![
            rimz::SidebarSubAgent {
                id: "child:review".to_owned(),
                name: "review".to_owned(),
                status: rimz::feed::AgentStatus::Running,
                phase: rimz::agents::TurnPhase::Reasoning,
                task: Some("check unsafe edges".to_owned()),
                model: Some("Haiku".to_owned()),
                effort: None,
                description: Some("audit auth watcher changes".to_owned()),
                total_tokens: Some(22_400),
                elapsed_secs: Some(180),
                started_at: Some(now),
                last_activity: now,
                registered_at: Some(now),
            },
            rimz::SidebarSubAgent {
                id: "child:test".to_owned(),
                name: "test".to_owned(),
                status: rimz::feed::AgentStatus::Success,
                phase: rimz::agents::TurnPhase::Idle,
                task: Some("run focused nextest".to_owned()),
                model: Some("Haiku".to_owned()),
                effort: None,
                description: None,
                total_tokens: Some(18_900),
                elapsed_secs: Some(260),
                started_at: Some(now),
                last_activity: now,
                registered_at: Some(now),
            },
        ];
    }

    rimz::SidebarRow {
        id: id.to_owned(),
        name: name.to_owned(),
        pane: Some(pane_ref(pane_raw, name, cwd, status.is_attention())),
        worktree_path: Some(cwd.to_owned()),
        worktree_branch: Some(branch.to_owned()),
        unread: false,
        inactive: false,
        last_activity: now,
        card: rimz::RowCard::Agent(Box::new(card)),
    }
}

fn pane_ref(raw: &str, command: &str, cwd: &str, focused: bool) -> rimz::feed::PaneRef {
    rimz::feed::PaneRef {
        pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, raw),
        session_name: "rimz-fixture".to_owned(),
        view_id: Some("tab_0".to_owned()),
        view_kind: Some(rimz::ViewKind::Tab),
        view_name: Some("main".to_owned()),
        is_focused: focused,
        is_floating: false,
        command: Some(command.to_owned()),
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn status_count(status: rimz::feed::AgentStatus, count: usize) -> rimz::SidebarStatusCount {
    rimz::SidebarStatusCount { status, count }
}

fn add_provider_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    snapshot.providers = vec![
        provider_panel(
            "claude",
            "Claude",
            173,
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            Some((25, 40)),
            spend_tally(6.84, 498_000, 4),
            now,
        ),
        provider_panel(
            "codex",
            "Codex",
            33,
            Some("0.135.0"),
            Some("ChatGPT Pro"),
            false,
            false,
            None,
            spend_tally(3.24, 214_000, 5),
            now,
        ),
    ];
}

#[allow(clippy::too_many_arguments)]
fn provider_panel(
    kind: &str,
    product_name: &str,
    color: u8,
    version: Option<&str>,
    plan: Option<&str>,
    metered: bool,
    remote_control: bool,
    windows: Option<(u8, u8)>,
    spending: rimz::SpendTally,
    now: jiff::Timestamp,
) -> rimz::SidebarProviderPanel {
    let window = |used: u8, mins: u32, resets_in_secs: u64| rimz::agents::RateLimitWindow {
        used_percentage: Some(used),
        resets_at: Some(now + std::time::Duration::from_secs(resets_in_secs)),
        duration_mins: Some(mins),
        ..Default::default()
    };
    let windows = windows
        .map(|(short, long)| {
            vec![
                window(short, 300, 2 * 60 * 60),
                window(long, 7 * 24 * 60, 2 * 24 * 60 * 60),
            ]
        })
        .unwrap_or_default();
    rimz::SidebarProviderPanel {
        kind: kind.to_owned(),
        product_name: product_name.to_owned(),
        art: vec![
            " ▐▛███▜▌".to_owned(),
            "▝▜█████▛▘".to_owned(),
            "  ▘▘ ▝▝".to_owned(),
        ],
        color,
        color_rgb: Some(match kind {
            "claude" => (0xd9, 0x77, 0x57),
            "codex" => (0x2f, 0xb1, 0xd1),
            "pi" => (0x27, 0xa0, 0x77),
            _ => (color, color, color),
        }),
        color_role: None,
        version: version.map(ToOwned::to_owned),
        plan: plan.map(ToOwned::to_owned),
        metered,
        remote_control,
        spending: Some(spending),
        extra_credits: None,
        windows,
    }
}

fn spend_tally(usd: f64, tokens: u64, sessions: u32) -> rimz::SpendTally {
    let window = |scale: f64| {
        let tokens = (tokens as f64 * scale) as u64;
        rimz::SpendWindow {
            usd: usd * scale,
            tokens,
            input: tokens * 7 / 10,
            output: tokens * 2 / 10,
            cache_write: tokens / 20,
            cache_read: tokens / 10,
            sessions,
        }
    };
    rimz::SpendTally {
        today: window(1.0),
        week: window(2.8),
        month: window(7.4),
        year: window(19.0),
    }
}
