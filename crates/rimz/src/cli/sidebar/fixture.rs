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
        viewed_panes: Vec::new(),
        presence: None,
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
        SidebarFixtureState::Cockpit => add_cockpit_fixture(&mut snapshot, now),
        SidebarFixtureState::Focus => add_focus_fixture(&mut snapshot, now),
        SidebarFixtureState::Economy => add_economy_fixture(&mut snapshot, now),
        SidebarFixtureState::Reach => add_reach_fixture(&mut snapshot, now),
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
        rimz::agents::AgentStatus::Running,
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
        rimz::agents::AgentStatus::Waiting,
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
        rimz::agents::AgentStatus::Failed,
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
                status_count(rimz::agents::AgentStatus::Running, 1),
                status_count(rimz::agents::AgentStatus::Waiting, 1),
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
            trunk_sync: None,
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/mux".to_owned(),
            label: "zellij-health".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![status_count(rimz::agents::AgentStatus::Failed, 1)],
            rows: vec![pi],
            hidden_count: 0,
            diff_added: Some(14),
            diff_removed: Some(3),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
        },
    ];
    snapshot.value_tally = Some(spend_tally(9.42, 712_000, 8));
    snapshot.workspace_value_tally = Some(spend_tally(6.84, 481_000, 5));
    snapshot.today_spend_live_usd = Some(10.08);
}

