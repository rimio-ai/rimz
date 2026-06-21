use super::binding_select::{
    BindingRejectReason, BindingSelectionMethod, PriorAgentPane, select_focused_pane_binding,
};
use super::lifecycle::append_lifecycle_event;
use super::lifecycle::fill_root_launch_identity;
use super::proctree::matches_agent_kind;
use BindingRejectReason::*;
use BindingSelectionMethod::{ClientFocus, SingleCandidate, TabFocus};
use rimz::agents::AgentLifecycleObservation;
use rimz::agents::lifecycle::{
    LifecycleSignal, LifecycleState, Transition, TransitionKind, TurnPhase,
};
use rimz::feed::AgentStatus;
use rimz::ids::AgentSessionId;
use rimz::ids::{MuxName, PaneId};
use rimz::pane::PaneRef;

struct Case {
    name: &'static str,
    panes: Vec<PaneRef>,
    client_focus: Option<Vec<PaneId>>,
    prior_stamps: Vec<(&'static str, jiff::Timestamp)>,
    expected_pane: Option<&'static str>,
    candidate_count: usize,
    method: BindingSelectionMethod,
    reject_reasons: Vec<(usize, BindingRejectReason)>,
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
        is_focused: focused,
        is_floating: false,
        command: Some(command.to_owned()),
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn candidate(raw: &str, focused: bool) -> PaneRef {
    pane(raw, "codex", "/repo/main", focused)
}

fn started(raw: &str, start: jiff::Timestamp) -> PaneRef {
    PaneRef {
        pane_process_start: Some(start),
        elevated_agent: None,
        first_seen_at_ms: None,
        ..candidate(raw, true)
    }
}

fn transition(kind: TransitionKind, compaction_closed: bool) -> Transition {
    Transition {
        next: LifecycleState {
            status: AgentStatus::Running,
            phase: TurnPhase::Reasoning,
            compacting: false,
        },
        kind,
        compaction_closed,
        opened_turn: false,
    }
}

fn root_observation() -> AgentLifecycleObservation {
    AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::Registered,
    )
}

fn launch_identity_env(
    _observation: &AgentLifecycleObservation,
    var: &'static str,
) -> Option<String> {
    match var {
        rimz::run::ENV_AGENT_ROLE => Some("coder".to_owned()),
        rimz::run::ENV_TEAM => Some("pcr".to_owned()),
        rimz::run::ENV_AGENT_PROFILE => Some("codex-coder".to_owned()),
        rimz::run::ENV_AGENT_MODEL => Some("env-model".to_owned()),
        rimz::run::ENV_AGENT_EFFORT => Some("env-effort".to_owned()),
        _ => None,
    }
}

fn cwd_reject(path: &str) -> BindingRejectReason {
    CwdMismatch {
        got: Some(path.to_owned()),
    }
}

fn command_reject(command: &str) -> BindingRejectReason {
    CommandMismatch {
        got: Some(command.to_owned()),
    }
}

fn stamped_old() -> BindingRejectReason {
    StampedToOther {
        agent_id: "old".to_owned(),
    }
}

#[test]
fn agent_kind_matches_known_launch_shapes() {
    for (comm, source, expected) in [
        ("claude", "claude", true),
        ("codex", "codex", true),
        ("node", "codex", true),
        ("node", "claude", false),
        ("zsh", "claude", false),
        ("bash", "codex", false),
    ] {
        assert_eq!(
            matches_agent_kind(comm, source),
            expected,
            "{comm}/{source}"
        );
    }
}

#[test]
fn lifecycle_append_gate_keeps_durable_truth_for_progress_signals() {
    let proof_of_work = LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
    };
    let mutating_tool = LifecycleSignal::ToolUsed {
        mutates: true,
        edits: false,
    };

    assert!(
        append_lifecycle_event(&mutating_tool, None),
        "post-tool progress is durable even when transition inspection is unavailable"
    );
    assert!(
        !append_lifecycle_event(&proof_of_work, None),
        "pre-tool proof-of-work drops when the prior rollup cannot be inspected"
    );
    assert!(
        !append_lifecycle_event(
            &proof_of_work,
            Some(transition(TransitionKind::Normal, false))
        ),
        "pre-tool proof-of-work does not fill the durable log during normal running turns"
    );
    assert!(
        append_lifecycle_event(
            &proof_of_work,
            Some(transition(
                TransitionKind::Reconciled {
                    from: AgentStatus::Idle,
                    reason: "tool used outside a running turn",
                },
                false,
            )),
        ),
        "pre-tool proof-of-work is durable when it reconciles a stale resting row"
    );
    assert!(
        append_lifecycle_event(
            &proof_of_work,
            Some(transition(TransitionKind::Normal, true))
        ),
        "pre-tool proof-of-work is durable when it closes an open compaction bracket"
    );
}

