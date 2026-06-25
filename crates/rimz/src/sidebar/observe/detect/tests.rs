use super::super::sig::{EventPaneSig, EventsSig, GroupSig, OwnViewSig, WatchedValues};
use super::*;
use crate::SidebarWorktreeKind;
use crate::sidebar::timing::OBSERVE_WARMUP;

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

fn row_with_total_tokens(id: &str, pane: &str, group: &str, total_tokens: Option<u64>) -> RowSig {
    let mut row = row(id, pane, group);
    row.watched.total_tokens = total_tokens;
    row
}

fn row_with_model(id: &str, pane: &str, group: &str, model: Option<&str>) -> RowSig {
    let mut row = row(id, pane, group);
    row.watched.model = model.map(str::to_owned);
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

fn with_sibling_count(mut frame: FrameSig, sibling_count: usize) -> FrameSig {
    if let Some(view) = &mut frame.own_view {
        view.sibling_count = sibling_count;
    }
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
fn duplicate_row_identity_invariants_fire_without_warmup() {
    let mut observer = Observer::default();

    let drafts = observer.observe(sig(
        0,
        vec![
            row("a", "p1", "main"),
            row("a", "p2", "main"),
            row("b", "p1", "main"),
        ],
    ));

    assert!(kinds(&drafts).contains(&"duplicate_row_id"));
    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::DuplicatePaneRows { pane_id, row_ids }
            if pane_id == "p1" && row_ids == &vec!["a".to_owned(), "b".to_owned()]
    )));
}

#[test]
fn roster_flap_suppression_edges_stay_quiet() {
    let mut pane_closed = Observer::default();
    pane_closed.observe(sig(0, vec![row("a", "p1", "main")]));
    pane_closed.observe(sig(11_000, vec![row("a", "p1", "main")]));
    pane_closed.observe(with_pane_closed(sig(12_000, Vec::new()), "p1"));
    let drafts = pane_closed.observe(sig(13_000, vec![row("a", "p1", "main")]));
    assert!(!kinds(&drafts).contains(&"roster_flap"));

    let mut no_siblings = Observer::default();
    no_siblings.observe(with_sibling_count(sig(0, vec![row("a", "p1", "main")]), 1));
    no_siblings.observe(with_sibling_count(
        sig(11_000, vec![row("a", "p1", "main")]),
        1,
    ));
    no_siblings.observe(with_sibling_count(sig(12_000, Vec::new()), 0));
    let drafts = no_siblings.observe(with_sibling_count(
        sig(13_000, vec![row("a", "p1", "main")]),
        0,
    ));
    assert!(!kinds(&drafts).contains(&"roster_flap"));
}

