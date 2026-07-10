use super::*;

pub(super) fn sidebar_fixture_snapshot(
    state: SidebarFixtureState,
) -> Result<rimz::SidebarSnapshot> {
    let now = fixture_now()?;
    let workspace_id = "ws_0123456789abcdef01234567".parse::<WorkspaceId>()?;
    let mut snapshot = rimz::SidebarSnapshot {
        snapshot_version: rimz::store::snapshot::SNAPSHOT_VERSION,
        workspace_id,
        display_name: "query-engine".to_owned(),
        generated_at: now,
        panes_produced_at_ms: Some(1_781_009_600_000),
        panes_observed_at_ms: None,
        focused_pane: None,
        viewed_panes: Vec::new(),
        presence: None,
        truth_degraded: None,
        now,
        worktree_groups: Vec::new(),
        agents: Vec::new(),
        wired_kinds: vec!["codex".to_owned()],
        wired_default_models: std::collections::BTreeMap::new(),
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
        today_spend_epoch_secs: None,
        fleet_day_spend_usd: None,
        fleet_day_spend_epoch_secs: None,
        fleet_budget: None,
        link: None,
        reflects_log: None,
        resume_outcomes: Some(Vec::new()),
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
    snapshot.sort_groups_for_presentation();
    for group in &mut snapshot.worktree_groups {
        move_fixture_overflow_to_tail(&mut group.rows);
        group.status_counts = status_counts_from_rows(&group.rows);
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
        "Opus 4.8",
        Some((341_700, 12.40)),
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
        "GPT-5.5",
        Some((193_480, 4.36)),
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
        Some((258_610, 2.18)),
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
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
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
            rows: with_overflow(vec![claude, codex, process], 3, now),
            diff_added: Some(182),
            diff_removed: Some(47),
            commits_ahead: Some(3),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/mux".to_owned(),
            label: "zellij-health".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![pi],
            diff_added: Some(14),
            diff_removed: Some(3),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
            pr_number: None,
        },
    ];
    snapshot.value_tally = Some(spend_tally(9.42, 712_000, 8));
    snapshot.workspace_value_tally = Some(spend_tally(6.84, 481_000, 5));
    snapshot.today_spend_live_usd = Some(10.08);
}

