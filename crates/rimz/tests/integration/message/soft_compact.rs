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

impl Fixture {
    fn lifecycle_with_prompt(&self, signal: LifecycleSignal, prompt: &str) {
        append_lifecycle(
            &self.env,
            self.request.kind.as_str(),
            "soft-test",
            self.request.agent_id.as_str(),
            signal,
            |observation| {
                observation.pane_id = Some(self.request.pane_id.clone());
                observation.prompt = rimz::agents::SanitizedPrompt::new(Some(prompt));
            },
        );
    }

    /// Claude's Esc as it reaches RimZ: no hook, only the mod's `turn.complete`
    /// with `reason: aborted` and the transcript's interruption row.
    fn claude_esc(&self) {
        let transcript = self.env.project_root.join("soft-esc.jsonl");
        let row = serde_json::json!({
            "type": "user",
            "timestamp": jiff::Timestamp::now().to_string(),
            "message": {"role": "user", "content": [
                {"type": "text", "text": "[Request interrupted by user]"}
            ]},
        });
        std::fs::write(&transcript, format!("{row}\n")).unwrap();
        let envelope = serde_json::json!({
            "v": "1.1.0",
            "event": "turn.complete",
            "session_id": self.request.agent_id.as_str(),
            "transcript_path": transcript,
            "claude_version": "2.1.326",
            "reason": "aborted",
        });
        let out = self.env.run_mod_feed("claude", &envelope.to_string());
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let shown = run_success(
            self.env
                .rimz()
                .env("RIMZ_TEST_PANE_LIST", &self.panes)
                .args(["agents", "show", "@claude", "--json"]),
            "agents show",
        );
        let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(
            shown["agent"]["status"], "success",
            "an interrupted row no longer owes the continue, so nothing lifts its rest: {shown}"
        );
    }

    fn agent(&self) -> rimz::agents::AgentState {
        self.env
            .store()
            .agent_with_context(
                &self.request.kind,
                &self.request.agent_id,
                rimz::RuntimeScope::Runtime,
            )
            .unwrap()
            .unwrap()
    }

