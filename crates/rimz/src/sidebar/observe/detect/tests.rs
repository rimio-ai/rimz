use super::super::sig::{
    AggregateKey, AggregateSig, EventPaneSig, EventsSig, GroupSig, OwnViewSig, WatchedValues,
    extract_sig,
};
use super::*;
use crate::SidebarWorktreeKind;
use crate::sidebar::events::EventStore;
use crate::sidebar::timing::OBSERVE_WARMUP;
use crate::{SpendTally, SpendWindow, WorkspaceId};

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
                let render_order = row_ids.clone();
                row_ids.sort();
                GroupSig {
                    key,
                    kind: SidebarWorktreeKind::Worktree,
                    row_ids,
                    render_order,
                    hidden_count: 0,
                    status_counts: status_counts
                        .into_iter()
                        .map(|(status, count)| StatusCountSig { status, count })
                        .collect(),
                }
            })
            .collect(),
        aggregates: Vec::new(),
        own_view: Some(OwnViewSig {
            sibling_count: 1,
            focused_pane: Some("zellij:terminal_1".to_owned()),
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

fn with_aggregate(
    mut frame: FrameSig,
    key: AggregateKey,
    committed: Option<&str>,
    pulled: Option<&str>,
) -> FrameSig {
    frame.aggregates = vec![AggregateSig {
        key,
        committed: committed.map(str::to_owned),
        pulled: pulled.map(str::to_owned),
    }];
    frame
}

fn spend_tally(year_usd: f64) -> SpendTally {
    SpendTally {
        year: SpendWindow {
            usd: year_usd,
            ..SpendWindow::default()
        },
        ..SpendTally::default()
    }
}

fn snapshot_with_spend(year_usd: Option<f64>) -> crate::SidebarSnapshot {
    let mut snapshot = crate::SidebarSnapshot::build(
        WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        Vec::new(),
        Vec::new(),
        jiff::Timestamp::now(),
    );
    snapshot.value_tally = year_usd.map(spend_tally);
    snapshot
}

fn extracted_spend_sig(at_ms: u64, committed_usd: f64, pulled_usd: f64) -> FrameSig {
    extract_sig(
        &snapshot_with_spend(Some(committed_usd)),
        &snapshot_with_spend(Some(pulled_usd)),
        &EventStore::default(),
        0,
        0,
        at_ms,
    )
}

fn has_kind(drafts: &[AnomalyDraft], key: &'static str) -> bool {
    drafts.iter().any(|draft| draft.kind.key() == key)
}

fn assert_lacks_kind(drafts: &[AnomalyDraft], key: &'static str, case: &str) {
    assert!(!has_kind(drafts, key), "{case}");
}

fn codex_mana_key() -> AggregateKey {
    AggregateKey::ProviderMana {
        kind: "codex".to_owned(),
        duration_mins: Some(300),
    }
}

#[test]
fn pending_roster_records_structural_and_stamp_changes() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main")]));
    let first = observer.pending_roster_update().expect("initial roster");
    assert_eq!(first.rows.len(), 1);

    observer.observe(sig(250, vec![row("a", "p1", "main")]));
    assert_eq!(
        observer
            .pending_roster_update()
            .expect("pending roster")
            .rows,
        first.rows
    );
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
    observer.clear_roster_update();

    let mut republished = sig(1_000, Vec::new());
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
fn single_frame_invariants_fire_without_warmup() {
    let mut observer = Observer::default();
    let drafts = observer.observe(sig(
        0,
        vec![
            row("a", "p1", "main"),
            row("a", "p2", "main"),
            row("b", "p1", "main"),
        ],
    ));
    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::DuplicateRowId { row_id, count } if row_id == "a" && *count == 2
    )));
    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::DuplicatePaneRows { pane_id, row_ids }
            if pane_id == "p1" && row_ids == &vec!["a".to_owned(), "b".to_owned()]
    )));

    let mut visible_count = sig(0, vec![row("a", "p1", "main")]);
    visible_count.groups[0].status_counts = vec![StatusCountSig {
        status: "failed".to_owned(),
        count: 1,
    }];
    assert!(has_kind(
        &Observer::default().observe(visible_count),
        "status_count_mismatch"
    ));

    let mut hidden_count = sig(0, vec![row("a", "p1", "main")]);
    hidden_count.groups[0].hidden_count = 2;
    hidden_count.groups[0].status_counts = vec![StatusCountSig {
        status: "running".to_owned(),
        count: 3,
    }];
    assert_lacks_kind(
        &Observer::default().observe(hidden_count),
        "status_count_mismatch",
        "hidden rows allow declared surplus",
    );

    let drafts = Observer::default().observe(sig(
        0,
        vec![
            row_with_subagents("parent", "p1", "main", vec!["child"]),
            row("child", "p2", "main"),
            row_with_subagents("self-nested", "p3", "main", vec!["self-nested"]),
        ],
    ));
    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::SubagentTopLevelLeak { agent_id } if agent_id == "self-nested"
    )));
    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::SubagentDoubleRender { id } if id == "child"
    )));

    let mut frameless = sig(0, vec![row("a", "p1", "main")]);
    frameless.panes_produced_at_ms = None;
    let drafts = Observer::default().observe(frameless);
    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::FramelessRows { rows } if rows == &vec!["a".to_owned()]
    )));
}

