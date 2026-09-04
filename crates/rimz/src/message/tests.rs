use std::time::Duration;

use jiff::Timestamp;

use super::*;
use crate::agents::{
    AgentContext, AgentState, AgentStatus, LifecycleSignal, TurnSettle, TurnSettleOutcome,
};
use crate::ids::{AgentKind, MuxName, PaneId, WorkspaceId};
use crate::store::message::MessageStatus;
use crate::store::message::tests::{
    after_condition, agent, agent_sender, condition_snapshot, when_condition,
};
use crate::store::snapshot::PaneAgent;

#[test]
fn command_segments_separate_arguments_from_the_slash_token() {
    assert_eq!(command_segments("/compact", "/compact"), ("/compact", None));
    assert_eq!(
        command_segments("/compact keep the open questions", "/compact"),
        ("/compact ", Some("keep the open questions"))
    );
    assert_eq!(
        command_segments("/compact ", "/compact"),
        ("/compact ", None)
    );
    assert_eq!(
        command_segments("/context compact", "/context compact"),
        ("/context compact", None)
    );
    assert_eq!(
        command_segments("/context compact keep it", "/context compact"),
        ("/context compact ", Some("keep it"))
    );
}

#[test]
fn draft_record_uses_recipient_identity_and_live_pane_context() {
    let agent = agent("sess-a", Some("lucid-atlas"));
    let pane = pane(
        "claude",
        Some("sess-a"),
        Some("lucid-atlas"),
        Some("auth"),
        None,
        "terminal_1",
    );
    let workspace_id = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let not_before = Timestamp::now();

    let live = draft(Some(not_before)).record(
        workspace_id.clone(),
        Recipient::Agent {
            agent: &agent,
            pane: Some(&pane),
        },
        None,
        "hello",
        Some("@claude"),
    );
    let parked = draft(Some(not_before)).record(
        workspace_id,
        Recipient::Agent {
            agent: &agent,
            pane: None,
        },
        None,
        "hello",
        Some("@claude"),
    );

    assert_eq!(live.agent_id, agent.agent_id);
    assert_eq!(live.pane_id.as_ref(), Some(&pane.pane_id));
    assert_eq!(live.channel.as_deref(), Some("auth"));
    assert_eq!(live.not_before, Some(not_before));
    assert_eq!(parked.pane_id, None);
    assert_eq!(parked.channel, None);
}

#[test]
fn draft_record_normalizes_bound_provisional_lazy_and_pane_identity() {
    let workspace_id = WorkspaceId::from_project_root(std::path::Path::new("/repo"));
    let mut bound = agent("sess-a", Some("lucid-atlas"));
    bound.channel = Some("bound-channel".to_owned());
    let bound_pane = pane(
        "claude",
        Some("pane-session"),
        Some("pane-name"),
        Some("pane-channel"),
        Some("/repo/pane-worktree"),
        "terminal_1",
    );
    let record = pane_record(workspace_id.clone(), &bound_pane, Some(&bound), "scope");
    assert_eq!(record.agent_id, bound.agent_id);
    assert_eq!(record.agent_name, bound.name);
    assert_eq!(record.channel.as_deref(), Some("bound-channel"));
    assert_eq!(record.pane_id.as_ref(), Some(&bound_pane.pane_id));

    let mut provisional = agent("sess-a", Some("lucid-atlas"));
    provisional.agent_id = AgentSessionId::from("launch_pending");
    provisional.channel = None;
    let provisional_pane = pane(
        "claude",
        None,
        None,
        Some("provisional-channel"),
        None,
        "terminal_2",
    );
    let record = agent_record(
        workspace_id.clone(),
        &provisional,
        Some(&provisional_pane),
        "scope",
    );
    assert_eq!(record.agent_id.as_str(), "launch_pending");
    assert_eq!(record.channel.as_deref(), Some("provisional-channel"));
    assert_eq!(record.pane_id.as_ref(), Some(&provisional_pane.pane_id));

    let lazy = pane("codex", None, None, None, Some("/repo/lazy"), "terminal_3");
    let record = pane_record(workspace_id.clone(), &lazy, None, "scope");
    assert_eq!(record.agent_id, synthetic_session_for_pane(&lazy.pane_id));
    assert_eq!(record.channel.as_deref(), Some("lazy"));

    let pane_only = pane(
        "codex",
        Some("pane-session"),
        Some("pane-name"),
        None,
        None,
        "terminal_4",
    );
    let record = pane_record(workspace_id.clone(), &pane_only, None, "scope");
    assert_eq!(record.agent_id.as_str(), "pane-session");
    assert_eq!(record.agent_name.as_deref(), Some("pane-name"));
    assert_eq!(record.channel.as_deref(), Some("scope"));

    let fresh = pane("codex", None, None, None, None, "terminal_5");
    let record = pane_record(workspace_id, &fresh, None, "explicit");
    assert_eq!(record.channel.as_deref(), Some("explicit"));
}

