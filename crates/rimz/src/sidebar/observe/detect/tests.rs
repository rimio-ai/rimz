use super::super::sig::{EventPaneSig, EventsSig, GroupSig, OwnViewSig, WatchedValues};
use super::*;
use crate::SidebarWorktreeKind;

fn sig(at_ms: u64, rows: Vec<RowSig>) -> FrameSig {
    let mut by_group = BTreeMap::<String, (Vec<String>, BTreeMap<String, usize>)>::new();
    for row in &rows {
        let (row_ids, status_counts) = by_group
            .entry(row.group_key.clone())
            .or_insert_with(|| (Vec::new(), BTreeMap::new()));
        row_ids.push(row.row_id.clone());
        if let Some(status) = row.watched.status.as_ref() {
            *status_counts.entry(status.clone()).or_default() += 1;
        }
    }
    FrameSig {
        at_ms,
        panes_produced_at_ms: Some(1),
        rows,
        groups: by_group
            .into_iter()
            .map(|(key, (mut row_ids, status_counts))| {
                row_ids.sort();
                GroupSig {
                    key,
                    kind: SidebarWorktreeKind::Worktree,
                    row_ids,
                    hidden_count: 0,
                    status_counts: status_counts
                        .into_iter()
                        .map(|(status, count)| StatusCountSig { status, count })
                        .collect(),
                }
            })
            .collect(),
        own_view: Some(OwnViewSig {
            sibling_count: 1,
            active_pane_id: Some("zellij:terminal_1".to_owned()),
            working_pane_ids: vec!["zellij:terminal_1".to_owned()],
        }),
        events: EventsSig::default(),
        pulled_rows: 0,
        pulled_panes_produced_at_ms: Some(1),
        gate_reject_streak: 0,
        health_failure_streak: 0,
    }
}

fn row(id: &str, pane: &str, group: &str) -> RowSig {
    row_with_status(id, pane, group, "running")
}

fn row_with_status(id: &str, pane: &str, group: &str, status: &str) -> RowSig {
    RowSig {
        row_id: id.to_owned(),
        is_agent: true,
        pane_id: Some(pane.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        group_key: group.to_owned(),
        watched: WatchedValues {
            status: Some(status.to_owned()),
            context_pct: Some(10),
            total_tokens: Some(100),
            todo_done: Some(1),
            group_key: group.to_owned(),
            model: Some("sonnet".to_owned()),
        },
        sub_agent_ids: Vec::new(),
    }
}

fn row_with_context_pct(id: &str, pane: &str, group: &str, context_pct: Option<u8>) -> RowSig {
    let mut row = row(id, pane, group);
    row.watched.context_pct = context_pct;
    row
}

fn row_with_subagents(id: &str, pane: &str, group: &str, sub_agent_ids: Vec<&str>) -> RowSig {
    let mut row = row(id, pane, group);
    row.sub_agent_ids = sub_agent_ids
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    row
}

fn with_hidden_count(mut frame: FrameSig, group_key: &str, hidden_count: usize) -> FrameSig {
    for group in &mut frame.groups {
        if group.key == group_key {
            group.hidden_count = hidden_count;
        }
    }
    frame
}

fn with_pane_closed(mut frame: FrameSig, pane_id: &str) -> FrameSig {
    frame.events.pane_closed.push(EventPaneSig {
        pane_id: pane_id.to_owned(),
        sent_at_ms: frame.at_ms,
    });
    frame
}

fn kinds(drafts: &[AnomalyDraft]) -> Vec<&'static str> {
    drafts.iter().map(|draft| draft.kind.key()).collect()
}

#[test]
fn roster_flap_fires_after_warmup() {
    let mut observer = Observer::default();
    assert!(
        observer
            .observe(sig(0, vec![row("a", "p1", "main")]))
            .is_empty()
    );
    assert!(
        observer
            .observe(sig(11_000, vec![row("a", "p1", "main")]))
            .is_empty()
    );
    assert!(observer.observe(sig(12_000, Vec::new())).is_empty());

    let drafts = observer.observe(sig(13_000, vec![row("a", "p1", "main")]));

    assert!(drafts.iter().any(|draft| matches!(
        draft.kind,
        AnomalyKind::RosterFlap {
            rows_before: 1,
            rows_after: 1,
            ..
        }
    )));
}