fn add_cockpit_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.display_name = "flightdeck".to_owned();
    snapshot.project_root = Some(PathBuf::from("/srv/code/flightdeck"));
    snapshot.worktree_roots = vec![PathBuf::from("/srv/code/flightdeck")];
    snapshot.theme.style = Some(rimz::config::ThemeStyle::Modern);
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    let claude = agent_row_with(
        "agent:claude:main",
        "claude",
        "terminal_31",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "stabilize render diff",
        "Opus 4.8",
        Some((247_310, 8.64)),
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
        "GPT-5.5",
        Some((103_970, 3.07)),
        now,
        AgentRowOptions {
            age_secs: Some(4 * 60),
            compaction_count: 1,
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
        "Opus 4.8",
        Some((797_420, 28.90)),
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
        "GPT-5.5",
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
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: now,
        card: rimz::RowCard::Process(rimz::ProcessCard {
            state: rimz::ProcessState::Busy,
            command_detail: Some("integration::backend".to_owned()),
            cpu_pct: Some(37),
            rss_kb: Some(412_000),
            io_bps: Some(8 * 1_048_576),
            ..rimz::ProcessCard::default()
        }),
    };
    let codex = agent_row_with(
        "agent:codex:pricing",
        "codex",
        "terminal_09",
        "/srv/code/query-engine/.rimz/worktrees/pricing-refresh",
        "pricing-refresh",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve pricing cache write",
        "GPT-5.5",
        Some((158_210, 4.82)),
        now,
        AgentRowOptions {
            age_secs: Some(18 * 60),
            ..AgentRowOptions::default()
        },
    );
    let mut pi = agent_row_with(
        "agent:pi:zellij",
        "pi",
        "terminal_35",
        "/srv/code/query-engine/.rimz/worktrees/zellij-health",
        "zellij-health",
        rimz::agents::AgentStatus::Failed,
        rimz::agents::TurnPhase::Idle,
        "debug zellij health probe",
        "GPT-5.5",
        Some((228_540, 5.76)),
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
        "Opus 4.8",
        Some((132_770, 2.41)),
        now,
        AgentRowOptions {
            age_secs: Some(2 * 60 * 60),
            ..AgentRowOptions::default()
        },
    );
    success.unread = true;
    let mut paused = agent_row_with(
        "agent:claude:mux-merge-paused",
        "claude",
        "terminal_37",
        "/srv/code/query-engine/.rimz/worktrees/mux-merge",
        "mux-merge",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "holding the tmux hook fix mid-land",
        "Opus 4.8",
        Some((469_180, 15.38)),
        now,
        AgentRowOptions {
            age_secs: Some(75 * 60),
            turn_error_label: Some("API Error: Overloaded".to_owned()),
            ..AgentRowOptions::default()
        },
    );
    paused.unread = true;
    pi.unread = true;
    let mut opencode_theme = agent_row_with(
        "agent:opencode:theme",
        "opencode",
        "terminal_40",
        "/srv/code/query-engine/.rimz/worktrees/theme-tune",
        "theme-tune",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "pick gallery color ramp",
        "GPT-5.5",
        Some((42_130, 0.68)),
        now,
        AgentRowOptions {
            age_secs: Some(60 * 60),
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
        "Opus 4.8",
        Some((314_260, 6.95)),
        now,
        AgentRowOptions {
            age_secs: Some(6 * 60),
            ..AgentRowOptions::default()
        },
    );
    let pi_observer = agent_row_with(
        "agent:claude:observer",
        "claude",
        "terminal_47",
        "/srv/code/query-engine/.rimz/worktrees/observer-lag",
        "observer-lag",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "frozen on the stale-pane triage",
        "Opus 4.8",
        Some((853_690, 29.84)),
        now,
        AgentRowOptions {
            age_secs: Some(93 * 60),
            turn_error_label: Some("API Error: Overloaded".to_owned()),
            ..AgentRowOptions::default()
        },
    );
    opencode_theme.unread = true;
    let codex_observer = agent_row_with(
        "agent:codex:observer",
        "codex",
        "terminal_48",
        "/srv/code/query-engine/.rimz/worktrees/observer-lag",
        "observer-lag",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "triage stale pane sample",
        "GPT-5.5",
        Some((185_730, 3.83)),
        now,
        AgentRowOptions {
            age_secs: Some(54 * 60),
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
        "Opus 4.8",
        None,
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
            rows: vec![claude, compacting, process],
            diff_added: Some(1840),
            diff_removed: Some(620),
            commits_ahead: Some(3),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/pricing-refresh".to_owned(),
            label: "pricing-refresh".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![codex],
            diff_added: Some(1180),
            diff_removed: Some(430),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_number: Some(91),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/zellij-health".to_owned(),
            label: "zellij-health".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![pi],
            diff_added: Some(760),
            diff_removed: Some(210),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/mux-merge".to_owned(),
            label: "mux-merge".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![success, paused],
            diff_added: Some(0),
            diff_removed: Some(0),
            commits_ahead: Some(0),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(true),
            landed: Some(true),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Merged),
            pr_state: Some(rimz::WorktreePrState::Merged),
            pr_number: Some(91),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/theme-tune".to_owned(),
            label: "theme-tune".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![opencode_theme, claude_theme],
            diff_added: Some(1320),
            diff_removed: Some(540),
            commits_ahead: Some(2),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Reconciling),
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_number: Some(91),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/observer-lag".to_owned(),
            label: "observer-lag".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![
                opencode,
                idle,
                pi_observer,
                codex_observer,
                claude_budget_idle,
            ],
            diff_added: Some(980),
            diff_removed: Some(360),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
            pr_number: None,
        },
    ];
    let mut codex_panel = provider_panel(
        "codex",
        Some("0.135.0"),
        Some("ChatGPT Pro"),
        true,
        true,
        budget_windows_timed(70, 90, 7_200, 423_360),
        spend_tally(428.0, 31_200_000, 24),
        now,
    );
    codex_panel.reset_credits = Some(rimz::ResetCredits {
        count: 2,
        soonest_expiry: None,
    });
    snapshot.providers = vec![
        provider_panel(
            "claude",
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            budget_windows(30, 50),
            spend_tally(612.0, 42_600_000, 28),
            now,
        ),
        codex_panel,
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
    snapshot.value_tally = Some(spend_tally(1_425.0, 91_100_000, 96));
    snapshot.workspace_value_tally = Some(spend_tally(1_168.0, 68_100_000, 78));
    snapshot.today_spend_live_usd = Some(392.0);
}