#[test]
fn user_input_requires_plain_human_delivery() {
    let human = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &agent("human", None),
        "prompt".to_owned(),
        true,
        DeliveryGate::Done,
    );
    assert!(human.is_user_input());
    assert!(!human.clone().with_automated(true).is_user_input());

    let mut resume = human.clone();
    resume.gate = DeliveryGate::Resume;
    assert!(!resume.is_user_input());
    assert!(
        !human
            .with_sender(agent_sender("coder", None))
            .is_user_input()
    );
}

#[test]
fn delivery_gates_follow_agent_lifecycle() {
    let mut running = agent("sess-interrupted", None);
    running.status = AgentStatus::Running;
    running.phase = crate::agents::TurnPhase::Reasoning;
    running.context = Some(settle_context(
        Some(running.last_activity),
        TurnSettleOutcome::Interrupted,
    ));
    let now = Timestamp::now();
    assert!(gate_open_for_agent(
        DeliveryGate::Done,
        &running,
        false,
        now
    ));
    assert!(gate_open_for_agent(DeliveryGate::Any, &running, false, now));

    let mut parked = agent("sess-parked", None);
    parked.status = AgentStatus::Running;
    parked.phase = crate::agents::TurnPhase::Parked;
    assert!(gate_open_for_agent(DeliveryGate::Done, &parked, false, now));
    assert!(gate_open_for_agent(DeliveryGate::Any, &parked, false, now));
    assert!(!gate_open_for_agent(
        DeliveryGate::Resume,
        &parked,
        false,
        now
    ));

    let mut plan = agent("sess-plan", None);
    plan.status = AgentStatus::Running;
    plan.phase = crate::agents::TurnPhase::Reasoning;
    plan.context = Some(settle_context(
        Some(plan.last_activity),
        TurnSettleOutcome::PlanProposed,
    ));
    assert!(plan.is_awaiting_input());
    assert!(!gate_open_for_agent(DeliveryGate::Any, &plan, false, now));

    let mut stale = running.clone();
    stale.last_activity += jiff::SignedDuration::from_secs(2);
    assert!(!gate_open_for_agent(DeliveryGate::Done, &stale, false, now));

    let mut compacting = agent("sess-compacting", None);
    compacting.status = AgentStatus::Idle;
    compacting.compacting_since = Some(now);
    for gate in [DeliveryGate::Done, DeliveryGate::Any, DeliveryGate::Resume] {
        assert!(!gate_open_for_agent(gate, &compacting, true, now));
    }
}

#[test]
fn when_condition_uses_raw_status_and_status_specific_dwell_base() {
    let now = Timestamp::from_second(10_000).unwrap();
    let cases = [
        (AgentStatus::Running, Some(9_900), None, 9_950),
        (AgentStatus::Waiting, None, Some(9_900), 9_950),
        (AgentStatus::Idle, None, None, 9_900),
        (AgentStatus::Success, None, None, 9_900),
        (AgentStatus::Failed, None, None, 9_900),
    ];
    for (status, turn_started, waiting_since, last_activity) in cases {
        let mut watched = agent("watched", Some("coder"));
        watched.status = status;
        watched.turn_started_at = turn_started.map(|secs| Timestamp::from_second(secs).unwrap());
        watched.waiting_since = waiting_since.map(|secs| Timestamp::from_second(secs).unwrap());
        watched.last_activity = Timestamp::from_second(last_activity).unwrap();
        let condition = when_condition(&watched, status, 75, None);
        let snapshot = condition_snapshot(vec![watched]);
        assert!(
            deliver::evaluate_when_condition(&condition, &snapshot, now, Duration::from_secs(30))
                .check
                .met,
            "{status:?}"
        );
    }

    let mut running = agent("running", None);
    running.status = AgentStatus::Running;
    running.last_activity = Timestamp::from_second(9_950).unwrap();
    let condition = when_condition(&running, AgentStatus::Running, 75, None);
    let snapshot = condition_snapshot(vec![running]);
    let check =
        deliver::evaluate_when_condition(&condition, &snapshot, now, Duration::from_secs(30)).check;
    assert!(!check.met);
    assert_eq!(check.trip_at, Some(Timestamp::from_second(10_025).unwrap()));
}

