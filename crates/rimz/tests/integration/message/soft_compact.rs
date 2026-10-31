use super::*;
use rimz::harness::soft_compact::SoftCompactRequest;
use rimz::store::message::SoftCompactStep;

struct Fixture {
    env: Env,
    request: SoftCompactRequest,
    panes: PathBuf,
}

impl Fixture {
    fn new(kind: &str) -> Self {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks(kind);
        for (key, value) in [
            ("harness.smart_compact", "100k"),
            (
                "harness.compact_instruction",
                "Keep the current plan and test results.",
            ),
            ("resume.auto_continue_text", "  keep going  "),
        ] {
            run_success(
                env.rimz().args(["config", "set", key, value]),
                "configure soft compaction",
            );
        }
        let request = SoftCompactRequest {
            workspace_id: env.workspace_id.clone(),
            kind: AgentKind::new_unchecked(kind),
            agent_id: "soft-session".into(),
            pane_id: PaneId::from_parts(MuxName::Zellij, TRACE_PANE),
            label: "@worker".into(),
            step: SoftCompactStep::WrapUp,
        };
        let session = env.resolve_workspace(&env.project_root).session_name;
        let mut pane = agent_pane(&env, kind);
        pane.session_name = session.clone();
        let panes = env.write_pane_fixture(std::slice::from_ref(&pane));
        let frame = rimz::sidebar::frame::assemble_frame(
            vec![pane],
            rimz::utils::time::unix_now_ms(),
            session,
        );
        rimz::disk::atomic::write_temp_then_rename_cache(
            &env.runtime_paths().pane_frame_path(),
            &frame,
        )
        .unwrap();
        let fixture = Self {
            env,
            request,
            panes,
        };
        fixture.lifecycle(LifecycleSignal::Registered);
        fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
        fixture.context(120_000);
        fixture
    }

    fn lifecycle(&self, signal: LifecycleSignal) {
        append_lifecycle(
            &self.env,
            self.request.kind.as_str(),
            "soft-test",
            self.request.agent_id.as_str(),
            signal,
            |observation| {
                observation.pane_id = Some(self.request.pane_id.clone());
            },
        );
    }

    fn context(&self, tokens: u64) {
        let mut context =
            rimz::agents::AgentContext::new(self.request.kind.as_str(), jiff::Timestamp::now());
        context.tokens = Some(rimz::agents::AgentTokenUsage {
            context_window_size: Some(200_000),
            current_usage: Some(rimz::agents::AgentCurrentUsage {
                input_tokens: Some(tokens),
                ..Default::default()
            }),
            ..Default::default()
        });
        let record = rimz::agents::context::record::AgentContextRecord::new(
            self.request.kind.as_str(),
            self.request.agent_id.as_str(),
            context,
        );
        rimz::store::agent_context::write_record(&self.env.runtime_paths(), &record).unwrap();
    }

    fn command(&self, step: SoftCompactStep) -> std::process::Command {
        let mut request = self.request.clone();
        request.step = step;
        let mut command = traced_rimz(&self.env, "soft-trace.log");
        command
            .env("RIMZ_TEST_PANE_LIST", &self.panes)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
            .args(rimz::child_process::agent_helper_argv(
                "soft-compact",
                &request,
            ));
        command
    }

    fn send(&self, step: SoftCompactStep) {
        run_success(&mut self.command(step), "soft compaction helper");
    }

    fn records(&self) -> Vec<MessageRecord> {
        let store = self.env.store();
        let (mut live, history) = store.list_messages_and_history().unwrap();
        live.extend(history);
        live
    }

    fn trace(&self) -> Vec<String> {
        trace_lines(&self.env.project_root.join("soft-trace.log"))
    }

