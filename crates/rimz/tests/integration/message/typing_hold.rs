use super::*;

struct Fixture {
    env: Env,
    bin: PathBuf,
    clients: PathBuf,
    trace: PathBuf,
    panes: PathBuf,
    tty: PathBuf,
}

impl Fixture {
    fn new(running: bool) -> Self {
        Self::for_kind("claude", running)
    }

    fn for_kind(kind: &str, running: bool) -> Self {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks(kind);
        let bin = env.project_root.join("bin");
        crate::common::write_path_shim(
            &bin,
            "tmux",
            &format!(
                r#"if [ "$1" = -S ]; then shift 3; fi
printf '%s\n' "$*" >> "$RIMZ_TEST_TYPING_TRACE"
case "$1" in
  -V) printf 'tmux 3.8\n' ;;
  list-clients) cat "$RIMZ_TEST_TYPING_CLIENTS" ;;
  list-panes) printf '%s,@1,%%3,{kind}\n' "$RIMZ_TEST_TYPING_SESSION" ;;
  load-buffer) dd of=/dev/null status=none ;;
esac"#
            ),
        );
        let clients = env.project_root.join("clients.txt");
        std::fs::write(&clients, "").unwrap();
        let trace = env.project_root.join("tmux.log");
        let pane_id = PaneId::from_parts(MuxName::Tmux, "%3");
        let anchor = jiff::Timestamp::now() - Duration::from_secs(3);
        for signal in [
            LifecycleSignal::Registered,
            LifecycleSignal::TurnStarted { turn_id: None },
        ]
        .into_iter()
        .chain((!running).then_some(LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        })) {
            let mut observation =
                AgentLifecycleObservation::new(Some("typing-session".into()), signal);
            observation.pane_id = Some(pane_id.clone());
            observation.worktree_path = Some(env.project_root.display().to_string());
            let mut event = EventEnvelope::agent_lifecycle(
                env.workspace_id.clone(),
                "rimz-test",
                kind,
                "typing-test",
                &observation,
            );
            event.timestamp = anchor;
            env.store().append_event(&event).unwrap();
        }
        let mut pane = agent_pane(&env, kind);
        pane.pane_id = pane_id;
        pane.session_name = env.resolve_workspace(&env.project_root).session_name;
        let panes = env.write_pane_fixture(std::slice::from_ref(&pane));
        let frame = rimz::sidebar::frame::assemble_frame(
            vec![pane],
            rimz::utils::time::unix_now_ms(),
            env.resolve_workspace(&env.project_root).session_name,
        );
        rimz::disk::atomic::write_temp_then_rename_cache(
            &env.runtime_paths().pane_frame_path(),
            &frame,
        )
        .unwrap();
        let tty = env.project_root.join("pane-tty");
        std::fs::write(&tty, b"").unwrap();
        rimz::disk::atomic::write_temp_then_rename_cache(
            &env.runtime_paths().lane_path("typing.json"),
            &serde_json::json!({
                "panes": {
                    "tmux:%3": {
                        "tty": tty,
                        "atime_s": 1,
                        "viewers": [rimz::ids::MuxClientId::Tmux("human".into())],
                        "quiet_until_ms": 0,
                        "input_ms": null
                    }
                }
            }),
        )
        .unwrap();
        let fixture = Self {
            env,
            bin,
            clients,
            trace,
            panes,
            tty,
        };
        // A future second leaves at least two seconds for assertions at whole-second resolution.
        fixture.input();
        fixture
    }

    fn command(&self) -> std::process::Command {
        let mut cmd = self.env.rimz();
        cmd.env(
            "PATH",
            format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("RIMZ_TEST_TYPING_CLIENTS", &self.clients)
        .env("RIMZ_TEST_TYPING_TRACE", &self.trace)
        .env(
            "RIMZ_TEST_TYPING_SESSION",
            self.env
                .resolve_workspace(&self.env.project_root)
                .session_name,
        )
        .env("RIMZ_TEST_PANE_LIST", &self.panes)
        .args(["--mux", "tmux"]);
        cmd
    }

    fn input(&self) {
        let input_s = u64::try_from(jiff::Timestamp::now().as_second()).unwrap() + 1;
        let quiet_ms = self.typing_record()["panes"]["tmux:%3"]["quiet_until_ms"]
            .as_u64()
            .unwrap_or(0);
        self.tty_input_at(input_s.max(quiet_ms.div_ceil(1_000)));
        let client_input_s = if cfg!(target_os = "linux") {
            1
        } else {
            input_s
        };
        std::fs::write(
            &self.clients,
            format!("human|%3|{client_input_s}|root||focused\n"),
        )
        .unwrap();
    }

    fn tty_input_at(&self, input_s: u64) {
        std::fs::File::open(&self.tty)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_accessed(std::time::UNIX_EPOCH + Duration::from_secs(input_s)),
            )
            .unwrap();
    }

    fn typing_record(&self) -> serde_json::Value {
        serde_json::from_slice(
            &std::fs::read(self.env.runtime_paths().lane_path("typing.json")).unwrap(),
        )
        .unwrap()
    }

    fn clear_input(&self) {
        self.tty_input_at(1);
        std::fs::write(&self.clients, "human|%3|1|root||focused\n").unwrap();
    }

    fn short_hold(&self) {
        run_success(
            self.command()
                .args(["config", "set", "harness.typing_hold", "2s"]),
            "short hold",
        );
    }

    fn record(&self) -> MessageRecord {
        self.env
            .store()
            .list_messages()
            .unwrap()
            .into_iter()
            .find(|r| r.body == MessageBody::Prompt)
            .unwrap()
    }

    fn reads(&self) -> usize {
        trace_lines(&self.trace)
            .iter()
            .filter(|line| line.starts_with("list-clients "))
            .count()
    }

    fn writes(&self) -> usize {
        trace_lines(&self.trace)
            .iter()
            .filter(|line| line.starts_with("load-buffer ") || line.starts_with("send-keys "))
            .count()
    }

    fn expire(&self, record: &MessageRecord) {
        let value = serde_json::to_value(record).unwrap();
        let until: jiff::Timestamp =
            serde_json::from_value(value["typing_hold"]["until"].clone()).unwrap();
        let remaining = until
            .duration_since(jiff::Timestamp::now())
            .as_millis()
            .max(0) as u64;
        std::thread::sleep(Duration::from_millis(remaining + 20));
        self.clear_input();
    }
}

