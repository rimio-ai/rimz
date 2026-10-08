//! Board commands through the CLI, durable store, and native hook delivery boundary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};
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
const CONFIG: &str = r#"leader: coder
stages: [Build, Review]
roles:
  - role: coder
    agent: worker
    owns: [Build]
    flip-compact: 100k
  - role: reviewer
    agent: worker
    owns: [Review]
"#;

#[test]
fn record_bootstraps_without_a_cohort_or_side_effects_then_flip_uses_the_board() {
    let fixture = Fixture::new();
    let receipt = success(
        fixture
            .command()
            .args(["teams", "record", "goal", "Ship it."])
            .output()
            .unwrap(),
    );
    assert_eq!(receipt, "recorded Goal @user\n");
    let bootstrapped = std::fs::read_to_string(fixture.board()).unwrap();
    let entry = bootstrapped
        .strip_prefix("# Blackboard\n\n## Goal\n")
        .and_then(|rest| rest.strip_suffix("\n## Decisions\n\n## Progress\n\n## Result\n"))
        .unwrap_or_else(|| panic!("{bootstrapped}"));
    assert!(entry.ends_with(" @user: Ship it.\n"), "{entry}");
    assert!(fixture.signals().is_empty());
    assert!(
        fixture
            .env
            .store()
            .list_pending_messages()
            .unwrap()
            .is_empty()
    );
    fixture.seed("coder", None);
    success(fixture.flip("Build", Some("coder"), Some("Start.")));
    let board = std::fs::read_to_string(fixture.board()).unwrap();
    assert!(board.starts_with("# Blackboard\nStage: Build (@coder)\n"));
    assert!(board.contains(entry));
    assert_eq!(board.matches("## Progress").count(), 1);
    assert!(board.contains("@coder: opened Build"));
}

#[test]
fn record_member_multiline_stdin_and_file_print_one_line_receipts() {
    let fixture = Fixture::new();
    fixture.seed("coder", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let mut command = fixture.command();
    command
        .args(["teams", "record", "Goal", "--stdin"])
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "launch_coder")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = fixture
        .env
        .spawn_payload(command, "first\r\n## Injected\n\nStage: Done\n")
        .wait_with_output()
        .unwrap();
    assert_eq!(success(output), "recorded Goal @coder\n");
    let board = std::fs::read_to_string(fixture.board()).unwrap();
    assert!(
        board.contains("@coder: first\n  ## Injected\n\n  Stage: Done\n"),
        "{board}"
    );
    assert!(board.contains("Stage: Build (@coder)"));
    assert!(!board.contains("\n## Injected"));
    let file = fixture.env.project_root.join("entry.txt");
    std::fs::write(&file, "Outcome\nnext").unwrap();
    let receipt = success(
        fixture
            .command()
            .args(["teams", "record", "Result", "--file"])
            .arg(file)
            .output()
            .unwrap(),
    );
    assert_eq!(receipt, "recorded Result @user\n");
    assert!(
        std::fs::read_to_string(fixture.board())
            .unwrap()
            .contains("@user: Outcome\n  next\n")
    );
    assert!(fixture.signals().is_empty());
    assert!(
        fixture
            .env
            .store()
            .list_pending_messages()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn record_refusals_leave_board_unchanged() {
    let fixture = Fixture::new();
    std::fs::write(fixture.board(), BOARD).unwrap();
    for (section, text, expected) in [
        (
            "Progress",
            "x",
            "`Progress` belongs to `rimz teams flip`; record takes Goal, Decisions, or Result",
        ),
        (
            "Progress log",
            "x",
            "`Progress log` belongs to `rimz teams flip`; record takes Goal, Decisions, or Result",
        ),
        (
            "Stage",
            "x",
            "`Stage` belongs to `rimz teams flip`; record takes Goal, Decisions, or Result",
        ),
        (
            "Evidence",
            "x",
            "unknown board section `Evidence`; choose Goal, Decisions, or Result",
        ),
        (
            "Goal",
            " \r\n ",
            "provide nonblank text for the board entry",
        ),
    ] {
        let output = fixture
            .command()
            .args(["teams", "record", section, text])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{output:?}"
        );
        assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
    }
}

#[test]
fn record_refuses_a_member_of_another_worktree() {
    let fixture = Fixture::new();
    let elsewhere = tempfile::tempdir().unwrap();
    fixture.seed_member(
        "claude",
        "coder",
        "coder",
        None,
        "elsewhere",
        elsewhere.path(),
    );
    let output = fixture
        .command()
        .args(["teams", "record", "Goal", "x"])
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "launch_coder")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("calling agent belongs to a team cohort in another worktree"),
        "{output:?}"
    );
    assert!(!fixture.board().exists());
}