fn add_cockpit_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.theme.style = Some(rimz::config::ThemeStyle::Modern);
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Never;
    let claude = agent_row_with(
        "agent:claude:main",
        "claude",
        "terminal_31",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "stabilize render diff",
        "Opus 4.1",
        Some((58, 200_000, 118_400)),
        now,
        AgentRowOptions {
            age_secs: Some(45),
            ..AgentRowOptions::default()
        },
    );
    let opencode = agent_row_with(
        "agent:opencode:main",
        "opencode",
        "terminal_32",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Reasoning,
        "trace tmux layout parity",
        "OpenCode",
        Some((32, 128_000, 44_200)),
        now,
        AgentRowOptions {
            age_secs: Some(4 * 60),
            sub_agents: Some(Vec::new()),
            ..AgentRowOptions::default()
        },
    );
    let compacting = agent_row_with(
        "agent:claude:compacting",
        "claude",
        "terminal_38",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Reasoning,
        "compact provider trace",
        "Opus 4.1",
        Some((91, 200_000, 182_300)),
        now,
        AgentRowOptions {
            age_secs: Some(8 * 60),
            compacting: true,
            compaction_count: 3,
            sub_agents: Some(Vec::new()),
            ..AgentRowOptions::default()
        },
    );
    let idle = agent_row_with(
        "agent:pi:idle",
        "pi",
        "terminal_39",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "park sidebar notes",
        "Pi",
        None,
        now,
        AgentRowOptions {
            age_secs: Some(65 * 60),
            ..AgentRowOptions::default()
        },
    );
    let process = rimz::SidebarRow {
        id: "process:cargo-nextest".to_owned(),
        name: "cargo nextest".to_owned(),
        pane: Some(pane_ref(
            "terminal_33",
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
            command_detail: Some("integration::backend".to_owned()),
            cpu_pct: Some(37),
            rss_kb: Some(412_000),
            ..rimz::ProcessCard::default()
        }),
    };
    let mut codex = agent_row_with(
        "agent:codex:pricing",
        "codex",
        "terminal_34",
        "/srv/code/query-engine/.rimz/worktrees/pricing-refresh",
        "pricing-refresh",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve pricing cache write",
        "GPT-5.1-Codex",
        Some((41, 272_000, 96_200)),
        now,
        AgentRowOptions {
            age_secs: Some(18 * 60),
            ..AgentRowOptions::default()
        },
    );
    codex.unread = true;
    let pi = agent_row_with(
        "agent:pi:zellij",
        "pi",
        "terminal_35",
        "/srv/code/query-engine/.rimz/worktrees/zellij-health",
        "zellij-health",
        rimz::agents::AgentStatus::Failed,
        rimz::agents::TurnPhase::Idle,
        "debug zellij health probe",
        "Pi",
        None,
        now,
        AgentRowOptions {
            age_secs: Some(42 * 60),
            ..AgentRowOptions::default()
        },
    );
    let mut success = agent_row_with(
        "agent:claude:mux-merge",
        "claude",
        "terminal_36",
        "/srv/code/query-engine/.rimz/worktrees/mux-merge",
        "mux-merge",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "land tmux hook fix",
        "Sonnet 4.6",
        Some((18, 200_000, 28_000)),
        now,
        AgentRowOptions {
            age_secs: Some(2 * 60 * 60),
            ..AgentRowOptions::default()
        },
    );
    success.unread = true;
    let paused = agent_row_with(
        "agent:codex:mux-merge",
        "codex",
        "terminal_37",
        "/srv/code/query-engine/.rimz/worktrees/mux-merge",
        "mux-merge",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "wait for provider window",
        "GPT-5.1-Codex",
        Some((77, 272_000, 205_100)),
        now,
        AgentRowOptions {
            age_secs: Some(75 * 60),
            ..AgentRowOptions::default()
        },
    );
    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![
                status_count(rimz::agents::AgentStatus::Running, 3),
                status_count(rimz::agents::AgentStatus::Idle, 1),
            ],
            rows: vec![claude, opencode, compacting, idle, process],
            hidden_count: 0,
            diff_added: Some(182),
            diff_removed: Some(47),
            commits_ahead: Some(3),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/pricing-refresh".to_owned(),
            label: "pricing-refresh".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![status_count(rimz::agents::AgentStatus::Waiting, 1)],
            rows: vec![codex],
            hidden_count: 0,
            diff_added: Some(24),
            diff_removed: Some(8),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/zellij-health".to_owned(),
            label: "zellij-health".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![status_count(rimz::agents::AgentStatus::Failed, 1)],
            rows: vec![pi],
            hidden_count: 0,
            diff_added: Some(14),
            diff_removed: Some(3),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/mux-merge".to_owned(),
            label: "mux-merge".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![
                status_count(rimz::agents::AgentStatus::Success, 1),
                status_count(rimz::agents::AgentStatus::Paused, 1),
            ],
            rows: vec![success, paused],
            hidden_count: 0,
            diff_added: Some(0),
            diff_removed: Some(0),
            commits_ahead: Some(0),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(true),
            landed: Some(true),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Merged),
            pr_state: Some(rimz::WorktreePrState::Merged),
        },
    ];
    snapshot.providers = vec![
        provider_panel(
            "claude",
            "Claude",
            173,
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            Some((44, 68)),
            spend_tally(612.0, 42_600_000, 28),
            now,
        ),
        provider_panel(
            "codex",
            "Codex",
            33,
            Some("0.135.0"),
            Some("ChatGPT Pro"),
            true,
            false,
            Some((58, 73)),
            spend_tally(428.0, 31_200_000, 24),
            now,
        ),
    ];
    snapshot.value_tally = Some(spend_tally(1_425.0, 91_100_000, 62));
    snapshot.workspace_value_tally = Some(spend_tally(1_168.0, 68_100_000, 43));
    snapshot.today_spend_live_usd = Some(392.0);
}

