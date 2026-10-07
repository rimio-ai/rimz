use super::*;

use crate::agents::AgentStatus;
use crate::ids::{AgentKind, AgentSessionId, PaneId, WorkspaceId};
use crate::pane::PaneRef;

#[test]
fn dispatch_park_causes_follow_schedule_conditions_readiness_and_fifo() {
    let mut receiver = resident("session", "terminal_1");
    receiver.status = AgentStatus::Idle;
    let pane = owner_pane("session", None);
    let snapshot = snapshot_with_panes(vec![receiver.clone()], vec![pane.clone()]);
    let target = ResolvedTarget {
        pane: Some(pane),
        agent: Some(receiver.clone()),
    };
    let absent = ResolvedTarget {
        pane: None,
        agent: Some(receiver.clone()),
    };
    let mut mode = PreparedMode {
        kind: DeliveryKind::Boundary,
        draft: MessageDraft {
            body: MessageBody::Prompt,
            enter: true,
            gate: DeliveryGate::Done,
            sender: MessageSender::Human,
            automated: false,
            force: false,
            auto_compact: None,
            not_before: None,
            after: Vec::new(),
            when: Vec::new(),
        },
    };
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    let parked = |reason| DispatchDecision::Parked {
        reason: Some(reason),
    };
    for kind in [
        DeliveryKind::Boundary,
        DeliveryKind::Steer,
        DeliveryKind::Interrupt,
    ] {
        mode.kind = kind;
        actual.push(dispatch_decision(&snapshot, &[], &absent, &mode, now()));
        expected.push(parked(ParkReason::NoPane));
    }
    mode.kind = DeliveryKind::Boundary;
    mode.draft.not_before = Some(now());
    mode.draft.after = vec![
        AfterCondition {
            kind: receiver.kind.clone(),
            agent_id: "planner".into(),
            agent_name: None,
            address: "@planner".to_owned(),
            met_at: None,
        },
        AfterCondition {
            kind: receiver.kind.clone(),
            agent_id: "reviewer".into(),
            agent_name: None,
            address: "@reviewer".to_owned(),
            met_at: None,
        },
    ];
    mode.draft.when = vec![WhenCondition {
        kind: receiver.kind.clone(),
        agent_id: "coder".into(),
        agent_name: None,
        address: "@coder".to_owned(),
        status: AgentStatus::Idle,
        dwell_secs: 3480,
        met_at: None,
    }];
    actual.push(dispatch_decision(&snapshot, &[], &target, &mode, now()));
    expected.push(parked(ParkReason::Scheduled(now())));
    mode.draft.not_before = None;
    actual.push(dispatch_decision(&snapshot, &[], &target, &mode, now()));
    expected.push(parked(ParkReason::After("@planner".to_owned())));
    for condition in &mut mode.draft.after {
        condition.met_at = Some(now());
    }
    actual.push(dispatch_decision(&snapshot, &[], &target, &mode, now()));
    expected.push(parked(ParkReason::When {
        address: "@coder".to_owned(),
        status: AgentStatus::Idle,
        dwell_secs: 3480,
    }));
    mode.draft.when[0].met_at = Some(now());
    let mut blocker = MessageRecord::new(
        WorkspaceId::from_project_root(std::path::Path::new("/project")),
        &receiver,
        "older".to_owned(),
        DeliveryGate::Done,
    );
    blocker.message_id = "msg_0000000000000001".parse().unwrap();
    for status in [
        crate::store::message::MessageStatus::Queued,
        crate::store::message::MessageStatus::Claimed,
        crate::store::message::MessageStatus::Sent,
    ] {
        blocker.status = status;
        blocker.last_attempt_at = Some(now());
        actual.push(dispatch_decision(
            &snapshot,
            &[blocker.clone()],
            &target,
            &mode,
            now(),
        ));
        expected.push(parked(ParkReason::Behind(blocker.message_id.clone())));
    }
    blocker.body = MessageBody::Command;
    actual.push(dispatch_decision(
        &snapshot,
        &[blocker.clone()],
        &target,
        &mode,
        now(),
    ));
    expected.push(DispatchDecision::Live);
    blocker.body = MessageBody::Prompt;
    for kind in [DeliveryKind::Steer, DeliveryKind::Interrupt] {
        mode.kind = kind;
        actual.push(dispatch_decision(
            &snapshot,
            &[blocker.clone()],
            &target,
            &mode,
            now(),
        ));
        expected.push(DispatchDecision::Live);
    }
    mode.kind = DeliveryKind::Boundary;
    let mut busy = snapshot;
    busy.agents[0].status = AgentStatus::Running;
    actual.push(dispatch_decision(&busy, &[blocker], &target, &mode, now()));
    expected.push(parked(ParkReason::Status(AgentStatus::Running)));
    assert_eq!(actual, expected);
}

