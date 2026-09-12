//! Stage hand-offs through the CLI, durable store, and native hook delivery boundary.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use rimz::store::event::{
    AgentLaunchPayload, AgentLaunchState, EventEnvelope, EventKind, SignalEventPayload,
    SignalSource,
};
use rimz::store::message::{
    DeliveryGate, HarnessNotice, MessageBody, MessageSender, MessageStatus,
};
use serde_json::json;

use crate::common::Env;

const BOARD: &str = "# Work\nStage: Build (@coder)\n\n## Progress log\n- existing entry\n\n## Evidence\nKeep this section.\n";
const CONFIG: &str = r#"
[agents.teams.forge]
stages = ["Build", "Review"]
[[agents.teams.forge.roles]]
role = "coder"
profile = "claude"
owns = ["Build"]
compact-on-handoff = true
[[agents.teams.forge.roles]]
role = "reviewer"
profile = "claude"
owns = ["Review"]
"#;

struct Fixture {
    env: Env,
    panes: PathBuf,
    trace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let env = Env::new();
        env.install_agent_hooks("claude");
        let config = env.config_root().join("rimz/agents.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(config, CONFIG).unwrap();
        let panes = env.write_pane_fixture(&[]);
        let trace = env.project_root.join("stage-mux.log");
        Self { env, panes, trace }
    }

    fn board(&self) -> PathBuf {
        self.env.project_root.join("blackboard.md")
    }

    fn command(&self) -> Command {
        let mut command = self.env.rimz();
        command
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", &self.trace)
            .env("RIMZ_TEST_PANE_LIST", &self.panes)
            .env("RIMZ_MESSAGE_SETTLE_MS", "0")
            .env(rimz::workspace::ENV_CHANNEL, "feature-team")
            .args(["--mux", "zellij"]);
        command
    }