fn add_focus_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.theme.style = Some(rimz::config::ThemeStyle::Modern);
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    let lead = agent_row_with(
        "agent:claude:auth-router",
        "claude",
        "terminal_41",
        "/srv/code/query-engine/.rimz/worktrees/auth-router",
        "feature/auth-router",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "wire auth router migration",
        "Opus 4.1",
        Some((72, 200_000, 151_900)),
        now,
        AgentRowOptions {
            handle: Some("coder".to_owned()),
            sub_agents: Some(vec![
                sub_agent(
                    "child:explore:routes",
                    "Explore",
                    rimz::agents::AgentStatus::Running,
                    rimz::agents::TurnPhase::Reasoning,
                    Some("map route ownership"),
                    Some("Haiku"),
                    Some("trace handler graph"),
                    Some(20_100),
                    Some(120),
                    now,
                ),
                sub_agent(
                    "child:explore:middleware",
                    "Explore",
                    rimz::agents::AgentStatus::Running,
                    rimz::agents::TurnPhase::Acting,
                    Some("prove middleware order"),
                    Some("Haiku"),
                    Some("exercise auth edge cases"),
                    Some(18_700),
                    Some(160),
                    now,
                ),
                sub_agent(
                    "child:explore:docs",
                    "Explore",
                    rimz::agents::AgentStatus::Success,
                    rimz::agents::TurnPhase::Idle,
                    Some("summarize docs drift"),
                    Some("Haiku"),
                    None,
                    Some(11_400),
                    Some(240),
                    now,
                ),
                sub_agent(
                    "child:general:test",
                    "general-purpose",
                    rimz::agents::AgentStatus::Running,
                    rimz::agents::TurnPhase::Acting,
                    Some("run focused nextest"),
                    Some("Sonnet 4.6"),
                    None,
                    Some(23_500),
                    Some(210),
                    now,
                ),
                sub_agent(
                    "child:plan:rollout",
                    "Plan",
                    rimz::agents::AgentStatus::Success,
                    rimz::agents::TurnPhase::Idle,
                    Some("sequence rollout guard"),
                    Some("Haiku"),
                    Some("write migration runbook"),
                    Some(9_900),
                    Some(360),
                    now,
                ),
                sub_agent(
                    "child:general:review",
                    "general-purpose",
                    rimz::agents::AgentStatus::Waiting,
                    rimz::agents::TurnPhase::Idle,
                    Some("review migration contract"),
                    Some("Sonnet 4.6"),
                    None,
                    Some(14_800),
                    Some(260),
                    now,
                ),
            ]),
            age_secs: Some(70),
            compaction_count: 2,
            ..AgentRowOptions::default()
        },
    );
    let planner = agent_row_with(
        "agent:claude:planner",
        "claude",
        "terminal_42",
        "/srv/code/query-engine/.rimz/worktrees/auth-router",
        "feature/auth-router",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "plan router cutover",
        "Opus 4.1",
        Some((22, 200_000, 36_400)),
        now,
        AgentRowOptions {
            handle: Some("planner".to_owned()),
            age_secs: Some(22 * 60),
            ..AgentRowOptions::default()
        },
    );
    let reviewer = agent_row_with(
        "agent:codex:reviewer",
        "codex",
        "terminal_43",
        "/srv/code/query-engine/.rimz/worktrees/auth-router",
        "feature/auth-router",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "review auth router diff",
        "GPT-5.1-Codex",
        Some((45, 272_000, 86_700)),
        now,
        AgentRowOptions {
            handle: Some("reviewer".to_owned()),
            age_secs: Some(9 * 60),
            ..AgentRowOptions::default()
        },
    );
    let docs = agent_row_with(
        "agent:pi:auth-docs",
        "pi",
        "terminal_44",
        "/srv/code/query-engine/.rimz/worktrees/auth-docs",
        "auth-docs",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "summarize OAuth drift",
        "Pi",
        None,
        now,
        AgentRowOptions {
            age_secs: Some(37 * 60),
            ..AgentRowOptions::default()
        },
    );
    let lint = agent_row_with(
        "agent:codex:auth-docs",
        "codex",
        "terminal_45",
        "/srv/code/query-engine/.rimz/worktrees/auth-docs",
        "auth-docs",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "wait for reviewer notes",
        "GPT-5.1-Codex",
        Some((24, 272_000, 64_200)),
        now,
        AgentRowOptions {
            age_secs: Some(58 * 60),
            ..AgentRowOptions::default()
        },
    );
    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/auth-router".to_owned(),
            label: "feature/auth-router".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![
                status_count(rimz::agents::AgentStatus::Running, 1),
                status_count(rimz::agents::AgentStatus::Success, 1),
                status_count(rimz::agents::AgentStatus::Waiting, 1),
            ],
            rows: vec![lead, planner, reviewer],
            hidden_count: 0,
            diff_added: Some(96),
            diff_removed: Some(31),
            commits_ahead: Some(2),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/auth-docs".to_owned(),
            label: "auth-docs".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![
                status_count(rimz::agents::AgentStatus::Success, 1),
                status_count(rimz::agents::AgentStatus::Idle, 1),
            ],
            rows: vec![docs, lint],
            hidden_count: 0,
            diff_added: Some(28),
            diff_removed: Some(12),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
        },
    ];
    snapshot.providers = vec![
        provider_panel(
            "pi",
            "Pi",
            35,
            Some("0.11.4"),
            Some("Pi Pro"),
            true,
            false,
            Some((21, 34)),
            spend_tally(318.0, 18_400_000, 18),
            now,
        ),
        provider_panel(
            "claude",
            "Claude",
            173,
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            Some((39, 52)),
            spend_tally(506.0, 38_200_000, 25),
            now,
        ),
        provider_panel(
            "codex",
            "Codex",
            33,
            Some("0.135.0"),
            Some("ChatGPT Pro"),
            true,
            false,
            Some((47, 61)),
            spend_tally(401.0, 26_600_000, 21),
            now,
        ),
    ];
    snapshot.value_tally = Some(spend_tally(1_125.0, 76_200_000, 48));
    snapshot.workspace_value_tally = Some(spend_tally(1_125.0, 76_200_000, 48));
    snapshot.today_spend_live_usd = Some(268.0);
}

