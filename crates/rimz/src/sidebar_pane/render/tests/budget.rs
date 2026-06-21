//! Perf guard for frame composition at fleet scale (see
//! docs/internals/health/performance.md → frame redraw). The render thread paints by
//! recomposing the whole frame from the cached snapshot, so its cost must stay
//! linear in the row count — an accidental per-row full-snapshot scan (O(rows²))
//! is exactly the regression a tens-of-agents fleet would feel as a stuttering
//! spinner. The bound is relative (big-vs-small ratio), never a tight wall-clock
//! budget that flakes on a busy CI box.

use std::io;
use std::time::{Duration, Instant};

use crate::agents::RateLimitWindow;
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::sidebar_pane::render::render_fixed;
use crate::{
    AgentCard, RowCard, SidebarProviderPanel, SidebarRow, SidebarSnapshot, SidebarStatusCount,
    SidebarSubAgent, SidebarWorktreeGroup, SidebarWorktreeKind, SpendTally, SpendWindow,
};

fn sub_agent(parent: &str, index: usize) -> SidebarSubAgent {
    let now = super::fixed_now();
    SidebarSubAgent {
        id: format!("{parent}-sub-{index}"),
        name: "Explore".to_owned(),
        status: crate::agents::AgentStatus::Running,
        phase: crate::agents::TurnPhase::Acting,
        task: None,
        model: Some("claude-opus-4-8".to_owned()),
        effort: Some("high".to_owned()),
        description: Some(format!("scan module {index} for callers")),
        total_tokens: Some(40_000 + (index as u64) * 7_321),
        elapsed_secs: Some(90 + index as i64),
        started_at: Some(now),
        last_activity: now,
        registered_at: Some(now),
    }
}

fn agent_row(group: usize, index: usize) -> SidebarRow {
    let id = format!("agent-{group}-{index}");
    SidebarRow {
        id: id.clone(),
        name: "claude".to_owned(),
        pane: Some(crate::pane::PaneRef {
            pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{group}_{index}")),
            session_name: "rimz-perf".to_owned(),
            view_id: Some(format!("tab_{group}")),
            view_kind: Some(crate::ids::ViewKind::Tab),
            view_name: None,
            is_focused: false,
            is_floating: false,
            command: Some("node".to_owned()),
            spawn_command: None,
            cwd: Some(format!("/repo/wt{group}")),
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }),
        worktree_path: Some(format!("/repo/wt{group}")),
        worktree_branch: Some(format!("feature-{group}")),
        unread: false,
        inactive: false,
        last_activity: super::fixed_now(),
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(crate::agents::AgentStatus::Running),
            phase: crate::agents::TurnPhase::Acting,
            task: Some(format!("refactor module {index} of worktree {group}")),
            model: Some("Opus".to_owned()),
            effort: Some("high".to_owned()),
            context_pct: Some(((index * 13) % 100) as u8),
            context_window: Some(200_000),
            total_tokens: Some(10_000 + (index as u64) * 991),
            todo_done: Some(3),
            todo_total: Some(7),
            context_severity: Some(crate::agents::ContextSeverity::Yellow),
            // Row 0 is the default selection, so its card expands these in every
            // composed frame — the sub-agent loop stays inside the measured work.
            sub_agents: (0..3).map(|sub| sub_agent(&id, sub)).collect(),
            ..AgentCard::default()
        })),
    }
}

fn spend_window(usd: f64) -> SpendWindow {
    SpendWindow {
        usd,
        tokens: (usd * 100_000.0) as u64,
        input: (usd * 80_000.0) as u64,
        output: (usd * 20_000.0) as u64,
        cache_read: (usd * 400_000.0) as u64,
        cache_write: (usd * 50_000.0) as u64,
        sessions: 4,
    }
}

fn provider_panel(index: usize) -> SidebarProviderPanel {
    SidebarProviderPanel {
        kind: format!("provider{index}"),
        product_name: format!("Provider {index}"),
        art: vec!["▐███▌".to_owned(), "▝▜█▛▘".to_owned(), " ▘▝ ".to_owned()],
        color: 100 + index as u8,
        color_rgb: None,
        color_role: None,
        version: Some("1.2.3".to_owned()),
        plan: Some("Max".to_owned()),
        metered: true,
        remote_control: false,
        spending: Some(SpendTally {
            headline: spend_window(4.2 + index as f64),
            week: spend_window(31.0 + index as f64),
            month: spend_window(118.0 + index as f64),
            year: spend_window(960.0 + index as f64),
        }),
        extra_credits: None,
        // Two budget windows per panel so the mana bars and the fleet ledger's
        // W/M columns pay their real per-window cost at provider scale.
        windows: vec![
            RateLimitWindow {
                used_percentage: Some(((index * 17) % 100) as u8),
                resets_at: Some(super::fixed_now()),
                duration_mins: Some(300),
                ..Default::default()
            },
            RateLimitWindow {
                used_percentage: Some(((index * 7) % 100) as u8),
                resets_at: Some(super::fixed_now()),
                duration_mins: Some(10_080),
                ..Default::default()
            },
        ],
    }
}