#[test]
fn resumed_provider_parks_until_registration_or_the_window() {
    let mut receiver = resident("session", "terminal_1");
    receiver.status = AgentStatus::Idle;
    receiver.resumed_at = Some(now());
    let pane = owner_pane("session", None);
    let mut mode = PreparedMode {
        kind: DeliveryKind::Boundary,
        draft: MessageDraft {
            body: MessageBody::Prompt,
            enter: true,
            gate: DeliveryGate::Done,
            sender: MessageSender::Human,
            automated: false,
            force: false,
            auto_compact: None,
            not_before: None,
            after: Vec::new(),
            when: Vec::new(),
        },
    };
    let snapshot = snapshot_with_panes(vec![receiver.clone()], vec![pane.clone()]);
    let target = ResolvedTarget {
        pane: Some(pane),
        agent: Some(receiver),
    };
    assert_eq!(
        dispatch_decision(&snapshot, &[], &target, &mode, now()),
        DispatchDecision::Parked {
            reason: Some(ParkReason::ProviderStarting)
        }
    );
    assert_eq!(
        dispatch_decision(
            &snapshot,
            &[],
            &target,
            &mode,
            now() + MessageBody::Prompt.delivery_window()
        ),
        DispatchDecision::Live
    );
    for kind in [DeliveryKind::Steer, DeliveryKind::Interrupt] {
        mode.kind = kind;
        assert_eq!(
            dispatch_decision(&snapshot, &[], &target, &mode, now()),
            DispatchDecision::Live
        );
    }
    mode.kind = DeliveryKind::Boundary;
    let mut registered = snapshot.clone();
    registered.agents[0].resumed_at = None;
    assert_eq!(
        dispatch_decision(&registered, &[], &target, &mode, now()),
        DispatchDecision::Live
    );
    let mut lazy = snapshot;
    lazy.agents[0].kind = AgentKind::new_unchecked("codex");
    lazy.agent_panes[0].kind = lazy.agents[0].kind.clone();
    let target = ResolvedTarget {
        pane: Some(lazy.agent_panes[0].clone()),
        agent: Some(lazy.agents[0].clone()),
    };
    assert_eq!(
        dispatch_decision(&lazy, &[], &target, &mode, now()),
        DispatchDecision::Live
    );
}

#[test]
fn queue_preflight_checks_the_target_account_home() {
    let temp = tempfile::tempdir().unwrap();
    let ambient = std::collections::BTreeMap::from([(
        "HOME".to_owned(),
        temp.path().join("u").to_string_lossy().into_owned(),
    )]);
    let mut target = agent("session", AgentStatus::Idle);
    target.login = Some("work".parse().unwrap());
    let login = crate::agents::ProviderLogin::named(
        target.kind.clone(),
        target.login.clone().unwrap(),
        temp.path().join("work"),
    )
    .unwrap();
    let login_env = login.env(&ambient);
    assert!(matches!(
        preflight_queue_hooks(&target, &login_env),
        Err(DispatchErr::HooksMissing { .. })
    ));
    crate::agents::find_definition(target.kind.as_str())
        .unwrap()
        .install_hooks(&login_env)
        .unwrap();
    assert!(preflight_queue_hooks(&target, &login_env).is_ok());
    assert!(matches!(
        preflight_queue_hooks(&target, &ambient),
        Err(DispatchErr::HooksMissing { .. })
    ));
}