#[test]
fn done_flip_stops_checkout_server_but_review_does_not() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let fixture = Fixture::new();
    fixture.running("coder", None);
    fixture.running("reviewer", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let config = serde_json::from_value(json!({
        "command": [crate::common::cargo_bin("lsp-server-stub", env!("CARGO_BIN_EXE_lsp-server-stub"))],
        "extensions": ["rs"], "root-markers": ["Cargo.toml"], "memory-estimate": "1M"
    })).unwrap();
    let request = rimz::lsp::admission::ServeRequest {
        root: fixture.env.project_root.canonicalize().unwrap(),
        project: fixture.env.project_root.clone(),
        server: "rust".into(),
        settings_hash: rimz::lsp::history::settings_hash(&config),
        config,
        policy: rimz::config::LspConfig {
            reserve_min: "0".into(),
            reserve_percent: 0,
            kill_floor_percent: 0,
            ..Default::default()
        },
        eager: false,
    };
    let (mut broker, directory) = super::lsp::spawn_test_broker(&fixture.env, &request);
    let rpc = |value: serde_json::Value| -> serde_json::Value {
        let mut stream = UnixStream::connect(directory.join("sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(stream, "{value}").unwrap();
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    };
    assert_eq!(
        rpc(
            json!({"op": "query", "method": "workspace/symbol", "params": {"query": "ready"}, "wait_ms": 4000})
        )["result"],
        json!([])
    );
    success(fixture.flip("Review", None, Some("Review it.")));
    assert_eq!(rpc(json!({"op": "status"}))["state"], "ready");
    success(fixture.flip("Done", None, Some("Finished.")));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let status = rpc(json!({"op": "status"}));
        if status["state"]["dormant"]["reason"] == "team done" && status["server_pid"].is_null() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Done did not stop server: {status}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        rpc(json!({"op": "stop", "reason": "checkout removed"}))["ok"],
        true
    );
    broker.wait().unwrap();
}

struct Fixture {
    env: Env,
    panes: PathBuf,
    trace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        crate::common::write_definition(
            &env,
            "agents",
            "claude",
            "description: Claude base",
            "Follow instructions.",
        );
        crate::common::write_definition(
            &env,
            "agents",
            "worker",
            "description: Worker\nagent: claude\ntools: []",
            "",
        );
        crate::common::write_definition(&env, "teams", "forge", CONFIG, "Complete the work.");
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
            .env(rimz::workspace::ENV_WORKTREE_PATH, &self.env.project_root)
            .env(rimz::workspace::ENV_CHANNEL, "feature-team")
            .args(["--mux", "zellij"]);
        command
    }

    fn seed(&self, role: &str, parent: Option<&str>) {
        self.seed_member(
            "claude",
            role,
            role,
            parent,
            "feature-team",
            &self.env.project_root,
        );
    }

    fn seed_member(
        &self,
        kind: &str,
        name: &str,
        role: &str,
        parent: Option<&str>,
        channel: &str,
        worktree: &Path,
    ) {
        let workspace = self.env.resolve_workspace(&self.env.project_root);
        self.env
            .store()
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id,
                &workspace.session_name,
                &AgentKind::new_unchecked(kind),
                AgentLaunchPayload {
                    agent_id: AgentSessionId::from(format!("launch_{name}")),
                    launch_id: Some(AgentSessionId::from(format!("launch_{name}"))),
                    agent_name: name.to_owned(),
                    agent_name_explicit: true,
                    launch: rimz::agents::LaunchParams {
                        team: Some("forge".to_owned()),
                        role: Some(role.to_owned()),
                        channel: Some(channel.to_owned()),
                        parent_agent_id: parent.map(AgentSessionId::from),
                        parent_agent_kind: parent.map(|_| AgentKind::new_unchecked("claude")),
                        launch_depth: parent.map(|_| 1),
                        ..Default::default()
                    },
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: None,
                    runtime_owner: None,
                    worktree_path: Some(worktree.display().to_string()),
                    worktree_branch: Some(channel.to_owned()),
                    prompt: None,
                    description: None,
                },
            ))
            .unwrap();
    }

    /// Seed a live row launched by the agent `launched_by`: a team seat when
    /// `role` is set, a plain agent otherwise.
    fn seed_launched(&self, name: &str, role: Option<&str>, launched_by: Option<&str>) {
        let workspace = self.env.resolve_workspace(&self.env.project_root);
        self.env
            .store()
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id,
                &workspace.session_name,
                &AgentKind::new_unchecked("claude"),
                AgentLaunchPayload {
                    agent_id: AgentSessionId::from(format!("launch_{name}")),
                    launch_id: Some(AgentSessionId::from(format!("launch_{name}"))),
                    agent_name: name.to_owned(),
                    agent_name_explicit: true,
                    launch: rimz::agents::LaunchParams {
                        team: role.map(|_| "forge".to_owned()),
                        role: role.map(ToOwned::to_owned),
                        channel: Some("feature-team".to_owned()),
                        launch_depth: launched_by.map(|_| 1),
                        launched_by: launched_by.map(|launcher| {
                            Box::new(rimz::agents::LaunchedBy {
                                kind: AgentKind::new_unchecked("claude"),
                                agent_id: AgentSessionId::from(format!("launch_{launcher}")),
                            })
                        }),
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
        self.hook(name, "SessionStart", None);
        self.hook(name, "UserPromptSubmit", None);
    }

    fn hook(&self, role: &str, event: &str, pane: Option<&str>) {
        self.feed(role, event, pane, None);
    }

    /// One hook's stdout; `runtime_env` feeds it as a launch stamped with the runtime switch
    /// does, from the provider's current directory.
    fn feed(
        &self,
        role: &str,
        event: &str,
        pane: Option<&str>,
        runtime_env: Option<&Path>,
    ) -> String {
        self.feed_from("claude", role, role, event, pane, runtime_env, "work")
    }

    #[allow(clippy::too_many_arguments)]
    fn feed_from(
        &self,
        source: &str,
        name: &str,
        session: &str,
        event: &str,
        pane: Option<&str>,
        runtime_env: Option<&Path>,
        prompt: &str,
    ) -> String {
        let mut command = self.command();
        if let Some(cwd) = runtime_env {
            command
                .env(rimz::harness::launch::ENV_RUNTIME_ENV, "1")
                .current_dir(cwd);
        }
        command
            .args(["hooks", "feed", "--source", source])
            .env(rimz::harness::launch::ENV_AGENT_NAME, name)
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
                    "session_id": session,
                    "cwd": runtime_env.unwrap_or(&self.env.project_root),
                    "worktree_branch": "feature-team",
                    "prompt": prompt
                })
                .to_string(),
            )
            .wait_with_output()
            .unwrap();
        success(output)
    }

    /// Seed a `kind` member of `role` whose session `sess-<role>` registered and then ended,
    /// with no hook reactor running, as a crash leaves it.
    fn ended(&self, kind: &str, role: &str) -> AgentSessionId {
        let workspace = self.env.resolve_workspace(&self.env.project_root);
        let session = AgentSessionId::from(format!("sess-{role}"));
        for signal in [LifecycleSignal::Registered, LifecycleSignal::Ended] {
            let mut observation = AgentLifecycleObservation::new(Some(session.clone()), signal);
            observation.agent_name = Some(role.to_owned());
            observation.launch = rimz::agents::LaunchParams {
                team: Some("forge".to_owned()),
                role: Some(role.to_owned()),
                channel: Some("feature-team".to_owned()),
                ..Default::default()
            };
            observation.worktree_path = Some(self.env.project_root.display().to_string());
            observation.worktree_branch = Some("feature-team".to_owned());
            self.env
                .store()
                .append_event(&EventEnvelope::agent_lifecycle(
                    workspace.workspace_id.clone(),
                    &workspace.session_name,
                    kind,
                    "seed",
                    &observation,
                ))
                .unwrap();
        }
        session
    }

    /// Resume `session` through the exec wrapper in pane `terminal_3`, with a provider that
    /// stays up, and return the wrapper once its re-wake has queued its Stage notice.
    fn resume(&self, kind: &str, role: &str, session: &AgentSessionId) -> Wrapper {
        let wrapper = self.spawn_resume(kind, role, session);
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.stage_records().is_empty() {
            assert!(
                Instant::now() < deadline,
                "resume never re-woke the stage: {}\n{:#?}",
                self.wrapper_log(),
                self.env.read_events()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        wrapper
    }

    /// Start the resume wrapper without waiting for anything it writes.
    fn spawn_resume(&self, kind: &str, role: &str, session: &AgentSessionId) -> Wrapper {
        let shims = self.env.home_root.join("provider-shims");
        // The wrapper's informational `--version` probe gets a prompt failure (an unknown
        // version) instead of a provider that sleeps through the probe's timeout.
        crate::common::write_path_shim(
            &shims,
            kind,
            "[ \"$1\" = --version ] && exit 1\nexec sleep 60",
        );
        let stderr = self.wrapper_log_path();
        let mut request = ExecRequest {
            isolation_default: None,
            kind: AgentKind::new_unchecked(kind),
            action: ExecAction::Resume {
                session_id: session.to_string(),
                extra_args: Vec::new(),
            },
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            team_prompt: None,
            loop_reminder: None,
            headless: None,
            skills: None,
            allowed_tools: None,
            provider_account: ProviderAccountState::Unbound,
            run_id: None,
            // Closing the pane on exit keeps the wrapper supervising rather than exec'ing.
            worktree_path: None,
            close_pane_on_exit: true,
            exit_on_run_completion: false,
            subagent: false,
            identity: ExecIdentity::default(),
        };
        request.identity.params = rimz::agents::LaunchParams {
            team: Some("forge".to_owned()),
            role: Some(role.to_owned()),
            channel: Some("feature-team".to_owned()),
            isolation: Some(rimz::config::Isolation::Host),
            ..Default::default()
        };
        Wrapper(
            self.command()
                .args(crate::common::exec_args(&self.env, &request))
                .arg("--root")
                .arg(&self.env.project_root)
                .env("SHELL", "/definitely/not/a/shell")
                .env("PATH", crate::common::path_with_front(&shims))
                .env("ZELLIJ_PANE_ID", "3")
                .env("RUST_LOG", "warn,rimz::cli::agents_cmd::exec=debug")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(&stderr).unwrap())
                .spawn()
                .unwrap(),
        )
    }

    /// Poll the event log until `done` holds.
    fn wait_for(&self, done: impl Fn(&[EventEnvelope]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let events = self.env.read_events();
            if done(&events) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out: {}\n{events:#?}",
                self.wrapper_log()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn wrapper_log_path(&self) -> PathBuf {
        self.env.home_root.join("resume-wrapper.err")
    }

    fn wrapper_log(&self) -> String {
        std::fs::read_to_string(self.wrapper_log_path()).unwrap_or_default()
    }

    /// The wrapper's stderr once it holds `line`.
    fn wrapper_log_until(&self, line: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let log = self.wrapper_log();
            if log.contains(line) {
                return log;
            }
            assert!(
                Instant::now() < deadline,
                "no `{line}` in the wrapper log: {log}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Sweep until the record `id` reaches `status`.
    fn sweep_until(&self, id: &rimz::ids::MessageId, status: MessageStatus) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            success(self.command().args(["message", "sweep"]).output().unwrap());
            let current = self
                .env
                .store()
                .list_messages()
                .unwrap()
                .into_iter()
                .find(|message| &message.message_id == id)
                .map(|message| message.status);
            if current == Some(status) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{id} remained {current:?}: {}",
                String::from_utf8_lossy(
                    &self
                        .command()
                        .args(["message", "show", id.as_str()])
                        .output()
                        .unwrap()
                        .stdout
                )
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn stage_records(&self) -> Vec<rimz::store::message::MessageRecord> {
        let store = self.env.store();
        store
            .list_messages()
            .unwrap()
            .into_iter()
            .chain(store.list_message_history().unwrap())
            .filter(|message| {
                message.sender
                    == MessageSender::Harness {
                        notice: HarnessNotice::Stage,
                    }
            })
            .collect()
    }

    fn running(&self, role: &str, pane: Option<&str>) {
        self.seed(role, None);
        self.hook(role, "SessionStart", pane);
        self.hook(role, "UserPromptSubmit", pane);
    }

    fn live_panes(&self, ids: &[&str]) {
        self.live_panes_running("claude", ids);
    }

    fn live_panes_running(&self, command: &str, ids: &[&str]) {
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
                command: Some(command.to_owned()),
                foreground_cmdline: None,
                spawn_command: None,
                cwd: Some(self.env.project_root.display().to_string()),
                pane_pid: None,
                pane_process_start: None,
                hosted_agent_kind: None,
                hosted_agent_process_start: None,
                hosted_agent_lineage: Vec::new(),
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
            command.arg(note);
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

    fn context_tokens(&self, role: &str, used: u64) {
        let mut context = rimz::agents::AgentContext::new("claude", jiff::Timestamp::now());
        context.tokens = Some(rimz::agents::AgentTokenUsage {
            context_window_size: Some(200_000),
            current_usage: Some(rimz::agents::AgentCurrentUsage {
                input_tokens: Some(used),
                ..Default::default()
            }),
            ..Default::default()
        });
        let record =
            rimz::agents::context::record::AgentContextRecord::new("claude", role, context);
        rimz::store::agent_context::write_record(&self.env.runtime_paths(), &record).unwrap();
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
    let note = "Review the consumer boundary.\rKeep the evidence.\nReady.";
    let output = success(fixture.flip("Review", None, Some(note)));
    assert!(
        output.contains("Flipped Build -> Review by @user"),
        "{output}"
    );
    insta::assert_snapshot!(output.replace(fixture.env.project_root.file_name().unwrap().to_str().unwrap(), "<worktree>"), @"
    Flipped Build -> Review by @user  (forge#feature-team · <worktree>)
      Build → [Review] → Done
      owner    @reviewer, woken at its next turn boundary
    ");
    let board = std::fs::read_to_string(fixture.board()).unwrap();
    assert!(board.contains("Stage: Review (@reviewer)\n"));
    assert!(board.contains(
        "@user: Build -> Review — Review the consumer boundary. Keep the evidence. Ready.\n"
    ));
    assert!(board.ends_with("\n## Evidence\nKeep this section.\n"));
    assert!(board.contains("- existing entry\n"));
    let stamp = board
        .lines()
        .find_map(|line| {
            line.strip_prefix("- ")?
                .split_once(" @user:")
                .map(|(stamp, _)| stamp)
        })
        .unwrap();
    jiff::Timestamp::strptime("%Y-%m-%dT%H:%M:%S%:z", stamp).unwrap();
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
    let flips = rimz::harness::team_stage::stage_flips(&fixture.env.store()).unwrap();
    assert_eq!(flips.len(), 1);
    assert_eq!(flips[0].channel(), "feature-team");
    assert_eq!(flips[0].from.as_deref(), Some("Build"));
    assert_eq!(flips[0].to, "Review");
    assert_eq!(flips[0].by, "user");
    assert_eq!(flips[0].note.as_deref(), Some(note));
    let messages = fixture.env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].agent_id.as_str(), "reviewer");
    assert_eq!(messages[0].gate, DeliveryGate::Done);
    assert_eq!(
        messages[0].sender,
        MessageSender::Harness {
            notice: HarnessNotice::Stage
        }
    );
    assert_eq!(
        messages[0].text,
        format!(
            "@user flipped the stage Build -> Review. Review is yours: pick it up from blackboard.md.\n\nNote: {note}\n\nYour report goes in your stage file and anything for the user to @coder; end the turn with the flip and no pane text."
        )
    );
    assert!(!messages[0].text.lines().any(|line| line.starts_with('{')));
}

#[test]
fn transcript_renders_flips_in_the_lane() {
    let fixture = Fixture::new();
    fixture.running("reviewer", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    success(fixture.flip("Review", None, Some("Review the seam.")));

    let json: serde_json::Value = serde_json::from_str(&success(
        fixture
            .command()
            .args(["transcript", "#feature-team", "--json"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    let flip = json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry.get("stage").is_some())
        .expect("flip entry");
    assert_eq!(flip["from"], "user");
    assert_eq!(flip["text"], "Review the seam.");
    assert_eq!(flip["stage"]["from"], "Build");
    assert_eq!(flip["stage"]["to"], "Review");
    let human = success(
        fixture
            .command()
            .args(["transcript", "#feature-team", "--color", "never"])
            .output()
            .unwrap(),
    );
    // The user's flip continues the user's own prompt line from `running`.
    let flip_line = human
        .lines()
        .skip_while(|line| *line != "work")
        .nth(1)
        .unwrap_or_default();
    assert!(flip_line.starts_with("⇢ Build → Review  "), "{human}");
    assert!(flip_line.ends_with("  Review the seam."), "{human}");
}

#[test]
fn prompt_submit_lists_a_memory_file_written_after_launch_once() {
    let fixture = Fixture::new();
    fixture.seed("coder", None);
    fixture.hook("coder", "SessionStart", None);
    std::fs::write(fixture.env.project_root.join("plan-notes.md"), "plan\n").unwrap();
    assert_eq!(
        fixture.feed("coder", "UserPromptSubmit", None, None),
        "",
        "a launch without the switch"
    );
    let below = fixture.env.project_root.join("crates");
    std::fs::create_dir(&below).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["commit", "-q", "--allow-empty", "-m", "initial"],
    ] {
        let status = Command::new("git")
            .current_dir(&fixture.env.project_root)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let reply: serde_json::Value =
        serde_json::from_str(&fixture.feed("coder", "UserPromptSubmit", None, Some(&below)))
            .unwrap();
    let reply = &reply["hookSpecificOutput"];
    assert_eq!(reply["hookEventName"], "UserPromptSubmit");
    let context = reply["additionalContext"].as_str().unwrap();
    assert!(
        context.starts_with(
            "<system_reminder>\n### Environment\n\nSampled as this prompt was submitted.\n\n```\n$ ls blackboard.md *-notes.md\nplan-notes.md\n```"
        ),
        "{context}"
    );
    assert!(
        context.contains("$ git status --short --branch\n## "),
        "{context}"
    );
    assert!(context.ends_with("</system_reminder>"), "{context}");
    assert_eq!(
        fixture.feed("coder", "UserPromptSubmit", None, Some(&below)),
        "",
        "the second prompt of the same conversation"
    );
}

#[test]
fn registration_rewake_reaches_stop_and_message_sweep_consumer() {
    let fixture = Fixture::new();
    std::fs::write(fixture.board(), BOARD).unwrap();
    fixture.seed("coder", None);
    fixture.hook("coder", "SessionStart", None);
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
    assert_eq!(fixture.signals().len(), 1);
    assert!(
        rimz::harness::team_stage::stage_flips(&fixture.env.store())
            .unwrap()
            .is_empty()
    );
    let messages = fixture.env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1);
    let id = messages[0].message_id.clone();
    assert!(
        messages[0]
            .text
            .contains("The team resumed at stage Build, which is yours.")
    );
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
    for text in [
        "Type: STAGE",
        "The team resumed at stage Build, which is yours.",
    ] {
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
fn flip_requires_a_positional_note_and_rejects_legacy_flags() {
    let fixture = Fixture::new();
    fixture.running("reviewer", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let output = fixture.flip("Review", None, None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("<NOTE>"));
    for note in ["", " ", "\t\r\n", "\u{2003}"] {
        let output = fixture.flip("Review", None, Some(note));
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("nonblank progress note"));
    }
    for flag in ["--note", "-m", "--steer", "--worktree"] {
        let output = fixture
            .command()
            .args([
                "teams",
                "flip",
                "Review",
                "review now",
                "--team",
                "forge",
                flag,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success(), "accepted {flag}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument"));
    }
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
    assert!(fixture.signals().is_empty());
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
}

#[test]
fn flip_bootstraps_a_missing_board_or_stage_without_losing_prose() {
    for existing in [None, Some("# Work\n\n## Evidence\nKeep this section.\n")] {
        let fixture = Fixture::new();
        fixture.running("reviewer", None);
        if let Some(existing) = existing {
            std::fs::write(fixture.board(), existing).unwrap();
        }
        let output = success(fixture.flip("Review", None, Some("Start the review.")));
        assert!(output.contains("Opened Review by @user"), "{output}");
        insta::allow_duplicates! {
        insta::assert_snapshot!(output.replace(fixture.env.project_root.file_name().unwrap().to_str().unwrap(), "<worktree>"), @"
            Opened Review by @user  (forge#feature-team · <worktree>)
              Build → [Review] → Done
              owner    @reviewer, woken at its next turn boundary
            ");
        }
        let board = std::fs::read_to_string(fixture.board()).unwrap();
        assert!(board.contains("Stage: Review (@reviewer)\n"), "{board}");
        assert!(
            board.contains("@user: opened Review — Start the review."),
            "{board}"
        );
        if existing.is_some() {
            assert!(
                board.contains("## Evidence\nKeep this section.\n"),
                "{board}"
            );
        }
        let signals = fixture.signals();
        assert_eq!(signals.len(), 1);
        assert!(!signals[0].payload.contains_key("from"));
        let messages = fixture.env.store().list_pending_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].text,
            "@user opened the stage Review. Review is yours: pick it up from blackboard.md.\n\nNote: Start the review.\n\nYour report goes in your stage file and anything for the user to @coder; end the turn with the flip and no pane text."
        );
    }
}

#[test]
fn user_can_flip_out_of_done_without_compacting_a_member() {
    let fixture = Fixture::new();
    fixture.running("coder", Some("terminal_3"));
    fixture.running("reviewer", None);
    fixture.live_panes(&["terminal_3"]);
    fixture.context_tokens("coder", 150_000);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let done = success(fixture.flip("Done", None, Some("Finished.")));
    assert!(done.contains("Build → Review → [Done]"), "{done}");
    assert!(!done.contains("owner"), "{done}");
    assert!(!done.contains("compact"), "{done}");
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
    let reopened = success(fixture.flip("Review", None, Some("One more review.")));
    assert!(
        reopened.contains("Flipped Done -> Review by @user"),
        "{reopened}"
    );
    assert!(!reopened.contains("compact"), "{reopened}");
    let signals = fixture.signals();
    assert_eq!(signals.len(), 2);
    assert_eq!(signals[1].payload["from"], "Done");
    assert_eq!(signals[1].payload["to"], "Review");
    let messages = fixture.env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].agent_id.as_str(), "reviewer");
    assert_ne!(messages[0].body, MessageBody::Command);
}

#[test]
fn flip_selects_the_worktree_cohort_not_the_current_channel() {
    let fixture = Fixture::new();
    fixture.running("coder", None);
    let other = fixture.env.home_root.join("other-worktree");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("blackboard.md"), BOARD).unwrap();
    std::fs::write(fixture.board(), BOARD).unwrap();
    fixture.seed_member(
        "claude",
        "other-coder",
        "coder",
        None,
        "other-channel",
        &other,
    );
    let output = success(
        fixture
            .command()
            .env(rimz::workspace::ENV_CHANNEL, "other-channel")
            .args(["teams", "flip", "Review", "Local worktree only."])
            .output()
            .unwrap(),
    );
    assert!(output.contains("forge#feature-team"), "{output}");
    assert_eq!(
        std::fs::read_to_string(other.join("blackboard.md")).unwrap(),
        BOARD
    );
    assert!(
        std::fs::read_to_string(fixture.board())
            .unwrap()
            .contains("Stage: Review (@reviewer)")
    );
    let signals = fixture.signals();
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].payload["instance"], "forge#feature-team");
    fixture.seed_member(
        "claude",
        "duplicate-coder",
        "coder",
        None,
        "duplicate-channel",
        &fixture.env.project_root,
    );
    let board = std::fs::read_to_string(fixture.board()).unwrap();
    for select_team in [false, true] {
        let mut command = fixture.command();
        command.args(["teams", "flip", "Build", "Ambiguous cohort."]);
        if select_team {
            command.args(["--team", "forge"]);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8(output.stderr).unwrap();
        insta::allow_duplicates! {
            insta::assert_snapshot!(error.replace(fixture.env.project_root.to_str().unwrap(), "<worktree>"), @"error: team `forge` has multiple live cohorts in worktree <worktree> in channels #duplicate-channel, #feature-team; stop the extra cohorts with `rimz teams stop forge -w <channel>` before flipping");
        }
    }
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), board);
    assert_eq!(fixture.signals().len(), 1);
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
}

#[test]
fn user_flip_resolves_git_toplevel_from_a_nested_directory() {
    let fixture = Fixture::new();
    success(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(&fixture.env.project_root)
            .output()
            .unwrap(),
    );
    fixture.running("coder", None);
    let nested = fixture.env.project_root.join("src/nested");
    std::fs::create_dir_all(&nested).unwrap();
    let output = success(
        fixture
            .command()
            .env_remove(rimz::workspace::ENV_WORKTREE_PATH)
            .current_dir(&nested)
            .args(["teams", "flip", "Build", "Start from a shell."])
            .output()
            .unwrap(),
    );
    assert!(output.contains("Opened Build by @user"), "{output}");
    assert!(fixture.board().exists());
    assert!(!nested.join("blackboard.md").exists());
    assert_eq!(
        fixture.signals()[0].payload["board"],
        json!(fixture.board().canonicalize().unwrap())
    );
}

#[test]
fn foreign_team_member_cannot_flip_the_selected_worktree_as_a_user() {
    let fixture = Fixture::new();
    fixture.running("coder", None);
    let other = fixture.env.home_root.join("other-worktree");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("blackboard.md"), BOARD).unwrap();
    std::fs::write(fixture.board(), BOARD).unwrap();
    fixture.seed_member(
        "claude",
        "other-coder",
        "coder",
        None,
        "other-channel",
        &other,
    );
    let output = fixture.flip("Review", Some("other-coder"), Some("Not my cohort."));
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("different team cohort"), "{error}");
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
    assert_eq!(
        std::fs::read_to_string(other.join("blackboard.md")).unwrap(),
        BOARD
    );
    assert!(fixture.signals().is_empty());
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
}

#[test]
fn flip_compaction_threshold_is_checked_before_queuing() {
    for live_pane in [true, false] {
        for used in [None, Some(99_999), Some(100_000), Some(100_001)] {
            let fixture = Fixture::new();
            fixture.running("coder", Some("terminal_3"));
            if live_pane {
                fixture.live_panes(&["terminal_3"]);
            }
            if let Some(used) = used {
                fixture.context_tokens("coder", used);
            }
            std::fs::write(fixture.board(), BOARD).unwrap();
            let output = success(fixture.flip("Review", Some("coder"), Some("Finished building.")));
            let messages = fixture.env.store().list_pending_messages().unwrap();
            let reached = used.is_some_and(|used| used >= 100_000);
            assert_eq!(
                messages.len(),
                usize::from(reached && live_pane),
                "{used:?}: {output}"
            );
            let assists =
                rimz::harness::assist_log::recent(&fixture.env.rimz_home().join("logs"), None);
            assert_eq!(assists.len(), usize::from(reached));
            if reached && live_pane {
                assert!(output.contains("compact  queued"), "{output}");
                assert_eq!(messages[0].body, MessageBody::Command);
                assert_eq!(messages[0].agent_id.as_str(), "coder");
                assert_eq!(messages[0].gate, DeliveryGate::Done);
                assert_eq!(messages[0].compacted_context_tokens, used);
                assert!(matches!(
                    &assists[0].assist,
                    rimz::harness::assist_log::Assist::FlipCompact { threshold: 100_000, occupied_tokens, .. } if *occupied_tokens == used
                ));
            } else if reached {
                assert!(
                    output.contains("compact  skipped: no bound pane"),
                    "{output}"
                );
                assert!(matches!(
                    &assists[0].assist,
                    rimz::harness::assist_log::Assist::FlipCompact {
                        message_id: None,
                        ..
                    }
                ));
            } else {
                assert!(!output.contains("compact"), "{output}");
            }
            assert!(
                std::fs::read_to_string(fixture.board())
                    .unwrap()
                    .contains("Stage: Review (@reviewer)\n")
            );
            assert_eq!(fixture.signals().len(), 1);
        }
    }
}

#[test]
fn flip_compaction_uses_machine_default_unless_role_overrides_it() {
    for (role_policy, default, expected_threshold) in [
        (None, "100k", Some(100_000)),
        (None, "off", None),
        (Some("off"), "100k", None),
        (Some("100k"), "200k", Some(100_000)),
        (None, "70%", Some(140_000)),
        (Some("70%"), "200k", Some(140_000)),
    ] {
        let fixture = Fixture::new();
        let role_config = CONFIG.replace(
            "flip-compact: 100k",
            &role_policy
                .map(|policy| format!("flip-compact: '{policy}'"))
                .unwrap_or_default(),
        );
        crate::common::write_definition(
            &fixture.env,
            "teams",
            "forge",
            &role_config,
            "Complete the work.",
        );
        std::fs::write(
            fixture.env.rimz_home().join("config.toml"),
            format!("[harness]\nflip_compact = \"{default}\"\n"),
        )
        .unwrap();
        fixture.running("coder", Some("terminal_3"));
        fixture.live_panes(&["terminal_3"]);
        fixture.context_tokens("coder", 190_000);
        std::fs::write(fixture.board(), BOARD).unwrap();
        let output = success(fixture.flip("Review", Some("coder"), Some("Finished.")));
        assert_eq!(
            output.contains("compact  queued"),
            expected_threshold.is_some(),
            "{output}"
        );
        assert_eq!(
            fixture.env.store().list_pending_messages().unwrap().len(),
            usize::from(expected_threshold.is_some())
        );
        if let Some(expected) = expected_threshold {
            assert!(
                output.contains(&format!("190k tokens, over {}k", expected / 1000)),
                "{output}"
            );
            let assists =
                rimz::harness::assist_log::recent(&fixture.env.rimz_home().join("logs"), None);
            assert!(
                matches!(&assists[0].assist, rimz::harness::assist_log::Assist::FlipCompact { threshold, .. } if *threshold == expected)
            );
        }
    }
}

#[test]
fn flipping_own_stage_to_done_never_compacts() {
    let fixture = Fixture::new();
    fixture.running("coder", Some("terminal_3"));
    fixture.live_panes(&["terminal_3"]);
    fixture.context_tokens("coder", 150_000);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let output = success(fixture.flip("Done", Some("coder"), Some("Finished.")));
    assert!(!output.contains("compact"), "{output}");
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
    assert!(
        rimz::harness::assist_log::recent(&fixture.env.rimz_home().join("logs"), None).is_empty()
    );
    assert!(
        std::fs::read_to_string(fixture.board())
            .unwrap()
            .contains("Stage: Done\n")
    );
    assert_eq!(fixture.signals().len(), 1);
}

#[test]
fn absent_owner_does_not_prevent_compacting_the_outgoing_stage_owner() {
    let fixture = Fixture::new();
    fixture.running("coder", Some("terminal_3"));
    fixture.live_panes(&["terminal_3"]);
    fixture.context_tokens("coder", 150_000);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let output = success(fixture.flip("Review", Some("coder"), Some("Ready for review.")));
    assert!(
        output.contains("owner    @reviewer, not live; woken on resume"),
        "{output}"
    );
    assert!(output.contains("compact  queued"), "{output}");
    let messages = fixture.env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].agent_id.as_str(), "coder");
    assert_eq!(messages[0].body, MessageBody::Command);
    assert_eq!(messages[0].gate, DeliveryGate::Done);
    assert_eq!(fixture.signals().len(), 1);
}

#[test]
fn missing_pane_skips_compaction_without_failing_the_flip() {
    let fixture = Fixture::new();
    fixture.running("coder", Some("terminal_3"));
    fixture.running("reviewer", None);
    fixture.context_tokens("coder", 150_000);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let output = success(fixture.flip("Review", Some("coder"), Some("Ready for review.")));
    assert!(
        output.contains("compact  skipped: no bound pane"),
        "{output}"
    );
    assert_eq!(fixture.signals().len(), 1);
    assert!(
        std::fs::read_to_string(fixture.board())
            .unwrap()
            .contains("Stage: Review (@reviewer)")
    );
    let messages = fixture.env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].agent_id.as_str(), "reviewer");
    assert_ne!(messages[0].body, MessageBody::Command);
    assert!(rimz::harness::assist_log::recent(&fixture.env.rimz_home().join("logs"), None).iter().any(|record| matches!(
        &record.assist,
        rimz::harness::assist_log::Assist::FlipCompact { error: Some(error), .. } if error == "no bound pane"
    )));
}