#[test]
fn boundary_hold_keeps_the_claim_budget_and_is_explained_without_mux_reads() {
    let fixture = Fixture::new(false);
    fixture.short_hold();
    let output = run_success(
        fixture.command().args(["message", "@claude", "go"]),
        "held boundary",
    );
    let record = fixture.record();
    assert_eq!(record.status, MessageStatus::Queued);
    assert_eq!(record.attempts, 0);
    // A held record keeps no pane: the replay below writes to the pane the agent
    // holds when the hold expires.
    assert_eq!(record.pane_id, None);
    let value = serde_json::to_value(&record).unwrap();
    assert_eq!(value["retry_after"], value["typing_hold"]["until"]);
    assert!(value["typing_hold"]["until"].is_string(), "{value}");
    let receipt = String::from_utf8_lossy(&output.stdout);
    assert!(
        receipt.contains("held")
            && receipt.contains("someone is using")
            && receipt.contains(record.message_id.as_str()),
        "{receipt}"
    );
    assert_eq!(fixture.writes(), 0);
    let reads = fixture.reads();
    run_success(
        fixture.command().args([
            "message",
            "deliver",
            "--message-id",
            record.message_id.as_str(),
        ]),
        "stored hold",
    );
    let shown = run_success(
        fixture
            .command()
            .args(["message", "show", record.message_id.as_str()]),
        "show hold",
    );
    let shown = String::from_utf8_lossy(&shown.stdout);
    assert!(
        shown.contains("typing hold") && shown.contains("retry"),
        "{shown}"
    );
    let shown = run_success(
        fixture
            .command()
            .args(["message", "show", record.message_id.as_str(), "--json"]),
        "show hold json",
    );
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        shown["delivery"]["check"]["typing_hold"]["until"],
        value["typing_hold"]["until"]
    );
    assert_eq!(fixture.reads(), reads);
    fixture.expire(&record);
    run_success(
        fixture.command().args(["message", "sweep"]),
        "expired hold sweep",
    );
    assert_eq!(fixture.record().status, MessageStatus::Sent);
    assert_eq!(fixture.record().attempts, 1);
    assert!(fixture.writes() > 0);
}

#[test]
fn typing_hold_deadline_survives_fifo_and_condition_sweep_deferrals() {
    let mut observed = Vec::new();
    let mut expected = Vec::new();
    for older_sent in [true, false] {
        let fixture = Fixture::new(false);
        run_success(
            fixture
                .command()
                .args(["config", "set", "harness.typing_hold", "2m"]),
            "long hold",
        );
        let store = fixture.env.store();
        let agent = store.snapshot().unwrap().agents[0].clone();
        if older_sent {
            let previous = MessageRecord::new(
                fixture.env.workspace_id.clone(),
                &agent,
                "previous".into(),
                DeliveryGate::Any,
            )
            .with_pane_id(PaneId::from_parts(MuxName::Tmux, "%3"));
            store.queue_message(&previous, "room").unwrap();
            store.record_sent_batch(&[previous], "room").unwrap();
        }
        run_success(
            fixture
                .command()
                .args(["message", "--steer", "@claude", "go"]),
            "held steer",
        );
        let mut held = store
            .list_messages()
            .unwrap()
            .into_iter()
            .find(|r| r.text == "go")
            .unwrap();
        if !older_sent {
            held.after.push(rimz::store::message::AfterCondition {
                kind: agent.kind,
                agent_id: "missing-upstream".into(),
                agent_name: None,
                address: "@upstream".into(),
                met_at: None,
            });
            store.queue_message(&held, "room").unwrap();
        }
        expected.push(held.typing_hold.map(|hold| hold.until));
        run_success(fixture.command().args(["message", "sweep"]), "held sweep");
        let held = store
            .list_messages()
            .unwrap()
            .into_iter()
            .find(|r| r.message_id == held.message_id)
            .unwrap();
        observed.push(held.retry_after);
        assert_eq!(fixture.writes(), 0);
    }
    assert_eq!(observed, expected);
}

#[test]
fn held_live_sends_replay_as_themselves_while_the_receiver_is_running() {
    for flags in [
        vec!["--steer"],
        vec!["--steer", "--force"],
        vec!["--interrupt"],
    ] {
        let fixture = Fixture::new(true);
        fixture.short_hold();
        let output = run_success(
            fixture
                .command()
                .arg("message")
                .args(&flags)
                .args(["@claude", "go"]),
            "held live send",
        );
        let record = fixture.record();
        assert_eq!(record.status, MessageStatus::Queued, "{flags:?}");
        assert_eq!(record.attempts, 0);
        let receipt = String::from_utf8_lossy(&output.stdout);
        assert!(
            receipt.contains("held") && receipt.contains("someone is using"),
            "{flags:?}: {receipt}"
        );
        let expected = if flags[0] == "--steer" {
            "steer"
        } else {
            "interrupt"
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["typing_hold"]["replay"], expected);
        let output = run_success(
            fixture
                .command()
                .args(["message", "steer", record.message_id.as_str()]),
            "steer held record",
        );
        let receipt = String::from_utf8_lossy(&output.stdout);
        assert!(receipt.contains("held"), "{receipt}");
        assert_eq!(fixture.writes(), 0);
        fixture.expire(&record);
        run_success(fixture.command().args(["message", "sweep"]), "replay sweep");
        assert_eq!(fixture.record().status, MessageStatus::Sent, "{flags:?}");
        assert!(fixture.writes() > 0);
    }
}