#[test]
fn dropped_message_count_attaches_to_first_emitted_anomaly() {
    let mut observer = Observer {
        dropped_msgs: 4,
        ..Observer::default()
    };

    let drafts = observer.observe(sig(0, vec![row("a", "p1", "main"), row("a", "p2", "main")]));

    assert_eq!(drafts.first().map(|draft| draft.dropped_msgs), Some(4));
    assert!(drafts.iter().skip(1).all(|draft| draft.dropped_msgs == 0));
    assert_eq!(observer.dropped_msgs, 0);
}

#[test]
fn roster_flap_respects_warmup_window_and_empty_tab_guards() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main")]));
    observer.observe(sig(11_000, vec![row("a", "p1", "main")]));
    observer.observe(sig(12_000, Vec::new()));
    let drafts = observer.observe(sig(13_000, vec![row("a", "p1", "main")]));
    assert!(drafts.iter().any(|draft| matches!(
        draft.kind,
        AnomalyKind::RosterFlap {
            rows_before: 1,
            rows_after: 1,
            empty_at_ms: 12_000,
            restored_at_ms: 13_000
        }
    )));

    let mut pre_warmup = Observer::default();
    pre_warmup.observe(sig(0, vec![row("a", "p1", "main")]));
    pre_warmup.observe(sig(1_000, Vec::new()));
    let drafts = pre_warmup.observe(sig(2_000, vec![row("a", "p1", "main")]));
    assert_lacks_kind(&drafts, "roster_flap", "pre-warmup empty/refill");

    let warmup_ms = OBSERVE_WARMUP.as_millis() as u64;
    let mut before = Observer::default();
    before.observe(sig(0, vec![row("a", "p1", "main")]));
    assert!(before.observe(sig(warmup_ms - 1, Vec::new())).is_empty());
    let drafts = before.observe(sig(warmup_ms, vec![row("a", "p1", "main")]));
    assert_lacks_kind(&drafts, "roster_flap", "boundary after pre-warmup empty");

    let mut at = Observer::default();
    at.observe(sig(0, vec![row("a", "p1", "main")]));
    at.observe(sig(warmup_ms, Vec::new()));
    let drafts = at.observe(sig(warmup_ms + 1, vec![row("a", "p1", "main")]));
    assert!(has_kind(&drafts, "roster_flap"));

    let mut pane_closed = Observer::default();
    pane_closed.observe(sig(0, vec![row("a", "p1", "main")]));
    pane_closed.observe(sig(11_000, vec![row("a", "p1", "main")]));
    pane_closed.observe(with_pane_closed(sig(12_000, Vec::new()), "p1"));
    let drafts = pane_closed.observe(sig(13_000, vec![row("a", "p1", "main")]));
    assert_lacks_kind(&drafts, "roster_flap", "pane closed");

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
    assert_lacks_kind(&drafts, "roster_flap", "empty tab has no siblings");

    let mut outside_window = Observer::default();
    outside_window.observe(sig(0, vec![row("a", "p1", "main")]));
    outside_window.observe(sig(11_000, vec![row("a", "p1", "main")]));
    outside_window.observe(sig(12_000, Vec::new()));
    let drafts = outside_window.observe(sig(23_001, vec![row("a", "p1", "main")]));
    assert_lacks_kind(&drafts, "roster_flap", "refill outside window");
}

