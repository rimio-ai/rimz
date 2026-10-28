//! `rimz teams wait`: board polling, cohort dissolution, joins, and exit codes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rimz::agents::{AgentLifecycleObservation, AskKind, LifecycleSignal};
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::writer::AgentLifecycleIntent;
use serde_json::Value;

use crate::common::Env;

const CONFIG: &str =
    "leader: coder\nstages: [Build]\nroles:\n  - {role: coder, agent: worker, owns: [Build]}";
const DONE_BOARD: &str = "# Blackboard\nStage: Done\n\n## Progress\n- flipped\n\n## Result\nPR: https://example.test/pr/1\n- gate green\n";

struct Fixture {
    env: Env,
}

#[test]
fn teams_show_root_alias_misses_use_main() {
    let fixture = Fixture::new();
    fixture.lane("a");
    for alias in [
        "main",
        "project",
        fixture.env.project_root.to_str().unwrap(),
    ] {
        let output = fixture
            .env
            .rimz()
            .args(["teams", "show", &format!("forge#{alias}")])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(text.contains("no live cohort in #main"), "{text}");
        assert!(text.contains("live cohorts: #a"), "{text}");
    }
}

impl Fixture {
    fn new() -> Self {
        let env = Env::new();
        env.record(&env.project_root);
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
            "description: Team worker\nagent: claude\ntools: []",
            "",
        );
        crate::common::write_definition(&env, "teams", "forge", CONFIG, "Complete the work.");
        Self { env }
    }

    fn lane(&self, lane: &str) -> PathBuf {
        let worktree = self.env.project_root.join(lane);
        std::fs::create_dir_all(&worktree).unwrap();
        rimz::harness::team_notes::register(
            &self.env.store(),
            &worktree,
            "forge",
            &toml::from_str("stages = ['Build']").unwrap(),
            rimz::harness::team_notes::Launch::Resume,
        )
        .unwrap();
        for role in ["coder", "reviewer"] {
            self.seed(
                &format!("{role}-{lane}"),
                role,
                lane,
                &worktree,
                Some("forge"),
                None,
            );
        }
        worktree
    }

    fn seed(
        &self,
        name: &str,
        role: &str,
        channel: &str,
        worktree: &Path,
        team: Option<&str>,
        pane: Option<&str>,
    ) {
        let workspace = self.env.resolve_workspace(&self.env.project_root);
        self.env
            .store()
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id,
                &workspace.session_name,
                &AgentKind::new_unchecked("claude"),
                AgentLaunchPayload {
                    system_prompt_digest: None,
                    agent_id: AgentSessionId::from(format!("launch_{name}")),
                    launch_id: Some(AgentSessionId::from(format!("launch_{name}"))),
                    agent_name: name.to_owned(),
                    agent_name_explicit: true,
                    launch: rimz::agents::LaunchParams {
                        team: team.map(str::to_owned),
                        role: Some(role.to_owned()),
                        channel: Some(channel.to_owned()),
                        ..Default::default()
                    },
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: pane.map(|pane| pane.parse().unwrap()),
                    runtime_owner: None,
                    worktree_path: Some(worktree.display().to_string()),
                    worktree_branch: Some(channel.to_owned()),
                    prompt: None,
                    description: None,
                },
            ))
            .unwrap();
    }

    fn end(&self, name: &str) {
        let workspace = self.env.resolve_workspace(&self.env.project_root);
        let observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(format!("launch_{name}"))),
            LifecycleSignal::Ended,
        );
        self.env
            .store()
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "SessionEnd",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }

    fn wait(&self, args: &[&str]) -> Command {
        let mut command = self.env.rimz();
        command.args(["teams", "wait"]).args(args);
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.wait(args).output().expect("run teams wait")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn wait_times_out_on_a_pending_board_and_settles_at_once_on_done() {
    let fixture = Fixture::new();
    let worktree = fixture.lane("a");

    let missing = fixture.run(&["forge#a", "--timeout", "1s"]);
    assert_eq!(
        missing.status.code(),
        Some(124),
        "{}",
        text(&missing.stderr)
    );
    assert!(
        text(&missing.stderr).contains("forge#a: timed out after 1s at none; cohort left running")
    );

    std::fs::create_dir_all(worktree.join(".rimz/team")).unwrap();

    std::fs::write(
        worktree.join(".rimz/team/blackboard.md"),
        "Stage: Build (@coder)\n",
    )
    .unwrap();
    let pending = fixture.run(&["forge", "-w", "a", "--timeout", "1s", "--json"]);
    assert_eq!(pending.status.code(), Some(124));
    let report: Value = serde_json::from_slice(&pending.stdout).unwrap();
    assert_eq!(report["status"], "timed_out");
    assert_eq!(report["stage"], "Build");
    assert_eq!(report["result"], Value::Null);
    assert!(
        text(&pending.stderr).contains("waiting on forge#a: Build (@coder, not live)"),
        "{}",
        text(&pending.stderr),
    );
    assert!(
        text(&pending.stderr).contains(
            "forge#a: timed out after 1s at Build (@coder, not live); cohort left running"
        ),
        "{}",
        text(&pending.stderr)
    );

    std::fs::create_dir_all(worktree.join(".rimz/team")).unwrap();

    std::fs::write(worktree.join(".rimz/team/blackboard.md"), DONE_BOARD).unwrap();
    let done = fixture.run(&["forge#a"]);
    assert_eq!(done.status.code(), Some(0), "{}", text(&done.stderr));
    assert!(text(&done.stderr).is_empty(), "{}", text(&done.stderr));
    assert_eq!(
        text(&done.stdout),
        "PR: https://example.test/pr/1\n- gate green\n"
    );
    let done = fixture.run(&["forge#a", "--json"]);
    let report: Value = serde_json::from_slice(&done.stdout).unwrap();
    assert_eq!(report["status"], "completed");
    assert_eq!(report["exit"], 0);
    assert_eq!(report["stage"], "Done");
    assert_eq!(
        report["board"],
        worktree
            .join(".rimz/team/blackboard.md")
            .display()
            .to_string()
    );
    assert_eq!(
        report["result"],
        "PR: https://example.test/pr/1\n- gate green"
    );

    std::fs::create_dir_all(worktree.join(".rimz/team")).unwrap();

    std::fs::write(worktree.join(".rimz/team/blackboard.md"), "Stage: Done\n").unwrap();
    let bare = fixture.run(&["forge#a"]);
    assert_eq!(bare.status.code(), Some(0));
    assert!(bare.stdout.is_empty());
    assert!(text(&bare.stderr).contains("no Result section"));

    let several = fixture.run(&["forge", "forge", "-w", "a"]);
    assert_eq!(several.status.code(), Some(1));
    assert!(text(&several.stderr).contains("selects one cohort"));
    let unknown = fixture.run(&["smith#a"]);
    assert!(text(&unknown.stderr).contains("configured teams: forge"));
}

#[test]
fn wait_attention_handles_round_trip_beside_teamless_role_rivals() {
    use std::io::BufRead;

    let fixture = Fixture::new();
    let worktree = fixture.lane("a");
    fixture.seed(
        "coder-a",
        "coder",
        "a",
        &worktree,
        Some("forge"),
        Some("zellij:terminal_1"),
    );
    fixture.seed(
        "inline-coder",
        "coder",
        "a",
        &worktree,
        None,
        Some("zellij:terminal_2"),
    );
    let board = worktree.join(".rimz/team/blackboard.md");
    std::fs::write(&board, "Stage: Build (@coder)\n").unwrap();
    let mut child = fixture
        .wait(&["forge#a", "--timeout", "20s"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn teams wait");
    let mut lines = std::io::BufReader::new(child.stderr.take().unwrap()).lines();
    let first = lines.next().expect("initial progress").unwrap();
    assert!(first.starts_with("waiting on forge#a:"), "{first}");

    let workspace = fixture.env.resolve_workspace(&fixture.env.project_root);
    for signal in [
        LifecycleSignal::TurnStarted { turn_id: None },
        LifecycleSignal::AwaitingInput {
            kind: AskKind::Permission,
            ask_id: None,
            detail: None,
            native_key: None,
        },
    ] {
        let observation = AgentLifecycleObservation::new(Some("launch_coder-a".into()), signal);
        fixture
            .env
            .store()
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }
    // `by_ref` keeps the pipe open until the child exits: a dropped reader fails its next write.
    let diagnostic = lines
        .by_ref()
        .map(|line| line.unwrap())
        .find(|line| line.contains(" waiting on you"))
        .expect("needs-you diagnostic");
    std::fs::write(&board, DONE_BOARD).unwrap();
    assert!(child.wait().unwrap().success(), "{diagnostic}");

    let handle = diagnostic
        .strip_prefix("forge#a: ")
        .and_then(|line| line.strip_suffix(" waiting on you (rimz teams focus 'forge#a')"))
        .unwrap_or_else(|| panic!("unexpected diagnostic: {diagnostic}"));
    let snapshot = fixture.env.store().snapshot_cached().unwrap();
    let resolved = rimz::address::resolve_one(
        &snapshot,
        handle,
        None,
        &rimz::address::AddressContext {
            channel: Some("a".to_owned()),
            origin: rimz::address::ChannelOrigin::Explicit,
        },
    );
    assert_eq!(
        resolved.map(|agent| agent.agent_id.as_str()),
        Ok("launch_coder-a"),
        "{diagnostic}"
    );
}

#[test]
fn wait_fails_when_the_cohort_ends_before_done() {
    let fixture = Fixture::new();
    let worktree = fixture.lane("a");
    // The board is a FIFO, so a write to it returns only once the waiter opens it to read.
    // The waiter reads its board after it selects the cohort, so the first write to return
    // is the earliest point the members can end without the wait refusing a dead cohort.
    let board = worktree.join(".rimz/team/blackboard.md");
    nix::unistd::mkfifo(&board, nix::sys::stat::Mode::S_IRWXU).expect("board fifo");

    let mut child = fixture
        .wait(&["forge#a", "--timeout", "20s"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn teams wait");
    let exited = std::sync::atomic::AtomicBool::new(false);
    let (board_read, selected) = std::sync::mpsc::channel();
    let (ended, _feeder_release) = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !exited.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = std::fs::write(&board, "Stage: Build (@coder)\n");
                let _ = board_read.send(());
            }
        });
        // A waiter that exits before it reads the board fails the assertions below.
        while selected
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err()
            && child.try_wait().expect("poll teams wait").is_none()
        {}
        fixture.end("coder-a");
        fixture.end("reviewer-a");
        let ended = child.wait_with_output().expect("teams wait exits");
        exited.store(true, std::sync::atomic::Ordering::SeqCst);
        // A read-write open never blocks; held past the scope, it frees a feeder parked on
        // a reader that will not come.
        let release = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&board)
            .expect("open board fifo");
        (ended, release)
    });

    assert_eq!(ended.status.code(), Some(1), "{}", text(&ended.stderr));
    assert!(ended.stdout.is_empty());
    assert!(
        text(&ended.stderr).contains("forge#a: cohort ended before Done (stage: Build)"),
        "{}",
        text(&ended.stderr)
    );
}

