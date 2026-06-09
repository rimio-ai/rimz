use super::*;

fn row_time() -> Timestamp {
    Timestamp::from_second(1_700_000_000).unwrap()
}

#[test]
fn serde_keeps_agent_card_flat_with_row_kind_key() {
    let row = SidebarRow {
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

    let value = serde_json::to_value(&row).unwrap();

    assert_eq!(value["row_kind"], "agent");
    assert!(value.get("card").is_none());
    assert!(value.get("unread").is_none());
    assert_eq!(value["prompt"], "fix auth flow");
    assert_eq!(serde_json::from_value::<SidebarRow>(value).unwrap(), row);
}

#[test]
fn serde_keeps_process_card_flat_with_row_kind_key() {
    let row = SidebarRow {
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

    let value = serde_json::to_value(&row).unwrap();

    assert_eq!(value["row_kind"], "process");
    assert!(value.get("card").is_none());
    assert!(value.get("status").is_none());
    assert_eq!(value["state"], "stuck");
    assert_eq!(value["command_detail"], "cargo build --release");
    assert_eq!(serde_json::from_value::<SidebarRow>(value).unwrap(), row);
}

#[test]
fn unread_round_trips_only_when_true() {
    let row = SidebarRow {
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

    let value = serde_json::to_value(&row).unwrap();

    assert_eq!(value["unread"], true);
    assert_eq!(serde_json::from_value::<SidebarRow>(value).unwrap(), row);
}
