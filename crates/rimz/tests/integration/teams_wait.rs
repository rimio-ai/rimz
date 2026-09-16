//! `rimz teams wait`: board polling, cohort dissolution, joins, and exit codes.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
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

impl Fixture {
    fn new() -> Self {
        let env = Env::new();
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
        for role in ["coder", "reviewer"] {
            self.seed(&format!("{role}-{lane}"), role, lane, &worktree);
        }
        worktree
    }

    fn seed(&self, name: &str, role: &str, channel: &str, worktree: &Path) {
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

    fn end(&self, name: &str) {
        let workspace = rimz::WorkspaceResolver::resolve(&self.env.project_root, None).unwrap();
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
    assert!(text(&missing.stderr).contains("forge#a (timed out)"));

    std::fs::write(worktree.join("blackboard.md"), "Stage: Build (@coder)\n").unwrap();
    let pending = fixture.run(&["forge", "-w", "a", "--timeout", "1s", "--json"]);
    assert_eq!(pending.status.code(), Some(124));
    let report: Value = serde_json::from_slice(&pending.stdout).unwrap();
    assert_eq!(report["status"], "timed_out");
    assert_eq!(report["stage"], "Build");
    assert_eq!(report["result"], Value::Null);

    std::fs::write(worktree.join("blackboard.md"), DONE_BOARD).unwrap();
    let done = fixture.run(&["forge#a"]);
    assert_eq!(done.status.code(), Some(0), "{}", text(&done.stderr));
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
        worktree.join("blackboard.md").display().to_string()
    );
    assert_eq!(
        report["result"],
        "PR: https://example.test/pr/1\n- gate green"
    );

    std::fs::write(worktree.join("blackboard.md"), "Stage: Done\n").unwrap();
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
fn wait_fails_when_the_cohort_ends_before_done() {
    let fixture = Fixture::new();
    let worktree = fixture.lane("a");
    std::fs::write(worktree.join("blackboard.md"), "Stage: Build (@coder)\n").unwrap();

    let child = fixture
        .wait(&["forge#a", "--timeout", "20s"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn teams wait");
    std::thread::sleep(std::time::Duration::from_secs(1));
    fixture.end("coder-a");
    fixture.end("reviewer-a");
    let ended = child.wait_with_output().expect("teams wait exits");

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
    std::fs::write(a.join("blackboard.md"), DONE_BOARD).unwrap();
    std::fs::write(b.join("blackboard.md"), "Stage: Build (@coder)\n").unwrap();

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

    std::fs::write(b.join("blackboard.md"), DONE_BOARD).unwrap();
    let all = fixture.run(&["forge#a", "forge#b"]);
    assert_eq!(all.status.code(), Some(0), "{}", text(&all.stderr));
    let stdout = text(&all.stdout);
    assert!(stdout.starts_with("--- forge#a ---\n"), "{stdout}");
    assert!(stdout.contains("--- forge#b ---\n"), "{stdout}");
}
