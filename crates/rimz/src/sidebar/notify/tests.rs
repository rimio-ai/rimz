use std::time::{Duration, Instant};

use jiff::Timestamp;

use super::*;
use crate::agents::{AgentState, AskKind, OpenAsk};
use crate::ids::{AskId, MuxName, PaneId, WorkspaceId};
use crate::pane::PaneRef;
use crate::sidebar::unread::{OpenedUnread, opened_unread};

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
    snapshot_with(agents)
}

fn snapshot_with(agents: Vec<AgentState>) -> SidebarSnapshot {
    let panes = agents
        .iter()
        .filter_map(|agent| agent.pane.clone())
        .collect::<Vec<_>>();
    SidebarSnapshot::build_with_agents(workspace(), agents, Timestamp::now())
        .with_live_panes(panes, None)
}

fn evaluate_opened(
    state: &mut NotificationState,
    snapshot: SidebarSnapshot,
    opened_ids: &[&str],
    prefs: &NotificationsPrefs,
    now_ms: u64,
) -> Vec<Notification> {
    evaluate_with_unread(state, snapshot, opened_ids, &[], prefs, now_ms)
}

fn evaluate_with_unread(
    state: &mut NotificationState,
    mut snapshot: SidebarSnapshot,
    opened_ids: &[&str],
    unread_ids: &[&str],
    prefs: &NotificationsPrefs,
    now_ms: u64,
) -> Vec<Notification> {
    mark_unread_rows(&mut snapshot, unread_ids);
    let opened = opened_rows(&mut snapshot, opened_ids);
    state.evaluate(&snapshot, &opened, prefs, now_ms)
}

fn evaluate_no_open(
    state: &mut NotificationState,
    snapshot: SidebarSnapshot,
    prefs: &NotificationsPrefs,
    now_ms: u64,
) -> Vec<Notification> {
    state.evaluate(&snapshot, &[], prefs, now_ms)
}

fn mark_unread_rows(snapshot: &mut SidebarSnapshot, ids: &[&str]) {
    for row in snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
    {
        if ids.iter().any(|id| *id == row.id) {
            row.unread = true;
        }
    }
}

fn opened_rows(snapshot: &mut SidebarSnapshot, ids: &[&str]) -> Vec<OpenedUnread> {
    let mut opened = Vec::new();
    let viewed = snapshot.viewed_panes.clone();
    for row in snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
    {
        if ids.iter().any(|id| *id == row.id) {
            row.unread = true;
            let mut unread = opened_unread(row, row.last_activity.as_millisecond(), false);
            unread.focused = unread
                .pane_id
                .as_ref()
                .is_some_and(|pane| viewed.contains(pane));
            opened.push(unread);
        }
    }
    opened
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

fn agent(id: &str, status: AgentStatus, _focused: bool) -> AgentState {
    let now = Timestamp::now();
    AgentState {
        status,
        pane: Some(PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, format!("%{id}")),
            session_name: "rimz-test".to_owned(),
            view_id: Some("view-1".to_owned()),
            view_kind: None,
            view_name: None,
            title: None,
            is_floating: false,
            command: Some("claude".to_owned()),
            foreground_cmdline: None,
            spawn_command: None,
            cwd: Some("/tmp/rimz-notify".to_owned()),
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }),
        ..crate::testkit::agent_state("claude", id, now)
    }
}

fn agent_with_context(
    id: &str,
    status: AgentStatus,
    role: &str,
    worktree_branch: &str,
    task: &str,
) -> AgentState {
    AgentState {
        role: Some(role.to_owned()),
        worktree_branch: Some(worktree_branch.to_owned()),
        task: Some(task.to_owned()),
        ..agent(id, status, false)
    }
}

#[test]
fn configured_unread_episode_triggers_fire() {
    let mut state = NotificationState::default();
    let prefs = NotificationsPrefs {
        triggers: vec![crate::config::NotificationTrigger::Failed],
        ..prefs()
    };
    assert!(
        evaluate_no_open(
            &mut state,
            snapshot(vec![agent("a1", AgentStatus::Running, false)]),
            &prefs,
            1,
        )
        .is_empty()
    );

    let waiting = evaluate_opened(
        &mut state,
        snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
        &["a1"],
        &prefs,
        2,
    );
    assert!(waiting.is_empty(), "waiting is not configured");

    let failed = evaluate_opened(
        &mut state,
        snapshot(vec![agent("a1", AgentStatus::Failed, false)]),
        &["a1"],
        &prefs,
        3,
    );
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].notification_kind, NotificationKind::Failed);
}