#[test]
fn condition_broadcast_is_typed_before_resolution() {
    let snapshot = snapshot_with_panes(Vec::new(), Vec::new());
    let err = resolution(&snapshot, &[], &context(None), false)
        .condition_target(ConditionKind::When, "@all", "@all idle 1m")
        .expect_err("broadcast condition must fail");
    assert!(matches!(
        err,
        DispatchErr::Condition(ConditionErr::Broadcast {
            kind: ConditionKind::When,
            ..
        })
    ));
}

#[test]
fn provisional_pane_skips_readiness_gate() {
    let launch = agent("launch_pending", AgentStatus::Running);
    let pane = pane_only("terminal_1", "coder");
    let snapshot = snapshot_with_panes(vec![launch.clone()], vec![pane.clone()]);
    let binding = crate::address::pane_binding(&snapshot, &pane, None).unwrap();
    assert!(binding.agent.is_some() && binding.exact_agent.is_none());
    let target = ResolvedTarget {
        pane: Some(pane),
        agent: Some(launch),
    };
    assert!(target.bound(&snapshot).is_none());
    let mode = PreparedMode {
        kind: DeliveryKind::Boundary,
        draft: MessageDraft {
            body: MessageBody::Prompt,
            enter: true,
            gate: DeliveryGate::Done,
            sender: MessageSender::Human,
            automated: false,
            force: false,
            auto_compact: None,
            not_before: None,
            after: Vec::new(),
            when: Vec::new(),
        },
    };

    assert_eq!(
        dispatch_decision(&snapshot, &[], &target, &mode, now()),
        DispatchDecision::Live
    );
}

#[test]
fn agent_sender_inherits_exact_named_sessions_turn_openers() {
    let opener = MessageId::parse("msg_0123456789abcdef").unwrap();
    let mut agent = agent("sess-1", AgentStatus::Running);
    agent.name = Some("coder".to_owned());
    let mut context = crate::agents::AgentContext::new("codex", now());
    context.turn_opened_by = vec![opener.clone()];
    agent.context = Some(context);
    let snapshot = SidebarSnapshot::build_with_agents(workspace_id(), vec![agent], now());
    let sender = MessageSender::Agent {
        agent_id: None,
        kind: AgentKind::new_unchecked("claude"),
        name: Some("coder".to_owned()),
        profile: None,
        role: None,
        channel: Some("chat".to_owned()),
    };
    assert_eq!(turn_openers_for_sender(&snapshot, &sender), vec![opener]);
    assert!(turn_openers_for_sender(&snapshot, &MessageSender::Human).is_empty());
    assert!(
        turn_openers_for_sender(
            &snapshot,
            &MessageSender::Subagent {
                kind: AgentKind::new_unchecked("codex"),
                name: "child".to_owned(),
            },
        )
        .is_empty()
    );
}

#[test]
fn co_resident_session_resolves_to_one_pane_backed_recipient() {
    let older = resident("sess-older", "terminal_1");
    let owner = resident("sess-owner", "terminal_1");
    let durable = vec![older.clone(), owner.clone()];
    let pane = owner_pane("sess-owner", Some("coder"));
    let snapshot = snapshot_with_panes(vec![older, owner], vec![pane]);

    let targets = resolution(&snapshot, &durable, &context(Some("project")), false)
        .resolve("@coder")
        .unwrap();

    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0]
            .agent
            .as_ref()
            .map(|agent| agent.agent_id.as_str()),
        Some("sess-owner")
    );
    assert!(targets[0].pane.is_some());
}

#[test]
fn durable_fallback_drops_shadowed_co_resident_session() {
    let durable = vec![
        resident("sess-older", "terminal_1"),
        resident("sess-owner", "terminal_1"),
    ];
    // The live pane deliberately lacks the role so resolution falls back
    // to the audit-scope durable candidates.
    let snapshot = snapshot_with_panes(Vec::new(), vec![owner_pane("sess-owner", None)]);

    for rollup_only in [false, true] {
        let targets = resolution(&snapshot, &durable, &context(Some("project")), rollup_only)
            .resolve("@coder")
            .unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(
            targets[0]
                .agent
                .as_ref()
                .map(|agent| agent.agent_id.as_str()),
            Some("sess-owner")
        );
    }

    assert!(matches!(
        resolution(&snapshot, &durable, &context(None), false).resolve("@sess-older"),
        Err(TargetErr::NoMatch { .. })
    ));
}