fn add_economy_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.theme.style = Some(rimz::config::ThemeStyle::Modern);
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    snapshot.theme.pets = rimz::config::PetsConfig {
        enabled: true,
        pet: "seedy".to_owned(),
        size: rimz::config::PetsSize::Medium,
        ..rimz::config::PetsConfig::default()
    };
    let opencode = agent_row_with(
        "agent:opencode:economy",
        "opencode",
        "terminal_51",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "rebalance metered sessions",
        "OpenCode",
        Some((46, 128_000, 58_400)),
        now,
        AgentRowOptions {
            age_secs: Some(55),
            ..AgentRowOptions::default()
        },
    );
    let claude = agent_row_with(
        "agent:claude:economy",
        "claude",
        "terminal_52",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Reasoning,
        "audit provider spend",
        "Sonnet 4.6",
        Some((38, 200_000, 75_400)),
        now,
        AgentRowOptions {
            age_secs: Some(6 * 60),
            ..AgentRowOptions::default()
        },
    );
    let codex = agent_row_with(
        "agent:codex:economy",
        "codex",
        "terminal_53",
        "/srv/code/query-engine/.rimz/worktrees/pricing-refresh",
        "pricing-refresh",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve price snapshot",
        "GPT-5.1-Codex",
        Some((49, 272_000, 101_200)),
        now,
        AgentRowOptions {
            age_secs: Some(14 * 60),
            ..AgentRowOptions::default()
        },
    );
    let pi = agent_row_with(
        "agent:pi:budget",
        "pi",
        "terminal_54",
        "/srv/code/query-engine/.rimz/worktrees/cost-caps",
        "cost-caps",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "publish budget notes",
        "Pi",
        None,
        now,
        AgentRowOptions {
            age_secs: Some(31 * 60),
            ..AgentRowOptions::default()
        },
    );
    let codex_idle = agent_row_with(
        "agent:codex:budget",
        "codex",
        "terminal_55",
        "/srv/code/query-engine/.rimz/worktrees/cost-caps",
        "cost-caps",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "hold weekly spend cap",
        "GPT-5.1-Codex",
        Some((19, 272_000, 45_200)),
        now,
        AgentRowOptions {
            age_secs: Some(83 * 60),
            ..AgentRowOptions::default()
        },
    );
    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine".to_owned(),
            label: "provider-ledger".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![status_count(rimz::agents::AgentStatus::Running, 2)],
            rows: vec![opencode, claude],
            hidden_count: 0,
            diff_added: Some(42),
            diff_removed: Some(11),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/pricing-refresh".to_owned(),
            label: "pricing-refresh".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![status_count(rimz::agents::AgentStatus::Waiting, 1)],
            rows: vec![codex],
            hidden_count: 0,
            diff_added: Some(19),
            diff_removed: Some(6),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/cost-caps".to_owned(),
            label: "cost-caps".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![
                status_count(rimz::agents::AgentStatus::Success, 1),
                status_count(rimz::agents::AgentStatus::Idle, 1),
            ],
            rows: vec![pi, codex_idle],
            hidden_count: 0,
            diff_added: Some(12),
            diff_removed: Some(4),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
        },
    ];
    let mut codex_panel = provider_panel(
        "codex",
        "Codex",
        33,
        Some("0.135.0"),
        Some("ChatGPT Pro"),
        true,
        false,
        Some((63, 71)),
        spend_tally(447.0, 32_200_000, 26),
        now,
    );
    codex_panel.extra_credits = Some(rimz::ExtraCredits::known(
        Some(12.0),
        Some(38.0),
        Some(50.0),
    ));
    snapshot.providers = vec![
        provider_panel(
            "opencode",
            "OpenCode",
            141,
            Some("0.7.2"),
            Some("Team"),
            true,
            false,
            Some((34, 48)),
            spend_tally(516.0, 28_400_000, 29),
            now,
        ),
        provider_panel(
            "claude",
            "Claude",
            173,
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            Some((25, 40)),
            spend_tally(682.0, 44_600_000, 31),
            now,
        ),
        codex_panel,
        provider_panel(
            "pi",
            "Pi",
            35,
            Some("0.11.4"),
            Some("Pi Pro"),
            true,
            false,
            Some((18, 29)),
            spend_tally(219.0, 13_200_000, 12),
            now,
        ),
    ];
    snapshot.value_tally = Some(spend_tally(1_780.0, 118_000_000, 72));
    snapshot.workspace_value_tally = Some(spend_tally(1_416.0, 88_400_000, 51));
    snapshot.today_spend_live_usd = Some(486.0);
}