#[test]
fn compaction_delivery_error_does_not_fail_a_completed_flip() {
    for broken_wait_stamp in [false, true] {
        let fixture = Fixture::new();
        fixture.running("coder", Some("terminal_3"));
        fixture.live_panes(&["terminal_3"]);
        fixture.hook("coder", "Stop", Some("terminal_3"));
        fixture.context_tokens("coder", 150_000);
        std::fs::write(fixture.board(), BOARD).unwrap();
        if broken_wait_stamp {
            std::fs::create_dir(fixture.env.runtime_paths().lane_path("message-wake.json"))
                .unwrap();
        }
        let output = success(
            fixture
                .command()
                .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
                .env(rimz::harness::launch::ENV_AGENT_ID, "launch_coder")
                .env("RIMZ_TEST_ZELLIJ_MODE", "fail-write")
                .args(["teams", "flip", "Review", "Finished.", "--team", "forge"])
                .output()
                .unwrap(),
        );
        assert!(output.contains("compact  queued"), "{output}");
        assert!(
            std::fs::read_to_string(fixture.board())
                .unwrap()
                .contains("Stage: Review (@reviewer)\n")
        );
        assert_eq!(fixture.signals().len(), 1);
        let messages = fixture.env.store().list_pending_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].body, MessageBody::Command);
        assert!(messages[0].last_error.is_some());
        let trace = std::fs::read_to_string(&fixture.trace).unwrap();
        assert!(
            trace.contains("\taction\twrite-chars\t--pane-id\tterminal_3\t--\t/compact"),
            "compaction must attempt a pane write: {trace}"
        );
        assert!(rimz::harness::assist_log::recent(&fixture.env.rimz_home().join("logs"), None).iter().any(|record| matches!(
        &record.assist,
        rimz::harness::assist_log::Assist::FlipCompact { message_id: Some(id), delivered: false, error, .. } if id == messages[0].message_id.as_str() && error.is_none()
    )));
    }
}

