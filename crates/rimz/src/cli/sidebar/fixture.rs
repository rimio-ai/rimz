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
    for group in &mut snapshot.worktree_groups {
        group.status_counts = status_counts_from_rows(&group.rows);
    }
    snapshot.sort_groups_for_presentation();
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
        "GPT-5.5",
        Some((57, 400_000, 176_500)),
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
            status_counts: Vec::new(),
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
            status_counts: Vec::new(),
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
            cost_usd: Some(18.40),
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
        "GPT-5.5",
        Some((32, 128_000, 44_200)),
        now,
        AgentRowOptions {
            age_secs: Some(4 * 60),
            cost_usd: Some(6.75),
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
            cost_usd: Some(29.80),
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
        "GPT-5.5",
        Some((12, 400_000, 38_400)),
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
            cost_usd: Some(4.60),
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
        "GPT-5.5",
        Some((64, 400_000, 224_800)),
        now,
        AgentRowOptions {
            age_secs: Some(42 * 60),
            cost_usd: Some(12.20),
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
            cost_usd: Some(3.15),
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
            cost_usd: Some(21.90),
            ..AgentRowOptions::default()
        },
    );
    let opencode_theme = agent_row_with(
        "agent:opencode:theme",
        "opencode",
        "terminal_40",
        "/srv/code/query-engine/.rimz/worktrees/theme-tune",
        "theme-tune",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "pick gallery color ramp",
        "GPT-5.5",
        Some((53, 128_000, 71_600)),
        now,
        AgentRowOptions {
            age_secs: Some(25 * 60),
            cost_usd: Some(5.20),
            ..AgentRowOptions::default()
        },
    );
    let claude_theme = agent_row_with(
        "agent:claude:theme",
        "claude",
        "terminal_46",
        "/srv/code/query-engine/.rimz/worktrees/theme-tune",
        "theme-tune",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "verify 256-color fallback",
        "Sonnet 4.6",
        Some((36, 200_000, 74_800)),
        now,
        AgentRowOptions {
            age_secs: Some(6 * 60),
            cost_usd: Some(11.60),
            ..AgentRowOptions::default()
        },
    );
    let pi_observer = agent_row_with(
        "agent:pi:observer",
        "pi",
        "terminal_47",
        "/srv/code/query-engine/.rimz/worktrees/observer-lag",
        "observer-lag",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "wait out OpenAI window",
        "GPT-5.5",
        Some((88, 400_000, 318_000)),
        now,
        AgentRowOptions {
            age_secs: Some(93 * 60),
            cost_usd: Some(33.10),
            ..AgentRowOptions::default()
        },
    );
    let codex_observer = agent_row_with(
        "agent:codex:observer",
        "codex",
        "terminal_48",
        "/srv/code/query-engine/.rimz/worktrees/observer-lag",
        "observer-lag",
        rimz::agents::AgentStatus::Failed,
        rimz::agents::TurnPhase::Idle,
        "triage stale pane sample",
        "GPT-5.1-Codex",
        Some((61, 272_000, 149_200)),
        now,
        AgentRowOptions {
            age_secs: Some(54 * 60),
            cost_usd: Some(9.80),
            ..AgentRowOptions::default()
        },
    );
    let claude_budget_idle = agent_row_with(
        "agent:claude:budget-idle",
        "claude",
        "terminal_49",
        "/srv/code/query-engine/.rimz/worktrees/observer-lag",
        "observer-lag",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "hold perf notes",
        "Sonnet 4.6",
        Some((14, 200_000, 31_400)),
        now,
        AgentRowOptions {
            age_secs: Some(3 * 60 * 60),
            ..AgentRowOptions::default()
        },
    );
    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
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
            status_counts: Vec::new(),
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
            status_counts: Vec::new(),
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
            status_counts: Vec::new(),
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
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/theme-tune".to_owned(),
            label: "theme-tune".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![opencode_theme, claude_theme],
            hidden_count: 0,
            diff_added: Some(64),
            diff_removed: Some(18),
            commits_ahead: Some(2),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Reconciling),
            pr_state: Some(rimz::WorktreePrState::Open),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/observer-lag".to_owned(),
            label: "observer-lag".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![pi_observer, codex_observer, claude_budget_idle],
            hidden_count: 0,
            diff_added: Some(37),
            diff_removed: Some(9),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
        },
    ];
    snapshot.providers = vec![
        provider_panel(
            "claude",
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            budget_windows(44, 68),
            spend_tally(612.0, 42_600_000, 28),
            now,
        ),
        provider_panel(
            "codex",
            Some("0.135.0"),
            Some("ChatGPT Pro"),
            true,
            false,
            budget_windows(58, 73),
            spend_tally(428.0, 31_200_000, 24),
            now,
        ),
    ];
    snapshot.value_tally = Some(spend_tally(1_425.0, 91_100_000, 96));
    snapshot.workspace_value_tally = Some(spend_tally(1_168.0, 68_100_000, 78));
    snapshot.today_spend_live_usd = Some(392.0);
}

