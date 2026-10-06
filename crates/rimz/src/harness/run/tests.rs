use super::*;
use std::path::Path;
use std::time::Duration;

use crate::agents::AgentState;
use crate::agents::LifecycleSignal;
use crate::ids::{AgentKind, MuxName, WorkspaceId};
use crate::pane::PaneRef;
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, StatePaths, RunRecord) {
    setup_for("claude")
}

fn setup_for(kind: &str) -> (tempfile::TempDir, StatePaths, RunRecord) {
    let dir = tempdir().unwrap();
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    paths.ensure_dirs().unwrap();
    let record = RunRecord::new(
        workspace_id,
        AgentKind::new_unchecked(kind),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    create(&paths, &record).unwrap();
    (dir, paths, record)
}

#[test]
fn delivery_stamps_subagent_answer_from_confirmed_conversation() {
    use crate::store::message::{DeliveryGate, HarnessNotice, MessageRecord, MessageSender};
    use crate::store::writer::DeliveryAckMatch;

    for selection in [
        DeliveryAckMatch::PromptCorrelated,
        DeliveryAckMatch::OldestSentBatch,
    ] {
        let (_dir, paths, mut record) = setup();
        let card = AgentState::stub("claude", "child", AgentStatus::Idle);
        record.subagent = true;
        record.agent_id = Some(card.agent_id.clone());
        record.status = RunStatus::Running;
        record.follow_up = Some(FollowUpTurn {
            started_at: Timestamp::now(),
            prompt: None,
        });
        create(&paths, &record).unwrap();
        let message = |text: &str| {
            MessageRecord::new(
                record.workspace_id.clone(),
                &card,
                text.into(),
                DeliveryGate::Done,
            )
        };
        let first = message("follow-up task\nsecond line");
        let second = message("additional instruction").with_sender(MessageSender::Agent {
            kind: card.kind.clone(),
            agent_id: Some("parent".into()),
            name: None,
            profile: None,
            role: None,
            channel: None,
        });
        let notice = message("notice").with_sender(MessageSender::Harness {
            notice: HarnessNotice::Stage,
        });
        let adapter = crate::agents::definition_by_kind("claude").unwrap();
        let _guard = WorkspaceLock::acquire(&paths.workspace_lock).unwrap();
        record_run_delivery(
            &paths,
            &card,
            adapter,
            std::slice::from_ref(&notice),
            selection,
            Path::new("/repo"),
            Some(&record.run_id),
        )
        .unwrap();
        let unstamped = load(&paths, &record.run_id).unwrap();
        assert!(unstamped.opened_by.is_empty());
        assert!(unstamped.follow_up.unwrap().prompt.is_none());
        record_run_delivery(
            &paths,
            &card,
            adapter,
            &[first.clone(), notice, second.clone()],
            selection,
            Path::new("/repo"),
            Some(&record.run_id),
        )
        .unwrap();
        let stamped = load(&paths, &record.run_id).unwrap();
        assert_eq!(
            stamped.opened_by,
            vec![first.message_id.clone(), second.message_id]
        );
        assert_eq!(
            stamped.follow_up.unwrap().prompt.as_deref(),
            Some("follow-up task\nsecond line\n\nadditional instruction")
        );
        let later = message("later parked delivery");
        record_run_delivery(
            &paths,
            &card,
            adapter,
            &[first, later.clone()],
            selection,
            Path::new("/repo"),
            Some(&record.run_id),
        )
        .unwrap();
        let appended = load(&paths, &record.run_id).unwrap();
        assert_eq!(appended.opened_by.len(), 3);
        assert_eq!(appended.opened_by.last(), Some(&later.message_id));
        assert_eq!(
            appended.follow_up.unwrap().prompt.as_deref(),
            Some("follow-up task\nsecond line\n\nadditional instruction")
        );
        for excluded in [
            "terminal",
            "peer",
            "team",
            "other-card",
            "unbound",
            "not-subagent",
        ] {
            let mut excluded_record = record.clone();
            match excluded {
                "terminal" => excluded_record.status = RunStatus::Completed,
                "peer" => {
                    excluded_record.peer = Some(crate::store::run::PeerRun {
                        launch_id: "peer-launch".into(),
                        opened_by: Vec::new(),
                    })
                }
                "team" => {
                    excluded_record.team = Some(crate::store::run::TeamRun {
                        launch_id: "team-launch".into(),
                        instance: "forge#x".into(),
                    })
                }
                "other-card" => excluded_record.agent_id = Some("other".into()),
                "unbound" => excluded_record.agent_id = None,
                "not-subagent" => excluded_record.subagent = false,
                _ => unreachable!(),
            }
            crate::store::run::write(&paths.runs_dir, &excluded_record).unwrap();
            record_run_delivery(
                &paths,
                &card,
                adapter,
                std::slice::from_ref(&later),
                selection,
                Path::new("/repo"),
                Some(&record.run_id),
            )
            .unwrap();
            assert_eq!(
                load(&paths, &record.run_id).unwrap(),
                excluded_record,
                "{excluded}"
            );
        }
        record.follow_up = None;
        crate::store::run::write(&paths.runs_dir, &record).unwrap();
        record_run_delivery(
            &paths,
            &card,
            adapter,
            std::slice::from_ref(&later),
            selection,
            Path::new("/repo"),
            Some(&record.run_id),
        )
        .unwrap();
        let launch = load(&paths, &record.run_id).unwrap();
        assert_eq!(launch.opened_by, vec![later.message_id]);
        assert!(launch.follow_up.is_none());
    }
}

#[test]
fn peer_responses_preserve_each_turn() {
    let (_dir, paths, mut first) = setup();
    first.agent_name = Some("peer".into());
    first.reader = Some("launcher".into());
    first.last_message = Some("first answer".into());
    let mut value = serde_json::to_value(&first).unwrap();
    value["peer"] = serde_json::json!({"launch_id": "peer-launch"});
    let first: RunRecord = serde_json::from_value(value).unwrap();
    let mut second = first.clone();
    second.run_id = RunId::new();
    second.last_message = Some("second answer".into());
    for record in [&first, &second] {
        create(&paths, record).unwrap();
        cancel(&paths, &record.run_id).unwrap();
        let path = paths
            .out_reader_dir(Some("launcher"))
            .join(format!("peer.{}.output", record.run_id));
        assert!(
            path.exists(),
            "terminal peer run publishes its own response"
        );
        assert_eq!(publish_response(&paths, record).unwrap(), Some(path));
    }
    for record in [&first, &second] {
        let path = paths
            .out_reader_dir(Some("launcher"))
            .join(format!("peer.{}.output", record.run_id));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            format!("{}\n", record.last_message.as_ref().unwrap())
        );
    }
    assert!(
        !paths
            .out_reader_dir(Some("child"))
            .join("peer.output")
            .exists()
    );
    #[cfg(unix)]
    for dir in [
        paths.out_dir.clone(),
        paths.out_reader_dir(Some("launcher")),
    ] {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{} is private", dir.display());
    }
}

