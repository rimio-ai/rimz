use super::binding_select::{
    BindingRejectReason, BindingSelectionMethod, PriorAgentPane, select_focused_pane_binding,
    session_already_stamped,
};
use super::proctree::matches_agent_kind;
use BindingRejectReason::*;
use BindingSelectionMethod::{ClientFocus, SingleCandidate, TabFocus};
use rimz::feed::PaneRef;
use rimz::ids::{MuxName, PaneId};

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
        command: Some(command.to_owned()),
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
    }
}

fn candidate(raw: &str, focused: bool) -> PaneRef {
    pane(raw, "codex", "/repo/main", focused)
}

fn started(raw: &str, start: jiff::Timestamp) -> PaneRef {
    PaneRef {
        pane_process_start: Some(start),
        elevated_agent: None,
        ..candidate(raw, true)
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
fn focused_pane_recovery_selects_or_rejects_by_focus_and_stamp_state() {
    let epoch = jiff::Timestamp::UNIX_EPOCH;
    let later = jiff::Timestamp::from_second(60).unwrap();
    let cases = vec![
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
    ];

    for case in cases {
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

#[test]
fn focused_pane_recovery_records_reject_reasons() {
    let occupied_id = id("terminal_42");
    let prior = vec![PriorAgentPane {
        kind: "codex",
        agent_id: "old",
        pane_id: Some(&occupied_id),
        last_activity: jiff::Timestamp::UNIX_EPOCH,
    }];
    let panes = vec![
        pane("terminal_4", "claude", "/repo/main", false),
        pane("terminal_30", "codex", "/repo/other", false),
        candidate("terminal_42", false),
    ];
    let selected = select_focused_pane_binding("codex", "new", "/repo/main", &prior, &panes, None);

    assert_eq!(selected.pane_id, None);
    assert_eq!(selected.method, BindingSelectionMethod::None);
    assert_eq!(selected.candidate_count, 0);
    for (index, reason) in [
        (0, command_reject("claude")),
        (1, cwd_reject("/repo/other")),
        (2, stamped_old()),
    ] {
        assert!(
            selected.candidates[index].reject_reasons.contains(&reason),
            "candidate {index} missing {reason:?}: {:?}",
            selected.candidates[index].reject_reasons,
        );
    }
}

#[test]
fn focused_pane_recovery_detects_existing_stamped_session() {
    let terminal_30 = id("terminal_30");
    let prior = vec![PriorAgentPane {
        kind: "codex",
        agent_id: "new",
        pane_id: Some(&terminal_30),
        last_activity: jiff::Timestamp::UNIX_EPOCH,
    }];

    assert!(session_already_stamped("codex", "new", &prior));
    assert!(!session_already_stamped("codex", "other", &prior));
}