#[test]
fn flip_precondition_errors_leave_board_signals_and_queue_unchanged() {
    let fixture = Fixture::new();
    fixture.running("coder", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let output = fixture.flip("Unknown", None, Some("handoff"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown stage"));
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
    assert!(fixture.signals().is_empty());
    assert!(fixture.env.store().list_messages().unwrap().is_empty());
    for (config, expected) in [
        (
            CONFIG
                .replace("owns: [Build]", "owns: []")
                .replace("owns: [Review]", "owns: []"),
            "has no owner",
        ),
        (CONFIG.replace("owns: [Review]", "owns: []"), "has no owner"),
        (
            CONFIG.replace("owns: [Review]", "owns: [Build]"),
            "owned by both",
        ),
    ] {
        crate::common::write_definition(
            &fixture.env,
            "teams",
            "forge",
            &config,
            "Complete the work.",
        );
        let output = fixture.flip("Review", None, Some("handoff"));
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
        assert!(fixture.signals().is_empty());
        assert!(fixture.env.store().list_messages().unwrap().is_empty());
    }
}

#[test]
fn member_hand_off_is_refused_on_an_uncommitted_worktree() {
    let fixture = Fixture::new();
    let root = &fixture.env.project_root;
    success(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(root)
            .output()
            .unwrap(),
    );
    std::fs::write(root.join(".git/info/exclude"), "/stage-mux.log\n").unwrap();
    fixture.running("coder", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    std::fs::write(root.join("half-done.rs"), "fn broken(\n").unwrap();

    let output = fixture.flip("Review", Some("coder"), Some("handoff"));
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains(
            "has uncommitted changes: half-done.rs; commit or discard them, then flip again"
        ),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
    assert!(fixture.signals().is_empty());

    success(fixture.flip("Build", Some("coder"), Some("still building")));
    success(fixture.flip("Review", None, Some("user forces the hand-off")));
    std::fs::remove_file(root.join("half-done.rs")).unwrap();
    std::fs::write(fixture.board(), BOARD).unwrap();
    success(fixture.flip("Review", Some("coder"), Some("committed")));
    assert_eq!(fixture.signals().len(), 3);
}

#[test]
fn concurrent_flips_keep_the_board_ledger_and_signals_in_one_order() {
    let fixture = Fixture::new();
    fixture.running("coder", None);
    std::fs::write(fixture.board(), BOARD).unwrap();
    let children = ["Review", "Done"].map(|stage| {
        fixture
            .command()
            .args(["teams", "flip", stage, "handoff", "--team", "forge"])
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
    assert!(
        error.contains("re-run `rimz teams flip Review \"<progress note>\"`"),
        "{error}"
    );
    insta::assert_snapshot!(error.as_ref(), @r#"error: completed board, signal; queued delivery requires claude hooks so messages can deliver at turn boundaries; run `rimz hooks install claude`; re-run `rimz teams flip Review "<progress note>"` to repeat the signal and delivery"#);
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
    let messages = fixture.env.store().list_pending_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].text,
        "@user re-opened Review. It is still yours: pick it up from blackboard.md.\n\nNote: review ready\n\nYour report goes in your stage file and anything for the user to @coder; end the turn with the flip and no pane text."
    );
}

#[test]
fn same_role_and_foreign_stage_do_not_compact_or_enqueue() {
    let fixture = Fixture::new();
    crate::common::write_definition(
        &fixture.env,
        "teams",
        "forge",
        &CONFIG
            .replace("stages: [Build, Review]", "stages: [Build, Polish, Review]")
            .replace("owns: [Build]", "owns: [Build, Polish]"),
        "Complete the work.",
    );
    fixture.running("coder", Some("terminal_3"));
    fixture.live_panes(&["terminal_3"]);
    fixture.context_tokens("coder", 150_000);
    std::fs::write(fixture.board(), BOARD).unwrap();
    for (stage, expected) in [
        ("Build", "owner    you, carry on"),
        ("Polish", "owner    you, carry on"),
        ("Done", "Flipped Review -> Done"),
    ] {
        if stage == "Done" {
            std::fs::write(
                fixture.board(),
                BOARD.replace("Build (@coder)", "Review (@reviewer)"),
            )
            .unwrap();
        }
        let output = success(fixture.flip(stage, Some("coder"), Some("handoff")));
        assert!(output.contains(expected), "{output}");
        assert!(!output.contains("compact"), "{output}");
        assert!(fixture.env.store().list_messages().unwrap().is_empty());
        assert!(
            rimz::harness::assist_log::recent(&fixture.env.rimz_home().join("logs"), None)
                .is_empty()
        );
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
    fixture.context_tokens("coder", 150_000);
    let store = fixture.env.store();
    let first = success(fixture.flip("Review", Some("coder"), Some("handoff")));
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
    success(fixture.flip("Build", None, Some("return to build")));
    let output = success(fixture.flip("Review", Some("coder"), Some("handoff")));
    assert!(output.contains("skipped"), "{output}");
    let messages = store.list_pending_messages().unwrap();
    assert_eq!(messages.len(), 4);
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
                && message.text.contains("Review is yours"))
    );
    assert_eq!(fixture.signals().len(), 3);
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
    success(fixture.flip("Review", None, Some("handoff")));
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
    assert!(
        messages[0]
            .text
            .contains("The team resumed at stage Review, which is yours.")
    );
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), board);
}

/// A resumed exec wrapper, killed and reaped when the test ends, pass or fail.
struct Wrapper(std::process::Child);

impl Drop for Wrapper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Where `text` was first pasted in the mux trace, which logs paste bytes as decimals.
fn trace_position(trace: &str, text: &str) -> Option<usize> {
    let bytes = text
        .bytes()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join("\t");
    trace.find(&bytes)
}

#[test]
fn resumed_lazy_member_gets_its_stage_from_the_wrapper_without_a_prompt() {
    let fixture = Fixture::new();
    fixture.env.install_agent_hooks("codex");
    crate::common::trust_codex_hooks(&fixture.env);
    let session = fixture.ended("codex", "coder");
    std::fs::write(fixture.board(), BOARD).unwrap();
    fixture.live_panes_running("codex", &["terminal_3"]);
    let _wrapper = fixture.resume("codex", "coder", &session);
    let stages = fixture.stage_records();
    assert_eq!(stages.len(), 1, "{stages:#?}");
    let stage = stages[0].clone();
    assert_eq!(stage.agent_id, session);
    assert!(
        stage
            .text
            .contains("The team resumed at stage Build, which is yours."),
        "{}",
        stage.text
    );
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
    fixture.sweep_until(&stage.message_id, MessageStatus::Sent);
    let trace = std::fs::read_to_string(&fixture.trace).unwrap();
    assert!(
        trace_position(&trace, "Type: STAGE").is_some(),
        "no STAGE in mux writes: {trace}"
    );
    let envelope = format!("Type: STAGE\nFrom: @rimz\nContent:\n{}", stage.text);
    fixture.feed_from(
        "codex",
        "coder",
        session.as_str(),
        "UserPromptSubmit",
        Some("terminal_3"),
        None,
        &envelope,
    );
    let delivered = fixture.stage_records();
    assert_eq!(delivered.len(), 1, "{delivered:#?}");
    assert_eq!(delivered[0].status, MessageStatus::Delivered);
    let entries = rimz::transcript::read_all(fixture.env.store().paths()).unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry.message_id.as_ref() == Some(&stage.message_id))
        .expect("stage transcript entry");
    assert_eq!(entry.from.as_deref(), Some("@rimz"));
    fixture.feed_from(
        "codex",
        "coder",
        session.as_str(),
        "SessionStart",
        Some("terminal_3"),
        None,
        "",
    );
    assert_eq!(
        fixture.stage_records().len(),
        1,
        "registration re-woke again"
    );
    assert_eq!(fixture.signals().len(), 1, "{:#?}", fixture.signals());
}

#[test]
fn resumed_claude_member_gets_one_stage_held_until_registration() {
    let fixture = Fixture::new();
    let session = fixture.ended("claude", "coder");
    std::fs::write(fixture.board(), BOARD).unwrap();
    fixture.live_panes(&["terminal_3"]);
    let _wrapper = fixture.resume("claude", "coder", &session);
    let stages = fixture.stage_records();
    assert_eq!(stages.len(), 1, "{stages:#?}");
    let id = stages[0].message_id.clone();
    success(
        fixture
            .command()
            .args(["message", "sweep"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fixture.stage_records()[0].status,
        MessageStatus::Queued,
        "a resumed Claude holds its notice until it registers"
    );
    fixture.feed_from(
        "claude",
        "coder",
        session.as_str(),
        "SessionStart",
        Some("terminal_3"),
        None,
        "",
    );
    fixture.sweep_until(&id, MessageStatus::Sent);
    assert_eq!(
        fixture.stage_records().len(),
        1,
        "registration re-woke again"
    );
    assert_eq!(fixture.signals().len(), 1, "{:#?}", fixture.signals());
    assert_eq!(std::fs::read_to_string(fixture.board()).unwrap(), BOARD);
}

#[test]
fn resume_skips_a_stage_delivered_after_its_attempt_began() {
    let fixture = Fixture::new();
    let session = fixture.ended("claude", "coder");
    std::fs::write(fixture.board(), BOARD).unwrap();
    fixture.live_panes(&["terminal_3"]);
    let worktree = fixture.env.project_root.canonicalize().unwrap();
    let board_lock = rimz::disk::lock::WorkspaceLock::acquire(
        &fixture.env.runtime_paths().board_lock(&worktree),
    )
    .unwrap();
    let _wrapper = fixture.spawn_resume("claude", "coder", &session);
    // Past its stamp, the wrapper's re-wake waits on the board lock. A provider that registered
    // early takes its notice in this gap; any Stage notice delivered here stands in for it.
    fixture.wait_for(|events| {
        events.iter().any(|event| {
            matches!(event.kind(), EventKind::AgentLifecycle(payload)
                if payload.event_name.as_deref() == Some("rimz.agent-resumed"))
        })
    });
    let kind = AgentKind::new_unchecked("claude");
    let notice = rimz::store::message::MessageRecord::new_for_card(
        fixture.env.workspace_id.clone(),
        kind.clone(),
        session.clone(),
        Some("coder".to_owned()),
        "stage".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Harness {
        notice: HarnessNotice::Stage,
    })
    .with_pane_id(PaneId::from_parts(MuxName::Zellij, "terminal_3"));
    let store = fixture.env.store();
    let session_name = fixture
        .env
        .resolve_workspace(&fixture.env.project_root)
        .session_name;
    store.queue_message(&notice, &session_name).unwrap();
    store
        .record_sent_batch(std::slice::from_ref(&notice), &session_name)
        .unwrap();
    store
        .confirm_delivered_for_card(
            &kind,
            &session,
            None,
            rimz::store::writer::DeliveryAck::TurnStarted { prompt: None },
            &session_name,
        )
        .unwrap();
    drop(board_lock);
    let log = fixture.wrapper_log_until("resume: ");
    assert!(log.contains("resume: no team stage to re-wake"), "{log}");
    let stages = fixture.stage_records();
    assert_eq!(stages.len(), 1, "{stages:#?}");
    assert_eq!(stages[0].status, MessageStatus::Delivered);
    assert!(fixture.signals().is_empty(), "{:#?}", fixture.signals());
}

#[test]
fn resumed_lazy_leader_gets_its_resume_prompt_then_its_stage() {
    let fixture = Fixture::new();
    fixture.env.install_agent_hooks("codex");
    crate::common::trust_codex_hooks(&fixture.env);
    let session = fixture.ended("codex", "coder");
    std::fs::write(fixture.board(), BOARD).unwrap();
    // The cohort resume queues the positional prompt to the leader before placing its pane.
    let prompt = rimz::store::message::MessageRecord::new_for_card(
        fixture.env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        session.clone(),
        Some("coder".to_owned()),
        "say hi".to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Human);
    let workspace = fixture.env.resolve_workspace(&fixture.env.project_root);
    fixture
        .env
        .store()
        .queue_message(&prompt, &workspace.session_name)
        .unwrap();
    fixture.live_panes_running("codex", &["terminal_3"]);
    let _wrapper = fixture.resume("codex", "coder", &session);
    let stage = fixture.stage_records()[0].clone();
    fixture.sweep_until(&prompt.message_id, MessageStatus::Sent);
    fixture.sweep_until(&stage.message_id, MessageStatus::Sent);
    let trace = std::fs::read_to_string(&fixture.trace).unwrap();
    let human = trace_position(&trace, "say hi").expect("resume prompt in mux writes");
    let notice = trace_position(&trace, "Type: STAGE").expect("stage in mux writes");
    assert!(human < notice, "the resume prompt goes first: {trace}");
}

#[test]
fn agent_launched_team_death_reports_once_and_respects_a_racing_done() {
    use rimz::harness::run;
    use rimz::store::run::RunStatus;

    for done in [false, true] {
        let fixture = Fixture::new();
        fixture.seed_launched("boss", None, None);
        fixture.seed_launched("coder", Some("coder"), Some("boss"));
        fixture.seed_launched("reviewer", Some("reviewer"), Some("boss"));
        std::fs::write(fixture.board(), BOARD).unwrap();
        let store = fixture.env.store();
        let leader = store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| agent.name.as_deref() == Some("coder"))
            .unwrap();
        let record = run::create_peer_prompt(
            store.paths(),
            &leader,
            None,
            rimz::agents::registry::definition_by_kind("claude").unwrap(),
            "Ship the feature.",
            &fixture.env.project_root,
            rimz::store::run::ReportTo::Launcher,
        )
        .unwrap()
        .unwrap();
        run::record_assistant_message(
            store.paths(),
            &record.run_id,
            "claude",
            &leader.agent_id,
            "Last answer.".into(),
        )
        .unwrap();
        fixture.hook("coder", "SessionEnd", None);
        fixture.hook("reviewer", "SessionEnd", None);
        if done {
            std::fs::write(fixture.board(), "# Work\nStage: Done\n").unwrap();
        }
        let settle = || {
            success(fixture.command().args(["agents", "subagent-digest", "--request",
                &json!({"workspace_id": fixture.env.workspace_id, "parent_agent_id": "launch_boss"}).to_string()])
                .output().unwrap());
        };
        settle();
        let settled = run::load(store.paths(), &record.run_id).unwrap();
        assert_eq!(
            settled.status,
            if done {
                RunStatus::Completed
            } else {
                RunStatus::Failed
            }
        );
        assert_eq!(
            settled.failure_tail.as_deref(),
            if done {
                None
            } else {
                Some("cohort ended before Done")
            }
        );
        let id = settled.report_message_id.unwrap();
        let reports = || {
            store
                .list_messages()
                .unwrap()
                .into_iter()
                .filter(|message| {
                    message.sender
                        == MessageSender::Harness {
                            notice: HarnessNotice::TeamReport,
                        }
                })
                .collect::<Vec<_>>()
        };
        let messages = reports();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, id);
        assert_eq!(messages[0].agent_id.as_str(), "boss");
        assert_eq!(messages[0].gate, DeliveryGate::Done);
        let header = if done {
            "reached Done"
        } else {
            "ended before Done at stage Build"
        };
        assert!(
            messages[0].text.starts_with(&format!(
                "Team forge#feature-team {header}; its leader reports:\n"
            )),
            "{}",
            messages[0].text
        );
        if !done {
            assert!(messages[0].text.contains("failed in "));
            assert!(messages[0].text.contains("cohort ended before Done"));
        }
        let response = rimz::harness::run::response_path(store.paths(), &record).unwrap();
        assert_eq!(
            std::fs::read_to_string(&response).unwrap(),
            "Last answer.\n"
        );
        assert!(messages[0].text.contains(&response.display().to_string()));
        settle();
        assert_eq!(reports().len(), 1);
        assert!(
            run::open_team_run_for(store.paths(), "forge#feature-team")
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn agent_launched_team_reports_its_leader_to_the_launcher_at_each_done() {
    let fixture = Fixture::new();
    fixture.seed_launched("boss", None, None);
    fixture.seed_launched("coder", Some("coder"), Some("boss"));
    fixture.seed_launched("reviewer", Some("reviewer"), Some("boss"));
    std::fs::write(fixture.board(), BOARD).unwrap();
    let store = fixture.env.store();
    let leader = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.name.as_deref() == Some("coder"))
        .unwrap();
    let adapter = rimz::agents::registry::definition_by_kind("claude").unwrap();
    let run = rimz::harness::run::create_peer_prompt(
        store.paths(),
        &leader,
        None,
        adapter,
        "Ship the feature.",
        &fixture.env.project_root,
        rimz::store::run::ReportTo::Launcher,
    )
    .unwrap()
    .expect("the prompted leader of an agent-launched team holds a team run");
    let answer = |text: &str| {
        let open = rimz::harness::run::open_team_run_for(store.paths(), "forge#feature-team")
            .unwrap()
            .unwrap();
        rimz::harness::run::record_assistant_message(
            store.paths(),
            &open.run_id,
            "claude",
            &leader.agent_id,
            text.to_owned(),
        )
        .unwrap();
    };
    let team_reports = || {
        store
            .list_messages()
            .unwrap()
            .into_iter()
            .filter(|message| {
                message.sender
                    == MessageSender::Harness {
                        notice: HarnessNotice::TeamReport,
                    }
            })
            .collect::<Vec<_>>()
    };
    answer("Shipped: PR #1.");

    let review = success(fixture.flip("Review", None, Some("Ready for review.")));
    assert!(!review.contains("TEAM_REPORT"), "{review}");
    assert!(team_reports().is_empty(), "only Done reports");

    let done = success(fixture.flip("Done", None, Some("Finished.")));
    assert!(
        done.contains("report   TEAM_REPORT to @boss, at its next turn boundary"),
        "{done}"
    );
    let reports = team_reports();
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert!(
        report.text.ends_with(&format!(
            "Memory: {}",
            fixture.board().canonicalize().unwrap().display()
        )),
        "{}",
        report.text
    );
    assert_eq!(report.agent_id.as_str(), "boss");
    assert_eq!(report.gate, DeliveryGate::Done);
    let response = rimz::harness::run::response_path(store.paths(), &run).unwrap();
    assert_eq!(
        std::fs::read_to_string(&response).unwrap(),
        "Shipped: PR #1.\n"
    );
    assert!(
        report.text.starts_with(
            "Team forge#feature-team reached Done; its leader reports:\n- @coder: completed in "
        ),
        "{}",
        report.text
    );
    assert!(
        report.text.contains("task: \"Ship the feature.\""),
        "{}",
        report.text
    );
    assert!(
        report.text.contains(&response.display().to_string()),
        "{}",
        report.text
    );
    assert!(
        store
            .list_messages()
            .unwrap()
            .iter()
            .all(|message| !matches!(
                message.sender,
                MessageSender::Harness {
                    notice: HarnessNotice::SubagentReport | HarnessNotice::AgentReport
                }
            )),
        "team seats stay out of the launcher's fleet report"
    );

    let reopened = success(fixture.flip("Review", None, Some("One more pass.")));
    assert!(
        reopened.contains("report   TEAM_REPORT again at the next Done"),
        "{reopened}"
    );
    answer("Fixed the review finding.");
    success(fixture.flip("Done", None, Some("Finished again.")));
    let reports = team_reports();
    assert_eq!(reports.len(), 2, "every Done reports");
    assert!(
        reports
            .iter()
            .any(|report| report.text != reports[0].text && report.agent_id.as_str() == "boss")
    );
}

#[test]
fn detached_team_follows_its_board_and_reports_to_nobody() {
    let fixture = Fixture::new();
    fixture.seed_launched("boss", None, None);
    fixture.seed_launched("coder", Some("coder"), Some("boss"));
    fixture.seed_launched("reviewer", Some("reviewer"), Some("boss"));
    std::fs::write(fixture.board(), BOARD).unwrap();
    let store = fixture.env.store();
    let leader = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents
        .into_iter()
        .find(|agent| agent.name.as_deref() == Some("coder"))
        .unwrap();
    let adapter = rimz::agents::registry::definition_by_kind("claude").unwrap();
    let run = rimz::harness::run::create_peer_prompt(
        store.paths(),
        &leader,
        None,
        adapter,
        "Ship the feature.",
        &fixture.env.project_root,
        rimz::store::run::ReportTo::Nobody,
    )
    .unwrap()
    .expect("a detached team still holds its team run");
    assert_eq!(run.report_to, rimz::store::run::ReportTo::Nobody);
    rimz::harness::run::record_assistant_message(
        store.paths(),
        &run.run_id,
        "claude",
        &leader.agent_id,
        "Shipped: PR #1.".to_owned(),
    )
    .unwrap();

    // Flips still message the seats; only the launcher's report is dropped.
    let team_reports = || {
        store
            .list_messages()
            .unwrap()
            .into_iter()
            .filter(|message| {
                message.agent_id.as_str() == "boss"
                    || message.sender
                        == MessageSender::Harness {
                            notice: HarnessNotice::TeamReport,
                        }
            })
            .collect::<Vec<_>>()
    };
    let detached = "report   none: the team was launched detached; rimz teams wait forge#feature-team blocks on Done";
    let done = success(fixture.flip("Done", None, Some("Finished.")));
    assert!(done.contains(detached), "{done}");
    assert!(!done.contains("TEAM_REPORT"), "{done}");
    let settled = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
    assert_eq!(settled.status, rimz::store::run::RunStatus::Completed);
    assert_eq!(settled.report_message_id, None);
    assert_eq!(
        std::fs::read_to_string(rimz::harness::run::response_path(store.paths(), &run).unwrap())
            .unwrap(),
        "Shipped: PR #1.\n"
    );
    assert!(team_reports().is_empty(), "{:?}", team_reports());

    let reopened = success(fixture.flip("Review", None, Some("One more pass.")));
    assert!(reopened.contains(detached), "{reopened}");
    assert!(!reopened.contains("TEAM_REPORT"), "{reopened}");
    assert_eq!(
        rimz::harness::run::open_team_run_for(store.paths(), "forge#feature-team")
            .unwrap()
            .expect("the reopened stretch has its run")
            .report_to,
        rimz::store::run::ReportTo::Nobody
    );
    success(fixture.flip("Done", None, Some("Finished again.")));
    assert!(team_reports().is_empty(), "{:?}", team_reports());
}