#[test]
fn responses_follow_each_terminal_transition() {
    for subagent in [true, false] {
        for ending in ["lifecycle", "cancel", "timeout"] {
            let (_dir, paths, mut record) = setup();
            record.subagent = subagent;
            record.agent_name = Some("child".into());
            create(&paths, &record).unwrap();
            record_assistant_message(
                &paths,
                &record.run_id,
                "claude",
                &"child".into(),
                "answer".into(),
            )
            .unwrap();
            match ending {
                "cancel" => {
                    cancel(&paths, &record.run_id).unwrap();
                }
                "timeout" => {
                    timeout(&paths, &record.run_id).unwrap();
                }
                _ => {
                    let observation = AgentLifecycleObservation::new(
                        Some("child".into()),
                        LifecycleSignal::TurnEnded {
                            errored: false,
                            parked_on_background: false,
                            turn_id: None,
                        },
                    );
                    record_lifecycle(
                        &paths,
                        &record.run_id,
                        "claude",
                        &observation,
                        Some("answer".into()),
                        || None,
                    )
                    .unwrap();
                }
            }
            let path = paths.out_reader_dir(Some("child")).join("child.output");
            assert_eq!(path.exists(), subagent, "{ending}");
            if subagent {
                assert_eq!(std::fs::read_to_string(path).unwrap(), "answer\n");
            }
        }
    }
}

#[test]
fn response_lands_before_the_terminal_record() {
    let (_dir, paths, mut record) = setup();
    record.subagent = true;
    record.agent_name = Some("child".into());
    record.reader = Some("parent".into());
    record.last_message = Some("answer".into());
    create(&paths, &record).unwrap();
    let record_path = paths.runs_dir.join(format!("{}.json", record.run_id));

    let result = update_record(&paths, &record.run_id, |record, now| {
        // A non-empty directory where the record renames to fails the write, even as root.
        std::fs::remove_file(&record_path).unwrap();
        std::fs::create_dir_all(record_path.join("squat")).unwrap();
        record.mark_terminal(RunStatus::Canceled, now);
        Ok(RecordMutation::Write(()))
    });

    assert!(result.is_err(), "the record write must fail");
    assert_eq!(
        std::fs::read_to_string(paths.out_reader_dir(Some("parent")).join("child.output")).unwrap(),
        "answer\n"
    );
}

#[test]
fn reopened_response_preserves_previous_answers() {
    for message in [Some("second answer"), Some(""), None] {
        let (_dir, paths, mut record) = setup();
        record.subagent = true;
        record.agent_name = Some("child".into());
        create(&paths, &record).unwrap();
        let mut observation = AgentLifecycleObservation::new(
            Some("child".into()),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
        );
        record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &observation,
            Some("first answer".into()),
            || None,
        )
        .unwrap();
        let path = paths.out_reader_dir(Some("child")).join("child.output");
        assert!(
            path.exists(),
            "terminal lifecycle must publish before returning"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first answer\n");
        observation.signal = LifecycleSignal::TurnStarted { turn_id: None };
        record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
            None
        })
        .unwrap();
        observation.signal = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        };
        record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &observation,
            message.map(str::to_owned),
            || None,
        )
        .unwrap();
        for ordinal in [2, 3] {
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "first answer\n");
            let second = paths.out_reader_dir(Some("child")).join("child.2.output");
            if message.is_some_and(|message| !message.is_empty()) {
                assert_eq!(std::fs::read_to_string(second).unwrap(), "second answer\n");
            } else {
                assert!(!second.exists());
            }
            if ordinal == 3 {
                assert_eq!(
                    std::fs::read_to_string(
                        paths.out_reader_dir(Some("child")).join("child.3.output")
                    )
                    .unwrap(),
                    "third answer\n"
                );
                break;
            }
            observation.signal = LifecycleSignal::TurnStarted { turn_id: None };
            record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
                None
            })
            .unwrap();
            observation.signal = LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            };
            record_lifecycle(
                &paths,
                &record.run_id,
                "claude",
                &observation,
                Some("third answer".into()),
                || None,
            )
            .unwrap();
        }
    }
}

#[test]
fn terminal_subagent_reopens_for_its_next_turn() {
    for (subagent, joined) in [(false, true), (true, true), (true, false)] {
        let (_dir, paths, mut record) = setup();
        record.subagent = subagent;
        record.agent_id = Some("child".into());
        record.status = RunStatus::Completed;
        record.completed_at = Some(record.started_at);
        record.joined_at = joined.then_some(record.started_at);
        record.report_message_id = Some(crate::ids::MessageId::new());
        record.last_message = Some("first answer".into());
        record.failure_tail = Some("first failure".into());
        record.deadline_at = Some(record.started_at + std::time::Duration::from_secs(30));
        create(&paths, &record).unwrap();
        let mut observation = AgentLifecycleObservation::new(
            Some("stranger".into()),
            LifecycleSignal::TurnStarted { turn_id: None },
        );
        record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
            None
        })
        .unwrap();
        assert_eq!(load(&paths, &record.run_id).unwrap(), record);
        observation.agent_id = Some("child".into());
        let before = Timestamp::now();
        record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
            None
        })
        .unwrap();
        let reopened = load(&paths, &record.run_id).unwrap();
        assert_eq!(reopened.follow_ups, u32::from(subagent));
        if !subagent {
            assert_eq!(reopened, record);
            continue;
        }
        assert_eq!(reopened.status, RunStatus::Running);
        assert_eq!(reopened.completed_at, None);
        assert_eq!(reopened.parked_at, None);
        assert_eq!(reopened.joined_at, None);
        assert_eq!(reopened.report_message_id, None);
        assert_eq!(reopened.last_message, None);
        assert_eq!(reopened.failure_tail, None);
        let value = serde_json::to_value(&reopened).unwrap();
        assert_eq!(
            value["follow_up"]["started_at"],
            serde_json::to_value(reopened.updated_at).unwrap()
        );
        assert!(value["follow_up"]["prompt"].is_null());
        if joined {
            assert!(value.get("earlier_answers").is_none());
        } else {
            let answers = value["earlier_answers"]
                .as_array()
                .expect("unjoined answer carried");
            assert_eq!(answers.len(), 1);
            assert_eq!(answers[0]["ordinal"], 1);
            assert_eq!(answers[0]["failure_tail"], "first failure");
            assert_eq!(
                answers[0]["report_message_id"],
                serde_json::to_value(&record.report_message_id).unwrap()
            );
            assert_eq!(
                answers[0]["started_at"],
                serde_json::to_value(record.started_at).unwrap()
            );
            assert_eq!(
                answers[0]["completed_at"],
                serde_json::to_value(record.completed_at).unwrap()
            );
            assert!(answers[0]["prompt"].is_null());
            assert!(answers[0]["joined_at"].is_null());
            assert!(answers[0].get("last_message").is_none());
        }
        assert!(reopened.deadline_at.unwrap() >= before + std::time::Duration::from_secs(30));
        observation.signal = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        };
        assert!(
            record_lifecycle(
                &paths,
                &record.run_id,
                "claude",
                &observation,
                Some("second answer".into()),
                || None
            )
            .unwrap()
            .is_some()
        );
        observation.signal = LifecycleSignal::TurnStarted { turn_id: None };
        record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
            None
        })
        .unwrap();
        let third = load(&paths, &record.run_id).unwrap();
        assert_eq!(
            third.deadline_at.unwrap(),
            third.updated_at + std::time::Duration::from_secs(30)
        );
    }
}

