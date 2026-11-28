//! Integration coverage for the agent-facing `rimz shell` and `rimz watch` doorways.

#[cfg(target_os = "linux")]
use crate::common::wait::wait_for_loop_runs_to_exit;
use crate::common::wait::{
    agent_wait, agent_wait_at, loop_instances_path, publish_claude_5h, register_agent,
    register_agent_with_owner, register_calling_agent, reset_minute, seed_signal_wait,
    wait_instances, wait_ok,
};
use crate::common::{Env, canonical, retained_runs_refusal, zellij_trace_shim};
use rimz::agents::{AgentLifecycleObservation, AskKind, LaunchParams, LifecycleSignal};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use rimz::store::event::{EventKind, MessageEventMethod};
use rimz::store::message::{
    DeliveryGate, HarnessNotice, MessageRecord, MessageSender, MessageStatus,
};
use rimz::store::writer::AgentLifecycleIntent;

/// A task record that does not read refuses the wait with its path, and leaves no row.
#[test]
fn unreadable_task_record_leaves_no_wait() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let record = env.rimz_home().join("loops/tasks.json");
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, "not json").unwrap();
    let output = agent_wait(&env)
        .args(["shell", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(record.to_str().unwrap()),
        "{output:?}"
    );
    std::fs::remove_file(&record).unwrap();
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "list", "--json"])).unwrap();
    assert_eq!(pending, serde_json::json!([]));
    assert!(!loop_instances_path(&env).exists() || wait_instances(&env).0.is_empty());
}

fn read_only_home_wait(
    env: &Env,
    bwrap: &std::path::Path,
    unshare_pid: bool,
) -> std::process::Command {
    let latest = env.store().paths().latest_snapshot().unwrap();
    if latest.exists() {
        std::fs::remove_file(&latest).unwrap();
    }
    let quote = |path: &std::path::Path| {
        shlex::try_quote(path.to_str().unwrap())
            .unwrap()
            .into_owned()
    };
    let wrapper = crate::common::write_path_shim(
        &env.home_root,
        "rimz-read-only-home",
        &format!(
            "exec {} --dev-bind / / {namespace} --ro-bind {home} {home} \
             --ro-bind {runtime} {runtime} -- {} \"$@\"",
            quote(bwrap),
            quote(&env.rimz_bin()),
            home = quote(&env.rimz_home()),
            runtime = quote(&env.runtime_root),
            namespace = if unshare_pid {
                "--unshare-pid --proc /proc"
            } else {
                ""
            },
        ),
    );
    agent_wait_at(env, &wrapper)
}

fn bubblewrap() -> Option<std::path::PathBuf> {
    match rimz::sandbox::preflight(rimz::config::Isolation::Scoped) {
        Ok(path) => Some(path.expect("scoped isolation uses bubblewrap")),
        Err(err) => {
            crate::common::skip(&format!("bubblewrap unusable: {err}"));
            None
        }
    }
}

fn pid_namespaced_bubblewrap() -> Option<std::path::PathBuf> {
    let bwrap = bubblewrap()?;
    let output = std::process::Command::new(&bwrap)
        .args([
            "--dev-bind",
            "/",
            "/",
            "--unshare-pid",
            "--proc",
            "/proc",
            "--",
            "true",
        ])
        .output()
        .expect("probe bubblewrap's PID namespace");
    if !output.status.success() {
        crate::common::skip(&format!(
            "bubblewrap unusable: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
        return None;
    }
    Some(bwrap)
}

fn register_owned_calling_agent(env: &Env) {
    register_agent_with_owner(
        env,
        "provider-session",
        "launch-session",
        "planner",
        LaunchParams::default(),
        Some(rimz::store::runtime::current_process_owner(
            rimz::pane::RuntimeOwnerKind::Agent,
            "launch-session",
        )),
    );
}

/// `wait list --json` rows without their clock columns. A refused command between
/// two reads can cross a second boundary, so `elapsed` and `check_in` may move by
/// one while every other column stays put.
fn pending_rows_at_rest(json: &str) -> serde_json::Value {
    let mut rows: serde_json::Value = serde_json::from_str(json).unwrap();
    for row in rows.as_array_mut().unwrap() {
        let row = row.as_object_mut().unwrap();
        for clock in ["elapsed", "elapsed_s", "check_in", "check_in_s"] {
            row.remove(clock);
        }
    }
    rows
}

fn assert_unresolved_wait(output: &std::process::Output, unchanged: &str, verb: &str) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.trim(),
        format!(
            "error: {unchanged}RimZ could not find the calling agent's live session from this \
             process (looked for kind `claude`, launch id `launch-session`). If this ran inside \
             a provider's command sandbox: Run `rimz {verb} …` as its own command, without a \
             chain, pipe, redirect, or `VAR=` prefix, so its routine rule runs outside the sandbox; if \
             `allow-routine-rimz` is off, approve the sandbox's escalation. If Codex's routine \
             rules predate this RimZ, refresh them with `rimz hooks install codex` from a user \
             shell and restart the agent"
        )
    );
}

#[test]
fn pid_namespaced_arm_refuses_unresolved_caller_and_leaves_no_wait() {
    let Some(bwrap) = pid_namespaced_bubblewrap() else {
        return;
    };
    let env = Env::new();
    env.record(&env.project_root);
    register_owned_calling_agent(&env);
    let output = read_only_home_wait(&env, &bwrap, true)
        .args(["shell", "--", "true"])
        .output()
        .unwrap();
    assert_unresolved_wait(&output, "nothing was armed: ", "shell");
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "list", "--json"])).unwrap();
    assert_eq!(pending, serde_json::json!([]));
    assert!(!loop_instances_path(&env).exists() || wait_instances(&env).0.is_empty());
}

#[test]
fn pid_namespaced_list_refuses_unresolved_caller_and_keeps_wait_pending() {
    let Some(bwrap) = pid_namespaced_bubblewrap() else {
        return;
    };
    let env = Env::new();
    env.record(&env.project_root);
    register_owned_calling_agent(&env);
    wait_ok(&env, &["watch", "--json", "pr=merged"]);
    let before = wait_instances(&env);
    let pending = wait_ok(&env, &["watch", "list", "--json"]);
    let output = read_only_home_wait(&env, &bwrap, true)
        .args(["watch", "list", "--json"])
        .output()
        .unwrap();
    assert_unresolved_wait(&output, "", "watch");
    assert_eq!(wait_instances(&env), before);
    assert_eq!(
        pending_rows_at_rest(&wait_ok(&env, &["watch", "list", "--json"])),
        pending_rows_at_rest(&pending)
    );
    wait_ok(&env, &["watch", "cancel", "--all"]);
}

#[test]
fn pid_namespaced_cancel_refuses_unresolved_caller_and_keeps_wait_pending() {
    let Some(bwrap) = pid_namespaced_bubblewrap() else {
        return;
    };
    let env = Env::new();
    env.record(&env.project_root);
    register_owned_calling_agent(&env);
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["watch", "--json", "pr=merged"])).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let before = wait_instances(&env);
    let pending = wait_ok(&env, &["watch", "list", "--json"]);
    let output = read_only_home_wait(&env, &bwrap, true)
        .args(["watch", "cancel", name])
        .output()
        .unwrap();
    assert_unresolved_wait(&output, "nothing was canceled: ", "watch");
    assert_eq!(wait_instances(&env), before);
    assert_eq!(
        pending_rows_at_rest(&wait_ok(&env, &["watch", "list", "--json"])),
        pending_rows_at_rest(&pending)
    );
    wait_ok(&env, &["watch", "cancel", name]);
}

#[test]
fn read_only_home_list_shows_hint_and_keeps_wait_pending() {
    let Some(bwrap) = bubblewrap() else {
        return;
    };
    let env = Env::new();
    env.record(&env.project_root);
    register_owned_calling_agent(&env);
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "--json", "--", "exec sleep 600"])).unwrap();
    let name = receipt["name"].as_str().unwrap();
    wait_until(&env, "process watcher did not start", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_some()
    });
    let before = wait_instances(&env);
    let output = read_only_home_wait(&env, &bwrap, false)
        .args(["shell", "list", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let gate = env.store().runtime_paths().lock_path("lease-gate.lock");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "error: this command cannot write the RimZ home, for example inside a provider's \
             command sandbox. Run `rimz shell …` as its own command, without a chain, pipe, \
             redirect, or `VAR=` prefix, so its routine rule runs outside the sandbox; if \
             `allow-routine-rimz` is off, approve the sandbox's escalation. If Codex's routine \
             rules predate this RimZ, refresh them with `rimz hooks install codex` from a user \
             shell and restart the agent\n  could not open lock file {}: Read-only file \
             system (os error 30)\n",
            gate.display()
        )
    );
    assert_eq!(wait_instances(&env), before);
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "list", "--json"])).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["name"], name);
    wait_ok(&env, &["shell", "cancel", name]);
}

#[test]
fn read_only_home_arm_shows_hint_and_leaves_no_wait() {
    let Some(bwrap) = bubblewrap() else {
        return;
    };
    let env = Env::new();
    env.record(&env.project_root);
    register_owned_calling_agent(&env);
    let output = read_only_home_wait(&env, &bwrap, false)
        .args(["shell", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.lines().next().is_some_and(|line| {
            line.starts_with("error: nothing was armed: this command cannot write the RimZ home")
        }),
        "{stderr}"
    );
    assert!(
        stderr.contains("Read-only file system (os error 30)"),
        "{stderr}"
    );
    assert!(!stderr.contains("nothing was canceled"), "{stderr}");
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "list", "--json"])).unwrap();
    assert_eq!(pending, serde_json::json!([]));
    assert!(!loop_instances_path(&env).exists() || wait_instances(&env).0.is_empty());
}

#[test]
fn read_only_home_cancel_shows_hint_and_keeps_wait_pending() {
    let Some(bwrap) = bubblewrap() else {
        return;
    };
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["watch", "--json", "pr=merged"])).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let before = wait_instances(&env);
    let output = read_only_home_wait(&env, &bwrap, false)
        .args(["watch", "cancel", name])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.lines().next().is_some_and(|line| {
            line.starts_with("error: nothing was canceled: this command cannot write the RimZ home")
        }),
        "{stderr}"
    );
    assert!(
        stderr.contains("Read-only file system (os error 30)"),
        "{stderr}"
    );
    assert!(!stderr.contains("nothing was armed"), "{stderr}");
    assert_eq!(wait_instances(&env), before);
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["watch", "list", "--json"])).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["name"], name);
    wait_ok(&env, &["watch", "cancel", name]);
    assert!(wait_instances(&env).0.is_empty());
}

#[test]
fn watcher_spawn_failure_leaves_no_wait() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let output = agent_wait(&env)
        .env("RIMZ_BIN", env.home_root.join("missing-rimz"))
        .args(["shell", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("starting watched command"),
        "{output:?}"
    );
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "list", "--json"])).unwrap();
    assert_eq!(pending, serde_json::json!([]));
    assert!(wait_instances(&env).0.is_empty());
}

/// A cancel whose task record does not read refuses with its path and keeps the wait.
#[test]
fn cancel_with_an_unreadable_task_record_keeps_wait_pending() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["watch", "--json", "pr=merged"])).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let before = wait_instances(&env);
    let record = env.rimz_home().join("loops/tasks.json");
    let kept = std::fs::read(&record).ok();
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, "not json").unwrap();
    let output = agent_wait(&env)
        .args(["watch", "cancel", name])
        .output()
        .unwrap();
    match &kept {
        Some(bytes) => std::fs::write(&record, bytes).unwrap(),
        None => std::fs::remove_file(&record).unwrap(),
    }
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(record.to_str().unwrap()),
        "{output:?}"
    );
    assert_eq!(wait_instances(&env), before);
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["watch", "list", "--json"])).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["name"], name);
    wait_ok(&env, &["watch", "cancel", name]);
    assert!(wait_instances(&env).0.is_empty());
}

#[test]
fn self_waits_put_lost_first_and_keep_long_redirects_whole() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let directory = env.home_root.join("long-directory-".repeat(9));
    std::fs::create_dir(&directory).unwrap();
    let clause = format!("> {} 2>&1", directory.join("gate.log").display());
    let command = format!("exec sleep 600 {clause}");
    let receipt: serde_json::Value = serde_json::from_str(&wait_ok(
        &env,
        &["shell", "--check-in", "90m", "--json", "--", &command],
    ))
    .unwrap();
    let live = receipt["name"].as_str().unwrap();
    let past: serde_json::Value = serde_json::from_str(&wait_ok(
        &env,
        &[
            "shell",
            "--check-in",
            "30m",
            "--json",
            "--",
            "exec sleep 600",
        ],
    ))
    .unwrap();
    let past = past["name"].as_str().unwrap();
    wait_until(&env, "watcher did not start", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), live)
            .unwrap()
            .is_some()
            && rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), past)
                .unwrap()
                .is_some()
    });
    let mut tasks = wait_instances(&env);
    tasks
        .0
        .get_mut(live)
        .unwrap()
        .wait_meta
        .as_mut()
        .unwrap()
        .armed_at = jiff::Timestamp::now() - jiff::SignedDuration::from_mins(55);
    tasks
        .0
        .get_mut(past)
        .unwrap()
        .wait_meta
        .as_mut()
        .unwrap()
        .armed_at = jiff::Timestamp::now() - jiff::SignedDuration::from_mins(47);
    let mut lost = tasks.0[live].clone();
    lost.wait_meta.as_mut().unwrap().armed_at =
        jiff::Timestamp::now() - jiff::SignedDuration::from_mins(41);
    lost.label = Some("lost build".into());
    tasks.0.insert("zz-lost".into(), lost);
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();
    let text = wait_ok(&env, &["shell", "list"]);
    assert!(text.starts_with("@planner#main · 3 shells\n"), "{text}");
    assert!(
        text.find("zz-lost").unwrap() < text.find(past).unwrap(),
        "{text}"
    );
    assert!(
        text.find(past).unwrap() < text.find(live).unwrap(),
        "{text}"
    );
    assert!(text.contains("lost · armed 41m ago"), "{text}");
    assert!(
        text.contains("watching 47m · past check-in at 30m"),
        "{text}"
    );
    let line = text.lines().find(|line| line.contains(live)).unwrap();
    assert!(line.contains("watching 55m · check-in in "), "{text}");
    assert!(line.ends_with(&clause), "{text}");
    assert!(line.chars().count() > 120, "{text}");
    assert!(!line.contains(" · in project"), "{text}");
    wait_ok(&env, &["shell", "cancel", "--all"]);
}