#[test]
fn projected_ask_rows_notify_as_waiting() {
    let mut state = NotificationState::default();
    let prefs = prefs();

    let mut agent = agent("a1", AgentStatus::Waiting, false);
    agent.waiting_since = Some(agent.last_activity);
    agent.open_ask = Some(OpenAsk {
        id: AskId::parse("ask_0123456789abcdef").unwrap(),
        kind: AskKind::Question,
        detail: Some("Choose?".to_owned()),
        native_key: None,
        since: agent.last_activity,
    });
    let next = snapshot_with(vec![agent]);
    assert_eq!(
        next.worktree_groups[0].rows[0].status(),
        Some(AgentStatus::Waiting),
        "the row carries the displayed status the sidebar paints",
    );

    let out = evaluate_opened(&mut state, next, &["a1"], &prefs, 100);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].notification_kind, NotificationKind::Waiting);
    assert_eq!(out[0].agents[0].agent_id, AgentSessionId::from("a1"));
    assert_eq!(
        out[0].agents[0].ask_id.as_ref().map(AskId::as_str),
        Some("ask_0123456789abcdef")
    );
}

#[test]
fn same_agent_is_debounced_within_window() {
    let mut state = NotificationState::default();
    let prefs = NotificationsPrefs {
        debounce_ms: 1_000,
        ..prefs()
    };
    assert_eq!(
        evaluate_opened(
            &mut state,
            snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
            &["a1"],
            &prefs,
            100,
        )
        .len(),
        1
    );
    evaluate_no_open(
        &mut state,
        snapshot(vec![agent("a1", AgentStatus::Running, false)]),
        &prefs,
        200,
    );

    let debounced = evaluate_opened(
        &mut state,
        snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
        &["a1"],
        &prefs,
        500,
    );
    assert!(debounced.is_empty());
}

#[test]
fn focused_agent_is_suppressed() {
    let mut state = NotificationState::default();
    let focused = agent("a1", AgentStatus::Waiting, true);
    let pane_id = focused.pane.as_ref().expect("agent pane").pane_id.clone();
    let mut snapshot = snapshot(vec![focused]);
    snapshot.focused_pane = Some(pane_id.clone());
    snapshot.viewed_panes = vec![pane_id];

    let out = evaluate_opened(&mut state, snapshot, &["a1"], &prefs(), 100);
    assert!(out.is_empty());
}

#[test]
fn global_title_body_templates_reskin_agent_notifications() {
    let mut state = NotificationState::default();
    let prefs = NotificationsPrefs {
        title: Some("RimZ: {{agent}} {{status}} in {{worktree}}".to_owned()),
        body: Some("{{handle}}: {{task}} ({{count}})".to_owned()),
        ..prefs()
    };

    let out = evaluate_opened(
        &mut state,
        snapshot(vec![agent_with_context(
            "a1",
            AgentStatus::Waiting,
            "planner",
            "feat/ntfy",
            "wire ntfy",
        )]),
        &["a1"],
        &prefs,
        100,
    );

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title, "RimZ: planner waiting in feat/ntfy");
    assert_eq!(out[0].body, "planner: wire ntfy (1)");
    assert_eq!(out[0].agents[0].worktree.as_deref(), Some("feat/ntfy"));
    assert_eq!(out[0].agents[0].task.as_deref(), Some("wire ntfy"));
    assert_eq!(out[0].agents[0].handle, "planner");
}

#[test]
fn default_title_body_stay_when_templates_are_unset() {
    let mut state = NotificationState::default();

    let out = evaluate_opened(
        &mut state,
        snapshot(vec![agent("a1", AgentStatus::Failed, false)]),
        &["a1"],
        &prefs(),
        100,
    );

    assert_eq!(out.len(), 1);
    assert_eq!(out[0].title, "RimZ: claude a1 failed");
    assert_eq!(out[0].body, "claude a1 needs a look.");
}