/// `groups` worktree groups of `per_group` agent cards plus `providers`
/// dashboard blocks — the synthetic fleet the guard scales. Every card carries
/// sub-agents and every panel carries spend figures and budget windows, so the
/// loop-heavy compose paths (the selected card's sub-agent expansion, the
/// dashboard's mana bars, the fleet ledger's per-window columns) sit inside the
/// measured work rather than short-circuiting on empty fixtures.
fn fleet(groups: usize, per_group: usize, providers: usize) -> SidebarSnapshot {
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap();
    let now = super::fixed_now();
    SidebarSnapshot {
        workspace_id,
        display_name: "query-engine".to_owned(),
        generated_at: now,
        panes_produced_at_ms: None,
        panes_observed_at_ms: None,
        focus_contested_panes: Vec::new(),
        truth_degraded: None,
        now,
        worktree_groups: (0..groups)
            .map(|group| SidebarWorktreeGroup {
                key: format!("/repo/wt{group}"),
                label: format!("feature-{group}"),
                kind: SidebarWorktreeKind::Worktree,
                status_counts: vec![SidebarStatusCount {
                    status: crate::agents::AgentStatus::Running,
                    count: per_group,
                }],
                rows: (0..per_group)
                    .map(|index| agent_row(group, index))
                    .collect(),
                hidden_count: 0,
                diff_added: Some(120),
                diff_removed: Some(40),
                commits_ahead: Some(3),
                commits_behind: Some(1),
                trunk: Some("main".to_owned()),
                clean: None,
                landed: None,
            })
            .collect(),
        needs_attention: Vec::new(),
        resolver_working: Vec::new(),
        agents: Vec::new(),
        wired_lazy_kinds: Vec::new(),
        lazy_agent_default_models: std::collections::BTreeMap::new(),
        agent_panes: Vec::new(),
        own_view: None,
        only_daemon_view_remains: false,
        project_root: Some(std::path::PathBuf::from("/repo")),
        worktree_roots: Vec::new(),
        worktree_home: None,
        root_class: crate::workspace::RootClass::Repo,
        sidebar: crate::config::SidebarConfig::default(),
        theme: crate::config::ThemeConfig::default(),
        pets: crate::config::PetsConfig::default(),
        attention: crate::config::AttentionConfig::default(),
        providers: (0..providers).map(provider_panel).collect(),
        value_tally: None,
        workspace_value_tally: None,
        today_spend_live_usd: None,
        link: None,
        reflects_log: None,
    }
}

fn render_n(snapshot: &SidebarSnapshot, rounds: u32) -> Duration {
    let start = Instant::now();
    for _ in 0..rounds {
        render_fixed(io::sink(), snapshot, None, 54, 200).expect("render");
    }
    start.elapsed()
}

/// Proves the fixture loads the loop-heavy paths it claims to: the selected
/// card's sub-agent expansion and the per-window mana bars must appear in the
/// composed frame, or the budget below measures a short-circuit instead of the
/// real work.
#[test]
fn the_synthetic_fleet_pays_the_loop_heavy_paths() {
    let mut out = Vec::new();
    render_fixed(&mut out, &fleet(10, 5, 8), None, 54, 200).expect("render");
    let frame = String::from_utf8_lossy(&out);
    assert!(
        frame.contains("subagents (3)"),
        "the default-selected card no longer expands its sub-agents"
    );
    assert!(
        frame.contains("5h"),
        "the provider panels no longer paint their budget windows"
    );
}

/// The linearity work-proxy: a 10-worktree / 50-agent / 8-provider fleet holds
/// roughly 10× the content of the small room, so its compose may cost roughly
/// 10× — generous slack included. What this catches is a superlinear regression
/// (a per-row scan of the whole snapshot, an O(rows²) sort) that would multiply
/// the ratio far past the slack, while staying immune to absolute machine speed.
#[test]
fn compose_scales_linearly_with_fleet_size() {
    const ROUNDS: u32 = 60;
    let small = fleet(1, 5, 1);
    let big = fleet(10, 5, 8);

    // Warm both paths once so lazy init (palette OnceLock, allocator warmup)
    // lands outside the measured rounds.
    render_n(&small, 5);
    render_n(&big, 5);

    let small_elapsed = render_n(&small, ROUNDS).max(Duration::from_micros(1));
    let big_elapsed = render_n(&big, ROUNDS);

    let ratio = big_elapsed.as_secs_f64() / small_elapsed.as_secs_f64();
    assert!(
        ratio < 60.0,
        "big/small compose ratio {ratio:.1}× suggests a superlinear regression \
         (content ratio is ~10×; big {big_elapsed:?}, small {small_elapsed:?})"
    );
}
