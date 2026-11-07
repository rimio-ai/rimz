use super::*;
use crate::agents::{AgentLifecycleObservation, AskKind, LifecycleSignal};

fn fixture(state: RaisedAskState) -> (tempfile::TempDir, StatePaths, Entry<RaisedAsk>) {
    fixture_for("claude", state)
}

fn fixture_for(
    kind: &str,
    state: RaisedAskState,
) -> (tempfile::TempDir, StatePaths, Entry<RaisedAsk>) {
    let dir = tempfile::tempdir().unwrap();
    let paths = StatePaths::under(
        crate::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    let ask = Entry {
        kind: AgentKind::new_unchecked(kind),
        agent_id: "session".into(),
        value: RaisedAsk {
            ask_id: AskId::new(),
            state,
            raised_at: Timestamp::UNIX_EPOCH,
            basis_at: Timestamp::UNIX_EPOCH,
            opened_at: None,
            text: None,
            options: vec![],
            raised_by: None,
        },
    };
    raise(&paths, ask.clone()).unwrap();
    (dir, paths, ask)
}

fn observation(signal: LifecycleSignal) -> AgentLifecycleObservation {
    AgentLifecycleObservation::new(Some("session".into()), signal)
}

fn end(errored: bool, parked_on_background: bool) -> LifecycleSignal {
    LifecycleSignal::TurnEnded {
        errored,
        parked_on_background,
        turn_id: None,
    }
}

fn observe(
    paths: &StatePaths,
    kind: &AgentKind,
    observation: &AgentLifecycleObservation,
    at: Timestamp,
    human: bool,
    reply: Option<&str>,
) -> Result<Option<(RaisedAsk, CloseCause)>, RaisedErr> {
    let prior = AgentState::stub(
        kind.as_str(),
        "session",
        crate::agents::AgentStatus::Running,
    );
    let transition = AgentState::transition(Some(&prior), &observation.signal, false, false);
    super::observe(paths, kind, observation, Some(transition), at, human, reply)
}

fn append(
    paths: &StatePaths,
    kind: &AgentKind,
    observation: &AgentLifecycleObservation,
    at: Timestamp,
) -> crate::store::writer::AgentLifecycleReceipt {
    let runtime = crate::RuntimePaths::under(paths.workspace_id.clone(), &paths.root).unwrap();
    crate::Store::open(paths.clone(), runtime)
        .unwrap()
        .for_hook_ingress(crate::EventId::new(), at)
        .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
            session_name: "test",
            agent_kind: kind.clone(),
            event_name: "test",
            observation,
            spawned_subagents: &[],
        })
        .unwrap()
}

#[test]
fn armed_ask_survives_a_codex_async_question_answer_until_its_clean_end() {
    let (_dir, paths, ask) = fixture_for("codex", RaisedAskState::Armed);
    append(
        &paths,
        &ask.kind,
        &observation(LifecycleSignal::TurnStarted { turn_id: None }),
        Timestamp::UNIX_EPOCH,
    );
    let reply = serde_json::json!([{"answer": "safe", "question": "Which?",
        "questionItemId": serde_json::json!(["request_user_input_async", "call", 0]).to_string()}]);
    let prompt =
        format!("<send_user_message_question_reply>\n{reply}\n</send_user_message_question_reply>");
    let definition = crate::agents::definition_by_kind("codex").unwrap();
    let decoded = definition
        .decode_hook(
            "UserPromptSubmit",
            &serde_json::json!({
                "session_id": "session", "prompt": prompt,
            }),
        )
        .unwrap();
    let obs = decoded.lifecycle().unwrap();
    assert!(obs.prompt.is_none());
    assert!(!obs.ask_queue.as_ref().unwrap().answered.is_empty());
    let at = Timestamp::from_second(1).unwrap();
    let receipt = append(&paths, &ask.kind, obs, at);
    super::observe(&paths, &ask.kind, obs, receipt.transition, at, true, None).unwrap();
    assert_eq!(
        read(&paths).asks.first().map(|ask| ask.value.state),
        Some(RaisedAskState::Armed),
        "an async answer is not a new task or the armed turn's end"
    );
    let obs = observation(end(false, false));
    let at = Timestamp::from_second(2).unwrap();
    let receipt = append(&paths, &ask.kind, &obs, at);
    super::observe(
        &paths,
        &ask.kind,
        &obs,
        receipt.transition,
        at,
        false,
        Some("Ship?"),
    )
    .unwrap();
    let record = read(&paths);
    assert!(record.is_open(&record.asks[0]));
    assert_eq!(record.asks[0].value.text.as_deref(), Some("Ship?"));
}

