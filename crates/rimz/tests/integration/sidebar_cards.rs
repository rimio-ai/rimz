use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
use rimz::ids::{MuxName, PaneId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::common::{Env, trace_lines, write_path_shim, zellij_trace_shim};

fn closed_path(env: &Env) -> PathBuf {
    env.runtime_paths().lanes_dir.join("closed-cards.json")
}

fn session(env: &Env) -> String {
    env.resolve_workspace(&env.project_root).session_name
}

fn command(env: &Env, args: &[&str]) -> Command {
    let mut command = env.rimz();
    command
        .current_dir(&env.home_root)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env("RIMZ_TEST_ZELLIJ_LOG", env.home_root.join("mux.log"))
        .args(["--mux", "zellij", "sidebar"])
        .args(args)
        .args(["--session-name", &session(env)]);
    command
}

fn exchange(env: &Env, expected: Value, reply: Value, mut command: Command) -> Output {
    let digest = Sha256::digest(format!("zellij\0{}", session(env)));
    let path = env
        .runtime_paths()
        .sock_dir
        .join(format!("host.{}.sock", hex::encode(&digest[..6])));
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut peer = loop {
        match listener.accept() {
            Ok((peer, _)) => break peer,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if child.try_wait().unwrap().is_some() {
                    std::fs::remove_file(&path).unwrap();
                    return child.wait_with_output().unwrap();
                }
                assert!(
                    Instant::now() < deadline,
                    "card request never reached the host"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("accept card request: {error}"),
        }
    };
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut line = String::new();
    BufReader::new(peer.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), expected);
    writeln!(peer, "{reply}").unwrap();
    drop(peer);
    let output = child.wait_with_output().unwrap();
    std::fs::remove_file(path).unwrap();
    output
}

fn assert_exit(output: &Output, code: i32) {
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(output.status.code(), Some(code), "{output:?}");
}

fn seed(env: &Env, id: &str, configure: impl FnOnce(&mut AgentLifecycleObservation)) {
    let transcript = env.project_root.join(format!("{id}.jsonl"));
    std::fs::write(&transcript, "{}\n").unwrap();
    let mut observation =
        AgentLifecycleObservation::new(Some(id.into()), LifecycleSignal::Registered);
    observation.agent_name = Some(format!("pet-{id}"));
    observation.worktree_path = Some(env.project_root.display().to_string());
    observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, "terminal_7"));
    observation.transcript_path = Some(transcript.display().to_string());
    configure(&mut observation);
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            session(env),
            "claude",
            "SessionStart",
            &observation,
        ))
        .unwrap();
}

fn entry(id: &str) -> Value {
    json!({"kind": "claude", "agent_id": id, "closed_at": 1234})
}

fn write_closed(env: &Env, entries: &[Value]) {
    rimz::disk::atomic::write_temp_then_rename_cache(&closed_path(env), &entries).unwrap();
}

fn read_closed(env: &Env) -> Value {
    serde_json::from_slice(&std::fs::read(closed_path(env)).unwrap()).unwrap()
}

fn resume_shim(env: &Env, fail: bool) -> PathBuf {
    write_path_shim(
        &env.home_root.join("resume-bin"),
        "rimz",
        &format!(
            "printf '%s\\n' \"$@\" > \"$HOME/resume-args\"\n\
             test ! -t 0 || exit 99\n\
             echo resume-receipt\n\
             echo resume-diagnostic >&2\n\
             exit {}",
            if fail { 1 } else { 0 }
        ),
    )
}

#[test]
fn card_verbs_route_off_room_and_keep_success_and_refusals_off_stdout() {
    let env = Env::new();
    env.record(&env.project_root);
    for (args, request) in [
        (
            vec!["walk", "next"],
            json!({"command":"walk", "direction":"next", "worktree":false}),
        ),
        (
            vec!["walk", "prev", "--worktree"],
            json!({"command":"walk", "direction":"prev", "worktree":true}),
        ),
        (vec!["slot", "9"], json!({"command":"slot", "n":9})),
        (vec!["last"], json!({"command":"last"})),
        (
            vec!["filter", "needs-you"],
            json!({"command":"filter_needs_you", "toggle":false}),
        ),
        (
            vec!["filter", "needs-you", "--toggle"],
            json!({"command":"filter_needs_you", "toggle":true}),
        ),
        (vec!["close"], json!({"command":"close_target"})),
    ] {
        for refused in [false, true] {
            let reply = if refused {
                json!({"reply":"refused", "reason":"sidebar: no matching card"})
            } else {
                json!({"reply":"done"})
            };
            let output = exchange(&env, request.clone(), reply, command(&env, &args));
            assert_exit(&output, 0);
            assert_eq!(
                String::from_utf8_lossy(&output.stderr).trim(),
                if refused {
                    "sidebar: no matching card"
                } else {
                    ""
                }
            );
        }
    }
    for slot in ["0", "10", "bad"] {
        assert_exit(&command(&env, &["slot", slot]).output().unwrap(), 2);
    }
}