    fn end_turn(&self) {
        self.lifecycle(LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        });
    }

    fn deliver(&self, id: &MessageId) {
        run_success(
            traced_rimz(&self.env, "soft-trace.log")
                .env("RIMZ_TEST_PANE_LIST", &self.panes)
                .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
                .args(["message", "deliver", "--message-id", id.as_str()]),
            "soft boundary delivery",
        );
    }

    fn stats(&self) -> serde_json::Value {
        let out = run_success(
            self.env
                .rimz()
                .args(["stats", "--assists", "--all", "--json"]),
            "soft assist stats",
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

fn full_cycle(kind: &str) {
    let fixture = Fixture::new(kind);
    fixture.send(SoftCompactStep::WrapUp);
    let records = fixture.records();
    assert_eq!(records.len(), 1, "the helper must send one wrap-up");
    let wrap_up = &records[0];
    assert_eq!(wrap_up.soft_compact, Some(SoftCompactStep::WrapUp));
    assert_eq!(wrap_up.status, MessageStatus::Sent);
    assert_eq!(wrap_up.sender, MessageSender::System);
    assert_eq!(wrap_up.gate, DeliveryGate::Any);
    assert!(wrap_up.automated);
    assert_eq!(wrap_up.auto_compact, None);
    assert!(
        fixture
            .trace()
            .iter()
            .any(|line| is_paste(line, &wrap_up.text))
    );
    assert!(
        !fixture.trace().iter().any(|line| is_compact_command(line)),
        "no command in an open turn"
    );
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(
        fixture.records().len(),
        1,
        "no second steer in the same epoch"
    );
    let store = fixture.env.store();
    let agent = store
        .agent_with_context(
            &fixture.request.kind,
            &fixture.request.agent_id,
            rimz::RuntimeScope::Runtime,
        )
        .unwrap()
        .unwrap();
    assert!(agent.soft_compact.is_some());
    let mut earlier = MessageRecord::new(
        fixture.env.workspace_id.clone(),
        &agent,
        "earlier user message".into(),
        DeliveryGate::Done,
    )
    .with_channel(agent.channel());
    earlier.message_id = MessageId::parse("msg_v000000000000000").unwrap();
    store.queue_message(&earlier, "rimz-test").unwrap();
    fixture.end_turn();
    fixture.send(SoftCompactStep::Continue);
    let records = fixture.records();
    let continuation = records
        .iter()
        .find(|record| record.soft_compact == Some(SoftCompactStep::Continue))
        .expect("continue queued at the boundary");
    assert_eq!(continuation.status, MessageStatus::Queued);
    assert!(
        continuation.message_id > earlier.message_id,
        "tail id must be assigned under the queue lock"
    );
    assert_eq!(continuation.text, "keep going");
    assert_eq!(continuation.sender, MessageSender::System);
    assert_eq!(continuation.gate, DeliveryGate::Done);
    assert_eq!(
        continuation.auto_compact,
        Some(AutoCompact::Tokens(100_000))
    );
    assert!(
        !fixture.trace().iter().any(|line| is_compact_command(line)),
        "the earlier record owns the boundary"
    );
    fixture.send(SoftCompactStep::Continue);
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|record| record.soft_compact == Some(SoftCompactStep::Continue))
            .count(),
        1
    );
    fixture.deliver(&earlier.message_id);
    let trace = fixture.trace();
    assert_eq!(
        trace.iter().filter(|line| is_compact_command(line)).count(),
        1
    );
    if kind == "claude" {
        assert_compact_segments_then_enter(&trace, "Keep the current plan and test results.");
    } else {
        assert!(
            trace.iter().any(|line| line.ends_with("\t--\t/compact")),
            "non-trailing adapter gets the bare command: {trace:?}"
        );
    }
    assert!(!trace.iter().any(|line| is_paste(line, "keep going")));
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(LifecycleSignal::CompactionEnded {
        auto: None,
        failed: false,
    });
    fixture.context(20_000);
    fixture.deliver(&earlier.message_id);
    let records = fixture.records();
    assert!(
        records
            .iter()
            .filter(|record| record.body == MessageBody::Prompt
                && record.soft_compact != Some(SoftCompactStep::WrapUp))
            .all(|record| record.status == MessageStatus::Sent)
    );
    let stats = fixture.stats();
    assert_eq!(stats["rollup"]["soft_compacts"], 1);
    assert_eq!(stats["rollup"]["compacts"], 1);
    assert_eq!(
        stats["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["assist"] == "soft_compact")
            .count(),
        1
    );
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    fixture.context(130_000);
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|record| record.soft_compact == Some(SoftCompactStep::WrapUp))
            .count(),
        2
    );
    assert_eq!(fixture.stats()["rollup"]["soft_compacts"], 2);
}

#[test]
fn trailing_adapter_completes_the_soft_compaction_cycle_at_the_fifo_tail() {
    full_cycle("claude");
}

#[test]
fn non_trailing_adapter_completes_the_soft_compaction_cycle_at_the_fifo_tail() {
    full_cycle("codex");
}

