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
        client_views: Vec::new(),
        pane_session_name: None,
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

struct WorktreeGroupSpec {
    key: &'static str,
    label: &'static str,
    rows: Vec<rimz::SidebarRow>,
    diff_added: Option<u32>,
    diff_removed: Option<u32>,
    commits_ahead: Option<u32>,
    commits_behind: Option<u32>,
    clean: Option<bool>,
    landed: Option<bool>,
    trunk_sync: Option<rimz::WorktreeTrunkSync>,
    pr_state: Option<rimz::WorktreePrState>,
    pr_ci: Option<rimz::WorktreePrCi>,
    pr_number: Option<u64>,
}

impl Default for WorktreeGroupSpec {
    fn default() -> Self {
        Self {
            key: "",
            label: "",
            rows: Vec::new(),
            diff_added: None,
            diff_removed: None,
            commits_ahead: None,
            commits_behind: None,
            clean: Some(false),
            landed: Some(false),
            trunk_sync: None,
            pr_state: None,
            pr_ci: None,
            pr_number: None,
        }
    }
}

fn worktree_group(spec: WorktreeGroupSpec) -> rimz::SidebarWorktreeGroup {
    rimz::SidebarWorktreeGroup {
        key: spec.key.to_owned(),
        label: spec.label.to_owned(),
        kind: rimz::SidebarWorktreeKind::Worktree,
        team: None,
        status_counts: Vec::new(),
        rows: spec.rows,
        diff_added: spec.diff_added,
        diff_removed: spec.diff_removed,
        commits_ahead: spec.commits_ahead,
        commits_behind: spec.commits_behind,
        trunk: Some("main".to_owned()),
        worktree_backed: false,
        finished: false,
        clean: spec.clean,
        landed: spec.landed,
        trunk_sync: spec.trunk_sync,
        pr_state: spec.pr_state,
        pr_ci: spec.pr_ci,
        pr_number: spec.pr_number,
        pr_url: None,
    }
}

struct ProcessRowSpec {
    id: &'static str,
    name: &'static str,
    pane: &'static str,
    command: &'static str,
    cwd: &'static str,
    branch: &'static str,
    state: rimz::ProcessState,
    detail: Option<&'static str>,
    cpu_pct: Option<u16>,
    rss_kb: Option<u64>,
    io_bps: Option<u64>,
    last_activity: jiff::Timestamp,
}