#[test]
fn cancel_all_leaves_subscriptions_and_stops_only_self_watchers() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    seed_signal_wait(&env, "deployment", "deploy.failed", true);
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "--json", "--", "exec sleep 600"])).unwrap();
    let name = receipt["name"].as_str().unwrap();
    wait_until(&env, "watcher did not start", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_some()
    });
    let canceled: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "cancel", "--all", "--json"])).unwrap();
    assert_eq!(canceled["canceled"], serde_json::json!([name]));
    assert_eq!(canceled["pending"], serde_json::json!([]));
    let watches: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["watch", "list", "--json"])).unwrap();
    assert_eq!(watches.as_array().unwrap().len(), 1, "{watches}");
    assert_eq!(watches[0]["owner"], "loop:deployment");
    wait_until(&env, "cancel left the self watcher alive", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_none()
    });
    assert_eq!(
        wait_ok(&env, &["watch", "cancel", "--all"]),
        "@planner#main · no watches · also woken by deploy.failed (loop deployment)\n"
    );
    wait_ok(&env, &["watch", "cancel", "deployment"]);
}

#[test]
fn room_wait_body_matches_loop_cards_and_json_lists_the_same_rows() {
    use rimz::config::{LoopConfig, Tasks};
    use std::collections::BTreeMap;

    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    register_agent(
        &env,
        "child-session",
        "child-launch",
        "modern-notch",
        LaunchParams {
            profile: Some("gatekeeper".into()),
            parent_agent_id: Some("launch-session".into()),
            launch_depth: Some(1),
            ..Default::default()
        },
    );
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["watch", "--json", "pr=merged"])).unwrap();
    let root_name = receipt["name"].as_str().unwrap();
    let mut tasks = wait_instances(&env);
    let root = tasks.0[root_name].clone();
    let mut child = root.clone();
    child.wait.as_mut().unwrap().session = "child-session".into();
    child.wait.as_mut().unwrap().handle = "@stale-child".into();
    tasks.0.insert("child-wait".into(), child);
    let mut orphan = root.clone();
    orphan.wait.as_mut().unwrap().session = "missing".into();
    orphan.wait.as_mut().unwrap().handle = "@stored".into();
    tasks.0.insert("orphan-wait".into(), orphan);
    let mut subscription = root.clone();
    subscription.wait_meta = None;
    subscription.fire_at = None;
    subscription.signal = Some("ci.failed".into());
    subscription.team = Some("forge#main".parse().unwrap());
    tasks.0.insert("team-wake".into(), subscription.clone());
    subscription.team = None;
    subscription.loop_task = Some("sweep".into());
    tasks.0.insert("resident-wake".into(), subscription.clone());
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();
    for name in [root_name, "child-wait", "orphan-wait"] {
        wait_ok(&env, &["loop", "disable", name]);
    }
    subscription.loop_task = None;
    let machine = LoopConfig {
        tasks: Tasks(BTreeMap::from([("handwritten".into(), subscription)])),
        ..Default::default()
    };
    std::fs::write(
        env.rimz_home().join("loop.toml"),
        toml::to_string(&machine).unwrap(),
    )
    .unwrap();
    let rows = env
        .rimz()
        .args(["watch", "list", "--json"])
        .output()
        .unwrap();
    assert!(
        rows.status.success(),
        "{}",
        String::from_utf8_lossy(&rows.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&rows.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 6);
    for row in rows {
        assert!(
            row.get("owner").is_some() && row.get("parent").is_some(),
            "{row}"
        );
        let name = row["name"].as_str().unwrap();
        let owner = match name {
            "team-wake" => "team:forge#main",
            "resident-wake" => "loop:sweep",
            "handwritten" => "loop:handwritten",
            _ => "self",
        };
        assert_eq!(row["owner"], owner);
        assert_eq!(
            row["parent"],
            if name == "child-wait" {
                receipt["target"].clone()
            } else {
                serde_json::Value::Null
            }
        );
    }
    let shell_text = |args: &[&str]| {
        let output = env.rimz().args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let waits = shell_text(&["watch", "list"]);
    assert!(
        waits.starts_with("~/project · whole room · 3 watches · 3 subscriptions\n\n"),
        "{waits}"
    );
    let body = waits.split_once("\n\n").unwrap().1;
    let loops = shell_text(&["loop", "list", "--all"]);
    let loop_body = loops
        .split_once("WAKE-UPS  ")
        .unwrap()
        .1
        .split_once('\n')
        .unwrap()
        .1;
    assert_eq!(body, loop_body);
    assert!(
        body.contains("  @planner#main › @gatekeeper (@modern-notch)\n"),
        "{body}"
    );
    assert!(body.contains("  @stored\n"), "{body}");
    assert!(body.contains("loop handwritten · ci.failed"), "{body}");
    assert!(!body.contains("@stale-child"), "{body}");
}

/// Registers the state a Claude `/clear` leaves: `cleared-session`, then its successor
/// `owner-session`, both on tmux pane `%1`, which is returned.
fn register_cleared_and_owner_sessions(env: &Env) -> rimz::pane::PaneRef {
    let pane = crate::common::tmux_pane("%1", "claude", &env.project_root);
    for session in ["cleared-session", "owner-session"] {
        let mut observation = AgentLifecycleObservation::new(
            Some(AgentSessionId::from(session)),
            LifecycleSignal::Registered,
        );
        observation.pane_id = Some(pane.pane_id.clone());
        observation.worktree_path = Some(env.project_root.display().to_string());
        observation.worktree_branch = Some("lane".into());
        crate::common::wait::stamp_hook_lane(env, &mut observation);
        env.store()
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }
    pane
}

#[test]
fn room_cards_name_the_pane_owner_like_the_arm_receipt() {
    let env = Env::new();
    env.record(&env.project_root);
    let pane = register_cleared_and_owner_sessions(&env);
    let snapshot = env.snapshot_json_with_panes(std::slice::from_ref(&pane));
    assert_eq!(snapshot["agents"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["agent_panes"][0]["agent_id"], "owner-session");
    let pane_fixture = env.write_pane_fixture(&[pane]);
    let run = |args: &[&str], caller: bool| {
        let mut command = env.rimz();
        command
            .args(["--mux", "tmux"])
            .args(args)
            .env("RIMZ_TEST_PANE_LIST", &pane_fixture);
        if caller {
            command
                .env("RIMZ_AGENT_KIND", "claude")
                .env("RIMZ_AGENT_ID", "owner-session");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    let receipt: serde_json::Value =
        serde_json::from_str(&run(&["watch", "--json", "pr=merged"], true)).unwrap();
    assert_eq!(receipt["target"], "@claude#main");
    let waits = run(&["watch", "list"], false);
    let body = waits.split_once("\n\n").unwrap().1;
    let loops = run(&["loop", "list", "--all"], false);
    let loop_body = loops
        .split_once("WAKE-UPS  ")
        .unwrap()
        .1
        .split_once('\n')
        .unwrap()
        .1;
    assert_eq!(body, loop_body);
    let header = body.lines().next().unwrap().trim();
    let name = header.strip_prefix(receipt["target"].as_str().unwrap());
    assert!(
        name.is_some_and(|name| name.starts_with(" (@") && name.ends_with(')')),
        "the header is the receipt's target with the agent's minted name beside it: {header}"
    );
}

#[test]
fn wait_labels_persist_for_every_trigger_and_replace_watch_previews() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let unicode_label = "é".repeat(60);
    for (verb, subject, label) in [
        ("watch", "pr=merged", unicode_label.as_str()),
        ("shell", "exec sleep 300", "run"),
    ] {
        let padded = format!("  {label}  ");
        let mut argv = vec![verb, "--label", &padded, "--json"];
        if verb == "shell" {
            argv.push("--");
        }
        argv.push(subject);
        let receipt: serde_json::Value = serde_json::from_str(&wait_ok(&env, &argv)).unwrap();
        let name = receipt["name"].as_str().unwrap();
        let tasks = serde_json::to_value(wait_instances(&env)).unwrap();
        assert_eq!(
            tasks[name]["label"], label,
            "label must survive the instance write"
        );
        let rows: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &[verb, "list", "--json"])).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_eq!(row["label"], label);
        if verb == "shell" {
            assert_eq!(row["trigger"], label);
            let text = wait_ok(&env, &["shell", "list"]);
            assert!(text.contains(label), "{text}");
            assert!(!text.contains(subject), "{text}");
        }
        let shown = wait_ok(&env, &["loop", "show", name]);
        assert!(
            shown
                .lines()
                .any(|line| line.trim_start().starts_with("label:") && line.contains(label)),
            "{shown}"
        );
        wait_ok(&env, &[verb, "cancel", name]);
    }
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["watch", "--json", "ci=passed"])).unwrap();
    assert!(receipt["pending"][0].get("label").is_none());
    let tasks = serde_json::to_value(wait_instances(&env)).unwrap();
    assert!(
        tasks[receipt["name"].as_str().unwrap()]
            .get("label")
            .is_none()
    );
}

#[test]
fn watch_lines_keep_their_condition_and_add_the_label_once() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let mut lines = Vec::new();
    for (expr, trigger) in [
        ("pr=merged", "when pr=merged"),
        ("ci=passed", "when ci=passed"),
    ] {
        let label = format!("label-{}", lines.len());
        let argv = ["watch", "--label", &label, "--json", expr];
        let receipt: serde_json::Value = serde_json::from_str(&wait_ok(&env, &argv)).unwrap();
        lines.push((receipt["name"].as_str().unwrap().to_owned(), trigger, label));
    }
    let shell = env.rimz().args(["watch", "list"]).output().unwrap();
    assert!(shell.status.success());
    for view in [
        wait_ok(&env, &["watch", "list"]),
        String::from_utf8(shell.stdout).unwrap(),
    ] {
        for (name, trigger, label) in &lines {
            let line = view
                .lines()
                .find(|line| line.contains(name.as_str()))
                .unwrap_or_else(|| panic!("{name} missing: {view}"));
            assert!(line.contains(trigger), "{line}");
            assert!(line.ends_with(&format!(" · {label}")), "{line}");
            assert_eq!(view.matches(label.as_str()).count(), 1, "{view}");
        }
    }
}

#[test]
fn wait_and_loop_labels_refuse_invalid_text_before_writing() {
    let env = Env::new();
    for label in [
        String::new(),
        "   ".into(),
        "x".repeat(61),
        "é".repeat(61),
        "two\nlines".into(),
        "line\rreturn".into(),
        "end\n".into(),
    ] {
        for args in [
            vec!["watch", "--label", &label, "pr=merged"],
            vec![
                "loop", "add", "labelled", "--every", "1h", "--label", &label, "--", "true",
            ],
        ] {
            let output = env.rimz().args(args).output().unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "invalid label accepted");
            assert!(
                error.contains("label must be one line of 1 to 60 characters"),
                "{error}"
            );
            assert!(!loop_instances_path(&env).exists());
            assert!(!env.rimz_home().join("loop.toml").exists());
        }
    }
}

#[test]
fn loop_labels_persist_in_machine_and_project_tasks() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    for (name, extra) in [
        ("machine", vec!["--", "true"]),
        ("project", vec!["--project", "--", "true"]),
    ] {
        let mut args = vec![
            "loop",
            "add",
            name,
            "--every",
            "1h",
            "--label",
            "  gate docs  ",
        ];
        args.extend(extra);
        wait_ok(&env, &args);
        let text = match name {
            "machine" => std::fs::read_to_string(env.rimz_home().join("loop.toml")).unwrap(),
            _ => std::fs::read_to_string(env.project_root.join(".rimz/config.toml")).unwrap(),
        };
        let value = toml::from_str::<toml::Value>(&text).unwrap();
        assert_eq!(value["tasks"][name]["label"].as_str(), Some("gate docs"));
        let shown = wait_ok(&env, &["loop", "show", name]);
        assert!(
            shown
                .lines()
                .any(|line| line.trim_start().starts_with("label:") && line.contains("gate docs")),
            "{shown}"
        );
    }
}

#[test]
fn wait_run_preserves_shell_string_and_runs_both_commands() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let command = "printf 'one\\n' && printf 'two\\n'";
    let receipt = wait_ok(&env, &["shell", "--json", "--", command]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["trigger"], format!("shell: {command}"));
    let records = wait_for_wait_records(&env, 1);
    let check = records[0].check.as_ref().unwrap();
    assert_eq!(check.code, Some(0));
    assert_eq!(
        std::fs::read_to_string(check.output_path.as_ref().unwrap()).unwrap(),
        "one\ntwo\n"
    );
    let message = wait_ok(
        &env,
        &[
            "message",
            "show",
            records[0].message_id.as_ref().unwrap().as_str(),
        ],
    );
    assert!(
        message.contains(&format!("waited on `{command}`")),
        "{message}"
    );
    wait_for_no_wait_instances(&env);
    // Several words run as that argv, each quoted for sh.
    let receipt = wait_ok(
        &env,
        &[
            "shell",
            "--json",
            "--",
            "sh",
            "-c",
            "printf one; printf two",
        ],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let quoted = "sh -c 'printf one; printf two'";
    assert_eq!(receipt["trigger"], format!("shell: {quoted}"));
    let records = wait_for_wait_records(&env, 2);
    let check = records[1].check.as_ref().unwrap();
    assert_eq!(check.code, Some(0));
    assert_eq!(
        std::fs::read_to_string(check.output_path.as_ref().unwrap()).unwrap(),
        "onetwo"
    );
    wait_for_no_wait_instances(&env);
}

