use std::time::{Duration, Instant};

use jiff::Timestamp;

use super::*;
use crate::agents::lifecycle::TurnPhase;
use crate::feed::{AgentState, FeedItem, FeedKind, PaneRef, Surface};
use crate::ids::{MuxName, PaneId, WorkspaceId};

fn workspace() -> WorkspaceId {
    WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-notify"))
}

fn prefs() -> NotificationsPrefs {
    NotificationsPrefs {
        coalesce_ms: 0,
        ..NotificationsPrefs::default()
    }
}

fn snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
    snapshot_with_items(Vec::new(), agents)
}

fn snapshot_with_items(items: Vec<FeedItem>, agents: Vec<AgentState>) -> SidebarSnapshot {
    let panes = agents
        .iter()
        .filter_map(|agent| agent.pane.clone())
        .collect::<Vec<_>>();
    SidebarSnapshot::build_with_agents(workspace(), items, agents, Timestamp::now())
        .with_live_panes(panes, None)
}

fn link_snapshot(tier: LinkTier, freshness: SidebarLinkFreshness) -> SidebarSnapshot {
    let mut snapshot = snapshot(Vec::new());
    snapshot.link = Some(SidebarLinkHealth {
        rtt_ms: Some(match tier {
            LinkTier::Good => 42,
            LinkTier::Degraded => 230,
            LinkTier::Bad => 800,
        }),
        miss_pct: match tier {
            LinkTier::Good => 0,
            LinkTier::Degraded => 4,
            LinkTier::Bad => 40,
        },
        tier,
        freshness,
        sampled_at_ms: 1,
    });
    snapshot
}

fn agent(id: &str, status: AgentStatus, focused: bool) -> AgentState {
    let now = Timestamp::now();
    AgentState {
        agent_id: AgentSessionId::from(id),
        kind: AgentKind::new_unchecked("claude"),
        name: None,
        kind_ordinal: None,
        alias: None,
        status,
        phase: TurnPhase::Idle,
        pane: Some(PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, format!("%{id}")),
            session_name: "rimz-test".to_owned(),
            view_id: Some("view-1".to_owned()),
            view_kind: None,
            view_name: None,
            is_focused: focused,
            command: Some("claude".to_owned()),
            spawn_command: None,
            cwd: Some("/tmp/rimz-notify".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }),
        agent_pid: None,
        agent_process_start: None,
        runtime_owner: None,
        parent_agent_id: None,
        worktree_path: None,
        worktree_branch: None,
        task: None,
        prompt: None,
        transcript_path: None,
        recent_prompts: Vec::new(),
        model: None,
        effort: None,
        context_pct: None,
        context_window: None,
        total_tokens: None,
        cache_read_input_tokens: None,
        fresh_input_tokens: None,
        output_tokens: None,
        todo_done: None,
        todo_total: None,
        context: None,
        subagent_description: None,
        subagent_started_at: None,
        turn_started_at: None,
        compacting_since: None,
        compaction_count: 0,
        last_seen: now,
        last_activity: now,
        registered_at: Some(now),
    }
}

fn agent_ask(source: &str, session_id: &str) -> FeedItem {
    let mut item = FeedItem::new(
        workspace(),
        Surface::NativeUi,
        FeedKind::Question,
        format!("{source} needs attention"),
        source,
        "agent-hook",
    );
    item.worktree_path = Some("/tmp/rimz-notify".to_owned());
    item.payload = serde_json::json!({ "session_id": session_id });
    item
}

fn script_ask_for_pane(pane: PaneRef) -> FeedItem {
    let mut item = FeedItem::new(
        workspace(),
        Surface::Script,
        FeedKind::Question,
        "approve deploy?",
        "deploy",
        "script",
    );
    item.pane = Some(pane);
    item
}

#[test]
fn configured_transition_edges_fire() {
    let mut state = NotificationState::default();
    let prefs = NotificationsPrefs {
        triggers: vec![crate::config::NotificationTrigger::Failed],
        ..prefs()
    };
    assert!(
        state
            .evaluate(
                &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
                &prefs,
                1,
            )
            .is_empty()
    );

    let waiting = state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
        &prefs,
        2,
    );
    assert!(waiting.is_empty(), "waiting is not configured");

    let failed = state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Failed, false)]),
        &prefs,
        3,
    );
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].notification_kind, NotificationKind::Failed);
}