    fn seed(&self, role: &str, parent: Option<&str>) {
        let workspace = rimz::WorkspaceResolver::resolve(&self.env.project_root, None).unwrap();
        self.env
            .store()
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id,
                &workspace.session_name,
                &AgentKind::new_unchecked("claude"),
                AgentLaunchPayload {
                    agent_id: AgentSessionId::from(format!("launch_{role}")),
                    launch_id: Some(AgentSessionId::from(format!("launch_{role}"))),
                    agent_name: role.to_owned(),
                    agent_name_explicit: true,
                    launch: rimz::agents::LaunchParams {
                        team: Some("forge".to_owned()),
                        role: Some(role.to_owned()),
                        channel: Some("feature-team".to_owned()),
                        parent_agent_id: parent.map(AgentSessionId::from),
                        parent_agent_kind: parent.map(|_| AgentKind::new_unchecked("claude")),
                        launch_depth: parent.map(|_| 1),
                        ..Default::default()
                    },
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: None,
                    worktree_path: Some(self.env.project_root.display().to_string()),
                    worktree_branch: Some("feature-team".to_owned()),
                    prompt: None,
                    description: None,
                },
            ))
            .unwrap();
    }

    fn hook(&self, role: &str, event: &str, pane: Option<&str>) {
        let mut command = self.command();
        command
            .args(["hooks", "feed", "--source", "claude"])
            .env(rimz::harness::launch::ENV_AGENT_NAME, role)
            .env("RIMZ_AGENT_PID", self.env.agent_owner_pid().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(pane) = pane {
            command.env("ZELLIJ_PANE_ID", pane.strip_prefix("terminal_").unwrap());
        }
        let output = self
            .env
            .spawn_payload(
                command,
                &json!({
                    "hook_event_name": event,
                    "session_id": role,
                    "cwd": self.env.project_root,
                    "worktree_path": self.env.project_root,
                    "worktree_branch": "feature-team",
                    "prompt": "work"
                })
                .to_string(),
            )
            .wait_with_output()
            .unwrap();
        success(output);
    }

    fn running(&self, role: &str, pane: Option<&str>) {
        self.seed(role, None);
        self.hook(role, "SessionStart", pane);
        self.hook(role, "UserPromptSubmit", pane);
    }

    fn live_panes(&self, ids: &[&str]) {
        let panes = ids
            .iter()
            .map(|pane| rimz::pane::PaneRef {
                pane_id: PaneId::from_parts(MuxName::Zellij, pane),
                session_name: "rimz-test".to_owned(),
                view_id: Some("tab_1".to_owned()),
                view_kind: Some(rimz::ids::ViewKind::Tab),
                view_name: Some("project".to_owned()),
                title: None,
                is_floating: false,
                command: Some("claude".to_owned()),
                foreground_cmdline: None,
                spawn_command: None,
                cwd: Some(self.env.project_root.display().to_string()),
                pane_pid: None,
                pane_process_start: None,
                hosted_agent_kind: None,
                hosted_agent_process_start: None,
                resumed_session_id: None,
                elevated_agent: None,
                first_seen_at_ms: None,
            })
            .collect::<Vec<_>>();
        self.env.write_pane_fixture(&panes);
    }

    fn flip(&self, to: &str, caller: Option<&str>, note: Option<&str>) -> Output {
        let mut command = self.command();
        command.args(["teams", "flip", to, "--team", "forge"]);
        if let Some(caller) = caller {
            command
                .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
                .env(
                    rimz::harness::launch::ENV_AGENT_ID,
                    format!("launch_{caller}"),
                );
        }
        if let Some(note) = note {
            command.args(["--note", note]);
        }
        command.output().unwrap()
    }

    fn signals(&self) -> Vec<SignalEventPayload> {
        self.env
            .read_events()
            .iter()
            .filter_map(|event| match event.kind() {
                EventKind::Signal(signal) if signal.name.as_str() == "team.stage" => Some(signal),
                _ => None,
            })
            .collect()
    }
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn flip_cli_persists_board_signal_and_owner_note() {
    let fixture = Fixture::new();
    fixture.running("coder", None);
    fixture.running("reviewer", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let note = "Review the consumer boundary.\nKeep the evidence.";
    let output = success(fixture.flip("Review", None, Some(note)));
    assert!(
        output.contains("flipped Build -> Review (@reviewer)"),
        "{output}"
    );
    insta::assert_snapshot!(output.replace(fixture.board().to_str().unwrap(), "<board>"), @"
    flipped Build -> Review (@reviewer) in forge#feature-team
      board    <board>
      message  queued for @reviewer#feature-team, delivers at its next turn boundary
    ");
    let board = std::fs::read_to_string(fixture.board()).unwrap();
    assert!(board.contains("Stage: Review (@reviewer)\n"));
    assert!(
        board.contains(
            "@user: Build -> Review — Review the consumer boundary. Keep the evidence.\n"
        )
    );
    assert!(board.ends_with("\n## Evidence\nKeep this section.\n"));
    assert!(board.contains("- existing entry\n"));
    let signals = fixture.signals();
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].source, SignalSource::Team);
    let payload = &signals[0].payload;
    assert_eq!(payload["instance"], "forge#feature-team");
    assert_eq!(payload["from"], "Build");
    assert_eq!(payload["to"], "Review");
    assert_eq!(payload["owner"], "reviewer");
    assert_eq!(payload["by"], "user");
    assert_eq!(
        payload["board"],
        json!(fixture.board().canonicalize().unwrap())
    );
    assert_eq!(payload["note"], note);
    let messages = fixture.env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].agent_id.as_str(), "reviewer");
    assert_eq!(messages[0].gate, DeliveryGate::Done);
    assert_eq!(
        messages[0].sender,
        MessageSender::Harness {
            notice: HarnessNotice::Signal
        }
    );
    assert!(messages[0].text.contains("stage Review is yours"));
    assert!(messages[0].text.ends_with(note));
}

#[test]
fn registration_rewake_reaches_stop_and_message_sweep_consumer() {
    let fixture = Fixture::new();
    std::fs::write(fixture.board(), BOARD).unwrap();
    fixture.seed("coder", None);
    fixture.hook("coder", "SessionStart", None);
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
    assert_eq!(fixture.signals().len(), 1);
    let messages = fixture.env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1);
    let id = messages[0].message_id.clone();
    assert!(messages[0].text.contains("stage Build is still yours"));
    fixture.hook("coder", "UserPromptSubmit", Some("terminal_3"));
    fixture.live_panes(&["terminal_3"]);
    success(
        fixture
            .command()
            .args(["message", "sweep"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fixture.env.store().list_pending_messages().unwrap()[0].message_id,
        id
    );
    fixture.hook("coder", "Stop", Some("terminal_3"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        success(
            fixture
                .command()
                .args(["message", "sweep"])
                .output()
                .unwrap(),
        );
        let messages = fixture.env.store().list_messages().unwrap();
        let status = messages
            .iter()
            .find(|message| message.message_id == id)
            .unwrap()
            .status;
        if status == MessageStatus::Sent {
            break;
        }
        assert!(Instant::now() < deadline, "re-wake remained {status}");
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        fixture
            .env
            .store()
            .list_pending_messages()
            .unwrap()
            .is_empty()
    );
    let trace = std::fs::read_to_string(&fixture.trace).unwrap();
    for text in ["Type: SIGNAL", "stage Build is still yours"] {
        let bytes = text
            .bytes()
            .map(|byte| byte.to_string())
            .collect::<Vec<_>>()
            .join("\t");
        assert!(
            trace.contains(&bytes),
            "missing {text} in mux writes: {trace}"
        );
    }
    assert_eq!(fixture.signals().len(), 1, "Stop must not reopen the stage");
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
}

#[test]
fn steer_flip_delivers_while_the_owner_is_running() {
    let fixture = Fixture::new();
    fixture.running("reviewer", Some("terminal_3"));
    fixture.live_panes(&["terminal_3"]);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let output = success(
        fixture
            .command()
            .args([
                "teams",
                "flip",
                "Review",
                "--team",
                "forge",
                "--steer",
                "-m",
                "review now",
            ])
            .output()
            .unwrap(),
    );
    assert!(
        output.contains("message  steered @reviewer#feature-team"),
        "{output}"
    );
    let messages = fixture.env.store().list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].gate, DeliveryGate::Any);
    assert_eq!(messages[0].status, MessageStatus::Sent);
    assert!(messages[0].text.ends_with("review now"));
    let trace = std::fs::read_to_string(&fixture.trace).unwrap();
    let bytes = "stage Review is yours"
        .bytes()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join("\t");
    assert!(
        trace.contains(&bytes),
        "stage notice was not sent to the running owner: {trace}"
    );
}

