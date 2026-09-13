//! Stage hand-offs through the CLI, durable store, and native hook delivery boundary.

use std::path::{Path, PathBuf};
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
flip-compact = "100k"
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
            .env(rimz::workspace::ENV_WORKTREE_PATH, &self.env.project_root)
            .env(rimz::workspace::ENV_CHANNEL, "feature-team")
            .args(["--mux", "zellij"]);
        command
    }

    fn seed(&self, role: &str, parent: Option<&str>) {
        self.seed_member(role, role, parent, "feature-team", &self.env.project_root);
    }

    fn seed_member(
        &self,
        name: &str,
        role: &str,
        parent: Option<&str>,
        channel: &str,
        worktree: &Path,
    ) {
        let workspace = rimz::WorkspaceResolver::resolve(&self.env.project_root, None).unwrap();
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
      note     Review the consumer boundary. Keep the evidence. Ready.
      owner    @reviewer, woken at its next turn boundary
    ");
    let board = std::fs::read_to_string(fixture.board()).unwrap();
    assert!(board.contains("Stage: Review (@reviewer)\n"));
    assert!(board.contains(
        "@user: Build -> Review — Review the consumer boundary. Keep the evidence. Ready.\n"
    ));
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
            "@user flipped the stage Build -> Review. Review is yours: pick it up from blackboard.md.\n\nNote: {note}"
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
    for (existing, declared_stages) in [
        (None, true),
        (Some("# Work\n\n## Evidence\nKeep this section.\n"), true),
        (None, false),
    ] {
        let fixture = Fixture::new();
        if !declared_stages {
            std::fs::write(
                fixture.env.config_root().join("rimz/agents.toml"),
                CONFIG.replace("stages = [\"Build\", \"Review\"]\n", ""),
            )
            .unwrap();
        }
        fixture.running("reviewer", None);
        if let Some(existing) = existing {
            std::fs::write(fixture.board(), existing).unwrap();
        }
        let output = success(fixture.flip("Review", None, Some("Start the review.")));
        assert!(output.contains("Opened Review by @user"), "{output}");
        if declared_stages {
            assert!(output.contains("Build → [Review] → Done"), "{output}");
        } else {
            insta::assert_snapshot!(output.replace(fixture.env.project_root.file_name().unwrap().to_str().unwrap(), "<worktree>"), @"
            Opened Review by @user  (forge#feature-team · <worktree>)
              note     Start the review.
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
            "@user opened the stage Review. Review is yours: pick it up from blackboard.md.\n\nNote: Start the review."
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
    fixture.seed_member("other-coder", "coder", None, "other-channel", &other);
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
    fixture.seed_member("other-coder", "coder", None, "other-channel", &other);
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
fn flip_compaction_threshold_is_checked_before_queuing_for_done() {
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
            let output = success(fixture.flip("Done", Some("coder"), Some("Finished building.")));
            let messages = fixture.env.store().list_pending_messages().unwrap();
            let reached = used.is_some_and(|used| used >= 100_000);
            assert_eq!(
                messages.len(),
                usize::from(reached && live_pane),
                "{used:?}: {output}"
            );
            let assists = rimz::harness::assist_log::recent(&fixture.env.state_root(), None);
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
                    .contains("Stage: Done\n")
            );
            assert_eq!(fixture.signals().len(), 1);
        }
    }
}

#[test]
fn flip_compaction_inherits_harness_threshold_unless_role_overrides_it() {
    for (role_policy, default, expected_threshold) in [
        (None, "100k", Some(100_000)),
        (Some("off"), "100k", None),
        (Some("100k"), "200k", Some(100_000)),
        (None, "70%", Some(140_000)),
        (Some("70%"), "200k", Some(140_000)),
    ] {
        let fixture = Fixture::new();
        let role_config = CONFIG.replace(
            "flip-compact = \"100k\"",
            &role_policy
                .map(|policy| format!("flip-compact = \"{policy}\""))
                .unwrap_or_default(),
        );
        std::fs::write(
            fixture.env.config_root().join("rimz/agents.toml"),
            role_config,
        )
        .unwrap();
        std::fs::write(
            fixture.env.config_root().join("rimz/config.toml"),
            format!("[harness]\nflip_compact = \"{default}\"\n"),
        )
        .unwrap();
        fixture.running("coder", Some("terminal_3"));
        fixture.live_panes(&["terminal_3"]);
        fixture.context_tokens("coder", 150_000);
        std::fs::write(fixture.board(), BOARD).unwrap();
        let output = success(fixture.flip("Done", Some("coder"), Some("Finished.")));
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
                output.contains(&format!("150k tokens, over {}k", expected / 1000)),
                "{output}"
            );
            let assists = rimz::harness::assist_log::recent(&fixture.env.state_root(), None);
            assert!(
                matches!(&assists[0].assist, rimz::harness::assist_log::Assist::FlipCompact { threshold, .. } if *threshold == expected)
            );
        }
    }
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
    assert!(rimz::harness::assist_log::recent(&fixture.env.state_root(), None).iter().any(|record| matches!(
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
            std::fs::create_dir(fixture.env.runtime_paths().root.join("message-wake.json"))
                .unwrap();
        }
        let output = success(
            fixture
                .command()
                .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
                .env(rimz::harness::launch::ENV_AGENT_ID, "launch_coder")
                .env("RIMZ_TEST_ZELLIJ_MODE", "fail-write")
                .args(["teams", "flip", "Done", "Finished.", "--team", "forge"])
                .output()
                .unwrap(),
        );
        assert!(
            output.contains(if broken_wait_stamp {
                "compact  skipped:"
            } else {
                "compact  queued"
            }),
            "{output}"
        );
        assert!(
            std::fs::read_to_string(fixture.board())
                .unwrap()
                .contains("Stage: Done\n")
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
        assert!(rimz::harness::assist_log::recent(&fixture.env.state_root(), None).iter().any(|record| matches!(
        &record.assist,
        rimz::harness::assist_log::Assist::FlipCompact { message_id: Some(id), delivered: false, error, .. } if id == messages[0].message_id.as_str() && error.is_some() == broken_wait_stamp
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
        let output = fixture.flip("Review", None, Some("handoff"));
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected));
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
    std::fs::write(
        root.join(".git/info/exclude"),
        "/blackboard.md\n/stage-mux.log\n",
    )
    .unwrap();
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
        "@user re-opened Review. It is still yours: pick it up from blackboard.md.\n\nNote: review ready"
    );
}

#[test]
fn same_role_and_foreign_stage_do_not_compact_or_enqueue() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.env.config_root().join("rimz/agents.toml"),
        CONFIG
            .replace(
                "stages = [\"Build\", \"Review\"]",
                "stages = [\"Build\", \"Polish\", \"Review\"]",
            )
            .replace("owns = [\"Build\"]", "owns = [\"Build\", \"Polish\"]"),
    )
    .unwrap();
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