fn add_focus_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.display_name = "huddle".to_owned();
    snapshot.project_root = Some(PathBuf::from("/srv/code/huddle"));
    snapshot.worktree_roots = vec![PathBuf::from("/srv/code/huddle")];
    snapshot.theme.style = Some(rimz::config::ThemeStyle::Modern);
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    let coder = agent_row_with(
        "agent:codex:coder",
        "codex",
        "terminal_41",
        "/srv/code/query-engine/.rimz/worktrees/auth-router",
        "feature/auth-router",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "wire auth router migration",
        "GPT-5.5",
        Some((207_110, 11.84)),
        now,
        AgentRowOptions {
            handle: Some("coder".to_owned()),
            launch_group: Some("auth-router-team".to_owned()),
            launch_ordinal: Some(1),
            sub_agents: Some(vec![
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
    let planner = agent_row_with(
        "agent:claude:planner",
        "claude",
        "terminal_42",
        "/srv/code/query-engine/.rimz/worktrees/auth-router",
        "feature/auth-router",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "plan router cutover",
        "Opus 4.8",
        Some((183_450, 14.72)),
        now,
        AgentRowOptions {
            handle: Some("planner".to_owned()),
            launch_group: Some("auth-router-team".to_owned()),
            launch_ordinal: Some(0),
            age_secs: Some(22 * 60),
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
                    rimz::agents::AgentStatus::Success,
                    rimz::agents::TurnPhase::Idle,
                    Some("sequence rollout guard"),
                    Some("Haiku"),
                    Some("write migration runbook"),
                    Some(9_900),
                    Some(300),
                    now,
                ),
                sub_agent(
                    "child:plan:review",
                    "Plan",
                    rimz::agents::AgentStatus::Success,
                    rimz::agents::TurnPhase::Idle,
                    Some("stage review gates"),
                    Some("Haiku"),
                    Some("split blocking checks"),
                    Some(8_700),
                    Some(240),
                    now,
                ),
            ]),
            ..AgentRowOptions::default()
        },
    );
    let reviewer = agent_row_with(
        "agent:pi:reviewer",
        "pi",
        "terminal_43",
        "/srv/code/query-engine/.rimz/worktrees/auth-router",
        "feature/auth-router",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "review auth router diff",
        "GPT-5.5",
        None,
        now,
        AgentRowOptions {
            handle: Some("reviewer".to_owned()),
            launch_group: Some("auth-router-team".to_owned()),
            launch_ordinal: Some(2),
            age_secs: Some(9 * 60),
            ..AgentRowOptions::default()
        },
    );
    let rollout_coder = agent_row_with(
        "agent:codex:rollout-coder",
        "codex",
        "terminal_44",
        "/srv/code/query-engine/.rimz/worktrees/rollout-guard",
        "rollout-guard",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "patch rollout smoke harness",
        "GPT-5.5",
        Some((142_260, 2.93)),
        now,
        AgentRowOptions {
            handle: Some("coder".to_owned()),
            age_secs: Some(5 * 60),
            ..AgentRowOptions::default()
        },
    );
    let mut rollout_reviewer = agent_row_with(
        "agent:claude:rollout-reviewer",
        "claude",
        "terminal_45",
        "/srv/code/query-engine/.rimz/worktrees/rollout-guard",
        "rollout-guard",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve staged migration",
        "Opus 4.8",
        Some((347_890, 9.04)),
        now,
        AgentRowOptions {
            handle: Some("reviewer".to_owned()),
            age_secs: Some(52 * 60),
            compaction_count: 1,
            ..AgentRowOptions::default()
        },
    );
    rollout_reviewer.unread = true;
    let mut ci_failed = agent_row_with(
        "agent:codex:ci",
        "codex",
        "terminal_48",
        "/srv/code/query-engine/.rimz/worktrees/ci-retry",
        "ci-retry",
        rimz::agents::AgentStatus::Failed,
        rimz::agents::TurnPhase::Idle,
        "fix flaky hook replay",
        "GPT-5.5",
        Some((167_440, 4.58)),
        now,
        AgentRowOptions {
            age_secs: Some(44 * 60),
            ..AgentRowOptions::default()
        },
    );
    ci_failed.unread = true;
    let pi_paused = agent_row_with(
        "agent:claude:ci-paused",
        "claude",
        "terminal_49",
        "/srv/code/query-engine/.rimz/worktrees/ci-retry",
        "ci-retry",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "parked mid hook-replay repair",
        "Opus 4.8",
        Some((823_560, 26.75)),
        now,
        AgentRowOptions {
            age_secs: Some(86 * 60),
            turn_error_label: Some("API Error: Overloaded".to_owned()),
            ..AgentRowOptions::default()
        },
    );
    let architect_tokens = agent_row_with(
        "agent:claude:token-budget",
        "claude",
        "terminal_50",
        "/srv/code/query-engine/.rimz/worktrees/token-budget",
        "token-budget",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "model OAuth token budget",
        "Opus 4.8",
        Some((74_330, 1.26)),
        now,
        AgentRowOptions {
            handle: Some("architect".to_owned()),
            age_secs: Some(12 * 60),
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
        Some((33_210, 0.54)),
        now,
        AgentRowOptions {
            handle: Some("developer".to_owned()),
            age_secs: Some(49 * 60),
            ..AgentRowOptions::default()
        },
    );
    let sre_tokens = agent_row_with(
        "agent:codex:token-budget",
        "codex",
        "terminal_57",
        "/srv/code/query-engine/.rimz/worktrees/token-budget",
        "token-budget",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "wait for spend review",
        "GPT-5.5",
        None,
        now,
        AgentRowOptions {
            handle: Some("sre".to_owned()),
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
            rows: vec![planner, coder, reviewer],
            diff_added: Some(1520),
            diff_removed: Some(470),
            commits_ahead: Some(2),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/rollout-guard".to_owned(),
            label: "rollout-guard".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![rollout_coder, rollout_reviewer],
            diff_added: Some(1180),
            diff_removed: Some(390),
            commits_ahead: Some(2),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_number: Some(91),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/ci-retry".to_owned(),
            label: "ci-retry".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![ci_failed, pi_paused],
            diff_added: Some(880),
            diff_removed: Some(240),
            commits_ahead: Some(1),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Reconciling),
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/token-budget".to_owned(),
            label: "token-budget".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![architect_tokens, opencode_tokens, sre_tokens],
            diff_added: Some(1420),
            diff_removed: Some(520),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_number: Some(91),
        },
    ];
    let mut codex_panel = provider_panel(
        "codex",
        Some("0.135.0"),
        Some("ChatGPT Pro"),
        true,
        true,
        budget_windows_timed(70, 90, 7_200, 423_360),
        spend_tally(401.0, 26_600_000, 21),
        now,
    );
    codex_panel.reset_credits = Some(rimz::ResetCredits {
        count: 2,
        soonest_expiry: None,
    });
    snapshot.providers = vec![
        provider_panel(
            "claude",
            Some("2.1.158"),
            Some("Claude Max"),
            true,
            true,
            budget_windows(30, 50),
            spend_tally(506.0, 38_200_000, 25),
            now,
        ),
        codex_panel,
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
    snapshot.link = Some(rimz::SidebarLinkHealth {
        rtt_ms: Some(48),
        miss_pct: 2,
        tier: rimz::remote::link::LinkTier::Good,
        freshness: rimz::SidebarLinkFreshness::Fresh,
        sampled_at_ms: now.as_millisecond() as u64,
    });
}

fn add_economy_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.display_name = "abacus".to_owned();
    snapshot.project_root = Some(PathBuf::from("/srv/code/abacus"));
    snapshot.worktree_roots = vec![PathBuf::from("/srv/code/abacus")];
    snapshot.theme.style = Some(rimz::config::ThemeStyle::Modern);
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    let mut opencode = agent_row_with(
        "agent:opencode:economy",
        "opencode",
        "terminal_51",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "rebalance metered sessions",
        "GPT-5.5",
        Some((175_350, 7.44)),
        now,
        AgentRowOptions {
            age_secs: Some(55),
            ..AgentRowOptions::default()
        },
    );
    opencode.unread = true;
    let mut claude = agent_row_with(
        "agent:claude:economy",
        "claude",
        "terminal_52",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Reasoning,
        "audit provider spend",
        "Opus 4.8",
        Some((296_430, 12.62)),
        now,
        AgentRowOptions {
            age_secs: Some(6 * 60),
            ..AgentRowOptions::default()
        },
    );
    claude.unread = true;
    let pnpm = rimz::SidebarRow {
        id: "process:pnpm-serve".to_owned(),
        name: "pnpm serve".to_owned(),
        pane: Some(pane_ref(
            "terminal_30",
            "pnpm serve",
            "/srv/code/query-engine",
            false,
        )),
        worktree_path: Some("/srv/code/query-engine".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: now,
        card: rimz::RowCard::Process(rimz::ProcessCard {
            state: rimz::ProcessState::Busy,
            command_detail: Some("vite dev :5173".to_owned()),
            cpu_pct: Some(12),
            rss_kb: Some(268_000),
            io_bps: Some(450 * 1_024),
            ..rimz::ProcessCard::default()
        }),
    };
    let codex = agent_row_with(
        "agent:codex:economy",
        "codex",
        "terminal_53",
        "/srv/code/query-engine/.rimz/worktrees/pricing-refresh",
        "pricing-refresh",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve price snapshot",
        "GPT-5.5",
        Some((118_620, 3.29)),
        now,
        AgentRowOptions {
            age_secs: Some(14 * 60),
            ..AgentRowOptions::default()
        },
    );
    let mut pi = agent_row_with(
        "agent:pi:budget",
        "pi",
        "terminal_54",
        "/srv/code/query-engine/.rimz/worktrees/cost-caps",
        "cost-caps",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "publish budget notes",
        "GPT-5.5",
        Some((96_840, 1.74)),
        now,
        AgentRowOptions {
            age_secs: Some(31 * 60),
            ..AgentRowOptions::default()
        },
    );
    pi.unread = true;
    let codex_idle = agent_row_with(
        "agent:codex:budget",
        "codex",
        "terminal_55",
        "/srv/code/query-engine/.rimz/worktrees/cost-caps",
        "cost-caps",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "hold weekly spend cap",
        "GPT-5.5",
        None,
        now,
        AgentRowOptions {
            age_secs: Some(83 * 60),
            ..AgentRowOptions::default()
        },
    );
    let opencode_limit = agent_row_with(
        "agent:opencode:limit",
        "opencode",
        "terminal_56",
        "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
        "usage-alerts",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve burst throttle",
        "GPT-5.5",
        Some((216_700, 5.18)),
        now,
        AgentRowOptions {
            age_secs: Some(19 * 60),
            ..AgentRowOptions::default()
        },
    );
    let mut pi_limit = agent_row_with(
        "agent:claude:limit-paused",
        "claude",
        "terminal_57",
        "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
        "usage-alerts",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "staged the budget-alert fix, idling",
        "Opus 4.8",
        Some((856_120, 29.36)),
        now,
        AgentRowOptions {
            age_secs: Some(2 * 60 * 60),
            turn_error_label: Some("API Error: rate limit exceeded".to_owned()),
            ..AgentRowOptions::default()
        },
    );
    pi_limit.unread = true;
    let claude_limit = agent_row_with(
        "agent:claude:limit",
        "claude",
        "terminal_58",
        "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
        "usage-alerts",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "repair budget alert webhook",
        "Opus 4.8",
        Some((372_640, 8.91)),
        now,
        AgentRowOptions {
            age_secs: Some(48 * 60),
            compacting: true,
            ..AgentRowOptions::default()
        },
    );
    let codex_credit = agent_row_with(
        "agent:codex:credits",
        "codex",
        "terminal_59",
        "/srv/code/query-engine/.rimz/worktrees/credit-store",
        "credit-store",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "fold extra-credit API",
        "GPT-5.5",
        Some((158_930, 2.67)),
        now,
        AgentRowOptions {
            age_secs: Some(7 * 60),
            ..AgentRowOptions::default()
        },
    );
    let opencode_credit = agent_row_with(
        "agent:opencode:credits",
        "opencode",
        "terminal_60",
        "/srv/code/query-engine/.rimz/worktrees/credit-store",
        "credit-store",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "land credit store docs",
        "GPT-5.5",
        Some((29_250, 0.42)),
        now,
        AgentRowOptions {
            age_secs: Some(55 * 60),
            ..AgentRowOptions::default()
        },
    );
    let mut claude_credit = agent_row_with(
        "agent:claude:credits",
        "claude",
        "terminal_66",
        "/srv/code/query-engine/.rimz/worktrees/credit-store",
        "credit-store",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "summarize metered tradeoffs",
        "Opus 4.8",
        Some((165_880, 4.08)),
        now,
        AgentRowOptions {
            age_secs: Some(71 * 60),
            ..AgentRowOptions::default()
        },
    );
    claude_credit.unread = true;
    let pi_credit_idle = agent_row_with(
        "agent:pi:credits",
        "pi",
        "terminal_67",
        "/srv/code/query-engine/.rimz/worktrees/credit-store",
        "credit-store",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "watch credit refill",
        "GPT-5.5",
        None,
        now,
        AgentRowOptions {
            age_secs: Some(3 * 60 * 60),
            ..AgentRowOptions::default()
        },
    );
    snapshot.worktree_groups = vec![
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine".to_owned(),
            label: "provider-store".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![opencode, claude, pnpm],
            diff_added: Some(1580),
            diff_removed: Some(520),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_number: Some(91),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/pricing-refresh".to_owned(),
            label: "pricing-refresh".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![codex],
            diff_added: Some(700),
            diff_removed: Some(210),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/cost-caps".to_owned(),
            label: "cost-caps".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![pi, codex_idle],
            diff_added: Some(1120),
            diff_removed: Some(360),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_number: Some(91),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/usage-alerts".to_owned(),
            label: "usage-alerts".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![opencode_limit, pi_limit, claude_limit],
            diff_added: Some(1980),
            diff_removed: Some(740),
            commits_ahead: Some(2),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/credit-store".to_owned(),
            label: "credit-store".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![codex_credit, opencode_credit, claude_credit, pi_credit_idle],
            diff_added: Some(0),
            diff_removed: Some(0),
            commits_ahead: Some(0),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(true),
            landed: Some(true),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Merged),
            pr_state: Some(rimz::WorktreePrState::Merged),
            pr_number: Some(91),
        },
    ];
    let mut codex_panel = provider_panel(
        "codex",
        Some("0.135.0"),
        Some("ChatGPT Pro"),
        true,
        true,
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
            budget_windows_timed(100, 100, 7_200, 345_600),
            spend_tally(516.0, 28_400_000, 29),
            now,
        ),
    ];
    snapshot.value_tally = Some(spend_tally(1_780.0, 118_000_000, 108));
    snapshot.workspace_value_tally = Some(spend_tally(1_416.0, 88_400_000, 83));
    snapshot.today_spend_live_usd = Some(486.0);
}