#[test]
fn wait_joins_cohorts_and_races_them_with_any() {
    let fixture = Fixture::new();
    let a = fixture.lane("a");
    let b = fixture.lane("b");
    std::fs::create_dir_all(a.join(".rimz/team")).unwrap();
    std::fs::write(a.join(".rimz/team/blackboard.md"), DONE_BOARD).unwrap();
    std::fs::create_dir_all(b.join(".rimz/team")).unwrap();
    std::fs::write(
        b.join(".rimz/team/blackboard.md"),
        "Stage: Build (@coder)\n",
    )
    .unwrap();

    let join = fixture.run(&["forge#a", "forge#b", "--timeout", "1s", "--json"]);
    assert_eq!(join.status.code(), Some(124));
    let report: Value = serde_json::from_slice(&join.stdout).unwrap();
    assert_eq!(report["forge#a"]["status"], "completed");
    assert_eq!(report["forge#b"]["status"], "timed_out");
    assert_eq!(report["forge#b"]["stage"], "Build");

    let race = fixture.run(&["forge#b", "forge#a", "--any"]);
    assert_eq!(race.status.code(), Some(0), "{}", text(&race.stderr));
    assert_eq!(
        text(&race.stdout),
        "--- forge#a ---\nPR: https://example.test/pr/1\n- gate green\n\n"
    );

    std::fs::create_dir_all(b.join(".rimz/team")).unwrap();

    std::fs::write(b.join(".rimz/team/blackboard.md"), DONE_BOARD).unwrap();
    let all = fixture.run(&["forge#a", "forge#b"]);
    assert_eq!(all.status.code(), Some(0), "{}", text(&all.stderr));
    let stdout = text(&all.stdout);
    assert!(stdout.starts_with("--- forge#a ---\n"), "{stdout}");
    assert!(stdout.contains("--- forge#b ---\n"), "{stdout}");
}