fn add_reach_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    snapshot.theme.pets = rimz::config::PetsConfig {
        enabled: true,
        pet: "rocky".to_owned(),
        size: rimz::config::PetsSize::Medium,
        ..rimz::config::PetsConfig::default()
    };
    let claude = agent_row_with(
        "agent:claude:reach",
        "claude",
        "terminal_61",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "verify remote attach",
        "Sonnet 4.6",
        Some((35, 200_000, 71_400)),
        now,
        AgentRowOptions {
            age_secs: Some(35),
            ..AgentRowOptions::default()
        },
    );
    let codex = agent_row_with(
        "agent:codex:reach",
        "codex",
        "terminal_62",
        "/srv/code/query-engine/.rimz/worktrees/remote-link",
        "remote-link",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve SSH link retry",
        "GPT-5.1-Codex",
        Some((52, 272_000, 112_200)),
        now,
        AgentRowOptions {
            age_secs: Some(11 * 60),
            ..AgentRowOptions::default()
        },
    );
    let pi = agent_row_with(
        "agent:pi:reach",
        "pi",
        "terminal_63",
        "/srv/code/query-engine/.rimz/worktrees/remote-link",
        "remote-link",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Reasoning,
        "trace ControlMaster jitter",
        "Pi",
        None,
        now,
        AgentRowOptions {
            age_secs: Some(19 * 60),
            ..AgentRowOptions::default()
        },
    );
    let opencode = agent_row_with(
        "agent:opencode:edge-cache",
        "opencode",
        "terminal_64",
        "/srv/code/query-engine/.rimz/worktrees/edge-cache",
        "edge-cache",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "land offline cache probe",
        "OpenCode",
        Some((29, 128_000, 41_600)),
        now,
        AgentRowOptions {
            age_secs: Some(46 * 60),
            ..AgentRowOptions::default()
        },
    );
    let claude_idle = agent_row_with(
        "agent:claude:edge-cache",
        "claude",
        "terminal_65",
        "/srv/code/query-engine/.rimz/worktrees/edge-cache",
        "edge-cache",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "wait for laptop reconnect",
        "Sonnet 4.6",
        Some((17, 200_000, 33_200)),
        now,
        AgentRowOptions {
            age_secs: Some(94 * 60),
            ..AgentRowOptions::default()
        },
    );
    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![status_count(rimz::agents::AgentStatus::Running, 1)],
            rows: vec![claude],
            hidden_count: 0,
            diff_added: Some(18),
            diff_removed: Some(4),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/remote-link".to_owned(),
            label: "remote-link".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![
                status_count(rimz::agents::AgentStatus::Running, 1),
                status_count(rimz::agents::AgentStatus::Waiting, 1),
            ],
            rows: vec![codex, pi],
            hidden_count: 0,
            diff_added: Some(7),
            diff_removed: Some(2),
            commits_ahead: Some(1),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/edge-cache".to_owned(),
            label: "edge-cache".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: vec![
                status_count(rimz::agents::AgentStatus::Success, 1),
                status_count(rimz::agents::AgentStatus::Idle, 1),
            ],
            rows: vec![opencode, claude_idle],
            hidden_count: 0,
            diff_added: Some(22),
            diff_removed: Some(5),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
        },
    ];
    snapshot.presence = Some(rimz::SidebarPresence::Detached);
    snapshot.link = Some(rimz::SidebarLinkHealth {
        rtt_ms: Some(48),
        miss_pct: 2,
        tier: rimz::remote::link::LinkTier::Good,
        freshness: rimz::SidebarLinkFreshness::Fresh,
        sampled_at_ms: now.as_millisecond() as u64,
    });
    snapshot.providers = vec![
        provider_panel(
            "claude",
            "Claude",
            173,
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            Some((29, 45)),
            spend_tally(438.0, 28_800_000, 22),
            now,
        ),
        provider_panel(
            "codex",
            "Codex",
            33,
            Some("0.135.0"),
            Some("ChatGPT Pro"),
            true,
            false,
            Some((36, 54)),
            spend_tally(286.0, 19_100_000, 16),
            now,
        ),
        provider_panel(
            "pi",
            "Pi",
            35,
            Some("0.11.4"),
            Some("Pi Pro"),
            true,
            false,
            Some((16, 28)),
            spend_tally(144.0, 8_900_000, 9),
            now,
        ),
        provider_panel(
            "opencode",
            "OpenCode",
            141,
            Some("0.7.2"),
            Some("Team"),
            true,
            false,
            Some((12, 21)),
            spend_tally(112.0, 7_200_000, 8),
            now,
        ),
    ];
    snapshot.value_tally = Some(spend_tally(980.0, 63_800_000, 39));
    snapshot.workspace_value_tally = Some(spend_tally(724.0, 41_100_000, 27));
    snapshot.today_spend_live_usd = Some(204.0);
}