    /// `rimz agents compact @<kind> <extra>`, the agent's own self-compaction.
    fn compact(&self, extra: &[&str]) -> String {
        let handle = format!("@{}", self.request.kind.as_str());
        let output = run_success(
            traced_rimz(&self.env, "soft-trace.log")
                .env("RIMZ_TEST_PANE_LIST", &self.panes)
                .args(["agents", "compact", handle.as_str()])
                .args(extra),
            "self compaction",
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The queued compaction command and the continue behind it, in queue order.
    fn command_and_continue(&self) -> (MessageRecord, MessageRecord) {
        let records = self.records();
        assert_eq!(records.len(), 2, "{records:?}");
        let command = records
            .iter()
            .find(|record| record.body == MessageBody::Command)
            .expect("compaction command")
            .clone();
        let continuation = records
            .iter()
            .find(|record| record.soft_compact == Some(SoftCompactStep::Continue))
            .expect("compaction continue")
            .clone();
        (command, continuation)
    }

    fn status_of(&self, id: &MessageId) -> MessageStatus {
        self.records()
            .into_iter()
            .find(|record| record.message_id == *id)
            .unwrap()
            .status
    }

    fn reminders(&self) -> usize {
        self.records()
            .iter()
            .filter(|record| record.soft_compact == Some(SoftCompactStep::WrapUp))
            .count()
    }

    fn assert_canceled_with(&self, id: &MessageId, reason: &str) {
        assert_eq!(self.status_of(id), MessageStatus::Canceled);
        let events = self.env.read_events();
        assert!(
            events.iter().any(|event| {
                event.method == "message.canceled"
                    && event.params_value()["message_id"] == id.as_str()
                    && event.params_value()["reason"]
                        .as_str()
                        .is_some_and(|text| text.contains(reason))
            }),
            "no cancel of {id} naming {reason:?}: {:?}",
            events
                .iter()
                .filter(|event| event.method == "message.canceled")
                .map(|event| event.params_value())
                .collect::<Vec<_>>()
        );
    }
}

fn compaction_end() -> LifecycleSignal {
    LifecycleSignal::CompactionEnded {
        auto: None,
        failed: false,
    }
}

fn reminder_once_per_epoch(kind: &str) {
    let fixture = Fixture::new(kind);
    fixture.send(SoftCompactStep::WrapUp);
    let records = fixture.records();
    assert_eq!(records.len(), 1, "the helper must send one reminder");
    let reminder = &records[0];
    assert_eq!(reminder.soft_compact, Some(SoftCompactStep::WrapUp));
    assert_eq!(reminder.status, MessageStatus::Sent);
    assert_eq!(reminder.sender, MessageSender::System);
    assert_eq!(reminder.gate, DeliveryGate::Any);
    assert!(reminder.automated);
    assert_eq!(reminder.auto_compact, None);
    assert_eq!(
        Some(reminder.text.as_str()),
        rimz::agents::compact_reminder(&fixture.agent()).as_deref()
    );
    assert!(
        fixture
            .trace()
            .iter()
            .any(|line| is_paste(line, &reminder.text))
    );
    fixture.send(SoftCompactStep::WrapUp);
    fixture.send(SoftCompactStep::Continue);
    assert_eq!(
        fixture.records().len(),
        1,
        "no second reminder in the epoch, and an older host's continue request sends nothing"
    );
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(LifecycleSignal::CompactionEnded {
        auto: Some(true),
        failed: false,
    });
    fixture.context(20_000);
    fixture.end_turn();
    fixture.send(SoftCompactStep::WrapUp);
    fixture.send(SoftCompactStep::Continue);
    assert_eq!(
        fixture.records().len(),
        1,
        "nothing follows a reminder after a native compaction and a turn end"
    );
    let trace = fixture.trace();
    assert!(
        !trace.iter().any(|line| is_compact_command(line)),
        "{trace:?}"
    );
    assert!(
        !trace.iter().any(|line| is_paste(line, "keep going")),
        "{trace:?}"
    );
    let stats = fixture.stats();
    assert_eq!(stats["rollup"]["soft_compacts"], 1);
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
    assert_eq!(fixture.reminders(), 2, "a new epoch re-arms the reminder");
    assert_eq!(fixture.stats()["rollup"]["soft_compacts"], 2);
}

#[test]
fn trailing_adapter_gets_one_reminder_per_epoch_and_nothing_after_it() {
    reminder_once_per_epoch("claude");
}

#[test]
fn non_trailing_adapter_gets_one_reminder_per_epoch_and_nothing_after_it() {
    reminder_once_per_epoch("codex");
}

#[test]
fn reminder_is_withheld_behind_an_owed_continue_blank_text_or_an_interrupt_but_not_a_clear() {
    let fixture = Fixture::new("claude");
    fixture.compact(&[]);
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(
        fixture.reminders(),
        0,
        "a compaction owed a continue withholds the reminder"
    );
    let fixture = Fixture::new("claude");
    run_success(
        fixture
            .env
            .rimz()
            .args(["config", "set", "resume.auto_continue_text", "  "]),
        "blank continue",
    );
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(fixture.reminders(), 0, "blank text withholds the reminder");

    let fixture = Fixture::new("claude");
    fixture.send(SoftCompactStep::WrapUp);
    fixture.lifecycle(LifecycleSignal::TurnInterrupted { turn_id: None });
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(
        fixture.reminders(),
        1,
        "an interrupt does not re-arm the epoch"
    );
    fixture.lifecycle(LifecycleSignal::Registered);
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(fixture.reminders(), 2, "a /clear re-arms the reminder");
}

#[test]
fn self_compaction_mid_turn_types_the_command_then_its_continue() {
    let fixture = Fixture::new("claude");
    fixture.send(SoftCompactStep::WrapUp);
    let receipt = fixture.compact(&["keep the plan"]);
    assert!(
        receipt.starts_with("queued compaction for @claude (msg_")
            && receipt
                .trim_end()
                .ends_with("delivers at its next turn boundary, then continues it"),
        "{receipt}"
    );
    let records = fixture.records();
    assert_eq!(records.len(), 3, "{records:?}");
    let command = records
        .iter()
        .find(|record| record.body == MessageBody::Command)
        .unwrap();
    let continuation = records
        .iter()
        .find(|record| record.soft_compact == Some(SoftCompactStep::Continue))
        .unwrap();
    assert_eq!(command.status, MessageStatus::Queued);
    assert_eq!(continuation.status, MessageStatus::Queued);
    assert_eq!(continuation.body, MessageBody::Prompt);
    assert_eq!(continuation.sender, MessageSender::System);
    assert_eq!(continuation.gate, DeliveryGate::Done);
    assert_eq!(continuation.text, "keep going");
    assert!(
        continuation.enqueued_at <= command.updated_at,
        "the continue is queued with its command, before the command's attempt: {command:?}"
    );
    assert_eq!(continuation.auto_compact, None);
    assert_eq!(
        continuation.follows_command.as_ref(),
        Some(&command.message_id)
    );
    assert!(continuation.message_id > command.message_id);
    assert!(
        fixture
            .agent()
            .soft_compact
            .is_some_and(|mark| mark.continue_owed.is_some())
    );

    fixture.end_turn();
    fixture.deliver(&command.message_id);
    assert_compact_segments_then_enter(&fixture.trace(), "keep the plan");
    assert_eq!(fixture.status_of(&command.message_id), MessageStatus::Sent);
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Queued,
        "the continue waits out the compaction the command opens"
    );
    assert!(
        !fixture
            .trace()
            .iter()
            .any(|line| is_paste(line, "keep going"))
    );
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(compaction_end());
    fixture.context(20_000);
    fixture.deliver(&continuation.message_id);
    let trace = fixture.trace();
    let command_at = trace
        .iter()
        .position(|line| is_compact_command(line))
        .unwrap();
    let pastes = trace
        .iter()
        .enumerate()
        .filter(|(_, line)| is_paste(line, "keep going"))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    assert_eq!(pastes.len(), 1, "{trace:?}");
    assert!(command_at < pastes[0], "{trace:?}");
    assert_eq!(
        trace.iter().filter(|line| is_compact_command(line)).count(),
        1
    );
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Sent
    );
}