#[test]
fn superseded_turn_reports_cannot_drop_or_open_the_current_turns_armed_ask() {
    for signal in [
        LifecycleSignal::TurnInterrupted {
            turn_id: Some("A".into()),
        },
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: Some("A".into()),
        },
    ] {
        let (_dir, paths, ask) = fixture_for("grok", RaisedAskState::Armed);
        for (at, id) in [
            (Timestamp::UNIX_EPOCH, "A"),
            (Timestamp::from_second(1).unwrap(), "B"),
        ] {
            append(
                &paths,
                &ask.kind,
                &observation(LifecycleSignal::TurnStarted {
                    turn_id: Some(id.into()),
                }),
                at,
            );
        }
        let obs = observation(signal);
        let at = Timestamp::from_second(2).unwrap();
        let receipt = append(&paths, &ask.kind, &obs, at);
        assert!(matches!(
            receipt.transition.unwrap().kind,
            crate::agents::lifecycle::TransitionKind::Ignored { .. }
        ));
        super::observe(
            &paths,
            &ask.kind,
            &obs,
            receipt.transition,
            at,
            false,
            Some("Old reply?"),
        )
        .unwrap();
        assert_eq!(
            read(&paths).asks,
            vec![ask.clone()],
            "a superseded report is inert"
        );
        let obs = observation(LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: Some("B".into()),
        });
        let at = Timestamp::from_second(3).unwrap();
        let receipt = append(&paths, &ask.kind, &obs, at);
        super::observe(
            &paths,
            &ask.kind,
            &obs,
            receipt.transition,
            at,
            false,
            Some("New reply?"),
        )
        .unwrap();
        assert_eq!(
            read(&paths).asks[0].value.text.as_deref(),
            Some("New reply?")
        );
    }
}

#[test]
fn a_prompt_resuming_parked_work_does_not_drop_an_armed_ask_as_unseen() {
    let (_dir, paths, ask) = fixture(RaisedAskState::Armed);
    append(
        &paths,
        &ask.kind,
        &observation(LifecycleSignal::TurnStarted { turn_id: None }),
        Timestamp::UNIX_EPOCH,
    );
    append(
        &paths,
        &ask.kind,
        &observation(end(false, true)),
        Timestamp::from_second(1).unwrap(),
    );
    let mut obs = observation(LifecycleSignal::TurnStarted { turn_id: None });
    obs.prompt =
        crate::agents::SanitizedPrompt::new(Some("Type: WAIT\nFrom: @rimz\nContent:\nready"));
    let at = Timestamp::from_second(2).unwrap();
    let receipt = append(&paths, &ask.kind, &obs, at);
    assert!(!receipt.transition.unwrap().opened_turn);
    super::observe(&paths, &ask.kind, &obs, receipt.transition, at, false, None).unwrap();
    assert_eq!(
        read(&paths).asks,
        vec![ask],
        "the fold resumed the same logical turn"
    );
}

#[test]
fn the_providers_own_compaction_command_does_not_answer_a_raised_question() {
    let (_dir, paths, ask) = fixture_for("qwen", RaisedAskState::Open);
    let mut obs = observation(LifecycleSignal::TurnStarted { turn_id: None });
    obs.prompt = crate::agents::SanitizedPrompt::new(Some(" \n /compress extra"));
    observe(
        &paths,
        &ask.kind,
        &obs,
        Timestamp::from_second(1).unwrap(),
        true,
        None,
    )
    .unwrap();
    let record = read(&paths);
    assert!(
        record.human_turns.is_empty(),
        "provider-native compaction is not an answer"
    );
    assert!(record.is_open(&record.asks[0]));
}