#[test]
fn pushing_a_held_boundary_message_replays_it_in_the_requested_live_mode() {
    for (verb, force) in [("steer", true), ("interrupt", false)] {
        let fixture = Fixture::new(false);
        fixture.short_hold();
        run_success(
            fixture.command().args(["message", "@claude", "go"]),
            "held boundary",
        );
        let record = fixture.record();
        assert_eq!(record.status, MessageStatus::Queued);
        let mut push = fixture.command();
        push.args(["message", verb, record.message_id.as_str()]);
        if force {
            push.arg("--force");
        }
        let output = run_success(&mut push, "push held record");
        assert!(String::from_utf8_lossy(&output.stdout).contains("held"));
        let held = fixture.record();
        let encoded = serde_json::to_value(&held).unwrap();
        assert_eq!(encoded["typing_hold"]["replay"], verb);
        assert_eq!(
            encoded["typing_hold"]["force"].as_bool().unwrap_or(false),
            force
        );
        assert!(!held.force);
        let mut observation = AgentLifecycleObservation::new(
            Some("typing-session".into()),
            LifecycleSignal::TurnStarted { turn_id: None },
        );
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%3"));
        fixture
            .env
            .store()
            .append_event(&EventEnvelope::agent_lifecycle(
                fixture.env.workspace_id.clone(),
                "rimz-test",
                "claude",
                "typing-test",
                &observation,
            ))
            .unwrap();
        assert_eq!(
            fixture.env.store().snapshot().unwrap().agents[0].status,
            rimz::agents::AgentStatus::Running
        );
        fixture.expire(&held);
        run_success(fixture.command().args(["message", "sweep"]), "replay sweep");
        assert_eq!(fixture.record().status, MessageStatus::Sent, "{verb}");
    }
}

#[test]
fn cancellation_after_pacing_stops_before_any_pane_write() {
    let fixture = Fixture::new(false);
    fixture.clear_input();
    let before = DeliveryRendezvous::new(&fixture.env, "cancel-before-write");
    let child = fixture
        .command()
        .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &before.path)
        .args(["message", "@claude", "go"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    let claimed = fixture.record();
    assert_eq!(claimed.status, MessageStatus::Claimed);
    fixture
        .env
        .store()
        .cancel_message(&claimed.message_id, "room", "withdrawn before write")
        .unwrap();
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.writes(), 0);
    assert!(
        !fixture
            .env
            .read_events()
            .iter()
            .any(|event| event.method == "message.sent")
    );
}

#[test]
fn typing_hold_only_bypasses_for_forced_interrupt_forced_keepalive_disabled_setting_or_read_failure()
 {
    let mut observed = Vec::new();
    for case in [
        "boundary",
        "forced-steer",
        "polite-keepalive",
        "forced-interrupt",
        "forced-keepalive",
        "off",
        "read-error",
    ] {
        let fixture = Fixture::new(false);
        if let Some(forced) = case.strip_suffix("-keepalive").map(|kind| kind == "forced") {
            fixture.keepalive_config();
            run_success(
                &mut fixture.keepalive_command(&fixture.keepalive(forced)),
                case,
            );
            observed.push(fixture.pings()[0].status);
            continue;
        }
        if case == "off" {
            run_success(
                fixture
                    .command()
                    .args(["config", "set", "harness.typing_hold", "off"]),
                "disable hold",
            );
        }
        if case == "read-error" {
            std::fs::remove_file(&fixture.clients).unwrap();
        }
        let mut cmd = fixture.command();
        cmd.arg("message");
        if case == "forced-steer" {
            cmd.args(["--steer", "--force"]);
        }
        if case == "forced-interrupt" {
            cmd.args(["--interrupt", "--force"]);
        }
        run_success(cmd.args(["@claude", "go"]), case);
        observed.push(fixture.record().status);
    }
    assert_eq!(
        observed,
        [
            MessageStatus::Queued,
            MessageStatus::Queued,
            MessageStatus::Errored,
            MessageStatus::Sent,
            MessageStatus::Sent,
            MessageStatus::Sent,
            MessageStatus::Sent
        ]
    );
}

#[test]
fn input_after_the_pacer_is_seen_before_any_paste_or_sent_record() {
    let fixture = Fixture::new(false);
    fixture.clear_input();
    let before = DeliveryRendezvous::new(&fixture.env, "typing-before-write");
    let child = fixture
        .command()
        .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &before.path)
        .args(["message", "@claude", "go"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    fixture.input();
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fixture.record().status, MessageStatus::Queued);
    assert_eq!(fixture.record().attempts, 0);
    assert_eq!(fixture.writes(), 0);
    assert!(
        !fixture
            .env
            .read_events()
            .iter()
            .any(|e| e.method == "message.sent")
    );
}