#[test]
fn codex_continuation_preserves_an_owed_continue_and_rearms_wrap_up() {
    linked_compaction_cycle(false);
}

#[test]
fn codex_continuation_rearms_after_the_predecessor_queued_continue() {
    linked_compaction_cycle(true);
}

fn linked_compaction_cycle(already_continued: bool) {
    let mut fixture = Fixture::new("codex");
    fixture.send(SoftCompactStep::WrapUp);
    fixture.end_turn();
    if already_continued {
        fixture.send(SoftCompactStep::Continue);
    }
    let mut observation = AgentLifecycleObservation::new(
        Some("continuation".into()),
        LifecycleSignal::CompactionEnded {
            auto: Some(false),
            failed: false,
        },
    );
    observation.compacted_from = Some(fixture.request.agent_id.clone());
    observation.pane_id = Some(fixture.request.pane_id.clone());
    fixture
        .env
        .store()
        .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
            session_name: "soft-test",
            agent_kind: fixture.request.kind.clone(),
            event_name: "PostCompact",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    fixture.request.agent_id = "continuation".into();
    fixture.context(20_000);
    fixture.send(SoftCompactStep::Continue);
    let records = fixture.records();
    let continues = records
        .iter()
        .filter(|record| record.soft_compact == Some(SoftCompactStep::Continue))
        .collect::<Vec<_>>();
    assert_eq!(continues.len(), 1, "continue must be queued exactly once");
    if !already_continued {
        assert_eq!(continues[0].agent_id, fixture.request.agent_id);
    }
    fixture.deliver(&continues[0].message_id);
    assert_eq!(
        fixture
            .records()
            .iter()
            .find(|record| record.message_id == continues[0].message_id)
            .unwrap()
            .status,
        MessageStatus::Sent,
        "the predecessor's queued continue must reach the continuation row"
    );
    assert!(
        fixture
            .trace()
            .iter()
            .any(|line| is_paste(line, "keep going"))
    );
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    let delivered = fixture
        .env
        .store()
        .confirm_delivered_for_card(
            &fixture.request.kind,
            &fixture.request.agent_id,
            None,
            rimz::store::writer::DeliveryAck::TurnStarted {
                prompt: Some("keep going"),
            },
            "soft-test",
        )
        .unwrap();
    assert!(
        delivered
            .iter()
            .any(|record| record.message_id == continues[0].message_id)
    );
    fixture.context(130_000);
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(
        fixture
            .records()
            .iter()
            .filter(|record| record.agent_id == fixture.request.agent_id
                && record.soft_compact == Some(SoftCompactStep::WrapUp)
                && record.status == MessageStatus::Sent)
            .count(),
        1,
        "the compaction that created this row must re-arm its wrap-up"
    );
}

#[test]
fn supervised_run_parks_until_the_soft_compaction_continue_turn_finishes() {
    use rimz::harness::run;
    use rimz::store::run::{RunRecord, RunStatus};

    let fixture = Fixture::new("claude");
    let store = fixture.env.store();
    let mut record = RunRecord::new(
        fixture.env.workspace_id.clone(),
        fixture.request.kind.clone(),
        rimz::agents::PermissionMode::Auto,
        "finish the task".into(),
        fixture.env.project_root.clone(),
    );
    record.agent_id = Some(fixture.request.agent_id.clone());
    record.status = RunStatus::Running;
    record.subagent = true;
    run::create(store.paths(), &record).unwrap();
    let settle = |signal: LifecycleSignal, text: &str| {
        fixture.lifecycle(signal.clone());
        run::settle_lifecycle(
            &store,
            &record.run_id,
            rimz::agents::definition_by_kind("claude").unwrap(),
            &AgentLifecycleObservation::new(record.agent_id.clone(), signal),
            Some(text.into()),
        )
        .unwrap()
    };
    let end = || LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    };
    fixture.send(SoftCompactStep::WrapUp);
    settle(end(), "note before compaction");
    let parked = run::load(store.paths(), &record.run_id).unwrap();
    assert_eq!(
        parked.status,
        RunStatus::Running,
        "the wrap-up is not the answer"
    );
    assert!(parked.parked_at.is_some());
    assert_eq!(
        run::settle_stranded_park(&store, &parked, parked.parked_at).unwrap(),
        run::ParkCheck::Live
    );
    fixture.context(20_000);
    fixture.send(SoftCompactStep::Continue);
    settle(LifecycleSignal::TurnStarted { turn_id: None }, "");
    assert_eq!(
        run::load(store.paths(), &record.run_id).unwrap().parked_at,
        None
    );
    store
        .confirm_delivered_for_card(
            &fixture.request.kind,
            &fixture.request.agent_id,
            None,
            rimz::store::writer::DeliveryAck::TurnStarted {
                prompt: Some("keep going"),
            },
            "soft-test",
        )
        .unwrap();
    assert_eq!(
        settle(end(), "finished task").unwrap().status,
        RunStatus::Completed
    );
}