#[test]
fn projected_pending_agent_ask_notifies_as_waiting() {
    let mut state = NotificationState::default();
    let prefs = prefs();
    state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
        &prefs,
        0,
    );

    let agent = agent("a1", AgentStatus::Running, false);
    let mut ask = agent_ask("claude", "a1");
    ask.updated_at = agent.last_activity + Duration::from_secs(1);
    let next = snapshot_with_items(vec![ask], vec![agent]);
    assert_eq!(
        next.agents[0].status,
        AgentStatus::Running,
        "the rollup keeps the raw lifecycle state"
    );
    assert_eq!(
        next.worktree_groups[0].rows[0].status(),
        Some(AgentStatus::Waiting),
        "the row carries the displayed status the sidebar paints"
    );

    let out = state.evaluate(&next, &prefs, 100);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].notification_kind, NotificationKind::Waiting);
    assert_eq!(out[0].agents[0].agent_id, AgentSessionId::from("a1"));
}

#[test]
fn projected_standalone_ask_row_notifies_after_seed() {
    let mut state = NotificationState::default();
    let prefs = prefs();
    state.evaluate(&snapshot(Vec::new()), &prefs, 0);

    let pane = PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, "%deploy"),
        session_name: "rimz-test".to_owned(),
        view_id: Some("view-1".to_owned()),
        view_kind: None,
        view_name: None,
        is_focused: false,
        command: Some("deploy".to_owned()),
        spawn_command: None,
        cwd: Some("/tmp/rimz-notify".to_owned()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    };
    let item = script_ask_for_pane(pane.clone());
    let request_id = item.request_id.to_string();
    let next =
        SidebarSnapshot::build_with_agents(workspace(), vec![item], Vec::new(), Timestamp::now())
            .with_live_panes(vec![pane], None);

    assert_eq!(next.worktree_groups[0].rows[0].id, request_id);
    assert_eq!(
        next.worktree_groups[0].rows[0].status(),
        Some(AgentStatus::Waiting)
    );
    let out = state.evaluate(&next, &prefs, 100);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].notification_kind, NotificationKind::Waiting);
    assert_eq!(out[0].agents[0].label, "approve deploy?");
}

#[test]
fn same_agent_is_debounced_within_window() {
    let mut state = NotificationState::default();
    let prefs = NotificationsPrefs {
        debounce_ms: 1_000,
        ..prefs()
    };
    state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
        &prefs,
        0,
    );
    assert_eq!(
        state
            .evaluate(
                &snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
                &prefs,
                100,
            )
            .len(),
        1
    );
    state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
        &prefs,
        200,
    );

    let debounced = state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
        &prefs,
        500,
    );
    assert!(debounced.is_empty());
}

#[test]
fn focused_agent_is_suppressed() {
    let mut state = NotificationState::default();
    state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
        &prefs(),
        0,
    );

    let out = state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Waiting, true)]),
        &prefs(),
        100,
    );
    assert!(out.is_empty());
}

#[test]
fn burst_coalesces_after_window() {
    let mut state = NotificationState::default();
    let prefs = NotificationsPrefs {
        coalesce_ms: 1_000,
        ..NotificationsPrefs::default()
    };
    state.evaluate(
        &snapshot(vec![
            agent("a1", AgentStatus::Running, false),
            agent("a2", AgentStatus::Running, false),
        ]),
        &prefs,
        0,
    );
    assert!(
        state
            .evaluate(
                &snapshot(vec![
                    agent("a1", AgentStatus::Waiting, false),
                    agent("a2", AgentStatus::Running, false),
                ]),
                &prefs,
                100,
            )
            .is_empty(),
        "the first edge waits for the coalesce window"
    );
    assert!(
        state
            .evaluate(
                &snapshot(vec![
                    agent("a1", AgentStatus::Waiting, false),
                    agent("a2", AgentStatus::Failed, false),
                ]),
                &prefs,
                500,
            )
            .is_empty(),
        "the second edge joins the same burst"
    );

    let out = state.evaluate(
        &snapshot(vec![
            agent("a1", AgentStatus::Waiting, false),
            agent("a2", AgentStatus::Failed, false),
        ]),
        &prefs,
        1_100,
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].notification_kind, NotificationKind::Coalesced);
    assert_eq!(out[0].agents.len(), 2);
}