#[test]
fn compact_first_prompt_hold_never_repeats_the_compact() {
    let fixture = Fixture::new(false);
    fixture.short_hold();
    seed_context_fill(&fixture.env, "typing-session", 80);
    fixture.clear_input();
    let before = DeliveryRendezvous::new(&fixture.env, "typing-compact-write");
    let child = fixture
        .command()
        .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &before.path)
        .args([
            "message",
            "--steer",
            "--smart-compact",
            "70%",
            "@claude",
            "go",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    before.arrive().write_all(&[1]).unwrap();
    let mut release = before.arrive();
    fixture.input();
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    assert_eq!(record.status, MessageStatus::Queued);
    assert_eq!(record.auto_compact, None);
    let commands = fixture
        .env
        .store()
        .list_messages()
        .unwrap()
        .into_iter()
        .filter(|r| r.body == MessageBody::Command)
        .count();
    assert_eq!(commands, 1);
    fixture.expire(&record);
    run_success(
        fixture.command().args(["message", "sweep"]),
        "compact replay",
    );
    assert_eq!(fixture.record().status, MessageStatus::Sent);
    assert_eq!(
        fixture
            .env
            .store()
            .list_messages()
            .unwrap()
            .into_iter()
            .filter(|r| r.body == MessageBody::Command)
            .count(),
        commands
    );
}

impl Fixture {
    fn keepalive_config(&self) {
        for (key, value) in [
            ("harness.keep_warm", "2h"),
            ("harness.keep_warm_min_ttl", "off"),
            ("harness.prompt_cache_ttl.claude", "61s"),
        ] {
            run_success(
                self.command().args(["config", "set", key, value]),
                "keepalive config",
            );
        }
    }

    fn keepalive(&self, forced: bool) -> rimz::harness::cache_keepalive::CacheKeepaliveRequest {
        let agent = self.env.store().snapshot().unwrap().agents[0].clone();
        rimz::harness::cache_keepalive::CacheKeepaliveRequest {
            workspace_id: self.env.workspace_id.clone(),
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%3"),
            anchor: agent.request_key().unwrap(),
            label: "@claude".into(),
            forced,
        }
    }

    fn keepalive_command(
        &self,
        request: &rimz::harness::cache_keepalive::CacheKeepaliveRequest,
    ) -> std::process::Command {
        let mut cmd = self.command();
        cmd.args(rimz::child_process::agent_helper_argv(
            "cache-keepalive",
            request,
        ));
        cmd
    }

    fn pings(&self) -> Vec<MessageRecord> {
        let mut messages = self.env.store().list_messages().unwrap();
        messages.extend(self.env.store().list_message_history().unwrap());
        messages.retain(|r| {
            matches!(
                r.sender,
                MessageSender::Harness {
                    notice: HarnessNotice::CacheKeepalive
                }
            )
        });
        messages.sort_by(|a, b| a.message_id.cmp(&b.message_id));
        messages
    }

    fn keepalive_assists(&self) -> Vec<(bool, bool, Option<String>)> {
        rimz::harness::assist_log::recent(&self.env.rimz_home().join("logs"), None)
            .into_iter()
            .filter_map(|record| match record.assist {
                rimz::harness::assist_log::Assist::CacheKeepalive {
                    forced,
                    delivered,
                    error,
                    ..
                } => Some((forced, delivered, error)),
                _ => None,
            })
            .collect()
    }
}

#[test]
fn a_held_record_ahead_retries_while_a_polite_ping_misses_and_a_forced_one_delivers_first() {
    for ahead in ["compaction", "message"] {
        let fixture = Fixture::new(false);
        fixture.keepalive_config();
        fixture.short_hold();
        run_success(
            &mut fixture.keepalive_command(&fixture.keepalive(false)),
            "held ping",
        );
        let pings = fixture.pings();
        assert_eq!(pings.len(), 1, "{ahead}: {pings:?}");
        assert_eq!(pings[0].status, MessageStatus::Errored);
        assert!(
            pings[0]
                .last_error
                .as_ref()
                .is_some_and(|r| r.contains("someone is using")),
            "{ahead}: {pings:?}"
        );
        let args: &[&str] = match ahead {
            "compaction" => &["agents", "compact", "@claude"],
            _ => &["message", "@claude", "go"],
        };
        run_success(fixture.command().args(args), ahead);
        let held = || {
            fixture
                .env
                .store()
                .list_messages()
                .unwrap()
                .into_iter()
                .find(|r| r.body == MessageBody::Command || r.text == "go")
                .unwrap()
        };
        let before = held();
        assert_eq!(before.status, MessageStatus::Queued, "{ahead}");
        assert_eq!(before.attempts, 0, "{ahead}");
        assert!(before.typing_hold.is_some(), "{ahead}: {before:?}");
        assert_eq!(fixture.writes(), 0, "{ahead}");

        // The forced ping passes the typing hold and the held record's place in the queue.
        fixture.input();
        run_success(
            &mut fixture.keepalive_command(&fixture.keepalive(true)),
            "forced ping past the held record",
        );
        let pings = fixture.pings();
        assert_eq!(pings.len(), 2, "{ahead}: {pings:?}");
        assert_eq!(pings[1].status, MessageStatus::Sent, "{ahead}: {pings:?}");
        assert_eq!(pings[1].batch_id, None, "{ahead}: the ping claims alone");
        assert_eq!(held(), before, "{ahead}: the held record is untouched");
        let writes = fixture.writes();
        assert!(writes > 0, "{ahead}");

        // Past its hold, the record still waits while the ping is unacknowledged, then delivers.
        fixture.expire(&before);
        run_success(
            fixture.command().args(["message", "sweep"]),
            "ping in flight",
        );
        assert_eq!(held().status, MessageStatus::Queued, "{ahead}");
        assert_eq!(fixture.writes(), writes, "{ahead}");
        let store = fixture.env.store();
        let agent = store.snapshot().unwrap().agents[0].clone();
        let acked = store
            .confirm_delivered_for_card(
                &agent.kind,
                &agent.agent_id,
                None,
                rimz::store::writer::DeliveryAck::TurnStarted { prompt: None },
                "room",
            )
            .unwrap();
        assert_eq!(
            acked.iter().map(|r| &r.message_id).collect::<Vec<_>>(),
            [&pings[1].message_id],
            "{ahead}"
        );
        run_success(fixture.command().args(["message", "sweep"]), "retry");
        assert_eq!(held().status, MessageStatus::Sent, "{ahead}");
        assert_eq!(held().attempts, 1, "{ahead}");
    }
}

#[test]
fn a_held_polite_ping_misses_and_the_forced_ping_delivers_past_the_hold() {
    let fixture = Fixture::new(false);
    fixture.keepalive_config();
    run_success(
        &mut fixture.keepalive_command(&fixture.keepalive(false)),
        "held ping",
    );
    assert_eq!(fixture.writes(), 0);

    // The hold is live before the pane lock and again under it, after the pacer.
    fixture.input();
    let before = DeliveryRendezvous::new(&fixture.env, "forced-ping-write");
    let child = fixture
        .keepalive_command(&fixture.keepalive(true))
        .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &before.path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before
        .arrive_timeout(Duration::from_secs(10))
        .expect("the forced ping reaches the pane write past the typing hold");
    fixture.input();
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let pings = fixture.pings();
    assert_eq!(pings.len(), 2, "{pings:?}");
    assert_eq!(pings[1].status, MessageStatus::Sent, "{pings:?}");
    assert!(pings[1].forced_keepalive, "{pings:?}");
    assert!(
        trace_lines(&fixture.trace)
            .iter()
            .any(|line| line.starts_with("load-buffer ")),
        "the forced ping reached the pane"
    );
    let assists = fixture.keepalive_assists();
    assert_eq!(assists.len(), 2, "{assists:?}");
    assert!(
        !assists[0].0
            && !assists[0].1
            && assists[0]
                .2
                .as_ref()
                .is_some_and(|error| error.contains("someone is using")),
        "the polite miss says the typing hold refused it: {assists:?}"
    );
    assert_eq!(
        assists[1],
        (true, true, None),
        "the forced send says forced"
    );
}

#[test]
fn a_ping_whose_cache_went_cold_under_the_pane_lock_is_never_pasted() {
    let fixture = Fixture::new(false);
    fixture.keepalive_config();
    // A 10 s lifetime with an 8 s margin: the 3 s old anchor is due and goes cold during the hold.
    for (key, value) in [
        ("harness.prompt_cache_margin", "8s"),
        ("harness.prompt_cache_ttl.claude", "10s"),
    ] {
        run_success(
            fixture.command().args(["config", "set", key, value]),
            "short lifetime",
        );
    }
    fixture.clear_input();
    let request = fixture.keepalive(true);
    let cold_at = fixture.env.store().snapshot().unwrap().agents[0]
        .last_request_at()
        .unwrap()
        + Duration::from_secs(10);
    let before = DeliveryRendezvous::new(&fixture.env, "cold-ping-write");
    let child = fixture
        .keepalive_command(&request)
        .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &before.path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    let left = cold_at.duration_since(jiff::Timestamp::now());
    std::thread::sleep(Duration::try_from(left).unwrap_or_default() + Duration::from_millis(100));
    release.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let pings = fixture.pings();
    assert_eq!(pings.len(), 1, "{pings:?}");
    assert_eq!(
        (pings[0].status, pings[0].last_error.as_deref()),
        (
            MessageStatus::Canceled,
            Some("lifespan ended before delivery")
        ),
        "{pings:?}"
    );
    assert_eq!(fixture.writes(), 0);
    assert!(
        !fixture
            .env
            .read_events()
            .iter()
            .any(|e| e.method == "message.sent")
    );
}

/// A cold stop on a polite ping at a batch's head settles the ping alone; the messages batched
/// behind it go back to the queue and deliver on the next attempt.
#[test]
fn a_cold_ping_at_a_batch_head_releases_the_messages_batched_behind_it() {
    let fixture = Fixture::new(false);
    fixture.keepalive_config();
    // A 2 s lifetime: the fixture's 3 s old turn is already cold at delivery.
    for (key, value) in [
        ("harness.prompt_cache_margin", "1s"),
        ("harness.prompt_cache_ttl.claude", "2s"),
    ] {
        run_success(
            fixture.command().args(["config", "set", key, value]),
            "short lifetime",
        );
    }
    fixture.clear_input();
    let agent = fixture.env.store().snapshot().unwrap().agents[0].clone();
    let message = |id: u64, text: &str, notice: Option<HarnessNotice>| {
        let mut record = MessageRecord::new(
            fixture.env.workspace_id.clone(),
            &agent,
            text.to_owned(),
            DeliveryGate::Done,
        )
        .with_sender(notice.map_or(MessageSender::Human, |notice| {
            MessageSender::Harness { notice }
        }));
        record.message_id = fixed_message_id(id);
        record
    };
    let ping = message(1, "Keep warm.", Some(HarnessNotice::CacheKeepalive));
    let human = message(2, "behind the ping", None);
    let digest = message(
        3,
        "@child completed: done",
        Some(HarnessNotice::SubagentReport),
    );
    queue_messages(&fixture.env, &[&ping, &human, &digest]);
    let by_id = |record: &MessageRecord| {
        let mut rows = fixture.env.store().list_messages().unwrap();
        rows.extend(fixture.env.store().list_message_history().unwrap());
        rows.into_iter()
            .find(|row| row.message_id == record.message_id)
            .unwrap()
    };

    run_success(
        fixture.command().args([
            "message",
            "deliver",
            "--message-id",
            ping.message_id.as_str(),
        ]),
        "deliver the batch behind a cold ping",
    );
    assert_eq!(fixture.writes(), 0, "nothing is pasted onto a cold cache");
    let cold = by_id(&ping);
    assert_eq!(cold.status, MessageStatus::Errored, "{cold:?}");
    assert_eq!(
        cold.last_error.as_deref(),
        Some("the receiver's prompt cache went cold before the ping was written"),
    );
    for record in [&human, &digest] {
        let released = by_id(record);
        assert_eq!(released.status, MessageStatus::Queued, "{released:?}");
        assert_eq!(released.attempts, 0, "{released:?}");
        assert_eq!(released.batch_id, None, "{released:?}");
        assert_eq!(released.last_sent_at, None, "{released:?}");
    }

    run_success(
        fixture.command().args([
            "message",
            "deliver",
            "--message-id",
            human.message_id.as_str(),
        ]),
        "deliver the released messages",
    );
    assert!(fixture.writes() > 0, "the released messages reach the pane");
    for record in [&human, &digest] {
        let sent = by_id(record);
        assert_eq!(sent.status, MessageStatus::Sent, "{sent:?}");
        assert_eq!(sent.batch_id, Some(human.message_id.clone()), "{sent:?}");
    }
}

impl Fixture {
    fn lifecycle_at(&self, signal: LifecycleSignal, prompt: Option<&str>, at: jiff::Timestamp) {
        let mut observation = AgentLifecycleObservation::new(Some("typing-session".into()), signal);
        observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%3"));
        observation.worktree_path = Some(self.env.project_root.display().to_string());
        observation.prompt = rimz::agents::SanitizedPrompt::new(prompt);
        let mut event = EventEnvelope::agent_lifecycle(
            self.env.workspace_id.clone(),
            "rimz-test",
            "claude",
            "typing-test",
            &observation,
        );
        event.timestamp = at;
        self.env.store().append_event(&event).unwrap();
    }

    fn record_request(&self, at: jiff::Timestamp) {
        let mut context = rimz::agents::AgentContext::new("claude", jiff::Timestamp::now());
        context.recorded_request_at = Some(at);
        let record = rimz::agents::context::record::AgentContextRecord::new(
            "claude",
            "typing-session",
            context,
        );
        rimz::store::agent_context::write_record(&self.env.runtime_paths(), &record).unwrap();
    }
}

/// The cold check under the pane write lock judges the clock the producer and the helper read:
/// the provider's recorded request, not the hook estimate, whichever way the two disagree.
#[test]
fn the_cold_check_under_the_pane_lock_reads_the_recorded_request() {
    for record_later in [false, true] {
        let fixture = Fixture::new(false);
        fixture.keepalive_config();
        // A 10 s lifetime with a 9 s margin: due one second after the clock's stamp.
        for (key, value) in [
            ("harness.prompt_cache_margin", "9s"),
            ("harness.prompt_cache_ttl.claude", "10s"),
        ] {
            run_success(
                fixture.command().args(["config", "set", key, value]),
                "short lifetime",
            );
        }
        let now = jiff::Timestamp::now();
        let recorded = if record_later {
            // A retried request: the record lands after the hook estimate (the fixture's turn,
            // 3 s ago), so the estimate goes cold 3 s before the record does.
            fixture.record_request(now);
            now
        } else {
            // A ping's close lands after its own recorded request, so the record goes cold
            // first.
            let open = now - Duration::from_millis(2_500);
            fixture.lifecycle_at(
                LifecycleSignal::TurnStarted { turn_id: None },
                Some("Type: CACHE_KEEPALIVE\nFrom: @rimz\nContent:\nKeep warm."),
                open,
            );
            fixture.lifecycle_at(
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    turn_id: None,
                },
                None,
                now,
            );
            fixture.record_request(open + Duration::from_millis(100));
            open + Duration::from_millis(100)
        };
        let estimate = fixture.env.store().snapshot().unwrap().agents[0]
            .request_key()
            .unwrap();
        let gap = if record_later {
            recorded.duration_since(estimate)
        } else {
            estimate.duration_since(recorded)
        };
        assert!(
            gap.as_millis() >= 2_000,
            "record_later={record_later}: {recorded} vs {estimate}"
        );
        std::thread::sleep(Duration::from_millis(1_100));
        fixture.clear_input();
        let request = fixture.keepalive(true);
        let first_cold = recorded.min(estimate) + Duration::from_secs(10);
        let before = DeliveryRendezvous::new(&fixture.env, "recorded-cold-check");
        let child = fixture
            .keepalive_command(&request)
            .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &before.path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut release = before.arrive();
        let left = first_cold.duration_since(jiff::Timestamp::now());
        std::thread::sleep(
            Duration::try_from(left).unwrap_or_default() + Duration::from_millis(100),
        );
        release.write_all(&[1]).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let pings = fixture.pings();
        assert_eq!(pings.len(), 1, "record_later={record_later}: {pings:?}");
        if record_later {
            assert_eq!(pings[0].status, MessageStatus::Sent, "{pings:?}");
            assert!(
                fixture.writes() > 0,
                "the ping the record calls warm reached the pane"
            );
        } else {
            // The ping's lifespan ends where the record calls the cache cold, so the write
            // check withdraws it before the cold check under the pane lock can error it.
            assert_eq!(
                (pings[0].status, pings[0].last_error.as_deref()),
                (
                    MessageStatus::Canceled,
                    Some("lifespan ended before delivery")
                ),
                "{pings:?}"
            );
            assert_eq!(
                fixture.writes(),
                0,
                "no ping text onto the cache the record calls cold"
            );
        }
    }
}

