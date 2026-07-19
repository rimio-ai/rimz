use super::*;

#[test]
fn delivery_sweep_applies_mixed_effects_in_one_transaction() {
    let q = Queue::new();
    let now = Timestamp::now();
    let retry_at = now + Duration::from_secs(30);
    let after = AfterCondition {
        kind: AgentKind::new_unchecked("claude"),
        agent_id: AgentSessionId::from("sess-planner"),
        agent_name: Some("planner".to_owned()),
        address: "@planner".to_owned(),
        met_at: None,
    };
    let when = WhenCondition {
        kind: AgentKind::new_unchecked("codex"),
        agent_id: AgentSessionId::from("sess-coder"),
        agent_name: Some("coder".to_owned()),
        address: "@coder".to_owned(),
        status: AgentStatus::Running,
        dwell_secs: 60,
        met_at: None,
    };
    let after_only = q.record(1).with_after(vec![after.clone()]);
    let combined = q
        .record(2)
        .with_after(vec![after])
        .with_when(vec![when.clone()]);
    let retry_only = q.record(3).with_when(vec![when.clone()]);
    let archived = q.record(4).with_when(vec![when.clone()]);
    let claimed = q.record(5).with_when(vec![when]);
    for record in [&after_only, &combined, &retry_only, &archived, &claimed] {
        q.queue_message(record, "session").unwrap();
    }
    q.claim_message_for_steer(&claimed.message_id, now)
        .unwrap()
        .expect("claimed");

    q.apply_delivery_sweep(
        &[
            sweep(&after_only.message_id, &[0], &[], None, None),
            sweep(&combined.message_id, &[0], &[0], None, None),
            sweep(&retry_only.message_id, &[], &[], Some(retry_at), None),
            sweep(
                &archived.message_id,
                &[],
                &[0],
                Some(retry_at),
                Some("watched agent ended"),
            ),
            sweep(&claimed.message_id, &[], &[0], Some(retry_at), None),
            sweep(&message_id(999), &[0], &[0], Some(retry_at), None),
        ],
        now,
        "session",
    )
    .unwrap();

    let live = q.live();
    let find = |id: &MessageId| live.iter().find(|record| record.message_id == *id).unwrap();
    assert_eq!(find(&after_only.message_id).after[0].met_at, Some(now));
    assert_eq!(find(&combined.message_id).after[0].met_at, Some(now));
    assert_eq!(find(&combined.message_id).when[0].met_at, Some(now));
    assert_eq!(find(&retry_only.message_id).retry_after, Some(retry_at));
    // A claimed record is not `Queued`, so the sweep skips it whole.
    assert_eq!(find(&claimed.message_id).status, MessageStatus::Claimed);
    assert_eq!(find(&claimed.message_id).when[0].met_at, None);
    assert!(
        live.iter()
            .all(|record| record.message_id != archived.message_id)
    );

    let archived = q
        .history()
        .into_iter()
        .find(|record| record.message_id == archived.message_id)
        .expect("archived history");
    assert_eq!(archived.status, MessageStatus::Archived);
    assert_eq!(archived.last_error.as_deref(), Some("watched agent ended"));

    assert_eq!(q.count("message.after_met"), 1);
    assert_eq!(q.count("message.when_met"), 1);
    assert_eq!(q.count("message.archived"), 1);
    assert_eq!(q.reason("message.after_met"), "@planner finished");
    assert_eq!(
        q.reason("message.when_met"),
        "@planner finished; @coder running 1m reached"
    );
}

#[test]
fn edit_message_updates_queued_record_and_appends_event() {
    let q = Queue::new();
    let message = q
        .record(1)
        .with_not_before(Some(Timestamp::now() + Duration::from_secs(60)));
    q.queue_message(&message, "session").unwrap();
    q.defer_message_wake(
        &message.message_id,
        Timestamp::now() + Duration::from_secs(30),
    )
    .unwrap();

    let edited = q
        .edit_message(
            &message.message_id,
            MessageEdit {
                text: Some("edited".to_owned()),
                gate: Some(DeliveryGate::Any),
                not_before: Some(None),
                force: Some(true),
                enter: Some(false),
                auto_compact: Some(Some(AutoCompact::Percent(70))),
            },
            "session",
        )
        .unwrap();

    let EditOutcome::Edited(edited) = edited else {
        panic!("message should edit");
    };
    assert_eq!(edited.text, "edited");
    assert_eq!(edited.gate, DeliveryGate::Any);
    assert_eq!(edited.not_before, None);
    assert_eq!(edited.retry_after, None);
    assert!(edited.force);
    assert!(!edited.enter);
    assert_eq!(edited.auto_compact, Some(AutoCompact::Percent(70)));

    let live = q.live();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0], *edited);
    assert_eq!(
        q.reason("message.edited"),
        "text, gate, schedule, force, enter, smart_compact"
    );
}