/// The watcher's own warnings go to the room's watcher log, so the output
/// file holds the watched command's bytes from its first.
#[test]
fn wait_run_output_file_holds_only_the_command_output() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    // A file in place of the live-runs directory makes the watcher's workflow wake warn.
    let workflow = env
        .store()
        .paths()
        .workflow(&rimz::ids::WorkflowRunId::new());
    let homes = workflow.home.parent().unwrap();
    std::fs::create_dir_all(homes.parent().unwrap()).unwrap();
    std::fs::write(homes, "not a directory").unwrap();
    let output = agent_wait(&env)
        .args(["shell", "--", "printf 'first line\\nsecond line\\n'"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let records = wait_for_wait_records(&env, 1);
    let output_path = records[0].check.as_ref().unwrap().output_path.clone();
    wait_for_no_wait_instances(&env);
    assert_eq!(
        std::fs::read_to_string(output_path.unwrap()).unwrap(),
        "first line\nsecond line\n"
    );
    let log = std::fs::read_to_string(env.store().paths().wait_watch_log()).unwrap();
    assert!(log.contains("could not wake workflows"), "{log}");
}

/// A watched command is the agent's own work, so it keeps the agent's temp
/// unit, where the agent was told its files live.
#[test]
fn wait_run_keeps_the_agent_temp_environment() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let unit = env.home_root.join("unit");
    let user = env.home_root.join("user-tmp");
    std::fs::create_dir_all(&unit).unwrap();
    std::fs::create_dir_all(&user).unwrap();
    let command = "printf '%s|%s|%s' \"${TMPDIR-unset}\" \"${CLAUDE_CODE_TMPDIR-unset}\" \"${RIMZ_USER_TMPDIR-unset}\"";
    let output = agent_wait(&env)
        .env("TMPDIR", &unit)
        .env("RIMZ_USER_TMPDIR", &user)
        .env("CLAUDE_CODE_TMPDIR", &unit)
        .env("RIMZ_TEMP_ROOT_KEYS", "CLAUDE_CODE_TMPDIR")
        .args(["shell", "--", command])
        .output()
        .expect("arm watched wait");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = wait_for_wait_records(&env, 1);
    let check = records[0].check.as_ref().unwrap();
    let (unit, user) = (unit.display(), user.display());
    assert_eq!(
        std::fs::read_to_string(check.output_path.as_ref().unwrap()).unwrap(),
        format!("{unit}|{unit}|{user}")
    );
    wait_for_no_wait_instances(&env);
}

#[test]
fn wait_run_rejects_empty_commands() {
    let env = Env::new();
    register_calling_agent(&env);
    for command in ["", "   "] {
        let output = agent_wait(&env)
            .args(["shell", "--", command])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("a shell needs a command"), "{stderr}");
        assert!(!loop_instances_path(&env).exists());
    }
}

/// A command the shell cannot start still answers once: its exit and the shell's own error in
/// the output file, and the row retires.
#[test]
fn a_shell_whose_command_cannot_start_reports_its_exit_and_error() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let output = agent_wait(&env)
        .args(["shell", "--", "rimz-absent-program-xyz"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let records = wait_for_wait_records(&env, 1);
    let check = records[0].check.as_ref().unwrap();
    let error = std::fs::read_to_string(check.output_path.as_ref().unwrap()).unwrap();
    assert!(error.contains("rimz-absent-program-xyz"), "{error}");
    let messages = wait_for_wait_messages(&env, 1);
    assert!(
        messages[0].text.contains("exit 127 after"),
        "{}",
        messages[0].text
    );
    wait_for_no_wait_instances(&env);
}

#[test]
fn wait_checkin_default_yields_to_keepalive_but_explicit_timeout_wins() {
    for (switch, ttl, explicit, expected) in [
        ("true", "1h", None, None),
        ("true", "5m", None, Some("30m")),
        ("false", "1h", None, Some("30m")),
        ("true", "off", None, Some("30m")),
        ("true", "1h", Some("1s"), Some("1s")),
    ] {
        for (verb, what) in [
            ("shell", &["--", "sleep 600"][..]),
            ("watch", &["pr=merged"][..]),
        ] {
            let env = Env::new();
            env.record(&env.project_root);
            register_calling_agent(&env);
            wait_ok(&env, &["config", "set", "harness.cache_keepalive", switch]);
            wait_ok(
                &env,
                &["config", "set", "harness.prompt_cache_ttl.claude", ttl],
            );
            let mut args = vec![verb, "--json"];
            if let Some(timeout) = explicit {
                args.extend(["--check-in", timeout]);
            }
            args.extend(what);
            let receipt: serde_json::Value = serde_json::from_str(&wait_ok(&env, &args)).unwrap();
            let name = receipt["name"].as_str().unwrap();
            let tasks = wait_instances(&env);
            assert_eq!(
                tasks.0[name].timeout.as_deref(),
                expected,
                "{verb}: keepalive {switch}, TTL {ttl}, explicit {explicit:?}"
            );
            wait_ok(&env, &[verb, "cancel", name]);
        }
    }
}

#[test]
fn agents_wait_ends_on_spent_provider_pause() {
    use rimz::agents::{
        AgentContext, AgentRateLimits, AgentTurnError, RateLimitCacheEntry, RateLimitWindow,
        RateLimitsCache, TurnErrorClass,
    };

    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let store = env.store();
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("provider-session")),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    let at = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(1);
    let mut context = AgentContext::new("claude", at);
    context.turn_error = Some(AgentTurnError {
        model: None,
        ended_turn: false,
        class: TurnErrorClass::PausedRateLimit,
        at,
        label: None,
        refill_at: None,
    });
    rimz::store::agent_context::write_record(
        &env.runtime_paths(),
        &rimz::agents::context::record::AgentContextRecord::new(
            "claude",
            "provider-session",
            context,
        ),
    )
    .unwrap();
    env.publish_rate_limits(&RateLimitsCache {
        entries: [(
            rimz::ids::LoginKey::default_for(AgentKind::new_unchecked("claude")).into(),
            RateLimitCacheEntry {
                limits: AgentRateLimits {
                    windows: vec![RateLimitWindow {
                        used_percentage: Some(100),
                        resets_at: Some(jiff::Timestamp::now() - std::time::Duration::from_secs(1)),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )]
        .into(),
        ..Default::default()
    });
    for stream in [false, true] {
        let mut command = env.rimz();
        command.args(["agents", "wait", "@planner", "--timeout", "0s", "--json"]);
        if stream {
            command.arg("--stream");
        }
        let output = command.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(1),
            "stream: {stream}, {output:?}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("paused"),
            "{output:?}"
        );
        if !stream {
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["@planner"]["status"], "failed");
        }
    }
}

fn agents_wait_exit(env: &Env) -> Option<i32> {
    env.rimz()
        .args(["agents", "wait", "@planner", "--timeout", "1s"])
        .output()
        .expect("run agents wait")
        .status
        .code()
}

#[test]
fn calling_agent_can_list_and_cancel_human_armed_loop_delivery_by_launch_identity() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    seed_signal_wait(&env, "deployment", "deploy.failed", true);
    let name = "deployment";

    let listed = agent_wait(&env)
        .args(["watch", "list", "--json"])
        .output()
        .expect("list wait as target agent");
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let rows: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("wait list JSON");
    assert_eq!(rows.as_array().expect("wait rows").len(), 1);
    assert_eq!(rows[0]["name"], name);
    assert_eq!(rows[0]["type"], "signal");
    assert_eq!(rows[0]["state"], "waiting");
    assert_eq!(rows[0]["owner"], "loop:deployment");
    assert_eq!(rows[0].get("parent"), Some(&serde_json::Value::Null));
    for key in ["elapsed", "elapsed_s", "check_in", "check_in_s"] {
        assert_eq!(rows[0].get(key), Some(&serde_json::Value::Null));
    }
    assert!(rows[0].get("dir").is_none());
    assert_eq!(
        wait_ok(&env, &["watch", "list"]),
        "@planner#main · no watches · also woken by deploy.failed (loop deployment)\n"
    );

    let canceled = agent_wait(&env)
        .args(["watch", "cancel", name])
        .output()
        .expect("cancel wait as target agent");
    assert!(
        canceled.status.success(),
        "{}",
        String::from_utf8_lossy(&canceled.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&canceled.stdout),
        format!("canceled {name}\n@planner#main · no watches\n")
    );
}

#[test]
fn wait_arm_refuses_a_plain_shell() {
    let env = Env::new();
    env.record(&env.project_root);
    let output = env
        .rimz()
        .args(["watch", "pr=merged"])
        .output()
        .expect("run wait from shell");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("arming a watch is only available to an agent"),
        "{stderr}"
    );
}

/// Each verb's `--check-in` help words what its one message says, as its MCP field does.
#[test]
fn each_verbs_check_in_help_names_what_it_waits_on() {
    let env = Env::new();
    for (verb, wording) in [
        (
            "shell",
            "Send one still-running message after this long; never stops the command.",
        ),
        (
            "watch",
            "Send one still-not-met message after this long; never stops the watch.",
        ),
    ] {
        let output = env.rimz().args([verb, "--help"]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(help.contains(wording), "{verb}: {help}");
        assert!(!help.contains("stops the wait"), "{verb}: {help}");
    }
}

#[test]
fn wait_rejects_watch_checkins_at_or_above_24_hours() {
    let env = Env::new();
    register_calling_agent(&env);
    for timeout in ["24h", "25h"] {
        let output = agent_wait(&env)
            .args(["shell", "--check-in", timeout, "--", "true"])
            .output()
            .expect("reject long watch check-in");
        assert!(!output.status.success(), "accepted --check-in {timeout}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--check-in must be less than 24h"),
            "{stderr}"
        );
    }
}

/// A linked worktree of the fixture project, canonical; `None` (skipped) when
/// git is unavailable.
fn linked_worktree(env: &Env) -> Option<std::path::PathBuf> {
    let initialized = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&env.project_root)
        .status();
    if !initialized.is_ok_and(|status| status.success()) {
        crate::common::skip("git unavailable");
        return None;
    }
    let commit = std::process::Command::new("git")
        .args([
            "-c",
            "user.email=rimz@example.invalid",
            "-c",
            "user.name=RimZ",
            "commit",
            "--allow-empty",
            "-qm",
            "initial",
        ])
        .current_dir(&env.project_root)
        .status()
        .expect("run git commit");
    assert!(commit.success(), "git commit failed");
    let linked = env.home_root.join("linked");
    let add = std::process::Command::new("git")
        .args(["worktree", "add", "-q", "-b", "linked"])
        .arg(&linked)
        .current_dir(&env.project_root)
        .status()
        .expect("run git worktree add");
    assert!(add.success(), "git worktree add failed");
    Some(canonical(&linked))
}

#[test]
fn watched_wait_runs_in_the_arming_worktree() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(linked) = linked_worktree(&env) else {
        return;
    };
    let release = env.home_root.join("release-watch");
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let output = agent_wait(&env)
        .current_dir(&linked)
        .args([
            "shell",
            "--json",
            "--",
            &format!(
                "while [ ! -e {} ]; do sleep 0.025; done; pwd -P",
                shlex::try_quote(release.to_str().unwrap()).unwrap()
            ),
        ])
        .output()
        .expect("arm watched wait from linked worktree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let tasks = wait_instances(&env);
    let entry = &tasks.0[name];
    assert_eq!(entry.root, canonical(&env.project_root));
    assert_eq!(entry.dir.as_deref(), Some(linked.as_path()));
    let listed: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "list", "--json"])).unwrap();
    assert_eq!(listed[0]["name"], name);
    assert_eq!(listed[0]["dir"], "~/linked");

    std::fs::write(&release, "").expect("release watched command");
    let records = wait_for_wait_records(&env, 1);
    assert_eq!(records[0].root, Some(canonical(&env.project_root)));
    let check = records[0].check.as_ref().unwrap();
    assert_eq!(check.code, Some(0));
    assert_eq!(check.output.trim(), linked.to_str().unwrap());
    wait_for_no_wait_instances(&env);
}

#[test]
fn watched_failure_preserves_full_output_and_delivers_its_summary() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let output = agent_wait(&env)
        .args([
            "shell",
            "--json",
            "--",
            "sleep 1; seq 1 5000; printf watched; exit 3",
        ])
        .output()
        .expect("arm watched wait");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = wait_for_wait_records(&env, 1);
    let check = records[0].check.as_ref().unwrap();
    let path = check.output_path.as_ref().expect("watch output path");
    assert_eq!(
        path,
        &env.store()
            .paths()
            .out_reader_dir(Some("planner"))
            .join(format!("{}.output", records[0].task)),
        "the output lands in the arming agent's out/ directory"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "the reader's out/ directory is private"
        );
    }
    let full = std::fs::read_to_string(path).expect("full watch output");
    assert_eq!(
        full,
        format!(
            "{}watched",
            (1..=5000)
                .map(|line| format!("{line}\n"))
                .collect::<String>()
        )
    );
    assert!(check.output.len() <= 4096);
    assert!(full.ends_with(&check.output));
    let message_id = records[0].message_id.as_ref().unwrap();
    let message = wait_ok(&env, &["message", "show", message_id.as_str()]);
    assert!(message.contains("waited on `"), "{message}");
    assert!(message.contains("exit 3 after"), "{message}");
    assert!(!message.contains("exit 3 after 0s"), "{message}");
    assert!(!message.contains("armed by you"), "{message}");
    assert!(
        message.contains(&format!(
            "output: {} ({})",
            path.display(),
            rimz::disk::summary::FileSummary::measure(path)
                .unwrap()
                .label()
        )),
        "{message}"
    );
    assert!(!message.contains("5000\n"), "{message}");
    assert!(!message.contains(&check.output), "{message}");
    let logs = wait_ok(&env, &["loop", "logs", &records[0].task]);
    assert!(
        logs.contains(&records[0].watch.as_ref().unwrap().label()),
        "{logs}"
    );
    assert!(logs.contains(&path.display().to_string()), "{logs}");
    #[cfg(target_os = "linux")]
    wait_for_loop_runs_to_exit(&env, &records[0].task);
    let output = env
        .rimz()
        .args(["loop", "show", &records[0].task])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr, retained_runs_refusal(&stderr, &records[0].task, 1));
    assert!(!message.contains("--- watch"), "{message}");
    assert_eq!(wait_for_wait_messages(&env, 1).len(), 1);

    wait_for_no_wait_instances(&env);
}

