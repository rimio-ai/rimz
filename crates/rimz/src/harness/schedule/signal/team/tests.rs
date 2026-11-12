use super::*;
use crate::address::{addressable_agents, resolve_one, team_cohorts};
use crate::agents::state::TransitionMode;
use crate::agents::{AskKind, TurnPhase};
use crate::ids::{AgentSessionId, EventId, TeamInstanceId, WorkspaceId};
use crate::store::message::DeliveryGate;
use crate::store::snapshot::SidebarSnapshot;
use std::collections::BTreeMap;

fn member(name: &str, channel: &str, status: AgentStatus) -> AgentState {
    let mut member = crate::sidebar::test_support::root_agent("codex", name, None);
    member.name = Some(name.to_owned());
    member.role = Some(name.to_owned());
    member.team = Some("forge".to_owned());
    member.channel = Some(channel.to_owned());
    member.status = status;
    member
}

fn event(member: &AgentState, prior: AgentStatus, signal: LifecycleSignal) -> LifecycleEvent {
    let mut state = member.lifecycle();
    state.status = prior;
    state.phase = TurnPhase::Idle;
    let transition = crate::agents::step(
        Some(&state),
        None,
        crate::agents::lifecycle::PriorTurnIds::default(),
        &signal,
    );
    LifecycleEvent::new(
        EventId::new(),
        jiff::Timestamp::UNIX_EPOCH,
        WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        member.kind.clone(),
        member.agent_id.clone(),
        member.name.clone(),
        member.parent_agent_id.clone(),
        signal,
        Some(prior),
        transition,
    )
}

fn derive(event: &LifecycleEvent, rows: &[AgentState], pending: &[MessageRecord]) -> Vec<Signal> {
    derive_with_sleeping(event, rows, pending, &BTreeSet::new())
}

fn derive_with_sleeping(
    event: &LifecycleEvent,
    rows: &[AgentState],
    pending: &[MessageRecord],
    sleeping: &BTreeSet<(AgentKind, AgentSessionId)>,
) -> Vec<Signal> {
    let member = rows
        .iter()
        .find(|row| row.kind == event.kind && row.agent_id == event.agent_id)
        .unwrap();
    let instance = member.team_instance();
    let cohorts = team_cohorts(rows);
    let live = cohorts
        .iter()
        .find(|cohort| Some(&cohort.instance) == instance.as_ref())
        .map_or(&[][..], |cohort| cohort.members.as_slice());
    let snapshot = SidebarSnapshot::build_with_agents(
        event.workspace_id.clone(),
        crate::store::runtime::RuntimeProjection::runtime_from_refs(rows, Vec::new()).agents,
        event.at,
    );
    let audit = rows.to_vec().into();
    team_lifecycle_signals(
        event,
        member,
        live,
        &addressable_agents(&snapshot),
        pending,
        sleeping,
        &audit,
    )
}

fn turn_end() -> LifecycleSignal {
    LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    }
}

#[test]
fn team_idle_waits_for_all_members_and_empty_queues() {
    let coder = member("coder", "auth", AgentStatus::Success);
    let mut reviewer = member("reviewer", "auth", AgentStatus::Running);
    reviewer.parent_agent_id = Some(coder.agent_id.clone());
    reviewer.parent_agent_kind = Some(coder.kind.clone());
    reviewer.launch_depth = Some(1);
    let event = event(&coder, AgentStatus::Running, turn_end());
    let mut rows = [coder, reviewer];
    assert!(derive(&event, &rows, &[]).is_empty());
    rows[1].status = AgentStatus::Idle;
    for row in &rows {
        let sleeping = BTreeSet::from([(row.kind.clone(), row.agent_id.clone())]);
        assert!(derive_with_sleeping(&event, &rows, &[], &sleeping).is_empty());
    }
    let signals = derive(&event, &rows, &[]);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].name.as_str(), "team.idle");
    assert_eq!(signals[0].source, SignalSource::Lifecycle);
    assert_eq!(
        signals[0].payload["members"],
        json!([
            {"handle": "@reviewer#auth", "status": "idle"},
            {"handle": "@coder#auth", "status": "success"}
        ])
    );
    for gate in [DeliveryGate::Done, DeliveryGate::Any, DeliveryGate::Resume] {
        for row in &rows {
            let mut queued =
                MessageRecord::new(event.workspace_id.clone(), row, "work".to_owned(), gate);
            queued.agent_id = AgentSessionId::from("launch_provisional");
            queued.not_before = Some("2099-01-01T00:00:00Z".parse().unwrap());
            assert!(derive(&event, &rows, &[queued]).is_empty());
        }
    }
    let mut repeated = event.clone();
    repeated.prior_status = Some(AgentStatus::Success);
    assert!(derive(&repeated, &rows, &[]).is_empty());
    repeated.prior_status = Some(AgentStatus::Idle);
    assert!(derive(&repeated, &rows, &[]).is_empty());
}