#[allow(clippy::too_many_arguments)]
fn agent_row(
    id: &str,
    name: &str,
    pane_raw: &str,
    cwd: &str,
    branch: &str,
    status: rimz::agents::AgentStatus,
    phase: rimz::agents::TurnPhase,
    task: &str,
    model: &str,
    context: Option<(u8, u64, u64)>,
    now: jiff::Timestamp,
) -> rimz::SidebarRow {
    agent_row_with(
        id,
        name,
        pane_raw,
        cwd,
        branch,
        status,
        phase,
        task,
        model,
        context,
        now,
        AgentRowOptions::default(),
    )
}

#[derive(Default)]
struct AgentRowOptions {
    handle: Option<String>,
    sub_agents: Option<Vec<rimz::SidebarSubAgent>>,
    age_secs: Option<i64>,
    compacting: bool,
    compaction_count: u32,
}

#[allow(clippy::too_many_arguments)]
fn agent_row_with(
    id: &str,
    name: &str,
    pane_raw: &str,
    cwd: &str,
    branch: &str,
    status: rimz::agents::AgentStatus,
    phase: rimz::agents::TurnPhase,
    task: &str,
    model: &str,
    context: Option<(u8, u64, u64)>,
    now: jiff::Timestamp,
    options: AgentRowOptions,
) -> rimz::SidebarRow {
    let (context_pct, context_window, total_tokens) = context
        .map_or((None, None, None), |(pct, window, total)| {
            (Some(pct), Some(window), Some(total))
        });
    let activity_at = options.age_secs.map_or(now, |secs| {
        now - std::time::Duration::from_secs(secs.max(0) as u64)
    });
    let mut card = rimz::AgentCard {
        status: Some(status),
        phase,
        surface: Some(rimz::Surface::NativeUi),
        task: Some(task.to_owned()),
        model: Some(model.to_owned()),
        handle: options.handle,
        context_pct,
        context_window,
        total_tokens,
        cache_read_input_tokens: Some(total_tokens.unwrap_or_default() / 3),
        fresh_input_tokens: Some(total_tokens.unwrap_or_default() / 5),
        output_tokens: Some(total_tokens.unwrap_or_default() / 8),
        context_severity: context_pct.map(|pct| {
            rimz::agents::ContextSeverity::classify(
                pct,
                total_tokens,
                &rimz::config::ContextMeterConfig::default(),
            )
        }),
        registered_at: Some(activity_at),
        compacting: options.compacting,
        compaction_count: options.compaction_count,
        ..rimz::AgentCard::default()
    };
    if status == rimz::agents::AgentStatus::Failed {
        card.turn_error_label = Some("API error".to_owned());
    }
    match options.sub_agents {
        Some(sub_agents) => card.sub_agents = sub_agents,
        None if status == rimz::agents::AgentStatus::Running => {
            card.sub_agents = default_sub_agents(now);
        }
        None => {}
    }

    rimz::SidebarRow {
        id: id.to_owned(),
        name: name.to_owned(),
        pane: Some(pane_ref(pane_raw, name, cwd, status.is_attention())),
        worktree_path: Some(cwd.to_owned()),
        worktree_branch: Some(branch.to_owned()),
        unread: false,
        inactive: false,
        last_activity: activity_at,
        card: rimz::RowCard::Agent(Box::new(card)),
    }
}