#[test]
fn watched_wait_survives_the_arming_process_group_exiting() {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let child = agent_wait(&env)
        .args(["shell", "--json", "--", "sleep 1; printf survived"])
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let armer_group = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
    let armed = child.wait_with_output().unwrap();
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let killed = nix::sys::signal::killpg(armer_group, nix::sys::signal::Signal::SIGTERM);
    assert!(killed.is_ok() || killed == Err(nix::errno::Errno::ESRCH));
    let records = wait_for_wait_records(&env, 1);
    assert!(
        matches!(records[0].watch.as_ref().unwrap(), rimz::harness::signal::WatchVerdict::Exited { code: Some(0), elapsed_ms } if *elapsed_ms >= 1_000)
    );
    let message = wait_for_wait_messages(&env, 1).pop().unwrap();
    assert!(message.text.starts_with("waited on `"), "{}", message.text);
    assert!(message.text.contains("survived"), "{}", message.text);
}

#[test]
fn missing_watcher_row_reports_its_error_to_the_wait_output() {
    let env = Env::new();
    env.record(&env.project_root);
    let store = env.store();
    let dir = store.paths().out_reader_dir(None);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("wait-missing.output");
    let output = std::fs::File::create(&path).unwrap();
    let status = env
        .rimz()
        .args(["shell", "watcher", "wait-missing"])
        .stderr(output)
        .status()
        .unwrap();
    assert!(!status.success());
    let log = std::fs::read_to_string(path).unwrap();
    assert!(
        log.contains("no wait named wait-missing in the catalog"),
        "{log}"
    );
}

#[test]
fn lost_watcher_delivers_elapsed_and_the_existing_output_summary() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let receipt = wait_ok(
        &env,
        &["shell", "--json", "--", "printf started; exec sleep 30"],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let store = env.store();
    let path = store
        .paths()
        .out_reader_dir(Some("planner"))
        .join(format!("{name}.output"));
    wait_until(&env, "watcher did not start", || {
        std::fs::read_to_string(&path).is_ok_and(|output| output == "started")
    });
    let watcher = rimz::harness::schedule::signal::watcher_info(store.runtime_paths(), name)
        .unwrap()
        .unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(watcher.pid).unwrap()),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    wait_until(&env, "watcher did not stop", || {
        rimz::harness::schedule::signal::watcher_info(store.runtime_paths(), name)
            .unwrap()
            .is_none()
    });
    let armed_at = jiff::Timestamp::now()
        .checked_sub(std::time::Duration::from_secs(60))
        .unwrap();
    let mut tasks = wait_instances(&env);
    tasks
        .0
        .get_mut(name)
        .unwrap()
        .wait_meta
        .as_mut()
        .unwrap()
        .armed_at = armed_at;
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();
    std::fs::write(
        store.runtime_paths().lane_path("loop-fire.json"),
        serde_json::to_vec(&std::collections::BTreeMap::from([(name, armed_at)])).unwrap(),
    )
    .unwrap();
    wait_ok(&env, &["loop", "tick"]);
    let records = wait_for_wait_records(&env, 1);
    let verdict = records[0].watch.as_ref().unwrap();
    assert!(
        matches!(verdict, rimz::harness::signal::WatchVerdict::Lost { elapsed_ms, .. } if *elapsed_ms >= 60_000)
    );
    assert_eq!(
        records[0].check.as_ref().unwrap().output_path.as_ref(),
        Some(&path)
    );
    let message = wait_for_wait_messages(&env, 1).pop().unwrap();
    assert!(message.text.contains(&verdict.label()), "{}", message.text);
    assert!(
        message
            .text
            .contains(&format!("output: {} (<1k tokens, 1 line)", path.display())),
        "{}",
        message.text
    );
    assert_eq!(records[0].check.as_ref().unwrap().output, "started");
    assert!(!message.text.lines().any(|line| line == "started"));
    let logs = wait_ok(&env, &["loop", "logs", name]);
    assert!(logs.contains(&verdict.label()), "{logs}");
    assert!(logs.contains(&path.display().to_string()), "{logs}");
    #[cfg(target_os = "linux")]
    wait_for_loop_runs_to_exit(&env, &records[0].task);
    let output = env
        .rimz()
        .args(["loop", "show", &records[0].task])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stderr, retained_runs_refusal(&stderr, &records[0].task, 1));
}

/// A check-in binds the row's task record, so its stamps move to the task id; a tick past the
/// grace still finds the live watcher's lock under the row name, and the exit is the one
/// terminal outcome.
#[cfg(target_os = "linux")]
#[test]
fn live_watcher_of_a_checked_in_wait_is_not_lost_past_the_grace() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let release = env.home_root.join("release");
    let command = format!(
        "printf started; while [ ! -e {} ]; do sleep 0.05; done",
        shlex::try_quote(release.to_str().unwrap()).unwrap()
    );
    let receipt = wait_ok(
        &env,
        &["shell", "--json", "--check-in", "1s", "--", &command],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let checkin = wait_for_wait_messages(&env, 1).pop().unwrap();
    assert!(
        checkin.text.contains("still running after"),
        "{}",
        checkin.text
    );
    wait_for_loop_runs_to_exit(&env, name);
    let store = env.store();
    let armed_at = jiff::Timestamp::now()
        .checked_sub(std::time::Duration::from_secs(60))
        .unwrap();
    std::fs::write(
        store.runtime_paths().lane_path("loop-fire.json"),
        serde_json::to_vec(&std::collections::BTreeMap::from([(name, armed_at)])).unwrap(),
    )
    .unwrap();
    wait_ok(&env, &["loop", "tick"]);
    wait_for_loop_runs_to_exit(&env, name);
    assert_eq!(wait_records(&env).len(), 1, "{:?}", wait_records(&env));
    assert!(wait_instances(&env).0.contains_key(name));

    std::fs::write(&release, "").unwrap();
    wait_for_wait_records(&env, 2);
    wait_for_no_wait_instances(&env);
    wait_for_loop_runs_to_exit(&env, name);
    let records = wait_records(&env);
    assert_eq!(records.len(), 2, "{records:?}");
    let verdict = records[1].watch.as_ref().unwrap();
    assert_eq!(
        records[1].check.as_ref().unwrap().code,
        Some(0),
        "{verdict:?}"
    );
    let (mut messages, history) = store.list_messages_and_history().unwrap();
    messages.extend(history);
    assert_eq!(messages.len(), 2, "{messages:?}");
    let exit = messages
        .iter()
        .find(|message| Some(&message.message_id) == records[1].message_id.as_ref())
        .unwrap();
    assert!(exit.text.contains(&verdict.label()), "{}", exit.text);
    let logs = wait_ok(&env, &["loop", "logs", name]);
    assert!(logs.contains(&verdict.label()), "{logs}");
    assert!(!logs.contains("watcher died"), "{logs}");
}

#[test]
fn watch_retires_without_delivery_when_its_polarity_does_not_match() {
    for (on, command) in [("fail", "true"), ("success", "false")] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        let receipt = wait_ok(&env, &["shell", "--on", on, "--", command]);
        let armed = receipt.lines().next().unwrap();
        assert!(
            armed.ends_with(&format!(": shell: {command} → @planner#main (on {on})")),
            "{receipt}"
        );
        assert!(receipt.contains("@planner#main · 1 shell\n"), "{receipt}");
        let records = wait_for_wait_records(&env, 1);
        assert_eq!(records[0].result.label(), "skipped");
        assert!(records[0].message_id.is_none());
        assert!(env.store().list_pending_messages().unwrap().is_empty());
        wait_for_no_wait_instances(&env);
    }
}

#[test]
fn self_wait_queues_with_any_gate_for_working_and_idle_targets() {
    for working in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        // The idle target rests after a finished turn, so only the wake in
        // flight can keep `agents wait` open once the wait row is consumed.
        let turn: &[LifecycleSignal] = if working {
            &[LifecycleSignal::TurnStarted { turn_id: None }]
        } else {
            &[
                LifecycleSignal::TurnStarted { turn_id: None },
                LifecycleSignal::TurnEnded {
                    errored: false,
                    parked_on_background: false,
                    turn_id: None,
                },
            ]
        };
        for signal in turn {
            let observation = AgentLifecycleObservation::new(
                Some(AgentSessionId::from("provider-session")),
                signal.clone(),
            );
            env.store()
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "rimz-test",
                    agent_kind: AgentKind::new_unchecked("claude"),
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        }
        wait_ok(&env, &["shell", "--", "printf self-wait-marker"]);
        let messages = wait_for_wait_messages(&env, 1);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].agent_id.as_str(), "provider-session");
        assert_eq!(messages[0].gate, DeliveryGate::Any);
        assert_eq!(
            messages[0].sender,
            MessageSender::Harness {
                notice: HarnessNotice::Wait
            }
        );
        assert!(messages[0].text.contains("self-wait-marker"));
        wait_for_no_wait_instances(&env);
        assert_eq!(
            agents_wait_exit(&env),
            Some(124),
            "wait settled while its wake was in flight"
        );
    }
}

