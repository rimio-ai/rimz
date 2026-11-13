use jiff::Timestamp;

use super::*;
use crate::agents::ParkDemotion;
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::ids::WorkspaceId;
use crate::store::message::{DeliveryGate, MessageStatus};

fn wait_store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let paths = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    (dir, store)
}

#[test]
fn wake_in_flight_keeps_a_rested_agent_sleeping() {
    let mut agent = crate::testkit::agent_state("claude", "sess-wake", Timestamp::UNIX_EPOCH);
    agent.status = AgentStatus::Success;
    agent.turn_started_at = Some(Timestamp::UNIX_EPOCH);
    let workspace = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let view = |sender, status| {
        let mut message = MessageRecord::new(
            workspace.clone(),
            &agent,
            "wake".to_owned(),
            DeliveryGate::Done,
        );
        message.sender = sender;
        message.status = status;
        TurnWaitView {
            snapshot: SidebarSnapshot::build_with_agents(
                workspace.clone(),
                vec![agent.clone()],
                Timestamp::now(),
            ),
            messages: vec![message],
            now: Timestamp::now(),
            card_status: CardStatus::from_demotion(ParkDemotion::default()),
        }
    };
    let wake = |notice| MessageSender::Harness { notice };
    for notice in [HarnessNotice::Wait, HarnessNotice::Signal] {
        for status in [
            MessageStatus::Queued,
            MessageStatus::Claimed,
            MessageStatus::Sent,
        ] {
            let view = view(wake(notice.clone()), status);
            assert_eq!(view.status(&agent), AgentStatus::Sleeping, "{status:?}");
            assert_eq!(view.completion(&agent), TurnCompletion::Open);
            assert_eq!(view.reason(&agent), Some(TurnWaitReason::WakeInFlight));
            let mut idle = agent.clone();
            idle.status = AgentStatus::Idle;
            assert_eq!(view.reason(&idle), Some(TurnWaitReason::WakeInFlight));
        }
    }
    let delivered = view(wake(HarnessNotice::Wait), MessageStatus::Delivered);
    assert_eq!(delivered.completion(&agent), TurnCompletion::Completed);
    for notice in [
        HarnessNotice::SubagentReport,
        HarnessNotice::AgentReport,
        HarnessNotice::TeamReport,
    ] {
        for status in [
            MessageStatus::Queued,
            MessageStatus::Claimed,
            MessageStatus::Sent,
            MessageStatus::Delivered,
        ] {
            let report = view(wake(notice.clone()), status);
            assert_eq!(report.completion(&agent), TurnCompletion::Completed);
            assert!(!report.wake_in_flight(&agent, false));
            assert_eq!(report.wake_in_flight(&agent, true), !status.is_terminal());
        }
    }
}

#[test]
fn turn_wait_reason_projects_each_open_status() {
    let workspace = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let view = TurnWaitView {
        snapshot: SidebarSnapshot::build_with_agents(workspace, vec![], Timestamp::now()),
        messages: vec![],
        now: Timestamp::now(),
        card_status: CardStatus::from_demotion(ParkDemotion::default()),
    };
    for (status, expected) in [
        (AgentStatus::Running, TurnWaitReason::Running),
        (AgentStatus::Waiting, TurnWaitReason::WaitingOnPerson(None)),
        (AgentStatus::Sleeping, TurnWaitReason::Sleeping(vec![])),
        (AgentStatus::Paused, TurnWaitReason::Paused),
        (AgentStatus::Idle, TurnWaitReason::NoTurnStarted),
        (AgentStatus::Success, TurnWaitReason::NoTurnStarted),
    ] {
        let mut agent = crate::testkit::agent_state("claude", "sess-reason", Timestamp::UNIX_EPOCH);
        agent.status = status;
        agent.turn_started_at = None;
        assert_eq!(view.reason(&agent), Some(expected), "{status:?}");
    }
    let mut agent = crate::testkit::agent_state("claude", "sess-done", Timestamp::UNIX_EPOCH);
    agent.status = AgentStatus::Success;
    agent.turn_started_at = Some(Timestamp::UNIX_EPOCH);
    assert_eq!(view.reason(&agent), None);
}