#[test]
fn edit_message_refuses_claimed_terminal_and_missing_records() {
    let q = Queue::new();
    let edit = || MessageEdit {
        text: Some("edited".to_owned()),
        ..MessageEdit::default()
    };
    let claimed = q.queue(1);
    q.claim_message_for_delivery(&claimed.message_id, Timestamp::now())
        .unwrap()
        .expect("claimed");

    assert_eq!(
        q.edit_message(&claimed.message_id, edit(), "session")
            .unwrap(),
        EditOutcome::NotOpen(MessageStatus::Claimed)
    );

    let terminal = q.queue(2);
    q.settle_message(
        &terminal.message_id,
        MessageStatus::Delivered,
        "session",
        None,
    )
    .unwrap();

    assert_eq!(
        q.edit_message(&terminal.message_id, edit(), "session")
            .unwrap(),
        EditOutcome::NotOpen(MessageStatus::Delivered)
    );
    assert_eq!(
        q.edit_message(&message_id(99), edit(), "session").unwrap(),
        EditOutcome::NotFound
    );
}

#[test]
fn orphan_gc_keeps_provisional_message_when_registered_card_name_is_live() {
    let q = Queue::new();
    let mut provisional = agent();
    provisional.agent_id = AgentSessionId::from("launch_a");
    provisional.name = Some("lucid-atlas".to_owned());
    let message = MessageRecord::new(
        q.workspace_id.clone(),
        &provisional,
        "next".to_owned(),
        true,
        DeliveryGate::Done,
    );
    q.queue_message(&message, "session").unwrap();

    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("real-session")),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some("lucid-atlas".to_owned());
    q.append_event(&EventEnvelope::agent_lifecycle(
        q.workspace_id.clone(),
        "session",
        "claude",
        "SessionStart",
        &observation,
    ))
    .unwrap();

    assert_eq!(q.archive_orphan_messages("session").unwrap(), 0);
    let messages = q.live();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].status, MessageStatus::Queued);
}

#[test]
fn archive_selects_only_matching_open_messages() {
    type Setup = fn(&Queue) -> Vec<MessageRecord>;
    type Archive = fn(&Queue, &[MessageRecord]) -> usize;

    // (case, queued records, the archive call, expected event reason, survivors)
    let cases: [(&str, Setup, Archive, &str, usize); 3] = [
        (
            "orphan receiver",
            |q| vec![q.queue(1)],
            |q, _| q.archive_orphan_messages("session").unwrap(),
            "receiver ended",
            0,
        ),
        (
            "by card",
            |q| {
                let target = q.queue(1);
                let mut other = q.record(2);
                other.agent_id = AgentSessionId::from("sess-2");
                q.queue_message(&other, "session").unwrap();
                vec![target, other]
            },
            |q, records| {
                let target = &records[0];
                q.archive_messages_for_card(
                    &target.kind,
                    &target.agent_id,
                    target.agent_name.as_deref(),
                    "receiver ended",
                    "session",
                )
                .unwrap()
            },
            "receiver ended",
            1,
        ),
        (
            "by channel",
            |q| {
                let docs = q.record(1).with_channel(Some("docs".to_owned()));
                let ops = q.record(2).with_channel(Some("ops".to_owned()));
                for record in [&docs, &ops] {
                    q.queue_message(record, "session").unwrap();
                }
                vec![docs, ops]
            },
            |q, _| {
                q.archive_channel_messages("docs", "worktree removed", "session")
                    .unwrap()
            },
            "worktree removed",
            1,
        ),
    ];

    for (case, setup, archive, reason, survivors) in cases {
        let q = Queue::new();
        let records = setup(&q);

        assert_eq!(archive(&q, &records), 1, "{case}");

        let live = q.live();
        assert_eq!(live.len(), survivors, "{case}");
        assert!(
            live.iter()
                .all(|record| record.status == MessageStatus::Queued),
            "{case}"
        );
        assert_eq!(q.count("message.archived"), 1, "{case}");
        assert_eq!(q.reason("message.archived"), reason, "{case}");
    }
}