fn default_sub_agents(now: jiff::Timestamp) -> Vec<rimz::SidebarSubAgent> {
    vec![
        sub_agent(
            "child:explore",
            "Explore",
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Reasoning,
            Some("check unsafe edges"),
            Some("Haiku"),
            Some("audit auth watcher changes"),
            Some(22_400),
            Some(180),
            now,
        ),
        sub_agent(
            "child:plan",
            "Plan",
            rimz::agents::AgentStatus::Success,
            rimz::agents::TurnPhase::Idle,
            Some("run focused nextest"),
            Some("Haiku"),
            None,
            Some(18_900),
            Some(260),
            now,
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn sub_agent(
    id: &str,
    name: &str,
    status: rimz::agents::AgentStatus,
    phase: rimz::agents::TurnPhase,
    task: Option<&str>,
    model: Option<&str>,
    description: Option<&str>,
    total_tokens: Option<u64>,
    elapsed_secs: Option<i64>,
    now: jiff::Timestamp,
) -> rimz::SidebarSubAgent {
    rimz::SidebarSubAgent {
        id: id.to_owned(),
        name: name.to_owned(),
        status,
        phase,
        task: task.map(ToOwned::to_owned),
        model: model.map(ToOwned::to_owned),
        effort: None,
        description: description.map(ToOwned::to_owned),
        total_tokens,
        elapsed_secs,
        started_at: Some(now),
        last_activity: now,
        registered_at: Some(now),
    }
}

fn pane_ref(raw: &str, command: &str, cwd: &str, focused: bool) -> rimz::pane::PaneRef {
    rimz::pane::PaneRef {
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

fn status_count(status: rimz::agents::AgentStatus, count: usize) -> rimz::SidebarStatusCount {
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
        headline: window(1.0),
        week: window(2.8),
        month: window(11.2),
        year: window(36.4),
    }
}