#[test]
fn notified_agents_are_pruned_when_they_disappear() {
    let mut state = NotificationState::default();
    state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Running, false)]),
        &prefs(),
        0,
    );

    let out = state.evaluate(
        &snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
        &prefs(),
        100,
    );
    assert_eq!(out.len(), 1);
    assert_eq!(state.last_notified_at_ms.len(), 1);

    state.evaluate(&snapshot(Vec::new()), &prefs(), 200);
    assert!(state.last_notified_at_ms.is_empty());
}

#[test]
fn link_degraded_notifies_after_hysteresis() {
    let mut state = LinkNotificationState::default();
    state.evaluate(
        &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
        &prefs(),
        0,
    );

    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
                &prefs(),
                9_999,
            )
            .notification
            .is_none(),
        "degraded must hold for the full window"
    );
    let out = state.evaluate(
        &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
        &prefs(),
        10_000,
    );

    let notification = out.notification.expect("degraded notification");
    assert_eq!(
        notification.notification_kind,
        NotificationKind::LinkDegraded
    );
    assert_eq!(notification.title, "Rimz: remote link degraded");
    assert!(notification.body.contains("RTT 230ms"));
    assert_eq!(
        out.alert,
        Some(LinkAlert {
            tier: LinkTier::Degraded,
            rtt_ms: Some(230),
            miss_pct: 4,
            since_ms: 0,
            recovered_after_ms: None,
        })
    );
}

#[test]
fn stale_link_stats_pause_degraded_hysteresis() {
    let mut state = LinkNotificationState::default();
    state.evaluate(
        &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
        &prefs(),
        0,
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Stale),
                &prefs(),
                9_000,
            )
            .notification
            .is_none()
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
                &prefs(),
                20_000,
            )
            .notification
            .is_none(),
        "the stale span is not counted toward the hold"
    );

    let out = state.evaluate(
        &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
        &prefs(),
        21_000,
    );
    assert_eq!(
        out.notification
            .expect("remaining fresh degraded window elapsed")
            .notification_kind,
        NotificationKind::LinkDegraded
    );
    assert_eq!(
        out.alert,
        Some(LinkAlert {
            tier: LinkTier::Degraded,
            rtt_ms: Some(230),
            miss_pct: 4,
            since_ms: 11_000,
            recovered_after_ms: None,
        })
    );
}

#[test]
fn link_recovered_notifies_after_good_hysteresis() {
    let mut state = LinkNotificationState::default();
    state.evaluate(
        &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
        &prefs(),
        0,
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
                &prefs(),
                10_000,
            )
            .notification
            .is_some(),
        "the link first enters an active degraded episode"
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
                &prefs(),
                10_000,
            )
            .notification
            .is_none(),
        "the first good sample starts the recovery clock"
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
                &prefs(),
                39_999,
            )
            .notification
            .is_none(),
        "good health must hold before recovery"
    );
    let out = state.evaluate(
        &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
        &prefs(),
        40_000,
    );

    let notification = out.notification.expect("recovery notification");
    assert_eq!(
        notification.notification_kind,
        NotificationKind::LinkRecovered
    );
    assert_eq!(notification.title, "Rimz: remote link recovered");
    assert_eq!(
        out.alert,
        Some(LinkAlert {
            tier: LinkTier::Good,
            rtt_ms: Some(42),
            miss_pct: 0,
            since_ms: 0,
            recovered_after_ms: Some(40_000),
        })
    );
}

#[test]
fn disappearing_active_link_closes_diagnostic_episode() {
    let mut state = LinkNotificationState::default();
    state.evaluate(
        &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
        &prefs(),
        0,
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
                &prefs(),
                10_000,
            )
            .alert
            .is_some(),
        "the link first enters an active degraded episode"
    );

    let out = state.evaluate(&snapshot(Vec::new()), &prefs(), 15_000);

    assert!(
        out.notification.is_none(),
        "an expired sidecar closes diagnostics without a user notification"
    );
    assert_eq!(
        out.alert,
        Some(LinkAlert {
            tier: LinkTier::Good,
            rtt_ms: None,
            miss_pct: 0,
            since_ms: 0,
            recovered_after_ms: Some(15_000),
        })
    );
    assert!(
        state
            .evaluate(&snapshot(Vec::new()), &prefs(), 16_000)
            .alert
            .is_none(),
        "the expiry close is emitted once"
    );
}