#[test]
fn delivered_human_messages_close_but_automated_turns_leave_questions_open() {
    use crate::store::message::{self, DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
    for (sender, prompt, closes) in [
        (MessageSender::Human, "yes", true),
        (
            MessageSender::Harness {
                notice: HarnessNotice::CacheKeepalive,
            },
            "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nping",
            false,
        ),
        (MessageSender::System, "/compact", false),
        (
            MessageSender::Harness {
                notice: HarnessNotice::SubagentReport,
            },
            "Type: SUBAGENT_REPORT\nFrom: @rimz\nContent:\nfinished",
            false,
        ),
    ] {
        let (_dir, paths, ask) = fixture(RaisedAskState::Open);
        let target = AgentState::stub("claude", "session", crate::agents::AgentStatus::Success);
        let record = MessageRecord::new(
            paths.workspace_id.clone(),
            &target,
            prompt.into(),
            DeliveryGate::Done,
        )
        .with_sender(sender);
        let sections = message::classify_submitted_prompt(prompt, &[&record], &[]);
        let mut obs = observation(LifecycleSignal::TurnStarted { turn_id: None });
        obs.prompt = crate::agents::SanitizedPrompt::new(Some(prompt));
        let human = message::human_opened_turn(
            &obs,
            &serde_json::json!({}),
            &sections,
            std::slice::from_ref(&record),
            false,
        );
        observe(
            &paths,
            &ask.kind,
            &obs,
            Timestamp::from_second(1).unwrap(),
            human,
            None,
        )
        .unwrap();
        let record = read(&paths);
        assert_eq!(record.is_open(&record.asks[0]), !closes, "{prompt}");
        if !closes {
            observe(
                &paths,
                &ask.kind,
                &observation(end(false, false)),
                Timestamp::from_second(2).unwrap(),
                false,
                None,
            )
            .unwrap();
            let record = read(&paths);
            assert!(record.is_open(&record.asks[0]), "{prompt}");
        }
    }
}

#[test]
fn human_stamp_is_a_replay_safe_max_and_closes_both_race_orders_but_not_ties() {
    let (_dir, paths, ask) = fixture(RaisedAskState::Open);
    let mut obs = observation(LifecycleSignal::TurnStarted { turn_id: None });
    obs.prompt = crate::agents::SanitizedPrompt::new(Some("yes"));
    let later = Timestamp::from_second(2).unwrap();
    let close = observe(&paths, &ask.kind, &obs, later, true, None).unwrap();
    assert!(close.is_some(), "a later human turn closes the open ask");
    let close = close.unwrap();
    assert_eq!(close.0.ask_id, ask.value.ask_id);
    assert_eq!(close.1, CloseCause::UserTurn);
    let record = read(&paths);
    assert!(!record.is_open(&record.asks[0]));
    assert!(
        observe(&paths, &ask.kind, &obs, later, true, None)
            .unwrap()
            .is_none()
    );
    observe(
        &paths,
        &ask.kind,
        &obs,
        Timestamp::from_second(1).unwrap(),
        true,
        None,
    )
    .unwrap();
    assert_eq!(read(&paths).human_turns[0].value.at, later);
    assert!(matches!(
        raise(&paths, ask.clone()),
        Err(RaisedErr::AlreadyAnswered)
    ));
    let mut tied = ask;
    tied.value.basis_at = later;
    raise(&paths, tied).unwrap();
    let record = read(&paths);
    assert!(record.is_open(&record.asks[0]));
}

#[test]
fn armed_outcomes_drop_failed_interrupted_or_unseen_turns_but_wait_for_background() {
    for (signal, cause, expected) in [
        (end(true, false), Some(CloseCause::TurnFailed), None),
        (
            LifecycleSignal::TurnInterrupted { turn_id: None },
            Some(CloseCause::TurnFailed),
            None,
        ),
        (
            LifecycleSignal::TurnStarted { turn_id: None },
            Some(CloseCause::TurnUnseen),
            None,
        ),
        (end(false, true), None, Some(RaisedAskState::Armed)),
        (
            LifecycleSignal::AwaitingInput {
                extra_questions: 0,
                kind: AskKind::Question,
                ask_id: None,
                detail: None,
                native_key: None,
            },
            None,
            Some(RaisedAskState::Armed),
        ),
        (end(false, false), None, Some(RaisedAskState::Open)),
    ] {
        let (_dir, paths, ask) = fixture(RaisedAskState::Armed);
        let obs = observation(signal);
        let at = Timestamp::from_second(1).unwrap();
        let closed = observe(&paths, &ask.kind, &obs, at, false, None).unwrap();
        assert_eq!(
            closed.as_ref().map(|(_, cause)| *cause),
            cause,
            "{:?}",
            obs.signal
        );
        assert_eq!(
            read(&paths).asks.first().map(|ask| ask.value.state),
            expected
        );
        assert!(
            observe(&paths, &ask.kind, &obs, at, false, None)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn armed_open_uses_explicit_then_frame_then_current_turn_transcript_text() {
    for (explicit, frame, expected) in [
        (Some("Explicit?"), Some("Frame?"), Some("Explicit?")),
        (None, Some("Frame?"), Some("Frame?")),
        (None, None, Some("Transcript?")),
    ] {
        let (_dir, paths, mut ask) = fixture(RaisedAskState::Armed);
        ask.value.text = explicit.map(ToOwned::to_owned);
        raise(&paths, ask.clone()).unwrap();
        crate::transcript::append(
            &paths,
            &crate::transcript::TranscriptEntry::new(
                Timestamp::UNIX_EPOCH,
                ask.kind.clone(),
                ask.agent_id.clone(),
                crate::transcript::TranscriptKind::Assistant,
                "Transcript?".into(),
            ),
        )
        .unwrap();
        observe(
            &paths,
            &ask.kind,
            &observation(end(false, false)),
            Timestamp::from_second(1).unwrap(),
            false,
            frame,
        )
        .unwrap();
        let record = read(&paths);
        assert!(record.is_open(&record.asks[0]));
        assert_eq!(record.asks[0].value.text.as_deref(), expected);
    }
}

#[test]
fn compact_never_stamps_and_late_frames_cannot_touch_a_newer_raise() {
    let (_dir, paths, mut ask) = fixture(RaisedAskState::Armed);
    let mut obs = observation(LifecycleSignal::TurnStarted { turn_id: None });
    obs.prompt = crate::agents::SanitizedPrompt::new(Some(" \n /compact extra"));
    observe(&paths, &ask.kind, &obs, Timestamp::UNIX_EPOCH, true, None).unwrap();
    assert!(read(&paths).human_turns.is_empty());
    ask.value.raised_at = Timestamp::from_second(5).unwrap();
    ask.value.basis_at = ask.value.raised_at;
    raise(&paths, ask.clone()).unwrap();
    for signal in [
        end(true, false),
        end(false, false),
        LifecycleSignal::TurnInterrupted { turn_id: None },
        obs.signal,
    ] {
        observe(
            &paths,
            &ask.kind,
            &observation(signal),
            Timestamp::from_second(1).unwrap(),
            false,
            Some("Old reply?"),
        )
        .unwrap();
        assert_eq!(read(&paths).asks[0], ask);
    }
    observe(
        &paths,
        &ask.kind,
        &observation(end(false, false)),
        Timestamp::from_second(6).unwrap(),
        false,
        None,
    )
    .unwrap();
    let record = read(&paths);
    assert!(record.is_open(&record.asks[0]));
    assert!(
        record.asks[0].value.text.is_none(),
        "an uncaptured reply opens without stale text"
    );
}
