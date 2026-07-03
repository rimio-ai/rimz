use super::*;
use crate::agents::{AgentCost, AgentCurrentUsage};

fn row_time() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

#[test]
fn serde_keeps_cards_flat_with_row_kind_key() {
    let agent = SidebarRow {
        id: "agent:s1".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        last_activity: row_time(),
        card: RowCard::Agent(Box::new(AgentCard {
            status: AgentStatus::Running,
            prompt: Some("fix auth flow".to_owned()),
            ..AgentCard::default()
        })),
    };

    let value = serde_json::to_value(&agent).unwrap();

    assert_eq!(value["row_kind"], "agent");
    assert!(value.get("card").is_none());
    assert!(value.get("unread").is_none());
    assert_eq!(value["prompt"], "fix auth flow");
    assert_eq!(serde_json::from_value::<SidebarRow>(value).unwrap(), agent);

    let process = SidebarRow {
        id: "process:%1".to_owned(),
        name: "cargo".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        last_activity: row_time(),
        card: RowCard::Process(ProcessCard {
            state: ProcessState::Stuck,
            command_detail: Some("cargo build --release".to_owned()),
            foreign_user: None,
            rss_kb: Some(512 * 1_024),
            cpu_pct: Some(42),
            io_bps: Some(1_024),
        }),
    };

    let value = serde_json::to_value(&process).unwrap();

    assert_eq!(value["row_kind"], "process");
    assert!(value.get("card").is_none());
    assert!(value.get("status").is_none());
    assert_eq!(value["state"], "stuck");
    assert_eq!(value["command_detail"], "cargo build --release");
    assert_eq!(
        serde_json::from_value::<SidebarRow>(value).unwrap(),
        process
    );

    let unread = SidebarRow {
        id: "agent:s1".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: true,
        inactive: false,
        last_activity: row_time(),
        card: RowCard::Agent(Box::new(AgentCard {
            status: AgentStatus::Success,
            ..AgentCard::default()
        })),
    };

    let value = serde_json::to_value(&unread).unwrap();

    assert_eq!(value["unread"], true);
    assert_eq!(serde_json::from_value::<SidebarRow>(value).unwrap(), unread);
}

#[test]
fn display_name_prefers_agent_handle_and_falls_back_to_row_name() {
    let mut agent = SidebarRow {
        id: "agent:s1".to_owned(),
        name: "claude".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        last_activity: row_time(),
        card: RowCard::Agent(Box::new(AgentCard {
            handle: Some("planner".to_owned()),
            team: None,
            launch_group: None,
            launch_ordinal: None,

            ..AgentCard::default()
        })),
    };
    assert_eq!(agent.display_name(), "planner");

    agent.as_agent_mut().unwrap().handle = None;
    assert_eq!(agent.display_name(), "claude");

    let process = SidebarRow {
        id: "process:%1".to_owned(),
        name: "cargo".to_owned(),
        pane: None,
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: None,
        channel: None,
        unread: false,
        inactive: false,
        last_activity: row_time(),
        card: RowCard::Process(ProcessCard::default()),
    };
    assert_eq!(process.display_name(), "cargo");
}

#[test]
fn agent_card_session_history_requires_positive_evidence() {
    assert!(!AgentCard::default().has_session_history());
    assert!(
        !AgentCard {
            total_tokens: Some(0),
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        AgentCard {
            total_tokens: Some(1),
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        AgentCard {
            compaction_count: 1,
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        AgentCard {
            context: Some(context_with_cost(0.01)),
            ..AgentCard::default()
        }
        .has_session_history()
    );
    assert!(
        !AgentCard {
            context: Some(context_with_cost(0.0)),
            ..AgentCard::default()
        }
        .has_session_history()
    );
}

fn context_with_cost(total_cost_usd: f64) -> AgentContext {
    AgentContext {
        source: "claude".to_owned(),
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
        cost: Some(AgentCost {
            total_cost_usd: Some(total_cost_usd),
            ..AgentCost::default()
        }),
        tokens: None,
        rate_limits: None,
        pr: None,
        account: None,
        turn_error: None,
        turn_complete: None,
        observed_at: row_time(),
    }
}

fn context_with_tokens(tokens: AgentTokenUsage) -> AgentContext {
    AgentContext {
        tokens: Some(tokens),
        ..context_with_cost(0.0)
    }
}

#[test]
fn context_gauge_percent_only_trusts_a_sidecar_percentage_paired_with_a_window() {
    // A sidecar percentage drawn against an unknown window cannot share a
    // denominator with the displayed window, so the gauge prefers the
    // fold-derived scalar — which the fold tied to the resolved window — over
    // it, avoiding a bar that disagrees with its window label.
    let untethered = AgentCard {
        context_pct: Some(16),
        context_window: Some(1_000_000),
        context: Some(context_with_tokens(AgentTokenUsage {
            context_window_size: None,
            used_percentage: Some(82),
            remaining_percentage: Some(18),
            current_usage: None,
        })),
        ..AgentCard::default()
    };
    assert_eq!(untethered.context_gauge_percent(), Some(16));

    // With its own window present, the sidecar percentage is authoritative and
    // shares the denominator the identity line shows.
    let tethered = AgentCard {
        context_pct: Some(16),
        context: Some(context_with_tokens(AgentTokenUsage {
            context_window_size: Some(1_000_000),
            used_percentage: Some(40),
            remaining_percentage: Some(60),
            current_usage: None,
        })),
        ..AgentCard::default()
    };
    assert_eq!(tethered.context_gauge_percent(), Some(40));
}

#[test]
fn context_gauge_percent_derives_from_sidecar_usage_when_percentage_is_absent() {
    let derived = AgentCard {
        context_pct: Some(0),
        context: Some(context_with_tokens(AgentTokenUsage {
            context_window_size: Some(258_400),
            used_percentage: None,
            remaining_percentage: None,
            current_usage: Some(AgentCurrentUsage {
                input_tokens: Some(6_700),
                output_tokens: Some(825),
                cache_creation_input_tokens: None,
                cache_read_input_tokens: Some(56_900),
            }),
        })),
        ..AgentCard::default()
    };

    assert_eq!(
        derived.context_gauge_percent(),
        Some(24),
        "rich Codex context derives the filled bar from current usage over its own window"
    );
}