#[test]
fn team_signals_are_scoped_to_instance() {
    let coder = member("coder", "auth", AgentStatus::Success);
    let docs = member("writer", "docs", AgentStatus::Running);
    let mut other_team = member("planner", "auth", AgentStatus::Running);
    other_team.team = Some("review".to_owned());
    let event = event(&coder, AgentStatus::Running, turn_end());
    let queued = MessageRecord::new(
        event.workspace_id.clone(),
        &docs,
        "work".to_owned(),
        DeliveryGate::Resume,
    );
    let sleeping = BTreeSet::from([
        (docs.kind.clone(), docs.agent_id.clone()),
        (other_team.kind.clone(), other_team.agent_id.clone()),
    ]);
    let signals = derive_with_sleeping(&event, &[coder, docs, other_team], &[queued], &sleeping);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].payload["team"], "forge");
    assert_eq!(signals[0].payload["instance"], "forge#auth");
    assert_eq!(signals[0].payload["member"], "@coder#auth");
    assert_eq!(
        signals[0].payload["members"],
        json!([
            {"handle": "@coder#auth", "status": "success"}
        ])
    );
}

#[test]
fn payload_handles_round_trip_beside_teamless_role_rivals() {
    let mut reviewer = member("team-reviewer", "auth", AgentStatus::Success);
    reviewer.role = Some("reviewer".to_owned());
    reviewer.kind_ordinal = Some(1);
    let coder = member("coder", "auth", AgentStatus::Idle);
    let mut rival = reviewer.clone();
    rival.agent_id = "inline-reviewer".into();
    rival.name = Some("inline-reviewer".to_owned());
    rival.team = None;
    rival.kind_ordinal = Some(2);
    let event = event(&reviewer, AgentStatus::Running, turn_end());
    let rows = [reviewer, coder, rival];
    let snapshot =
        SidebarSnapshot::build_with_agents(event.workspace_id.clone(), rows.to_vec(), event.at);
    let signals = derive(&event, &rows, &[]);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].name.as_str(), "team.idle");
    let payload = &signals[0].payload;
    assert_eq!(payload["members"].as_array().unwrap().len(), 2);
    for (handle, agent) in [
        (&payload["member"], &rows[0]),
        (&payload["members"][0]["handle"], &rows[1]),
        (&payload["members"][1]["handle"], &rows[0]),
    ] {
        let handle = handle.as_str().unwrap();
        let resolved = resolve_one(
            &snapshot,
            handle,
            None,
            &AddressContext {
                channel: Some("auth".parse().unwrap()),
                origin: ChannelOrigin::Explicit,
            },
        );
        assert_eq!(
            resolved.map(|agent| (&agent.kind, &agent.agent_id)),
            Ok((&agent.kind, &agent.agent_id)),
            "{handle}"
        );
    }
    assert_ne!(payload["member"], "@reviewer#auth");
}

#[test]
fn ended_member_uses_session_id_beside_later_tier_profile_rival() {
    let mut reviewer = member("team-reviewer", "auth", AgentStatus::Running);
    reviewer.role = Some("reviewer".to_owned());
    reviewer.agent_id = "sess-team-reviewer".into();
    let event = event(&reviewer, AgentStatus::Running, LifecycleSignal::Ended);
    reviewer.ended_at = Some(event.at);
    let mut rival = member("profile-reviewer", "auth", AgentStatus::Running);
    rival.team = None;
    rival.role = None;
    rival.profile = Some("reviewer".to_owned());
    let snapshot = SidebarSnapshot::build_with_agents(
        event.workspace_id.clone(),
        vec![rival.clone()],
        event.at,
    );
    let context = AddressContext {
        channel: Some("auth".parse().unwrap()),
        origin: ChannelOrigin::Explicit,
    };
    assert_eq!(
        resolve_one(&snapshot, "@reviewer#auth", None, &context)
            .map(|agent| (&agent.kind, &agent.agent_id)),
        Ok((&rival.kind, &rival.agent_id))
    );
    let signals = derive(&event, &[reviewer, rival], &[]);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].name.as_str(), "team.ended");
    for handle in [
        &signals[0].payload["member"],
        &signals[0].payload["members"][0]["handle"],
    ] {
        assert_eq!(handle, "@sess-team-reviewer#auth");
        let handle = handle.as_str().unwrap();
        let resolved = resolve_one(&snapshot, handle, None, &context);
        assert!(resolved.is_err(), "{handle}: {resolved:?}");
    }
}