#[test]
fn flip_precondition_errors_leave_board_signals_and_queue_unchanged() {
    let fixture = Fixture::new();
    fixture.running("coder", None);
    for (board, stage, expected) in [
        (Some(BOARD), "Unknown", "unknown stage"),
        (None, "Review", "no board"),
        (Some("# Work\nNo stage yet.\n"), "Review", "no Stage: line"),
    ] {
        match board {
            Some(board) => std::fs::write(fixture.board(), board).unwrap(),
            None => std::fs::remove_file(fixture.board()).unwrap(),
        }
        let before = fixture.signals();
        let output = fixture.flip(stage, None, None);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(fixture.board()).ok().as_deref(),
            board
        );
        assert_eq!(fixture.signals(), before);
        assert!(fixture.env.store().list_messages().unwrap().is_empty());
    }
    std::fs::write(fixture.board(), BOARD).unwrap();
    for (config, expected) in [
        (
            CONFIG
                .replace("owns = [\"Build\"]", "owns = []")
                .replace("owns = [\"Review\"]", "owns = []"),
            "has no stage owners",
        ),
        (
            CONFIG.replace("owns = [\"Review\"]", "owns = []"),
            "has no owner",
        ),
        (
            CONFIG.replace("owns = [\"Review\"]", "owns = [\"Build\"]"),
            "duplicate owner",
        ),
    ] {
        std::fs::write(fixture.env.config_root().join("rimz/agents.toml"), config).unwrap();
        let output = fixture.flip("Review", None, None);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
        assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
        assert!(fixture.signals().is_empty());
        assert!(fixture.env.store().list_messages().unwrap().is_empty());
    }
}