#[test]
fn duplicate_rows_fire_without_warmup() {
    let mut observer = Observer::default();

    let drafts = observer.observe(sig(0, vec![row("a", "p1", "main"), row("a", "p2", "main")]));

    assert!(kinds(&drafts).contains(&"duplicate_row_id"));
}

#[test]
fn duplicate_pane_rows_fire_without_warmup() {
    let mut observer = Observer::default();

    let drafts = observer.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p1", "main")]));

    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::DuplicatePaneRows { pane_id, row_ids }
            if pane_id == "p1" && row_ids == &vec!["a".to_owned(), "b".to_owned()]
    )));
}

#[test]
fn pane_close_suppresses_roster_flap_empty_edge() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main")]));
    observer.observe(sig(11_000, vec![row("a", "p1", "main")]));
    let mut empty = sig(12_000, Vec::new());
    empty.events.pane_closed.push(EventPaneSig {
        pane_id: "p1".to_owned(),
        sent_at_ms: 12_000,
    });
    observer.observe(empty);

    let drafts = observer.observe(sig(13_000, vec![row("a", "p1", "main")]));

    assert!(!kinds(&drafts).contains(&"roster_flap"));
}

#[test]
fn row_presence_flap_reports_single_row_disappearance() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    observer.observe(sig(
        11_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    observer.observe(sig(12_000, vec![row("b", "p2", "main")]));

    let drafts = observer.observe(sig(
        13_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));

    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::RowPresenceFlap { row_id, pane_id, .. }
            if row_id == "a" && pane_id.as_deref() == Some("p1")
    )));
}

#[test]
fn pending_roster_tracks_subsecond_disappearance_until_cleared() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main")]));
    let first = observer.pending_roster_update().expect("initial roster");
    assert_eq!(first.rows.len(), 1);
    observer.clear_roster_update();

    observer.observe(sig(500, Vec::new()));
    let empty = observer.pending_roster_update().expect("empty roster");
    assert!(empty.rows.is_empty());

    observer.observe(sig(750, Vec::new()));
    assert!(
        observer
            .pending_roster_update()
            .expect("retry pending roster")
            .rows
            .is_empty()
    );
}

#[test]
fn pending_roster_tracks_frame_stamp_advances() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main")]));
    observer.clear_roster_update();

    let mut republished = sig(500, vec![row("a", "p1", "main")]);
    republished.panes_produced_at_ms = Some(2);
    observer.observe(republished);

    assert_eq!(
        observer
            .pending_roster_update()
            .expect("republished roster")
            .panes_produced_at_ms,
        Some(2)
    );
}

#[test]
fn short_lived_row_catches_phantom_external() {
    let mut observer = Observer::default();
    observer.observe(sig(0, Vec::new()));
    observer.observe(sig(11_000, vec![row("p", "p9", "external")]));

    let drafts = observer.observe(sig(12_000, Vec::new()));

    assert!(matches!(
        drafts.as_slice(),
        [AnomalyDraft {
            kind: AnomalyKind::ShortLivedRow { row_id, group_key, .. },
            ..
        }] if row_id == "p" && group_key == "external"
    ));
}

#[test]
fn windowed_detectors_are_suppressed_during_warmup() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    assert!(observer.observe(sig(1_000, Vec::new())).is_empty());

    let drafts = observer.observe(sig(
        2_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));

    assert!(!kinds(&drafts).contains(&"roster_flap"));
    assert!(!kinds(&drafts).contains(&"row_presence_flap"));
}