#[test]
fn team_waiting_fires_once_per_entry() {
    let member = member("coder", "auth", AgentStatus::Waiting);
    let waiting = LifecycleSignal::AwaitingInput {
        extra_questions: 0,
        kind: AskKind::Question,
        ask_id: None,
        detail: None,
        native_key: None,
    };
    let entered = event(&member, AgentStatus::Running, waiting.clone());
    let repeated = event(&member, AgentStatus::Waiting, waiting.clone());
    let reentered = event(&member, AgentStatus::Running, waiting);
    let rows = [member];
    assert_eq!(
        derive(&entered, &rows, &[])[0].name.as_str(),
        "team.waiting"
    );
    assert!(derive(&repeated, &rows, &[]).is_empty());
    assert_eq!(
        derive(&reentered, &rows, &[])[0].name.as_str(),
        "team.waiting"
    );
}

#[test]
fn team_failed_requires_an_errored_turn_end() {
    let member = member("coder", "auth", AgentStatus::Failed);
    let failed = event(
        &member,
        AgentStatus::Running,
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let rows = [member];
    let signals = derive(&failed, &rows, &[]);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].name.as_str(), "team.failed");
    assert_eq!(signals[0].payload["members"][0]["status"], "failed");
    let ignored = event(&rows[0], AgentStatus::Failed, LifecycleSignal::Compacting);
    assert!(derive(&ignored, &rows, &[]).is_empty());
}

#[test]
fn team_ended_uses_the_final_audit_member_for_terminal_events() {
    for (terminal, status) in [
        (LifecycleSignal::Ended, "failed"),
        (LifecycleSignal::Lost, "running"),
    ] {
        let mut coder = member("coder", "auth", AgentStatus::Running);
        let mut reviewer = member("reviewer", "auth", AgentStatus::Success);
        reviewer.ended_at = Some(jiff::Timestamp::UNIX_EPOCH);
        let event = event(&coder, AgentStatus::Running, terminal.clone());
        if matches!(terminal, LifecycleSignal::Ended) {
            assert!(matches!(event.transition, LifecycleTransition::Normal));
            coder.ended_at = Some(event.at);
        } else {
            assert!(matches!(
                event.transition,
                LifecycleTransition::Ignored { .. }
            ));
        }
        let rows = [coder, reviewer];
        let cohorts = team_cohorts(&rows);
        if matches!(terminal, LifecycleSignal::Ended) {
            assert!(cohorts.is_empty());
        } else {
            assert!(rows[0].ended_at.is_none());
            assert_eq!(cohorts[0].members.len(), 1);
        }
        let signals = derive(&event, &rows, &[]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].name.as_str(), "team.ended");
        assert_eq!(signals[0].payload["instance"], "forge#auth");
        assert_eq!(signals[0].payload["member"], "@coder#auth");
        assert_eq!(
            signals[0].payload["members"],
            json!([
                {"handle": "@coder#auth", "status": status}
            ])
        );
    }
}

#[test]
fn terminal_member_leaving_only_resting_members_enters_idle_not_ended() {
    for terminal in [LifecycleSignal::Ended, LifecycleSignal::Lost] {
        let mut coder = member("coder", "auth", AgentStatus::Running);
        let reviewer = member("reviewer", "auth", AgentStatus::Success);
        let event = event(&coder, AgentStatus::Running, terminal.clone());
        if matches!(terminal, LifecycleSignal::Ended) {
            coder.ended_at = Some(event.at);
        }
        let mut rows = [coder, reviewer];
        let sleeping = BTreeSet::from([(rows[1].kind.clone(), rows[1].agent_id.clone())]);
        assert!(derive_with_sleeping(&event, &rows, &[], &sleeping).is_empty());
        let signals = derive(&event, &rows, &[]);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].name.as_str(), "team.idle");
        let mut already_idle = event.clone();
        already_idle.prior_status = Some(AgentStatus::Idle);
        assert!(derive(&already_idle, &rows, &[]).is_empty());
        let sleeping = BTreeSet::from([(rows[0].kind.clone(), rows[0].agent_id.clone())]);
        assert_eq!(
            derive_with_sleeping(&already_idle, &rows, &[], &sleeping)[0]
                .name
                .as_str(),
            "team.idle"
        );
        rows[1].status = AgentStatus::Running;
        assert!(derive(&event, &rows, &[]).is_empty());
    }
}

#[test]
fn provider_children_and_non_team_members_do_not_emit_team_signals() {
    let mut member = member("child", "auth", AgentStatus::Success);
    member.parent_agent_id = Some(AgentSessionId::from("parent"));
    let ended = event(&member, AgentStatus::Running, LifecycleSignal::Ended);
    assert!(derive(&ended, &[member.clone()], &[]).is_empty());
    member.parent_agent_id = None;
    member.team = None;
    assert!(derive(&ended, &[member], &[]).is_empty());
}