#[test]
fn durable_deadline_defaults_for_old_records_and_times_out_once_due() {
    let (_dir, paths, record) = setup();
    let mut old_json = serde_json::to_value(&record).expect("serialize run");
    old_json
        .as_object_mut()
        .expect("run object")
        .remove("deadline_at");
    let old: RunRecord = serde_json::from_value(old_json).expect("deserialize old run");
    assert_eq!(old.deadline_at, None);

    let deadline = record.started_at + std::time::Duration::from_secs(30);
    let mut timed = record.clone();
    timed.deadline_at = Some(deadline);
    create(&paths, &timed).expect("write deadline");

    let (_, wrote) = timeout_if_due(
        &paths,
        &record.run_id,
        deadline - std::time::Duration::from_secs(1),
        None,
    )
    .expect("check early deadline");
    assert!(!wrote);

    let (settled, wrote) =
        timeout_if_due(&paths, &record.run_id, deadline, None).expect("settle due deadline");
    assert!(wrote);
    assert_eq!(settled.status, RunStatus::TimedOut);
    assert_eq!(settled.completed_at, Some(deadline));

    let (_, wrote) =
        timeout_if_due(&paths, &record.run_id, deadline, None).expect("repeat due deadline");
    assert!(!wrote);
}

#[test]
fn stop_is_claimed_once_and_reopen_rearms_the_original_timeout() {
    let (_dir, paths, mut record) = setup();
    let now: Timestamp = "2026-01-01T01:00:00Z".parse().unwrap();
    record.subagent = true;
    record.timeout = Some(std::time::Duration::from_secs(1800));
    record.grace = Some(std::time::Duration::from_secs(180));
    record.started_at = now - std::time::Duration::from_secs(7200);
    record.deadline_at = Some(now - std::time::Duration::from_secs(1));
    create(&paths, &record).unwrap();
    assert_eq!(
        claim_rung(&paths, &record.run_id, now, |_| false).unwrap(),
        None
    );
    assert_eq!(
        load(&paths, &record.run_id).unwrap().deadline_notice_at,
        None
    );
    assert_eq!(
        claim_rung(&paths, &record.run_id, now, |_| true).unwrap(),
        Some(super::super::deadline::Rung::Stop {
            at: record.deadline_at.unwrap()
        })
    );
    for _ in 0..3 {
        assert_eq!(
            claim_rung(&paths, &record.run_id, now, |_| panic!("already claimed")).unwrap(),
            None
        );
    }
    let mut record = load(&paths, &record.run_id).unwrap();
    assert_eq!(record.deadline_notice_at, record.deadline_at);
    let observation = AgentLifecycleObservation::new(
        Some("child".into()),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    for elapsed in [0, 3600] {
        record.status = RunStatus::Completed;
        let reopen_at = now + std::time::Duration::from_secs(elapsed);
        fold_lifecycle(&mut record, "claude", &observation, None, reopen_at, None);
        assert_eq!(
            record.deadline_at,
            Some(reopen_at + std::time::Duration::from_secs(1800))
        );
        assert_eq!(record.deadline_notice_at, None);
    }
}

#[test]
fn kill_harvest_is_write_once_and_published_only_after_grace() {
    for existing in [None, Some("existing answer")] {
        let (_dir, paths, mut record) = setup();
        record.subagent = true;
        record.agent_name = Some("child".into());
        record.status = RunStatus::Running;
        record.last_message = existing.map(str::to_owned);
        record.deadline_at = Some(record.started_at);
        record.grace = Some(std::time::Duration::from_secs(180));
        create(&paths, &record).unwrap();
        let (_, wrote) = timeout_if_due(
            &paths,
            &record.run_id,
            record.started_at,
            Some("partial answer".into()),
        )
        .unwrap();
        assert!(!wrote);
        assert_eq!(load(&paths, &record.run_id).unwrap(), record);
        let (settled, wrote) = timeout_if_due(
            &paths,
            &record.run_id,
            record.started_at + std::time::Duration::from_secs(180),
            Some("partial answer".into()),
        )
        .unwrap();
        assert!(wrote);
        assert_eq!(settled.status, RunStatus::TimedOut);
        let expected = existing.unwrap_or("partial answer");
        assert_eq!(settled.last_message.as_deref(), Some(expected));
        assert_eq!(
            std::fs::read_to_string(paths.out_reader_dir(Some("child")).join("child.output"))
                .unwrap(),
            format!("{expected}\n")
        );
    }
}

#[test]
fn stranded_park_requires_two_checks_and_preserves_racing_turns() {
    let (dir, paths, mut record) = setup();
    let runtime =
        crate::RuntimePaths::under(record.workspace_id.clone(), &dir.path().join("rt")).unwrap();
    let store = Store::open(paths, runtime).unwrap();
    let socket = std::os::unix::net::UnixDatagram::bind(crate::store::run::run_socket_path(
        store.runtime_paths(),
        &record.run_id,
    ))
    .unwrap();
    socket.set_nonblocking(true).unwrap();
    let mut frame = [0; 1024];
    record.status = RunStatus::Running;
    record.agent_id = Some("session".into());
    create(store.paths(), &record).unwrap();
    assert_eq!(
        settle_stranded_park(&store, &record, None).unwrap(),
        ParkCheck::Live
    );
    assert_eq!(load(store.paths(), &record.run_id).unwrap(), record);

    let at = Timestamp::now();
    record.parked_at = Some(at);
    create(store.paths(), &record).unwrap();
    assert_eq!(
        settle_stranded_park(&store, &record, None).unwrap(),
        ParkCheck::Stranded(at)
    );
    assert_eq!(load(store.paths(), &record.run_id).unwrap(), record);

    for parked_at in [None, Some(at + std::time::Duration::from_secs(1))] {
        let mut revived = record.clone();
        revived.parked_at = parked_at;
        create(store.paths(), &revived).unwrap();
        assert_eq!(
            settle_stranded_park(&store, &record, Some(at)).unwrap(),
            ParkCheck::Live
        );
        assert_eq!(load(store.paths(), &record.run_id).unwrap(), revived);
    }
    assert_eq!(
        socket.recv(&mut frame).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );

    for tail in [None, Some("earlier evidence".to_owned())] {
        record.failure_tail = tail.clone();
        create(store.paths(), &record).unwrap();
        assert_eq!(
            settle_stranded_park(&store, &record, Some(at)).unwrap(),
            ParkCheck::Settled
        );
        let failed = load(store.paths(), &record.run_id).unwrap();
        assert_eq!(failed.status, RunStatus::Failed);
        assert_eq!(failed.parked_at, None);
        let count = socket.recv(&mut frame).unwrap();
        let crate::store::run::WakeupFrame::RunCompleted {
            workspace_id,
            run_id,
            status,
        } = serde_json::from_slice(&frame[..count]).unwrap();
        assert_eq!(workspace_id, record.workspace_id);
        assert_eq!(run_id, record.run_id);
        assert_eq!(status, RunStatus::Failed);
        assert_eq!(
            failed.failure_tail.as_deref(),
            Some(tail.as_deref().unwrap_or(STRANDED_PARK_REASON))
        );
        assert_eq!(
            settle_stranded_park(&store, &failed, Some(at)).unwrap(),
            ParkCheck::Live
        );
        assert_eq!(
            settle_stranded_park(&store, &record, Some(at)).unwrap(),
            ParkCheck::Live
        );
    }

    // The arm that stops the wrapper failing a run that is legitimately
    // waiting: with a wake owed, no number of checks settles the park.
    record.status = RunStatus::Running;
    record.failure_tail = None;
    create(store.paths(), &record).unwrap();
    let mut registered =
        AgentLifecycleObservation::new(record.agent_id.clone(), LifecycleSignal::Registered);
    registered.pane_id = Some(crate::ids::PaneId::from_parts(MuxName::Tmux, "%1"));
    store
        .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
            session_name: "park-test",
            agent_kind: record.kind.clone(),
            event_name: "test",
            observation: &registered,
            spawned_subagents: &[],
        })
        .unwrap();
    let agent = store
        .snapshot_cached()
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| Some(&agent.agent_id) == record.agent_id.as_ref())
        .expect("the parked run's card");
    let wake = crate::store::message::MessageRecord::new(
        record.workspace_id.clone(),
        &agent,
        "wake".to_owned(),
        crate::store::message::DeliveryGate::Done,
    )
    .with_sender(crate::store::message::MessageSender::Harness {
        notice: crate::store::message::HarnessNotice::Wait,
    });
    store.queue_message(&wake, "park-test").unwrap();
    for previous in [None, Some(at)] {
        assert_eq!(
            settle_stranded_park(&store, &record, previous).unwrap(),
            ParkCheck::Live
        );
    }
    assert_eq!(load(store.paths(), &record.run_id).unwrap(), record);
    assert_eq!(
        socket.recv(&mut frame).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

/// A park whose turn end carried no transcript path is woken by a turn start
/// that does: clearing the park cannot swallow the late-path fold the
/// non-Claude adapters depend on.
#[test]
fn parked_wake_turn_folds_its_first_transcript_path() {
    let (_dir, paths, record) = setup();
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    assert!(
        record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
            Some(OwedWake::Wait)
        })
        .unwrap()
        .is_none()
    );
    let parked = load(&paths, &record.run_id).unwrap();
    assert!(parked.parked_at.is_some());
    assert_eq!(parked.transcript_path, None);

    observation.signal = LifecycleSignal::TurnStarted { turn_id: None };
    observation.transcript_path = Some("/tmp/late.jsonl".to_owned());
    record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
        panic!("turn start must not read owed")
    })
    .unwrap();
    let woken = load(&paths, &record.run_id).unwrap();
    assert_eq!(woken.parked_at, None);
    assert_eq!(woken.status, RunStatus::Running);
    assert_eq!(woken.transcript_path, observation.transcript_path);
}