#[test]
fn self_wait_parks_at_an_open_prompt_then_delivers_at_the_turn_boundary() {
    let env = Env::new();
    let pane_fixture = register_waiting_caller(&env);
    let output = traced_wait(&env, &pane_fixture)
        .args(["shell", "--", "printf wake-marker"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let records = wait_for_wait_records(&env, 1);
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    assert_eq!(messages.len(), 1);
    let wake = &messages[0];
    assert_eq!(wake.status, MessageStatus::Queued);
    assert_eq!(wake.agent_id.as_str(), "provider-session");
    assert_eq!(wake.gate, DeliveryGate::Any);
    assert_eq!(
        wake.sender,
        MessageSender::Harness {
            notice: HarnessNotice::Wait
        }
    );
    assert_eq!(
        wake.last_error.as_deref(),
        Some("agent is waiting on input in its pane")
    );
    assert_eq!(wait_for_wait_messages(&env, 1).len(), 1);
    assert_no_wait_paste(&env);
    wait_for_no_wait_instances(&env);
    assert_eq!(records[0].result.label(), "delivered");
    assert_eq!(records[0].message_id.as_ref(), Some(&wake.message_id));
    assert_eq!(agents_wait_exit(&env), Some(124));

    let output = traced_wait(&env, &pane_fixture)
        .args(["message", "sweep"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let parked = env.store().list_pending_messages().unwrap();
    assert_eq!(parked[0].status, MessageStatus::Queued);
    assert_eq!(parked[0].attempts, wake.attempts);
    assert_no_wait_paste(&env);

    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("provider-session")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    env.store()
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "Stop",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    assert_eq!(agents_wait_exit(&env), Some(124), "wake is still owed");
    let output = traced_wait(&env, &pane_fixture)
        .args(["message", "sweep"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let sent = env.store().list_messages().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].message_id, wake.message_id);
    assert_eq!(sent[0].status, MessageStatus::Sent);
    let trace = std::fs::read_to_string(env.project_root.join("wait-trace.log")).unwrap();
    let marker = "wake-marker"
        .bytes()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join("\t");
    assert!(
        trace.lines().any(|line| {
            line.contains("\taction\twrite\t--pane-id\tterminal_3\t27\t91\t50\t48\t48\t126\t")
                && line.contains(&marker)
                && line.ends_with("\t27\t91\t50\t48\t49\t126")
        }),
        "wake was not pasted to the agent's pane: {trace}"
    );
}

#[test]
fn watch_checkin_parks_at_an_open_prompt_without_consuming_or_killing_command() {
    let env = Env::new();
    let pane_fixture = register_waiting_caller(&env);
    let release = env.home_root.join("release");
    let pid_path = env.home_root.join("command.pid");
    let command = format!(
        "printf '%s' \"$$\" > {}; printf checkin-marker; while [ ! -e {} ]; do sleep 0.05; done; printf final-marker",
        shlex::try_quote(pid_path.to_str().unwrap()).unwrap(),
        shlex::try_quote(release.to_str().unwrap()).unwrap()
    );
    let output = traced_wait(&env, &pane_fixture)
        .args(["shell", "--json", "--check-in", "1s", "--", &command])
        .output()
        .unwrap();
    assert!(output.status.success());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let records = wait_for_wait_records(&env, 1);
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    assert_eq!(messages.len(), 1);
    let checkin = &messages[0];
    assert_eq!(checkin.status, MessageStatus::Queued);
    assert_eq!(checkin.gate, DeliveryGate::Any);
    assert_eq!(
        checkin.sender,
        MessageSender::Harness {
            notice: HarnessNotice::Wait
        }
    );
    assert_eq!(
        checkin.last_error.as_deref(),
        Some("agent is waiting on input in its pane")
    );
    assert!(checkin.text.contains("still running after"));
    assert_eq!(records[0].result.label(), "delivered");
    assert_eq!(records[0].message_id.as_ref(), Some(&checkin.message_id));
    assert_no_wait_paste(&env);
    let listed: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["shell", "list", "--json"])).unwrap();
    assert_eq!(listed[0]["name"], name);
    assert_eq!(listed[0]["state"], "watching");
    assert!(wait_instances(&env).0.contains_key(name));
    let mut pid = None;
    wait_until(&env, "command did not write its pid", || {
        pid = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|text| text.parse::<u32>().ok());
        pid.is_some()
    });
    assert!(rimz::proc::process_is_live(pid.unwrap(), None));

    std::fs::write(&release, "").unwrap();
    let records = wait_for_wait_records(&env, 2);
    assert_eq!(records[1].check.as_ref().unwrap().code, Some(0));
    wait_for_no_wait_instances(&env);
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    assert_eq!(messages.len(), 2);
    let canceled = messages
        .iter()
        .find(|message| message.message_id == checkin.message_id)
        .unwrap();
    assert_eq!(canceled.status, MessageStatus::Canceled);
    let wake = messages
        .iter()
        .find(|message| Some(&message.message_id) == records[1].message_id.as_ref())
        .unwrap();
    assert_eq!(wake.status, MessageStatus::Queued);
    assert_eq!(wake.gate, DeliveryGate::Any);
    assert_no_wait_paste(&env);
}

#[test]
fn watch_checkin_is_canceled_when_the_final_wake_steers_live() {
    let env = Env::new();
    let pane_fixture = register_waiting_caller(&env);
    let release = env.home_root.join("release");
    let command = format!(
        "printf checkin-marker; while [ ! -e {} ]; do sleep 0.05; done; printf final-marker",
        shlex::try_quote(release.to_str().unwrap()).unwrap()
    );
    let output = traced_wait(&env, &pane_fixture)
        .args(["shell", "--check-in", "1s", "--", &command])
        .output()
        .unwrap();
    assert!(output.status.success());
    let records = wait_for_wait_records(&env, 1);
    let checkin = wait_for_wait_messages(&env, 1).pop().unwrap();
    assert_eq!(checkin.status, MessageStatus::Queued);
    assert_eq!(records[0].message_id.as_ref(), Some(&checkin.message_id));
    assert_no_wait_paste(&env);

    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("provider-session")),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    env.store()
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "rimz-test",
            agent_kind: AgentKind::new_unchecked("claude"),
            event_name: "Stop",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    std::fs::write(&release, "").unwrap();
    let records = wait_for_wait_records(&env, 2);
    assert!(matches!(
        records[1].watch.as_ref().unwrap(),
        rimz::harness::signal::WatchVerdict::Exited { code: Some(0), .. }
    ));
    assert_eq!(records[1].result.label(), "delivered");
    wait_for_no_wait_instances(&env);
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    assert_eq!(messages.len(), 2);
    let canceled = messages
        .iter()
        .find(|message| message.message_id == checkin.message_id)
        .unwrap();
    assert_eq!(canceled.status, MessageStatus::Canceled);
    let events = env.read_events();
    assert!(
        events.iter().any(|event| {
            matches!(event.kind(), EventKind::Message { method, payload }
                if method == MessageEventMethod::Canceled
                    && payload.message_id == checkin.message_id
                    && payload.reason.as_deref() == Some("superseded by the wait's final outcome"))
        }),
        "{events:?}"
    );
    let wake = messages
        .iter()
        .find(|message| Some(&message.message_id) == records[1].message_id.as_ref())
        .unwrap();
    assert_eq!(wake.status, MessageStatus::Sent);
    assert!(wake.text.contains("exit 0 after"), "{}", wake.text);
    assert!(!wake.text.contains("still running after"), "{}", wake.text);
    let trace_path = env.project_root.join("wait-trace.log");
    let trace = std::fs::read_to_string(&trace_path).unwrap();
    let writes: Vec<_> = trace
        .lines()
        .filter(|line| line.contains("\taction\twrite\t"))
        .collect();
    // The wake's paste, then Claude's send-now keys, Enter, Ctrl+X and Ctrl+S.
    assert_eq!(writes.len(), 4, "{trace}");
    assert!(
        writes[0].contains("\taction\twrite\t--pane-id\tterminal_3\t27\t91\t50\t48\t48\t126\t")
            && writes[0].ends_with("\t27\t91\t50\t48\t49\t126"),
        "{trace}"
    );
    for (write, key) in writes[1..].iter().zip(["13", "24", "19"]) {
        assert!(
            write.ends_with(&format!("\taction\twrite\t--pane-id\tterminal_3\t{key}")),
            "{trace}"
        );
    }
    let verdict = wake
        .text
        .lines()
        .last()
        .unwrap()
        .bytes()
        .map(|byte| byte.to_string())
        .collect::<Vec<_>>()
        .join("\t");
    assert!(writes[0].contains(&verdict), "{trace}");

    let output = traced_wait(&env, &pane_fixture)
        .args(["message", "sweep"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let trace = std::fs::read_to_string(trace_path).unwrap();
    assert_eq!(
        trace
            .lines()
            .filter(|line| line.contains("\taction\twrite\t"))
            .count(),
        writes.len(),
        "the sweep writes nothing more: {trace}"
    );
}

/// A watch's final wake cancels the check-in it left parked behind an open prompt.
#[cfg(target_os = "linux")]
#[test]
fn a_watch_checkin_is_canceled_when_the_final_wake_fires() {
    let env = Env::new();
    let pane_fixture = register_waiting_caller(&env);
    let run = |args: &[&str]| {
        let output = traced_wait(&env, &pane_fixture)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    publish_claude_5h(&env, 90);
    let receipt: serde_json::Value = serde_json::from_str(&run(&[
        "watch",
        "--json",
        "--check-in",
        "1s",
        "window.5h.left >= 40",
    ]))
    .unwrap();
    let name = receipt["name"].as_str().unwrap().to_owned();
    run(&["loop", "tick"]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    run(&["loop", "tick"]);
    let checkin = wait_for_wait_messages(&env, 1).pop().unwrap();
    assert_eq!(checkin.status, MessageStatus::Queued, "{checkin:?}");
    wait_for_wait_records(&env, 1);
    wait_for_loop_runs_to_exit(&env, &name);
    publish_claude_5h(&env, 10);
    run(&["loop", "tick"]);
    let records = wait_for_wait_records(&env, 2);
    assert_eq!(records[1].result.label(), "delivered", "{records:?}");
    wait_for_loop_runs_to_exit(&env, &name);
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    let mut statuses: Vec<_> = messages
        .iter()
        .map(|message| (message.message_id == checkin.message_id, message.status))
        .collect();
    statuses.sort_by_key(|(checkin, _)| !checkin);
    assert_eq!(
        statuses,
        [
            (true, MessageStatus::Canceled),
            (false, MessageStatus::Queued)
        ],
        "{messages:?}"
    );
}

/// A shell whose outcome a join claimed cancels the check-in it left parked behind an open
/// prompt, though it sends no wake of its own.
#[cfg(target_os = "linux")]
#[test]
fn a_joined_shells_parked_checkin_is_canceled() {
    let env = Env::new();
    let pane_fixture = register_waiting_caller(&env);
    let release = env.home_root.join("release");
    let command = format!(
        "while [ ! -e {} ]; do sleep 0.05; done",
        shlex::try_quote(release.to_str().unwrap()).unwrap()
    );
    let output = traced_wait(&env, &pane_fixture)
        .args(["shell", "--json", "--check-in", "1s", "--", &command])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let checkin = wait_for_wait_messages(&env, 1).pop().unwrap();
    assert_eq!(checkin.status, MessageStatus::Queued, "{checkin:?}");
    wait_for_wait_records(&env, 1);
    wait_for_loop_runs_to_exit(&env, name);
    let path = loop_instances_path(&env);
    let mut rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    rows[name]["wait-meta"]["joined_until"] =
        serde_json::json!(jiff::Timestamp::now() + jiff::SignedDuration::from_secs(600));
    std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();
    std::fs::write(&release, "").unwrap();
    let records = wait_for_wait_records(&env, 2);
    assert_eq!(records[1].result.label(), "joined", "{records:?}");
    wait_for_no_wait_instances(&env);
    wait_for_loop_runs_to_exit(&env, name);
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    let statuses: Vec<_> = messages
        .iter()
        .map(|message| (message.message_id == checkin.message_id, message.status))
        .collect();
    assert_eq!(statuses, [(true, MessageStatus::Canceled)], "{messages:?}");
}

/// A pid, check, or file wait an older RimZ stored is retired with a record `loop logs` shows
/// by the row's name, whether the row had fired or never did.
#[cfg(target_os = "linux")]
#[test]
fn retired_legacy_waits_show_in_loop_logs_with_their_reason() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let fired = arm_false_watch(&env);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    wait_ok(&env, &["loop", "tick"]);
    wait_for_wait_records(&env, 1);
    wait_for_loop_runs_to_exit(&env, &fired);
    let path = loop_instances_path(&env);
    let mut rows: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut legacy = rows[&fired].clone();
    legacy.as_object_mut().unwrap().remove("when");
    legacy["watch"] = serde_json::json!({"pid": 4242});
    rows[&fired] = legacy.clone();
    legacy["watch"] = serde_json::json!({"file": "/repo/app.log", "grep": "ready"});
    rows["wait-never-fired"] = legacy;
    std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();
    wait_ok(&env, &["events", "emit", "deploy.done"]);
    assert!(
        wait_instances(&env).0.is_empty(),
        "{:?}",
        wait_instances(&env)
    );
    for name in [fired.as_str(), "wait-never-fired"] {
        let logs = wait_ok(&env, &["loop", "logs", name]);
        assert!(
            logs.contains("canceled")
                && logs.contains("RimZ no longer watches a pid, a check, or a file"),
            "{name}: {logs}"
        );
    }
}

fn register_waiting_caller(env: &Env) -> std::path::PathBuf {
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(env);
    let pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_3");
    for signal in [
        LifecycleSignal::TurnStarted { turn_id: None },
        LifecycleSignal::AwaitingInput {
            extra_questions: 0,
            kind: AskKind::Permission,
            ask_id: None,
            detail: None,
            native_key: None,
        },
    ] {
        let mut observation =
            AgentLifecycleObservation::new(Some(AgentSessionId::from("provider-session")), signal);
        observation.pane_id = Some(pane_id.clone());
        env.store()
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }
    env.write_pane_fixture(&[rimz::pane::PaneRef {
        pane_id,
        session_name: "rimz-test".to_owned(),
        view_id: Some("tab_1".to_owned()),
        view_kind: Some(rimz::ids::ViewKind::Tab),
        view_name: Some("project".to_owned()),
        title: None,
        is_floating: false,
        command: Some("claude".to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(env.project_root.display().to_string()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }])
}

fn traced_wait(env: &Env, pane_fixture: &std::path::Path) -> std::process::Command {
    let mut command = agent_wait(env);
    command
        .env("RIMZ_TEST_PANE_LIST", pane_fixture)
        .env("RIMZ_ZELLIJ_BIN", zellij_trace_shim())
        .env(
            "RIMZ_TEST_ZELLIJ_LOG",
            env.project_root.join("wait-trace.log"),
        );
    command
}

fn assert_no_wait_paste(env: &Env) {
    let trace =
        std::fs::read_to_string(env.project_root.join("wait-trace.log")).unwrap_or_default();
    assert!(!trace.contains("\taction\twrite\t"), "{trace}");
}

#[test]
fn watch_checkin_delivers_once_without_consuming_or_killing_command() {
    for (on, exit, delivers_exit) in [
        ("any", "0", true),
        ("success", "0", true),
        ("fail", "3", true),
        ("success", "3", false),
        ("fail", "0", false),
    ] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        let release = env.home_root.join("release");
        let pid_path = env.home_root.join("command.pid");
        let receipt = wait_ok(
            &env,
            &[
                "shell",
                "--json",
                "--on",
                on,
                "--check-in",
                "1s",
                "--",
                &format!(
                    "printf '%s' \"$$\" > {}; printf checkin-marker; while [ ! -e {} ]; do sleep 0.05; done; printf final-marker; exit {exit}",
                    shlex::try_quote(pid_path.to_str().unwrap()).unwrap(),
                    shlex::try_quote(release.to_str().unwrap()).unwrap()
                ),
            ],
        );
        let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
        let name = receipt["name"].as_str().unwrap();
        let messages = wait_for_wait_messages(&env, 1);
        let notice = &messages[0];
        let listed: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["shell", "list", "--json"])).unwrap();
        assert_eq!(listed[0]["state"], "watching");
        assert!(
            listed[0]["elapsed_s"].as_u64().unwrap() >= listed[0]["check_in_s"].as_u64().unwrap()
        );
        assert!(
            notice.text.contains("still running after"),
            "--on {on}: {}",
            notice.text
        );
        // The check-in clock starts at the shell's spawn, so it can fire
        // before the command's first print lands.
        assert!(
            notice.text.contains("(<1k tokens, 1 line) [") || notice.text.contains("no output"),
            "{}",
            notice.text
        );
        assert!(!notice.text.lines().any(|line| line == "checkin-marker"));
        assert!(
            notice
                .text
                .contains(&format!("Stop it: rimz shell cancel {name}")),
            "{}",
            notice.text
        );
        assert!(!notice.text.contains("Another check-in"), "{}", notice.text);
        assert!(!notice.text.contains("rimz wait"), "{}", notice.text);
        assert_eq!(notice.agent_id.as_str(), "provider-session");
        assert_eq!(
            notice.sender,
            MessageSender::Harness {
                notice: HarnessNotice::Wait
            }
        );
        assert_eq!(notice.gate, DeliveryGate::Any);
        let report: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
                .unwrap();
        assert_eq!(report["agent"]["status"], "running");
        assert_eq!(
            report["agent"]["pending_waits"][0]["trigger"]["kind"],
            "command"
        );
        let mut pid = None;
        wait_until(&env, "command did not write its pid", || {
            pid = std::fs::read_to_string(&pid_path)
                .ok()
                .and_then(|text| text.parse::<u32>().ok());
            pid.is_some()
        });
        let pid = pid.unwrap();
        let observe_until = std::time::Instant::now() + std::time::Duration::from_millis(1200);
        while std::time::Instant::now() < observe_until {
            assert!(
                rimz::proc::process_is_live(pid, None),
                "check-in killed the command"
            );
            assert!(
                wait_instances(&env).0.contains_key(name),
                "check-in consumed the instance"
            );
            assert_eq!(
                env.store().list_pending_messages().unwrap().len(),
                1,
                "duplicate check-in"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let records = wait_for_wait_records(&env, 1);
        assert_eq!(records.len(), 1);
        let check = records[0].check.as_ref().unwrap();
        assert!(!check.timed_out);
        assert_eq!(check.code, None);
        let output_path = check.output_path.as_ref().unwrap().display().to_string();
        assert_eq!(
            notice.text.contains(&output_path),
            notice.text.contains("(<1k tokens, 1 line) ["),
            "{}",
            notice.text
        );

        std::fs::write(&release, "").unwrap();
        let records = wait_for_wait_records(&env, 2);
        assert_eq!(records.len(), 2);
        assert!(matches!(
            records[1].watch.as_ref().unwrap(),
            rimz::harness::signal::WatchVerdict::Exited { code: Some(code), .. }
                if code.to_string() == exit
        ));
        wait_for_no_wait_instances(&env);
        let report: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
                .unwrap();
        assert_eq!(report["agent"]["status"], "idle");
        assert_eq!(report["agent"]["pending_waits"], serde_json::json!([]));
        let (mut messages, history) = env.store().list_messages_and_history().unwrap();
        messages.extend(history);
        assert_eq!(messages.len(), if delivers_exit { 2 } else { 1 });
        assert_eq!(
            messages
                .iter()
                .filter(|message| message.text.contains("still running after"))
                .count(),
            1
        );
        if delivers_exit {
            assert_eq!(
                messages
                    .iter()
                    .find(|message| message.message_id == notice.message_id)
                    .unwrap()
                    .status,
                MessageStatus::Canceled
            );
            let final_message = messages
                .iter()
                .find(|message| message.message_id != notice.message_id)
                .unwrap();
            assert!(
                final_message.text.contains(&format!("exit {exit} after")),
                "{}",
                final_message.text
            );
            assert!(
                final_message.text.contains("(<1k tokens, 1 line) ["),
                "{}",
                final_message.text
            );
            assert!(
                !final_message
                    .text
                    .lines()
                    .any(|line| line == "checkin-markerfinal-marker")
            );
            assert_eq!(
                records[1].message_id.as_ref(),
                Some(&final_message.message_id)
            );
        } else {
            assert_eq!(records[1].result.label(), "skipped");
            assert!(records[1].message_id.is_none());
        }
    }
}

/// A shell's exit is sent as a message unless a join's claim on its row is still ahead: then
/// the row is consumed and its history records the outcome as joined, and nothing is sent. A
/// claim past its deadline holds nothing back.
#[test]
fn a_live_join_claim_takes_a_shells_outcome_and_an_expired_one_lets_it_through() {
    for (ahead, result, sent) in [(true, "joined", 0), (false, "delivered", 1)] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        let release = env.home_root.join("release");
        let receipt: serde_json::Value = serde_json::from_str(&wait_ok(
            &env,
            &[
                "shell",
                "--json",
                "--",
                &format!(
                    "while [ ! -e {} ]; do sleep 0.05; done; exit 0",
                    shlex::try_quote(release.to_str().unwrap()).unwrap()
                ),
            ],
        ))
        .unwrap();
        let name = receipt["name"].as_str().unwrap();
        let path = crate::common::wait::loop_instances_path(&env);
        let mut rows: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let until = jiff::Timestamp::now()
            + jiff::SignedDuration::from_secs(if ahead { 600 } else { -600 });
        rows[name]["wait-meta"]["joined_until"] = serde_json::json!(until);
        std::fs::write(&path, serde_json::to_vec(&rows).unwrap()).unwrap();
        std::fs::write(&release, "").unwrap();
        let records = wait_for_wait_records(&env, 1);
        assert_eq!(records[0].result.label(), result, "{records:?}");
        assert!(
            matches!(
                records[0].watch,
                Some(rimz::harness::signal::WatchVerdict::Exited { code: Some(0), .. })
            ),
            "{records:?}"
        );
        wait_for_no_wait_instances(&env);
        if sent > 0 {
            wait_for_wait_messages(&env, sent);
        }
        let (mut messages, history) = env.store().list_messages_and_history().unwrap();
        messages.extend(history);
        assert_eq!(messages.len(), sent, "claim ahead {ahead}: {messages:?}");
    }
}

/// A signal fired from another channel while the subscriber's swap replacement is still
/// starting wakes that replacement under its own channel's handle, not a namesake in the
/// firing channel.
#[test]
fn a_signal_from_another_channel_wakes_a_starting_holder_and_not_a_namesake() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    crate::common::wait::register_calling_agent_with_launch(
        &env,
        LaunchParams {
            channel: Some("beta".into()),
            ..LaunchParams::default()
        },
    );
    // The row a signal wait armed from `#beta` stores: the agent's launch and its
    // channel's handle.
    seed_signal_wait(&env, "deploy", "deploy.done", false);
    let mut tasks = wait_instances(&env);
    let target = tasks.0.get_mut("deploy").unwrap().wait.as_mut().unwrap();
    target.session = AgentSessionId::from("launch-session");
    target.handle = "@planner#beta".to_owned();
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    let source = store
        .audit_rows()
        .unwrap()
        .iter()
        .find(|row| row.agent_id == "provider-session")
        .cloned()
        .unwrap();
    let reservation = store
        .begin_agent_swap(
            &source,
            &rimz::store::writer::AgentLaunchRequest {
                kind: AgentKind::new_unchecked("claude"),
                agent_id: "launch_swap".into(),
                name: rimz::store::writer::AgentLaunchName::Explicit("planner".into()),
                login: rimz::store::writer::LaunchLogin::RoomDefault,
                launch: LaunchParams {
                    lane: source.lane.clone(),
                    ..LaunchParams::default()
                },
                run_id: None,
                prompt: None,
            },
            rimz::store::writer::AgentLaunchScope {
                session_name: workspace.session_name.clone(),
                cwd: env.project_root.clone(),
                branch: None,
                description: None,
            },
        )
        .unwrap()
        .single_identity()
        .unwrap()
        .clone();
    store
        .bind_agent_launch(
            &reservation,
            &workspace.session_name,
            &env.project_root,
            &PaneId::from_parts(MuxName::Tmux, "%9"),
            None,
        )
        .unwrap();
    crate::common::wait::register_agent(
        &env,
        "sess-alpha",
        "launch-alpha",
        "planner",
        LaunchParams {
            channel: Some("alpha".into()),
            ..LaunchParams::default()
        },
    );
    crate::common::room::seed_sidebar_heartbeat(
        &env.runtime_paths(),
        MuxName::Zellij,
        &workspace.session_name,
        "deploy",
    );
    let emitted = env
        .rimz()
        .args(["events", "emit", "deploy.done"])
        .env(rimz::workspace::ENV_CHANNEL, "alpha")
        .output()
        .unwrap();
    assert!(
        emitted.status.success(),
        "{}",
        String::from_utf8_lossy(&emitted.stderr)
    );
    let messages = wait_for_wait_messages(&env, 1);
    let receivers = messages
        .iter()
        .map(|message| (message.agent_id.as_str(), message.launch_id.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(
        receivers,
        [(reservation.agent_id.as_str(), Some("launch-session"))],
        "{messages:#?}"
    );
}

#[test]
fn once_wait_subscriber_is_consumed_by_watcher_checkin() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    seed_signal_wait(&env, "audit", "shell.*", false);
    let release = env.home_root.join("release");
    let receipt = wait_ok(
        &env,
        &[
            "shell",
            "--json",
            "--check-in",
            "1s",
            "--",
            &format!(
                "printf checkin-marker; while [ ! -e {} ]; do sleep 0.05; done; printf final-marker",
                shlex::try_quote(release.to_str().unwrap()).unwrap()
            ),
        ],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let records = wait_for_wait_records(&env, 2);
    assert_eq!(records.len(), 2);
    let audit = records
        .iter()
        .find(|record| record.task == "audit")
        .unwrap();
    assert!(matches!(
        audit.watch.as_ref().unwrap(),
        rimz::harness::signal::WatchVerdict::Running { .. }
    ));
    let messages = wait_for_wait_messages(&env, 2);
    let notice = messages
        .iter()
        .find(|message| Some(&message.message_id) == audit.message_id.as_ref())
        .expect("subscriber check-in reached the durable message consumer");
    assert_eq!(
        notice.sender,
        MessageSender::Harness {
            notice: HarnessNotice::Signal
        }
    );
    let tasks = wait_instances(&env);
    assert!(
        tasks.0.contains_key(name),
        "check-in consumed the watched task"
    );
    assert!(
        !tasks.0.contains_key("audit"),
        "check-in did not consume the once subscriber"
    );

    std::fs::write(&release, "").unwrap();
    wait_for_no_wait_instances(&env);
    wait_until(&env, "watcher did not finish its exit delivery", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_none()
    });
    let records = wait_for_wait_records(&env, 3);
    assert_eq!(records.len(), 3);
    assert_eq!(
        records
            .iter()
            .filter(|record| record.task == "audit")
            .count(),
        1
    );
    assert!(records.iter().any(|record| {
        record.task == name
            && matches!(
                record.watch.as_ref(),
                Some(rimz::harness::signal::WatchVerdict::Exited { code: Some(0), .. })
            )
    }));
    assert_eq!(wait_for_wait_messages(&env, 2).len(), 2);
    let checkin_id = records
        .iter()
        .find(|record| {
            record.task == name
                && matches!(
                    record.watch.as_ref(),
                    Some(rimz::harness::signal::WatchVerdict::Running { .. })
                )
        })
        .unwrap()
        .message_id
        .as_ref()
        .unwrap();
    assert!(
        env.store()
            .list_message_history()
            .unwrap()
            .iter()
            .any(|message| &message.message_id == checkin_id
                && message.status == MessageStatus::Canceled)
    );
}

/// A watcher fires in its own process, and that fire reconciles before it
/// selects anything, so a watcher whose subscriber died retires its own row
/// mid-fire. It has to survive doing so: `watcher_info` opens its own
/// descriptor and reads the holder's pid back out of the lock, so an
/// unguarded `stop_watcher` would `killpg` the firing watcher's own group and
/// take the fire — and every live sibling's delivery — with it.
#[test]
fn watcher_survives_retiring_its_own_row_mid_fire() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    register_agent(
        &env,
        "sibling-session",
        "sibling-launch",
        "scout",
        LaunchParams::default(),
    );
    seed_signal_wait(&env, "audit", "shell.*", true);
    let mut tasks = wait_instances(&env);
    tasks
        .0
        .get_mut("audit")
        .expect("sibling subscriber")
        .wait
        .as_mut()
        .expect("pinned target")
        .session = AgentSessionId::from("sibling-session");
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();

    let release = env.home_root.join("release");
    let receipt = wait_ok(
        &env,
        &[
            "shell",
            "--json",
            "--",
            &format!(
                "while [ ! -e {} ]; do sleep 0.05; done",
                shlex::try_quote(release.to_str().unwrap()).unwrap()
            ),
        ],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap().to_owned();
    wait_until(&env, "watcher did not start", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), &name)
            .unwrap()
            .is_some()
    });
    // The watcher's own subscriber dies while it watches, stamped by a
    // producer that reaches no hook.
    stamp_session_ended(&env, "provider-session");
    std::fs::write(&release, "").unwrap();

    let records = wait_for_wait_records(&env, 1);
    assert_eq!(
        records
            .iter()
            .map(|record| record.task.as_str())
            .collect::<Vec<_>>(),
        vec!["audit"],
        "the fire reached its live sibling and not the dead subscriber: {records:?}"
    );
    assert_eq!(wait_for_wait_messages(&env, 1).len(), 1);
    wait_until(
        &env,
        "the dead subscriber's watch row survived its own fire",
        || !wait_instances(&env).0.contains_key(&name),
    );
    assert!(
        wait_instances(&env).0.contains_key("audit"),
        "the live sibling's subscription was retired"
    );
}