#[test]
fn native_mid_turn_compaction_then_interrupt_never_queues_continue() {
    let fixture = Fixture::new("claude");
    fixture.send(SoftCompactStep::WrapUp);
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(LifecycleSignal::CompactionEnded {
        auto: Some(true),
        failed: false,
    });
    fixture.context(20_000);
    fixture.send(SoftCompactStep::Continue);
    assert!(
        !fixture
            .records()
            .iter()
            .any(|record| record.soft_compact == Some(SoftCompactStep::Continue)),
        "native compaction did not end the turn"
    );
    fixture.lifecycle(LifecycleSignal::TurnInterrupted { turn_id: None });
    fixture.end_turn();
    fixture.send(SoftCompactStep::Continue);
    assert!(
        !fixture
            .trace()
            .iter()
            .any(|line| is_paste(line, "keep going"))
    );
    assert!(
        !fixture
            .records()
            .iter()
            .any(|record| record.soft_compact == Some(SoftCompactStep::Continue))
    );
}

#[test]
fn errored_wrap_up_waits_for_a_clean_turn_end_before_continue() {
    let fixture = Fixture::new("claude");
    fixture.send(SoftCompactStep::WrapUp);
    fixture.lifecycle(LifecycleSignal::TurnEnded {
        errored: true,
        parked_on_background: false,
        turn_id: None,
    });
    fixture.send(SoftCompactStep::Continue);
    assert!(
        !fixture
            .records()
            .iter()
            .any(|record| record.soft_compact == Some(SoftCompactStep::Continue)),
        "an errored turn is not a clean wrap-up"
    );
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    fixture.end_turn();
    fixture.context(20_000);
    fixture.send(SoftCompactStep::Continue);
    assert!(
        fixture
            .trace()
            .iter()
            .any(|line| is_paste(line, "keep going"))
    );
}