#[test]
fn input_before_the_compact_command_keeps_the_prompts_compaction_intent() {
    let fixture = Fixture::new(false);
    fixture.short_hold();
    seed_context_fill(&fixture.env, "typing-session", 80);
    fixture.clear_input();
    let before = DeliveryRendezvous::new(&fixture.env, "typing-before-compact");
    let child = fixture
        .command()
        .env("RIMZ_TEST_MESSAGE_BEFORE_WRITE", &before.path)
        .args([
            "message",
            "--steer",
            "--smart-compact",
            "70%",
            "@claude",
            "go",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut release = before.arrive();
    fixture.input();
    release.write_all(&[1]).unwrap();
    // The unfixed path reaches the prompt write too. Release it so the assertion,
    // rather than a blocked child, shows the missing hold.
    if let Some(mut release) = before.arrive_timeout(Duration::from_secs(10)) {
        release.write_all(&[1]).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let record = fixture.record();
    assert_eq!(record.status, MessageStatus::Queued);
    assert_eq!(record.auto_compact, Some(AutoCompact::Percent(70)));
    assert_eq!(fixture.writes(), 0);
    fixture.expire(&record);
    run_success(
        fixture.command().args(["message", "sweep"]),
        "compact retry",
    );
    assert_eq!(fixture.record().status, MessageStatus::Sent);
    let commands = fixture
        .env
        .store()
        .list_messages()
        .unwrap()
        .into_iter()
        .filter(|r| r.body == MessageBody::Command && r.status == MessageStatus::Sent)
        .count();
    assert_eq!(commands, 1);
}

#[cfg(target_os = "linux")]
#[test]
fn pane_writes_stamp_the_target_room_before_and_after_each_backend_result() {
    let mut actual = Vec::new();
    for kind in ["text", "key", "paste"] {
        for fail in [false, true] {
            let fixture = Fixture::new(false);
            run_success(
                fixture
                    .command()
                    .args(["config", "set", "harness.typing_hold", "off"]),
                "disable hold for explicit write",
            );
            crate::common::write_path_shim(
                &fixture.bin,
                "tmux",
                r#"if [ "$1" = -S ]; then shift 3; fi
case "$1" in
  -V) printf 'tmux 3.8\n' ;;
  list-panes) printf '%s,@1,%%3,claude\n' "$RIMZ_TEST_TYPING_SESSION" ;;
  list-clients) cat "$RIMZ_TEST_TYPING_CLIENTS" ;;
  send-keys|load-buffer)
    if [ "$1" = load-buffer ]; then dd of=/dev/null status=none; fi
    if [ -f "$RIMZ_TEST_TYPING_RECORD" ]; then
      cp "$RIMZ_TEST_TYPING_RECORD" "$RIMZ_TEST_TYPING_BEFORE.$1"
    fi
    printf '{"panes":{}}' > "$RIMZ_TEST_TYPING_RECORD"
    exit "$RIMZ_TEST_TYPING_WRITE_RC"
    ;;
esac"#,
            );
            let record = fixture.env.runtime_paths().lane_path("typing.json");
            let before = fixture.env.project_root.join("before-write");
            let mut command = fixture.command();
            command
                .env("RIMZ_TEST_TYPING_RECORD", &record)
                .env("RIMZ_TEST_TYPING_BEFORE", &before)
                .env("RIMZ_TEST_TYPING_WRITE_RC", if fail { "1" } else { "0" });
            match kind {
                "text" => command.args(["pane", "send", "tmux:%3", "hello"]),
                "key" => command.args(["pane", "send", "tmux:%3", "--key", "Enter"]),
                "paste" => command.args(["message", "--steer", "@claude", "hello"]),
                _ => unreachable!("the table names every write kind"),
            };
            let output = command.output().unwrap();
            if !fail {
                assert!(output.status.success(), "{kind}: {output:?}");
            }
            let stamped = |path: &Path| {
                std::fs::read(path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                    .and_then(|value| value["panes"]["tmux:%3"]["quiet_until_ms"].as_u64())
                    .is_some_and(|until| until > rimz::utils::time::unix_now_ms())
            };
            let backend = if kind == "paste" {
                "load-buffer"
            } else {
                "send-keys"
            };
            let at_call = before.with_extension(backend);
            let after_paste =
                kind != "paste" || fail || stamped(&before.with_extension("send-keys"));
            actual.push((kind, fail, stamped(&at_call), stamped(&record), after_paste));
        }
    }
    let expected: Vec<_> = ["text", "key", "paste"]
        .into_iter()
        .flat_map(|kind| [false, true].map(|fail| (kind, fail, true, true, true)))
        .collect();
    assert_eq!(actual, expected);
}