#[test]
fn fold_lifecycle_maps_each_disposition_to_its_run_status() {
    for (signal, expected) in [
        (
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
            RunStatus::Completed,
        ),
        (
            LifecycleSignal::TurnEnded {
                errored: true,
                parked_on_background: false,
                turn_id: None,
            },
            RunStatus::Failed,
        ),
        (
            LifecycleSignal::TurnInterrupted { turn_id: None },
            RunStatus::Canceled,
        ),
        (LifecycleSignal::Ended, RunStatus::Failed),
    ] {
        for owed in [None, Some(OwedWake::Wait)] {
            let (_dir, paths, record) = setup();
            let observation = AgentLifecycleObservation::new(
                Some(AgentSessionId::from("sess-1")),
                signal.clone(),
            );
            let mut consulted = false;
            let completed =
                record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
                    let _guard = WorkspaceLock::try_acquire(&paths.workspace_lock)
                        .unwrap()
                        .expect("owed reads must precede the workspace lock");
                    consulted = true;
                    owed
                })
                .unwrap();
            assert_eq!(consulted, expected == RunStatus::Completed);
            let parked = expected == RunStatus::Completed && owed.is_some();
            assert_eq!(completed.is_none(), parked);
            let persisted = load(&paths, &record.run_id).unwrap();
            assert_eq!(
                persisted.status,
                if parked { RunStatus::Running } else { expected },
                "{:?}",
                observation.signal
            );
            assert_eq!(persisted.parked_at.is_some(), parked);
            assert_eq!(persisted.completed_at.is_none(), parked);
        }
    }
}

#[test]
fn parked_run_resumes_and_completes_once() {
    let (_dir, paths, record) = setup();
    let mut ended = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    ended.transcript_path = Some("/tmp/parked.jsonl".to_owned());
    assert!(
        record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &ended,
            Some("waiting".to_owned()),
            || Some(OwedWake::Wait)
        )
        .unwrap()
        .is_none()
    );
    let parked = load(&paths, &record.run_id).unwrap();
    assert_eq!(parked.status, RunStatus::Running);
    assert!(parked.parked_at.is_some());
    assert_eq!(parked.completed_at, None);
    assert_eq!(parked.transcript_path, ended.transcript_path);
    assert_eq!(parked.last_message.as_deref(), Some("waiting"));

    let mut observation = ended.clone();
    observation.signal = LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
        name: None,
        native_key: None,
        turn_id: None,
    };
    record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
        panic!("non-completion must not read owed")
    })
    .unwrap();
    assert_eq!(
        load(&paths, &record.run_id).unwrap().parked_at,
        parked.parked_at
    );
    observation.signal = LifecycleSignal::TurnStarted { turn_id: None };
    record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
        panic!("turn start must not read owed")
    })
    .unwrap();
    let running = load(&paths, &record.run_id).unwrap();
    assert_eq!(running.parked_at, None);
    assert_eq!(running.follow_ups, record.follow_ups);
    assert_eq!(running.status, RunStatus::Running);

    let completed = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &ended,
        Some("done".to_owned()),
        || None,
    )
    .unwrap()
    .expect("terminal update");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(completed.parked_at, None);
    assert_eq!(completed.last_message.as_deref(), Some("done"));
    assert!(
        record_lifecycle(&paths, &record.run_id, "claude", &ended, None, || None)
            .unwrap()
            .is_none()
    );
    assert_eq!(load(&paths, &record.run_id).unwrap(), completed);
}

#[test]
fn terminal_writes_clear_parked_runs() {
    for status in [RunStatus::Canceled, RunStatus::TimedOut, RunStatus::Failed] {
        let (_dir, paths, record) = setup();
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from("sess-1")),
            LifecycleSignal::TurnEnded {
                errored: false,
                parked_on_background: false,
                turn_id: None,
            },
        );
        record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
            Some(OwedWake::Subagents)
        })
        .unwrap();
        assert!(load(&paths, &record.run_id).unwrap().parked_at.is_some());
        match status {
            RunStatus::Canceled => {
                cancel(&paths, &record.run_id).unwrap();
            }
            RunStatus::TimedOut => {
                timeout(&paths, &record.run_id).unwrap();
            }
            _ => {
                observation.signal = LifecycleSignal::Ended;
                record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
                    panic!("failure must not read owed")
                })
                .unwrap()
                .expect("terminal update");
            }
        }
        let terminal = load(&paths, &record.run_id).unwrap();
        assert_eq!(terminal.status, status);
        assert_eq!(terminal.parked_at, None);
    }
}

#[test]
fn lifecycle_completion_writes_terminal_record_once() {
    let (_dir, paths, record) = setup();
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let completed = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &observation,
        Some("done".to_owned()),
        || None,
    )
    .unwrap()
    .expect("terminal update");
    assert_eq!(completed.status, RunStatus::Completed);
    assert_eq!(completed.last_message.as_deref(), Some("done"));
    assert_eq!(completed.agent_id.as_deref(), Some("sess-1"));

    let repeated = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &observation,
        Some("done".to_owned()),
        || None,
    )
    .unwrap();
    assert!(repeated.is_none());
}

#[test]
fn subagent_observation_does_not_complete_parent_run() {
    let (_dir, paths, record) = setup();
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("child-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    observation.parent_agent_id = Some(AgentSessionId::from("sess-parent"));

    let update = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &observation,
        Some("child done".to_owned()),
        || panic!("subagent observation must not read owed"),
    )
    .unwrap();
    assert!(update.is_none());
    let after = load(&paths, &record.run_id).unwrap();
    assert_eq!(after.status, RunStatus::Pending);
    assert_eq!(after.last_message, None);
}