#[test]
fn root_launch_identity_fills_from_env_then_config_without_clobbering_payload() {
    let mut observed = root_observation();
    fill_root_launch_identity(
        &mut observed,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        launch_identity_env,
    );
    assert_eq!(observed.role.as_deref(), Some("coder"));
    assert_eq!(observed.team.as_deref(), Some("pcr"));
    assert_eq!(observed.profile.as_deref(), Some("codex-coder"));
    assert_eq!(observed.model.as_deref(), Some("env-model"));
    assert_eq!(observed.effort.as_deref(), Some("env-effort"));

    let mut payload = root_observation();
    payload.role = Some("payload-role".to_owned());
    payload.team = Some("payload-team".to_owned());
    payload.profile = Some("payload-profile".to_owned());
    payload.model = Some("payload-model".to_owned());
    payload.effort = Some("payload-effort".to_owned());
    fill_root_launch_identity(
        &mut payload,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        launch_identity_env,
    );
    assert_eq!(payload.role.as_deref(), Some("payload-role"));
    assert_eq!(payload.team.as_deref(), Some("payload-team"));
    assert_eq!(payload.profile.as_deref(), Some("payload-profile"));
    assert_eq!(payload.model.as_deref(), Some("payload-model"));
    assert_eq!(payload.effort.as_deref(), Some("payload-effort"));

    let mut configured = root_observation();
    fill_root_launch_identity(
        &mut configured,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        |_observation, var| match var {
            rimz::run::ENV_AGENT_ROLE => Some("coder".to_owned()),
            rimz::run::ENV_TEAM => Some("pcr".to_owned()),
            rimz::run::ENV_AGENT_PROFILE => Some("codex-coder".to_owned()),
            _ => None,
        },
    );
    assert_eq!(configured.model.as_deref(), Some("cfg-model"));
    assert_eq!(configured.effort.as_deref(), Some("cfg-effort"));
}

#[test]
fn subagent_launch_identity_is_not_inherited_from_parent_env() {
    let mut observed = root_observation();
    observed.parent_agent_id = Some(AgentSessionId::from("parent-1"));

    fill_root_launch_identity(
        &mut observed,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        launch_identity_env,
    );

    assert_eq!(observed.role, None);
    assert_eq!(observed.team, None);
    assert_eq!(observed.profile, None);
    assert_eq!(observed.model, None);
    assert_eq!(observed.effort, None);
}

#[test]
fn focused_pane_recovery_selects_or_rejects_by_focus_and_stamp_state() {
    for case in focus_recovery_cases() {
        let prior_ids: Vec<PaneId> = case.prior_stamps.iter().map(|(raw, _)| id(raw)).collect();
        let prior: Vec<PriorAgentPane<'_>> = case
            .prior_stamps
            .iter()
            .zip(&prior_ids)
            .map(|((_, last_activity), pane_id)| PriorAgentPane {
                kind: "codex",
                agent_id: "old",
                pane_id: Some(pane_id),
                last_activity: *last_activity,
            })
            .collect();
        let selected = select_focused_pane_binding(
            "codex",
            "new",
            "/repo/main",
            &prior,
            &case.panes,
            case.client_focus.as_deref(),
        );

        assert_eq!(
            selected.pane_id.as_ref().map(|pane| pane.raw()),
            case.expected_pane,
            "{} selected pane",
            case.name,
        );
        assert_eq!(
            selected.candidate_count, case.candidate_count,
            "{} candidate count",
            case.name,
        );
        assert_eq!(
            selected.method, case.method,
            "{} selection method",
            case.name
        );
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
            name: "unprovable foreign stamp",
            panes: vec![candidate("terminal_30", true)],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![("terminal_30", epoch)],
            expected_pane: None,
            candidate_count: 0,
            method: BindingSelectionMethod::None,
            reject_reasons: vec![(0, stamped_old())],
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
            name: "current foreign stamp",
            panes: vec![started("terminal_30", epoch)],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![("terminal_30", later)],
            expected_pane: None,
            candidate_count: 0,
            method: BindingSelectionMethod::None,
            reject_reasons: vec![(0, stamped_old())],
        },
        Case {
            name: "all rejected candidates record their reasons",
            panes: vec![
                pane("terminal_4", "claude", "/repo/main", false),
                pane("terminal_30", "codex", "/repo/other", false),
                candidate("terminal_42", false),
            ],
            client_focus: None,
            prior_stamps: vec![("terminal_42", epoch)],
            expected_pane: None,
            candidate_count: 0,
            method: BindingSelectionMethod::None,
            reject_reasons: vec![
                (0, command_reject("claude")),
                (1, cwd_reject("/repo/other")),
                (2, stamped_old()),
            ],
        },
    ]
}