#[test]
fn a_request_behind_a_waiting_compaction_joins_it_and_one_compaction_runs_before_its_continue() {
    let fixture = Fixture::new("claude");
    fixture.compact(&["--no-continue"]);
    let command = fixture.records()[0].clone();
    assert_eq!(
        (command.body, command.status),
        (MessageBody::Command, MessageStatus::Queued)
    );
    fixture.send(SoftCompactStep::WrapUp);
    assert_eq!(
        fixture.reminders(),
        1,
        "a waiting command no longer withholds the reminder"
    );
    for _ in 0..2 {
        let receipt = fixture.compact(&[]);
        assert_eq!(
            receipt.trim_end(),
            format!(
                "compaction {} is already queued for @claude; delivers at its next turn boundary, then continues it",
                command.message_id
            )
        );
    }
    let records = fixture.records();
    assert_eq!(
        records
            .iter()
            .filter(|record| record.body == MessageBody::Command)
            .count(),
        1,
        "{records:?}"
    );
    let continues: Vec<_> = records
        .iter()
        .filter(|record| record.soft_compact == Some(SoftCompactStep::Continue))
        .collect();
    assert_eq!(continues.len(), 1, "{records:?}");
    let continuation = continues[0].clone();
    assert_eq!(
        continuation.follows_command.as_ref(),
        Some(&command.message_id)
    );
    assert!(
        fixture
            .agent()
            .soft_compact
            .is_some_and(|mark| mark.continue_owed.is_some())
    );

    let refused = traced_rimz(&fixture.env, "soft-trace.log")
        .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
        .args(["agents", "compact", "@claude", "keep the plan"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(
        stderr.contains(&format!(
            "@claude: compaction {} is already queued and has not been typed; an instruction cannot change its text — rerun without the instruction to join it",
            command.message_id
        )) && !stderr.contains("never follows a compaction"),
        "{stderr}"
    );
    assert_eq!(fixture.records().len(), records.len());

    fixture.end_turn();
    fixture.deliver(&command.message_id);
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(compaction_end());
    fixture.context(20_000);
    fixture.deliver(&continuation.message_id);
    let trace = fixture.trace();
    let commands: Vec<_> = trace
        .iter()
        .enumerate()
        .filter(|(_, line)| is_compact_command(line))
        .map(|(index, _)| index)
        .collect();
    let pastes: Vec<_> = trace
        .iter()
        .enumerate()
        .filter(|(_, line)| is_paste(line, "keep going"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!((commands.len(), pastes.len()), (1, 1), "{trace:?}");
    assert!(commands[0] < pastes[0], "{trace:?}");
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Sent
    );
}

#[test]
fn self_compaction_with_no_continue_or_blank_text_queues_the_command_alone() {
    for blank in [false, true] {
        let fixture = Fixture::new("claude");
        if blank {
            run_success(
                fixture
                    .env
                    .rimz()
                    .args(["config", "set", "resume.auto_continue_text", "  "]),
                "blank continue",
            );
        }
        let receipt = fixture.compact(if blank { &[] } else { &["--no-continue"] });
        assert!(
            receipt
                .trim_end()
                .ends_with("delivers at its next turn boundary, with no continue after"),
            "{receipt}"
        );
        let records = fixture.records();
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(records[0].body, MessageBody::Command);
        assert!(
            !fixture
                .agent()
                .soft_compact
                .is_some_and(|mark| mark.continue_owed.is_some())
        );
    }
}

#[test]
fn owed_continue_is_withdrawn_once_its_row_stops_owing_it() {
    enum Takeover {
        Signal(LifecycleSignal),
        UserPrompt,
        ClaudeEsc,
    }
    for after_claim in [false, true] {
        for takeover in [
            Takeover::Signal(LifecycleSignal::TurnInterrupted { turn_id: None }),
            Takeover::Signal(LifecycleSignal::Registered),
            Takeover::UserPrompt,
            Takeover::ClaudeEsc,
        ] {
            let fixture = Fixture::new("claude");
            fixture.end_turn();
            fixture.context(20_000);
            let store = fixture.env.store();
            let agent = fixture.agent();
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
            let take_over = || match &takeover {
                Takeover::Signal(signal) => fixture.lifecycle(signal.clone()),
                Takeover::UserPrompt => {
                    fixture.lifecycle_with_prompt(
                        LifecycleSignal::TurnStarted { turn_id: None },
                        "do this instead",
                    );
                    fixture.end_turn();
                }
                Takeover::ClaudeEsc => fixture.claude_esc(),
            };
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
                take_over();
                release.write_all(&[1]).unwrap();
                assert!(child.wait_with_output().unwrap().status.success());
            } else {
                take_over();
                fixture.deliver(&continuation.message_id);
            }
            fixture.assert_canceled_with(
                &continuation.message_id,
                "compaction continue cleared before delivery",
            );
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
fn a_canceled_compactions_continue_leaves_the_next_compactions_continue_owed() {
    let fixture = Fixture::new("claude");
    fixture.compact(&[]);
    let (first_command, first_continue) = fixture.command_and_continue();
    run_success(
        fixture
            .env
            .rimz()
            .args(["message", "cancel", first_command.message_id.as_str()]),
        "cancel the first compaction",
    );
    fixture.compact(&[]);
    let records = fixture.records();
    let pending = |body: MessageBody| {
        records
            .iter()
            .find(|record| {
                record.body == body
                    && record.status == MessageStatus::Queued
                    && record.message_id > first_continue.message_id
            })
            .expect("the second compaction's records")
            .clone()
    };
    let (command, continuation) = (pending(MessageBody::Command), pending(MessageBody::Prompt));
    fixture.end_turn();
    fixture.deliver(&first_continue.message_id);
    assert_eq!(
        fixture.status_of(&first_continue.message_id),
        MessageStatus::Canceled,
        "the canceled compaction's continue is owed nothing"
    );
    fixture.deliver(&command.message_id);
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(compaction_end());
    fixture.context(20_000);
    fixture.deliver(&continuation.message_id);
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Sent,
        "the older continue's withdrawal leaves this one owed"
    );
    assert_eq!(
        fixture
            .trace()
            .iter()
            .filter(|line| is_paste(line, "keep going"))
            .count(),
        1
    );
}

#[test]
fn owed_continue_survives_a_harness_notice_and_is_withdrawn_with_an_unsent_command() {
    let fixture = Fixture::new("claude");
    fixture.compact(&[]);
    let (command, continuation) = fixture.command_and_continue();
    fixture.lifecycle_with_prompt(
        LifecycleSignal::TurnStarted { turn_id: None },
        "Type: WAIT\nFrom: @rimz\nContent:\nwaited on `cargo test`",
    );
    assert!(
        fixture
            .agent()
            .soft_compact
            .is_some_and(|mark| mark.continue_owed.is_some()),
        "a harness notice is not a takeover"
    );
    run_success(
        fixture
            .env
            .rimz()
            .args(["message", "cancel", command.message_id.as_str()]),
        "cancel the compaction",
    );
    fixture.end_turn();
    fixture.deliver(&continuation.message_id);
    fixture.assert_canceled_with(
        &continuation.message_id,
        &format!(
            "compaction {} canceled before it was typed",
            command.message_id
        ),
    );
    assert!(
        !fixture
            .trace()
            .iter()
            .any(|line| is_paste(line, "keep going"))
    );
}

/// A compaction the agent ran itself inside its open turn reaches the goal of the command
/// still queued for it and of the continue behind it: both are withdrawn, neither typed.
#[test]
fn a_queued_compaction_and_its_continue_are_withdrawn_once_the_agent_compacts_on_its_own() {
    let fixture = Fixture::new("claude");
    fixture.compact(&[]);
    let (command, continuation) = fixture.command_and_continue();
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(LifecycleSignal::CompactionEnded {
        auto: Some(true),
        failed: false,
    });
    fixture.context(20_000);
    fixture.end_turn();
    for id in [&command.message_id, &continuation.message_id] {
        if fixture.status_of(id) == MessageStatus::Queued {
            fixture.deliver(id);
        }
    }
    fixture.assert_canceled_with(
        &command.message_id,
        "agent compacted before this compaction was typed",
    );
    fixture.assert_canceled_with(
        &continuation.message_id,
        "compaction continue cleared before delivery",
    );
    let trace = fixture.trace();
    assert!(
        !trace
            .iter()
            .any(|line| is_compact_command(line) || is_paste(line, "keep going")),
        "{trace:?}"
    );
}

/// A continue, joined to its command or queued with it, is withdrawn once a delivered prompt
/// starts a turn after the compaction; a keepalive ping is not the agent working.
#[test]
fn a_turn_started_after_the_compaction_withdraws_its_continue_and_a_keepalive_does_not() {
    for keepalive in [false, true] {
        let fixture = Fixture::new("claude");
        fixture.compact(&["--no-continue"]);
        fixture.compact(&[]);
        let (command, continuation) = fixture.command_and_continue();
        fixture.end_turn();
        fixture.deliver(&command.message_id);
        fixture.lifecycle(LifecycleSignal::Compacting);
        fixture.lifecycle(compaction_end());
        fixture.context(20_000);
        let prompt = if keepalive {
            "Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nKeep warm."
        } else {
            "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nthe review is in"
        };
        fixture.lifecycle_with_prompt(LifecycleSignal::TurnStarted { turn_id: None }, prompt);
        fixture.end_turn();
        if fixture.status_of(&continuation.message_id) == MessageStatus::Queued {
            fixture.deliver(&continuation.message_id);
        }
        let pastes = fixture
            .trace()
            .iter()
            .filter(|line| is_paste(line, "keep going"))
            .count();
        if keepalive {
            assert_eq!(
                (fixture.status_of(&continuation.message_id), pastes),
                (MessageStatus::Sent, 1)
            );
        } else {
            fixture.assert_canceled_with(
                &continuation.message_id,
                "compaction continue cleared before delivery",
            );
            assert_eq!(pastes, 0);
        }
    }
}

#[test]
fn supervised_run_parks_until_the_compaction_continue_turn_finishes() {
    use rimz::harness::run;
    use rimz::store::run::{RunRecord, RunStatus};

    for withdrawn in [false, true] {
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
        fixture.compact(&[]);
        let (command, continuation) = fixture.command_and_continue();
        settle(end(), "note before compaction");
        let parked = run::load(store.paths(), &record.run_id).unwrap();
        assert_eq!(
            parked.status,
            RunStatus::Running,
            "the turn before the compaction is not the answer"
        );
        assert!(parked.parked_at.is_some());
        assert_eq!(
            run::settle_stranded_park(&store, &parked, parked.parked_at).unwrap(),
            run::ParkCheck::Live
        );
        if withdrawn {
            fixture.lifecycle(LifecycleSignal::TurnInterrupted { turn_id: None });
            let parked = run::load(store.paths(), &record.run_id).unwrap();
            assert_eq!(
                run::settle_stranded_park(&store, &parked, parked.parked_at).unwrap(),
                run::ParkCheck::Settled,
                "a cleared continue owes the run nothing"
            );
            let settled = run::load(store.paths(), &record.run_id).unwrap();
            assert_eq!(
                (settled.status, settled.last_message.as_deref()),
                (RunStatus::Completed, Some("note before compaction")),
                "a withdrawn continue leaves the parked turn's answer as the finish"
            );
            continue;
        }
        fixture.deliver(&command.message_id);
        fixture.lifecycle(LifecycleSignal::Compacting);
        fixture.lifecycle(compaction_end());
        fixture.context(20_000);
        fixture.deliver(&continuation.message_id);
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
}

#[test]
fn codex_continuation_receives_the_owed_continue_and_rearms_the_reminder() {
    let mut fixture = Fixture::new("codex");
    fixture.send(SoftCompactStep::WrapUp);
    fixture.compact(&[]);
    let records = fixture.records();
    let command = records
        .iter()
        .find(|record| record.body == MessageBody::Command)
        .unwrap()
        .clone();
    let continuation = records
        .iter()
        .find(|record| record.soft_compact == Some(SoftCompactStep::Continue))
        .unwrap()
        .clone();
    fixture.end_turn();
    fixture.deliver(&command.message_id);
    assert!(
        fixture
            .trace()
            .iter()
            .any(|line| line.ends_with("\t--\t/compact")),
        "non-trailing adapter gets the bare command: {:?}",
        fixture.trace()
    );
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
    assert!(
        fixture
            .agent()
            .soft_compact
            .is_some_and(|mark| mark.continue_owed.is_some()),
        "the successor inherits the owed continue"
    );
    fixture.context(20_000);
    fixture.deliver(&continuation.message_id);
    assert_eq!(
        fixture.status_of(&continuation.message_id),
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
            .any(|record| record.message_id == continuation.message_id)
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
        "the compaction that created this row must re-arm its reminder"
    );
}

#[test]
fn reminder_skips_an_input_prompt_raised_while_it_waited_for_the_pane_lock() {
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
        extra_questions: 0,
        kind: AskKind::Permission,
        ask_id: None,
        detail: None,
        native_key: None,
    });
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        fixture.trace().iter().all(|line| is_mux_read(line)),
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
fn an_unreadable_queue_fails_the_reminder() {
    let fixture = Fixture::new("claude");
    let messages_dir = fixture.env.store().paths().messages_dir.clone();
    std::fs::create_dir_all(&messages_dir).unwrap();
    std::fs::write(messages_dir.join("messages.jsonl"), "not json\n").unwrap();
    let output = fixture.command(SoftCompactStep::WrapUp).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        stderr.contains("sending smart-compaction reminder") && stderr.contains("messages.jsonl"),
        "{stderr}"
    );
}

#[test]
fn reminder_write_failure_cancels_the_miss_and_records_one_failed_assist() {
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