#[test]
fn windowed_detectors_arm_exactly_at_warmup_expiry() {
    let warmup_ms = OBSERVE_WARMUP.as_millis() as u64;
    let mut before = Observer::default();
    before.observe(sig(0, vec![row("a", "p1", "main")]));
    assert!(before.observe(sig(warmup_ms - 1, Vec::new())).is_empty());
    let drafts = before.observe(sig(warmup_ms, vec![row("a", "p1", "main")]));
    assert!(!kinds(&drafts).contains(&"roster_flap"));

    let mut at = Observer::default();
    at.observe(sig(0, vec![row("a", "p1", "main")]));
    at.observe(sig(warmup_ms, Vec::new()));
    let drafts = at.observe(sig(warmup_ms + 1, vec![row("a", "p1", "main")]));
    assert!(kinds(&drafts).contains(&"roster_flap"));
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
    assert!(
        drafts
            .iter()
            .all(|draft| !matches!(draft.kind, AnomalyKind::RosterFlap { .. }))
    );
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
fn flap_detectors_stay_quiet_outside_window() {
    let mut roster = Observer::default();
    roster.observe(sig(0, vec![row("a", "p1", "main")]));
    roster.observe(sig(11_000, vec![row("a", "p1", "main")]));
    roster.observe(sig(12_000, Vec::new()));
    assert!(
        !kinds(&roster.observe(sig(23_001, vec![row("a", "p1", "main")]))).contains(&"roster_flap")
    );

    let mut presence = Observer::default();
    presence.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    presence.observe(sig(
        11_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    presence.observe(sig(12_000, vec![row("b", "p2", "main")]));

    let drafts = presence.observe(sig(
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
fn rebound_pane_under_new_row_id_is_not_short_lived() {
    let mut observer = Observer::default();
    observer.observe(sig(0, Vec::new()));
    // A worktree pane first appears under a branch-keyed identity.
    observer.observe(sig(11_000, vec![row("branch:wt", "p9", "branch:wt")]));

    // Enumeration catches up: the same pane re-keys to its path identity. The
    // old row id is gone, but the pane still backs a row, so it was rebound,
    // not removed.
    let drafts = observer.observe(sig(12_000, vec![row("/repo/wt", "p9", "/repo/wt")]));

    assert!(!kinds(&drafts).contains(&"short_lived_row"));
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
fn value_oscillation_reports_established_value_bounces() {
    for (field, initial, via, back, expected_from, expected_via) in [
        (
            WatchedField::GroupKey,
            row("a", "p1", "external"),
            row("a", "p1", "main"),
            row("a", "p1", "external"),
            "external",
            "main",
        ),
        (
            WatchedField::ContextPct,
            row_with_context_pct("a", "p1", "main", Some(10)),
            row_with_context_pct("a", "p1", "main", None),
            row_with_context_pct("a", "p1", "main", Some(10)),
            "10",
            "<none>",
        ),
        (
            WatchedField::ContextPct,
            row_with_context_pct("a", "p1", "main", Some(10)),
            row_with_context_pct("a", "p1", "main", Some(20)),
            row_with_context_pct("a", "p1", "main", Some(10)),
            "10",
            "20",
        ),
        (
            WatchedField::TotalTokens,
            row_with_total_tokens("a", "p1", "main", Some(100)),
            row_with_total_tokens("a", "p1", "main", Some(200)),
            row_with_total_tokens("a", "p1", "main", Some(100)),
            "100",
            "200",
        ),
        (
            WatchedField::Model,
            row_with_model("a", "p1", "main", Some("sonnet")),
            row_with_model("a", "p1", "main", Some("opus")),
            row_with_model("a", "p1", "main", Some("sonnet")),
            "sonnet",
            "opus",
        ),
    ] {
        let mut observer = Observer::default();
        observer.observe(sig(0, vec![initial.clone()]));
        observer.observe(sig(11_000, vec![initial]));
        observer.observe(sig(12_000, vec![via]));

        let drafts = observer.observe(sig(13_000, vec![back]));

        assert!(
            drafts.iter().any(|draft| matches!(
                &draft.kind,
                AnomalyKind::ValueOscillation {
                    row_id,
                    field: observed,
                    from,
                    via,
                    ..
                } if row_id == "a"
                    && *observed == field
                    && from == expected_from
                    && via == expected_via
            )),
            "missing {field:?} oscillation"
        );
    }
}

#[test]
fn first_anomaly_carries_and_resets_dropped_message_count() {
    let mut observer = Observer {
        dropped_msgs: 4,
        ..Observer::default()
    };

    let drafts = observer.observe(sig(0, vec![row("a", "p1", "main"), row("a", "p2", "main")]));

    assert_eq!(drafts.first().map(|draft| draft.dropped_msgs), Some(4));
    assert_eq!(observer.dropped_msgs, 0);
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
fn status_count_checks_respect_hidden_rows() {
    let cases = [
        (
            "hidden rows allow a larger declared tally",
            2,
            "running",
            3,
            false,
        ),
        (
            "visible groups require exact declared counts",
            0,
            "failed",
            1,
            true,
        ),
    ];
    for (name, hidden_count, status, count, should_mismatch) in cases {
        let mut observer = Observer::default();
        let mut frame = sig(0, vec![row("a", "p1", "main")]);
        frame.groups[0].hidden_count = hidden_count;
        frame.groups[0].status_counts = vec![StatusCountSig {
            status: status.to_owned(),
            count,
        }];

        let drafts = observer.observe(frame);

        assert_eq!(
            kinds(&drafts).contains(&"status_count_mismatch"),
            should_mismatch,
            "{name}"
        );
    }
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