#[test]
fn agent_broadcast_excludes_only_the_caller_after_channel_resolution() {
    let mut caller = named_agent("caller", "planner", "project");
    caller.launch_id = Some(AgentSessionId::from("launch-planner"));
    let first_peer = named_agent("first-peer", "coder", "project");
    let second_peer = named_agent("second-peer", "reviewer", "project");
    let other_channel = named_agent("other-channel", "docs", "docs");
    let durable = vec![
        caller.clone(),
        first_peer.clone(),
        second_peer.clone(),
        other_channel,
    ];
    let snapshot = snapshot_with_panes(durable.clone(), Vec::new());
    let mut targets = resolution(&snapshot, &durable, &context(None), false)
        .resolve("@all#project")
        .unwrap();

    exclude_broadcast_caller(
        "@all#project",
        &mut targets,
        &durable,
        Some(&launch_caller("launch-planner")),
        None,
    )
    .unwrap();

    let ids = targets
        .iter()
        .filter_map(|target| target.agent.as_ref())
        .map(|agent| agent.agent_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["first-peer", "second-peer"]);
}

#[test]
fn exact_self_handle_is_not_broadcast_filtered() {
    let mut caller = named_agent("caller", "planner", "project");
    caller.launch_id = Some(AgentSessionId::from("launch-planner"));
    let durable = vec![caller];
    let snapshot = snapshot_with_panes(durable.clone(), Vec::new());
    let mut targets = resolution(&snapshot, &durable, &context(Some("project")), false)
        .resolve("@planner")
        .unwrap();

    exclude_broadcast_caller(
        "@planner",
        &mut targets,
        &durable,
        Some(&launch_caller("launch-planner")),
        None,
    )
    .unwrap();

    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0]
            .agent
            .as_ref()
            .map(|agent| agent.agent_id.as_str()),
        Some("caller")
    );
}

#[test]
fn explicit_selector_fanout_keeps_the_caller() {
    let mut caller = named_agent("caller", "planner", "project");
    caller.launch_id = Some(AgentSessionId::from("launch-planner"));
    let peer = named_agent("peer", "coder", "project");
    let durable = vec![caller, peer];
    let snapshot = snapshot_with_panes(durable.clone(), Vec::new());
    let mut targets = resolution(&snapshot, &durable, &context(Some("project")), false)
        .resolve("@claude")
        .unwrap();

    exclude_broadcast_caller(
        "@claude",
        &mut targets,
        &durable,
        Some(&launch_caller("launch-planner")),
        None,
    )
    .unwrap();

    assert_eq!(targets.len(), 2);
}

#[test]
fn broadcast_excludes_a_legacy_pane_only_caller() {
    let caller_pane = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut caller = named_agent("caller", "planner", "project");
    caller.pane = Some(PaneRef::from_id(caller_pane.clone()));
    let durable = vec![caller];
    let snapshot = snapshot_with_panes(
        Vec::new(),
        vec![
            pane_only("terminal_1", "planner"),
            pane_only("terminal_2", "coder"),
        ],
    );
    let legacy = crate::harness::ancestry::CallerIdentity {
        kind: AgentKind::new_unchecked("claude"),
        launch_id: None,
        pane_id: Some(caller_pane),
        name: None,
        profile: None,
        role: None,
    };
    let mut targets = resolution(&snapshot, &durable, &context(Some("project")), false)
        .resolve("@all")
        .unwrap();

    exclude_broadcast_caller("@all", &mut targets, &durable, Some(&legacy), None).unwrap();

    assert_eq!(targets.len(), 1);
    assert_eq!(
        targets[0].pane.as_ref().map(|pane| pane.pane_id.as_str()),
        Some("zellij:terminal_2")
    );
}

