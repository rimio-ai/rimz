use super::*;
use rimz::harness::soft_compact::{ReminderVerdict, arm, reminder_due};
use rimz::store::event::{SOFT_COMPACT_CLAIMED_METHOD, SoftCompactClaim};
use rimz::store::message::{AutoCompact, SoftCompactStep};

/// The card the fixture drives.
struct Card {
    workspace_id: rimz::ids::WorkspaceId,
    kind: AgentKind,
    agent_id: AgentSessionId,
    pane_id: PaneId,
    label: String,
}

struct Fixture {
    env: Env,
    request: Card,
    panes: PathBuf,
    continue_text: std::cell::RefCell<String>,
}

const REMINDER_LINE: &str = "Your context is getting high.";

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
        let request = Card {
            workspace_id: env.workspace_id.clone(),
            kind: AgentKind::new_unchecked(kind),
            agent_id: "soft-session".into(),
            pane_id: PaneId::from_parts(MuxName::Zellij, TRACE_PANE),
            label: "@worker".into(),
        };
        let program = rimz::agents::spec_by_kind(kind)
            .and_then(|spec| spec.launch.program)
            .unwrap_or(kind);
        let session = env.resolve_workspace(&env.project_root).session_name;
        let mut pane = agent_pane(&env, program);
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
            continue_text: std::cell::RefCell::new("keep going".to_owned()),
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

    fn lifecycle_usage(&self, signal: LifecycleSignal, tokens: u64) {
        append_lifecycle(
            &self.env,
            self.request.kind.as_str(),
            "soft-test",
            self.request.agent_id.as_str(),
            signal,
            |observation| {
                observation.pane_id = Some(self.request.pane_id.clone());
                observation.usage.context_window = Some(200_000);
                observation.usage.total_tokens = Some(tokens);
                observation.usage.fresh_input_tokens = Some(tokens);
                observation.usage.cache_read_input_tokens = Some(0);
                observation.usage.cache_write_input_tokens = Some(0);
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

    /// One host pass's reminder decision over the published snapshot at the fixture's 100k line.
    /// When a rung is due it arms it, and the agent's next root hook-context reply must carry it.
    fn remind(&self) -> bool {
        let session = self
            .env
            .resolve_workspace(&self.env.project_root)
            .session_name;
        let snapshot = rimz::sidebar::consumer::PublishedSnapshotReader::new(
            self.env.runtime_paths(),
            session,
            None,
        )
        .read(self.env.store().paths())
        .unwrap();
        let agent = rimz::store::snapshot::find_agent(
            &snapshot.agents,
            &self.request.kind,
            &self.request.agent_id,
        )
        .unwrap();
        let threshold = AutoCompact::Tokens(100_000);
        let ReminderVerdict::Due(rung) = reminder_due(
            &snapshot,
            agent,
            Some(threshold),
            &self.continue_text.borrow(),
            jiff::Timestamp::now(),
            || false,
        ) else {
            return false;
        };
        let claim = SoftCompactClaim {
            agent_id: self.request.agent_id.clone(),
            compactions: agent.compaction_count,
            rung,
            threshold,
            label: Some(self.request.label.clone()),
            occupied_tokens: agent.occupied_context_tokens(),
        };
        assert!(arm(
            &self.env.runtime_paths(),
            &self.request.kind,
            None,
            &claim
        ));
        let reply = self.reply_hook();
        assert!(reply.contains(REMINDER_LINE), "{reply}");
        true
    }

    /// Arms rung 1 for the card as the host pass would, keyed by its `launch` id or, without
    /// one, by its session alone.
    fn arm_rung_one(&self, launch: Option<&str>) {
        let claim = SoftCompactClaim {
            agent_id: self.request.agent_id.clone(),
            compactions: 0,
            rung: 1,
            threshold: AutoCompact::Tokens(100_000),
            label: None,
            occupied_tokens: None,
        };
        assert!(arm(
            &self.env.runtime_paths(),
            &self.request.kind,
            launch.map(AgentSessionId::from).as_ref(),
            &claim
        ));
    }

    /// The pending reminder files armed and not yet claimed.
    fn pending(&self) -> Vec<PathBuf> {
        let dir = self.env.runtime_paths().live_dir.join("soft-compact");
        std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().ends_with("-pending.json"))
            .collect()
    }

    /// The card's root hook on the event its adapter's hook-context reply names, in a payload
    /// every built-in decoder reads, from a carrier that reads the reply.
    fn reply_hook_payload(&self) -> serde_json::Value {
        let event = rimz::agents::spec_by_kind(self.request.kind.as_str())
            .and_then(|spec| spec.capabilities.hook_context)
            .map_or("PostToolUse", |reply| reply.event());
        let session = self.request.agent_id.as_str();
        serde_json::json!({
            "hook_event_name": event,
            "hookEventName": event,
            "session_id": session,
            "conversationId": session,
            "cwd": self.env.project_root,
            "workspacePaths": [self.env.project_root],
            "invocationNum": 2,
            "tool_name": "Read",
            "tool_input": {"file_path": "src/lib.rs"},
            "tool_response": {},
            "reads_reply": true,
        })
    }

    /// The agent's root hook-context hook, returning its stdout reply.
    fn reply_hook(&self) -> String {
        self.hook_reply(&self.reply_hook_payload())
    }

    /// Feeds `payload` as the card's hook and returns its stdout reply.
    fn hook_reply(&self, payload: &serde_json::Value) -> String {
        let output = self
            .env
            .run_hook(self.request.kind.as_str(), &payload.to_string());
        assert!(output.status.success(), "{output:?}");
        let reply = String::from_utf8(output.stdout).unwrap();
        assert!(
            reply.lines().count() <= 1
                && reply
                    .lines()
                    .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok()),
            "hook stdout stays one JSON reply: {reply}"
        );
        reply
    }

    fn set_continue_text(&self, text: &str) {
        run_success(
            self.env
                .rimz()
                .args(["config", "set", "resume.auto_continue_text", text]),
            "set continue text",
        );
        self.continue_text.replace(text.to_owned());
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
        let output = run_success(
            self.person_compact(extra)
                .env("RIMZ_AGENT_KIND", self.request.kind.as_str())
                .env("RIMZ_AGENT_ID", self.request.agent_id.as_str()),
            "self compaction",
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// `rimz agents compact @<kind> <extra>` as a person runs it, outside any agent.
    fn person_compact(&self, extra: &[&str]) -> std::process::Command {
        let handle = format!("@{}", self.request.kind.as_str());
        let mut command = traced_rimz(&self.env, "soft-trace.log");
        command
            .env("RIMZ_TEST_PANE_LIST", &self.panes)
            .args(["agents", "compact", handle.as_str()])
            .args(extra);
        command
    }

    /// The handle receipts name the agent by.
    fn named(&self) -> String {
        rimz::address::handle_with_name(
            &format!("@{}", self.request.kind.as_str()),
            self.records()[0].agent_name.as_deref(),
        )
    }

    /// Rests the row as a finished compaction does and types the continue it owes.
    /// `auto` is the trigger the compaction's end reports.
    fn compact_and_continue(&self, continuation: &MessageRecord, auto: Option<bool>) {
        self.lifecycle(LifecycleSignal::Compacting);
        self.lifecycle(LifecycleSignal::CompactionEnded {
            auto,
            failed: false,
            echo: false,
        });
        self.context(20_000);
        self.deliver(&continuation.message_id);
        let trace = self.trace();
        let command_at = trace.iter().rposition(|line| is_compact_command(line));
        let pastes: Vec<_> = trace
            .iter()
            .enumerate()
            .filter(|(_, line)| is_paste(line, "keep going"))
            .map(|(index, _)| index)
            .collect();
        assert_eq!(pastes.len(), 1, "{trace:?}");
        assert!(command_at < Some(pastes[0]), "{trace:?}");
        assert_eq!(
            self.status_of(&continuation.message_id),
            MessageStatus::Sent
        );
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

    /// Reminders the agent's hooks claimed, as the apply side recorded them.
    fn reminders(&self) -> usize {
        let events = self.env.read_events();
        events
            .iter()
            .filter(|event| event.method == SOFT_COMPACT_CLAIMED_METHOD)
            .count()
    }

    fn reminder_rung(&self) -> Option<u8> {
        self.agent().soft_compact.and_then(|mark| mark.rung)
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

/// A person's hard request to a busy agent of a steering kind types the command alone now and
/// presses send-now; its continue stays queued past the stop and is typed once the compaction rests
/// the row, which a steered command does with no turn end, its compaction closing on the manual
/// trigger.
#[test]
fn a_hard_request_steers_the_command_into_a_busy_agent_and_continues_it_after() {
    for kind in ["claude", "antigravity"] {
        let fixture = Fixture::new(kind);
        let output = fixture.person_compact(&["--hard"]).output().unwrap();
        assert!(output.status.success(), "{kind}: {output:?}");
        let (command, continuation) = fixture.command_and_continue();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim_end(),
            format!(
                "compacting {} now ({}), then continues it if nothing else is waiting",
                fixture.named(),
                command.message_id
            )
        );
        assert_eq!(
            (command.status, continuation.status, continuation.past_stop),
            (MessageStatus::Sent, MessageStatus::Queued, true),
            "{kind}"
        );
        assert!(
            fixture
                .agent()
                .ends_turn_for_compaction(jiff::Timestamp::now()),
            "{kind}"
        );
        let trace = fixture.trace();
        let typed = trace
            .iter()
            .position(|line| is_compact_command(line))
            .unwrap_or_else(|| panic!("{kind}: no command typed: {trace:?}"));
        assert_eq!(keys_after(&trace, typed), send_now_keys(kind), "{kind}");
        assert!(!trace.iter().any(|line| is_paste(line, "keep going")));
        fixture.compact_and_continue(&continuation, Some(false));
    }
}

/// A person's hard request to a busy Codex presses its interrupt keys and types nothing until its
/// stop arrives, which records as a clean turn end; then it types the command and submits it with
/// the command keys, and the continue stays owed.
#[test]
fn a_hard_request_stops_a_busy_agent_then_types_the_command_and_owes_its_continue() {
    let fixture = Fixture::new("codex");
    trust_codex_preflight_hooks(&fixture.env);
    let started = Instant::now();
    let child = fixture
        .person_compact(&["--hard"])
        .env("RIMZ_MESSAGE_INTERRUPT_CONFIRM_MS", "10000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    while !fixture.trace().iter().any(|line| is_escape_key(line)) {
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        !fixture.trace().iter().any(|line| is_compact_command(line)),
        "nothing is typed before the stop"
    );
    let stop = json!({
        "hook_event_name": "Interrupt",
        "session_id": fixture.request.agent_id.as_str(),
        "cwd": fixture.env.project_root,
    });
    let hook = fixture.env.run_hook("codex", &stop.to_string());
    assert!(hook.status.success(), "{hook:?}");
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("interrupt not confirmed"),
        "{output:?}"
    );
    let trace = fixture.trace();
    let escape = trace.iter().position(|line| is_escape_key(line)).unwrap();
    let typed = trace.iter().position(|line| is_compact_command(line));
    assert!(typed > Some(escape), "{trace:?}");
    assert_eq!(keys_after(&trace, typed.unwrap()), ["13"], "{trace:?}");
    let (command, continuation) = fixture.command_and_continue();
    assert_eq!(
        (command.status, continuation.status, continuation.past_stop),
        (MessageStatus::Sent, MessageStatus::Queued, true)
    );
    let row = fixture.agent();
    assert!(
        row.ends_turn_for_compaction(jiff::Timestamp::now()) && !row.holds_open_turn(),
        "RimZ's stop is a clean turn end that keeps the debt: {row:?}"
    );
    fixture.compact_and_continue(&continuation, None);
}

/// A stop the interrupt window does not see still types the command; the continue typed after
/// it starts a turn that closes the debt, and a later stop is a person's again.
#[test]
fn a_hard_request_types_the_command_unseen_stop_and_its_debt_closes_with_the_continue() {
    let fixture = Fixture::new("codex");
    trust_codex_preflight_hooks(&fixture.env);
    let output = fixture
        .person_compact(&["--hard"])
        .env("RIMZ_MESSAGE_INTERRUPT_CONFIRM_MS", "200")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let trace = fixture.trace();
    let escape = trace.iter().position(|line| is_escape_key(line));
    let typed = trace.iter().position(|line| is_compact_command(line));
    assert!(escape.is_some() && typed > escape, "{trace:?}");
    assert_eq!(keys_after(&trace, typed.unwrap()), ["13"], "{trace:?}");
    let (_, continuation) = fixture.command_and_continue();
    assert!(
        fixture
            .agent()
            .ends_turn_for_compaction(jiff::Timestamp::now())
    );

    fixture.end_turn();
    fixture.compact_and_continue(&continuation, None);
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    assert!(
        !fixture
            .agent()
            .ends_turn_for_compaction(jiff::Timestamp::now())
    );
    let stop = json!({
        "hook_event_name": "Interrupt",
        "session_id": fixture.request.agent_id.as_str(),
        "cwd": fixture.env.project_root,
    });
    assert!(
        fixture
            .env
            .run_hook("codex", &stop.to_string())
            .status
            .success()
    );
    let stopped = fixture
        .env
        .store()
        .read_events()
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|event| match event.kind() {
            rimz::store::event::EventKind::AgentLifecycle(payload) => {
                Some(payload.observation.signal)
            }
            _ => None,
        });
    assert!(
        matches!(stopped, Some(LifecycleSignal::TurnInterrupted { .. })),
        "{stopped:?}"
    );
}

/// A hard request compacts at the turn end, naming why, where it cannot end the turn now: the
/// agent's own request, an agent waiting on an answer, no continue text. It queues what the soft
/// request queues. A resting agent gets the ordinary compaction and receipt.
#[test]
fn a_hard_request_falls_back_to_the_turn_end_and_names_why() {
    let queued = |fixture: &Fixture, receipt: &str, continues: bool, why: &str| {
        let lines: Vec<_> = receipt.lines().collect();
        let named = fixture.named();
        assert!(
            lines[0].starts_with(&format!("queued compaction for {named} (msg_"))
                && lines[0].ends_with(if continues {
                    "delivers at its next turn boundary, then continues it if nothing else is waiting"
                } else {
                    "delivers at its next turn boundary, with no continue after"
                }),
            "{receipt}"
        );
        assert_eq!(
            lines[1..],
            [format!("--hard did not end {named}'s turn: {why}")],
            "{receipt}"
        );
        let records = fixture.records();
        assert_eq!(records.len(), 1 + usize::from(continues), "{records:?}");
        assert!(
            records
                .iter()
                .all(|record| record.status == MessageStatus::Queued && !record.past_stop),
            "{records:?}"
        );
        assert!(
            !fixture
                .trace()
                .iter()
                .any(|line| is_escape_key(line) || is_compact_command(line))
        );
    };
    let own = Fixture::new("claude");
    let receipt = own.compact(&["--hard"]);
    queued(
        &own,
        &receipt,
        true,
        "an agent's own compaction waits for its turn to end",
    );
    let resting_own = Fixture::new("claude");
    resting_own.end_turn();
    let receipt = resting_own.compact(&["--hard"]);
    let records = resting_own.records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        receipt.trim_end(),
        format!(
            "compacting {} ({})",
            resting_own.named(),
            records[0].message_id
        ),
        "a resting agent's own request names no reason"
    );

    let waiting = Fixture::new("claude");
    waiting.lifecycle(LifecycleSignal::AwaitingInput {
        extra_questions: 0,
        kind: AskKind::Permission,
        ask_id: None,
        detail: None,
        native_key: None,
    });
    let output = waiting.person_compact(&["--hard"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    queued(
        &waiting,
        &String::from_utf8_lossy(&output.stdout),
        false,
        "it is waiting on an answer",
    );

    let blank = Fixture::new("claude");
    run_success(
        blank
            .env
            .rimz()
            .args(["config", "set", "resume.auto_continue_text", "  "]),
        "blank continue text",
    );
    let output = blank.person_compact(&["--hard"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    queued(
        &blank,
        &String::from_utf8_lossy(&output.stdout),
        false,
        "resume.auto_continue_text is empty, and ending a turn for a compaction owes a continue",
    );

    let resting = Fixture::new("claude");
    resting.end_turn();
    let output = resting.person_compact(&["--hard"]).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let records = resting.records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim_end(),
        format!("compacting {} ({})", resting.named(), records[0].message_id)
    );
    assert_eq!(records[0].status, MessageStatus::Sent);
}

/// A hard request that finds a compaction queued joins it, owes its continue past the stop (a
/// soft continue already queued is replaced), and makes its hard attempt on that command.
#[test]
fn a_hard_request_joins_a_queued_compaction_and_types_it_now() {
    for own_continue in [false, true] {
        let fixture = Fixture::new("claude");
        if own_continue {
            fixture.compact(&[]);
        } else {
            run_success(&mut fixture.person_compact(&[]), "soft compaction");
        }
        let queued = fixture.records();
        let command = queued
            .iter()
            .find(|record| record.body == MessageBody::Command)
            .unwrap()
            .clone();
        let output = fixture.person_compact(&["--hard"]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim_end(),
            format!(
                "compacting {} now ({}), then continues it if nothing else is waiting",
                fixture.named(),
                command.message_id
            )
        );
        let records = fixture.records();
        let commands: Vec<_> = records
            .iter()
            .filter(|record| record.body == MessageBody::Command)
            .collect();
        assert_eq!(commands.len(), 1, "{records:?}");
        assert_eq!(
            (&commands[0].message_id, commands[0].status),
            (&command.message_id, MessageStatus::Sent)
        );
        let mut continues: Vec<_> = records
            .iter()
            .filter(|record| record.soft_compact == Some(SoftCompactStep::Continue))
            .collect();
        continues.sort_by_key(|record| &record.message_id);
        let continues: Vec<_> = continues
            .iter()
            .map(|record| (record.status, record.past_stop))
            .collect();
        let mut expected = vec![(MessageStatus::Queued, true)];
        if own_continue {
            expected.insert(0, (MessageStatus::Canceled, false));
        }
        assert_eq!(continues, expected, "{records:?}");
        assert!(
            fixture
                .agent()
                .ends_turn_for_compaction(jiff::Timestamp::now())
        );
        let trace = fixture.trace();
        let typed = trace
            .iter()
            .position(|line| is_compact_command(line))
            .unwrap();
        assert_eq!(keys_after(&trace, typed), send_now_keys("claude"));
    }
}

fn compaction_end() -> LifecycleSignal {
    LifecycleSignal::CompactionEnded {
        auto: None,
        failed: false,
        echo: false,
    }
}

fn reminder_once_per_epoch(kind: &str) {
    let fixture = Fixture::new(kind);
    assert!(fixture.remind(), "past both lines, one reminder fires");
    assert_eq!(fixture.reminders(), 1);
    assert_eq!(
        fixture.reminder_rung(),
        Some(2),
        "a jump past both lines spends both rungs"
    );
    assert!(
        fixture.records().is_empty(),
        "the reminder never enters the queue: {:?}",
        fixture.records()
    );
    assert!(
        !fixture.reply_hook().contains(REMINDER_LINE),
        "a claimed reminder attaches once"
    );
    assert!(!fixture.remind(), "no second reminder in the epoch");
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(LifecycleSignal::CompactionEnded {
        auto: Some(true),
        failed: false,
        echo: false,
    });
    fixture.context(20_000);
    fixture.end_turn();
    assert!(
        !fixture.remind(),
        "nothing follows a reminder after a native compaction and a turn end"
    );
    let trace = fixture.trace();
    assert!(
        !trace
            .iter()
            .any(|line| is_compact_command(line) || line.contains(REMINDER_LINE)),
        "{trace:?}"
    );
    let stats = fixture.stats();
    assert_eq!(stats["rollup"]["soft_compacts"], 1);
    let events = stats["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["assist"] == "soft_compact")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 1, "{stats}");
    assert_eq!(
        (&events[0]["rung"], &events[0]["delivered"]),
        (&serde_json::json!(2), &serde_json::json!(true)),
        "{stats}"
    );
    fixture.lifecycle_usage(LifecycleSignal::TurnStarted { turn_id: None }, 20_000);
    fixture.context(105_000);
    assert!(fixture.remind(), "a new epoch re-arms the first rung");
    assert_eq!(fixture.reminder_rung(), Some(1));
    assert!(!fixture.remind(), "the first rung fires once");
    fixture.context(115_000);
    assert!(
        fixture.remind(),
        "ten percent past the line, the second rung fires"
    );
    assert_eq!(fixture.reminder_rung(), Some(2));
    assert!(!fixture.remind(), "the second rung fires once");
    assert_eq!(fixture.reminders(), 3);
    assert_eq!(fixture.stats()["rollup"]["soft_compacts"], 3);
}

#[test]
fn trailing_adapter_gets_one_reminder_per_epoch_and_nothing_after_it() {
    reminder_once_per_epoch("claude");
}

#[test]
fn non_trailing_adapter_gets_one_reminder_per_epoch_and_nothing_after_it() {
    reminder_once_per_epoch("codex");
}

/// Every built-in whose hook-context reply can carry the reminder takes it once, in that reply.
#[test]
fn every_hook_context_kind_carries_a_claimed_reminder_once() {
    let kinds: Vec<_> = rimz::agents::all_definitions()
        .filter(|definition| rimz::agents::reminder_skip(definition).is_none())
        .map(|definition| definition.spec().kind)
        .collect();
    assert!(kinds.contains(&"antigravity"), "{kinds:?}");
    for kind in kinds {
        let fixture = Fixture::new(kind);
        fixture.arm_rung_one(None);
        let reply = fixture.reply_hook();
        assert_eq!(reply.matches(REMINDER_LINE).count(), 1, "{kind}: {reply}");
        assert!(fixture.pending().is_empty(), "{kind}");
        assert!(!fixture.reply_hook().contains(REMINDER_LINE), "{kind}");
        assert_eq!(fixture.reminders(), 1, "{kind}");
    }
}

/// Antigravity carries both rungs in its `PreInvocation` reply, and a native grandchild's
/// conversation under the same launch claims nothing.
#[test]
fn antigravity_injects_each_rung_as_a_system_message_for_its_own_conversation() {
    reminder_once_per_epoch("antigravity");
    let fixture = Fixture::new("antigravity");
    fixture.arm_rung_one(Some("agy-launch"));
    let feed = |conversation: &str| {
        let mut payload = fixture.reply_hook_payload();
        payload.as_object_mut().unwrap().remove("session_id");
        payload["conversationId"] = serde_json::json!(conversation);
        let mut hook = fixture.env.hook_command("antigravity");
        hook.env(rimz::harness::launch::ENV_AGENT_ID, "agy-launch");
        let output = fixture
            .env
            .spawn_payload(hook, &payload.to_string())
            .wait_with_output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let armed = fixture.pending();
    assert_eq!(feed("native-grandchild"), serde_json::json!({}));
    assert_eq!(fixture.pending(), armed, "the reminder stays armed");
    let reply = feed(fixture.request.agent_id.as_str());
    let text = reply["injectSteps"][0]["systemMessage"]["systemMessage"]
        .as_str()
        .unwrap_or_default();
    assert!(text.contains(REMINDER_LINE), "{reply}");
    fixture.env.drain_hooks();
    assert_eq!(fixture.reminders(), 1);
}

#[test]
fn reminder_is_withheld_behind_an_owed_continue_blank_text_or_an_interrupt_but_not_a_clear() {
    let fixture = Fixture::new("claude");
    fixture.compact(&[]);
    assert!(
        !fixture.remind(),
        "a compaction owed a continue withholds the reminder"
    );
    let fixture = Fixture::new("claude");
    fixture.set_continue_text("  ");
    assert!(!fixture.remind(), "blank text withholds the reminder");

    let fixture = Fixture::new("claude");
    assert!(fixture.remind());
    fixture.lifecycle(LifecycleSignal::TurnInterrupted { turn_id: None });
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    assert!(!fixture.remind(), "an interrupt does not re-arm the epoch");
    fixture.lifecycle(LifecycleSignal::Registered);
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    assert!(fixture.remind(), "a /clear re-arms the reminder");
    assert_eq!(fixture.reminders(), 2);
}

#[test]
fn self_compaction_mid_turn_types_the_command_then_its_continue() {
    let fixture = Fixture::new("claude");
    assert!(fixture.remind());
    let receipt = fixture.compact(&["keep the plan"]);
    let named =
        rimz::address::handle_with_name("@claude", fixture.records()[0].agent_name.as_deref());
    assert!(
        receipt.starts_with(&format!("queued compaction for {named} (msg_"))
            && receipt.trim_end().ends_with(
                "delivers at its next turn boundary, then continues it if nothing else is waiting"
            ),
        "{receipt}"
    );
    let records = fixture.records();
    assert_eq!(records.len(), 2, "{records:?}");
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
    assert!(
        fixture.remind(),
        "a waiting command no longer withholds the reminder"
    );
    for _ in 0..2 {
        let receipt = fixture.compact(&[]);
        assert_eq!(
            receipt.trim_end(),
            format!(
                "compaction {} is already queued for {}; delivers at its next turn boundary, then continues it if nothing else is waiting",
                command.message_id,
                rimz::address::handle_with_name("@claude", command.agent_name.as_deref())
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
            "{}: compaction {} is already queued and has not been typed; an instruction cannot change its text — rerun without the instruction to join it",
            rimz::address::handle_with_name("@claude", fixture.agent().name.as_deref()),
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
                        continuation.message_id.as_str(),
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
        echo: false,
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
                None,
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
    assert!(fixture.remind());
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
            echo: false,
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
    assert!(
        fixture.remind(),
        "the compaction that created this row must re-arm its reminder"
    );
    assert_eq!(fixture.reminders(), 2);
}

#[test]
fn an_unparsable_pending_reminder_attaches_nothing_and_records_no_claim() {
    let fixture = Fixture::new("claude");
    fixture.arm_rung_one(None);
    let pending = fixture.pending();
    assert_eq!(pending.len(), 1, "{pending:?}");
    std::fs::write(&pending[0], "not json").unwrap();
    let reply = fixture.reply_hook();
    assert!(!reply.contains(REMINDER_LINE), "{reply}");
    assert_eq!(fixture.reminders(), 0);
    assert_eq!(fixture.stats()["rollup"]["soft_compacts"], 0);
}

/// OpenCode's shared-service client (`tui.ts`) discards the feed's stdout, so its `tool_after`
/// claims nothing; the server plugin (`index.ts`) marks the envelope whose reply it reads.
#[test]
fn opencode_claims_a_reminder_only_from_the_carrier_that_reads_the_reply() {
    let fixture = Fixture::new("opencode");
    fixture.arm_rung_one(None);
    let mut tool_after = serde_json::json!({
        "hook_event_name": "tool_after",
        "session_id": fixture.request.agent_id.as_str(),
        "cwd": fixture.env.project_root,
        "tool_name": "bash",
        "is_error": false,
    });
    let reply = fixture.hook_reply(&tool_after);
    assert!(!reply.contains(REMINDER_LINE), "{reply}");
    assert_eq!(fixture.pending().len(), 1, "the reminder stays armed");
    assert_eq!(fixture.reminders(), 0);
    assert_eq!(fixture.stats()["rollup"]["soft_compacts"], 0);
    tool_after["reads_reply"] = serde_json::json!(true);
    let reply = fixture.hook_reply(&tool_after);
    assert!(reply.contains(REMINDER_LINE), "{reply}");
    assert!(fixture.pending().is_empty());
    assert_eq!(fixture.reminders(), 1);
}

/// A hook the shared Codex daemon routes carries the daemon's environment, which belongs to
/// whichever launch started it: the claim goes by the payload's session, and the text names
/// only the command every session has.
#[test]
fn a_daemon_routed_hook_claims_by_session_and_reads_no_daemon_env() {
    #[cfg(not(target_os = "linux"))]
    {
        crate::common::skip("/proc recovery is Linux-only");
    }
    #[cfg(target_os = "linux")]
    {
        let fixture = Fixture::new("codex");
        fixture.arm_rung_one(None);
        let daemon = crate::common::spawn_sibling_codex(
            &fixture.env,
            &fixture.env.home_root,
            &["app-server"],
        );
        let mut hook = fixture.env.hook_command("codex");
        hook.current_dir(&fixture.env.project_root)
            .env("RIMZ_AGENT_PID", daemon.child.id().to_string())
            .env(rimz::harness::launch::ENV_AGENT_ID, "another-launch")
            .env(rimz::harness::launch::ENV_SELF_COMPACT_TOOL, "1");
        let output = fixture
            .env
            .spawn_payload(hook, &fixture.reply_hook_payload().to_string())
            .wait_with_output()
            .unwrap();
        fixture.env.drain_hooks();
        assert!(output.status.success(), "{output:?}");
        let reply = String::from_utf8(output.stdout).unwrap();
        assert!(reply.contains("`rimz agents compact @me`"), "{reply}");
        assert!(!reply.contains("self_compact"), "{reply}");
        assert!(fixture.pending().is_empty());
        assert_eq!(fixture.reminders(), 1);
    }
}

fn record_mutating_tool(fixture: &Fixture) {
    fixture.lifecycle(LifecycleSignal::ToolUsed {
        effect: rimz::agents::ToolEffect::Mutating,
        name: Some("Bash".into()),
        native_key: None,
        turn_id: None,
    });
}

fn assert_reminder_with_background_precompact(precompact: bool) {
    let fixture = Fixture::new("claude");
    assert!(fixture.remind());
    if precompact {
        fixture.lifecycle(LifecycleSignal::Compacting);
    }
    record_mutating_tool(&fixture);
    fixture.context(130_000);
    assert!(!fixture.remind());
    assert_eq!(fixture.reminders(), 1);
    assert_eq!(fixture.agent().compaction_count, 0);
}

fn assert_queued_compaction_with_background_precompact(precompact: bool) {
    let fixture = Fixture::new("claude");
    fixture.compact(&[]);
    let (command, continuation) = fixture.command_and_continue();
    if precompact {
        fixture.lifecycle(LifecycleSignal::Compacting);
    }
    record_mutating_tool(&fixture);
    fixture.context(130_000);
    assert!(
        fixture
            .agent()
            .soft_compact
            .is_some_and(|mark| mark.continue_owed.is_some()),
        "owed continue must survive a false PreCompact and tool report"
    );
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Queued
    );
    fixture.end_turn();
    // A harness-delivered turn start before any real compaction must keep the
    // owed continue intact (a false `compacted_at` would clear it).
    fixture.lifecycle_with_prompt(
        LifecycleSignal::TurnStarted { turn_id: None },
        "Type: AGENT_MESSAGE\nFrom: @planner\nContent:\nthe review is in",
    );
    assert!(
        fixture
            .agent()
            .soft_compact
            .is_some_and(|mark| mark.continue_owed.is_some()),
        "owed continue must survive the next harness-delivered turn start"
    );
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Queued
    );
    fixture.end_turn();
    fixture.deliver(&command.message_id);
    assert!(fixture.trace().iter().any(|line| is_compact_command(line)));
}

#[test]
fn reminder_does_not_repeat_after_background_precompact() {
    assert_reminder_with_background_precompact(true);
}

#[test]
fn reminder_does_not_repeat_without_background_precompact() {
    assert_reminder_with_background_precompact(false);
}

#[test]
fn queued_compaction_and_owed_continue_survive_background_precompact() {
    assert_queued_compaction_with_background_precompact(true);
}

#[test]
fn queued_compaction_and_owed_continue_without_background_precompact() {
    assert_queued_compaction_with_background_precompact(false);
}

#[test]
fn fill_guard_withholds_repeat_reminder_until_lifecycle_reports_low_fill_while_clear_rearms_immediately()
 {
    let fixture = Fixture::new("claude");
    assert!(fixture.remind());

    // Count moves to 1, sidecar drops then crosses 100k again, but no lifecycle event reports a
    // low fill: the fill guard withholds the repeat reminder.
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(LifecycleSignal::CompactionEnded {
        auto: Some(true),
        failed: false,
        echo: false,
    });
    fixture.context(20_000);
    fixture.context(130_000);
    assert!(
        !fixture.remind(),
        "a moved count with no fold low-water reading must withhold"
    );

    // A lifecycle reading at or over the 100k line still withholds.
    fixture.lifecycle_usage(
        LifecycleSignal::ToolUsed {
            effect: rimz::agents::ToolEffect::Observed,
            name: Some("Read".to_owned()),
            turn_id: None,
            native_key: None,
        },
        100_000,
    );
    assert!(
        !fixture.remind(),
        "a low-water reading at the line must withhold"
    );

    // Once a lifecycle event reports a fill under 100k, the guard admits the repeat reminder.
    fixture.lifecycle_usage(
        LifecycleSignal::ToolUsed {
            effect: rimz::agents::ToolEffect::Observed,
            name: Some("Read".to_owned()),
            turn_id: None,
            native_key: None,
        },
        20_000,
    );
    fixture.lifecycle_usage(
        LifecycleSignal::ToolUsed {
            effect: rimz::agents::ToolEffect::Observed,
            name: Some("Read".to_owned()),
            turn_id: None,
            native_key: None,
        },
        130_000,
    );
    assert!(
        fixture.remind(),
        "a fold low-water reading under the line must re-arm the reminder"
    );
    assert_eq!(fixture.reminders(), 2);

    // `/clear` drops the mark and re-arms immediately without needing a low-water reading.
    fixture.lifecycle(LifecycleSignal::Registered);
    fixture.lifecycle(LifecycleSignal::TurnStarted { turn_id: None });
    fixture.context(130_000);
    assert!(
        fixture.remind(),
        "/clear drops the mark and re-arms without the fill guard"
    );
    assert_eq!(fixture.reminders(), 3);
}

#[test]
fn next_prompt_kind_folds_command_and_continue_at_turn_end_and_on_steer() {
    // 1. Turn-end delivery on antigravity with multi-line continue text folds onto one typed line,
    //    records both Sent with batch_id None and the folded text on the tail, and presses Enter
    //    alone (never send_now).
    {
        let fixture = Fixture::new("antigravity");
        run_success(
            fixture.env.rimz().args([
                "config",
                "set",
                "resume.auto_continue_text",
                "  keep going\n\n  on step 2  ",
            ]),
            "set multi-line continue text",
        );
        fixture.compact(&[]);
        let (command, continuation) = fixture.command_and_continue();
        fixture.end_turn();
        fixture.deliver(&command.message_id);

        let trace = fixture.trace();
        assert_compact_segments_then_enter(&trace, "keep going on step 2");
        let write_prefix = format!("\taction\twrite-chars\t--pane-id\t{TRACE_PANE}\t--\t");
        let tail_at = trace
            .iter()
            .position(|line| line.ends_with(&format!("{write_prefix}keep going on step 2")))
            .expect("folded continue typed");
        assert_eq!(
            keys_after(&trace, tail_at),
            vec!["13".to_owned()],
            "boundary folded command presses Enter only, never Alt+M: {trace:?}"
        );
        assert!(
            !trace.iter().any(|line| is_paste(line, "keep going")),
            "folded continue is typed on the command line, not pasted: {trace:?}"
        );
        let records = fixture.records();
        let sent_cmd = records
            .iter()
            .find(|r| r.message_id == command.message_id)
            .unwrap();
        let sent_tail = records
            .iter()
            .find(|r| r.message_id == continuation.message_id)
            .unwrap();
        assert_eq!(sent_cmd.status, MessageStatus::Sent);
        assert_eq!(sent_cmd.text, "/compact");
        assert_eq!(sent_cmd.batch_id, None);
        assert_eq!(sent_tail.status, MessageStatus::Sent);
        assert_eq!(sent_tail.text, "keep going on step 2");
        assert_eq!(sent_tail.batch_id, None);
    }

    // 2. An interrupt before turn end withdraws the continue and types bare `/compact` + Enter.
    {
        let fixture = Fixture::new("antigravity");
        fixture.compact(&[]);
        let (command, continuation) = fixture.command_and_continue();
        fixture.lifecycle(LifecycleSignal::TurnInterrupted { turn_id: None });
        fixture.deliver(&command.message_id);

        let trace = fixture.trace();
        let write_prefix = format!("\taction\twrite-chars\t--pane-id\t{TRACE_PANE}\t--\t");
        let bare_at = trace
            .iter()
            .position(|line| line.ends_with(&format!("{write_prefix}/compact")))
            .unwrap_or_else(|| panic!("expected bare /compact write: {trace:?}"));
        assert_eq!(keys_after(&trace, bare_at), vec!["13".to_owned()]);
        assert_eq!(fixture.status_of(&command.message_id), MessageStatus::Sent);
        assert_eq!(
            fixture.status_of(&continuation.message_id),
            MessageStatus::Canceled
        );
    }

    // 3. Steering a queued compaction with a live tail into an open turn types the folded line
    //    and presses send_now (Enter then Alt+M on Antigravity).
    {
        let fixture = Fixture::new("antigravity");
        fixture.compact(&[]);
        let (command, continuation) = fixture.command_and_continue();
        run_success(
            traced_rimz(&fixture.env, "soft-trace.log")
                .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
                .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
                .args(["message", "steer", command.message_id.as_str()]),
            "steer queued compaction",
        );
        let trace = fixture.trace();
        let write_prefix = format!("\taction\twrite-chars\t--pane-id\t{TRACE_PANE}\t--\t");
        assert!(
            trace
                .iter()
                .any(|line| line.ends_with(&format!("{write_prefix}/compact "))),
            "expected command token segment: {trace:?}"
        );
        let tail_at = trace
            .iter()
            .position(|line| line.ends_with(&format!("{write_prefix}keep going")))
            .unwrap_or_else(|| panic!("expected folded continue segment: {trace:?}"));
        assert_eq!(
            keys_after(&trace, tail_at),
            vec!["13".to_owned(), "27\t109".to_owned()],
            "steered folded compaction into an open turn presses send_now: {trace:?}"
        );
        assert_eq!(fixture.status_of(&command.message_id), MessageStatus::Sent);
        assert_eq!(
            fixture.status_of(&continuation.message_id),
            MessageStatus::Sent
        );
    }

    // 4. An intervening prompt between command and tail keeps FIFO order: bare `/compact` first.
    {
        let fixture = Fixture::new("antigravity");
        fixture.compact(&["--no-continue"]);
        let command = fixture
            .records()
            .into_iter()
            .find(|r| r.body == MessageBody::Command)
            .unwrap();
        let peer = MessageRecord::new(
            fixture.env.workspace_id.clone(),
            &fixture.agent(),
            "peer update".to_owned(),
            DeliveryGate::Done,
        );
        fixture
            .env
            .store()
            .queue_message(&peer, "soft-test")
            .unwrap();
        fixture.compact(&[]);
        let continuation = fixture
            .records()
            .into_iter()
            .find(|r| r.soft_compact == Some(SoftCompactStep::Continue))
            .unwrap();
        assert!(command.message_id < peer.message_id && peer.message_id < continuation.message_id);

        fixture.end_turn();
        fixture.deliver(&command.message_id);
        let trace = fixture.trace();
        let write_prefix = format!("\taction\twrite-chars\t--pane-id\t{TRACE_PANE}\t--\t");
        assert!(
            trace
                .iter()
                .any(|line| line.ends_with(&format!("{write_prefix}/compact"))),
            "expected bare /compact ahead of intervening peer: {trace:?}"
        );
        assert_eq!(fixture.status_of(&command.message_id), MessageStatus::Sent);
        assert_eq!(fixture.status_of(&peer.message_id), MessageStatus::Queued);
        assert_eq!(
            fixture.status_of(&continuation.message_id),
            MessageStatus::Queued
        );
    }
}

#[test]
fn ceiling_compact_helper_enforces_or_withdraws_pair_and_records_assist() {
    let stamp_ceiling = |fixture: &Fixture, ceiling: u64| {
        let workspace = fixture.env.resolve_workspace(&fixture.env.project_root);
        let owner = rimz::store::runtime::process_owner(
            rimz::pane::RuntimeOwnerKind::Agent,
            fixture.request.agent_id.as_str(),
            fixture.env.agent_owner_pid(),
        );
        fixture
            .env
            .store()
            .append_event(&EventEnvelope::agent_attached(
                workspace.workspace_id,
                workspace.session_name,
                &fixture.request.kind,
                rimz::store::event::AgentAttachPayload {
                    record: Some(rimz::agents::LaunchRecord {
                        auto_compact: Some(ceiling),
                        ..Default::default()
                    }),
                    tier: None,
                    mode: None,
                    agent_id: fixture.request.agent_id.clone(),
                    isolation: None,
                    effective_isolation: None,
                    launch_id: None,
                    epoch: Some(0),
                    login: None,
                    pane_id: fixture.request.pane_id.clone(),
                    pane_pid: None,
                    runtime_owner: owner,
                },
            ))
            .unwrap();
    };
    let ceiling_cmd = |fixture: &Fixture| {
        let request = rimz::harness::soft_compact::CeilingCompactRequest {
            workspace_id: fixture.request.workspace_id.clone(),
            kind: fixture.request.kind.clone(),
            agent_id: fixture.request.agent_id.clone(),
            pane_id: fixture.request.pane_id.clone(),
            label: fixture.request.label.clone(),
        };
        let mut command = traced_rimz(&fixture.env, "soft-trace.log");
        command
            .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
            .args(rimz::child_process::agent_helper_argv(
                "ceiling-compact",
                &request,
            ));
        command
    };

    // 1. Due on fresh state: queues and steers folded line with send_now, records Sent on both,
    //    attaches OverCeiling premise, and logs a delivered ceiling_compact assist under compacts.
    {
        let fixture = Fixture::new("antigravity");
        stamp_ceiling(&fixture, 100_000);
        run_success(&mut ceiling_cmd(&fixture), "ceiling compact helper");

        let trace = fixture.trace();
        let write_prefix = format!("\taction\twrite-chars\t--pane-id\t{TRACE_PANE}\t--\t");
        assert!(
            trace
                .iter()
                .any(|line| line.ends_with(&format!("{write_prefix}/compact "))),
            "expected command segment: {trace:?}"
        );
        let tail_at = trace
            .iter()
            .position(|line| line.ends_with(&format!("{write_prefix}keep going")))
            .unwrap_or_else(|| panic!("expected folded continue segment: {trace:?}"));
        assert_eq!(
            keys_after(&trace, tail_at),
            vec!["13".to_owned(), "27\t109".to_owned()],
            "enforced ceiling steer presses send_now: {trace:?}"
        );

        let (command, continuation) = fixture.command_and_continue();
        assert_eq!(command.status, MessageStatus::Sent);
        assert_eq!(continuation.status, MessageStatus::Sent);
        assert!(command.automated);
        assert_eq!(
            command.premises,
            vec![rimz::store::message::Premise::OverCeiling { threshold: 100_000 }]
        );

        let stats = fixture.stats();
        assert_eq!(stats["rollup"]["compacts"], 1, "{stats}");
        let ceiling_events: Vec<_> = stats["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["assist"] == "ceiling_compact")
            .collect();
        assert_eq!(ceiling_events.len(), 1, "{stats}");
        assert_eq!(ceiling_events[0]["delivered"], true);
        assert_eq!(ceiling_events[0]["occupied_tokens"], 120_000);
    }

    // 2. Queued hand-off compaction command withholds enforcement (no new command, no assist).
    {
        let fixture = Fixture::new("antigravity");
        stamp_ceiling(&fixture, 100_000);
        let hand_off = MessageRecord::new(
            fixture.env.workspace_id.clone(),
            &fixture.agent(),
            "/compact".to_owned(),
            DeliveryGate::Done,
        )
        .with_body(MessageBody::Command)
        .with_automated(true)
        .with_premises(vec![rimz::store::message::Premise::OverHandOffLimit {
            threshold: 100_000,
        }]);
        fixture
            .env
            .store()
            .queue_message(&hand_off, "soft-test")
            .unwrap();

        run_success(
            &mut ceiling_cmd(&fixture),
            "ceiling compact withheld by queued hand-off",
        );
        let records = fixture.records();
        assert_eq!(records.len(), 1, "no new command queued: {records:?}");
        assert_eq!(records[0].message_id, hand_off.message_id);
        assert_eq!(records[0].status, MessageStatus::Queued);
        assert!(fixture.trace().iter().all(|line| is_mux_read(line)));
        assert_eq!(fixture.stats()["events"].as_array().unwrap().len(), 0);
    }

    // 3. Inside 10-minute cool-down: withholds enforcement (no command, no assist).
    {
        let fixture = Fixture::new("antigravity");
        stamp_ceiling(&fixture, 100_000);
        fixture.lifecycle(LifecycleSignal::CompactionEnded {
            auto: Some(true),
            failed: false,
            echo: false,
        });
        fixture.context(120_000);
        run_success(
            &mut ceiling_cmd(&fixture),
            "ceiling compact withheld in cooldown",
        );
        assert!(fixture.records().is_empty());
        assert!(fixture.trace().iter().all(|line| is_mux_read(line)));
        assert_eq!(fixture.stats()["events"].as_array().unwrap().len(), 0);
    }

    // 4. Premise broken before typing (turn ended OR context dropped below ceiling): withdraws
    //    both command and tail (neither left Queued), types nothing, and records a held assist.
    for drop_context in [false, true] {
        let fixture = Fixture::new("antigravity");
        stamp_ceiling(&fixture, 100_000);
        let before = DeliveryRendezvous::new(
            &fixture.env,
            if drop_context {
                "ceiling-drop-ctx"
            } else {
                "ceiling-turn-end"
            },
        );
        let child = ceiling_cmd(&fixture)
            .env("RIMZ_TEST_PANE_WRITE_BEFORE_LOCK", &before.path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut release = before.arrive();
        if drop_context {
            fixture.context(50_000);
        } else {
            fixture.end_turn();
        }
        release.write_all(&[1]).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            fixture.trace().iter().all(|line| is_mux_read(line)),
            "broken premise types nothing: {:?}",
            fixture.trace()
        );
        assert!(
            fixture.env.store().list_messages().unwrap().is_empty(),
            "neither command nor tail stays queued"
        );
        let (command, continuation) = fixture.command_and_continue();
        assert_eq!(command.status, MessageStatus::Canceled);
        assert_eq!(continuation.status, MessageStatus::Canceled);
        let stats = fixture.stats();
        assert_eq!(stats["rollup"]["compacts"], 0);
        assert_eq!(stats["rollup"]["held"]["compacts"], 1);
    }
}

#[test]
fn waiting_compaction_is_typed_ahead_of_older_queued_prompt_and_message_settles_continue() {
    // Antigravity folds a command with its continue only when nothing waits between the two.
    for (kind, with_compaction_hook, grown_fill) in [
        ("claude", true, None),
        ("claude", true, Some(121_771)),
        ("claude", false, None),
        ("antigravity", false, None),
    ] {
        let fixture = Fixture::new(kind);
        let workspace = fixture.env.resolve_workspace(&fixture.env.project_root);
        let agent = fixture.agent();
        let digest = rimz::message::synthetic::SyntheticMessage {
            agent: &agent,
            text: "All 2 subagents settled (probe digest)".to_owned(),
            sender: MessageSender::Harness {
                notice: rimz::store::message::HarnessNotice::SubagentReport,
            },
            gate: DeliveryGate::Done,
            premises: rimz::store::message::Premise::NONE,
        }
        .record(&workspace);
        let second_waiting = rimz::message::synthetic::SyntheticMessage {
            agent: &agent,
            text: "second waiting notice".to_owned(),
            sender: MessageSender::Harness {
                notice: rimz::store::message::HarnessNotice::SubagentReport,
            },
            gate: DeliveryGate::Done,
            premises: rimz::store::message::Premise::NONE,
        }
        .record(&workspace);
        fixture
            .env
            .store()
            .queue_message(&digest, &workspace.session_name)
            .unwrap();
        if with_compaction_hook && grown_fill.is_none() {
            fixture
                .env
                .store()
                .queue_message(&second_waiting, &workspace.session_name)
                .unwrap();
        }

        fixture.compact(&[]);
        let records = fixture.records();
        let command = records
            .iter()
            .find(|r| r.body == MessageBody::Command)
            .unwrap()
            .clone();
        let continuation = records
            .iter()
            .find(|r| r.soft_compact == Some(SoftCompactStep::Continue))
            .unwrap()
            .clone();
        assert!(command.message_id > digest.message_id);

        if with_compaction_hook && grown_fill.is_none() {
            // `rimz message show` for a record behind both an older prompt and a newer compaction
            // command names the compaction command — the record typed next.
            let shown = run_success(
                fixture
                    .env
                    .rimz()
                    .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
                    .args([
                        "message",
                        "show",
                        second_waiting.message_id.as_str(),
                        "--json",
                    ]),
                "message show behind compaction",
            );
            let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
            assert_eq!(
                shown["delivery"]["check"]["fifo"]["blocker"].as_str(),
                Some(command.message_id.as_str()),
                "show must name the compaction command as the blocker: {shown}"
            );
            assert!(
                shown["delivery"]["verdict"]
                    .as_str()
                    .is_some_and(|v| v.contains(command.message_id.as_str())),
                "verdict must name the compaction command: {shown}"
            );
            // Cancel the second notice so the rest of the test checks a single digest.
            run_success(
                fixture
                    .env
                    .rimz()
                    .args(["message", "cancel", second_waiting.message_id.as_str()]),
                "cancel extra notice",
            );
        }

        if let Some(tokens) = grown_fill {
            fixture.context(tokens);
        }
        fixture.end_turn();

        // Sweep picks the compaction command as the head and types it alone ahead of the digest.
        run_success(
            traced_rimz(&fixture.env, "soft-trace.log")
                .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
                .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
                .args(["message", "sweep"]),
            "sweep turn end with queued compaction",
        );
        let trace = fixture.trace();
        assert_eq!(
            trace.iter().filter(|l| is_compact_command(l)).count(),
            1,
            "compaction must be typed first: {trace:?}"
        );
        assert!(
            !trace.iter().any(
                |line| line.contains("\taction\twrite-chars\t") && line.ends_with("keep going")
            ),
            "the command is typed without its continue folded on: {trace:?}"
        );
        assert_eq!(fixture.status_of(&command.message_id), MessageStatus::Sent);
        assert_eq!(fixture.status_of(&digest.message_id), MessageStatus::Queued);
        assert_eq!(
            fixture.status_of(&continuation.message_id),
            MessageStatus::Queued
        );

        if with_compaction_hook {
            fixture.lifecycle(LifecycleSignal::Compacting);
            fixture.lifecycle(compaction_end());
            fixture.context(20_000);
        }

        // Next sweep delivers the waiting digest alone — never batched with the continue.
        run_success(
            traced_rimz(&fixture.env, "soft-trace.log")
                .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
                .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
                .args(["message", "sweep"]),
            "sweep waiting digest after compaction",
        );
        assert_eq!(fixture.status_of(&digest.message_id), MessageStatus::Sent);
        assert_eq!(
            fixture.status_of(&continuation.message_id),
            MessageStatus::Queued,
            "continue must not be batched with the waiting digest"
        );
        assert!(
            !fixture
                .trace()
                .iter()
                .any(|line| is_paste(line, "keep going")),
            "continue must not be pasted with the waiting digest"
        );

        // The digest's turn start clears the owed continue and withdraws it in the ack commit.
        let rendered_digest = format!(
            "Type: SUBAGENT_REPORT\nFrom: @rimz\nContent:\n{}",
            digest.text
        );
        fixture.lifecycle_with_prompt(
            LifecycleSignal::TurnStarted { turn_id: None },
            &rendered_digest,
        );
        let row = fixture.agent();
        fixture
            .env
            .store()
            .confirm_delivered_for_card_with(
                row.card_ref(),
                rimz::store::writer::DeliveryAck::TurnStarted {
                    prompt: Some(&rendered_digest),
                },
                Some(&row),
                row.epoch,
                &workspace.session_name,
                |_, _| {},
            )
            .unwrap();
        assert_eq!(
            fixture.status_of(&digest.message_id),
            MessageStatus::Delivered
        );
        fixture.assert_canceled_with(
            &continuation.message_id,
            "compaction continue cleared before delivery",
        );

        // At that turn's end, nothing else is typed.
        let trace_len = fixture.trace().len();
        fixture.end_turn();
        run_success(
            traced_rimz(&fixture.env, "soft-trace.log")
                .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
                .env("RIMZ_MESSAGE_SETTLE_MS", "0")
                .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
                .args(["message", "sweep"]),
            "sweep after digest turn ends",
        );
        assert!(
            fixture.trace()[trace_len..]
                .iter()
                .all(|line| !line.contains("\taction\twrite") && !is_paste(line, "keep going")),
            "nothing must be typed after the digest turn ends: {:?}",
            &fixture.trace()[trace_len..]
        );
    }
}

#[test]
fn prompts_around_a_queued_continue_batch_ahead_of_it_and_fresh_send_is_not_parked() {
    // 1. Prompts on both sides of a queued continue's id share one batch ahead of it.
    let fixture = Fixture::new("claude");
    let workspace = fixture.env.resolve_workspace(&fixture.env.project_root);
    let agent = fixture.agent();
    let store = fixture.env.store();
    let before = MessageRecord::new(
        fixture.env.workspace_id.clone(),
        &agent,
        "first waiting prompt".into(),
        DeliveryGate::Done,
    );
    store
        .queue_message(&before, &workspace.session_name)
        .unwrap();
    let mut continuation = MessageRecord::new(
        fixture.env.workspace_id.clone(),
        &agent,
        "keep going".into(),
        DeliveryGate::Done,
    );
    continuation.soft_compact = Some(SoftCompactStep::Continue);
    continuation.sender = MessageSender::System;
    store
        .queue_message(&continuation, &workspace.session_name)
        .unwrap();
    let after = MessageRecord::new(
        fixture.env.workspace_id.clone(),
        &agent,
        "second waiting prompt".into(),
        DeliveryGate::Done,
    );
    store
        .queue_message(&after, &workspace.session_name)
        .unwrap();
    assert!(before.message_id < continuation.message_id);
    assert!(continuation.message_id < after.message_id);

    fixture.end_turn();
    fixture.context(20_000);
    run_success(
        traced_rimz(&fixture.env, "soft-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
            .args(["message", "sweep"]),
        "sweep batch around continue",
    );
    assert_eq!(fixture.status_of(&before.message_id), MessageStatus::Sent);
    assert_eq!(fixture.status_of(&after.message_id), MessageStatus::Sent);
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Queued,
        "continue stays out of the batch"
    );
    let live = store.list_messages().unwrap();
    let sent_before = live
        .iter()
        .find(|r| r.message_id == before.message_id)
        .unwrap();
    let sent_after = live
        .iter()
        .find(|r| r.message_id == after.message_id)
        .unwrap();
    assert_eq!(
        sent_before.batch_id,
        Some(before.message_id.clone()),
        "prompts on both sides of continue must share one batch"
    );
    assert_eq!(sent_after.batch_id, sent_before.batch_id);

    // 2. A fresh send to a resting agent whose continue is still Queued is not parked behind it.
    let fixture = Fixture::new("claude");
    let workspace = fixture.env.resolve_workspace(&fixture.env.project_root);
    let store = fixture.env.store();
    fixture.end_turn();
    fixture.context(20_000);
    let agent = fixture.agent();
    let mut continuation = MessageRecord::new(
        fixture.env.workspace_id.clone(),
        &agent,
        "keep going".into(),
        DeliveryGate::Done,
    );
    continuation.soft_compact = Some(SoftCompactStep::Continue);
    continuation.sender = MessageSender::System;
    store
        .queue_message(&continuation, &workspace.session_name)
        .unwrap();

    let send_out = run_success(
        traced_rimz(&fixture.env, "soft-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
            .args(["message", "@claude", "--", "fresh task for resting agent"]),
        "fresh send past queued continue",
    );
    let stdout = String::from_utf8_lossy(&send_out.stdout);
    assert!(
        stdout.starts_with("sent to "),
        "fresh send must deliver live instead of parking behind a queued continue: {stdout}"
    );
    let fresh_id = MessageId::parse(&sent_id_from_stdout(&send_out.stdout)).unwrap();
    assert_eq!(fixture.status_of(&fresh_id), MessageStatus::Sent);
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Queued
    );

    // 3. A fresh send DOES park behind a continue that is already Sent.
    let fixture = Fixture::new("claude");
    let workspace = fixture.env.resolve_workspace(&fixture.env.project_root);
    let store = fixture.env.store();
    fixture.end_turn();
    fixture.context(20_000);
    let agent = fixture.agent();
    let mut continuation = MessageRecord::new(
        fixture.env.workspace_id.clone(),
        &agent,
        "keep going".into(),
        DeliveryGate::Done,
    );
    continuation.soft_compact = Some(SoftCompactStep::Continue);
    continuation.sender = MessageSender::System;
    store
        .queue_message(&continuation, &workspace.session_name)
        .unwrap();
    fixture.deliver(&continuation.message_id);
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Sent
    );
    let parked_out = run_success(
        traced_rimz(&fixture.env, "soft-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
            .env("RIMZ_MESSAGE_DELIVERY_WINDOW_MS", "60000")
            .args(["message", "@claude", "--", "must park behind sent continue"]),
        "fresh send behind sent continue",
    );
    let parked_stdout = String::from_utf8_lossy(&parked_out.stdout);
    assert!(
        parked_stdout.contains(&format!("behind {}", continuation.message_id)),
        "fresh send must park behind an in-flight continue: {parked_stdout}"
    );
}

#[test]
fn supervised_run_parked_on_continue_carries_on_through_a_waiting_message_turn() {
    use rimz::harness::run;
    use rimz::store::run::{RunRecord, RunStatus};

    let fixture = Fixture::new("claude");
    let workspace = fixture.env.resolve_workspace(&fixture.env.project_root);
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

    let settle = |signal: LifecycleSignal, prompt: Option<&str>, text: &str| {
        match prompt {
            Some(prompt) => fixture.lifecycle_with_prompt(signal.clone(), prompt),
            None => fixture.lifecycle(signal.clone()),
        }
        let mut obs = AgentLifecycleObservation::new(record.agent_id.clone(), signal);
        obs.prompt = rimz::agents::SanitizedPrompt::new(prompt);
        run::settle_lifecycle(
            &store,
            &record.run_id,
            rimz::agents::definition_by_kind("claude").unwrap(),
            &obs,
            Some(text.into()),
            None,
        )
        .unwrap()
    };
    let end = || LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    };

    // A message waits mid-turn, then the agent requests compaction.
    let agent = fixture.agent();
    let waiting = rimz::message::synthetic::SyntheticMessage {
        agent: &agent,
        text: "follow-up from parent".to_owned(),
        sender: MessageSender::Agent {
            kind: AgentKind::new_unchecked("claude"),
            agent_id: Some("parent-session".into()),
            name: Some("planner".to_owned()),
            profile: None,
            role: None,
            channel: None,
        },
        gate: DeliveryGate::Done,
        premises: rimz::store::message::Premise::NONE,
    }
    .record(&workspace);
    store
        .queue_message(&waiting, &workspace.session_name)
        .unwrap();
    fixture.compact(&[]);
    let records = fixture.records();
    let command = records
        .iter()
        .find(|r| r.body == MessageBody::Command)
        .unwrap()
        .clone();
    let continuation = records
        .iter()
        .find(|r| r.soft_compact == Some(SoftCompactStep::Continue))
        .unwrap()
        .clone();

    // Turn ends: run parks on SoftCompact.
    settle(end(), None, "wrapping up for compact");
    let parked = run::load(store.paths(), &record.run_id).unwrap();
    assert_eq!(parked.status, RunStatus::Running);
    assert!(parked.parked_at.is_some());
    assert_eq!(
        parked.parked_on,
        Some(rimz::store::run::ParkCause::SoftCompact)
    );

    // Compaction runs first, then the waiting message is delivered ahead of the continue.
    run_success(
        traced_rimz(&fixture.env, "soft-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
            .args(["message", "sweep"]),
        "deliver compaction ahead of waiting message",
    );
    assert_eq!(
        fixture.status_of(&command.message_id),
        MessageStatus::Sent,
        "compaction command must deliver ahead of the waiting message"
    );
    assert_eq!(
        fixture.status_of(&waiting.message_id),
        MessageStatus::Queued
    );
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Queued
    );
    fixture.lifecycle(LifecycleSignal::Compacting);
    fixture.lifecycle(compaction_end());
    fixture.context(20_000);
    run_success(
        traced_rimz(&fixture.env, "soft-trace.log")
            .env("RIMZ_TEST_PANE_LIST", &fixture.panes)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .env("RIMZ_MESSAGE_INTERVAL_MS", "0")
            .args(["message", "sweep"]),
        "deliver waiting message ahead of continue",
    );
    assert_eq!(fixture.status_of(&waiting.message_id), MessageStatus::Sent);
    assert_eq!(
        fixture.status_of(&continuation.message_id),
        MessageStatus::Queued
    );

    // Waiting message opens a turn: park clears (run carries on, does NOT complete early) and
    // the continue is withdrawn.
    let rendered = format!(
        "Type: AGENT_MESSAGE\nFrom: @planner (claude)\nContent:\n{}",
        waiting.text
    );
    settle(
        LifecycleSignal::TurnStarted { turn_id: None },
        Some(&rendered),
        "",
    );
    let row = fixture.agent();
    store
        .confirm_delivered_for_card_with(
            row.card_ref(),
            rimz::store::writer::DeliveryAck::TurnStarted {
                prompt: Some(&rendered),
            },
            Some(&row),
            row.epoch,
            &workspace.session_name,
            |_, _| {},
        )
        .unwrap();
    let active = run::load(store.paths(), &record.run_id).unwrap();
    assert_eq!(
        (active.status, active.parked_at),
        (RunStatus::Running, None),
        "run must carry on through the waiting message's turn"
    );
    fixture.assert_canceled_with(
        &continuation.message_id,
        "compaction continue cleared before delivery",
    );

    // Completing that turn completes the run with both turns' answers joined.
    let finished = settle(end(), None, "finished after waiting message").unwrap();
    assert_eq!(finished.status, RunStatus::Completed);
    assert_eq!(
        finished.last_message.as_deref(),
        Some("wrapping up for compact\n\n---\n\nfinished after waiting message")
    );
}

#[test]
fn continue_is_owed_only_to_a_self_request_and_not_to_a_person_peer_or_unresolved_caller() {
    let seed_busy = |name: &str, launch_id: &str| {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        run_success(
            env.rimz().args([
                "config",
                "set",
                "resume.auto_continue_text",
                "  keep going  ",
            ]),
            "configure continue text",
        );
        let session = env.resolve_workspace(&env.project_root).session_name;
        let mut pane = agent_pane(&env, "claude");
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
        let pane_id = PaneId::from_parts(MuxName::Zellij, TRACE_PANE);
        crate::common::wait::register_agent(
            &env,
            "target-session",
            launch_id,
            name,
            rimz::agents::LaunchParams::default(),
        );
        append_lifecycle(
            &env,
            "claude",
            "soft-test",
            "target-session",
            LifecycleSignal::TurnStarted { turn_id: None },
            |obs| {
                obs.pane_id = Some(pane_id.clone());
                obs.launch_id = Some(launch_id.into());
                obs.agent_name = Some(name.to_owned());
            },
        );
        // Register a distinct peer agent with its own launch id.
        crate::common::wait::register_agent(
            &env,
            "peer-session",
            "launch-peer",
            "peer-agent",
            rimz::agents::LaunchParams::default(),
        );
        append_lifecycle(
            &env,
            "claude",
            "soft-test",
            "peer-session",
            LifecycleSignal::TurnStarted { turn_id: None },
            |obs| {
                obs.launch_id = Some("launch-peer".into());
                obs.agent_name = Some("peer-agent".to_owned());
            },
        );
        (env, panes)
    };

    let run_compact = |(env, panes): &(Env, PathBuf), envs: &[(&str, &str)], args: &[&str]| {
        let mut cmd = traced_rimz(env, "soft-trace.log");
        cmd.env("RIMZ_TEST_PANE_LIST", panes);
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.args(["agents", "compact"]).args(args);
        String::from_utf8_lossy(&run_success(&mut cmd, "agents compact").stdout).into_owned()
    };

    let records_of = |(env, _): &(Env, PathBuf)| {
        let (mut live, history) = env.store().list_messages_and_history().unwrap();
        live.extend(history);
        live
    };

    let target_row = |(env, _): &(Env, PathBuf)| {
        env.store()
            .agent_with_context(
                &AgentKind::new_unchecked("claude"),
                &AgentSessionId::from("target-session"),
                rimz::RuntimeScope::Runtime,
            )
            .unwrap()
            .unwrap()
    };

    // 1. A person's request on a busy agent queues the command with no continue.
    let fixture = seed_busy("target-agent", "launch-target");
    let person_receipt = run_compact(&fixture, &[], &["@target-agent"]);
    assert!(
        person_receipt
            .trim_end()
            .ends_with("delivers at its next turn boundary, with no continue after"),
        "person request must report no continue: {person_receipt}"
    );
    let records = records_of(&fixture);
    assert_eq!(
        records.len(),
        1,
        "person request must queue only the command: {records:?}"
    );
    assert_eq!(records[0].body, MessageBody::Command);
    assert!(
        !target_row(&fixture)
            .soft_compact
            .is_some_and(|mark| mark.continue_owed.is_some()),
        "person request must owe no continue"
    );

    // 2. A peer's request queues no continue; `--no-continue` from a peer is accepted and
    //    changes nothing; a caller with no row owes no continue; then the agent's own `@me`
    //    request joins and adds the continue; then a peer joining after sees
    //    `then continues it if nothing else is waiting`.
    let fixture = seed_busy("target-agent", "launch-target");
    let peer_env = [
        ("RIMZ_AGENT_KIND", "claude"),
        ("RIMZ_AGENT_ID", "launch-peer"),
    ];
    let peer_receipt = run_compact(&fixture, &peer_env, &["@target-agent"]);
    assert!(
        peer_receipt
            .trim_end()
            .ends_with("delivers at its next turn boundary, with no continue after"),
        "peer request must report no continue: {peer_receipt}"
    );
    assert_eq!(records_of(&fixture).len(), 1);

    let peer_no_continue = run_compact(&fixture, &peer_env, &["@target-agent", "--no-continue"]);
    assert!(
        peer_no_continue
            .trim_end()
            .ends_with("delivers at its next turn boundary, with no continue after"),
        "peer --no-continue must join with no continue: {peer_no_continue}"
    );
    assert_eq!(records_of(&fixture).len(), 1);

    let ghost_env = [
        ("RIMZ_AGENT_KIND", "claude"),
        ("RIMZ_AGENT_ID", "launch-ghost"),
    ];
    let ghost_join = run_compact(&fixture, &ghost_env, &["@target-agent"]);
    assert!(
        ghost_join
            .trim_end()
            .ends_with("delivers at its next turn boundary, with no continue after"),
        "unresolved caller must owe no continue: {ghost_join}"
    );
    assert_eq!(records_of(&fixture).len(), 1);

    // Self-request by `@me` joins the peer-queued command and adds the continue.
    let self_env = [
        ("RIMZ_AGENT_KIND", "claude"),
        ("RIMZ_AGENT_ID", "launch-target"),
    ];
    let self_join = run_compact(&fixture, &self_env, &["@me"]);
    assert!(
        self_join.trim_end().ends_with(
            "delivers at its next turn boundary, then continues it if nothing else is waiting"
        ),
        "self @me join must add the continue: {self_join}"
    );
    let records = records_of(&fixture);
    assert_eq!(
        records.len(),
        2,
        "self join must add one continue: {records:?}"
    );
    assert!(
        records
            .iter()
            .any(|r| r.soft_compact == Some(SoftCompactStep::Continue))
    );
    assert!(
        target_row(&fixture)
            .soft_compact
            .is_some_and(|mark| mark.continue_owed.is_some())
    );

    // A peer joining after the agent's own request reports that a continue follows.
    let peer_after_self = run_compact(&fixture, &peer_env, &["@target-agent"]);
    assert!(
        peer_after_self.trim_end().ends_with(
            "delivers at its next turn boundary, then continues it if nothing else is waiting"
        ),
        "peer joining after self continue must report continues: {peer_after_self}"
    );
    assert_eq!(records_of(&fixture).len(), 2);

    // 3. Unresolved caller starting fresh queues no continue.
    let fixture = seed_busy("target-agent", "launch-target");
    let ghost_fresh = run_compact(&fixture, &ghost_env, &["@target-agent"]);
    assert!(
        ghost_fresh
            .trim_end()
            .ends_with("delivers at its next turn boundary, with no continue after"),
        "fresh unresolved caller must queue no continue: {ghost_fresh}"
    );
    assert_eq!(records_of(&fixture).len(), 1);

    // 4. Self-request by own handle queues a continue; `--no-continue` skips it.
    let fixture = seed_busy("target-agent", "launch-target");
    let self_handle = run_compact(&fixture, &self_env, &["@target-agent"]);
    assert!(
        self_handle.trim_end().ends_with(
            "delivers at its next turn boundary, then continues it if nothing else is waiting"
        ),
        "self request by own handle must queue a continue: {self_handle}"
    );
    assert_eq!(records_of(&fixture).len(), 2);

    let fixture = seed_busy("target-agent", "launch-target");
    let self_no_continue = run_compact(&fixture, &self_env, &["@me", "--no-continue"]);
    assert!(
        self_no_continue
            .trim_end()
            .ends_with("delivers at its next turn boundary, with no continue after"),
        "self request with --no-continue must owe no continue: {self_no_continue}"
    );
    assert_eq!(records_of(&fixture).len(), 1);
}