fn add_focus_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.theme.style = Some(rimz::config::ThemeStyle::Modern);
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    let mut lead = agent_row_with(
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
            cost_usd: Some(37.25),
            sub_agents: Some(vec![
                sub_agent(
                    "child:explore:routes",
                    "Explore",
                    rimz::agents::AgentStatus::Success,
                    rimz::agents::TurnPhase::Idle,
                    Some("map route ownership"),
                    Some("Haiku"),
                    Some("trace handler graph"),
                    Some(20_100),
                    Some(420),
                    now,
                ),
                sub_agent(
                    "child:explore:middleware",
                    "Explore",
                    rimz::agents::AgentStatus::Success,
                    rimz::agents::TurnPhase::Idle,
                    Some("prove middleware order"),
                    Some("Haiku"),
                    Some("exercise auth edge cases"),
                    Some(18_700),
                    Some(390),
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
                    Some(360),
                    now,
                ),
                sub_agent(
                    "child:plan:rollout",
                    "Plan",
                    rimz::agents::AgentStatus::Running,
                    rimz::agents::TurnPhase::Reasoning,
                    Some("sequence rollout guard"),
                    Some("Haiku"),
                    Some("write migration runbook"),
                    Some(9_900),
                    Some(300),
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
                    "child:general:review",
                    "general-purpose",
                    rimz::agents::AgentStatus::Success,
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
    lead.unread = true;
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
            cost_usd: Some(8.35),
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
            cost_usd: Some(5.95),
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
        "GPT-5.5",
        Some((27, 400_000, 96_300)),
        now,
        AgentRowOptions {
            age_secs: Some(37 * 60),
            cost_usd: Some(2.80),
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
    let rollout = agent_row_with(
        "agent:claude:rollout",
        "claude",
        "terminal_46",
        "/srv/code/query-engine/.rimz/worktrees/rollout-guard",
        "rollout-guard",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve staged migration",
        "Opus 4.1",
        Some((64, 200_000, 142_800)),
        now,
        AgentRowOptions {
            age_secs: Some(17 * 60),
            cost_usd: Some(18.70),
            ..AgentRowOptions::default()
        },
    );
    let opencode_rollout = agent_row_with(
        "agent:opencode:rollout",
        "opencode",
        "terminal_47",
        "/srv/code/query-engine/.rimz/worktrees/rollout-guard",
        "rollout-guard",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "patch rollout smoke harness",
        "GPT-5.5",
        Some((42, 128_000, 67_300)),
        now,
        AgentRowOptions {
            age_secs: Some(5 * 60),
            cost_usd: Some(6.30),
            ..AgentRowOptions::default()
        },
    );
    let ci_failed = agent_row_with(
        "agent:codex:ci",
        "codex",
        "terminal_48",
        "/srv/code/query-engine/.rimz/worktrees/ci-retry",
        "ci-retry",
        rimz::agents::AgentStatus::Failed,
        rimz::agents::TurnPhase::Idle,
        "fix flaky hook replay",
        "GPT-5.1-Codex",
        Some((58, 272_000, 133_500)),
        now,
        AgentRowOptions {
            age_secs: Some(44 * 60),
            cost_usd: Some(10.25),
            ..AgentRowOptions::default()
        },
    );
    let pi_paused = agent_row_with(
        "agent:pi:ci",
        "pi",
        "terminal_49",
        "/srv/code/query-engine/.rimz/worktrees/ci-retry",
        "ci-retry",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "resume after OpenAI cap",
        "GPT-5.5",
        Some((91, 400_000, 329_600)),
        now,
        AgentRowOptions {
            age_secs: Some(86 * 60),
            cost_usd: Some(27.40),
            ..AgentRowOptions::default()
        },
    );
    let pi_tokens = agent_row_with(
        "agent:pi:token-budget",
        "pi",
        "terminal_50",
        "/srv/code/query-engine/.rimz/worktrees/token-budget",
        "token-budget",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Reasoning,
        "model OAuth token budget",
        "GPT-5.5",
        Some((37, 400_000, 138_400)),
        now,
        AgentRowOptions {
            age_secs: Some(12 * 60),
            cost_usd: Some(7.85),
            ..AgentRowOptions::default()
        },
    );
    let opencode_tokens = agent_row_with(
        "agent:opencode:token-budget",
        "opencode",
        "terminal_56",
        "/srv/code/query-engine/.rimz/worktrees/token-budget",
        "token-budget",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "land budget doc example",
        "GPT-5.5",
        Some((26, 128_000, 35_900)),
        now,
        AgentRowOptions {
            age_secs: Some(49 * 60),
            cost_usd: Some(2.10),
            ..AgentRowOptions::default()
        },
    );
    let claude_token_idle = agent_row_with(
        "agent:claude:token-budget",
        "claude",
        "terminal_57",
        "/srv/code/query-engine/.rimz/worktrees/token-budget",
        "token-budget",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "wait for spend review",
        "Sonnet 4.6",
        Some((16, 200_000, 33_700)),
        now,
        AgentRowOptions {
            age_secs: Some(104 * 60),
            ..AgentRowOptions::default()
        },
    );
    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/auth-router".to_owned(),
            label: "feature/auth-router".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
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
            status_counts: Vec::new(),
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
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/rollout-guard".to_owned(),
            label: "rollout-guard".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![rollout, opencode_rollout],
            hidden_count: 0,
            diff_added: Some(52),
            diff_removed: Some(17),
            commits_ahead: Some(2),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: Some(rimz::WorktreePrState::Open),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/ci-retry".to_owned(),
            label: "ci-retry".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![ci_failed, pi_paused],
            hidden_count: 0,
            diff_added: Some(18),
            diff_removed: Some(6),
            commits_ahead: Some(1),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Reconciling),
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/token-budget".to_owned(),
            label: "token-budget".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![pi_tokens, opencode_tokens, claude_token_idle],
            hidden_count: 0,
            diff_added: Some(31),
            diff_removed: Some(11),
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
            "claude",
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            budget_windows(39, 52),
            spend_tally(506.0, 38_200_000, 25),
            now,
        ),
        provider_panel(
            "codex",
            Some("0.135.0"),
            Some("ChatGPT Pro"),
            true,
            false,
            budget_windows(47, 61),
            spend_tally(401.0, 26_600_000, 21),
            now,
        ),
        provider_panel(
            "pi",
            Some("0.11.4"),
            Some("OpenAI OAuth"),
            true,
            false,
            budget_windows(21, 34),
            spend_tally(318.0, 18_400_000, 18),
            now,
        ),
        provider_panel(
            "opencode",
            Some("0.7.2"),
            Some("OpenAI OAuth"),
            true,
            false,
            budget_windows(18, 32),
            spend_tally(156.0, 9_800_000, 14),
            now,
        ),
    ];
    snapshot.value_tally = Some(spend_tally(1_125.0, 76_200_000, 88));
    snapshot.workspace_value_tally = Some(spend_tally(1_125.0, 76_200_000, 88));
    snapshot.today_spend_live_usd = Some(268.0);
}

fn add_economy_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.theme.style = Some(rimz::config::ThemeStyle::Modern);
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    snapshot.theme.pets = rimz::config::PetsConfig {
        enabled: true,
        pet: "seedy".to_owned(),
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
        "GPT-5.5",
        Some((46, 128_000, 58_400)),
        now,
        AgentRowOptions {
            age_secs: Some(55),
            cost_usd: Some(14.20),
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
            cost_usd: Some(22.45),
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
            cost_usd: Some(7.10),
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
        "GPT-5.5",
        Some((33, 400_000, 118_900)),
        now,
        AgentRowOptions {
            age_secs: Some(31 * 60),
            cost_usd: Some(3.25),
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
    let mut opencode_limit = agent_row_with(
        "agent:opencode:limit",
        "opencode",
        "terminal_56",
        "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
        "usage-alerts",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve burst throttle",
        "GPT-5.5",
        Some((61, 128_000, 84_200)),
        now,
        AgentRowOptions {
            age_secs: Some(19 * 60),
            cost_usd: Some(8.45),
            ..AgentRowOptions::default()
        },
    );
    opencode_limit.unread = true;
    let pi_limit = agent_row_with(
        "agent:pi:limit",
        "pi",
        "terminal_57",
        "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
        "usage-alerts",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "wait for weekly cap reset",
        "GPT-5.5",
        Some((100, 400_000, 386_000)),
        now,
        AgentRowOptions {
            age_secs: Some(2 * 60 * 60),
            cost_usd: Some(39.80),
            ..AgentRowOptions::default()
        },
    );
    let claude_limit = agent_row_with(
        "agent:claude:limit",
        "claude",
        "terminal_58",
        "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
        "usage-alerts",
        rimz::agents::AgentStatus::Failed,
        rimz::agents::TurnPhase::Idle,
        "repair budget alert webhook",
        "Sonnet 4.6",
        Some((69, 200_000, 154_400)),
        now,
        AgentRowOptions {
            age_secs: Some(48 * 60),
            cost_usd: Some(17.35),
            ..AgentRowOptions::default()
        },
    );
    let codex_credit = agent_row_with(
        "agent:codex:credits",
        "codex",
        "terminal_59",
        "/srv/code/query-engine/.rimz/worktrees/credit-ledger",
        "credit-ledger",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "fold extra-credit API",
        "GPT-5.1-Codex",
        Some((44, 272_000, 104_800)),
        now,
        AgentRowOptions {
            age_secs: Some(7 * 60),
            cost_usd: Some(6.95),
            ..AgentRowOptions::default()
        },
    );
    let opencode_credit = agent_row_with(
        "agent:opencode:credits",
        "opencode",
        "terminal_60",
        "/srv/code/query-engine/.rimz/worktrees/credit-ledger",
        "credit-ledger",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "land credit ledger docs",
        "GPT-5.5",
        Some((22, 128_000, 30_100)),
        now,
        AgentRowOptions {
            age_secs: Some(55 * 60),
            cost_usd: Some(1.90),
            ..AgentRowOptions::default()
        },
    );
    let claude_credit = agent_row_with(
        "agent:claude:credits",
        "claude",
        "terminal_66",
        "/srv/code/query-engine/.rimz/worktrees/credit-ledger",
        "credit-ledger",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "summarize metered tradeoffs",
        "Sonnet 4.6",
        Some((28, 200_000, 52_600)),
        now,
        AgentRowOptions {
            age_secs: Some(71 * 60),
            cost_usd: Some(4.25),
            ..AgentRowOptions::default()
        },
    );
    let pi_credit_idle = agent_row_with(
        "agent:pi:credits",
        "pi",
        "terminal_67",
        "/srv/code/query-engine/.rimz/worktrees/credit-ledger",
        "credit-ledger",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "watch credit refill",
        "GPT-5.5",
        Some((9, 400_000, 22_100)),
        now,
        AgentRowOptions {
            age_secs: Some(3 * 60 * 60),
            ..AgentRowOptions::default()
        },
    );
    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine".to_owned(),
            label: "provider-ledger".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
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
            status_counts: Vec::new(),
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
            status_counts: Vec::new(),
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
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/usage-alerts".to_owned(),
            label: "usage-alerts".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![opencode_limit, pi_limit, claude_limit],
            hidden_count: 0,
            diff_added: Some(43),
            diff_removed: Some(13),
            commits_ahead: Some(2),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/credit-ledger".to_owned(),
            label: "credit-ledger".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![codex_credit, opencode_credit, claude_credit, pi_credit_idle],
            hidden_count: 0,
            diff_added: Some(58),
            diff_removed: Some(22),
            commits_ahead: Some(3),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Reconciling),
            pr_state: Some(rimz::WorktreePrState::Open),
        },
    ];
    let mut codex_panel = provider_panel(
        "codex",
        Some("0.135.0"),
        Some("ChatGPT Pro"),
        true,
        false,
        budget_windows_timed(15, 47, 4 * 60 * 60 + 30 * 60, 6 * 24 * 60 * 60),
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
            "claude",
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            budget_windows(18, 26),
            spend_tally(682.0, 44_600_000, 31),
            now,
        ),
        codex_panel,
        provider_panel(
            "pi",
            Some("0.11.4"),
            Some("OpenAI OAuth"),
            true,
            false,
            budget_windows_timed(100, 100, 60 * 60, 3 * 24 * 60 * 60),
            spend_tally(219.0, 13_200_000, 12),
            now,
        ),
        provider_panel(
            "opencode",
            Some("0.7.2"),
            Some("OpenAI OAuth"),
            true,
            false,
            budget_windows_timed(30, 58, 4 * 60 * 60 + 30 * 60, 6 * 24 * 60 * 60),
            spend_tally(516.0, 28_400_000, 29),
            now,
        ),
    ];
    snapshot.value_tally = Some(spend_tally(1_780.0, 118_000_000, 108));
    snapshot.workspace_value_tally = Some(spend_tally(1_416.0, 88_400_000, 83));
    snapshot.today_spend_live_usd = Some(486.0);
}