#[cfg(target_os = "linux")]
#[test]
fn counted_terminal_input_keeps_holding_after_the_viewer_leaves() {
    let fixture = Fixture::new(false);
    run_success(
        fixture
            .command()
            .args(["message", "--steer", "@claude", "first"]),
        "count viewed input",
    );
    let first = fixture.record();
    assert_eq!(first.status, MessageStatus::Queued);
    std::fs::write(&fixture.clients, "human|%4|1|root||\n").unwrap();
    run_success(
        fixture
            .command()
            .args(["message", "--steer", "@claude", "second"]),
        "hold after departure",
    );
    let second = fixture
        .env
        .store()
        .list_messages()
        .unwrap()
        .into_iter()
        .find(|record| record.text == "second")
        .unwrap();
    assert_eq!(
        (
            second.status,
            second.typing_hold.map(|hold| hold.until),
            fixture.writes()
        ),
        (
            MessageStatus::Queued,
            first.typing_hold.map(|hold| hold.until),
            0
        )
    );
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_advance_with_an_unfocused_client_does_not_hold_delivery() {
    let fixture = Fixture::new(false);
    std::fs::write(&fixture.clients, "human|%3|1|root||attached\n").unwrap();
    run_success(
        fixture.command().args(["message", "@claude", "go"]),
        "unfocused terminal input",
    );
    assert_eq!(fixture.record().status, MessageStatus::Sent);
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_advance_seen_elsewhere_is_consumed_without_a_hold() {
    let fixture = Fixture::new(false);
    let input_s = u64::try_from(jiff::Timestamp::now().as_second()).unwrap();
    fixture.tty_input_at(input_s);
    std::fs::write(&fixture.clients, format!("human|%4|{input_s}|root||\n")).unwrap();
    run_success(
        fixture.command().args(["message", "@claude", "go"]),
        "unviewed input",
    );
    let typing = fixture.typing_record();
    assert_eq!(
        (
            fixture.record().status,
            typing["panes"]["tmux:%3"]["atime_s"].as_u64()
        ),
        (MessageStatus::Sent, Some(input_s))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_advance_after_pane_send_is_excluded() {
    let fixture = Fixture::new(false);
    run_success(
        fixture
            .command()
            .args(["pane", "send", "tmux:%3", "explicit input"]),
        "pane send exclusion",
    );
    let input_s = u64::try_from(jiff::Timestamp::now().as_second()).unwrap();
    fixture.tty_input_at(input_s);
    run_success(
        fixture.command().args(["message", "@claude", "go"]),
        "excluded input",
    );
    let typing = fixture.typing_record();
    assert_eq!(
        (
            fixture.record().status,
            typing["panes"]["tmux:%3"]["atime_s"].as_u64()
        ),
        (MessageStatus::Sent, Some(input_s))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn terminal_record_failures_send_without_falling_back_to_the_client_clock() {
    let mut actual = Vec::new();
    for case in ["missing", "stat", "lock"] {
        let fixture = Fixture::new(false);
        let lock = fixture.env.runtime_paths().locks_dir.join("typing.lock");
        let _guard =
            (case == "lock").then(|| rimz::disk::lock::WorkspaceLock::acquire(&lock).unwrap());
        if case == "missing" {
            std::fs::remove_file(fixture.env.runtime_paths().lane_path("typing.json")).unwrap();
        }
        if case == "stat" {
            std::fs::remove_file(&fixture.tty).unwrap();
        }
        std::fs::write(
            &fixture.clients,
            format!(
                "human|%3|{}|root||\n",
                jiff::Timestamp::now().as_second() + 1
            ),
        )
        .unwrap();
        run_success(fixture.command().args(["message", "@claude", "go"]), case);
        actual.push((case, fixture.record().status, fixture.reads()));
    }
    assert_eq!(
        actual,
        [
            ("missing", MessageStatus::Sent, 0),
            ("stat", MessageStatus::Sent, 0),
            ("lock", MessageStatus::Sent, 2),
        ]
    );
}

/// Runs the hidden timeout helper on `kind`'s overdue subagent run while the
/// child's pane is being typed in, and returns the run and the stop the typing
/// hold queued: claude replays it as an interrupt, kimi as a steer.
fn held_deadline_stop(kind: &str) -> (Fixture, rimz::store::run::RunRecord, MessageRecord) {
    let fixture = Fixture::for_kind(kind, true);
    fixture.short_hold();
    let env = &fixture.env;
    let mut run = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked(kind),
        rimz::agents::PermissionMode::Auto,
        "task".into(),
        env.project_root.clone(),
    );
    run.status = rimz::store::run::RunStatus::Running;
    run.subagent = true;
    run.agent_id = Some(AgentSessionId::from("typing-session"));
    run.timeout = Some(Duration::from_secs(1800));
    run.deadline_at = Some(jiff::Timestamp::now() - Duration::from_secs(11));
    run.grace = Some(Duration::from_secs(180));
    rimz::harness::run::create(env.store().paths(), &run).unwrap();
    let request = rimz::harness::run_timeout::RunTimeoutRequest {
        workspace_id: env.workspace_id.clone(),
        run_id: run.run_id.clone(),
        interrupted_at: None,
    };
    run_success(
        fixture
            .command()
            .args(rimz::child_process::agent_helper_argv(
                "run-timeout",
                &request,
            )),
        "deadline stop helper",
    );
    let loaded = rimz::harness::run::load(env.store().paths(), &run.run_id).unwrap();
    assert_eq!(loaded.deadline_notice_at, run.deadline_at, "{kind}");
    let stop = fixture.record();
    assert!(stop.text.starts_with("Time is up."), "{stop:?}");
    assert_eq!(stop.status, MessageStatus::Queued, "{kind}");
    let replay = if kind == "claude" {
        "interrupt"
    } else {
        "steer"
    };
    let value = serde_json::to_value(&stop).unwrap();
    assert_eq!(value["typing_hold"]["replay"], replay, "{kind}");
    assert_eq!(fixture.writes(), 0, "{kind}");
    (fixture, run, stop)
}

fn sweep_withdraws_the_stop(fixture: &Fixture, stop: &MessageRecord, reason: &str) {
    fixture.expire(stop);
    run_success(fixture.command().args(["message", "sweep"]), "replay sweep");
    assert_eq!(fixture.writes(), 0, "{reason}");
    let history = fixture.env.store().list_message_history().unwrap();
    let row = history
        .iter()
        .find(|row| row.message_id == stop.message_id)
        .expect("the stop is withdrawn");
    assert_eq!(row.status, MessageStatus::Canceled);
    assert_eq!(row.attempts, 0);
    let events = fixture.env.read_events();
    assert!(
        events.iter().any(|event| event.method == "message.canceled"
            && event.params_value()["message_id"] == stop.message_id.as_str()
            && event.params_value()["reason"] == reason),
        "{events:?}"
    );
}

#[test]
fn a_held_deadline_stop_is_withdrawn_once_its_run_ends() {
    for kind in ["claude", "kimi"] {
        let (fixture, run, stop) = held_deadline_stop(kind);
        let store = fixture.env.store();
        rimz::harness::run::cancel(store.paths(), store.runtime_paths(), &run.run_id).unwrap();
        sweep_withdraws_the_stop(&fixture, &stop, "child run canceled before delivery");
    }
}

#[test]
fn a_held_deadline_stop_is_withdrawn_once_its_run_reopens() {
    for kind in ["claude", "kimi"] {
        let (fixture, run, stop) = held_deadline_stop(kind);
        let store = fixture.env.store();
        let paths = store.paths().clone();
        // A raw cancel skips the terminal write's withdrawal, standing in for a run that
        // reopened before that withdrawal read it.
        cancel_run_past_its_owner(&fixture.env, &run.run_id);
        let observation = AgentLifecycleObservation::new(
            Some("typing-session".into()),
            LifecycleSignal::TurnStarted { turn_id: None },
        );
        rimz::harness::run::record_lifecycle(&paths, &run.run_id, kind, &observation, None, || {
            None
        })
        .unwrap();
        let reopened = rimz::harness::run::load(&paths, &run.run_id).unwrap();
        assert_eq!(reopened.status, rimz::store::run::RunStatus::Running);
        assert_eq!(reopened.deadline_notice_at, None, "{kind}");
        sweep_withdraws_the_stop(&fixture, &stop, "deadline stop superseded before delivery");
    }
}

#[test]
fn a_held_deadline_stop_replays_once_while_its_run_holds_the_stop() {
    for kind in ["claude", "kimi"] {
        let (fixture, run, stop) = held_deadline_stop(kind);
        assert_eq!(stop.about_run.as_ref(), Some(&run.run_id), "{kind}");
        fixture.expire(&stop);
        run_success(fixture.command().args(["message", "sweep"]), "replay sweep");
        let sent = message_by_id(&fixture.env, &stop.message_id);
        assert_eq!(sent.status, MessageStatus::Sent, "{kind}");
        let pastes = trace_lines(&fixture.trace)
            .iter()
            .filter(|line| line.starts_with("load-buffer "))
            .count();
        assert_eq!(pastes, 1, "{kind}");
    }
}