#[test]
fn card_commands_report_missing_room_and_host_as_faults() {
    let env = Env::new();
    for args in [
        vec!["walk", "next"],
        vec!["slot", "1"],
        vec!["last"],
        vec!["filter", "needs-you"],
        vec!["close"],
        vec!["reopen"],
    ] {
        let output = command(&env, &args).output().unwrap();
        assert_exit(&output, 1);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("room"),
            "{output:?}"
        );
    }
    env.record(&env.project_root);
    let output = command(&env, &["walk", "next"]).output().unwrap();
    assert_exit(&output, 1);
    assert!(String::from_utf8_lossy(&output.stderr).contains("rimz sidebar repair"));
}

#[test]
fn close_stops_children_before_the_highlighted_session_and_records_only_the_root() {
    let env = Env::new();
    env.record(&env.project_root);
    seed(&env, "root", |_| {});
    seed(&env, "child", |observation| {
        observation.launch.parent_agent_id = Some("root".into());
        observation.launch.launch_depth = Some(1);
        observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, "terminal_8"));
    });
    let output = exchange(
        &env,
        json!({"command":"close_target"}),
        json!({"reply":"close_target", "agent":{
            "kind":"claude", "agent_id":"root", "pane":"zellij:terminal_7"
        }}),
        command(&env, &["close"]),
    );
    assert_exit(&output, 0);
    let trace = trace_lines(&env.home_root.join("mux.log"));
    let closes: Vec<_> = trace
        .iter()
        .filter(|line| line.contains("close-pane"))
        .collect();
    assert_eq!(closes.len(), 2, "{trace:?}");
    assert!(closes[0].ends_with("8"), "{closes:?}");
    assert!(closes[1].ends_with("7"), "{closes:?}");
    let entries = read_closed(&env);
    assert_eq!(entries.as_array().unwrap().len(), 1);
    assert_eq!(entries[0]["agent_id"], "root");
    assert_eq!(entries[0]["kind"], "claude");
    assert!(entries[0]["closed_at"].as_i64().unwrap() > 1234);
}

#[test]
fn close_process_does_not_record_an_entry_and_a_stop_failure_records_nothing() {
    let env = Env::new();
    env.record(&env.project_root);
    let output = exchange(
        &env,
        json!({"command":"close_target"}),
        json!({"reply":"close_target", "pane":"zellij:terminal_9"}),
        command(&env, &["close"]),
    );
    assert_exit(&output, 0);
    assert!(!closed_path(&env).exists());
    assert!(
        trace_lines(&env.home_root.join("mux.log"))
            .iter()
            .any(|line| line.contains("close-pane") && line.ends_with("9"))
    );

    seed(&env, "root", |_| {});
    let mut failed = command(&env, &["close"]);
    failed.env("RIMZ_TEST_ZELLIJ_FAIL_CLOSE_PANE", "1");
    let output = exchange(
        &env,
        json!({"command":"close_target"}),
        json!({"reply":"close_target", "agent":{
            "kind":"claude", "agent_id":"root", "pane":"zellij:terminal_7"
        }}),
        failed,
    );
    assert_exit(&output, 1);
    assert!(!closed_path(&env).exists());
}

fn close_unbound_card(id: &str) {
    let env = Env::new();
    env.record(&env.project_root);
    if id == "unbound" {
        seed(&env, id, |observation| observation.pane_id = None);
    }
    let output = exchange(
        &env,
        json!({"command":"close_target"}),
        json!({"reply":"close_target", "agent": {
            "kind":"claude", "agent_id":id, "pane":"zellij:terminal_7"
        }}),
        command(&env, &["close"]),
    );
    assert_exit(&output, 0);
    assert!(
        trace_lines(&env.home_root.join("mux.log"))
            .iter()
            .any(|line| line.contains("close-pane") && line.ends_with("7"))
    );
    assert!(!closed_path(&env).exists());
}