fn add_reach_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    snapshot.display_name = "farlink".to_owned();
    snapshot.project_root = Some(PathBuf::from("/srv/code/farlink"));
    snapshot.worktree_roots = vec![PathBuf::from("/srv/code/farlink")];
    snapshot.theme.display.provider_tabs = rimz::config::ProviderTabsMode::Always;
    let mut claude = agent_row_with(
        "agent:claude:reach",
        "claude",
        "terminal_61",
        "/srv/code/query-engine",
        "main",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "verify remote attach",
        "Opus 4.8",
        Some((333_710, 13.56)),
        now,
        AgentRowOptions {
            age_secs: Some(35),
            compaction_count: 2,
            ..AgentRowOptions::default()
        },
    );
    claude.unread = true;
    let mut codex = agent_row_with(
        "agent:codex:reach",
        "codex",
        "terminal_62",
        "/srv/code/query-engine/.rimz/worktrees/remote-link",
        "remote-link",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve SSH link retry",
        "GPT-5.5",
        Some((153_560, 3.71)),
        now,
        AgentRowOptions {
            age_secs: Some(11 * 60),
            ..AgentRowOptions::default()
        },
    );
    codex.unread = true;
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
        Some((185_910, 4.44)),
        now,
        AgentRowOptions {
            age_secs: Some(19 * 60),
            ..AgentRowOptions::default()
        },
    );
    let mut opencode = agent_row_with(
        "agent:opencode:edge-cache",
        "opencode",
        "terminal_67",
        "/srv/code/query-engine/.rimz/worktrees/edge-cache",
        "edge-cache",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "land offline cache probe",
        "GPT-5.5",
        Some((61_170, 0.91)),
        now,
        AgentRowOptions {
            age_secs: Some(46 * 60),
            ..AgentRowOptions::default()
        },
    );
    opencode.unread = true;
    let claude_idle = agent_row_with(
        "agent:claude:edge-cache",
        "claude",
        "terminal_68",
        "/srv/code/query-engine/.rimz/worktrees/edge-cache",
        "edge-cache",
        rimz::agents::AgentStatus::Idle,
        rimz::agents::TurnPhase::Idle,
        "wait for laptop reconnect",
        "Opus 4.8",
        None,
        now,
        AgentRowOptions {
            age_secs: Some(94 * 60),
            ..AgentRowOptions::default()
        },
    );
    let pi_netcheck = agent_row_with(
        "agent:pi:netcheck",
        "pi",
        "terminal_64",
        "/srv/code/query-engine/.rimz/worktrees/network-check",
        "network-check",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "debug jump-host DNS",
        "GPT-5.5",
        Some((251_620, 6.24)),
        now,
        AgentRowOptions {
            age_secs: Some(51 * 60),
            ..AgentRowOptions::default()
        },
    );
    let claude_netcheck_paused = agent_row_with(
        "agent:claude:netcheck-paused",
        "claude",
        "terminal_65",
        "/srv/code/query-engine/.rimz/worktrees/network-check",
        "network-check",
        rimz::agents::AgentStatus::Paused,
        rimz::agents::TurnPhase::Idle,
        "halfway through the jump-host DNS fix",
        "Opus 4.8",
        Some((414_390, 10.76)),
        now,
        AgentRowOptions {
            age_secs: Some(73 * 60),
            turn_error_label: Some("API Error: rate limit exceeded".to_owned()),
            ..AgentRowOptions::default()
        },
    );
    let mut claude_netcheck = agent_row_with(
        "agent:claude:netcheck",
        "claude",
        "terminal_66",
        "/srv/code/query-engine/.rimz/worktrees/network-check",
        "network-check",
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::TurnPhase::Idle,
        "approve firewall rule change",
        "Opus 4.8",
        Some((236_510, 6.88)),
        now,
        AgentRowOptions {
            age_secs: Some(24 * 60),
            ..AgentRowOptions::default()
        },
    );
    claude_netcheck.unread = true;
    let codex_web = agent_row_with(
        "agent:codex:web",
        "codex",
        "terminal_69",
        "/srv/code/query-engine/.rimz/worktrees/browser-reach",
        "browser-reach",
        rimz::agents::AgentStatus::Running,
        rimz::agents::TurnPhase::Acting,
        "exercise browser handoff",
        "GPT-5.5",
        Some((129_080, 2.36)),
        now,
        AgentRowOptions {
            age_secs: Some(8 * 60),
            ..AgentRowOptions::default()
        },
    );
    let mut pi_web = agent_row_with(
        "agent:pi:web",
        "pi",
        "terminal_70",
        "/srv/code/query-engine/.rimz/worktrees/browser-reach",
        "browser-reach",
        rimz::agents::AgentStatus::Success,
        rimz::agents::TurnPhase::Idle,
        "document OAuth browser path",
        "GPT-5.5",
        Some((50_190, 0.73)),
        now,
        AgentRowOptions {
            age_secs: Some(64 * 60),
            ..AgentRowOptions::default()
        },
    );
    pi_web.unread = true;
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
        Some((85_470, 1.58)),
        now,
        AgentRowOptions {
            age_secs: Some(13 * 60),
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
        "Opus 4.8",
        None,
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
            diff_added: Some(720),
            diff_removed: Some(160),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/remote-link".to_owned(),
            label: "remote-link".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![codex, pi],
            diff_added: Some(1240),
            diff_removed: Some(380),
            commits_ahead: Some(1),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/edge-cache".to_owned(),
            label: "edge-cache".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![opencode, claude_idle],
            diff_added: Some(560),
            diff_removed: Some(140),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_number: Some(91),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/network-check".to_owned(),
            label: "network-check".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![pi_netcheck, claude_netcheck_paused, claude_netcheck],
            diff_added: Some(1680),
            diff_removed: Some(610),
            commits_ahead: Some(1),
            commits_behind: Some(1),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: None,
            pr_number: None,
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/browser-reach".to_owned(),
            label: "browser-reach".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![codex_web, pi_web],
            diff_added: Some(940),
            diff_removed: Some(280),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: Some(rimz::WorktreePrState::Closed),
            pr_number: Some(91),
        },
        rimz::SidebarWorktreeGroup {
            key: "/srv/code/query-engine/.rimz/worktrees/stats-relay".to_owned(),
            label: "stats-relay".to_owned(),
            kind: rimz::SidebarWorktreeKind::Worktree,
            status_counts: Vec::new(),
            rows: vec![opencode_stats, claude_stats_idle],
            diff_added: Some(0),
            diff_removed: Some(0),
            commits_ahead: Some(0),
            commits_behind: Some(0),
            trunk: Some("main".to_owned()),
            worktree_backed: false,
            clean: Some(true),
            landed: Some(false),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Pristine),
            pr_state: None,
            pr_number: None,
        },
    ];
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
            true,
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
            budget_windows_timed(100, 76, 10_800, 172_800),
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
    context: Option<(u64, f64)>,
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
    launch_group: Option<String>,
    launch_ordinal: Option<u32>,
    sub_agents: Option<Vec<rimz::SidebarSubAgent>>,
    age_secs: Option<i64>,
    account_sub_provider: Option<&'static str>,
    turn_error_label: Option<String>,
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
    context: Option<(u64, f64)>,
    now: jiff::Timestamp,
    options: AgentRowOptions,
) -> rimz::SidebarRow {
    let is_idle = status == rimz::agents::AgentStatus::Idle;
    // context = (live context-window tokens for the bar/breakdown, cumulative
    // session cost in USD). The two are independent: fill is a snapshot, cost is
    // lifetime spend and does not track the current fill.
    let (context_pct, context_window, total_tokens, cost_usd) = if is_idle {
        (None, None, None, None)
    } else {
        context.map_or((None, None, None, None), |(total, cost)| {
            let window = context_window_for_kind(name);
            let used = total.min(window - 1);
            let pct = used.saturating_mul(100).div_euclid(window).min(100) as u8;
            (Some(pct), Some(window), Some(used), Some(cost))
        })
    };
    let activity_at = options.age_secs.map_or(now, |secs| {
        now - std::time::Duration::from_secs(secs.max(0) as u64)
    });
    let account_sub_provider = options
        .account_sub_provider
        .or_else(|| openai_sub_provider(name));
    // Cache-heavy, realistic split: the first three fields sum to the window fill,
    // with cache_read taking the remainder for an exact sum; output rides separately
    // and lands in the window next turn. Fresh input is a small slice of the cached
    // context. Only Claude reports explicit cache-creation tokens — GPT/Codex caching
    // is implicit, so those cards carry none.
    let split = total_tokens.map(|tokens| {
        let fresh_input = tokens / 25;
        let cache_write = if name == "claude" { tokens / 10 } else { 0 };
        let cache_read = tokens - cache_write - fresh_input;
        let output = tokens / 8;
        (cache_read, cache_write, fresh_input, output)
    });
    let mut card = rimz::AgentCard {
        status,
        phase,
        task: (!is_idle).then(|| task.to_owned()),
        model: Some(model.to_owned()),
        effort: Some("xhigh".to_owned()),
        handle: options.handle,
        team: None,
        launch_group: options.launch_group,
        launch_ordinal: options.launch_ordinal,

        context_pct,
        context_window,
        total_tokens,
        cache_read_input_tokens: split.map(|tokens| tokens.0),
        cache_write_input_tokens: split.map(|tokens| tokens.1),
        fresh_input_tokens: split.map(|tokens| tokens.2),
        output_tokens: split.map(|tokens| tokens.3),
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
    if !is_idle && (cost_usd.is_some() || account_sub_provider.is_some()) {
        card.context = Some(agent_context(name, now, cost_usd, account_sub_provider));
    }
    card.turn_error_label = options
        .turn_error_label
        .or_else(|| (status == rimz::agents::AgentStatus::Failed).then(|| "API error".to_owned()));
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
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: activity_at,
        card: rimz::RowCard::Agent(Box::new(card)),
    }
}