fn stamp_session_ended(env: &Env, session_id: &str) {
    let workspace = env.resolve_workspace(&env.project_root);
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(session_id)),
        LifecycleSignal::Ended,
    );
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "rimz.agent-ended",
            &observation,
        ))
        .expect("stamp session ended without a hook");
}

#[test]
fn wait_cancel_before_watcher_start_prevents_command() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let receipt = wait_ok(&env, &["watch", "--json", "pr=merged"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let mut tasks = wait_instances(&env);
    let entry = tasks.0.get_mut(name).unwrap();
    entry.when = None;
    entry.watch = Some(rimz::config::WatchSpec::Command(
        "touch command-started".to_owned(),
    ));
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();

    wait_ok(&env, &["shell", "cancel", name]);
    let output = env
        .rimz()
        .args(["shell", "watcher", name])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no wait named"));
    assert!(!env.project_root.join("command-started").exists());
    assert!(wait_instances(&env).0.is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
}

#[test]
fn wait_cancel_all_stops_command_groups_and_prints_pending() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let mut pids = Vec::new();
    for index in 0..2 {
        let path = env.home_root.join(format!("command-{index}.pids"));
        wait_ok(
            &env,
            &[
                "shell",
                "--",
                &format!(
                    "sleep 30 & printf '%s %s' \"$$\" \"$!\" > {}; wait",
                    shlex::try_quote(path.to_str().unwrap()).unwrap()
                ),
            ],
        );
        wait_until(&env, "command group did not start", || {
            std::fs::read_to_string(&path).is_ok_and(|text| text.split_whitespace().count() == 2)
        });
        let command_pids = std::fs::read_to_string(&path).unwrap();
        for pid in command_pids.split_whitespace() {
            let pid = pid.parse::<u32>().unwrap();
            assert!(rimz::proc::process_is_live(pid, None));
            pids.push(pid);
        }
    }
    let names = wait_instances(&env).0.into_keys().collect::<Vec<_>>();
    let canceled = wait_ok(&env, &["shell", "cancel", "--all"]);
    assert!(canceled.starts_with("canceled "), "{canceled}");
    for name in names {
        assert!(canceled.contains(&name), "{canceled}");
    }
    assert!(
        canceled.ends_with("@planner#main · no shells\n"),
        "{canceled}"
    );
    wait_until(&env, "cancel left watched descendants alive", || {
        pids.iter()
            .all(|pid| !rimz::proc::process_is_live(*pid, None))
    });
    assert!(wait_instances(&env).0.is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
}

#[test]
fn wait_receipts_and_list_share_pending_rows() {
    let stable_rows = |mut rows: serde_json::Value| {
        for row in rows.as_array_mut().unwrap() {
            let row = row.as_object_mut().unwrap();
            row.remove("elapsed");
            row.remove("elapsed_s");
        }
        rows
    };
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let first = wait_ok(&env, &["watch", "--json", "pr=merged"]);
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    let second = wait_ok(&env, &["watch", "--json", "ci=passed"]);
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    let listed = wait_ok(&env, &["watch", "list", "--json"]);
    let listed: serde_json::Value = serde_json::from_str(&listed).unwrap();
    assert_eq!(first["pending"].as_array().unwrap().len(), 1);
    assert_eq!(second["pending"].as_array().unwrap().len(), 2);
    assert_eq!(stable_rows(second["pending"].clone()), stable_rows(listed));
    wait_ok(&env, &["loop", "disable", first["name"].as_str().unwrap()]);
    wait_ok(
        &env,
        &[
            "loop",
            "pause",
            second["name"].as_str().unwrap(),
            "--for",
            "1h",
        ],
    );
    let held = wait_ok(&env, &["watch", "list", "--json"]);
    let held: serde_json::Value = serde_json::from_str(&held).unwrap();
    let held = held.as_array().unwrap();
    assert_eq!(
        held.iter()
            .find(|row| row["name"] == first["name"])
            .unwrap()["state"],
        "disabled"
    );
    let paused = held
        .iter()
        .find(|row| row["name"] == second["name"])
        .unwrap();
    assert_eq!(paused["state"], "paused");
    assert!(
        paused["trigger"]
            .as_str()
            .unwrap()
            .contains(" · resumes in ")
    );
    let canceled = wait_ok(
        &env,
        &["watch", "cancel", first["name"].as_str().unwrap(), "--json"],
    );
    let canceled: serde_json::Value = serde_json::from_str(&canceled).unwrap();
    assert_eq!(canceled["canceled"], serde_json::json!([first["name"]]));
    assert_eq!(canceled["pending"].as_array().unwrap().len(), 1);
    assert_eq!(canceled["pending"][0]["name"], second["name"]);
    let listed = wait_ok(&env, &["watch", "list", "--json"]);
    assert_eq!(
        stable_rows(canceled["pending"].clone()),
        stable_rows(serde_json::from_str::<serde_json::Value>(&listed).unwrap())
    );
    let human = wait_ok(&env, &["watch", "pr=closed"]);
    assert!(human.starts_with("armed watch-"), "{human}");
    assert!(human.contains("@planner#main · 2 watches\n"), "{human}");
    assert!(human.contains(second["name"].as_str().unwrap()), "{human}");
}

/// Ends one turn of the calling agent, so only a pending wait can keep it sleeping.
fn rest_calling_agent(env: &Env) {
    for signal in [
        LifecycleSignal::TurnStarted { turn_id: None },
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    ] {
        let observation =
            AgentLifecycleObservation::new(Some(AgentSessionId::from("provider-session")), signal);
        env.store()
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: "rimz-test",
                agent_kind: AgentKind::new_unchecked("claude"),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .expect("record a finished turn");
    }
}

fn wait_refusal(env: &Env, args: &[&str]) -> String {
    let output = agent_wait(env).args(args).output().unwrap();
    assert!(!output.status.success(), "accepted {args:?}");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn watch_arms_a_one_shot_self_wait_in_the_arming_checkout() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(linked) = linked_worktree(&env) else {
        return;
    };
    register_calling_agent(&env);
    rest_calling_agent(&env);
    publish_claude_5h(&env, 30);
    let mut names = Vec::new();
    let (args, kind, label) = (&["pr=merged && ci=passed"][..], "condition", "state");
    {
        let output = agent_wait(&env)
            .current_dir(&linked)
            .arg("watch")
            .args(["--label", label, "--json"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let name = receipt["name"].as_str().unwrap().to_owned();
        assert!(name.starts_with("watch-"), "{receipt}");
        assert!(receipt.get("existing").is_none(), "{receipt}");
        assert_eq!(receipt["target"], "@planner#main");
        let row = receipt["pending"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name.as_str())
            .unwrap()
            .clone();
        assert_eq!(row["type"], kind, "{row}");
        assert_eq!(row["state"], "waiting", "{row}");
        assert_eq!(row["owner"], "self", "{row}");
        assert_eq!(row["label"], label, "{row}");
        assert_eq!(row["dir"], "~/linked", "{row}");
        assert_eq!(receipt["trigger"], row["trigger"], "{receipt}");
        let tasks = wait_instances(&env);
        let entry = &tasks.0[&name];
        assert_eq!(
            entry.wait.as_ref().unwrap().session.as_str(),
            "launch-session"
        );
        assert!(
            entry
                .wait_meta
                .as_ref()
                .is_some_and(|meta| meta.delay.is_none()),
            "{entry:?}"
        );
        assert_eq!(entry.root, canonical(&env.project_root));
        assert_eq!(entry.dir.as_deref(), Some(linked.as_path()));
        assert!(entry.every.is_none() && entry.cron.is_none());
        assert!(entry.signal.is_none() && entry.hold.is_none() && entry.prompt.is_none());
        assert!(entry.fire_at.is_none(), "{entry:?}");
        assert!(!entry.repeat, "{entry:?}");
        assert_eq!(
            entry.when.as_deref(),
            Some(&["pr=merged && ci=passed".to_owned()][..])
        );
        assert_eq!(row["trigger"], "when pr=merged && ci=passed");
        names.push(name);
    }
    let report: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"])).unwrap();
    assert_eq!(report["agent"]["status"], "sleeping", "{report}");
    let pending = report["agent"]["pending_waits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|wait| wait["name"].as_str().unwrap().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(pending, names.iter().cloned().collect());
    let listed = wait_ok(&env, &["watch", "list"]);
    for name in &names {
        assert!(listed.contains(name.as_str()), "{listed}");
    }
    wait_ok(&env, &["watch", "cancel", "--all"]);
    assert!(wait_instances(&env).0.is_empty());
}

#[test]
fn watch_receipts_name_the_window_and_the_condition_reading() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let first_lines = |args: &[&str]| -> Vec<String> {
        let mut argv = vec!["watch"];
        argv.extend_from_slice(args);
        wait_ok(&env, &argv)
            .lines()
            .take(2)
            .map(ToOwned::to_owned)
            .collect()
    };
    let forge = first_lines(&["pr=merged"]);
    assert!(
        forge[0].starts_with("armed watch-")
            && forge[0].ends_with(": when pr=merged → @planner#main"),
        "{forge:?}"
    );
    assert_eq!(
        forge[1],
        "  now: unknown · pr: readings paused (room closed)"
    );
    let resets_at = publish_claude_5h(&env, 92);
    let window = first_lines(&["window.5h.left >= 40"]);
    assert!(
        window[0].ends_with(": when window.5h.left>=40 → @planner#main"),
        "{window:?}"
    );
    assert_eq!(
        window[1],
        format!(
            "  now: waiting · window.5h.left: 8 (resets {})",
            reset_minute(resets_at)
        )
    );
    publish_claude_5h(&env, 10);
    let met = first_lines(&["window.5h.left >= 40 && pr=merged"]);
    assert_eq!(
        met[1],
        "  now: unknown · window.5h.left: 90, pr: readings paused (room closed)"
    );
}

#[test]
fn watch_twice_names_the_existing_watch_while_another_expression_arms_again() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let arm = |args: &[&str]| -> serde_json::Value {
        let mut argv = vec!["watch", "--json"];
        argv.extend_from_slice(args);
        serde_json::from_str(&wait_ok(&env, &argv)).unwrap()
    };
    let first = arm(&["pr=merged"]);
    let again = arm(&["pr=merged"]);
    assert_eq!(again["name"], first["name"], "{again}");
    assert_eq!(again["existing"], true, "{again}");
    assert_eq!(again["trigger"], "when pr=merged");
    assert_eq!(again["pending"].as_array().unwrap().len(), 1, "{again}");
    let human = wait_ok(&env, &["watch", "pr=merged"]);
    assert_eq!(
        human.lines().next().unwrap(),
        format!(
            "already waiting {}: when pr=merged → @planner#main",
            first["name"].as_str().unwrap()
        )
    );
    assert_eq!(wait_instances(&env).0.len(), 1);
    let other = arm(&["ci=passed"]);
    assert_ne!(other["name"], first["name"]);
    assert!(other.get("existing").is_none(), "{other}");
    assert_eq!(wait_instances(&env).0.len(), 2);
}

/// Arms a labeled watch on a window through `command`, publishing the window it reads, then
/// fires it the way the scheduler would.
fn arm_and_fire(env: &Env, command: impl Fn() -> std::process::Command) -> String {
    publish_claude_5h(env, 10);
    let output = command()
        .args(["watch", "--label", "wake when", "--json"])
        .arg("window.5h.left >= 40")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let name = receipt["name"].as_str().unwrap().to_owned();
    for index in 0..2 {
        let output = command().args(["loop", "tick"]).output().unwrap();
        assert!(
            output.status.success(),
            "tick {index}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if index == 0 {
            // First sight arms the condition; only a later tick fires it. The
            // tick writes its plan before it spawns the detached runner, so
            // the plan, not the runner's message or row, shows no fire.
            let runtime = env.runtime_paths();
            let stamps = rimz::harness::schedule::last_stamps(&runtime);
            assert!(stamps.contains_key(&name), "{stamps:?}");
            let conditions: serde_json::Value = std::fs::read(runtime.lane_path("loop-when.json"))
                .map_or(serde_json::Value::Null, |bytes| {
                    serde_json::from_slice(&bytes).unwrap()
                });
            assert_ne!(conditions[&name]["fired"], true, "{conditions}");
        }
    }
    name
}

#[test]
fn a_watch_steers_a_wake_carrying_its_label_and_readings() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    rest_calling_agent(&env);
    let name = arm_and_fire(&env, || agent_wait(&env));
    let messages = wait_for_wait_messages(&env, 1);
    assert_eq!(messages.len(), 1);
    let wake = &messages[0];
    assert_eq!(wake.agent_id.as_str(), "provider-session");
    assert_eq!(wake.gate, DeliveryGate::Any);
    assert_eq!(
        wake.sender,
        MessageSender::Harness {
            notice: HarnessNotice::Wait
        }
    );
    let lines = wake.text.lines().collect::<Vec<_>>();
    assert_eq!(
        lines[0], "waited on window.5h.left>=40 · wake when",
        "{}",
        wake.text
    );
    assert!(
        lines[1].starts_with("fired after ") && lines[1].ends_with(&format!(" [{name}]")),
        "{}",
        wake.text
    );
    assert!(lines.contains(&"window.5h.left: 90"), "{}", wake.text);
    wait_for_no_wait_instances(&env);
}

/// Arms a watch on a window that reads false, with a one-second check-in, and gives the tick
/// its first sight of it.
fn arm_false_watch(env: &Env) -> String {
    publish_claude_5h(env, 90);
    let receipt: serde_json::Value = serde_json::from_str(&wait_ok(
        env,
        &[
            "watch",
            "--json",
            "--check-in",
            "1s",
            "window.5h.left >= 40",
        ],
    ))
    .unwrap();
    wait_ok(env, &["loop", "tick"]);
    receipt["name"].as_str().unwrap().to_owned()
}

#[test]
fn a_watch_checks_in_once_at_its_due_time_and_keeps_waiting() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let name = arm_false_watch(&env);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    wait_ok(&env, &["loop", "tick"]);
    let messages = wait_for_wait_messages(&env, 1);
    let notice = &messages[0];
    let lines = notice.text.lines().collect::<Vec<_>>();
    assert_eq!(lines[0], "waited on window.5h.left>=40", "{}", notice.text);
    assert!(
        lines[1].starts_with("still not met after ")
            && lines[1].ends_with(&format!(" · still watching [{name}]")),
        "{}",
        notice.text
    );
    assert_eq!(
        lines[2..],
        ["", &format!("Stop it: rimz watch cancel {name}")],
        "{}",
        notice.text
    );
    assert_eq!(notice.gate, DeliveryGate::Any);
    let records = wait_for_wait_records(&env, 1);
    assert_eq!(records[0].result.label(), "delivered", "{records:?}");
    for _ in 0..2 {
        wait_ok(&env, &["loop", "tick"]);
    }
    assert!(wait_instances(&env).0.contains_key(&name));
    let listed: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["watch", "list", "--json"])).unwrap();
    assert_eq!(listed[0]["name"], name.as_str(), "{listed}");
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(wait_records(&env).len(), 1);
}

