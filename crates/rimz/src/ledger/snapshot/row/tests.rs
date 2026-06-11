use super::*;
use crate::agents::AgentCost;

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
        unread: false,
        last_activity: row_time(),
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(AgentStatus::Running),
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
        unread: false,
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
        unread: true,
        last_activity: row_time(),
        card: RowCard::Agent(Box::new(AgentCard {
            status: Some(AgentStatus::Success),
            ..AgentCard::default()
        })),
    };

    let value = serde_json::to_value(&unread).unwrap();

    assert_eq!(value["unread"], true);
    assert_eq!(serde_json::from_value::<SidebarRow>(value).unwrap(), unread);
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
        observed_at: row_time(),
    }
}