#[test]
fn queued_continue_retires_on_takeover_before_candidacy_and_after_batch_claim() {
    for after_claim in [false, true] {
        for signal in [
            LifecycleSignal::TurnInterrupted { turn_id: None },
            LifecycleSignal::Registered,
        ] {
            let fixture = Fixture::new("claude");
            fixture.send(SoftCompactStep::WrapUp);
            fixture.end_turn();
            fixture.context(20_000);
            let store = fixture.env.store();
            let agent = store
                .agent_with_context(
                    &fixture.request.kind,
                    &fixture.request.agent_id,
                    rimz::RuntimeScope::Runtime,
                )
                .unwrap()
                .unwrap();
            let earlier = MessageRecord::new(
                fixture.env.workspace_id.clone(),
                &agent,
                "earlier user message".into(),
                DeliveryGate::Done,
            );
            if after_claim {
                store.queue_message(&earlier, "soft-test").unwrap();
            }
            let mut continuation = MessageRecord::new(
                fixture.env.workspace_id.clone(),
                &agent,
                "keep going".into(),
                DeliveryGate::Done,
            );
            continuation.soft_compact = Some(SoftCompactStep::Continue);
            continuation.sender = MessageSender::System;
            store.queue_message(&continuation, "soft-test").unwrap();
            if after_claim {
                let gate = DeliveryRendezvous::new(&fixture.env, "soft-after-claim");
                let child = traced_rimz(&fixture.env, "soft-trace.log")
                    .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
                    .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                    .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
                    .env("RIMZ_TEST_DELIVERY_AFTER_CLAIM", &gate.path)
                    .args([
                        "message",
                        "deliver",
                        "--message-id",
                        earlier.message_id.as_str(),
                    ])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                let mut release = gate.arrive();
                fixture.lifecycle(signal);
                release.write_all(&[1]).unwrap();
                assert!(child.wait_with_output().unwrap().status.success());
            } else {
                fixture.lifecycle(signal);
                fixture.deliver(&continuation.message_id);
            }
            let retired = fixture
                .records()
                .into_iter()
                .find(|record| record.message_id == continuation.message_id)
                .unwrap();
            assert_eq!(retired.status, MessageStatus::Canceled);
            assert!(fixture.env.read_events().iter().any(|event| {
                event.method == "message.canceled"
                    && event.params_value()["message_id"] == continuation.message_id.as_str()
                    && event.params_value()["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("soft-compaction cycle"))
            }));
            assert!(
                !fixture
                    .trace()
                    .iter()
                    .any(|line| line.contains("keep going") || is_paste(line, "keep going"))
            );
        }
    }
}

#[test]
fn native_compaction_still_continues_without_another_command() {
    let fixture = Fixture::new("claude");
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(
        fixture.records().len(),
        1,
        "one wrap-up before native compaction"
    );
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(LifecycleSignal::CompactionEnded {
        auto: Some(true),
        failed: false,
    });
    fixture.context(20_000);
    fixture.end_turn();
    fixture.send(SoftCompactStep::Continue);
    assert!(fixture.records().iter().any(|record| record.soft_compact
        == Some(SoftCompactStep::Continue)
        && record.status == MessageStatus::Sent));
    assert!(!fixture.trace().iter().any(|line| is_compact_command(line)));
    assert!(
        fixture
            .trace()
            .iter()
            .any(|line| is_paste(line, "keep going"))
    );
    assert_eq!(fixture.stats()["rollup"]["soft_compacts"], 1);
}

#[test]
fn interruption_and_blank_continue_text_do_not_strand_a_soft_cycle() {
    let fixture = Fixture::new("claude");
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(
        fixture.records().len(),
        1,
        "one wrap-up before interruption"
    );
    fixture.lifecycle(LifecycleSignal::TurnInterrupted { turn_id: None });
    fixture.end_turn();
    fixture.send(SoftCompactStep::Continue);
    assert_eq!(
        fixture.records().len(),
        1,
        "user takeover cancels continuation"
    );
    run_success(
        fixture
            .env
            .rimz()
            .args(["config", "set", "resume.auto_continue_text", "  "]),
        "blank continue",
    );
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(
        fixture.records().len(),
        1,
        "blank text disables the trigger"
    );
}

#[test]
fn wrap_up_skips_an_input_prompt_raised_while_it_waited_for_the_pane_lock() {
    let fixture = Fixture::new("claude");
    let before = DeliveryRendezvous::new(&fixture.env, "soft-before-lock");
    let child = fixture
        .command(SoftCompactStep::WrapUp)
        .env("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK", &before.path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    fixture.lifecycle(LifecycleSignal::AwaitingInput {
        kind: AskKind::Permission,
        ask_id: None,
        detail: None,
        native_key: None,
    });
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        fixture.trace().is_empty(),
        "no write into an input prompt: {:?}, {output:?}",
        fixture.trace()
    );
    let records = fixture.records();
    assert_eq!(records.len(), 1, "{output:?}");
    assert_eq!(records[0].status, MessageStatus::Canceled);
    assert_eq!(
        records[0].last_error.as_deref(),
        Some("agent is waiting on input in its pane")
    );
}

#[test]
fn wrap_up_write_failure_cancels_the_miss_and_records_one_failed_assist() {
    let fixture = Fixture::new("claude");
    let output = fixture
        .command(SoftCompactStep::WrapUp)
        .env("RIMZ_TEST_ZELLIJ_MODE", "fail-write")
        .output()
        .unwrap();
    let records = fixture.records();
    assert_eq!(
        records.len(),
        1,
        "the failed attempt stays recorded: {output:?}"
    );
    assert_eq!(records[0].status, MessageStatus::Canceled);
    assert!(records[0].last_error.is_some());
    assert!(
        fixture.env.store().list_messages().unwrap().is_empty(),
        "a missed steer must never reach the boundary"
    );
    let stats = fixture.stats();
    assert_eq!(stats["rollup"]["soft_compacts"], 0);
    let attempts = stats["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["assist"] == "soft_compact")
        .collect::<Vec<_>>();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["delivered"], false);
    assert_eq!(
        attempts[0]["error"].as_str(),
        records[0].last_error.as_deref()
    );
}