#[test]
fn a_watch_cancelled_before_its_check_in_sends_none() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let name = arm_false_watch(&env);
    let shown = wait_ok(&env, &["loop", "show", &name]);
    let labels: Vec<_> = shown
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(|label| label.trim_end_matches(':'))
        .filter(|label| ["check-in", "timeout"].contains(label))
        .collect();
    assert_eq!(labels, ["check-in"], "{shown}");
    wait_ok(&env, &["watch", "cancel", &name]);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    wait_ok(&env, &["loop", "tick"]);
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    assert!(
        messages.is_empty(),
        "a cancelled watch checked in: {messages:?}"
    );
    assert!(
        wait_records(&env)
            .iter()
            .all(|record| record.result.label() != "delivered"),
        "{:?}",
        wait_records(&env)
    );
}

/// The external tick reads forge keys as unread, so only the sidebar host fires a `pr=` wait:
/// it spawns `loop run <name> --condition-json` with its readings, as this test does.
#[test]
fn forge_wait_fires_with_its_label_and_pr_reading() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    rest_calling_agent(&env);
    let runtime = env.runtime_paths();
    crate::common::room::seed_sidebar_heartbeat(
        &runtime,
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "forge",
    );
    let root = env.project_root.to_string_lossy().to_string();
    std::fs::create_dir_all(runtime.lane_path("pr-state.json").parent().unwrap()).unwrap();
    std::fs::write(
        runtime.lane_path("pr-state.json"),
        serde_json::json!({
            "branch_ci": {},
            "path_repos": {root.clone(): "repo"},
            "repos": {"repo": {
                "ok": true,
                "refreshed_at_ms": jiff::Timestamp::now().as_millisecond() + 3_600_000,
            }},
            "states": {root: {"state": "merged"}},
        })
        .to_string(),
    )
    .unwrap();
    let receipt = wait_ok(&env, &["watch", "--label", "landed", "pr=merged"]);
    let name = receipt
        .split_whitespace()
        .nth(1)
        .unwrap()
        .trim_end_matches(':')
        .to_owned();
    assert_eq!(receipt.lines().nth(1), Some("  now: due"), "{receipt}");
    let fire = agent_wait(&env)
        .args(["loop", "run", &name, "--condition-json"])
        .arg(
            serde_json::json!({
                "when": "pr=merged", "hold": null, "held_ms": 0,
                "readings": {"pr": "merged"}
            })
            .to_string(),
        )
        .output()
        .unwrap();
    assert!(
        fire.status.success(),
        "{}",
        String::from_utf8_lossy(&fire.stderr)
    );
    let messages = wait_for_wait_messages(&env, 1);
    assert_eq!(messages.len(), 1, "{messages:?}");
    let wake = &messages[0];
    assert_eq!(
        wake.sender,
        MessageSender::Harness {
            notice: HarnessNotice::Wait
        }
    );
    assert_eq!(wake.gate, DeliveryGate::Any);
    let lines = wake.text.lines().collect::<Vec<_>>();
    assert_eq!(lines[0], "waited on pr=merged · landed", "{}", wake.text);
    assert!(
        lines[1].starts_with("fired after ") && lines[1].ends_with(&format!(" [{name}]")),
        "{}",
        wake.text
    );
    assert!(lines.contains(&"pr: merged"), "{}", wake.text);
    wait_for_no_wait_instances(&env);
}