#[test]
fn same_kind_child_process_does_not_complete_bound_parent_run() {
    let (_dir, paths, record) = setup();
    let parent = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-parent")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    record_lifecycle(&paths, &record.run_id, "claude", &parent, None, || None).unwrap();

    let child = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-child")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let update = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &child,
        Some("child done".to_owned()),
        || None,
    )
    .unwrap();

    assert!(update.is_none());
    let after = load(&paths, &record.run_id).unwrap();
    assert_eq!(after.status, RunStatus::Running);
    assert_eq!(after.agent_id.as_deref(), Some("sess-parent"));
    assert_eq!(after.last_message, None);
}

#[test]
fn terminal_transitions_are_once_only_and_map_exit_codes() {
    let (_dir, paths, record) = setup();
    let timed_out = timeout(&paths, &record.run_id).unwrap();
    assert_eq!(timed_out.status, RunStatus::TimedOut);
    assert!(timed_out.completed_at.is_some());
    assert_eq!(timed_out.status.exit_code(), 124);

    assert!(
        fail_if_nonterminal(&paths, &record.run_id, "late")
            .unwrap()
            .is_none()
    );
    let still_timed_out = load(&paths, &record.run_id).unwrap();
    assert_eq!(still_timed_out.status, RunStatus::TimedOut);
    assert_eq!(still_timed_out.failure_tail, None);

    let (_dir, paths, record) = setup();
    let (canceled, wrote) = cancel(&paths, &record.run_id).unwrap();
    assert!(wrote);
    assert_eq!(canceled.status, RunStatus::Canceled);
    assert!(canceled.completed_at.is_some());
    assert_eq!(canceled.status.exit_code(), 130);

    let (still_canceled, wrote) = cancel(&paths, &record.run_id).unwrap();
    assert!(!wrote);
    assert_eq!(still_canceled.status, RunStatus::Canceled);

    let (_dir, paths, record) = setup();
    let (budgeted, wrote) = budget_exceeded(&paths, &record.run_id, Some(5.25)).unwrap();
    assert!(wrote);
    assert_eq!(budgeted.status, RunStatus::BudgetExceeded);
    assert_eq!(budgeted.cost_usd, Some(5.25));
    assert_eq!(budgeted.status.exit_code(), 125);

    assert_eq!(RunStatus::Completed.exit_code(), 0);
    assert_eq!(RunStatus::Failed.exit_code(), 1);
    assert_eq!(RunStatus::VerifyFailed.exit_code(), 123);
    assert_eq!(RunStatus::BudgetExceeded.exit_code(), 125);
    assert!(RunStatus::Failed.is_retryable());
    assert!(RunStatus::VerifyFailed.is_terminal());
    assert!(!RunStatus::VerifyFailed.is_retryable());
    assert!(!RunStatus::Completed.is_retryable());
    assert!(!RunStatus::TimedOut.is_retryable());
    assert!(!RunStatus::BudgetExceeded.is_retryable());
    assert!(!RunStatus::Canceled.is_retryable());
    assert!(RunStatus::BudgetExceeded.is_terminal());
    assert!(RunStatus::Canceled.is_terminal());
}

#[test]
fn verify_transitions_reopen_completed_runs_and_finish_once() {
    let (_dir, paths, record) = setup();
    let completed = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    record_lifecycle(&paths, &record.run_id, "claude", &completed, None, || None)
        .unwrap()
        .expect("completed run");
    let first = RunVerify {
        cmd: "cargo xtask test run".to_owned(),
        attempts: 1,
        passed: false,
        code: Some(1),
        timed_out: false,
        output: "red".to_owned(),
    };

    let reopened = reopen_for_verify(&paths, &record.run_id, first.clone()).unwrap();
    assert_eq!(reopened.follow_ups, record.follow_ups);
    assert_eq!(reopened.status, RunStatus::Running);
    assert_eq!(reopened.completed_at, None);
    assert_eq!(reopened.verify.as_ref(), Some(&first));
    assert!(reopen_for_verify(&paths, &record.run_id, first.clone()).is_err());

    record_lifecycle(&paths, &record.run_id, "claude", &completed, None, || None)
        .unwrap()
        .expect("second completed turn");
    let second = RunVerify {
        attempts: 2,
        output: "still red".to_owned(),
        ..first
    };
    let failed = verify_failed(&paths, &record.run_id, second.clone()).unwrap();
    assert_eq!(failed.status, RunStatus::VerifyFailed);
    assert_eq!(failed.verify.as_ref(), Some(&second));
    let updated_at = failed.updated_at;

    let repeated = verify_failed(&paths, &record.run_id, second).unwrap();
    assert_eq!(repeated.updated_at, updated_at);
}

fn completed_for_verify() -> (tempfile::TempDir, StatePaths, RunRecord) {
    let (dir, paths, record) = setup();
    let completed = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let record = record_lifecycle(&paths, &record.run_id, "claude", &completed, None, || None)
        .unwrap()
        .expect("completed run");
    assert_eq!(record.status, RunStatus::Completed);
    (dir, paths, record)
}

fn red_verify(attempts: u32) -> RunVerify {
    RunVerify {
        cmd: "cargo xtask test run".to_owned(),
        attempts,
        passed: false,
        code: Some(1),
        timed_out: false,
        output: "red".to_owned(),
    }
}

#[test]
fn settle_verify_cancels_after_storing_the_verify() {
    let (_dir, paths, record) = completed_for_verify();
    let verify = RunVerify {
        passed: true,
        ..red_verify(3)
    };
    let VerifyStep::Settled(settled) =
        settle_verify(&paths, &record.run_id, verify.clone(), true, 3).unwrap()
    else {
        panic!("a requested cancellation settles the run");
    };
    assert_eq!(settled.status, RunStatus::Canceled);
    assert_eq!(settled.verify.as_ref(), Some(&verify));
}

#[test]
fn settle_verify_keeps_a_passed_run_completed() {
    let (_dir, paths, record) = completed_for_verify();
    let verify = RunVerify {
        passed: true,
        code: Some(0),
        ..red_verify(3)
    };
    let VerifyStep::Settled(settled) =
        settle_verify(&paths, &record.run_id, verify.clone(), false, 3).unwrap()
    else {
        panic!("a pass settles the run");
    };
    assert_eq!(settled.status, RunStatus::Completed);
    assert_eq!(settled.verify.as_ref(), Some(&verify));
}

#[test]
fn settle_verify_fails_a_red_run_at_the_attempt_cap() {
    let (_dir, paths, record) = completed_for_verify();
    let VerifyStep::Settled(settled) =
        settle_verify(&paths, &record.run_id, red_verify(3), false, 3).unwrap()
    else {
        panic!("the attempt cap settles the run");
    };
    assert_eq!(settled.status, RunStatus::VerifyFailed);
    assert_eq!(settled.verify.as_ref(), Some(&red_verify(3)));
}

#[test]
fn settle_verify_reopens_a_red_run_under_the_cap() {
    let (_dir, paths, record) = completed_for_verify();
    let VerifyStep::Reprompt(reopened) =
        settle_verify(&paths, &record.run_id, red_verify(1), false, 3).unwrap()
    else {
        panic!("a red verify under the cap re-prompts");
    };
    assert_eq!(reopened.status, RunStatus::Running);
    assert_eq!(reopened.completed_at, None);
    assert_eq!(reopened.verify.as_ref(), Some(&red_verify(1)));
}