fn add_reach_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    snapshot.theme.pets = rimz::config::PetsConfig {
        enabled: true,
        pet: "rocky".to_owned(),
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
            cost_usd: Some(16.80),
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
            cost_usd: Some(4.90),
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
        "GPT-5.5",
        Some((44, 400_000, 162_700)),
        now,
        AgentRowOptions {
            age_secs: Some(19 * 60),
            cost_usd: Some(9.40),
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
        "GPT-5.5",
        Some((29, 128_000, 41_600)),
        now,
        AgentRowOptions {
            age_secs: Some(46 * 60),
            cost_usd: Some(2.65),
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
    let mut pi_vpn = agent_row_with(
        "agent:pi:vpn",
        "pi",
        "terminal_66",
        "/srv/code/query-engine/.rimz/worktrees/vpn-check",
        "vpn-check",
        rimz::agents::AgentStatus::Failed,
        rimz::agents::TurnPhase::Idle,
        "debug jump-host DNS",
        "GPT-5.5",
        Some((73, 400_000, 246_800)),
        now,
        AgentRowOptions {
            age_secs: Some(51 * 60),
            cost_usd: Some(13.55),
            ..AgentRowOptions::default()
        },
    );
    pi_vpn.unread = true;
    let opencode_vpn = agent_row_with(
        "agent:opencode:vpn",
        "opencode",
        "terminal_67",
        "/srv/code/query-engine/.rimz/worktrees/vpn-check",
        "vpn-check",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "wait for remote tunnel quota",
        "GPT-5.5",
        Some((82, 128_000, 104_900)),
        now,
        AgentRowOptions {
            age_secs: Some(73 * 60),
            cost_usd: Some(18.30),
            ..AgentRowOptions::default()
        },
    );
    let claude_vpn = agent_row_with(
        "agent:claude:vpn",
        "claude",
        "terminal_68",
        "/srv/code/query-engine/.rimz/worktrees/vpn-check",
        "vpn-check",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve VPN key rotation",
        "Sonnet 4.6",
        Some((46, 200_000, 82_300)),
        now,
        AgentRowOptions {
            age_secs: Some(24 * 60),
            cost_usd: Some(6.45),
            ..AgentRowOptions::default()
        },
    );
    let codex_web = agent_row_with(
        "agent:codex:web",
        "codex",
        "terminal_69",
        "/srv/code/query-engine/.rimz/worktrees/browser-reach",
        "browser-reach",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "exercise browser handoff",
        "GPT-5.1-Codex",
        Some((39, 272_000, 93_700)),
        now,
        AgentRowOptions {
            age_secs: Some(8 * 60),
            cost_usd: Some(5.35),
            ..AgentRowOptions::default()
        },
    );
    let pi_web = agent_row_with(
        "agent:pi:web",
        "pi",
        "terminal_70",
        "/srv/code/query-engine/.rimz/worktrees/browser-reach",
        "browser-reach",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "document OAuth browser path",
        "GPT-5.5",
        Some((21, 400_000, 74_600)),
        now,
        AgentRowOptions {
            age_secs: Some(64 * 60),
            cost_usd: Some(2.75),
            ..AgentRowOptions::default()
        },
    );
    let opencode_stats = agent_row_with(
        "agent:opencode:stats",
        "opencode",
        "terminal_71",
        "/srv/code/query-engine/.rimz/worktrees/stats-relay",
        "stats-relay",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Reasoning,
        "profile stats relay latency",
        "GPT-5.5",
        Some((31, 128_000, 47_900)),
        now,
        AgentRowOptions {
            age_secs: Some(13 * 60),
            cost_usd: Some(4.70),
            ..AgentRowOptions::default()
        },
    );
    let claude_stats_idle = agent_row_with(
        "agent:claude:stats",
        "claude",
        "terminal_72",
        "/srv/code/query-engine/.rimz/worktrees/stats-relay",
        "stats-relay",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "hold remote stats notes",
        "Sonnet 4.6",
        Some((11, 200_000, 25_800)),
        now,
        AgentRowOptions {
            age_secs: Some(2 * 60 * 60),
            ..AgentRowOptions::default()
        },
    );
    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine".to_owned(),
            label: "main".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
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
            status_counts: Vec::new(),
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
            status_counts: Vec::new(),
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
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/vpn-check".to_owned(),
            label: "vpn-check".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![pi_vpn, opencode_vpn, claude_vpn],
            hidden_count: 0,
            diff_added: Some(26),
            diff_removed: Some(7),
            commits_ahead: Some(1),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/browser-reach".to_owned(),
            label: "browser-reach".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![codex_web, pi_web],
            hidden_count: 0,
            diff_added: Some(19),
            diff_removed: Some(5),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Reconciling),
            pr_state: Some(rimz::WorktreePrState::Open),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/stats-relay".to_owned(),
            label: "stats-relay".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![opencode_stats, claude_stats_idle],
            hidden_count: 0,
            diff_added: Some(35),
            diff_removed: Some(10),
            commits_ahead: Some(2),
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
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            budget_windows(29, 45),
            spend_tally(438.0, 28_800_000, 22),
            now,
        ),
        provider_panel(
            "codex",
            Some("0.135.0"),
            Some("ChatGPT Pro"),
            true,
            false,
            budget_windows(36, 54),
            spend_tally(286.0, 19_100_000, 16),
            now,
        ),
        provider_panel(
            "pi",
            Some("0.11.4"),
            Some("OpenAI OAuth"),
            true,
            false,
            budget_windows(16, 28),
            spend_tally(144.0, 8_900_000, 9),
            now,
        ),
        provider_panel(
            "opencode",
            Some("0.7.2"),
            Some("OpenAI OAuth"),
            true,
            false,
            budget_windows(12, 21),
            spend_tally(112.0, 7_200_000, 8),
            now,
        ),
    ];
    snapshot.value_tally = Some(spend_tally(980.0, 63_800_000, 84));
    snapshot.workspace_value_tally = Some(spend_tally(724.0, 41_100_000, 64));
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
    cost_usd: Option<f64>,
    account_sub_provider: Option<&'static str>,
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
    let account_sub_provider = options
        .account_sub_provider
        .or_else(|| openai_sub_provider(name));
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
    if options.cost_usd.is_some() || account_sub_provider.is_some() {
        card.context = Some(agent_context(
            name,
            now,
            options.cost_usd,
            account_sub_provider,
        ));
    }
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