#[test]
fn provider_error_settles_the_turn_until_new_activity() {
    use crate::agents::{AgentContext, AgentTurnError, TurnErrorClass};

    let at = Timestamp::from_second(1_000).unwrap();
    let now = Timestamp::from_second(2_000).unwrap();
    let workspace = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let view = TurnWaitView {
        snapshot: SidebarSnapshot::build_with_agents(workspace, vec![], now),
        messages: vec![],
        now,
        card_status: CardStatus::from_demotion(ParkDemotion::default()),
    };
    for (class, second, completion, reason) in [
        (
            TurnErrorClass::PausedOverloaded,
            1_010,
            TurnCompletion::Open,
            Some(TurnWaitReason::Paused),
        ),
        (
            TurnErrorClass::Failed,
            1_000,
            TurnCompletion::Open,
            Some(TurnWaitReason::Running),
        ),
        (TurnErrorClass::Failed, 1_010, TurnCompletion::Failed, None),
    ] {
        let mut agent = crate::testkit::agent_state("claude", "sess-error", at);
        agent.status = AgentStatus::Running;
        agent.turn_started_at = Some(at);
        agent.context = Some(AgentContext {
            turn_error: Some(AgentTurnError {
                ended_turn: false,
                class,
                at: Timestamp::from_second(second).unwrap(),
                label: None,
            }),
            ..AgentContext::new("claude", now)
        });
        assert_eq!(view.completion(&agent), completion, "{class:?} at {second}");
        assert_eq!(view.reason(&agent), reason, "{class:?} at {second}");
    }
}

#[test]
fn spent_provider_pauses_end_turns_but_recoverable_and_budget_parks_stay_open() {
    use crate::agents::TurnErrorClass::{PausedOverloaded, PausedRateLimit, PausedSpendLimit};
    use crate::agents::{
        AgentContext, AgentTurnError, BudgetPark, BudgetScope, BudgetWindow, ProviderCapacity,
        RateLimitWindow,
    };
    use TurnCompletion::{Failed, Open};

    let at = Timestamp::from_second(1_000).unwrap();
    let now = Timestamp::from_second(2_000).unwrap();
    let workspace = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    for (class, exhausted, reset_offset, budget, expected) in [
        (PausedRateLimit, true, None, false, Failed),
        (PausedSpendLimit, true, None, false, Failed),
        (PausedOverloaded, true, None, false, Failed),
        (PausedRateLimit, false, Some(-1), false, Failed),
        (PausedSpendLimit, false, Some(-1), false, Failed),
        (PausedRateLimit, false, Some(1), false, Open),
        (PausedRateLimit, false, None, false, Open),
        (PausedOverloaded, false, Some(-1), false, Open),
        (PausedRateLimit, true, None, true, Open),
        (PausedRateLimit, false, Some(-1), true, Open),
    ] {
        let mut agent = crate::testkit::agent_state("claude", "sess-error", at);
        agent.status = AgentStatus::Running;
        agent.turn_started_at = Some(at);
        agent.context = Some(AgentContext {
            turn_error: Some(AgentTurnError {
                ended_turn: false,
                class,
                at: at + jiff::SignedDuration::from_secs(1),
                label: None,
            }),
            ..AgentContext::new("claude", now)
        });
        agent.budget_park = budget.then_some(BudgetPark {
            cap_usd: 1.0,
            spend_usd: 2.0,
            window: BudgetWindow::Day,
            at,
            scope: BudgetScope::Turn,
            account_kind: None,
            resets_at: None,
            reset_window: None,
        });
        let capacities = reset_offset
            .map(|offset| {
                [(
                    agent.login_key(),
                    ProviderCapacity::from_windows(vec![RateLimitWindow {
                        used_percentage: Some(100),
                        resets_at: Some(now + jiff::SignedDuration::from_secs(offset)),
                        ..Default::default()
                    }]),
                )]
                .into()
            })
            .unwrap_or_default();
        let exhausted = if exhausted {
            [(agent.kind.clone(), agent.agent_id.clone())].into()
        } else {
            Default::default()
        };
        let view = TurnWaitView {
            snapshot: SidebarSnapshot::build_with_agents(workspace.clone(), vec![], now),
            messages: vec![],
            now,
            card_status: CardStatus::from_demotion(ParkDemotion::new(&capacities, exhausted, now)),
        };
        assert_eq!(
            view.completion(&agent),
            expected,
            "{class:?}, budget: {budget}"
        );
        assert_eq!(
            view.reason(&agent),
            (expected == TurnCompletion::Open).then_some(TurnWaitReason::Paused),
            "{class:?}, budget: {budget}"
        );
    }
}