#[test]
fn close_idle_agent_closes_the_row_pane_without_recording() {
    close_unbound_card("zellij:terminal_7");
}

#[test]
fn close_unbound_session_closes_the_row_pane_without_recording() {
    close_unbound_card("unbound");
}

#[test]
fn close_promoted_child_does_not_record_an_unresumable_card() {
    let env = Env::new();
    env.record(&env.project_root);
    seed(&env, "orphan", |observation| {
        observation.launch.parent_agent_id = Some("gone-parent".into());
        observation.launch.launch_depth = Some(1);
    });
    let output = exchange(
        &env,
        json!({"command":"close_target"}),
        json!({"reply":"close_target", "agent": {
            "kind":"claude", "agent_id":"orphan", "pane":"zellij:terminal_7"
        }}),
        command(&env, &["close"]),
    );
    assert_exit(&output, 0);
    assert!(!closed_path(&env).exists());
}

#[test]
fn close_session_without_saved_conversation_does_not_record_a_card() {
    let env = Env::new();
    env.record(&env.project_root);
    seed(&env, "empty", |observation| {
        observation.transcript_path =
            Some(env.project_root.join("missing.jsonl").display().to_string());
    });
    let output = exchange(
        &env,
        json!({"command":"close_target"}),
        json!({"reply":"close_target", "agent": {
            "kind":"claude", "agent_id":"empty", "pane":"zellij:terminal_7"
        }}),
        command(&env, &["close"]),
    );
    assert_exit(&output, 0);
    assert!(!closed_path(&env).exists());
}

#[test]
fn close_bounds_the_cache_and_saves_team_worktree_replay() {
    let env = Env::new();
    env.record(&env.project_root);
    let repo = env.project_root.file_name().unwrap().to_str().unwrap();
    let worktree = env.home_root.join(format!("{repo}-worktrees/feature"));
    std::fs::create_dir_all(&worktree).unwrap();
    seed(&env, "team", |observation| {
        observation.launch.team = Some("forge".to_owned());
        observation.launch.role = Some("coder".to_owned());
        observation.worktree_path = Some(worktree.display().to_string());
    });
    write_closed(
        &env,
        &(0..32).map(|n| entry(&n.to_string())).collect::<Vec<_>>(),
    );
    let output = exchange(
        &env,
        json!({"command":"close_target"}),
        json!({"reply":"close_target", "agent":{
            "kind":"claude", "agent_id":"team", "pane":"zellij:terminal_7"
        }}),
        command(&env, &["close"]),
    );
    assert_exit(&output, 0);
    let entries = read_closed(&env);
    assert_eq!(entries.as_array().unwrap().len(), 32);
    assert_eq!(entries[0]["agent_id"], "1");
    assert_eq!(entries[31]["team"], "forge");
    assert_eq!(entries[31]["role"], "coder");
    assert_eq!(entries[31]["worktree"], "feature");
}

#[test]
fn reopen_replays_the_newest_closed_session_and_prunes_newer_live_entries() {
    let env = Env::new();
    env.record(&env.project_root);
    seed(&env, "live", |observation| {
        observation.runtime_owner = Some(rimz::store::runtime::process_owner(
            rimz::pane::RuntimeOwnerKind::Agent,
            "live",
            env.agent_owner_pid(),
        ));
    });
    write_closed(&env, &[entry("older"), entry("closed"), entry("live")]);
    let mut cmd = command(&env, &["reopen"]);
    cmd.env("RIMZ_BIN", resume_shim(&env, false));
    let output = cmd.output().unwrap();
    assert_exit(&output, 0);
    assert!(output.stderr.is_empty(), "{output:?}");
    let args = std::fs::read_to_string(env.home_root.join("resume-args")).unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "--root",
            env.project_root.to_str().unwrap(),
            "--mux",
            "zellij",
            "agents",
            "resume",
            "@closed",
        ]
    );
    assert_eq!(read_closed(&env), json!([entry("older")]));
}