#[test]
fn archive_messages_watching_card_expires_only_unmet_conditions() {
    let q = Queue::new();
    let watched = AgentState::stub("claude", "sess-planner", AgentStatus::Running);
    let condition = WhenCondition {
        kind: watched.kind.clone(),
        agent_id: watched.agent_id.clone(),
        agent_name: Some("planner".to_owned()),
        address: "@planner".to_owned(),
        status: AgentStatus::Running,
        dwell_secs: 7_200,
        met_at: None,
    };
    let unmet = q.record(1).with_when(vec![condition.clone()]);
    let mut met_condition = condition;
    met_condition.met_at = Some(Timestamp::now());
    let met = q.record(2).with_when(vec![met_condition]);
    for record in [&unmet, &met] {
        q.queue_message(record, "session").unwrap();
    }

    let archived = q
        .archive_messages_watching_card(
            &watched.kind,
            &watched.agent_id,
            Some("planner"),
            "session",
        )
        .unwrap();

    assert_eq!(archived, 1);
    let pending = q.live();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message_id, met.message_id);
    let archived = q
        .history()
        .into_iter()
        .find(|message| message.message_id == unmet.message_id)
        .unwrap();
    assert_eq!(
        archived.last_error.as_deref(),
        Some("watched agent @planner ended before 'running 2h' was met")
    );
}

#[test]
fn single_terminal_transitions_share_exact_history_and_event_contract() {
    for (index, status, method) in [
        (1, MessageStatus::Delivered, "message.delivered"),
        (2, MessageStatus::TimedOut, "message.timed_out"),
        (3, MessageStatus::Errored, "message.errored"),
        (4, MessageStatus::Abandoned, "message.abandoned"),
        (5, MessageStatus::Archived, "message.archived"),
        (6, MessageStatus::Canceled, "message.canceled"),
    ] {
        let q = Queue::new();
        let mut queued = q.record(index);
        queued.text = format!("terminal {index}");
        q.queue_message(&queued, "session").unwrap();
        let before = Timestamp::now();

        let terminal = q
            .settle_message(
                &queued.message_id,
                status,
                "session",
                Some("terminal reason"),
            )
            .unwrap()
            .expect("accepted terminal transition");

        assert!(q.live().is_empty());
        assert_eq!(q.history(), vec![terminal.clone()]);
        assert_eq!(terminal.status, status);
        assert_eq!(terminal.text, format!("terminal {index}"));
        assert!(terminal.updated_at >= before);
        assert_eq!(
            terminal.delivered_at,
            (status == MessageStatus::Delivered).then_some(terminal.updated_at)
        );
        assert_eq!(
            terminal.last_error.as_deref(),
            (status == MessageStatus::Archived).then_some("terminal reason")
        );
        let event = q.events().pop().expect("terminal event");
        assert_eq!(event.method, method);
        assert_eq!(event.params_value()["status"], status.as_str());
        assert_eq!(q.reason(method), "terminal reason");
    }
}

#[test]
fn send_error_for_missing_message_archives_supplied_record_once() {
    let q = Queue::new();
    let supplied = q.record(1);

    let errored = q
        .record_send_error(&supplied, "pane vanished", "session")
        .unwrap()
        .expect("missing record fallback");

    assert!(q.live().is_empty());
    assert_eq!(q.history(), vec![errored.clone()]);
    assert_eq!(errored.status, MessageStatus::Errored);
    assert_eq!(errored.text, supplied.text);
    assert_eq!(errored.last_error.as_deref(), Some("pane vanished"));
    assert_eq!(errored.delivered_at, None);
    assert_eq!(q.methods(), ["message.errored"]);
    assert_eq!(q.reason("message.errored"), "pane vanished");
}