#[test]
fn burst_coalesces_after_window() {
    let mut state = NotificationState::default();
    let prefs = NotificationsPrefs {
        coalesce_ms: 1_000,
        ..NotificationsPrefs::default()
    };
    assert!(
        evaluate_opened(
            &mut state,
            snapshot(vec![
                agent("a1", AgentStatus::Waiting, false),
                agent("a2", AgentStatus::Running, false),
            ]),
            &["a1"],
            &prefs,
            100,
        )
        .is_empty(),
        "the first edge waits for the coalesce window"
    );
    assert!(
        evaluate_with_unread(
            &mut state,
            snapshot(vec![
                agent("a1", AgentStatus::Waiting, false),
                agent("a2", AgentStatus::Failed, false),
            ]),
            &["a2"],
            &["a1"],
            &prefs,
            500,
        )
        .is_empty(),
        "the second edge joins the same burst"
    );

    let out = evaluate_with_unread(
        &mut state,
        snapshot(vec![
            agent("a1", AgentStatus::Waiting, false),
            agent("a2", AgentStatus::Failed, false),
        ]),
        &[],
        &["a1", "a2"],
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

    let out = evaluate_opened(
        &mut state,
        snapshot(vec![agent("a1", AgentStatus::Waiting, false)]),
        &["a1"],
        &prefs(),
        100,
    );
    assert_eq!(out.len(), 1);
    assert_eq!(state.last_notified_at_ms.len(), 1);

    evaluate_no_open(&mut state, snapshot(Vec::new()), &prefs(), 200);
    assert!(state.last_notified_at_ms.is_empty());
}

#[test]
fn link_degraded_opens_diagnostic_after_hysteresis() {
    let mut state = LinkNotificationState::default();
    state.evaluate(
        &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
        0,
    );

    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
                9_999,
            )
            .is_none(),
        "degraded must hold for the full window"
    );
    let alert = state.evaluate(
        &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
        10_000,
    );

    assert_eq!(
        alert,
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
fn stale_link_stats_pause_hysteresis() {
    // Degraded direction: a stale span pauses the degrade hold, and the stale
    // span is not counted toward it.
    let mut state = LinkNotificationState::default();
    state.evaluate(
        &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
        0,
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Stale),
                9_000,
            )
            .is_none()
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
                20_000,
            )
            .is_none(),
        "the stale span is not counted toward the hold"
    );

    let alert = state.evaluate(
        &link_snapshot(LinkTier::Degraded, SidebarLinkFreshness::Fresh),
        21_000,
    );
    assert_eq!(
        alert,
        Some(LinkAlert {
            tier: LinkTier::Degraded,
            rtt_ms: Some(230),
            miss_pct: 4,
            since_ms: 11_000,
            recovered_after_ms: None,
        })
    );

    // Recovery direction: from an active degraded episode the first good sample
    // starts the recovery clock, a stale span pauses it, and the stale span is
    // not counted toward recovery either.
    let mut state = LinkNotificationState::default();
    state.evaluate(
        &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
        0,
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
                10_000,
            )
            .is_some(),
        "the link first enters an active degraded episode"
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
                10_000,
            )
            .is_none(),
        "the first good sample starts the recovery clock"
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Stale),
                39_000,
            )
            .is_none()
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
                50_000,
            )
            .is_none(),
        "the stale span is not counted toward recovery"
    );

    let alert = state.evaluate(
        &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
        51_000,
    );
    assert_eq!(
        alert,
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
fn link_recovery_closes_diagnostic_after_good_hysteresis() {
    let mut state = LinkNotificationState::default();
    state.evaluate(
        &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
        0,
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
                10_000,
            )
            .is_some(),
        "the link first enters an active degraded episode"
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
                10_000,
            )
            .is_none(),
        "the first good sample starts the recovery clock"
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
                39_999,
            )
            .is_none(),
        "good health must hold before recovery"
    );
    let alert = state.evaluate(
        &link_snapshot(LinkTier::Good, SidebarLinkFreshness::Fresh),
        40_000,
    );

    assert_eq!(
        alert,
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
        0,
    );
    assert!(
        state
            .evaluate(
                &link_snapshot(LinkTier::Bad, SidebarLinkFreshness::Fresh),
                10_000,
            )
            .is_some(),
        "the link first enters an active degraded episode"
    );

    let alert = state.evaluate(&snapshot(Vec::new()), 15_000);

    assert_eq!(
        alert,
        Some(LinkAlert {
            tier: LinkTier::Good,
            rtt_ms: None,
            miss_pct: 0,
            since_ms: 0,
            recovered_after_ms: Some(15_000),
        })
    );
    assert!(
        state.evaluate(&snapshot(Vec::new()), 16_000).is_none(),
        "the expiry close is emitted once"
    );
}