#[test]
fn concurrent_flips_keep_the_board_ledger_and_signals_in_one_order() {
    let fixture = Fixture::new();
    fixture.running("coder", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let children = ["Review", "Done"].map(|stage| {
        fixture
            .command()
            .args(["teams", "flip", stage, "--team", "forge"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    });
    for child in children {
        success(child.wait_with_output().unwrap());
    }
    let signals = fixture.signals();
    assert_eq!(signals.len(), 2);
    assert_eq!(signals[0].payload["from"], "Build");
    assert_eq!(signals[1].payload["from"], signals[0].payload["to"]);
    let board = std::fs::read_to_string(fixture.board()).unwrap();
    let mut last_position = 0;
    for signal in &signals {
        let transition = format!(
            "@user: {} -> {}",
            signal.payload["from"].as_str().unwrap(),
            signal.payload["to"].as_str().unwrap()
        );
        let position = board.find(&transition).unwrap();
        assert!(position > last_position);
        last_position = position;
    }
    assert_eq!(
        rimz::harness::scratch::board_stage(&fixture.env.project_root)
            .unwrap()
            .name,
        signals[1].payload["to"].as_str().unwrap()
    );
}

#[test]
fn failed_owner_delivery_reports_partial_flip_and_same_stage_repairs_it() {
    let fixture = Fixture::new();
    fixture.running("reviewer", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    std::fs::remove_file(fixture.env.agent_config_path("claude")).unwrap();
    let output = fixture.flip("Review", None, Some("review ready"));
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("completed board, signal"), "{error}");
    assert!(error.contains("re-run `rimz teams flip Review`"), "{error}");
    insta::assert_snapshot!(error.as_ref(), @"error: completed board, signal; queued delivery requires claude hooks so messages can deliver at turn boundaries; run `rimz hooks install claude`; re-run `rimz teams flip Review` to repeat the signal and delivery");
    assert!(
        std::fs::read_to_string(fixture.board())
            .unwrap()
            .contains("Stage: Review (@reviewer)")
    );
    assert_eq!(fixture.signals().len(), 1);
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
    fixture.env.install_agent_hooks("claude");
    success(fixture.flip("Review", None, Some("review ready")));
    let signals = fixture.signals();
    assert_eq!(signals.len(), 2);
    assert_eq!(signals[1].payload["from"], "Review");
    assert_eq!(
        fixture.env.store().list_pending_messages().unwrap().len(),
        1
    );
}

#[test]
fn self_done_and_absent_owner_do_not_compact_or_enqueue() {
    let fixture = Fixture::new();
    fixture.running("coder", Some("terminal_3"));
    fixture.live_panes(&["terminal_3"]);
    std::fs::write(fixture.board(), BOARD).unwrap();
    for (stage, expected) in [
        ("Build", "stage is yours"),
        ("Review", "owner @reviewer is not live"),
        ("Done", "flipped Review -> Done"),
    ] {
        let output = success(fixture.flip(stage, Some("coder"), None));
        assert!(output.contains(expected), "{output}");
        assert!(fixture.env.store().list_messages().unwrap().is_empty());
    }
    let board = std::fs::read_to_string(fixture.board()).unwrap();
    assert!(board.contains("Stage: Done\n"));
    let signals = fixture.signals();
    assert_eq!(signals.len(), 3);
    assert!(!signals[2].payload.contains_key("owner"));
    fixture.hook("coder", "SessionStart", Some("terminal_3"));
    assert_eq!(fixture.signals(), signals);
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
}

#[test]
fn pending_compaction_does_not_repeat_or_prevent_owner_handoff() {
    let fixture = Fixture::new();
    fixture.running("coder", Some("terminal_3"));
    fixture.running("reviewer", None);
    fixture.live_panes(&["terminal_3"]);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let store = fixture.env.store();
    let first = success(fixture.flip("Review", Some("coder"), None));
    assert!(first.contains("compact  queued"), "{first}");
    let messages = store.list_pending_messages().unwrap();
    assert_eq!(messages.len(), 2);
    let pending = messages
        .iter()
        .find(|message| message.body == MessageBody::Command)
        .unwrap();
    assert_eq!(pending.agent_id.as_str(), "coder");
    assert_eq!(pending.gate, DeliveryGate::Done);
    assert!(pending.text.starts_with("/compact"));
    let pending_id = pending.message_id.clone();
    let output = success(fixture.flip("Review", Some("coder"), None));
    assert!(output.contains("compact  skipped"), "{output}");
    let messages = store.list_pending_messages().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.body == MessageBody::Command)
            .count(),
        1
    );
    assert!(
        messages
            .iter()
            .any(|message| message.message_id == pending_id)
    );
    assert!(
        messages
            .iter()
            .any(|message| message.agent_id.as_str() == "reviewer"
                && message.text.contains("stage Review is yours"))
    );
    assert_eq!(fixture.signals().len(), 2);
    assert!(
        std::fs::read_to_string(fixture.board())
            .unwrap()
            .contains("Stage: Review (@reviewer)")
    );
}

#[test]
fn child_registration_does_not_rewake_owned_stage() {
    let fixture = Fixture::new();
    std::fs::write(fixture.board(), BOARD).unwrap();
    fixture.seed("coder", Some("parent"));
    fixture.hook("coder", "SessionStart", None);
    assert!(fixture.signals().is_empty());
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
}

#[test]
fn absent_owner_is_rewoken_when_its_root_session_registers() {
    let fixture = Fixture::new();
    fixture.running("coder", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    success(fixture.flip("Review", None, None));
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
    let board = std::fs::read_to_string(fixture.board()).unwrap();
    fixture.hook("coder", "SessionStart", None);
    assert_eq!(
        fixture.signals().len(),
        1,
        "non-owner registration must not reopen the stage"
    );
    fixture.seed("reviewer", None);
    fixture.hook("reviewer", "SessionStart", None);
    let signals = fixture.signals();
    assert_eq!(signals.len(), 2);
    assert_eq!(signals[1].source, SignalSource::Team);
    assert_eq!(signals[1].payload["from"], "Review");
    assert_eq!(signals[1].payload["to"], "Review");
    assert_eq!(signals[1].payload["by"], "rimz");
    let messages = fixture.env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].agent_id.as_str(), "reviewer");
    assert!(messages[0].text.contains("stage Review is still yours"));
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), board);
}