#[test]
fn when_condition_reports_mismatch_gone_and_preserves_latch() {
    let now = Timestamp::from_second(10_000).unwrap();
    let watched = agent("watched", Some("coder"));
    let pending = when_condition(&watched, AgentStatus::Running, 60, None);
    let snapshot = condition_snapshot(vec![watched.clone()]);
    let mismatch =
        deliver::evaluate_when_condition(&pending, &snapshot, now, Duration::from_secs(30));
    assert!(!mismatch.check.met);
    assert_eq!(mismatch.check.trip_at, None);
    let gone = deliver::evaluate_when_condition(
        &pending,
        &condition_snapshot(Vec::new()),
        now,
        Duration::from_secs(30),
    );
    assert_eq!(
        gone.archive_reason.as_deref(),
        Some(pending.expiry_reason().as_str())
    );

    let latched = when_condition(&watched, AgentStatus::Running, 60, Some(now));
    assert!(
        deliver::evaluate_when_condition(
            &latched,
            &condition_snapshot(Vec::new()),
            now,
            Duration::from_secs(30),
        )
        .check
        .met
    );

    let receiver = agent("receiver", None);
    let blocked = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &receiver,
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_after(vec![after_condition(&watched, Some(now))])
    .with_when(vec![pending]);
    assert!(!blocked.is_deliverable(now));
    assert!(blocked.with_when(vec![latched]).is_deliverable(now));
}

#[test]
fn when_parser_accepts_literal_statuses_and_duration_units() {
    for status in ["running", "waiting", "idle", "success", "failed"] {
        assert_eq!(parse_when_status(status).unwrap().as_str(), status);
    }
    assert!(
        parse_when_status("paused")
            .unwrap_err()
            .contains("supported statuses")
    );
    assert_eq!(parse_when_duration("58m").unwrap(), 3_480);
    assert!(
        parse_when_duration("0m")
            .unwrap_err()
            .contains("greater than zero")
    );
}

#[test]
fn delivery_checkpoint_recognizes_turn_boundaries() {
    let checkpoint = crate::agents::DELIVERY_CHECKPOINT;
    assert!(checkpoint.contains(&LifecycleSignal::TurnInterrupted { turn_id: None }));
    assert!(checkpoint.contains(&LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
    }));
    assert!(checkpoint.contains(&LifecycleSignal::TurnEnded {
        errored: true,
        parked_on_background: false,
    }));
    assert!(checkpoint.contains(&LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: true,
    }));
    assert!(!checkpoint.contains(&LifecycleSignal::Registered));
    assert!(checkpoint.contains(&LifecycleSignal::CompactionEnded {
        auto: None,
        failed: false,
    }));
    assert!(checkpoint.contains(&LifecycleSignal::CompactionEnded {
        auto: Some(false),
        failed: true,
    }));
    assert!(!checkpoint.contains(&LifecycleSignal::SubagentStopped { errored: false }));
}