#[test]
fn roster_flap_stays_quiet_outside_window() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main")]));
    observer.observe(sig(11_000, vec![row("a", "p1", "main")]));
    observer.observe(sig(12_000, Vec::new()));

    let drafts = observer.observe(sig(23_001, vec![row("a", "p1", "main")]));

    assert!(!kinds(&drafts).contains(&"roster_flap"));
}

#[test]
fn row_presence_flap_stays_quiet_outside_window() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    observer.observe(sig(
        11_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    observer.observe(sig(12_000, vec![row("b", "p2", "main")]));

    let drafts = observer.observe(sig(
        20_001,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));

    assert!(!kinds(&drafts).contains(&"row_presence_flap"));
}

#[test]
fn hidden_group_suppresses_presence_flap() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    observer.observe(sig(
        11_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    observer.observe(with_hidden_count(
        sig(12_000, vec![row("b", "p2", "main")]),
        "main",
        1,
    ));

    let drafts = observer.observe(sig(
        13_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));

    assert!(!kinds(&drafts).contains(&"row_presence_flap"));
}

#[test]
fn pane_closed_suppresses_row_presence_flap() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    observer.observe(sig(
        11_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    observer.observe(with_pane_closed(
        sig(12_000, vec![row("b", "p2", "main")]),
        "p1",
    ));

    let drafts = observer.observe(sig(
        13_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));

    assert!(!kinds(&drafts).contains(&"row_presence_flap"));
}

#[test]
fn pane_closed_suppresses_short_lived_row() {
    let mut observer = Observer::default();
    observer.observe(sig(0, Vec::new()));
    observer.observe(sig(11_000, vec![row("p", "p9", "external")]));

    let drafts = observer.observe(with_pane_closed(sig(12_000, Vec::new()), "p9"));

    assert!(!kinds(&drafts).contains(&"short_lived_row"));
}

#[test]
fn group_key_oscillation_reports_worktree_external_bounce() {
    let mut observer = Observer::default();
    observer.observe(sig(0, Vec::new()));
    observer.observe(sig(11_000, vec![row("a", "p1", "external")]));
    observer.observe(sig(12_000, vec![row("a", "p1", "main")]));

    let drafts = observer.observe(sig(13_000, vec![row("a", "p1", "external")]));

    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::ValueOscillation {
            row_id,
            field: WatchedField::GroupKey,
            from,
            via,
            ..
        } if row_id == "a" && from == "external" && via == "main"
    )));
}

#[test]
fn first_enrichment_does_not_count_as_value_oscillation() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row_with_context_pct("a", "p1", "main", None)]));
    observer.observe(sig(
        11_000,
        vec![row_with_context_pct("a", "p1", "main", None)],
    ));
    observer.observe(sig(
        12_000,
        vec![row_with_context_pct("a", "p1", "main", Some(10))],
    ));

    let drafts = observer.observe(sig(
        13_000,
        vec![row_with_context_pct("a", "p1", "main", None)],
    ));

    assert!(!drafts.iter().any(|draft| matches!(
        draft.kind,
        AnomalyKind::ValueOscillation {
            field: WatchedField::ContextPct,
            ..
        }
    )));
}

#[test]
fn established_value_disappearance_and_return_counts_as_oscillation() {
    let mut observer = Observer::default();
    observer.observe(sig(
        0,
        vec![row_with_context_pct("a", "p1", "main", Some(10))],
    ));
    observer.observe(sig(
        11_000,
        vec![row_with_context_pct("a", "p1", "main", Some(10))],
    ));
    observer.observe(sig(
        12_000,
        vec![row_with_context_pct("a", "p1", "main", None)],
    ));

    let drafts = observer.observe(sig(
        13_000,
        vec![row_with_context_pct("a", "p1", "main", Some(10))],
    ));

    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::ValueOscillation {
            field: WatchedField::ContextPct,
            from,
            via,
            ..
        } if from == "10" && via == "<none>"
    )));
}