#[test]
fn record_spend_persists_tokens_and_ignores_non_finite_cost() {
    let (_dir, paths, record) = setup();

    let updated = record_spend(
        &paths,
        &record.run_id,
        Some(f64::NAN),
        Some(1_200),
        Some(340),
    )
    .unwrap();

    assert_eq!(updated.cost_usd, None);
    assert_eq!(updated.input_tokens, Some(1_200));
    assert_eq!(updated.output_tokens, Some(340));
    let unchanged = record_spend(&paths, &record.run_id, None, None, None).unwrap();
    assert_eq!(unchanged, updated);

    for invalid in [f64::INFINITY, -1.0] {
        let unchanged = record_spend(&paths, &record.run_id, Some(invalid), None, None).unwrap();
        assert_eq!(unchanged, updated);
    }
}

#[test]
fn lifecycle_and_assistant_messages_require_matching_live_root_run() {
    let (_dir, paths, record) = setup();
    let started = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );

    assert!(
        record_lifecycle(&paths, &record.run_id, "codex", &started, None, || None)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        load(&paths, &record.run_id).unwrap().status,
        RunStatus::Pending
    );

    record_lifecycle(&paths, &record.run_id, "claude", &started, None, || None).unwrap();
    record_assistant_message(
        &paths,
        &record.run_id,
        "claude",
        &AgentSessionId::from("sess-1"),
        "matching".to_owned(),
    )
    .unwrap();
    for (kind, session) in [("codex", "sess-1"), ("claude", "sess-2")] {
        record_assistant_message(
            &paths,
            &record.run_id,
            kind,
            &AgentSessionId::from(session),
            "ignored".to_owned(),
        )
        .unwrap();
    }
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .last_message
            .as_deref(),
        Some("matching")
    );
}

#[test]
fn record_lifecycle_folds_transcript_path_on_run_writes() {
    let (_dir, paths, record) = setup();

    let mut started = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    started.transcript_path = Some("/tmp/first.jsonl".to_owned());
    assert!(
        record_lifecycle(&paths, &record.run_id, "claude", &started, None, || None)
            .unwrap()
            .is_none()
    );
    let running = load(&paths, &record.run_id).unwrap();
    assert_eq!(running.status, RunStatus::Running);
    assert_eq!(running.transcript_path.as_deref(), Some("/tmp/first.jsonl"));

    let mut tool = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
            name: None,
            native_key: None,
            turn_id: None,
        },
    );
    tool.transcript_path = Some("/tmp/second.jsonl".to_owned());
    record_lifecycle(&paths, &record.run_id, "claude", &tool, None, || None).unwrap();
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .transcript_path
            .as_deref(),
        Some("/tmp/first.jsonl"),
        "a non-terminal running observation does not add a run-store write"
    );

    let mut stopped = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    stopped.transcript_path = Some("/tmp/second.jsonl".to_owned());
    record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &stopped,
        Some("done".to_owned()),
        || None,
    )
    .unwrap();
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .transcript_path
            .as_deref(),
        Some("/tmp/second.jsonl")
    );
}

#[test]
fn record_lifecycle_folds_first_late_transcript_path() {
    let (_dir, paths, record) = setup_for("codex");

    let started = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    record_lifecycle(&paths, &record.run_id, "codex", &started, None, || None).unwrap();
    let running = load(&paths, &record.run_id).unwrap();
    assert_eq!(running.status, RunStatus::Running);
    assert_eq!(running.transcript_path, None);

    let mut tool = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::ToolUsed {
            mutates: true,
            edits: true,
            name: None,
            native_key: None,
            turn_id: None,
        },
    );
    tool.transcript_path = Some("/tmp/late.jsonl".to_owned());
    record_lifecycle(&paths, &record.run_id, "codex", &tool, None, || None).unwrap();
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .transcript_path
            .as_deref(),
        Some("/tmp/late.jsonl")
    );
}