#[test]
fn reopen_replays_team_seats_with_the_recorded_worktree() {
    let env = Env::new();
    env.record(&env.project_root);
    let mut team = entry("team");
    team["team"] = json!("forge");
    team["role"] = json!("reviewer");
    team["worktree"] = json!("feature");
    write_closed(&env, &[team]);
    let mut cmd = command(&env, &["reopen"]);
    cmd.env("RIMZ_BIN", resume_shim(&env, false));
    let output = cmd.output().unwrap();
    assert_exit(&output, 0);
    let args = std::fs::read_to_string(env.home_root.join("resume-args")).unwrap();
    assert_eq!(
        args.lines().skip(4).collect::<Vec<_>>(),
        ["agents", "forge.reviewer", "--continue", "-w", "feature",]
    );
    assert_eq!(read_closed(&env), json!([]));
}

fn reopen_skips_unknown_liveness(owner: Option<rimz::pane::RuntimeOwnerKind>) {
    let env = Env::new();
    env.record(&env.project_root);
    seed(&env, "live", |observation| {
        observation.runtime_owner = owner
            .map(|kind| rimz::store::runtime::process_owner(kind, "live", env.agent_owner_pid()));
    });
    write_closed(&env, &[entry("older"), entry("live")]);
    let mut cmd = command(&env, &["reopen"]);
    cmd.env("RIMZ_BIN", resume_shim(&env, false));
    let output = cmd.output().unwrap();
    assert_exit(&output, 0);
    let args = std::fs::read_to_string(env.home_root.join("resume-args")).unwrap();
    assert_eq!(args.lines().last(), Some("@older"));
    assert_eq!(read_closed(&env), json!([]));
}

#[test]
fn reopen_skips_alive_rows_without_an_owner() {
    reopen_skips_unknown_liveness(None);
}

#[test]
fn reopen_skips_alive_rows_with_a_daemon_owner() {
    reopen_skips_unknown_liveness(Some(rimz::pane::RuntimeOwnerKind::Daemon));
}

#[test]
fn reopen_empty_corrupt_and_all_live_caches_are_successful_noops() {
    let env = Env::new();
    env.record(&env.project_root);
    seed(&env, "live", |observation| {
        observation.runtime_owner = Some(rimz::store::runtime::process_owner(
            rimz::pane::RuntimeOwnerKind::Agent,
            "live",
            env.agent_owner_pid(),
        ));
    });
    for contents in [
        None,
        Some("not-json".to_owned()),
        Some(json!([entry("live")]).to_string()),
    ] {
        if let Some(contents) = contents {
            std::fs::write(closed_path(&env), contents).unwrap();
        }
        let output = command(&env, &["reopen"]).output().unwrap();
        assert_exit(&output, 0);
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "sidebar reopen: no closed card to reopen"
        );
    }
    assert_eq!(read_closed(&env), json!([entry("live")]));
}

#[test]
fn reopen_failure_drops_only_the_failed_entry_and_next_reopen_reaches_older() {
    let env = Env::new();
    env.record(&env.project_root);
    seed(&env, "live", |observation| {
        observation.runtime_owner = Some(rimz::store::runtime::process_owner(
            rimz::pane::RuntimeOwnerKind::Agent,
            "live",
            env.agent_owner_pid(),
        ));
    });
    write_closed(&env, &[entry("older"), entry("closed"), entry("live")]);
    let mut cmd = command(&env, &["reopen"]);
    cmd.env("RIMZ_BIN", resume_shim(&env, true));
    let output = cmd.output().unwrap();
    assert_exit(&output, 1);
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("resume-diagnostic"),
        "{output:?}"
    );
    let args = std::fs::read_to_string(env.home_root.join("resume-args")).unwrap();
    assert_eq!(args.lines().last(), Some("@closed"));
    assert_eq!(read_closed(&env), json!([entry("older"), entry("live")]));

    let mut cmd = command(&env, &["reopen"]);
    cmd.env("RIMZ_BIN", resume_shim(&env, false));
    assert_exit(&cmd.output().unwrap(), 0);
    let args = std::fs::read_to_string(env.home_root.join("resume-args")).unwrap();
    assert_eq!(args.lines().last(), Some("@older"));
    assert_eq!(read_closed(&env), json!([]));
}