#[test]
fn a_ping_only_turn_emits_no_team_signal() {
    let mut seat = member("coder", "auth", AgentStatus::Success);
    seat.phase = TurnPhase::Idle;
    let rows = [seat.clone(), member("judge", "auth", AgentStatus::Idle)];
    let audit = |prior: &AgentState, signal: LifecycleSignal, mode| {
        let transition = AgentState::transition(Some(prior), &signal, mode);
        LifecycleEvent::new(
            EventId::new(),
            jiff::Timestamp::UNIX_EPOCH,
            WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            prior.kind.clone(),
            prior.agent_id.clone(),
            prior.name.clone(),
            None,
            signal,
            Some(prior.status),
            transition,
        )
    };
    let opened = audit(
        &seat,
        LifecycleSignal::TurnStarted { turn_id: None },
        TransitionMode::KeepaliveEdge,
    );
    assert_eq!(opened.status, AgentStatus::Success);
    assert!(derive(&opened, &rows, &[]).is_empty());
    seat.ping_turn = true;
    for signal in [
        turn_end(),
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
        },
    ] {
        let closed = audit(&seat, signal, TransitionMode::Edge);
        assert_eq!(closed.prior_status, Some(closed.status));
        assert!(
            derive(&closed, &rows, &[]).is_empty(),
            "no team.idle or team.failed for a ping"
        );
    }
}

#[test]
fn launched_child_stop_is_terminal_but_ignored_nonterminal_events_do_not_emit() {
    let mut child = member("child", "auth", AgentStatus::Success);
    child.parent_agent_id = Some(AgentSessionId::from("parent"));
    child.parent_agent_kind = Some(child.kind.clone());
    child.launch_depth = Some(1);
    let stopped = event(
        &child,
        AgentStatus::Running,
        LifecycleSignal::SubagentStopped { errored: false },
    );
    let rows = [child];
    assert!(rows[0].ended_at.is_none());
    let signals = derive(&stopped, &rows, &[]);
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].name.as_str(), "team.ended");
    let mut ignored = event(&rows[0], AgentStatus::Running, turn_end());
    ignored.transition = LifecycleTransition::Ignored {
        reason: "duplicate".to_owned(),
    };
    assert!(derive(&ignored, &rows, &[]).is_empty());
}

#[test]
fn one_cohort_writes_one_instance_into_each_of_its_records() {
    let root = tempfile::tempdir().unwrap();
    let workspace = crate::WorkspaceResolver::resolve(root.path(), None).unwrap();
    let selector =
        crate::harness::signal::parse_signal_selector("idle", "team.idle", None).unwrap();
    for (stamp, expected) in [(Some("feat x"), "forge#feat-x"), (None, "forge#external")] {
        let mut coder = member("coder", "", AgentStatus::Success);
        coder.channel = stamp.map(str::to_owned);
        coder.worktree_path = None;
        let rows = [coder.clone()];
        let listed = team_cohorts(&rows).remove(0).instance;
        let run = crate::store::run::TeamRun {
            launch_id: coder.agent_id.clone(),
            instance: coder.team_instance().unwrap(),
        };
        let run: crate::store::run::TeamRun =
            serde_json::from_value(serde_json::to_value(&run).unwrap()).unwrap();
        let stage = crate::harness::team_stage::StageSignal {
            team: "forge".to_owned(),
            instance: TeamInstanceId::new("forge", coder.lane_name()).unwrap(),
            to: "Build".to_owned(),
            by: "user".to_owned(),
            board: root.path().join("board.md"),
            at: jiff::Timestamp::UNIX_EPOCH,
            from: None,
            owner: None,
            note: None,
        };
        let stage: crate::harness::team_stage::StageSignal =
            serde_json::from_value(serde_json::to_value(&stage).unwrap()).unwrap();
        let event = event(&coder, AgentStatus::Running, turn_end());
        let lifecycle = derive(&event, &rows, &[]);
        let lifecycle: TeamInstanceId = lifecycle[0].payload["instance"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let mut matches = BTreeMap::new();
        crate::harness::schedule::arm::default_signal_matches(
            &workspace,
            &rows,
            &coder,
            &selector,
            &mut matches,
        )
        .unwrap();
        let subscribed: TeamInstanceId = matches["instance"].parse().unwrap();

        assert_eq!(listed.to_string(), expected);
        for (record, instance) in [
            ("team run", &run.instance),
            ("stage signal", &stage.instance),
            ("lifecycle signal", &lifecycle),
            ("loop subscription", &subscribed),
        ] {
            assert_eq!(instance, &listed, "{record} for {expected}");
        }
    }
}