/// A forge wait refuses at arming only when the room's reading already says its checkout has
/// no supported forge, and once armed it says why its key is unread wherever it is shown.
#[test]
fn forge_waits_refuse_without_a_supported_forge_and_say_why_once_armed() {
    const NO_FORGE: &str = "no supported forge for this checkout";
    const STALE: &str = "probed before the last fire or first sight";
    const NO_FORGE_FIX: &str = "RimZ reads a git checkout whose `origin` remote is on github.com (through `gh`) or on a host whose name contains gitea, forgejo or codeberg (through `tea`), so wait on something else here";
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let runtime = env.runtime_paths();
    let root = env.project_root.to_string_lossy().to_string();
    let cache = runtime.lane_path("pr-state.json");
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    let publish = |repo: &str, detached: bool, refreshed_at_ms: u64| {
        std::fs::write(
            &cache,
            serde_json::json!({
                "branch_ci": {},
                "path_repos": {root.clone(): repo},
                "repos": {
                    "repo": {"ok": true, "refreshed_at_ms": refreshed_at_ms},
                    "<unsupported>": {"ok": true, "refreshed_at_ms": refreshed_at_ms},
                },
                "states": {root.clone(): {"state": "open"}},
                "detached": if detached { vec![root.clone()] } else { Vec::new() },
            })
            .to_string(),
        )
        .unwrap();
    };
    let now_line = |receipt: &str| receipt.lines().nth(1).unwrap_or_default().to_owned();
    let listed = || -> Vec<serde_json::Value> {
        serde_json::from_str(&wait_ok(&env, &["watch", "list", "--json"])).unwrap()
    };
    // The clock pass's first sight of each wait, which a condition reads from.
    let sight = |stamp: &str| {
        let stamps = runtime.lane_path("loop-fire.json");
        std::fs::create_dir_all(stamps.parent().unwrap()).unwrap();
        let names = listed()
            .iter()
            .map(|row| (row["name"].as_str().unwrap().to_owned(), stamp))
            .collect::<std::collections::BTreeMap<_, _>>();
        std::fs::write(stamps, serde_json::to_vec(&names).unwrap()).unwrap();
    };
    let loops = |stamp: &str| -> (String, serde_json::Value) {
        sight(stamp);
        let text = env.rimz().args(["loop", "list", "--all"]).output().unwrap();
        let json = env
            .rimz()
            .args(["loop", "list", "--json"])
            .output()
            .unwrap();
        (
            String::from_utf8(text.stdout).unwrap(),
            serde_json::from_slice(&json.stdout).unwrap(),
        )
    };
    let attention = |json: &serde_json::Value| -> Vec<serde_json::Value> {
        json["rooms"][0]["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|task| task["attention"].clone())
            .collect()
    };

    // With the room closed nothing is known, so it arms and says why in the room-closed words.
    let closed = wait_ok(&env, &["watch", "pr=merged"]);
    assert_eq!(
        now_line(&closed),
        "  now: unknown · pr: readings paused (room closed)"
    );
    wait_ok(&env, &["watch", "cancel", "--all"]);

    crate::common::room::seed_sidebar_heartbeat(
        &runtime,
        MuxName::Zellij,
        &env.resolve_workspace(&env.project_root).session_name,
        "no-forge",
    );
    // A scope never probed arms as well.
    let unprobed = wait_ok(&env, &["watch", "pr=merged"]);
    assert_eq!(now_line(&unprobed), "  now: unknown · pr: not checked yet");

    // The next probe finds no supported forge: the armed wait says so and needs you.
    publish("<unsupported>", false, 1_000);
    let rows = listed();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["unread"], serde_json::json!({"pr": NO_FORGE}));
    let text = wait_ok(&env, &["watch", "list"]);
    assert!(
        text.contains(&format!("waiting · pr: {NO_FORGE}")),
        "{text}"
    );
    let (text, json) = loops("1970-01-01T00:00:00Z");
    assert!(text.contains("· 1 need you"), "{text}");
    assert!(!text.contains("all healthy"), "{text}");
    assert_eq!(attention(&json), [serde_json::json!("no_forge")], "{json}");

    // A known no-forge reading refuses before anything is stored.
    let refused = agent_wait(&env)
        .args(["watch", "ci=passed"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{stderr}");
    assert!(
        stderr.contains("`ci` cannot be read in ")
            && stderr.contains(&format!("{NO_FORGE}; {NO_FORGE_FIX}")),
        "{stderr}"
    );
    assert_eq!(listed().len(), 1, "the refused wait stored nothing");

    // A supported-forge reading before first sight is unread in both views until a
    // later probe passes that stamp.
    publish("repo", false, 1_000);
    let (text, json) = loops("1970-01-01T00:00:02Z");
    assert!(text.contains(&format!("pr: {STALE}")), "{text}");
    let unread = serde_json::json!({"pr": STALE});
    assert_eq!(json["rooms"][0]["tasks"][0]["unread"], unread, "{json}");
    let rows = listed();
    assert_eq!(rows[0]["unread"], unread, "{rows:?}");
    let text = wait_ok(&env, &["watch", "list"]);
    assert!(text.contains(&format!("waiting · pr: {STALE}")), "{text}");

    // The reading comes back: the reason and the attention are gone.
    publish("repo", false, 3_000);
    let rows = listed();
    assert!(rows[0].get("unread").is_none(), "{rows:?}");
    let text = wait_ok(&env, &["watch", "list"]);
    assert!(!text.contains(STALE), "{text}");
    let (text, json) = loops("1970-01-01T00:00:02Z");
    assert!(!text.contains(STALE), "{text}");
    assert!(
        json["rooms"][0]["tasks"][0].get("unread").is_none(),
        "{json}"
    );
    assert!(text.contains("all healthy"), "{text}");
    assert_eq!(attention(&json), [serde_json::Value::Null], "{json}");

    // A detached HEAD reads unknown under its own reason, arms, and needs nothing.
    publish("<unsupported>", true, 1_000);
    let detached = wait_ok(&env, &["watch", "ci=passed"]);
    assert_eq!(
        now_line(&detached),
        "  now: unknown · ci: the checkout's HEAD is detached, so it has no branch to read"
    );
    let (text, json) = loops("1970-01-01T00:00:00Z");
    assert!(text.contains("all healthy"), "{text}");
    assert_eq!(
        attention(&json),
        [serde_json::Value::Null, serde_json::Value::Null],
        "{json}"
    );

    // A fresh probe that leaves a key unread (an open PR whose review the forge has not
    // reported) says why on every surface, as `loop show` does.
    const UNREPORTED: &str = "not reported for this checkout's open PR";
    publish("repo", false, 1_000);
    let review = wait_ok(&env, &["watch", "pr.review=approved"]);
    assert_eq!(
        now_line(&review),
        format!("  now: unknown · pr.review: {UNREPORTED}")
    );
    let unread = serde_json::json!({"pr.review": UNREPORTED});
    let rows = listed();
    let row = rows
        .iter()
        .find(|row| row["trigger"].as_str().unwrap().contains("pr.review"))
        .unwrap();
    assert_eq!(row["unread"], unread, "{rows:?}");
    let (_, json) = loops("1970-01-01T00:00:00Z");
    let tasks = json["rooms"][0]["tasks"].as_array().unwrap();
    assert!(tasks.iter().any(|task| task["unread"] == unread), "{json}");
}

#[test]
fn a_watch_parks_with_any_gate_at_an_open_prompt() {
    let env = Env::new();
    let pane_fixture = register_waiting_caller(&env);
    arm_and_fire(&env, || traced_wait(&env, &pane_fixture));
    wait_for_wait_records(&env, 1);
    let messages = wait_for_wait_messages(&env, 1);
    assert_eq!(messages.len(), 1);
    let wake = &messages[0];
    assert_eq!(wake.status, MessageStatus::Queued);
    assert_eq!(wake.gate, DeliveryGate::Any);
    assert_eq!(
        wake.sender,
        MessageSender::Harness {
            notice: HarnessNotice::Wait
        }
    );
    assert_eq!(
        wake.last_error.as_deref(),
        Some("agent is waiting on input in its pane")
    );
    assert_no_wait_paste(&env);
    wait_for_no_wait_instances(&env);
}

#[test]
fn watch_refusals_name_what_the_expression_lacks() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    for (args, expected) in [
        (
            &["watch", "window.5h.left >= 40"][..],
            "no current claude 5h window reading; open the room's sidebar or run `rimz accounts \
             --refresh`",
        ),
        (&["watch", "pr=="][..], "clause 1, position "),
        (
            &["watch", "team.stage=Bogus"][..],
            "no team defines stage Bogus; stages: Done",
        ),
        (
            &["watch", "--label", "x"][..],
            "a watch needs an expression",
        ),
        (&["shell", "--", "  "][..], "a shell needs a command"),
    ] {
        let stderr = wait_refusal(&env, args);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        for wrong in [
            "schedule `",
            "task",
            "--check-only",
            "--agent",
            "--surplus",
            "rimz wait",
        ] {
            assert!(!stderr.contains(wrong), "{args:?} names {wrong}: {stderr}");
        }
    }
    assert!(!loop_instances_path(&env).exists());
}

#[test]
fn shell_and_watch_refuse_each_others_flags_and_list_when_bare() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    for args in [
        &["watch", "--on", "fail", "pr=merged"][..],
        &["watch", "pr=merged", "ci=passed"],
        &["shell", "--when", "pr=merged", "--", "true"],
        &["shell", "--pid", "123"],
        &["shell", "--file", "app.log"],
    ] {
        let stderr = wait_refusal(&env, args);
        assert!(stderr.starts_with("error: "), "{args:?}: {stderr}");
    }
    assert!(!loop_instances_path(&env).exists());
    assert_eq!(wait_ok(&env, &["shell"]), wait_ok(&env, &["shell", "list"]));
    assert_eq!(wait_ok(&env, &["watch"]), wait_ok(&env, &["watch", "list"]));
}

/// Liveness bound for every poll in this file. A watched wait delivers
/// through three detached process starts, so a tighter bound measures the
/// scheduler under load; a poll returns the moment its condition holds.
const POLL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

fn wait_until(env: &Env, description: &str, mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out: {description}; pending messages: {:?}; run records: {:?}; wait-watch log: {}",
            env.store().list_pending_messages(),
            wait_records(env),
            wait_watch_log(env)
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// The room's watcher log, where a watcher's errors and warnings land.
fn wait_watch_log(env: &Env) -> String {
    let path = env.store().paths().wait_watch_log();
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| format!("absent at {}: {err}", path.display()))
}

fn wait_for_wait_messages(env: &Env, count: usize) -> Vec<MessageRecord> {
    let mut messages = Vec::new();
    wait_until(
        env,
        &format!("expected {count} durable wait messages at the consumer"),
        || {
            messages = env.store().list_pending_messages().unwrap();
            messages.len() >= count
        },
    );
    messages
}

fn wait_for_no_wait_instances(env: &Env) {
    wait_until(env, "wait instance was not retired", || {
        wait_instances(env).0.is_empty()
    });
}

fn loop_runs_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("logs/loop-runs.log.jsonl")
}

fn wait_records(env: &Env) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let path = loop_runs_path(env);
    let text = std::fs::read_to_string(path).unwrap_or_default();
    // A record still being appended has no newline yet.
    text[..text.rfind('\n').map_or(0, |end| end + 1)]
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn wait_for_wait_records(
    env: &Env,
    count: usize,
) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let mut records = Vec::new();
    wait_until(env, &format!("expected {count} run records"), || {
        records = wait_records(env);
        records.len() >= count
    });
    records
}