fn context_window_for_kind(kind: &str) -> u64 {
    if kind == "claude" { 1_000_000 } else { 272_000 }
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
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        turn_interrupted: None,
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
        effort: Some("xhigh".to_owned()),
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
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

const FIXTURE_WORKTREE_ROW_CAP: usize = 6;

fn with_overflow(
    mut rows: Vec<rimz::SidebarRow>,
    hidden: usize,
    now: jiff::Timestamp,
) -> Vec<rimz::SidebarRow> {
    let visible = capped_visible_count(&rows);
    let base_hidden = rows.len().saturating_sub(visible);
    let fillers_kept = FIXTURE_WORKTREE_ROW_CAP.saturating_sub(visible);
    let filler_count = hidden.saturating_sub(base_hidden) + fillers_kept;
    let path = rows
        .first()
        .and_then(|row| row.worktree_path.clone())
        .unwrap_or_else(|| "/srv/code/query-engine".to_owned());
    let branch = rows
        .first()
        .and_then(|row| row.worktree_branch.clone())
        .unwrap_or_else(|| "main".to_owned());
    let seed = rows
        .first()
        .map(|row| {
            row.id
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect::<String>()
        })
        .unwrap_or_else(|| "group".to_owned());
    rows.extend((0..filler_count).map(|index| {
        let mut row = agent_row_with(
            &format!("agent:fixture-overflow:{seed}:{index}"),
            "codex",
            &format!("overflow_{seed}_{index}"),
            &path,
            &branch,
            rimz::agents::AgentStatus::Idle,
            rimz::agents::TurnPhase::Idle,
            "queued background follow-up",
            "GPT-5.1-Codex",
            None,
            now,
            AgentRowOptions {
                age_secs: Some(24 * 60 * 60 + i64::try_from(index).unwrap_or_default()),
                ..AgentRowOptions::default()
            },
        );
        row.inactive = true;
        row.archived = true;
        row
    }));
    rows
}

fn capped_visible_count(rows: &[rimz::SidebarRow]) -> usize {
    let process_is_only_live_member = rows.iter().map(row_band).min() == Some(0)
        && rows
            .iter()
            .filter(|row| row_band(row) == 0)
            .all(rimz::SidebarRow::is_process);
    let liveness_process_id = if process_is_only_live_member {
        rows.iter()
            .find(|row| row.is_process() && row_band(row) == 0)
            .map(|row| row.id.as_str())
    } else {
        None
    };
    let mut visible = 0;
    for row in rows {
        if row.unread
            || row
                .status()
                .is_some_and(|status| status != rimz::agents::AgentStatus::Idle)
            || row.pane.as_ref().is_some_and(|pane| pane.is_focused)
            || liveness_process_id == Some(row.id.as_str())
            || visible < FIXTURE_WORKTREE_ROW_CAP
        {
            visible += 1;
        }
    }
    visible
}

fn row_band(row: &rimz::SidebarRow) -> u8 {
    if row.archived {
        2
    } else if row.inactive {
        1
    } else {
        0
    }
}

fn move_fixture_overflow_to_tail(rows: &mut Vec<rimz::SidebarRow>) {
    let mut overflow = Vec::new();
    let mut visible = Vec::new();
    for row in std::mem::take(rows) {
        if row.id.starts_with("agent:fixture-overflow:") {
            overflow.push(row);
        } else {
            visible.push(row);
        }
    }
    visible.extend(overflow);
    *rows = visible;
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
    .collect::<Vec<_>>()
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
        day_budget: None,
        extra_credits: matches!(kind, "pi" | "opencode").then_some(rimz::ExtraCredits::Disabled),
        reset_credits: None,
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
        let sessions = ((sessions as f64 * scale).round() as u32).max(1);
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