#[test]
fn status_churn_counts_only_real_transitions() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row_with_status("a", "p1", "main", "running")]));
    observer.observe(sig(
        11_000,
        vec![row_with_status("a", "p1", "main", "running")],
    ));
    observer.observe(sig(
        12_000,
        vec![row_with_status("a", "p1", "main", "waiting")],
    ));
    observer.observe(sig(
        13_000,
        vec![row_with_status("a", "p1", "main", "running")],
    ));

    let third_transition = observer.observe(sig(
        14_000,
        vec![row_with_status("a", "p1", "main", "idle")],
    ));
    assert!(!kinds(&third_transition).contains(&"status_churn"));

    let fourth_transition = observer.observe(sig(
        15_000,
        vec![row_with_status("a", "p1", "main", "running")],
    ));
    assert!(fourth_transition.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::StatusChurn {
            row_id,
            transitions: 4,
            ..
        } if row_id == "a"
    )));
}

#[test]
fn hidden_group_allows_declared_status_count_to_exceed_visible_tally() {
    let mut observer = Observer::default();
    let mut frame = sig(0, vec![row("a", "p1", "main")]);
    frame.groups[0].hidden_count = 2;
    frame.groups[0].status_counts = vec![StatusCountSig {
        status: "running".to_owned(),
        count: 3,
    }];

    let drafts = observer.observe(frame);

    assert!(!kinds(&drafts).contains(&"status_count_mismatch"));
}

#[test]
fn visible_group_requires_exact_status_counts() {
    let mut observer = Observer::default();
    let mut frame = sig(0, vec![row("a", "p1", "main")]);
    frame.groups[0].status_counts = vec![StatusCountSig {
        status: "failed".to_owned(),
        count: 1,
    }];

    let drafts = observer.observe(frame);

    assert!(kinds(&drafts).contains(&"status_count_mismatch"));
}

#[test]
fn own_view_active_must_be_working_pane() {
    let mut observer = Observer::default();
    let mut frame = sig(0, vec![row("a", "p1", "main")]);
    frame.own_view = Some(OwnViewSig {
        sibling_count: 1,
        active_pane_id: Some("missing".to_owned()),
        working_pane_ids: vec!["p1".to_owned()],
    });

    let drafts = observer.observe(frame);

    assert!(kinds(&drafts).contains(&"own_view_incoherent"));
}

#[test]
fn subagent_projection_errors_are_detected() {
    let mut observer = Observer::default();
    let drafts = observer.observe(sig(
        0,
        vec![
            row_with_subagents("parent", "p1", "main", vec!["child"]),
            row("child", "p2", "main"),
            row_with_subagents("self-nested", "p3", "main", vec!["self-nested"]),
        ],
    ));

    assert!(kinds(&drafts).contains(&"subagent_double_render"));
    assert!(kinds(&drafts).contains(&"subagent_top_level_leak"));
}

#[test]
fn frameless_rows_are_detected_without_warmup() {
    let mut observer = Observer::default();
    let mut frame = sig(0, vec![row("a", "p1", "main")]);
    frame.panes_produced_at_ms = None;

    let drafts = observer.observe(frame);

    assert!(kinds(&drafts).contains(&"frameless_rows"));
}

#[test]
fn historical_detector_maps_prune_after_row_absence_window() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row_with_status("a", "p1", "main", "running")]));
    observer.observe(sig(
        11_000,
        vec![row_with_status("a", "p1", "main", "running")],
    ));
    observer.observe(sig(
        12_000,
        vec![row_with_status("a", "p1", "main", "waiting")],
    ));
    assert!(observer.values.keys().any(|(row_id, _)| row_id == "a"));
    assert!(observer.last_status.contains_key("a"));
    assert!(observer.status_transitions.contains_key("a"));

    observer.observe(sig(13_000, Vec::new()));
    observer.observe(sig(21_001, Vec::new()));

    assert!(!observer.values.keys().any(|(row_id, _)| row_id == "a"));
    assert!(!observer.last_status.contains_key("a"));
    assert!(!observer.status_transitions.contains_key("a"));
}
