use super::super::*;

use crate::agents::{AgentStatus, SessionOrigin};
use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use crate::pane::PaneRef;
use crate::store::snapshot::testkit::agent;

use HookPaneRecoveryMethod::{ClientFocus, OccupiedSoleCandidate, SingleCandidate, TabFocus};
use HookPaneRecoveryRejectReason::*;

struct Case {
    name: &'static str,
    panes: Vec<PaneRef>,
    client_focus: Option<Vec<PaneId>>,
    prior_stamps: Vec<(&'static str, jiff::Timestamp)>,
    expected_pane: Option<&'static str>,
    candidate_count: usize,
    method: HookPaneRecoveryMethod,
    reject_reasons: Vec<(usize, HookPaneRecoveryRejectReason)>,
}

fn id(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

fn pane(raw: &str, command: &str, cwd: &str, focused: bool) -> PaneRef {
    PaneRef {
        pane_id: id(raw),
        session_name: "rimz-test".to_owned(),
        view_id: None,
        view_kind: None,
        view_name: None,
        title: None,
        is_focused: focused,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn candidate(raw: &str, focused: bool) -> PaneRef {
    pane(raw, "codex", "/repo/main", focused)
}

fn hosted_candidate(raw: &str, focused: bool) -> PaneRef {
    PaneRef {
        hosted_agent_kind: Some(AgentKind::new_unchecked("codex")),
        ..pane(raw, "chezmoi cd", "/repo/main", focused)
    }
}

fn started(raw: &str, start: jiff::Timestamp) -> PaneRef {
    PaneRef {
        pane_process_start: Some(start),
        ..candidate(raw, true)
    }
}

fn prior(
    kind: &str,
    agent_id: &str,
    pane_id: Option<PaneId>,
    status: AgentStatus,
    origin: Option<SessionOrigin>,
    last_activity: jiff::Timestamp,
) -> AgentState {
    let mut state = agent(kind, agent_id, status, 0);
    state.pane = pane_id.map(PaneRef::from_id);
    state.origin = origin;
    state.last_activity = last_activity;
    state
}

fn select(
    prior_agents: &[AgentState],
    panes: &[PaneRef],
    client_focus: Option<&[PaneId]>,
    origin: Option<SessionOrigin>,
    phase: HookPaneRecoveryPhase,
) -> HookPaneRecoverySelection {
    select_kind("codex", prior_agents, panes, client_focus, origin, phase)
}

fn select_kind(
    kind: &str,
    prior_agents: &[AgentState],
    panes: &[PaneRef],
    client_focus: Option<&[PaneId]>,
    origin: Option<SessionOrigin>,
    phase: HookPaneRecoveryPhase,
) -> HookPaneRecoverySelection {
    let kind = AgentKind::new_unchecked(kind);
    let agent_id = AgentSessionId::from("new");
    HookPaneRecoveryContext::new(&kind, &agent_id, origin, phase, prior_agents).select(
        "/repo/main",
        panes,
        client_focus,
    )
}

fn cwd_reject(path: &str) -> HookPaneRecoveryRejectReason {
    CwdMismatch {
        got: Some(path.to_owned()),
    }
}

fn command_reject(command: &str) -> HookPaneRecoveryRejectReason {
    CommandMismatch {
        got: Some(command.to_owned()),
    }
}

#[test]
fn focused_pane_recovery_selects_or_rejects_by_focus_and_stamp_state() {
    for case in focus_recovery_cases() {
        let prior_agents = case
            .prior_stamps
            .iter()
            .map(|(raw, last_activity)| {
                prior(
                    "codex",
                    "old",
                    Some(id(raw)),
                    AgentStatus::Idle,
                    Some(SessionOrigin::Fresh),
                    *last_activity,
                )
            })
            .collect::<Vec<_>>();
        let selected = select(
            &prior_agents,
            &case.panes,
            case.client_focus.as_deref(),
            Some(SessionOrigin::Fresh),
            HookPaneRecoveryPhase::TurnStarted,
        );

        assert_eq!(
            selected.pane_id.as_ref().map(PaneId::raw),
            case.expected_pane,
            "{} selected pane",
            case.name,
        );
        assert_eq!(
            selected.candidate_count, case.candidate_count,
            "{} candidate count",
            case.name,
        );
        assert_eq!(selected.method, case.method, "{} method", case.name);
        for (index, reason) in case.reject_reasons {
            assert!(
                selected.candidates[index].reject_reasons.contains(&reason),
                "{} candidate {index} missing {reason:?}: {:?}",
                case.name,
                selected.candidates[index].reject_reasons,
            );
        }
    }
}

fn focus_recovery_cases() -> Vec<Case> {
    let epoch = jiff::Timestamp::UNIX_EPOCH;
    let later = jiff::Timestamp::from_second(60).unwrap();
    vec![
        Case {
            name: "unique client focus",
            panes: vec![
                candidate("terminal_4", true),
                candidate("terminal_30", true),
            ],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![],
            expected_pane: Some("terminal_30"),
            candidate_count: 2,
            method: ClientFocus,
            reject_reasons: vec![(0, NotInClientFocus)],
        },
        Case {
            name: "single candidate without focus",
            panes: vec![
                candidate("terminal_4", false),
                pane("terminal_30", "codex", "/repo/other", true),
            ],
            client_focus: None,
            prior_stamps: vec![],
            expected_pane: Some("terminal_4"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![(1, cwd_reject("/repo/other"))],
        },
        Case {
            name: "hosted agent under wrapper command",
            panes: vec![hosted_candidate("terminal_176", true)],
            client_focus: Some(vec![id("terminal_176")]),
            prior_stamps: vec![],
            expected_pane: Some("terminal_176"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![],
        },
        Case {
            name: "ambiguous client focus",
            panes: vec![
                candidate("terminal_4", true),
                candidate("terminal_30", true),
            ],
            client_focus: Some(vec![id("terminal_4"), id("terminal_30")]),
            prior_stamps: vec![],
            expected_pane: None,
            candidate_count: 2,
            method: ClientFocus,
            reject_reasons: vec![(0, Ambiguous { n: 2 }), (1, Ambiguous { n: 2 })],
        },
        Case {
            name: "tab focus fallback",
            panes: vec![
                candidate("terminal_4", false),
                candidate("terminal_30", true),
            ],
            client_focus: None,
            prior_stamps: vec![],
            expected_pane: Some("terminal_30"),
            candidate_count: 2,
            method: TabFocus,
            reject_reasons: vec![(0, NotTabFocused)],
        },
        Case {
            name: "codex can share occupied pane when no free candidate",
            panes: vec![candidate("terminal_30", true)],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![("terminal_30", epoch)],
            expected_pane: Some("terminal_30"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![],
        },
        Case {
            name: "stale foreign stamp",
            panes: vec![started("terminal_30", later)],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![("terminal_30", epoch)],
            expected_pane: Some("terminal_30"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![],
        },
        Case {
            name: "codex can share current occupied pane",
            panes: vec![started("terminal_30", epoch)],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![("terminal_30", later)],
            expected_pane: Some("terminal_30"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![],
        },
        Case {
            name: "occupied fallback records surrounding reasons",
            panes: vec![
                pane("terminal_4", "claude", "/repo/main", false),
                pane("terminal_30", "codex", "/repo/other", false),
                candidate("terminal_42", true),
            ],
            client_focus: None,
            prior_stamps: vec![("terminal_42", epoch)],
            expected_pane: Some("terminal_42"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![
                (0, command_reject("claude")),
                (1, cwd_reject("/repo/other")),
            ],
        },
    ]
}

#[test]
fn provisional_launch_stamp_allows_recovery_for_known_session() {
    let pane_id = id("terminal_30");
    let prior_agents = vec![
        prior(
            "codex",
            "launch_abc",
            Some(pane_id.clone()),
            AgentStatus::Idle,
            None,
            jiff::Timestamp::UNIX_EPOCH,
        ),
        prior(
            "codex",
            "new",
            None,
            AgentStatus::Running,
            Some(SessionOrigin::Fresh),
            jiff::Timestamp::UNIX_EPOCH,
        ),
    ];
    let selected = select(
        &prior_agents,
        &[candidate("terminal_30", true)],
        Some(std::slice::from_ref(&pane_id)),
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::TurnStarted,
    );

    assert_eq!(selected.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(selected.method, SingleCandidate);
    assert_eq!(
        selected.candidates[0].occupied_by_agent_id.as_deref(),
        Some("launch_abc")
    );
    assert!(selected.candidates[0].reject_reasons.is_empty());
}

#[test]
fn occupied_pane_fallback_stays_daemon_hooked_and_first_event_only() {
    let pane_id = id("terminal_30");
    let old_codex = prior(
        "codex",
        "old",
        Some(pane_id.clone()),
        AgentStatus::Idle,
        Some(SessionOrigin::Fresh),
        jiff::Timestamp::UNIX_EPOCH,
    );
    let occupied = candidate("terminal_30", true);
    let focus = [pane_id.clone()];

    let selected = select(
        std::slice::from_ref(&old_codex),
        std::slice::from_ref(&occupied),
        Some(&focus),
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::TurnStarted,
    );
    assert_eq!(selected.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(
        selected.candidates[0].occupied_by_agent_id.as_deref(),
        Some("old")
    );
    assert!(selected.candidates[0].reject_reasons.is_empty());

    let selected = select(
        std::slice::from_ref(&old_codex),
        std::slice::from_ref(&occupied),
        Some(&focus),
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::Registered,
    );
    assert_eq!(selected.pane_id, None);
    assert_eq!(selected.candidate_count, 0);

    let selected = select(
        std::slice::from_ref(&old_codex),
        std::slice::from_ref(&occupied),
        Some(&focus),
        None,
        HookPaneRecoveryPhase::TurnStarted,
    );
    assert_eq!(selected.pane_id, Some(pane_id.clone()));

    let known_new = prior(
        "codex",
        "new",
        None,
        AgentStatus::Running,
        Some(SessionOrigin::Fresh),
        jiff::Timestamp::UNIX_EPOCH,
    );
    let selected = select(
        &[old_codex, known_new],
        &[occupied],
        Some(&focus),
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::TurnStarted,
    );
    assert_eq!(selected.pane_id, None);
    assert_eq!(selected.candidate_count, 0);
    assert_eq!(
        selected.candidates[0].occupied_by_agent_id.as_deref(),
        Some("old")
    );
    assert!(
        selected.candidates[0]
            .reject_reasons
            .contains(&StampedToOther {
                agent_id: "old".to_owned()
            })
    );

    let claude = prior(
        "claude",
        "old",
        Some(pane_id.clone()),
        AgentStatus::Idle,
        Some(SessionOrigin::Fresh),
        jiff::Timestamp::UNIX_EPOCH,
    );
    let kind = AgentKind::new_unchecked("claude");
    let incoming = AgentSessionId::from("new");
    let claude_pane = pane("terminal_30", "claude", "/repo/main", true);
    let selected = HookPaneRecoveryContext::new(
        &kind,
        &incoming,
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::TurnStarted,
        &[claude],
    )
    .select("/repo/main", &[claude_pane], Some(&focus));
    assert_eq!(selected.pane_id, None);
    assert_eq!(selected.candidate_count, 0);
}

#[test]
fn sole_resting_fresh_occupied_pane_binds_without_focus() {
    let pane_id = id("terminal_30");
    let owner = prior(
        "codex",
        "old",
        Some(pane_id.clone()),
        AgentStatus::Idle,
        Some(SessionOrigin::Fresh),
        jiff::Timestamp::UNIX_EPOCH,
    );
    let selected = select(
        &[owner],
        &[candidate("terminal_30", false)],
        Some(&[]),
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::TurnStarted,
    );

    assert_eq!(selected.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(selected.method, OccupiedSoleCandidate);
    assert_eq!(selected.candidate_count, 1);
    assert!(selected.candidates[0].reject_reasons.is_empty());
}

#[test]
fn antigravity_turn_start_follows_the_sole_resting_conversation_without_lineage() {
    let pane_id = id("terminal_30");
    let owner = prior(
        "antigravity",
        "old",
        Some(pane_id.clone()),
        AgentStatus::Success,
        None,
        jiff::Timestamp::UNIX_EPOCH,
    );
    let occupied = pane("terminal_30", "agy", "/repo/main", false);
    let selected = select_kind(
        "antigravity",
        &[owner],
        &[occupied],
        Some(&[]),
        None,
        HookPaneRecoveryPhase::TurnStarted,
    );

    assert_eq!(selected.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(selected.method, OccupiedSoleCandidate);

    for status in [AgentStatus::Running, AgentStatus::Waiting] {
        let owner = prior(
            "antigravity",
            "old",
            Some(pane_id.clone()),
            status,
            None,
            jiff::Timestamp::UNIX_EPOCH,
        );
        let occupied = pane("terminal_30", "agy", "/repo/main", true);
        let selected = select_kind(
            "antigravity",
            &[owner],
            &[occupied],
            Some(std::slice::from_ref(&pane_id)),
            None,
            HookPaneRecoveryPhase::TurnStarted,
        );
        assert_eq!(
            selected.pane_id, None,
            "{status:?} owner stays authoritative"
        );
    }
}

#[test]
fn opencode_registration_and_turn_start_follow_the_resting_conversation() {
    let pane_id = id("terminal_30");
    let resting = prior(
        "opencode",
        "old",
        Some(pane_id.clone()),
        AgentStatus::Success,
        None,
        jiff::Timestamp::UNIX_EPOCH,
    );
    let occupied = pane("terminal_30", "opencode", "/repo/main", true);
    let focus = [pane_id.clone()];

    let selected = select_kind(
        "opencode",
        std::slice::from_ref(&resting),
        std::slice::from_ref(&occupied),
        Some(&focus),
        None,
        HookPaneRecoveryPhase::Registered,
    );
    assert_eq!(selected.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(selected.method, SingleCandidate);
    assert!(selected.candidates[0].reject_reasons.is_empty());

    let known_paneless = prior(
        "opencode",
        "new",
        None,
        AgentStatus::Running,
        None,
        jiff::Timestamp::UNIX_EPOCH,
    );
    let selected = select_kind(
        "opencode",
        &[resting.clone(), known_paneless.clone()],
        std::slice::from_ref(&occupied),
        Some(&focus),
        None,
        HookPaneRecoveryPhase::TurnStarted,
    );
    assert_eq!(selected.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(selected.method, SingleCandidate);
    assert!(selected.candidates[0].reject_reasons.is_empty());

    let running = prior(
        "opencode",
        "old",
        Some(pane_id),
        AgentStatus::Running,
        None,
        jiff::Timestamp::UNIX_EPOCH,
    );
    for (phase, prior_agents) in [
        (HookPaneRecoveryPhase::Registered, vec![running.clone()]),
        (
            HookPaneRecoveryPhase::TurnStarted,
            vec![running.clone(), known_paneless.clone()],
        ),
    ] {
        let selected = select_kind(
            "opencode",
            &prior_agents,
            std::slice::from_ref(&occupied),
            Some(&focus),
            None,
            phase,
        );
        assert_eq!(selected.pane_id, None, "{phase:?} keeps the running owner");
    }
}

#[test]
fn occupied_pane_without_focus_requires_clear_lineage_and_resting_owner() {
    let pane_id = id("terminal_30");
    for (label, status, owner_origin, incoming_origin) in [
        (
            "running owner",
            AgentStatus::Running,
            Some(SessionOrigin::Fresh),
            Some(SessionOrigin::Fresh),
        ),
        (
            "waiting owner",
            AgentStatus::Waiting,
            Some(SessionOrigin::Fresh),
            Some(SessionOrigin::Fresh),
        ),
        (
            "forked owner",
            AgentStatus::Idle,
            Some(SessionOrigin::Forked),
            Some(SessionOrigin::Fresh),
        ),
        (
            "unknown owner",
            AgentStatus::Idle,
            None,
            Some(SessionOrigin::Fresh),
        ),
        (
            "forked incoming",
            AgentStatus::Idle,
            Some(SessionOrigin::Fresh),
            Some(SessionOrigin::Forked),
        ),
        (
            "unknown incoming",
            AgentStatus::Idle,
            Some(SessionOrigin::Fresh),
            None,
        ),
    ] {
        let owner = prior(
            "codex",
            "old",
            Some(pane_id.clone()),
            status,
            owner_origin,
            jiff::Timestamp::UNIX_EPOCH,
        );
        let selected = select(
            &[owner],
            &[candidate("terminal_30", false)],
            Some(&[]),
            incoming_origin,
            HookPaneRecoveryPhase::TurnStarted,
        );

        assert_eq!(selected.pane_id, None, "{label}");
        assert_eq!(selected.candidate_count, 0, "{label}");
    }
}

#[test]
fn several_resting_fresh_occupied_panes_stay_unbound_without_focus() {
    let prior_agents = [
        prior(
            "codex",
            "old-1",
            Some(id("terminal_30")),
            AgentStatus::Idle,
            Some(SessionOrigin::Fresh),
            jiff::Timestamp::UNIX_EPOCH,
        ),
        prior(
            "codex",
            "old-2",
            Some(id("terminal_42")),
            AgentStatus::Success,
            Some(SessionOrigin::Fresh),
            jiff::Timestamp::UNIX_EPOCH,
        ),
    ];
    let selected = select(
        &prior_agents,
        &[
            candidate("terminal_30", false),
            candidate("terminal_42", false),
        ],
        Some(&[]),
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::TurnStarted,
    );

    assert_eq!(selected.pane_id, None);
    assert_eq!(selected.candidate_count, 2);
}

#[test]
fn pane_started_after_session_is_rejected_unless_it_resumes_that_session() {
    let registered_at = jiff::Timestamp::from_second(30).unwrap();
    let pane_start = jiff::Timestamp::from_second(60).unwrap();
    let mut incoming = prior(
        "codex",
        "new",
        None,
        AgentStatus::Running,
        Some(SessionOrigin::Fresh),
        registered_at,
    );
    incoming.registered_at = Some(registered_at);
    let selected = select(
        std::slice::from_ref(&incoming),
        &[started("terminal_30", pane_start)],
        None,
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::TurnStarted,
    );
    assert_eq!(selected.pane_id, None);
    assert_eq!(selected.candidate_count, 0);
    assert!(
        selected.candidates[0]
            .reject_reasons
            .contains(&StartedAfterSession)
    );

    let resumed = PaneRef {
        resumed_session_id: Some(AgentSessionId::from("new")),
        ..started("terminal_30", pane_start)
    };
    let selected = select(
        &[incoming],
        &[resumed],
        None,
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::TurnStarted,
    );
    assert_eq!(
        selected.pane_id.as_ref().map(PaneId::raw),
        Some("terminal_30")
    );
    assert_eq!(selected.method, SingleCandidate);
}

#[test]
fn recovery_diagnostic_wire_names_stay_compatible() {
    for (method, expected) in [
        (HookPaneRecoveryMethod::None, "none"),
        (HookPaneRecoveryMethod::SingleCandidate, "single_candidate"),
        (
            HookPaneRecoveryMethod::OccupiedSoleCandidate,
            "occupied_sole_candidate",
        ),
        (HookPaneRecoveryMethod::ClientFocus, "client_focus"),
        (HookPaneRecoveryMethod::TabFocus, "tab_focus"),
    ] {
        assert_eq!(serde_json::to_value(method).unwrap(), expected);
    }
    assert_eq!(
        serde_json::to_value(StartedAfterSession).unwrap(),
        serde_json::json!({ "reason": "started_after_session" })
    );
    assert_eq!(
        serde_json::to_value(StampedToOther {
            agent_id: "old".to_owned()
        })
        .unwrap(),
        serde_json::json!({ "reason": "stamped_to_other", "agent_id": "old" })
    );

    let occupied = prior(
        "codex",
        "old",
        Some(id("terminal_30")),
        AgentStatus::Idle,
        Some(SessionOrigin::Fresh),
        jiff::Timestamp::UNIX_EPOCH,
    );
    let selected = select(
        &[occupied],
        &[candidate("terminal_30", true)],
        Some(&[]),
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::Registered,
    );
    let candidate = serde_json::to_value(&selected.candidates[0]).unwrap();
    assert_eq!(candidate["occupied_by_agent_id"], "old");
    assert_eq!(candidate["reject_reasons"][0]["reason"], "stamped_to_other");
}

#[test]
fn hook_recovery_keeps_raw_cwd_stricter_than_projection_worktree() {
    let wrapped = PaneRef {
        command: Some("/bin/rimz agents exec codex --worktree-path /repo/main".to_owned()),
        spawn_command: Some("/bin/rimz agents exec codex --worktree-path /repo/main".to_owned()),
        cwd: None,
        ..candidate("terminal_30", true)
    };
    let evidence = pane_binding_evidence(&wrapped);
    assert_eq!(evidence.raw_cwd, None);
    assert_eq!(evidence.projection_worktree, Some("/repo/main"));

    let selected = select(
        &[],
        &[wrapped],
        None,
        Some(SessionOrigin::Fresh),
        HookPaneRecoveryPhase::TurnStarted,
    );
    assert_eq!(selected.pane_id, None);
    assert!(
        selected.candidates[0]
            .reject_reasons
            .contains(&CwdMismatch { got: None })
    );
}

#[test]
fn foreign_owner_diagnostic_is_independent_of_rollup_order() {
    let pane_id = id("terminal_30");
    let mut primary = prior(
        "codex",
        "primary",
        Some(pane_id.clone()),
        AgentStatus::Idle,
        Some(SessionOrigin::Fresh),
        jiff::Timestamp::UNIX_EPOCH,
    );
    primary.registered_at = Some(jiff::Timestamp::from_second(10).unwrap());
    let mut secondary = prior(
        "codex",
        "secondary",
        Some(pane_id.clone()),
        AgentStatus::Idle,
        Some(SessionOrigin::Fresh),
        jiff::Timestamp::UNIX_EPOCH,
    );
    secondary.registered_at = Some(jiff::Timestamp::from_second(20).unwrap());

    for prior_agents in [
        vec![primary.clone(), secondary.clone()],
        vec![secondary.clone(), primary.clone()],
    ] {
        let selected = select(
            &prior_agents,
            &[candidate("terminal_30", true)],
            Some(std::slice::from_ref(&pane_id)),
            Some(SessionOrigin::Fresh),
            HookPaneRecoveryPhase::Registered,
        );
        assert_eq!(
            selected.candidates[0].occupied_by_agent_id.as_deref(),
            Some("primary")
        );
    }
}