#[test]
fn spent_pause_evidence_is_lazy_and_cached_per_view() {
    use crate::agents::{
        AgentContext, AgentRateLimits, AgentTurnError, RateLimitCacheEntry, RateLimitWindow,
        RateLimitsCache, TurnErrorClass,
    };

    let (_dir, store) = wait_store();
    let at = Timestamp::from_second(1_000).unwrap();
    let now = Timestamp::from_second(2_000).unwrap();
    let mut agent = crate::testkit::agent_state("claude", "sess-error", at);
    agent.status = AgentStatus::Running;
    agent.turn_started_at = Some(at);
    let mut view = TurnWaitView::load(&store).unwrap();
    assert!(!view.wake_in_flight(&agent, true));
    assert!(!view.soft_compact_in_flight(&agent));
    assert_eq!(view.completion(&agent), TurnCompletion::Open);
    assert!(!view.card_status.evidence_built());

    agent.context = Some(AgentContext {
        turn_error: Some(AgentTurnError {
            ended_turn: false,
            class: TurnErrorClass::PausedRateLimit,
            at: at + jiff::SignedDuration::from_secs(1),
            label: None,
        }),
        ..AgentContext::new("claude", now)
    });
    view.snapshot = SidebarSnapshot::build_with_agents(
        store.paths().workspace_id.clone(),
        vec![agent.clone()],
        now,
    );
    assert!(!view.wake_in_flight(&agent, true));
    assert!(!view.soft_compact_in_flight(&agent));
    assert!(!view.card_status.evidence_built());
    assert_eq!(view.reason(&agent), Some(TurnWaitReason::Paused));

    let cache = RateLimitsCache {
        entries: [(
            agent.login_key().into(),
            RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(100),
                        resets_at: Some(now - std::time::Duration::from_secs(1)),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    };
    crate::disk::atomic::write_temp_then_rename_cache(
        &RateLimitsCache::shared_path(store.runtime_paths()),
        &cache,
    )
    .unwrap();
    assert_eq!(view.reason(&agent), Some(TurnWaitReason::Paused));
    let mut reloaded = TurnWaitView::load(&store).unwrap();
    reloaded.snapshot = view.snapshot;
    assert_eq!(reloaded.completion(&agent), TurnCompletion::Failed);
    assert_eq!(reloaded.reason(&agent), None);
}

#[test]
fn turn_wait_reason_defers_person_until_permission_review_expires() {
    let at = Timestamp::from_second(1_000).unwrap();
    let mut agent = crate::testkit::agent_state("codex", "child", at);
    agent.status = AgentStatus::Waiting;
    agent.parent_agent_id = Some("parent".into());
    agent.launch_depth = Some(1);
    agent.mode = Some(crate::agents::PermissionMode::Auto);
    agent.waiting_since = Some(at);
    let ask = crate::agents::OpenAsk {
        extra_questions: 0,
        id: crate::ids::AskId::new(),
        kind: crate::agents::AskKind::Permission,
        detail: None,
        native_key: None,
        since: at,
    };
    agent.open_ask = Some(ask.clone());
    let workspace = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    for (second, reason) in [
        (1_059, TurnWaitReason::Running),
        (1_060, TurnWaitReason::WaitingOnPerson(Some(ask))),
    ] {
        let now = Timestamp::from_second(second).unwrap();
        let view = TurnWaitView {
            snapshot: SidebarSnapshot::build_with_agents(
                workspace.clone(),
                vec![agent.clone()],
                now,
            ),
            messages: vec![],
            now,
            card_status: CardStatus::from_demotion(ParkDemotion::default()),
        };
        assert_eq!(view.reason(&agent), Some(reason));
    }
}

#[test]
fn overdue_sent_wake_releases_sleeping_without_reconciliation() {
    let now = Timestamp::from_second(1_700_000_100).unwrap();
    let mut agent = crate::testkit::agent_state("claude", "sess-wake", now);
    agent.turn_started_at = Some(Timestamp::UNIX_EPOCH);
    let workspace = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    for status in [AgentStatus::Success, AgentStatus::Idle] {
        agent.status = status;
        for notice in [HarnessNotice::Wait, HarnessNotice::Signal] {
            let mut message = MessageRecord::new(
                workspace.clone(),
                &agent,
                "wake".to_owned(),
                DeliveryGate::Done,
            )
            .with_sender(MessageSender::Harness { notice });
            message.status = MessageStatus::Sent;
            let overdue =
                now - message.body.delivery_window() - std::time::Duration::from_millis(1);
            for (sent_at, retry_after, compacting, held) in [
                (overdue, None, false, false),
                (now, None, false, true),
                (
                    overdue,
                    Some(now + message.body.delivery_window()),
                    false,
                    true,
                ),
                (overdue, None, true, true),
                (overdue, Some(overdue), true, true),
            ] {
                agent.compacting_since = compacting.then_some(
                    now - jiff::SignedDuration::from_secs(
                        crate::agents::COMPACTING_WINDOW_SECS + 1,
                    ),
                );
                message.updated_at = sent_at;
                message.last_sent_at = Some(sent_at);
                message.retry_after = retry_after;
                let view = TurnWaitView {
                    snapshot: SidebarSnapshot::build_with_agents(
                        workspace.clone(),
                        vec![agent.clone()],
                        now,
                    ),
                    messages: vec![message.clone()],
                    now,
                    card_status: CardStatus::from_demotion(ParkDemotion::default()),
                };
                assert_eq!(
                    view.status(&agent),
                    if held { AgentStatus::Sleeping } else { status }
                );
                assert_eq!(
                    view.completion(&agent),
                    if held {
                        TurnCompletion::Open
                    } else {
                        TurnCompletion::Completed
                    }
                );
                assert_eq!(
                    view.reason(&agent),
                    held.then_some(TurnWaitReason::WakeInFlight)
                );
            }
        }
    }
}