#[test]
fn live_status_joins_agent_state() {
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let mut record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = RunStatus::Running;
    record.agent_id = Some(AgentSessionId::from("sess-1"));
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");
    let mut pane = PaneRef::from_id(pane_id.clone());
    pane.session_name = "rimz-test".to_owned();
    let mut agent = agent_state("claude", "sess-1", AgentStatus::Waiting);
    agent.phase = TurnPhase::Idle;
    agent.pane = Some(pane);
    agent.usage.context_pct = Some(42);
    agent.waiting_since = Some(Timestamp::UNIX_EPOCH);
    let snapshot =
        SidebarSnapshot::build_with_agents(workspace_id, vec![agent], Timestamp::UNIX_EPOCH);

    let live = live_status(&record, &snapshot).expect("live status");
    assert_eq!(live.agent_status, AgentStatus::Waiting);
    assert_eq!(live.phase, TurnPhase::Idle);
    assert_eq!(live.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(live.context_pct, Some(42));
    assert_eq!(live.parked_at, None);

    record.parked_at = Some(Timestamp::UNIX_EPOCH);
    let parked = live_status(&record, &snapshot).expect("parked live status");
    assert_eq!(parked.parked_at, Some(Timestamp::UNIX_EPOCH));
}

#[test]
fn live_status_is_absent_for_unbound_or_terminal_runs() {
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
    let mut record = RunRecord::new(
        workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.status = RunStatus::Running;
    let snapshot = SidebarSnapshot::build_with_agents(
        workspace_id,
        vec![agent_state("claude", "sess-1", AgentStatus::Running)],
        Timestamp::UNIX_EPOCH,
    );
    assert!(live_status(&record, &snapshot).is_none());

    record.agent_id = Some(AgentSessionId::from("sess-1"));
    record.status = RunStatus::Completed;
    assert!(live_status(&record, &snapshot).is_none());
}

#[test]
fn record_pane_persists_launch_pane_id() {
    let (_dir, paths, record) = setup();
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%7");

    let updated = record_pane(&paths, &record.run_id, pane_id.clone()).unwrap();
    assert_eq!(updated.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(
        load(&paths, &record.run_id).unwrap().pane_id.as_ref(),
        Some(&pane_id)
    );
}

#[test]
fn record_provider_process_persists_pid_reuse_guard() {
    let (_dir, paths, record) = setup();

    let updated =
        record_provider_process(&paths, &record.run_id, 42, Some("start-42".to_owned())).unwrap();

    assert_eq!(updated.provider_pid, Some(42));
    assert_eq!(updated.provider_process_start.as_deref(), Some("start-42"));
    let loaded = load(&paths, &record.run_id).unwrap();
    assert_eq!(loaded.provider_pid, Some(42));
    assert_eq!(loaded.provider_process_start.as_deref(), Some("start-42"));
}

#[test]
fn fail_if_nonterminal_writes_the_reason_with_the_failed_status() {
    let (_dir, paths, record) = setup();
    let reason = format!("{}{}\n", "a".repeat(FAILURE_TAIL_CAP), "b".repeat(20));
    let failed = fail_if_nonterminal(&paths, &record.run_id, &reason)
        .unwrap()
        .expect("newly failed");
    assert_eq!(failed.status, RunStatus::Failed);
    let stored = load(&paths, &record.run_id).unwrap();
    assert_eq!(stored.status, RunStatus::Failed);
    let tail = stored.failure_tail.expect("reason");
    assert_eq!(tail.len(), FAILURE_TAIL_CAP);
    assert!(tail.ends_with('b'));

    let (_dir, paths, record) = setup();
    record_failure_tail(&paths, &record.run_id, "pane tail").unwrap();
    let failed = fail_if_nonterminal(&paths, &record.run_id, "launch error")
        .unwrap()
        .expect("newly failed");
    assert_eq!(failed.failure_tail.as_deref(), Some("pane tail"));

    let (_dir, paths, record) = setup();
    let failed = fail_if_nonterminal(&paths, &record.run_id, " \n")
        .unwrap()
        .expect("a blank reason still fails the run");
    assert_eq!(failed.status, RunStatus::Failed);
    assert_eq!(failed.failure_tail, None);
}

#[test]
fn record_failure_tail_persists_first_non_empty_tail() {
    let (_dir, paths, record) = setup();

    let stored = record_failure_tail(&paths, &record.run_id, "first\n\n").unwrap();
    assert_eq!(stored.failure_tail.as_deref(), Some("first"));

    let unchanged = record_failure_tail(&paths, &record.run_id, "second").unwrap();
    assert_eq!(unchanged.failure_tail.as_deref(), Some("first"));
    assert_eq!(
        load(&paths, &record.run_id)
            .unwrap()
            .failure_tail
            .as_deref(),
        Some("first")
    );
}

#[test]
fn record_failure_tail_ignores_empty_tail() {
    let (_dir, paths, record) = setup();

    let stored = record_failure_tail(&paths, &record.run_id, " \n\t").unwrap();

    assert_eq!(stored.failure_tail, None);
    assert_eq!(load(&paths, &record.run_id).unwrap().failure_tail, None);
}

#[test]
fn record_failure_tail_caps_stored_tail() {
    let (_dir, paths, record) = setup();
    let tail = format!("{}{}", "a".repeat(FAILURE_TAIL_CAP), "b".repeat(20));

    let stored = record_failure_tail(&paths, &record.run_id, &tail).unwrap();

    let stored = stored.failure_tail.expect("tail");
    assert_eq!(stored.len(), FAILURE_TAIL_CAP);
    assert!(stored.starts_with('a'));
    assert!(stored.ends_with('b'));
}

fn agent_state(kind: &str, id: &str, status: AgentStatus) -> AgentState {
    let mut agent = crate::sidebar::test_support::root_agent(kind, id, None);
    agent.name = None;
    agent.kind_ordinal = None;
    agent.status = status;
    agent.last_seen = Timestamp::UNIX_EPOCH;
    agent.last_activity = Timestamp::UNIX_EPOCH;
    agent.registered_at = Some(Timestamp::UNIX_EPOCH);
    agent
}

fn team_record(paths: &StatePaths, instance: &str) -> RunRecord {
    let mut record = RunRecord::new(
        paths.workspace_id.clone(),
        AgentKind::new_unchecked("claude"),
        PermissionMode::Auto,
        "ship the feature".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    );
    record.agent_name = Some("lead".into());
    record.reader = Some("launcher".into());
    record.team = Some(crate::store::run::TeamRun {
        launch_id: "lead-launch".into(),
        instance: instance.to_owned(),
    });
    create(paths, &record).unwrap();
    record
}

fn turn_end(agent: &str) -> AgentLifecycleObservation {
    AgentLifecycleObservation::new(
        Some(agent.into()),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    )
}

#[test]
fn team_run_keeps_the_leaders_last_message_across_turns_and_rows() {
    let (_dir, paths, _) = setup();
    let record = team_record(&paths, "forge#x");
    for (agent, answer) in [("lead-1", "first"), ("lead-2", "second")] {
        let terminal = record_lifecycle(
            &paths,
            &record.run_id,
            "claude",
            &turn_end(agent),
            Some(answer.into()),
            || None,
        )
        .unwrap();
        assert!(terminal.is_none(), "a turn end leaves a team run open");
        let stored = load(&paths, &record.run_id).unwrap();
        assert_eq!(stored.status, RunStatus::Running);
        assert_eq!(stored.last_message.as_deref(), Some(answer));
        assert_eq!(stored.agent_id.as_ref().map(|id| id.as_str()), Some(agent));
    }
    // A failed turn end is still one turn of the leader's, not the team's end.
    let errored = AgentLifecycleObservation::new(
        Some("lead-2".into()),
        LifecycleSignal::TurnEnded {
            errored: true,
            parked_on_background: false,
            turn_id: None,
        },
    );
    record_lifecycle(&paths, &record.run_id, "claude", &errored, None, || None).unwrap();
    record_assistant_message(
        &paths,
        &record.run_id,
        "claude",
        &"lead-3".into(),
        "third".into(),
    )
    .unwrap();
    let stored = load(&paths, &record.run_id).unwrap();
    assert!(!stored.status.is_terminal());
    assert_eq!(stored.last_message.as_deref(), Some("third"));
}

#[test]
fn team_run_settles_once_per_done_and_reopens_for_the_next() {
    let (_dir, paths, _) = setup();
    let first = team_record(&paths, "forge#x");
    let other = team_record(&paths, "forge#y");
    record_assistant_message(
        &paths,
        &first.run_id,
        "claude",
        &"lead-1".into(),
        "done".into(),
    )
    .unwrap();
    assert_eq!(
        open_team_run_for(&paths, "forge#x")
            .unwrap()
            .map(|run| run.run_id),
        Some(first.run_id.clone())
    );
    assert!(
        reopen_team_run(&paths, "forge#x").unwrap().is_none(),
        "one open run per cohort"
    );

    let report = crate::ids::MessageId::new();
    let settled = settle_team_run(&paths, &first.run_id, Some(&report), None)
        .unwrap()
        .expect("first Done settles the run");
    assert_eq!(settled.status, RunStatus::Completed);
    assert_eq!(settled.report_message_id.as_ref(), Some(&report));
    assert!(
        settle_team_run(&paths, &first.run_id, Some(&report), None)
            .unwrap()
            .is_none()
    );
    let response = paths
        .out_reader_dir(Some("launcher"))
        .join(format!("lead.{}.output", first.run_id));
    assert_eq!(std::fs::read_to_string(response).unwrap(), "done\n");
    assert!(open_team_run_for(&paths, "forge#x").unwrap().is_none());

    let reopened = reopen_team_run(&paths, "forge#x")
        .unwrap()
        .expect("leaving Done opens the next stretch");
    assert_ne!(reopened.run_id, first.run_id);
    assert_eq!(reopened.team, first.team);
    assert_eq!(reopened.prompt, first.prompt);
    assert_eq!(reopened.reader.as_deref(), Some("launcher"));
    assert_eq!(reopened.last_message, None);
    assert_eq!(
        open_team_run_for(&paths, "forge#x")
            .unwrap()
            .map(|run| run.run_id),
        Some(reopened.run_id)
    );
    assert_eq!(
        open_team_run_for(&paths, "forge#y")
            .unwrap()
            .map(|run| run.run_id),
        Some(other.run_id),
        "cohorts settle independently"
    );
}

#[test]
fn provider_startup_exit_reports_only_unprovoked_failures_within_the_window() {
    let exit = ProviderExit {
        fresh_launch: false,
        success: false,
        abrupt: false,
        signaled: false,
        relaunches: 3,
        startup: Duration::from_millis(1),
    };
    assert!(provider_startup_exit(exit));
    assert!(!provider_startup_exit(ProviderExit {
        abrupt: true,
        ..exit
    }));
    assert!(!provider_startup_exit(ProviderExit {
        signaled: true,
        ..exit
    }));
    assert!(provider_startup_exit(ProviderExit {
        startup: Duration::from_secs(60),
        ..exit
    }));
    assert!(!provider_startup_exit(ProviderExit {
        success: true,
        ..exit
    }));
    assert!(!provider_startup_exit(ProviderExit {
        startup: Duration::from_secs(61),
        ..exit
    }));
}

#[test]
fn startup_relaunch_is_due_only_for_an_unopened_nonzero_exit_below_the_cap() {
    use StartupRelaunch::{Due, No, Spent};
    let died = ProviderExit {
        fresh_launch: true,
        success: false,
        abrupt: false,
        signaled: false,
        relaunches: 0,
        startup: Duration::from_secs(5),
    };
    let pending = StartupEvidence::Run(RunStatus::Pending);
    let provisional = StartupEvidence::Card { provisional: true };
    let slow = ProviderExit {
        startup: Duration::from_secs(3600),
        ..died
    };
    for relaunches in 0..3 {
        let exit = ProviderExit { relaunches, ..slow };
        assert_eq!(startup_relaunch(exit, pending, 3), Due, "{relaunches}");
    }
    assert_eq!(startup_relaunch(died, provisional, 3), Due);
    let at_window = ProviderExit {
        startup: Duration::from_secs(60),
        ..died
    };
    assert_eq!(startup_relaunch(at_window, provisional, 3), Due);
    let past_window = ProviderExit {
        startup: Duration::from_secs(61),
        ..died
    };
    assert_eq!(
        startup_relaunch(past_window, provisional, 3),
        No,
        "a provisional card is normal for a session nobody prompted"
    );
    assert_eq!(
        startup_relaunch(died, StartupEvidence::Card { provisional: false }, 3),
        No,
        "the card was adopted"
    );

    for evidence in [pending, provisional] {
        for (why, exit, cap) in [
            (
                "a resume or fork",
                ProviderExit {
                    fresh_launch: false,
                    ..died
                },
                3,
            ),
            (
                "exit status 0",
                ProviderExit {
                    success: true,
                    ..died
                },
                3,
            ),
            (
                "the wrapper ended the provider",
                ProviderExit {
                    abrupt: true,
                    ..died
                },
                3,
            ),
            (
                "a stop or interrupt signal",
                ProviderExit {
                    signaled: true,
                    ..died
                },
                3,
            ),
            ("the relaunch is disabled", died, 0),
        ] {
            assert_eq!(startup_relaunch(exit, evidence, cap), No, "{why}");
        }
        let spent = ProviderExit {
            relaunches: 3,
            ..died
        };
        assert_eq!(startup_relaunch(spent, evidence, 3), Spent);
    }
    let opened = ProviderExit {
        relaunches: 3,
        success: true,
        ..died
    };
    assert_eq!(
        startup_relaunch(opened, pending, 3),
        No,
        "the cap is spent only by a startup death"
    );

    for status in [
        RunStatus::Running,
        RunStatus::Completed,
        RunStatus::Failed,
        RunStatus::VerifyFailed,
        RunStatus::Canceled,
        RunStatus::TimedOut,
        RunStatus::BudgetExceeded,
    ] {
        assert_eq!(
            startup_relaunch(died, StartupEvidence::Run(status), 3),
            No,
            "{status:?}"
        );
    }
}

#[test]
fn a_detached_background_run_publishes_its_response_at_the_terminal_write() {
    let (_dir, paths, mut record) = setup();
    record.report_to = ReportTo::Nobody;
    record.agent_name = Some("worker".into());
    record.agent_id = Some("worker".into());
    record.reader = Some("launcher".into());
    create(&paths, &record).unwrap();
    record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &turn_end("worker"),
        Some("detached answer".into()),
        || None,
    )
    .unwrap();
    assert!(load(&paths, &record.run_id).unwrap().status.is_terminal());
    let path = paths.out_reader_dir(Some("launcher")).join("worker.output");
    assert_eq!(
        std::fs::read_to_string(&path).ok().as_deref(),
        Some("detached answer\n"),
        "no fleet reporter publishes a detached run, so its terminal write must"
    );
}

#[test]
fn a_detached_answer_is_dropped_when_its_subagent_reopens_attached() {
    let (_dir, paths, mut record) = setup();
    record.subagent = true;
    record.report_to = ReportTo::Nobody;
    record.agent_id = Some("child".into());
    record.status = RunStatus::Completed;
    record.completed_at = Some(record.started_at);
    record.last_message = Some("detached answer".into());
    create(&paths, &record).unwrap();
    let observation = AgentLifecycleObservation::new(
        Some("child".into()),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    record_lifecycle(&paths, &record.run_id, "claude", &observation, None, || {
        None
    })
    .unwrap();
    let reopened = load(&paths, &record.run_id).unwrap();
    assert_eq!(reopened.status, RunStatus::Running);
    assert_eq!(reopened.follow_ups, 1);
    assert_eq!(
        reopened.report_to,
        ReportTo::Launcher,
        "the follow-up is its launcher's again"
    );
    assert!(
        reopened.earlier_answers.is_empty(),
        "the detached answer joins no later digest"
    );
    let settled = record_lifecycle(
        &paths,
        &record.run_id,
        "claude",
        &turn_end("child"),
        Some("follow-up answer".into()),
        || None,
    )
    .unwrap()
    .expect("the follow-up turn settles the run");
    let owed = settled
        .answer_claims()
        .filter(|claim| claim.owed)
        .map(|claim| claim.ordinal)
        .collect::<Vec<_>>();
    assert_eq!(owed, [2], "only the follow-up answer is reported");
}

#[test]
fn a_detached_team_run_reopens_detached() {
    let (_dir, paths, _) = setup();
    let mut first = team_record(&paths, "forge#x");
    first.report_to = ReportTo::Nobody;
    create(&paths, &first).unwrap();
    settle_team_run(&paths, &first.run_id, None, None)
        .unwrap()
        .expect("Done settles the detached run");
    let reopened = reopen_team_run(&paths, "forge#x")
        .unwrap()
        .expect("leaving Done opens the next stretch");
    assert_eq!(reopened.report_to, ReportTo::Nobody);
}

#[test]
fn a_park_is_claimed_once_until_the_child_acts_again() {
    let (_dir, paths, record) = setup();
    let parked: Timestamp = "2026-01-01T01:00:00Z".parse().unwrap();
    let resumed = parked + std::time::Duration::from_secs(600);
    let stamp = || load(&paths, &record.run_id).unwrap().park_noticed_activity;

    assert!(claim_park_notice(&paths, &record.run_id, parked).unwrap());
    assert!(!claim_park_notice(&paths, &record.run_id, parked).unwrap());
    assert_eq!(stamp(), Some(parked));
    assert!(claim_park_notice(&paths, &record.run_id, resumed).unwrap());
    assert!(!claim_park_notice(&paths, &record.run_id, parked).unwrap());
    assert_eq!(stamp(), Some(resumed));

    release_park_notice(&paths, &record.run_id, parked).unwrap();
    assert_eq!(stamp(), Some(resumed));
    release_park_notice(&paths, &record.run_id, resumed).unwrap();
    assert_eq!(stamp(), None);
    assert!(claim_park_notice(&paths, &record.run_id, resumed).unwrap());

    cancel(&paths, &record.run_id).unwrap();
    assert!(
        !claim_park_notice(
            &paths,
            &record.run_id,
            resumed + std::time::Duration::from_secs(1)
        )
        .unwrap()
    );
}