#[test]
fn row_presence_reports_real_blinks_and_short_lived_rows() {
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
        AnomalyKind::RowPresenceFlap {
            row_id,
            pane_id,
            gone_at_ms: 12_000,
            back_at_ms: 13_000
        } if row_id == "a" && pane_id.as_deref() == Some("p1")
    )));
    assert_lacks_kind(
        &drafts,
        "roster_flap",
        "single-row blink keeps roster nonempty",
    );

    let mut phantom = Observer::default();
    phantom.observe(sig(0, Vec::new()));
    phantom.observe(sig(11_000, vec![row("p", "p9", "external")]));
    let drafts = phantom.observe(sig(12_000, Vec::new()));
    assert!(matches!(
        drafts.as_slice(),
        [AnomalyDraft {
            kind:
                AnomalyKind::ShortLivedRow {
                    row_id,
                    pane_id,
                    group_key,
                    born_at_ms: 11_000,
                    gone_at_ms: 12_000,
                },
            ..
        }] if row_id == "p" && pane_id.as_deref() == Some("p9") && group_key == "external"
    ));
}

#[test]
fn row_presence_ignores_expected_absence_causes() {
    let mut hidden = Observer::default();
    hidden.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    hidden.observe(sig(
        11_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    hidden.observe(with_hidden_count(
        sig(12_000, vec![row("b", "p2", "main")]),
        "main",
        1,
    ));
    let drafts = hidden.observe(sig(
        13_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    assert_lacks_kind(&drafts, "row_presence_flap", "hidden group cap");

    let mut pane_closed = Observer::default();
    pane_closed.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    pane_closed.observe(sig(
        11_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    pane_closed.observe(with_pane_closed(
        sig(12_000, vec![row("b", "p2", "main")]),
        "p1",
    ));
    let drafts = pane_closed.observe(sig(
        13_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    assert_lacks_kind(&drafts, "row_presence_flap", "closed pane absence");

    let mut closed_short = Observer::default();
    closed_short.observe(sig(0, Vec::new()));
    closed_short.observe(sig(11_000, vec![row("p", "p9", "external")]));
    let drafts = closed_short.observe(with_pane_closed(sig(12_000, Vec::new()), "p9"));
    assert_lacks_kind(&drafts, "short_lived_row", "closed pane short-lived row");

    let mut outside_window = Observer::default();
    outside_window.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    outside_window.observe(sig(
        11_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    outside_window.observe(sig(12_000, vec![row("b", "p2", "main")]));
    let drafts = outside_window.observe(sig(
        20_001,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    assert_lacks_kind(&drafts, "row_presence_flap", "return outside window");

    let mut rebound = Observer::default();
    rebound.observe(sig(0, Vec::new()));
    rebound.observe(sig(11_000, vec![row("branch:wt", "p9", "branch:wt")]));
    let drafts = rebound.observe(sig(12_000, vec![row("/repo/wt", "p9", "/repo/wt")]));
    assert_lacks_kind(&drafts, "short_lived_row", "branch key rebounded to path");
}

#[test]
fn value_oscillation_reports_each_watched_field() {
    struct Case {
        field: WatchedField,
        initial: RowSig,
        via: RowSig,
        back: RowSig,
        expected_from: &'static str,
        expected_via: &'static str,
    }

    for case in [
        Case {
            field: WatchedField::Status,
            initial: row_with_status("a", "p1", "main", "running"),
            via: row_with_status("a", "p1", "main", "waiting"),
            back: row_with_status("a", "p1", "main", "running"),
            expected_from: "running",
            expected_via: "waiting",
        },
        Case {
            field: WatchedField::GroupKey,
            initial: row("a", "p1", "external"),
            via: row("a", "p1", "main"),
            back: row("a", "p1", "external"),
            expected_from: "external",
            expected_via: "main",
        },
        Case {
            field: WatchedField::ContextPct,
            initial: row_with_context_pct("a", "p1", "main", Some(10)),
            via: row_with_context_pct("a", "p1", "main", Some(20)),
            back: row_with_context_pct("a", "p1", "main", Some(10)),
            expected_from: "10",
            expected_via: "20",
        },
        Case {
            field: WatchedField::TotalTokens,
            initial: row_with_total_tokens("a", "p1", "main", Some(100)),
            via: row_with_total_tokens("a", "p1", "main", Some(200)),
            back: row_with_total_tokens("a", "p1", "main", Some(100)),
            expected_from: "100",
            expected_via: "200",
        },
        Case {
            field: WatchedField::Model,
            initial: row_with_model("a", "p1", "main", Some("sonnet")),
            via: row_with_model("a", "p1", "main", Some("opus")),
            back: row_with_model("a", "p1", "main", Some("sonnet")),
            expected_from: "sonnet",
            expected_via: "opus",
        },
    ] {
        let mut observer = Observer::default();
        observer.observe(sig(0, vec![case.initial.clone()]));
        observer.observe(sig(11_000, vec![case.initial]));
        observer.observe(sig(12_000, vec![case.via]));

        let drafts = observer.observe(sig(13_000, vec![case.back]));

        assert!(
            drafts.iter().any(|draft| matches!(
                &draft.kind,
                AnomalyKind::ValueOscillation {
                    row_id,
                    field,
                    from,
                    via,
                    ..
                } if row_id == "a"
                    && *field == case.field
                    && from == case.expected_from
                    && via == case.expected_via
            )),
            "missing {:?} oscillation",
            case.field
        );
    }

    let mut first_enrichment = Observer::default();
    first_enrichment.observe(sig(0, vec![row_with_context_pct("a", "p1", "main", None)]));
    first_enrichment.observe(sig(
        11_000,
        vec![row_with_context_pct("a", "p1", "main", None)],
    ));
    first_enrichment.observe(sig(
        12_000,
        vec![row_with_context_pct("a", "p1", "main", Some(10))],
    ));
    let drafts = first_enrichment.observe(sig(
        13_000,
        vec![row_with_context_pct("a", "p1", "main", None)],
    ));
    assert_lacks_kind(
        &drafts,
        "value_oscillation",
        "first None to value to None enrichment",
    );

    let mut outside_window = Observer::default();
    outside_window.observe(sig(0, vec![row_with_status("a", "p1", "main", "running")]));
    outside_window.observe(sig(
        11_000,
        vec![row_with_status("a", "p1", "main", "running")],
    ));
    outside_window.observe(sig(
        12_000,
        vec![row_with_status("a", "p1", "main", "waiting")],
    ));
    let drafts = outside_window.observe(sig(
        17_001,
        vec![row_with_status("a", "p1", "main", "running")],
    ));
    assert_lacks_kind(&drafts, "value_oscillation", "status bounce outside window");
}

#[test]
fn status_churn_requires_four_real_transitions() {
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
    let repeated = observer.observe(sig(
        12_500,
        vec![row_with_status("a", "p1", "main", "waiting")],
    ));
    assert_lacks_kind(&repeated, "status_churn", "same status repeated");
    observer.observe(sig(
        13_000,
        vec![row_with_status("a", "p1", "main", "running")],
    ));

    let third_transition = observer.observe(sig(
        14_000,
        vec![row_with_status("a", "p1", "main", "idle")],
    ));
    assert_lacks_kind(&third_transition, "status_churn", "third transition");

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
fn aggregate_oscillation_reports_spend_and_mana_bounces() {
    struct Case {
        name: &'static str,
        key: AggregateKey,
        from: &'static str,
        via_committed: Option<&'static str>,
        via_pulled: Option<&'static str>,
        expected_pulled_via: &'static str,
    }

    for case in [
        Case {
            name: "cockpit producer published zero",
            key: AggregateKey::CockpitTally,
            from: "1234",
            via_committed: None,
            via_pulled: None,
            expected_pulled_via: "0",
        },
        Case {
            name: "cockpit consumer zeroed",
            key: AggregateKey::CockpitTally,
            from: "1234",
            via_committed: None,
            via_pulled: Some("1234"),
            expected_pulled_via: "1234",
        },
        Case {
            name: "provider mana bounce",
            key: codex_mana_key(),
            from: "88",
            via_committed: Some("0"),
            via_pulled: Some("0"),
            expected_pulled_via: "0",
        },
    ] {
        let mut observer = Observer::default();
        observer.observe(with_aggregate(
            sig(0, Vec::new()),
            case.key.clone(),
            Some(case.from),
            Some(case.from),
        ));
        observer.observe(with_aggregate(
            sig(11_000, Vec::new()),
            case.key.clone(),
            Some(case.from),
            Some(case.from),
        ));
        observer.observe(with_aggregate(
            sig(12_000, Vec::new()),
            case.key.clone(),
            case.via_committed,
            case.via_pulled,
        ));

        let drafts = observer.observe(with_aggregate(
            sig(13_000, Vec::new()),
            case.key.clone(),
            Some(case.from),
            Some(case.from),
        ));

        assert!(
            drafts.iter().any(|draft| matches!(
                &draft.kind,
                AnomalyKind::AggregateOscillation {
                    aggregate,
                    from,
                    via,
                    back,
                    pulled_via,
                    ..
                } if aggregate == &case.key
                    && from == case.from
                    && via == "0"
                    && back == case.from
                    && pulled_via.as_deref() == Some(case.expected_pulled_via)
            )),
            "missing aggregate oscillation for {}",
            case.name
        );
    }

    let mut first_appearance = Observer::default();
    first_appearance.observe(with_aggregate(
        sig(0, Vec::new()),
        AggregateKey::WorkspaceTally,
        None,
        None,
    ));
    first_appearance.observe(with_aggregate(
        sig(11_000, Vec::new()),
        AggregateKey::WorkspaceTally,
        None,
        None,
    ));
    first_appearance.observe(with_aggregate(
        sig(12_000, Vec::new()),
        AggregateKey::WorkspaceTally,
        Some("7"),
        Some("7"),
    ));
    let drafts = first_appearance.observe(with_aggregate(
        sig(13_000, Vec::new()),
        AggregateKey::WorkspaceTally,
        None,
        None,
    ));
    assert_lacks_kind(
        &drafts,
        "aggregate_oscillation",
        "aggregate first appearance",
    );

    let mut warmup = Observer::default();
    warmup.observe(with_aggregate(
        sig(0, Vec::new()),
        AggregateKey::CockpitTally,
        Some("7"),
        Some("7"),
    ));
    warmup.observe(with_aggregate(
        sig(1_000, Vec::new()),
        AggregateKey::CockpitTally,
        None,
        None,
    ));
    let drafts = warmup.observe(with_aggregate(
        sig(2_000, Vec::new()),
        AggregateKey::CockpitTally,
        Some("7"),
        Some("7"),
    ));
    assert_lacks_kind(&drafts, "aggregate_oscillation", "aggregate warmup");
}

#[test]
fn aggregate_reset_reports_spend_drops_only() {
    struct ResetCase {
        name: &'static str,
        key: AggregateKey,
        from: &'static str,
        pulled: Option<&'static str>,
    }

    for case in [
        ResetCase {
            name: "cockpit producer published zero",
            key: AggregateKey::CockpitTally,
            from: "1234",
            pulled: Some("0"),
        },
        ResetCase {
            name: "cockpit consumer zeroed",
            key: AggregateKey::CockpitTally,
            from: "1234",
            pulled: Some("1234"),
        },
        ResetCase {
            name: "workspace spend reset",
            key: AggregateKey::WorkspaceTally,
            from: "500",
            pulled: Some("0"),
        },
        ResetCase {
            name: "provider spend reset",
            key: AggregateKey::ProviderSpend {
                kind: "claude".to_owned(),
            },
            from: "500",
            pulled: Some("500"),
        },
    ] {
        let mut observer = Observer::default();
        observer.observe(with_aggregate(
            sig(0, Vec::new()),
            case.key.clone(),
            Some(case.from),
            Some(case.from),
        ));
        observer.observe(with_aggregate(
            sig(11_000, Vec::new()),
            case.key.clone(),
            Some(case.from),
            Some(case.from),
        ));

        let drafts = observer.observe(with_aggregate(
            sig(12_000, Vec::new()),
            case.key.clone(),
            Some("0"),
            case.pulled,
        ));

        assert_eq!(
            drafts
                .iter()
                .filter(|draft| matches!(draft.kind, AnomalyKind::AggregateReset { .. }))
                .count(),
            1,
            "wrong aggregate reset count for {}",
            case.name
        );
        assert!(
            drafts.iter().any(|draft| matches!(
                &draft.kind,
                AnomalyKind::AggregateReset {
                    aggregate,
                    from,
                    pulled,
                } if aggregate == &case.key && from == case.from && pulled.as_deref() == case.pulled
            )),
            "missing aggregate reset for {}",
            case.name
        );
    }

    let mut mana_roll = Observer::default();
    mana_roll.observe(with_aggregate(
        sig(0, Vec::new()),
        codex_mana_key(),
        Some("88"),
        Some("88"),
    ));
    mana_roll.observe(with_aggregate(
        sig(11_000, Vec::new()),
        codex_mana_key(),
        Some("88"),
        Some("88"),
    ));
    let drafts = mana_roll.observe(with_aggregate(
        sig(12_000, Vec::new()),
        codex_mana_key(),
        Some("0"),
        Some("0"),
    ));
    assert_lacks_kind(&drafts, "aggregate_reset", "provider mana can roll to zero");

    let mut first_zero = Observer::default();
    first_zero.observe(with_aggregate(
        sig(0, Vec::new()),
        AggregateKey::WorkspaceTally,
        None,
        None,
    ));
    first_zero.observe(with_aggregate(
        sig(11_000, Vec::new()),
        AggregateKey::WorkspaceTally,
        None,
        None,
    ));
    let drafts = first_zero.observe(with_aggregate(
        sig(12_000, Vec::new()),
        AggregateKey::WorkspaceTally,
        Some("0"),
        Some("0"),
    ));
    assert_lacks_kind(&drafts, "aggregate_reset", "first zero");

    let mut warmup_zero = Observer::default();
    warmup_zero.observe(with_aggregate(
        sig(0, Vec::new()),
        AggregateKey::CockpitTally,
        Some("7"),
        Some("7"),
    ));
    assert!(
        warmup_zero
            .observe(with_aggregate(
                sig(1_000, Vec::new()),
                AggregateKey::CockpitTally,
                Some("0"),
                Some("0"),
            ))
            .is_empty()
    );
    let drafts = warmup_zero.observe(with_aggregate(
        sig(11_000, Vec::new()),
        AggregateKey::CockpitTally,
        Some("0"),
        Some("0"),
    ));
    assert_lacks_kind(&drafts, "aggregate_reset", "warmup zero");

    let mut recovery = Observer::default();
    recovery.observe(with_aggregate(
        sig(0, Vec::new()),
        AggregateKey::ProviderSpend {
            kind: "claude".to_owned(),
        },
        Some("0"),
        Some("0"),
    ));
    recovery.observe(with_aggregate(
        sig(11_000, Vec::new()),
        AggregateKey::ProviderSpend {
            kind: "claude".to_owned(),
        },
        Some("0"),
        Some("0"),
    ));
    let drafts = recovery.observe(with_aggregate(
        sig(12_000, Vec::new()),
        AggregateKey::ProviderSpend {
            kind: "claude".to_owned(),
        },
        Some("500"),
        Some("500"),
    ));
    assert_lacks_kind(&drafts, "aggregate_reset", "zero-to-nonzero recovery");
}

#[test]
fn aggregate_spend_signature_quantizes_to_cents() {
    let frame = extracted_spend_sig(0, 1.234, 1.236);
    let cockpit = frame
        .aggregates
        .iter()
        .find(|aggregate| aggregate.key == AggregateKey::CockpitTally)
        .expect("cockpit aggregate");
    assert_eq!(cockpit.committed.as_deref(), Some("123"));
    assert_eq!(cockpit.pulled.as_deref(), Some("124"));

    let mut observer = Observer::default();
    observer.observe(extracted_spend_sig(0, 1.2340, 1.2340));
    observer.observe(extracted_spend_sig(11_000, 1.2344, 1.2344));
    let drafts = observer.observe(extracted_spend_sig(12_000, 1.2340, 1.2340));
    assert_lacks_kind(&drafts, "aggregate_oscillation", "same rounded cents");
}

#[test]
fn order_flap_reports_only_stable_membership_reorders() {
    let mut observer = Observer::default();
    observer.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
    observer.observe(sig(
        11_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    observer.observe(sig(
        12_000,
        vec![row("b", "p2", "main"), row("a", "p1", "main")],
    ));
    let drafts = observer.observe(sig(
        13_000,
        vec![row("a", "p1", "main"), row("b", "p2", "main")],
    ));
    assert!(drafts.iter().any(|draft| matches!(
        &draft.kind,
        AnomalyKind::OrderFlap {
            group_key,
            order,
            via_order,
            ..
        } if group_key == "main"
            && order == &vec!["a".to_owned(), "b".to_owned()]
            && via_order == &vec!["b".to_owned(), "a".to_owned()]
    )));

    for (name, via) in [
        (
            "membership changes under same stable edge",
            vec![row("a", "p1", "main"), row("c", "p3", "main")],
        ),
        (
            "capped tail rotation changes visible set",
            vec![row("b", "p2", "main"), row("c", "p3", "main")],
        ),
    ] {
        let mut observer = Observer::default();
        observer.observe(sig(0, vec![row("a", "p1", "main"), row("b", "p2", "main")]));
        observer.observe(sig(
            11_000,
            vec![row("a", "p1", "main"), row("b", "p2", "main")],
        ));
        observer.observe(sig(12_000, via));
        let drafts = observer.observe(sig(
            13_000,
            vec![row("a", "p1", "main"), row("b", "p2", "main")],
        ));
        assert_lacks_kind(&drafts, "order_flap", name);
    }
}

#[test]
fn historical_detector_state_prunes_after_row_absence_window() {
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