fn process_row(spec: ProcessRowSpec) -> rimz::SidebarRow {
    rimz::SidebarRow {
        id: spec.id.to_owned(),
        name: spec.name.to_owned(),
        pane: Some(pane_ref(spec.pane, spec.command, spec.cwd, false)),
        worktree_path: Some(spec.cwd.to_owned()),
        worktree_branch: Some(spec.branch.to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        last_activity: spec.last_activity,
        card: rimz::RowCard::Process(rimz::ProcessCard {
            state: spec.state,
            command_detail: spec.detail.map(ToOwned::to_owned),
            cpu_pct: spec.cpu_pct,
            rss_kb: spec.rss_kb,
            io_bps: spec.io_bps,
            ..rimz::ProcessCard::default()
        }),
    }
}

fn add_fleet_fixture(snapshot: &mut rimz::SidebarSnapshot, now: jiff::Timestamp) {
    let claude = agent_row(
        AgentRowSpec {
            id: "agent:claude:auth",
            name: "claude",
            pane: "terminal_21",
            cwd: "/srv/code/query-engine",
            branch: "feature/auth-router",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "port auth watcher",
            model: "Opus 4.8",
            context: Some((341_700, 12.40)),
            ..AgentRowSpec::default()
        },
        now,
    );
    let codex = agent_row(
        AgentRowSpec {
            id: "agent:codex:pricing",
            name: "codex",
            pane: "terminal_22",
            cwd: "/srv/code/query-engine/.rimz/worktrees/pricing",
            branch: "pricing-refresh",
            status: rimz::agents::AgentStatus::Waiting,
            phase: rimz::agents::TurnPhase::Idle,
            task: "approve pricing cache write",
            model: "GPT-5.5",
            context: Some((193_480, 4.36)),
            ..AgentRowSpec::default()
        },
        now,
    );
    let pi = agent_row(
        AgentRowSpec {
            id: "agent:pi:mux",
            name: "pi",
            pane: "terminal_23",
            cwd: "/srv/code/query-engine/.rimz/worktrees/mux",
            branch: "zellij-health",
            status: rimz::agents::AgentStatus::Failed,
            phase: rimz::agents::TurnPhase::Idle,
            task: "debug zellij health probe",
            model: "GPT-5.5",
            context: Some((258_610, 2.18)),
            ..AgentRowSpec::default()
        },
        now,
    );
    let process = process_row(ProcessRowSpec {
        id: "process:cargo-nextest",
        name: "cargo nextest",
        pane: "terminal_24",
        command: "cargo nextest run",
        cwd: "/srv/code/query-engine",
        branch: "main",
        state: rimz::ProcessState::Busy,
        detail: Some("integration::backend::zellij"),
        cpu_pct: Some(37),
        rss_kb: Some(412_000),
        io_bps: None,
        last_activity: now,
    });

    snapshot.worktree_groups = vec![
        WorktreeGroupSpec {
            key: "/srv/code/query-engine",
            label: "main",
            rows: with_overflow(vec![claude, codex, process], 3, now),
            diff_added: Some(182),
            diff_removed: Some(47),
            commits_ahead: Some(3),
            commits_behind: Some(1),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/mux",
            label: "zellij-health",
            rows: vec![pi],
            diff_added: Some(14),
            diff_removed: Some(3),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            ..WorktreeGroupSpec::default()
        },
    ]
    .into_iter()
    .map(worktree_group)
    .collect();
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
    let claude = agent_row(
        AgentRowSpec {
            id: "agent:claude:main",
            name: "claude",
            pane: "terminal_31",
            cwd: "/srv/code/query-engine",
            branch: "main",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "stabilize render diff",
            model: "Opus 4.8",
            context: Some((247_310, 8.64)),
            age_secs: Some(45),
            ..AgentRowSpec::default()
        },
        now,
    );
    let opencode = agent_row(
        AgentRowSpec {
            id: "agent:opencode:main",
            name: "opencode",
            pane: "terminal_32",
            cwd: "/srv/code/query-engine",
            branch: "main",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Reasoning,
            task: "trace tmux layout parity",
            model: "GPT-5.5",
            context: Some((103_970, 3.07)),
            age_secs: Some(4 * 60),
            compaction_count: 1,
            sub_agents: Some(Vec::new()),
            ..AgentRowSpec::default()
        },
        now,
    );
    let compacting = agent_row(
        AgentRowSpec {
            id: "agent:claude:compacting",
            name: "claude",
            pane: "terminal_38",
            cwd: "/srv/code/query-engine",
            branch: "main",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Reasoning,
            task: "compact provider trace",
            model: "Opus 4.8",
            context: Some((797_420, 28.90)),
            age_secs: Some(8 * 60),
            compacting: true,
            compaction_count: 3,
            sub_agents: Some(Vec::new()),
            ..AgentRowSpec::default()
        },
        now,
    );
    let idle = agent_row(
        AgentRowSpec {
            id: "agent:pi:idle",
            name: "pi",
            pane: "terminal_39",
            cwd: "/srv/code/query-engine",
            branch: "main",
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            task: "park sidebar notes",
            model: "GPT-5.5",
            context: None,
            age_secs: Some(65 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let process = process_row(ProcessRowSpec {
        id: "process:cargo-nextest",
        name: "cargo nextest",
        pane: "terminal_33",
        command: "cargo nextest run",
        cwd: "/srv/code/query-engine",
        branch: "main",
        state: rimz::ProcessState::Busy,
        detail: Some("integration::backend"),
        cpu_pct: Some(37),
        rss_kb: Some(412_000),
        io_bps: Some(8 * 1_048_576),
        last_activity: now,
    });
    let codex = agent_row(
        AgentRowSpec {
            id: "agent:codex:pricing",
            name: "codex",
            pane: "terminal_09",
            cwd: "/srv/code/query-engine/.rimz/worktrees/pricing-refresh",
            branch: "pricing-refresh",
            status: rimz::agents::AgentStatus::Waiting,
            phase: rimz::agents::TurnPhase::Idle,
            task: "approve pricing cache write",
            model: "GPT-5.5",
            context: Some((158_210, 4.82)),
            age_secs: Some(18 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let mut pi = agent_row(
        AgentRowSpec {
            id: "agent:pi:zellij",
            name: "pi",
            pane: "terminal_35",
            cwd: "/srv/code/query-engine/.rimz/worktrees/zellij-health",
            branch: "zellij-health",
            status: rimz::agents::AgentStatus::Failed,
            phase: rimz::agents::TurnPhase::Idle,
            task: "debug zellij health probe",
            model: "GPT-5.5",
            context: Some((228_540, 5.76)),
            age_secs: Some(42 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let mut success = agent_row(
        AgentRowSpec {
            id: "agent:claude:mux-merge",
            name: "claude",
            pane: "terminal_36",
            cwd: "/srv/code/query-engine/.rimz/worktrees/mux-merge",
            branch: "mux-merge",
            status: rimz::agents::AgentStatus::Success,
            phase: rimz::agents::TurnPhase::Idle,
            task: "land tmux hook fix",
            model: "Opus 4.8",
            context: Some((132_770, 2.41)),
            age_secs: Some(2 * 60 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    success.unread = true;
    let mut paused = agent_row(
        AgentRowSpec {
            id: "agent:claude:mux-merge-paused",
            name: "claude",
            pane: "terminal_37",
            cwd: "/srv/code/query-engine/.rimz/worktrees/mux-merge",
            branch: "mux-merge",
            status: rimz::agents::AgentStatus::Paused,
            phase: rimz::agents::TurnPhase::Idle,
            task: "holding the tmux hook fix mid-land",
            model: "Opus 4.8",
            context: Some((469_180, 15.38)),
            age_secs: Some(75 * 60),
            turn_error_label: Some("API Error: Overloaded".to_owned()),
            ..AgentRowSpec::default()
        },
        now,
    );
    paused.unread = true;
    pi.unread = true;
    let mut opencode_theme = agent_row(
        AgentRowSpec {
            id: "agent:opencode:theme",
            name: "opencode",
            pane: "terminal_40",
            cwd: "/srv/code/query-engine/.rimz/worktrees/theme-tune",
            branch: "theme-tune",
            status: rimz::agents::AgentStatus::Waiting,
            phase: rimz::agents::TurnPhase::Idle,
            task: "pick gallery color ramp",
            model: "GPT-5.5",
            context: Some((42_130, 0.68)),
            age_secs: Some(60 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let claude_theme = agent_row(
        AgentRowSpec {
            id: "agent:claude:theme",
            name: "claude",
            pane: "terminal_46",
            cwd: "/srv/code/query-engine/.rimz/worktrees/theme-tune",
            branch: "theme-tune",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "verify 256-color fallback",
            model: "Opus 4.8",
            context: Some((314_260, 6.95)),
            age_secs: Some(6 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let pi_observer = agent_row(
        AgentRowSpec {
            id: "agent:claude:observer",
            name: "claude",
            pane: "terminal_47",
            cwd: "/srv/code/query-engine/.rimz/worktrees/observer-lag",
            branch: "observer-lag",
            status: rimz::agents::AgentStatus::Paused,
            phase: rimz::agents::TurnPhase::Idle,
            task: "frozen on the stale-pane triage",
            model: "Opus 4.8",
            context: Some((853_690, 29.84)),
            age_secs: Some(93 * 60),
            turn_error_label: Some("API Error: Overloaded".to_owned()),
            ..AgentRowSpec::default()
        },
        now,
    );
    opencode_theme.unread = true;
    let codex_observer = agent_row(
        AgentRowSpec {
            id: "agent:codex:observer",
            name: "codex",
            pane: "terminal_48",
            cwd: "/srv/code/query-engine/.rimz/worktrees/observer-lag",
            branch: "observer-lag",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "triage stale pane sample",
            model: "GPT-5.5",
            context: Some((185_730, 3.83)),
            age_secs: Some(54 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let claude_budget_idle = agent_row(
        AgentRowSpec {
            id: "agent:claude:budget-idle",
            name: "claude",
            pane: "terminal_49",
            cwd: "/srv/code/query-engine/.rimz/worktrees/observer-lag",
            branch: "observer-lag",
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            task: "hold perf notes",
            model: "Opus 4.8",
            context: None,
            age_secs: Some(3 * 60 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    snapshot.worktree_groups = vec![
        WorktreeGroupSpec {
            key: "/srv/code/query-engine",
            label: "main",
            rows: vec![claude, compacting, process],
            diff_added: Some(1840),
            diff_removed: Some(620),
            commits_ahead: Some(3),
            commits_behind: Some(1),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/pricing-refresh",
            label: "pricing-refresh",
            rows: vec![codex],
            diff_added: Some(1180),
            diff_removed: Some(430),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_ci: Some(rimz::WorktreePrCi::Pending),
            pr_number: Some(91),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/zellij-health",
            label: "zellij-health",
            rows: vec![pi],
            diff_added: Some(760),
            diff_removed: Some(210),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/mux-merge",
            label: "mux-merge",
            rows: vec![success, paused],
            diff_added: Some(0),
            diff_removed: Some(0),
            commits_ahead: Some(0),
            commits_behind: Some(0),
            clean: Some(true),
            landed: Some(true),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Merged),
            pr_state: Some(rimz::WorktreePrState::Merged),
            pr_ci: None,
            pr_number: Some(91),
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/theme-tune",
            label: "theme-tune",
            rows: vec![opencode_theme, claude_theme],
            diff_added: Some(1320),
            diff_removed: Some(540),
            commits_ahead: Some(2),
            commits_behind: Some(1),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Reconciling),
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_ci: Some(rimz::WorktreePrCi::Failing),
            pr_number: Some(91),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/observer-lag",
            label: "observer-lag",
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
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            ..WorktreeGroupSpec::default()
        },
    ]
    .into_iter()
    .map(worktree_group)
    .collect();
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
        expiries: Vec::new(),
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
    let coder = agent_row(
        AgentRowSpec {
            id: "agent:codex:coder",
            name: "codex",
            pane: "terminal_41",
            cwd: "/srv/code/query-engine/.rimz/worktrees/auth-router",
            branch: "feature/auth-router",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "wire auth router migration",
            model: "GPT-5.5",
            context: Some((207_110, 11.84)),
            handle: Some("coder".to_owned()),
            launch_group: Some("auth-router-team".to_owned()),
            launch_ordinal: Some(1),
            sub_agents: Some(vec![
                sub_agent(
                    SubAgentSpec {
                        id: "child:general:test",
                        name: "general-purpose",
                        status: rimz::agents::AgentStatus::Running,
                        phase: rimz::agents::TurnPhase::Acting,
                        task: Some("run focused nextest"),
                        model: Some("Sonnet 4.6"),
                        description: None,
                        total_tokens: Some(23_500),
                        elapsed_secs: Some(210),
                    },
                    now,
                ),
                sub_agent(
                    SubAgentSpec {
                        id: "child:general:review",
                        name: "general-purpose",
                        status: rimz::agents::AgentStatus::Success,
                        phase: rimz::agents::TurnPhase::Idle,
                        task: Some("review migration contract"),
                        model: Some("Sonnet 4.6"),
                        description: None,
                        total_tokens: Some(14_800),
                        elapsed_secs: Some(260),
                    },
                    now,
                ),
            ]),
            age_secs: Some(70),
            compaction_count: 2,
            ..AgentRowSpec::default()
        },
        now,
    );
    let planner = agent_row(
        AgentRowSpec {
            id: "agent:claude:planner",
            name: "claude",
            pane: "terminal_42",
            cwd: "/srv/code/query-engine/.rimz/worktrees/auth-router",
            branch: "feature/auth-router",
            status: rimz::agents::AgentStatus::Success,
            phase: rimz::agents::TurnPhase::Idle,
            task: "plan router cutover",
            model: "Opus 4.8",
            context: Some((183_450, 14.72)),
            handle: Some("planner".to_owned()),
            launch_group: Some("auth-router-team".to_owned()),
            launch_ordinal: Some(0),
            age_secs: Some(22 * 60),
            sub_agents: Some(vec![
                sub_agent(
                    SubAgentSpec {
                        id: "child:explore:routes",
                        name: "Explore",
                        status: rimz::agents::AgentStatus::Success,
                        phase: rimz::agents::TurnPhase::Idle,
                        task: Some("map route ownership"),
                        model: Some("Haiku"),
                        description: Some("trace handler graph"),
                        total_tokens: Some(20_100),
                        elapsed_secs: Some(420),
                    },
                    now,
                ),
                sub_agent(
                    SubAgentSpec {
                        id: "child:explore:middleware",
                        name: "Explore",
                        status: rimz::agents::AgentStatus::Success,
                        phase: rimz::agents::TurnPhase::Idle,
                        task: Some("prove middleware order"),
                        model: Some("Haiku"),
                        description: Some("exercise auth edge cases"),
                        total_tokens: Some(18_700),
                        elapsed_secs: Some(390),
                    },
                    now,
                ),
                sub_agent(
                    SubAgentSpec {
                        id: "child:explore:docs",
                        name: "Explore",
                        status: rimz::agents::AgentStatus::Success,
                        phase: rimz::agents::TurnPhase::Idle,
                        task: Some("summarize docs drift"),
                        model: Some("Haiku"),
                        description: None,
                        total_tokens: Some(11_400),
                        elapsed_secs: Some(360),
                    },
                    now,
                ),
                sub_agent(
                    SubAgentSpec {
                        id: "child:plan:rollout",
                        name: "Plan",
                        status: rimz::agents::AgentStatus::Success,
                        phase: rimz::agents::TurnPhase::Idle,
                        task: Some("sequence rollout guard"),
                        model: Some("Haiku"),
                        description: Some("write migration runbook"),
                        total_tokens: Some(9_900),
                        elapsed_secs: Some(300),
                    },
                    now,
                ),
                sub_agent(
                    SubAgentSpec {
                        id: "child:plan:review",
                        name: "Plan",
                        status: rimz::agents::AgentStatus::Success,
                        phase: rimz::agents::TurnPhase::Idle,
                        task: Some("stage review gates"),
                        model: Some("Haiku"),
                        description: Some("split blocking checks"),
                        total_tokens: Some(8_700),
                        elapsed_secs: Some(240),
                    },
                    now,
                ),
            ]),
            ..AgentRowSpec::default()
        },
        now,
    );
    let reviewer = agent_row(
        AgentRowSpec {
            id: "agent:pi:reviewer",
            name: "pi",
            pane: "terminal_43",
            cwd: "/srv/code/query-engine/.rimz/worktrees/auth-router",
            branch: "feature/auth-router",
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            task: "review auth router diff",
            model: "GPT-5.5",
            context: None,
            handle: Some("reviewer".to_owned()),
            launch_group: Some("auth-router-team".to_owned()),
            launch_ordinal: Some(2),
            age_secs: Some(9 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let rollout_coder = agent_row(
        AgentRowSpec {
            id: "agent:codex:rollout-coder",
            name: "codex",
            pane: "terminal_44",
            cwd: "/srv/code/query-engine/.rimz/worktrees/rollout-guard",
            branch: "rollout-guard",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "patch rollout smoke harness",
            model: "GPT-5.5",
            context: Some((142_260, 2.93)),
            handle: Some("coder".to_owned()),
            age_secs: Some(5 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let mut rollout_reviewer = agent_row(
        AgentRowSpec {
            id: "agent:claude:rollout-reviewer",
            name: "claude",
            pane: "terminal_45",
            cwd: "/srv/code/query-engine/.rimz/worktrees/rollout-guard",
            branch: "rollout-guard",
            status: rimz::agents::AgentStatus::Waiting,
            phase: rimz::agents::TurnPhase::Idle,
            task: "approve staged migration",
            model: "Opus 4.8",
            context: Some((347_890, 9.04)),
            handle: Some("reviewer".to_owned()),
            age_secs: Some(52 * 60),
            compaction_count: 1,
            ..AgentRowSpec::default()
        },
        now,
    );
    rollout_reviewer.unread = true;
    let mut ci_failed = agent_row(
        AgentRowSpec {
            id: "agent:codex:ci",
            name: "codex",
            pane: "terminal_48",
            cwd: "/srv/code/query-engine/.rimz/worktrees/ci-retry",
            branch: "ci-retry",
            status: rimz::agents::AgentStatus::Failed,
            phase: rimz::agents::TurnPhase::Idle,
            task: "fix flaky hook replay",
            model: "GPT-5.5",
            context: Some((167_440, 4.58)),
            age_secs: Some(44 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    ci_failed.unread = true;
    let pi_paused = agent_row(
        AgentRowSpec {
            id: "agent:claude:ci-paused",
            name: "claude",
            pane: "terminal_49",
            cwd: "/srv/code/query-engine/.rimz/worktrees/ci-retry",
            branch: "ci-retry",
            status: rimz::agents::AgentStatus::Paused,
            phase: rimz::agents::TurnPhase::Idle,
            task: "parked mid hook-replay repair",
            model: "Opus 4.8",
            context: Some((823_560, 26.75)),
            age_secs: Some(86 * 60),
            turn_error_label: Some("API Error: Overloaded".to_owned()),
            ..AgentRowSpec::default()
        },
        now,
    );
    let architect_tokens = agent_row(
        AgentRowSpec {
            id: "agent:claude:token-budget",
            name: "claude",
            pane: "terminal_50",
            cwd: "/srv/code/query-engine/.rimz/worktrees/token-budget",
            branch: "token-budget",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "model OAuth token budget",
            model: "Opus 4.8",
            context: Some((74_330, 1.26)),
            handle: Some("architect".to_owned()),
            age_secs: Some(12 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let opencode_tokens = agent_row(
        AgentRowSpec {
            id: "agent:opencode:token-budget",
            name: "opencode",
            pane: "terminal_56",
            cwd: "/srv/code/query-engine/.rimz/worktrees/token-budget",
            branch: "token-budget",
            status: rimz::agents::AgentStatus::Success,
            phase: rimz::agents::TurnPhase::Idle,
            task: "land budget doc example",
            model: "GPT-5.5",
            context: Some((33_210, 0.54)),
            handle: Some("developer".to_owned()),
            age_secs: Some(49 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let sre_tokens = agent_row(
        AgentRowSpec {
            id: "agent:codex:token-budget",
            name: "codex",
            pane: "terminal_57",
            cwd: "/srv/code/query-engine/.rimz/worktrees/token-budget",
            branch: "token-budget",
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            task: "wait for spend review",
            model: "GPT-5.5",
            context: None,
            handle: Some("sre".to_owned()),
            age_secs: Some(104 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    snapshot.worktree_groups = vec![
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/auth-router",
            label: "feature/auth-router",
            rows: vec![planner, coder, reviewer],
            diff_added: Some(1520),
            diff_removed: Some(470),
            commits_ahead: Some(2),
            commits_behind: Some(0),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/rollout-guard",
            label: "rollout-guard",
            rows: vec![rollout_coder, rollout_reviewer],
            diff_added: Some(1180),
            diff_removed: Some(390),
            commits_ahead: Some(2),
            commits_behind: Some(0),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_ci: Some(rimz::WorktreePrCi::Passing),
            pr_number: Some(91),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/ci-retry",
            label: "ci-retry",
            rows: vec![ci_failed, pi_paused],
            diff_added: Some(880),
            diff_removed: Some(240),
            commits_ahead: Some(1),
            commits_behind: Some(1),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Reconciling),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/token-budget",
            label: "token-budget",
            rows: vec![architect_tokens, opencode_tokens, sre_tokens],
            diff_added: Some(1420),
            diff_removed: Some(520),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_ci: Some(rimz::WorktreePrCi::Pending),
            pr_number: Some(91),
            ..WorktreeGroupSpec::default()
        },
    ]
    .into_iter()
    .map(worktree_group)
    .collect();
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
        expiries: Vec::new(),
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
    let mut opencode = agent_row(
        AgentRowSpec {
            id: "agent:opencode:economy",
            name: "opencode",
            pane: "terminal_51",
            cwd: "/srv/code/query-engine",
            branch: "main",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "rebalance metered sessions",
            model: "GPT-5.5",
            context: Some((175_350, 7.44)),
            age_secs: Some(55),
            ..AgentRowSpec::default()
        },
        now,
    );
    opencode.unread = true;
    let mut claude = agent_row(
        AgentRowSpec {
            id: "agent:claude:economy",
            name: "claude",
            pane: "terminal_52",
            cwd: "/srv/code/query-engine",
            branch: "main",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Reasoning,
            task: "audit provider spend",
            model: "Opus 4.8",
            context: Some((296_430, 12.62)),
            age_secs: Some(6 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    claude.unread = true;
    let pnpm = process_row(ProcessRowSpec {
        id: "process:pnpm-serve",
        name: "pnpm serve",
        pane: "terminal_30",
        command: "pnpm serve",
        cwd: "/srv/code/query-engine",
        branch: "main",
        state: rimz::ProcessState::Busy,
        detail: Some("vite dev :5173"),
        cpu_pct: Some(12),
        rss_kb: Some(268_000),
        io_bps: Some(450 * 1_024),
        last_activity: now,
    });
    let codex = agent_row(
        AgentRowSpec {
            id: "agent:codex:economy",
            name: "codex",
            pane: "terminal_53",
            cwd: "/srv/code/query-engine/.rimz/worktrees/pricing-refresh",
            branch: "pricing-refresh",
            status: rimz::agents::AgentStatus::Waiting,
            phase: rimz::agents::TurnPhase::Idle,
            task: "approve price snapshot",
            model: "GPT-5.5",
            context: Some((118_620, 3.29)),
            age_secs: Some(14 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let mut pi = agent_row(
        AgentRowSpec {
            id: "agent:pi:budget",
            name: "pi",
            pane: "terminal_54",
            cwd: "/srv/code/query-engine/.rimz/worktrees/cost-caps",
            branch: "cost-caps",
            status: rimz::agents::AgentStatus::Success,
            phase: rimz::agents::TurnPhase::Idle,
            task: "publish budget notes",
            model: "GPT-5.5",
            context: Some((96_840, 1.74)),
            age_secs: Some(31 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    pi.unread = true;
    let codex_idle = agent_row(
        AgentRowSpec {
            id: "agent:codex:budget",
            name: "codex",
            pane: "terminal_55",
            cwd: "/srv/code/query-engine/.rimz/worktrees/cost-caps",
            branch: "cost-caps",
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            task: "hold weekly spend cap",
            model: "GPT-5.5",
            context: None,
            age_secs: Some(83 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let opencode_limit = agent_row(
        AgentRowSpec {
            id: "agent:opencode:limit",
            name: "opencode",
            pane: "terminal_56",
            cwd: "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
            branch: "usage-alerts",
            status: rimz::agents::AgentStatus::Waiting,
            phase: rimz::agents::TurnPhase::Idle,
            task: "approve burst throttle",
            model: "GPT-5.5",
            context: Some((216_700, 5.18)),
            age_secs: Some(19 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let mut pi_limit = agent_row(
        AgentRowSpec {
            id: "agent:claude:limit-paused",
            name: "claude",
            pane: "terminal_57",
            cwd: "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
            branch: "usage-alerts",
            status: rimz::agents::AgentStatus::Paused,
            phase: rimz::agents::TurnPhase::Idle,
            task: "staged the budget-alert fix, idling",
            model: "Opus 4.8",
            context: Some((856_120, 29.36)),
            age_secs: Some(2 * 60 * 60),
            turn_error_label: Some("API Error: rate limit exceeded".to_owned()),
            ..AgentRowSpec::default()
        },
        now,
    );
    pi_limit.unread = true;
    let claude_limit = agent_row(
        AgentRowSpec {
            id: "agent:claude:limit",
            name: "claude",
            pane: "terminal_58",
            cwd: "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
            branch: "usage-alerts",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "repair budget alert webhook",
            model: "Opus 4.8",
            context: Some((372_640, 8.91)),
            age_secs: Some(48 * 60),
            compacting: true,
            ..AgentRowSpec::default()
        },
        now,
    );
    let codex_credit = agent_row(
        AgentRowSpec {
            id: "agent:codex:credits",
            name: "codex",
            pane: "terminal_59",
            cwd: "/srv/code/query-engine/.rimz/worktrees/credit-store",
            branch: "credit-store",
            status: rimz::agents::AgentStatus::Success,
            phase: rimz::agents::TurnPhase::Idle,
            task: "fold extra-credit API",
            model: "GPT-5.5",
            context: Some((158_930, 2.67)),
            age_secs: Some(7 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let opencode_credit = agent_row(
        AgentRowSpec {
            id: "agent:opencode:credits",
            name: "opencode",
            pane: "terminal_60",
            cwd: "/srv/code/query-engine/.rimz/worktrees/credit-store",
            branch: "credit-store",
            status: rimz::agents::AgentStatus::Success,
            phase: rimz::agents::TurnPhase::Idle,
            task: "land credit store docs",
            model: "GPT-5.5",
            context: Some((29_250, 0.42)),
            age_secs: Some(55 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let mut claude_credit = agent_row(
        AgentRowSpec {
            id: "agent:claude:credits",
            name: "claude",
            pane: "terminal_66",
            cwd: "/srv/code/query-engine/.rimz/worktrees/credit-store",
            branch: "credit-store",
            status: rimz::agents::AgentStatus::Success,
            phase: rimz::agents::TurnPhase::Idle,
            task: "summarize metered tradeoffs",
            model: "Opus 4.8",
            context: Some((165_880, 4.08)),
            age_secs: Some(71 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    claude_credit.unread = true;
    let pi_credit_idle = agent_row(
        AgentRowSpec {
            id: "agent:pi:credits",
            name: "pi",
            pane: "terminal_67",
            cwd: "/srv/code/query-engine/.rimz/worktrees/credit-store",
            branch: "credit-store",
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            task: "watch credit refill",
            model: "GPT-5.5",
            context: None,
            age_secs: Some(3 * 60 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    snapshot.worktree_groups = vec![
        WorktreeGroupSpec {
            key: "/srv/code/query-engine",
            label: "provider-store",
            rows: vec![opencode, claude, pnpm],
            diff_added: Some(1580),
            diff_removed: Some(520),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_ci: Some(rimz::WorktreePrCi::Passing),
            pr_number: Some(91),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/pricing-refresh",
            label: "pricing-refresh",
            rows: vec![codex],
            diff_added: Some(700),
            diff_removed: Some(210),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/cost-caps",
            label: "cost-caps",
            rows: vec![pi, codex_idle],
            diff_added: Some(1120),
            diff_removed: Some(360),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_ci: Some(rimz::WorktreePrCi::Pending),
            pr_number: Some(91),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/usage-alerts",
            label: "usage-alerts",
            rows: vec![opencode_limit, pi_limit, claude_limit],
            diff_added: Some(1980),
            diff_removed: Some(740),
            commits_ahead: Some(2),
            commits_behind: Some(1),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/credit-store",
            label: "credit-store",
            rows: vec![codex_credit, opencode_credit, claude_credit, pi_credit_idle],
            diff_added: Some(0),
            diff_removed: Some(0),
            commits_ahead: Some(0),
            commits_behind: Some(0),
            clean: Some(true),
            landed: Some(true),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Merged),
            pr_state: Some(rimz::WorktreePrState::Merged),
            pr_ci: None,
            pr_number: Some(91),
        },
    ]
    .into_iter()
    .map(worktree_group)
    .collect();
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
    let mut claude = agent_row(
        AgentRowSpec {
            id: "agent:claude:reach",
            name: "claude",
            pane: "terminal_61",
            cwd: "/srv/code/query-engine",
            branch: "main",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "verify remote attach",
            model: "Opus 4.8",
            context: Some((333_710, 13.56)),
            age_secs: Some(35),
            compaction_count: 2,
            ..AgentRowSpec::default()
        },
        now,
    );
    claude.unread = true;
    let mut codex = agent_row(
        AgentRowSpec {
            id: "agent:codex:reach",
            name: "codex",
            pane: "terminal_62",
            cwd: "/srv/code/query-engine/.rimz/worktrees/remote-link",
            branch: "remote-link",
            status: rimz::agents::AgentStatus::Waiting,
            phase: rimz::agents::TurnPhase::Idle,
            task: "approve SSH link retry",
            model: "GPT-5.5",
            context: Some((153_560, 3.71)),
            age_secs: Some(11 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    codex.unread = true;
    let pi = agent_row(
        AgentRowSpec {
            id: "agent:pi:reach",
            name: "pi",
            pane: "terminal_63",
            cwd: "/srv/code/query-engine/.rimz/worktrees/remote-link",
            branch: "remote-link",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Reasoning,
            task: "trace ControlMaster jitter",
            model: "GPT-5.5",
            context: Some((185_910, 4.44)),
            age_secs: Some(19 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let mut opencode = agent_row(
        AgentRowSpec {
            id: "agent:opencode:edge-cache",
            name: "opencode",
            pane: "terminal_67",
            cwd: "/srv/code/query-engine/.rimz/worktrees/edge-cache",
            branch: "edge-cache",
            status: rimz::agents::AgentStatus::Success,
            phase: rimz::agents::TurnPhase::Idle,
            task: "land offline cache probe",
            model: "GPT-5.5",
            context: Some((61_170, 0.91)),
            age_secs: Some(46 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    opencode.unread = true;
    let claude_idle = agent_row(
        AgentRowSpec {
            id: "agent:claude:edge-cache",
            name: "claude",
            pane: "terminal_68",
            cwd: "/srv/code/query-engine/.rimz/worktrees/edge-cache",
            branch: "edge-cache",
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            task: "wait for laptop reconnect",
            model: "Opus 4.8",
            context: None,
            age_secs: Some(94 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let pi_netcheck = agent_row(
        AgentRowSpec {
            id: "agent:pi:netcheck",
            name: "pi",
            pane: "terminal_64",
            cwd: "/srv/code/query-engine/.rimz/worktrees/network-check",
            branch: "network-check",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "debug jump-host DNS",
            model: "GPT-5.5",
            context: Some((251_620, 6.24)),
            age_secs: Some(51 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let claude_netcheck_paused = agent_row(
        AgentRowSpec {
            id: "agent:claude:netcheck-paused",
            name: "claude",
            pane: "terminal_65",
            cwd: "/srv/code/query-engine/.rimz/worktrees/network-check",
            branch: "network-check",
            status: rimz::agents::AgentStatus::Paused,
            phase: rimz::agents::TurnPhase::Idle,
            task: "halfway through the jump-host DNS fix",
            model: "Opus 4.8",
            context: Some((414_390, 10.76)),
            age_secs: Some(73 * 60),
            turn_error_label: Some("API Error: rate limit exceeded".to_owned()),
            ..AgentRowSpec::default()
        },
        now,
    );
    let mut claude_netcheck = agent_row(
        AgentRowSpec {
            id: "agent:claude:netcheck",
            name: "claude",
            pane: "terminal_66",
            cwd: "/srv/code/query-engine/.rimz/worktrees/network-check",
            branch: "network-check",
            status: rimz::agents::AgentStatus::Waiting,
            phase: rimz::agents::TurnPhase::Idle,
            task: "approve firewall rule change",
            model: "Opus 4.8",
            context: Some((236_510, 6.88)),
            age_secs: Some(24 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    claude_netcheck.unread = true;
    let codex_web = agent_row(
        AgentRowSpec {
            id: "agent:codex:web",
            name: "codex",
            pane: "terminal_69",
            cwd: "/srv/code/query-engine/.rimz/worktrees/browser-reach",
            branch: "browser-reach",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Acting,
            task: "exercise browser handoff",
            model: "GPT-5.5",
            context: Some((129_080, 2.36)),
            age_secs: Some(8 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let mut pi_web = agent_row(
        AgentRowSpec {
            id: "agent:pi:web",
            name: "pi",
            pane: "terminal_70",
            cwd: "/srv/code/query-engine/.rimz/worktrees/browser-reach",
            branch: "browser-reach",
            status: rimz::agents::AgentStatus::Success,
            phase: rimz::agents::TurnPhase::Idle,
            task: "document OAuth browser path",
            model: "GPT-5.5",
            context: Some((50_190, 0.73)),
            age_secs: Some(64 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    pi_web.unread = true;
    let opencode_stats = agent_row(
        AgentRowSpec {
            id: "agent:opencode:stats",
            name: "opencode",
            pane: "terminal_71",
            cwd: "/srv/code/query-engine/.rimz/worktrees/stats-relay",
            branch: "stats-relay",
            status: rimz::agents::AgentStatus::Running,
            phase: rimz::agents::TurnPhase::Reasoning,
            task: "profile stats relay latency",
            model: "GPT-5.5",
            context: Some((85_470, 1.58)),
            age_secs: Some(13 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    let claude_stats_idle = agent_row(
        AgentRowSpec {
            id: "agent:claude:stats",
            name: "claude",
            pane: "terminal_72",
            cwd: "/srv/code/query-engine/.rimz/worktrees/stats-relay",
            branch: "stats-relay",
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            task: "hold remote stats notes",
            model: "Opus 4.8",
            context: None,
            age_secs: Some(2 * 60 * 60),
            ..AgentRowSpec::default()
        },
        now,
    );
    snapshot.worktree_groups = vec![
        WorktreeGroupSpec {
            key: "/srv/code/query-engine",
            label: "main",
            rows: vec![claude],
            diff_added: Some(720),
            diff_removed: Some(160),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/remote-link",
            label: "remote-link",
            rows: vec![codex, pi],
            diff_added: Some(1240),
            diff_removed: Some(380),
            commits_ahead: Some(1),
            commits_behind: Some(1),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/edge-cache",
            label: "edge-cache",
            rows: vec![opencode, claude_idle],
            diff_added: Some(560),
            diff_removed: Some(140),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            pr_state: Some(rimz::WorktreePrState::Open),
            pr_ci: Some(rimz::WorktreePrCi::Passing),
            pr_number: Some(91),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/network-check",
            label: "network-check",
            rows: vec![pi_netcheck, claude_netcheck_paused, claude_netcheck],
            diff_added: Some(1680),
            diff_removed: Some(610),
            commits_ahead: Some(1),
            commits_behind: Some(1),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/browser-reach",
            label: "browser-reach",
            rows: vec![codex_web, pi_web],
            diff_added: Some(940),
            diff_removed: Some(280),
            commits_ahead: Some(1),
            commits_behind: Some(0),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Diverged),
            pr_state: Some(rimz::WorktreePrState::Closed),
            pr_number: Some(91),
            ..WorktreeGroupSpec::default()
        },
        WorktreeGroupSpec {
            key: "/srv/code/query-engine/.rimz/worktrees/stats-relay",
            label: "stats-relay",
            rows: vec![opencode_stats, claude_stats_idle],
            diff_added: Some(0),
            diff_removed: Some(0),
            commits_ahead: Some(0),
            commits_behind: Some(0),
            clean: Some(true),
            trunk_sync: Some(rimz::WorktreeTrunkSync::Pristine),
            ..WorktreeGroupSpec::default()
        },
    ]
    .into_iter()
    .map(worktree_group)
    .collect();
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

struct AgentRowSpec<'a> {
    id: &'a str,
    name: &'a str,
    pane: &'a str,
    cwd: &'a str,
    branch: &'a str,
    status: rimz::agents::AgentStatus,
    phase: rimz::agents::TurnPhase,
    task: &'a str,
    model: &'a str,
    /// Live context-window tokens for the bar/breakdown and cumulative session
    /// cost in USD. Fill is a snapshot; cost is independent lifetime spend.
    context: Option<(u64, f64)>,
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

impl Default for AgentRowSpec<'_> {
    fn default() -> Self {
        Self {
            id: "",
            name: "",
            pane: "",
            cwd: "",
            branch: "",
            status: rimz::agents::AgentStatus::Idle,
            phase: rimz::agents::TurnPhase::Idle,
            task: "",
            model: "",
            context: None,
            handle: None,
            launch_group: None,
            launch_ordinal: None,
            sub_agents: None,
            age_secs: None,
            account_sub_provider: None,
            turn_error_label: None,
            compacting: false,
            compaction_count: 0,
        }
    }
}

fn agent_row(spec: AgentRowSpec<'_>, now: jiff::Timestamp) -> rimz::SidebarRow {
    let is_idle = spec.status == rimz::agents::AgentStatus::Idle;
    let (context_pct, context_window, total_tokens, cost_usd) = if is_idle {
        (None, None, None, None)
    } else {
        spec.context
            .map_or((None, None, None, None), |(total, cost)| {
                let window = context_window_for_kind(spec.name);
                let used = total.min(window - 1);
                let pct = used.saturating_mul(100).div_euclid(window).min(100) as u8;
                (Some(pct), Some(window), Some(used), Some(cost))
            })
    };
    let activity_at = spec.age_secs.map_or(now, |secs| {
        now - std::time::Duration::from_secs(secs.max(0) as u64)
    });
    let account_sub_provider = spec
        .account_sub_provider
        .or_else(|| openai_sub_provider(spec.name));
    // Cache-heavy, realistic split: the first three fields sum to the window fill,
    // with cache_read taking the remainder for an exact sum; output rides separately
    // and lands in the window next turn. Fresh input is a small slice of the cached
    // context. Only Claude reports explicit cache-creation tokens — GPT/Codex caching
    // is implicit, so those cards carry none.
    let split = total_tokens.map(|tokens| {
        let fresh_input = tokens / 25;
        let cache_write = if spec.name == "claude" {
            tokens / 10
        } else {
            0
        };
        let cache_read = tokens - cache_write - fresh_input;
        let output = tokens / 8;
        (cache_read, cache_write, fresh_input, output)
    });
    let mut card = rimz::AgentCard {
        status: spec.status,
        phase: spec.phase,
        task: (!is_idle).then(|| spec.task.to_owned()),
        model: Some(spec.model.to_owned()),
        effort: Some("xhigh".to_owned()),
        handle: spec.handle,
        team: None,
        launch_group: spec.launch_group,
        launch_ordinal: spec.launch_ordinal,

        usage: rimz::agents::AgentUsageSummary {
            context_pct,
            context_window,
            total_tokens,
            cache_read_input_tokens: split.map(|tokens| tokens.0),
            cache_write_input_tokens: split.map(|tokens| tokens.1),
            fresh_input_tokens: split.map(|tokens| tokens.2),
            output_tokens: split.map(|tokens| tokens.3),
        },
        context_severity: context_pct.map(|pct| {
            rimz::agents::ContextSeverity::classify(
                pct,
                total_tokens,
                &rimz::config::ContextMeterConfig::default(),
            )
        }),
        registered_at: Some(activity_at),
        compacting: spec.compacting,
        compaction_count: spec.compaction_count,
        ..rimz::AgentCard::default()
    };
    if !is_idle && (cost_usd.is_some() || account_sub_provider.is_some()) {
        card.context = Some(agent_context(
            spec.name,
            now,
            cost_usd,
            account_sub_provider,
        ));
    }
    card.turn_error_label = spec.turn_error_label.or_else(|| {
        (spec.status == rimz::agents::AgentStatus::Failed).then(|| "API error".to_owned())
    });
    match spec.sub_agents {
        Some(sub_agents) => card.sub_agents = sub_agents,
        None if spec.status == rimz::agents::AgentStatus::Running => {
            card.sub_agents = default_sub_agents(now);
        }
        None => {}
    }

    rimz::SidebarRow {
        id: spec.id.to_owned(),
        name: spec.name.to_owned(),
        pane: Some(pane_ref(
            spec.pane,
            spec.name,
            spec.cwd,
            spec.status.is_attention(),
        )),
        worktree_path: Some(spec.cwd.to_owned()),
        worktree_branch: Some(spec.branch.to_owned()),
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
        cost: cost_usd.map(|usd| rimz::agents::AgentCost {
            total_cost_usd: Some(usd),
            ..rimz::agents::AgentCost::default()
        }),
        account: sub_provider.map(|provider| rimz::agents::AgentAccount {
            sub_provider: Some(provider.to_owned()),
            plan: Some("OpenAI OAuth".to_owned()),
            metered: Some(false),
            ..rimz::agents::AgentAccount::default()
        }),
        ..rimz::agents::AgentContext::new(source, now)
    }
}

fn default_sub_agents(now: jiff::Timestamp) -> Vec<rimz::SidebarSubAgent> {
    vec![
        sub_agent(
            SubAgentSpec {
                id: "child:explore",
                name: "Explore",
                status: rimz::agents::AgentStatus::Success,
                phase: rimz::agents::TurnPhase::Idle,
                task: Some("check unsafe edges"),
                model: Some("Haiku"),
                description: Some("audit auth watcher changes"),
                total_tokens: Some(22_400),
                elapsed_secs: Some(320),
            },
            now,
        ),
        sub_agent(
            SubAgentSpec {
                id: "child:plan",
                name: "Plan",
                status: rimz::agents::AgentStatus::Running,
                phase: rimz::agents::TurnPhase::Reasoning,
                task: Some("run focused nextest"),
                model: Some("Haiku"),
                description: None,
                total_tokens: Some(18_900),
                elapsed_secs: Some(180),
            },
            now,
        ),
    ]
}

struct SubAgentSpec<'a> {
    id: &'a str,
    name: &'a str,
    status: rimz::agents::AgentStatus,
    phase: rimz::agents::TurnPhase,
    task: Option<&'a str>,
    model: Option<&'a str>,
    description: Option<&'a str>,
    total_tokens: Option<u64>,
    elapsed_secs: Option<i64>,
}

fn sub_agent(spec: SubAgentSpec<'_>, now: jiff::Timestamp) -> rimz::SidebarSubAgent {
    let registered_at = spec.elapsed_secs.map_or(now, |secs| {
        now - std::time::Duration::from_secs(secs.max(0) as u64)
    });
    rimz::SidebarSubAgent {
        id: spec.id.to_owned(),
        name: spec.name.to_owned(),
        status: spec.status,
        phase: spec.phase,
        task: spec.task.map(ToOwned::to_owned),
        model: spec.model.map(ToOwned::to_owned),
        effort: Some("xhigh".to_owned()),
        description: spec.description.map(ToOwned::to_owned),
        total_tokens: spec.total_tokens,
        elapsed_secs: spec.elapsed_secs,
        started_at: Some(registered_at),
        last_activity: now,
        registered_at: Some(registered_at),
    }
}

fn pane_ref(raw: &str, command: &str, cwd: &str, focused: bool) -> rimz::pane::PaneRef {
    let _ = focused;
    rimz::pane::PaneRef {
        pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, raw),
        session_name: "rimz-fixture".to_owned(),
        view_id: Some("tab_0".to_owned()),
        view_kind: Some(rimz::ViewKind::Tab),
        view_name: Some("main".to_owned()),
        title: None,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
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

fn with_overflow(
    mut rows: Vec<rimz::SidebarRow>,
    hidden: usize,
    now: jiff::Timestamp,
) -> Vec<rimz::SidebarRow> {
    let visible = rimz::sidebar_pane::view::capped_visible_rows(&rows, None).len();
    let base_hidden = rows.len().saturating_sub(visible);
    let fillers_kept = rimz::sidebar_pane::view::WORKTREE_ROW_CAP.saturating_sub(visible);
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
        let mut row = agent_row(
            AgentRowSpec {
                id: &format!("agent:fixture-overflow:{seed}:{index}"),
                name: "codex",
                pane: &format!("overflow_{seed}_{index}"),
                cwd: &path,
                branch: &branch,
                status: rimz::agents::AgentStatus::Idle,
                phase: rimz::agents::TurnPhase::Idle,
                task: "queued background follow-up",
                model: "GPT-5.1-Codex",
                context: None,
                age_secs: Some(24 * 60 * 60 + i64::try_from(index).unwrap_or_default()),
                ..AgentRowSpec::default()
            },
            now,
        );
        row.inactive = true;
        row.archived = true;
        row
    }));
    rows
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
    let emblem = rimz::agents::emblem_for(kind);
    let (product_name, color, color_rgb) =
        if let Some(definition) = rimz::agents::spec_by_kind(kind) {
            (
                definition.display_name.to_owned(),
                definition.brand.color,
                Some(definition.brand.color_rgb),
            )
        } else {
            (provider_title_case(kind), 244, None)
        };
    rimz::SidebarProviderPanel {
        kind: kind.to_owned(),
        account_scope: Default::default(),
        product_name,
        art: emblem.lines,
        art_tints: emblem.tints,
        color,
        color_rgb,
        color_role: None,
        version: version.map(ToOwned::to_owned),
        plan: plan.map(ToOwned::to_owned),
        metered,
        remote_control: if remote_control {
            rimz::RemoteControlBadge::Healthy
        } else {
            rimz::RemoteControlBadge::Hidden
        },
        active_sessions: spending.headline.sessions,
        spending: Some(spending),
        day_budget: None,
        extra_credits: matches!(kind, "pi" | "opencode").then_some(rimz::ExtraCredits::Disabled),
        reset_credits: None,
        window_placeholders: Vec::new(),
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
            ..Default::default()
        }
    };
    rimz::SpendTally {
        headline: window(1.0),
        week: window(2.8),
        month: window(11.2),
        year: window(36.4),
    }
}