#[test]
fn command_spawn_receives_notification_env() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("env.txt");
    let command = format!(
        "printf '%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n%s\\n' \"$RIMZ_NOTIFY_TITLE\" \"$RIMZ_NOTIFY_BODY\" \"$RIMZ_NOTIFY_AGENT\" \"$RIMZ_NOTIFY_KIND\" \"${{RIMZ_NOTIFY_UNREAD-unset}}\" \"$RIMZ_NOTIFY_PANE\" \"$RIMZ_NOTIFY_ROOT\" \"$RIMZ_NOTIFY_ASK\" > {}",
        sh_quote(&out)
    );
    let notification = Notification {
        agents: vec![NotificationAgent {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("sess-1"),
            label: "claude sess-1".to_owned(),
            handle: "claude sess-1".to_owned(),
            worktree: None,
            task: None,
            pane_id: Some(PaneId::from_parts(MuxName::Tmux, "%9")),
            root: Some("/repo".to_owned()),
            ask_id: Some(crate::ids::AskId::parse("ask_0123456789abcdef").unwrap()),
            new_status: Some(AgentStatus::Waiting),
        }],
        notification_kind: NotificationKind::Waiting,
        title: "RimZ: claude needs you".to_owned(),
        body: "claude sess-1 is waiting for input.".to_owned(),
        unread_count: None,
    };

    let prefs = NotificationsPrefs {
        command: Some(command),
        ..NotificationsPrefs::default()
    };
    assert_eq!(spawn_notify_handlers(&prefs, &notification), 1);

    let expected = "RimZ: claude needs you\nclaude sess-1 is waiting for input.\nclaude sess-1\nwaiting\nunset\ntmux:%9\n/repo\nask_0123456789abcdef\n";
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
fn handlers_spawn_only_matching_conditions_and_shell_quote_templates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("matched.txt");
    let miss = dir.path().join("miss.txt");
    let prefs = NotificationsPrefs {
        handler: vec![
            crate::config::NotifyHandler {
                command: format!("printf nope > {}", sh_quote(&miss)),
                when: crate::config::NotifyCondition {
                    kind: vec![NotificationKind::Failed],
                    ..crate::config::NotifyCondition::default()
                },
                ..crate::config::NotifyHandler::default()
            },
            crate::config::NotifyHandler {
                command: format!(
                    "printf '%s\\n%s\\n%s\\n' {{{{agent}}}} {{{{handle}}}} {{{{task}}}} > {}",
                    sh_quote(&out)
                ),
                when: crate::config::NotifyCondition {
                    kind: vec![NotificationKind::Waiting],
                    worktree: vec!["feat/*".to_owned()],
                    handle: vec!["@planner".to_owned()],
                },
                ..crate::config::NotifyHandler::default()
            },
        ],
        ..NotificationsPrefs::default()
    };
    let notification = Notification {
        agents: vec![NotificationAgent {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: AgentSessionId::from("sess-1"),
            label: "danger task".to_owned(),
            handle: "planner".to_owned(),
            worktree: Some("feat/ntfy".to_owned()),
            task: Some("\"; rm -rf /".to_owned()),
            pane_id: None,
            root: None,
            ask_id: None,
            new_status: Some(AgentStatus::Waiting),
        }],
        notification_kind: NotificationKind::Waiting,
        title: "RimZ: danger".to_owned(),
        body: "body".to_owned(),
        unread_count: None,
    };

    assert_eq!(spawn_notify_handlers(&prefs, &notification), 1);
    assert_eq!(wait_for_text(&out), "planner\nplanner\n\"; rm -rf /\n");
    assert!(!miss.exists());
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
        title: "RimZ: 2 unread need you".to_owned(),
        body: "2 unread rows still need you.".to_owned(),
        unread_count: Some(2),
    };

    let prefs = NotificationsPrefs {
        command: Some(command),
        ..NotificationsPrefs::default()
    };
    assert_eq!(spawn_notify_handlers(&prefs, &notification), 1);

    let expected = "RimZ: 2 unread need you\n2 unread rows still need you.\n\nreminder\n2\n";
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

fn wait_for_text(path: &std::path::Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::read_to_string(path).expect("command wrote file")
}