#[test]
fn solo_agent_broadcast_reports_no_peers() {
    let mut caller = named_agent("caller", "planner", "project");
    caller.launch_id = Some(AgentSessionId::from("launch-planner"));
    let durable = vec![caller];
    let snapshot = snapshot_with_panes(durable.clone(), Vec::new());
    let mut targets = resolution(&snapshot, &durable, &context(Some("project")), false)
        .resolve("@all")
        .unwrap();
    let channel = "project".to_owned();

    let err = exclude_broadcast_caller(
        "@all",
        &mut targets,
        &durable,
        Some(&launch_caller("launch-planner")),
        Some(channel.as_str()),
    )
    .expect_err("the caller is not its own peer");

    assert!(matches!(
        &err,
        DispatchErr::NoPeers {
            channel: Some(channel)
        } if channel == "project"
    ));
    assert!(
        err.to_string()
            .starts_with("no other agents in the current channel")
    );
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::parse("ws_000000000000000000000000").unwrap()
}

fn resolution<'a>(
    snapshot: &'a SidebarSnapshot,
    durable_agents: &'a [AgentState],
    channel: &'a AddressContext,
    rollup_only: bool,
) -> ResolutionView<'a> {
    ResolutionView {
        snapshot,
        durable_agents,
        scope: None,
        channel,
        rollup_only,
    }
}

fn context(channel: Option<&str>) -> AddressContext {
    AddressContext {
        channel: channel.map(ToOwned::to_owned),
        origin: crate::address::ChannelOrigin::Stamped,
        project_root: "/repo".into(),
    }
}

fn snapshot_with_panes(agents: Vec<AgentState>, panes: Vec<PaneAgent>) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace_id(), agents, now());
    snapshot.agent_panes = panes;
    snapshot
}

fn agent(id: &str, status: AgentStatus) -> AgentState {
    let mut agent = AgentState::stub("claude", id, status);
    agent.worktree_path = Some("/repo/project".to_owned());
    agent.worktree_branch = Some("project".to_owned());
    agent
}

fn named_agent(id: &str, name: &str, channel: &str) -> AgentState {
    let mut agent = agent(id, AgentStatus::Running);
    agent.name = Some(name.to_owned());
    agent.channel = Some(channel.to_owned());
    agent.worktree_branch = Some(channel.to_owned());
    agent
}

fn launch_caller(launch_id: &str) -> crate::harness::ancestry::CallerIdentity {
    crate::harness::ancestry::CallerIdentity {
        kind: AgentKind::new_unchecked("claude"),
        launch_id: Some(AgentSessionId::from(launch_id)),
        pane_id: None,
        name: None,
        profile: None,
        role: None,
    }
}

fn resident(id: &str, pane: &str) -> AgentState {
    let mut agent = agent(id, AgentStatus::Running);
    agent.role = Some("coder".to_owned());
    agent.pane = Some(PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, pane)));
    agent
}

fn owner_pane(id: &str, role: Option<&str>) -> PaneAgent {
    PaneAgent {
        kind: AgentKind::new_unchecked("claude"),
        kind_ordinal: Some(2),
        name: Some("owner".to_owned()),
        name_explicit: false,
        profile: None,
        role: role.map(ToOwned::to_owned),
        channel: None,
        agent_id: Some(AgentSessionId::from(id)),
        pane_id: PaneId::from_parts(MuxName::Zellij, "terminal_1"),
        pane_pid: None,
        worktree_path: Some("/repo/project".to_owned()),
        worktree_branch: Some("project".to_owned()),
    }
}

fn pane_only(pane: &str, role: &str) -> PaneAgent {
    PaneAgent {
        pane_id: PaneId::from_parts(MuxName::Zellij, pane),
        agent_id: None,
        role: Some(role.to_owned()),
        worktree_path: Some("/repo/project".to_owned()),
        worktree_branch: Some("project".to_owned()),
        ..owner_pane("", None)
    }
}

fn now() -> Timestamp {
    Timestamp::UNIX_EPOCH
}