#[test]
fn after_condition_requires_an_open_gate_and_quiescent_ready_queue() {
    let now = Timestamp::now();
    let mut upstream = agent("sess-upstream", Some("planner"));
    let condition = after_condition(&upstream, None);

    for status in [AgentStatus::Running, AgentStatus::Waiting] {
        upstream.status = status;
        assert!(
            !deliver::evaluate_after_condition(
                &condition,
                DeliveryGate::Done,
                &[],
                &condition_snapshot(vec![upstream.clone()]),
                now
            )
            .check
            .met
        );
    }

    upstream.status = AgentStatus::Failed;
    assert!(
        !deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            &[],
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );
    assert!(
        deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Any,
            &[],
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );

    upstream.status = AgentStatus::Idle;
    let ready = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-message")),
        &upstream,
        "work".to_owned(),
        true,
        DeliveryGate::Done,
    );
    assert!(
        !deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            std::slice::from_ref(&ready),
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );
    let sent = MessageRecord {
        status: MessageStatus::Sent,
        ..ready.clone()
    };
    assert!(
        !deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            std::slice::from_ref(&sent),
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );
    let scheduled = ready.with_not_before(Some(now + jiff::SignedDuration::from_secs(60)));
    assert!(
        deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            std::slice::from_ref(&scheduled),
            &condition_snapshot(vec![upstream.clone()]),
            now
        )
        .check
        .met
    );
    assert!(
        deliver::evaluate_after_condition(
            &condition,
            DeliveryGate::Done,
            &[],
            &condition_snapshot(vec![upstream]),
            now
        )
        .check
        .met
    );
}

#[test]
fn schedule_parser_accepts_durations_and_rolls_wall_clock_forward() {
    let now = jiff::civil::date(2026, 6, 24)
        .at(8, 0, 0, 0)
        .in_tz("UTC")
        .unwrap();
    assert_eq!(
        parse_schedule_at("90s", &now).unwrap(),
        now.timestamp() + jiff::SignedDuration::from_secs(90)
    );
    assert_eq!(
        parse_schedule_at("08:30", &now).unwrap(),
        jiff::civil::date(2026, 6, 24)
            .at(8, 30, 0, 0)
            .in_tz("UTC")
            .unwrap()
            .timestamp()
    );
    assert_eq!(
        parse_schedule_at("07:30", &now).unwrap(),
        jiff::civil::date(2026, 6, 25)
            .at(7, 30, 0, 0)
            .in_tz("UTC")
            .unwrap()
            .timestamp()
    );
    assert!(parse_schedule_at("0s", &now).is_err());
    assert!(parse_schedule_at("tomorrow", &now).is_err());
    assert!(parse_schedule_at("25:00", &now).is_err());
}

fn draft(not_before: Option<Timestamp>) -> MessageDraft {
    MessageDraft {
        body: MessageBody::Prompt,
        enter: true,
        gate: DeliveryGate::Done,
        sender: MessageSender::Human,
        automated: false,
        force: false,
        auto_compact: None,
        not_before,
        after: Vec::new(),
        when: Vec::new(),
    }
}

fn agent_record(
    workspace_id: WorkspaceId,
    agent: &AgentState,
    pane: Option<&PaneAgent>,
    channel: &str,
) -> MessageRecord {
    draft(None).record(
        workspace_id,
        Recipient::Agent { agent, pane },
        Some(channel),
        "hello",
        Some("@claude"),
    )
}

fn pane_record(
    workspace_id: WorkspaceId,
    pane: &PaneAgent,
    bound: Option<&AgentState>,
    channel: &str,
) -> MessageRecord {
    draft(None).record(
        workspace_id,
        Recipient::Pane { pane, bound },
        Some(channel),
        "hello",
        Some("@claude"),
    )
}

fn pane(
    kind: &str,
    agent_id: Option<&str>,
    name: Option<&str>,
    channel: Option<&str>,
    worktree_path: Option<&str>,
    raw: &str,
) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::new_unchecked(kind),
        kind_ordinal: None,
        name: name.map(ToOwned::to_owned),
        name_explicit: false,
        profile: None,
        role: None,
        channel: channel.map(ToOwned::to_owned),
        agent_id: agent_id.map(AgentSessionId::from),
        pane_id: PaneId::from_parts(MuxName::Zellij, raw),
        pane_pid: None,
        worktree_path: worktree_path.map(ToOwned::to_owned),
        worktree_branch: None,
    }
}

/// A Codex sidecar whose resting marker postdates `after` by one second.
fn settle_context(after: Option<Timestamp>, outcome: TurnSettleOutcome) -> AgentContext {
    AgentContext {
        settle: after.map(|at| TurnSettle::new(at + jiff::SignedDuration::from_secs(1), outcome)),
        ..AgentContext::new("codex", Timestamp::now())
    }
}