fn openai_sub_provider(kind: &str) -> Option<&'static str> {
    matches!(kind, "pi" | "opencode").then_some("openai")
}

fn agent_context(
    source: &str,
    now: jiff::Timestamp,
    cost_usd: Option<f64>,
    sub_provider: Option<&str>,
) -> rimz::agents::AgentContext {
    rimz::agents::AgentContext {
        source: source.to_owned(),
        session_name: None,
        session_preview: None,
        model_id: None,
        model_display_name: None,
        effort: None,
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: None,
        exceeds_200k_tokens: None,
        cost: cost_usd.map(|usd| rimz::agents::AgentCost {
            total_cost_usd: Some(usd),
            ..rimz::agents::AgentCost::default()
        }),
        tokens: None,
        rate_limits: None,
        pr: None,
        account: sub_provider.map(|provider| rimz::agents::AgentAccount {
            sub_provider: Some(provider.to_owned()),
            plan: Some("OpenAI OAuth".to_owned()),
            metered: Some(false),
            ..rimz::agents::AgentAccount::default()
        }),
        turn_error: None,
        turn_complete: None,
        observed_at: now,
    }
}

fn default_sub_agents(now: jiff::Timestamp) -> Vec<rimz::SidebarSubAgent> {
    vec![
        sub_agent(
            "child:explore",
            "Explore",
            rimz::agents::AgentStatus::Success,
            rimz::agents::TurnPhase::Idle,
            Some("check unsafe edges"),
            Some("Haiku"),
            Some("audit auth watcher changes"),
            Some(22_400),
            Some(320),
            now,
        ),
        sub_agent(
            "child:plan",
            "Plan",
            rimz::agents::AgentStatus::Running,
            rimz::agents::TurnPhase::Reasoning,
            Some("run focused nextest"),
            Some("Haiku"),
            None,
            Some(18_900),
            Some(180),
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
    let registered_at = elapsed_secs.map_or(now, |secs| {
        now - std::time::Duration::from_secs(secs.max(0) as u64)
    });
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
        started_at: Some(registered_at),
        last_activity: now,
        registered_at: Some(registered_at),
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

fn status_counts_from_rows(rows: &[rimz::SidebarRow]) -> Vec<rimz::SidebarStatusCount> {
    [
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::AgentStatus::Failed,
        rimz::agents::AgentStatus::Paused,
        rimz::agents::AgentStatus::Success,
        rimz::agents::AgentStatus::Running,
        rimz::agents::AgentStatus::Idle,
    ]
    .into_iter()
    .filter_map(|status| {
        let count = rows
            .iter()
            .filter(|row| row.status() == Some(status))
            .count();
        (count > 0).then_some(rimz::SidebarStatusCount { status, count })
    })
    .collect()
}

fn add_provider_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    snapshot.providers = vec![
        provider_panel(
            "claude",
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            budget_windows(25, 40),
            spend_tally(6.84, 498_000, 4),
            now,
        ),
        provider_panel(
            "codex",
            Some("0.135.0"),
            Some("ChatGPT Pro"),
            false,
            false,
            Vec::new(),
            spend_tally(3.24, 214_000, 5),
            now,
        ),
    ];
}

#[allow(clippy::too_many_arguments)]
fn provider_panel(
    kind: &str,
    version: Option<&str>,
    plan: Option<&str>,
    metered: bool,
    remote_control: bool,
    windows: Vec<ProviderWindowFixture>,
    spending: rimz::SpendTally,
    now: jiff::Timestamp,
) -> rimz::SidebarProviderPanel {
    let windows = windows
        .into_iter()
        .map(|window| rimz::agents::RateLimitWindow {
            used_percentage: Some(window.used),
            resets_at: Some(now + std::time::Duration::from_secs(window.resets_in_secs)),
            duration_mins: Some(window.duration_mins),
            ..Default::default()
        })
        .collect();
    let (product_name, art, color, color_rgb) =
        if let Some(descriptor) = rimz::agents::descriptor_by_kind(kind) {
            (
                descriptor.display_name.to_owned(),
                descriptor
                    .brand
                    .emblem
                    .trim_matches('\n')
                    .lines()
                    .map(ToOwned::to_owned)
                    .collect(),
                descriptor.brand.color,
                Some(descriptor.brand.color_rgb),
            )
        } else {
            (provider_title_case(kind), Vec::new(), 244, None)
        };
    rimz::SidebarProviderPanel {
        kind: kind.to_owned(),
        product_name,
        art,
        color,
        color_rgb,
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

#[derive(Clone, Copy)]
struct ProviderWindowFixture {
    used: u8,
    duration_mins: u32,
    resets_in_secs: u64,
}

fn budget_windows(short_used: u8, long_used: u8) -> Vec<ProviderWindowFixture> {
    budget_windows_timed(short_used, long_used, 2 * 60 * 60, 2 * 24 * 60 * 60)
}

fn budget_windows_timed(
    short_used: u8,
    long_used: u8,
    short_resets_in_secs: u64,
    long_resets_in_secs: u64,
) -> Vec<ProviderWindowFixture> {
    vec![
        ProviderWindowFixture {
            used: short_used,
            duration_mins: 300,
            resets_in_secs: short_resets_in_secs,
        },
        ProviderWindowFixture {
            used: long_used,
            duration_mins: 7 * 24 * 60,
            resets_in_secs: long_resets_in_secs,
        },
    ]
}

fn provider_title_case(value: &str) -> String {
    value
        .split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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