#[test]
fn stale_link_stats_pause_recovery_hysteresis() {
    let mut state = LinkNotificationState::default();
    state.evaluate(
        &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
        &prefs(),
        0,
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
                &prefs(),
                10_000,
            )
            .notification
            .is_some(),
        "the link first enters an active degraded episode"
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
                &prefs(),
                10_000,
            )
            .notification
            .is_none(),
        "the first good sample starts the recovery clock"
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Stale),
                &prefs(),
                39_000,
            )
            .notification
            .is_none()
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
                &prefs(),
                50_000,
            )
            .notification
            .is_none(),
        "the stale span is not counted toward recovery"
    );

    let out = state.evaluate(
        &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
        &prefs(),
        51_000,
    );
    assert_eq!(
        out.notification
            .expect("remaining fresh good window elapsed")
            .notification_kind,
        NotificationKind::LinkRecovered
    );
    assert_eq!(
        out.alert,
        Some(LinkAlert {
            tier: LinkTier::Good,
            rtt_ms: Some(42),
            miss_pct: 0,
            since_ms: 11_000,
            recovered_after_ms: Some(40_000),
        })
    );
}

#[test]
fn command_spawn_receives_notification_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("env.txt");
    let command = format!(
        "printf '%s\\n%s\\n%s\\n%s\\n%s\\n' \"$RIMZ_NOTIFY_TITLE\" \"$RIMZ_NOTIFY_BODY\" \"$RIMZ_NOTIFY_AGENT\" \"$RIMZ_NOTIFY_KIND\" \"${{RIMZ_NOTIFY_UNREAD-unset}}\" > {}",
        sh_quote(&out)
    );
    let notification = Notification {
        agents: vec![NotificationAgent {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("sess-1"),
            label: "claude sess-1".to_owned(),
            pane_id: None,
            prev_status: Some(AgentStatus::Running),
            new_status: Some(AgentStatus::Waiting),
        }],
        notification_kind: NotificationKind::Waiting,
        title: "Rimz: claude needs you".to_owned(),
        body: "claude sess-1 is waiting for input.".to_owned(),
        unread_count: None,
    };

    let pid = spawn_notify_command(&command, &notification).expect("spawn command");
    assert!(pid > 0);

    let expected = "Rimz: claude needs you\nclaude sess-1 is waiting for input.\nclaude sess-1\nwaiting\nunset\n";
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut text = String::new();
    while Instant::now() < deadline {
        if let Ok(current) = std::fs::read_to_string(&out) {
            text = current;
            if text == expected {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if text.is_empty() {
        text = std::fs::read_to_string(&out).expect("command wrote env file");
    }
    assert_eq!(text, expected);
}

#[test]
fn command_spawn_receives_unread_env_for_reminders() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("env.txt");
    let command = format!(
        "printf '%s\\n%s\\n%s\\n%s\\n%s\\n' \"$RIMZ_NOTIFY_TITLE\" \"$RIMZ_NOTIFY_BODY\" \"$RIMZ_NOTIFY_AGENT\" \"$RIMZ_NOTIFY_KIND\" \"$RIMZ_NOTIFY_UNREAD\" > {}",
        sh_quote(&out)
    );
    let notification = Notification {
        agents: Vec::new(),
        notification_kind: NotificationKind::Reminder,
        title: "Rimz: 2 unread need you".to_owned(),
        body: "2 unread rows still need you.".to_owned(),
        unread_count: Some(2),
    };

    let pid = spawn_notify_command(&command, &notification).expect("spawn command");
    assert!(pid > 0);

    let expected = "Rimz: 2 unread need you\n2 unread rows still need you.\n\nreminder\n2\n";
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut text = String::new();
    while Instant::now() < deadline {
        if let Ok(current) = std::fs::read_to_string(&out) {
            text = current;
            if text == expected {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if text.is_empty() {
        text = std::fs::read_to_string(&out).expect("command wrote env file");
    }
    assert_eq!(text, expected);
}

fn sh_quote(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    format!("'{}'", raw.replace('\'', "'\\''"))
}
