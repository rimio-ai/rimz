//! Integration coverage for the agent-facing `rimz wait` doorway.

use crate::common::wait::{
    agent_wait, agent_wait_at, loop_instances_path, publish_claude_5h, publish_claude_5h_resetting,
    register_agent, register_agent_with_owner, register_calling_agent,
    register_calling_agent_with_launch, wait_instances, wait_ok,
};
use crate::common::{Env, canonical, retained_runs_refusal, zellij_trace_shim};
use rimz::agents::{AgentLifecycleObservation, AskKind, LaunchParams, LifecycleSignal};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use rimz::store::event::{EventKind, MessageEventMethod};
use rimz::store::message::{
    DeliveryGate, HarnessNotice, MessageRecord, MessageSender, MessageStatus,
};
use rimz::store::writer::AgentLifecycleIntent;

#[test]
fn overlay_failure_leaves_no_wait() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let loops = env.rimz_home().join("loops");
    std::fs::create_dir_all(&loops).unwrap();
    let permissions = std::fs::metadata(&loops).unwrap().permissions();
    std::fs::set_permissions(&loops, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(loops.join("write-probe"), b"").is_ok() {
        std::fs::set_permissions(&loops, permissions).unwrap();
        crate::common::skip("this user bypasses directory write permissions");
        return;
    }
    let output = agent_wait(&env).args(["wait", "--run", "true"]).output();
    std::fs::set_permissions(&loops, permissions).unwrap();
    let output = output.unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("loop-arming.lock"),
        "{output:?}"
    );
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
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

fn assert_unresolved_wait(output: &std::process::Output, unchanged: &str) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.trim(),
        format!(
            "error: {unchanged}RimZ could not find the calling agent's live session from this \
             process (looked for kind `claude`, launch id `launch-session`). If this ran inside \
             a provider's command sandbox: Run `rimz wait …` as its own command, without a chain, \
             pipe, redirect, or `VAR=` prefix, so its routine rule runs outside the sandbox; if \
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
        .args(["wait", "--run", "true"])
        .output()
        .unwrap();
    assert_unresolved_wait(&output, "nothing was armed: ");
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
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
    wait_ok(&env, &["wait", "--in", "5m", "--json"]);
    let before = wait_instances(&env);
    let pending = wait_ok(&env, &["wait", "list", "--json"]);
    let output = read_only_home_wait(&env, &bwrap, true)
        .args(["wait", "list", "--json"])
        .output()
        .unwrap();
    assert_unresolved_wait(&output, "");
    assert_eq!(wait_instances(&env), before);
    assert_eq!(
        pending_rows_at_rest(&wait_ok(&env, &["wait", "list", "--json"])),
        pending_rows_at_rest(&pending)
    );
    wait_ok(&env, &["wait", "cancel", "--all"]);
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
        serde_json::from_str(&wait_ok(&env, &["wait", "--in", "5m", "--json"])).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let before = wait_instances(&env);
    let pending = wait_ok(&env, &["wait", "list", "--json"]);
    let output = read_only_home_wait(&env, &bwrap, true)
        .args(["wait", "cancel", name])
        .output()
        .unwrap();
    assert_unresolved_wait(&output, "nothing was canceled: ");
    assert_eq!(wait_instances(&env), before);
    assert_eq!(
        pending_rows_at_rest(&wait_ok(&env, &["wait", "list", "--json"])),
        pending_rows_at_rest(&pending)
    );
    wait_ok(&env, &["wait", "cancel", name]);
}

#[test]
fn read_only_home_list_shows_hint_and_keeps_wait_pending() {
    let Some(bwrap) = bubblewrap() else {
        return;
    };
    let env = Env::new();
    env.record(&env.project_root);
    register_owned_calling_agent(&env);
    let receipt: serde_json::Value = serde_json::from_str(&wait_ok(
        &env,
        &["wait", "--pid", &std::process::id().to_string(), "--json"],
    ))
    .unwrap();
    let name = receipt["name"].as_str().unwrap();
    wait_until(&env, "process watcher did not start", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_some()
    });
    let before = wait_instances(&env);
    let output = read_only_home_wait(&env, &bwrap, false)
        .args(["wait", "list", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let gate = env.store().runtime_paths().lock_path("lease-gate.lock");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "error: this command cannot write the RimZ home, for example inside a provider's \
             command sandbox. Run `rimz wait …` as its own command, without a chain, pipe, \
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
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["name"], name);
    wait_ok(&env, &["wait", "cancel", name]);
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
        .args(["wait", "--run", "true"])
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
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
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
        serde_json::from_str(&wait_ok(&env, &["wait", "--in", "5m", "--json"])).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let before = wait_instances(&env);
    let output = read_only_home_wait(&env, &bwrap, false)
        .args(["wait", "cancel", name])
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
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["name"], name);
    wait_ok(&env, &["wait", "cancel", name]);
    assert!(wait_instances(&env).0.is_empty());
}

#[test]
fn watcher_spawn_failure_leaves_no_wait() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let output = agent_wait(&env)
        .env("RIMZ_BIN", env.home_root.join("missing-rimz"))
        .args(["wait", "--run", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("starting watched command"),
        "{output:?}"
    );
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
    assert_eq!(pending, serde_json::json!([]));
    assert!(wait_instances(&env).0.is_empty());
}

#[test]
fn config_edit_failure_leaves_no_delivery_instance() {
    use rimz::config::{LoopConfig, TaskEntry, Tasks};
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let machine = LoopConfig {
        tasks: Tasks(BTreeMap::from([(
            "replacement".into(),
            TaskEntry {
                root: env.project_root.clone(),
                agent: Some("claude".into()),
                prompt: Some("original".into()),
                at: Some("07:00".into()),
                ..Default::default()
            },
        )])),
        ..Default::default()
    };
    let home = env.rimz_home();
    let path = home.join("loop.toml");
    let original = toml::to_string(&machine).unwrap();
    std::fs::write(&path, &original).unwrap();
    std::fs::create_dir_all(home.join("loops")).unwrap();
    let permissions = std::fs::metadata(&home).unwrap().permissions();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(home.join("write-probe"), b"").is_ok() {
        std::fs::set_permissions(&home, permissions).unwrap();
        crate::common::skip("this user bypasses directory write permissions");
        return;
    }
    let output = agent_wait(&env)
        .args(["loop", "add", "replacement", "--wait", "--at", "07:00"])
        .output();
    std::fs::set_permissions(&home, permissions).unwrap();
    let output = output.unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("loop.toml"),
        "{output:?}"
    );
    assert!(wait_instances(&env).0.is_empty());
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[test]
fn cancel_overlay_failure_keeps_wait_pending() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "--in", "5m", "--json"])).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let before = wait_instances(&env);
    let loops = env.rimz_home().join("loops");
    let mut paths = vec![loops.clone()];
    paths.extend(
        std::fs::read_dir(&loops)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "lock")),
    );
    let permissions: Vec<_> = paths
        .iter()
        .map(|path| std::fs::metadata(path).unwrap().permissions())
        .collect();
    for path in &paths {
        let mode = if path == &loops { 0o555 } else { 0o444 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let bypasses_permissions = std::fs::OpenOptions::new()
        .write(true)
        .open(loops.join("loop-arming.lock"))
        .is_ok();
    let output =
        (!bypasses_permissions).then(|| agent_wait(&env).args(["wait", "cancel", name]).output());
    for (path, permissions) in paths.iter().zip(permissions) {
        std::fs::set_permissions(path, permissions).unwrap();
    }
    if bypasses_permissions {
        crate::common::skip("this user bypasses directory write permissions");
        return;
    }
    let output = output.unwrap().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("loop-arming.lock"),
        "{output:?}"
    );
    assert_eq!(wait_instances(&env), before);
    let pending: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1);
    assert_eq!(pending[0]["name"], name);
    wait_ok(&env, &["wait", "cancel", name]);
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
        &["wait", "--run", &command, "--check-in", "90m", "--json"],
    ))
    .unwrap();
    let live = receipt["name"].as_str().unwrap();
    let past: serde_json::Value = serde_json::from_str(&wait_ok(
        &env,
        &["wait", "--check", "false", "--check-in", "30m", "--json"],
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
    let text = wait_ok(&env, &["wait", "list"]);
    assert!(text.starts_with("@planner#main · 3 waits\n"), "{text}");
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
    wait_ok(&env, &["wait", "cancel", "--all"]);
}

#[test]
fn cancel_all_leaves_subscriptions_and_stops_only_self_watchers() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    wait_ok(
        &env,
        &[
            "loop",
            "add",
            "deployment",
            "--wait",
            "--signal",
            "deploy.failed",
        ],
    );
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "--check", "false", "--json"])).unwrap();
    let name = receipt["name"].as_str().unwrap();
    wait_until(&env, "watcher did not start", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_some()
    });
    let canceled: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "cancel", "--all", "--json"])).unwrap();
    assert_eq!(canceled["canceled"], serde_json::json!([name]));
    assert_eq!(canceled["pending"].as_array().unwrap().len(), 1);
    assert_eq!(canceled["pending"][0]["owner"], "loop:deployment");
    wait_until(&env, "cancel left the self watcher alive", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_none()
    });
    assert_eq!(
        wait_ok(&env, &["wait", "cancel", "--all"]),
        "@planner#main · no waits · also woken by deploy.failed (loop deployment)\n"
    );
    wait_ok(&env, &["wait", "cancel", "deployment"]);
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
        serde_json::from_str(&wait_ok(&env, &["wait", "--in", "5m", "--json"])).unwrap();
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
        .args(["wait", "list", "--json"])
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
    let waits = shell_text(&["wait", "list"]);
    assert!(
        waits.starts_with("~/project · whole room · 3 waits · 3 subscriptions\n\n"),
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
        serde_json::from_str(&run(&["wait", "--in", "5m", "--json"], true)).unwrap();
    assert_eq!(receipt["target"], "@claude#main");
    let waits = run(&["wait", "list"], false);
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
    assert_eq!(
        body.lines().next().unwrap().trim(),
        receipt["target"].as_str().unwrap()
    );
}

#[test]
fn loop_wait_targets_name_and_resolve_the_pane_owner_like_the_arm_receipt() {
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for (name, address) in [
        ("bare", None),
        ("named", Some("@claude")),
        ("shadowed", Some("@cleared-session")),
    ] {
        let env = Env::new();
        env.record(&env.project_root);
        let pane = register_cleared_and_owner_sessions(&env);
        let pane_fixture = env.write_pane_fixture(&[pane]);
        let run = |args: &[&str]| {
            env.rimz()
                .args(["--mux", "tmux"])
                .args(args)
                .env("RIMZ_TEST_PANE_LIST", &pane_fixture)
                .env("RIMZ_AGENT_KIND", "claude")
                .env("RIMZ_AGENT_ID", "owner-session")
                .output()
                .unwrap()
        };
        let receipt = run(&["wait", "--in", "5m", "--json"]);
        assert!(
            receipt.status.success(),
            "{}",
            String::from_utf8_lossy(&receipt.stderr)
        );
        let receipt: serde_json::Value = serde_json::from_slice(&receipt.stdout).unwrap();
        assert_eq!(receipt["target"], "@claude#main");

        let mut args = vec!["loop", "add", name, "--wait"];
        args.extend(address);
        args.extend(["--signal", "deploy.done"]);
        let output = run(&args);
        let tasks = wait_instances(&env);
        assert_eq!(
            tasks.0.contains_key(name),
            output.status.success(),
            "{name}"
        );
        actual.push(serde_json::json!({
            "task": name,
            "success": output.status.success(),
            "stderr": String::from_utf8(output.stderr).unwrap().trim(),
            "wait": tasks.0.get(name).and_then(|task| task.wait.as_ref()),
        }));
        expected.push(if address == Some("@cleared-session") {
            serde_json::json!({
                "task": name,
                "success": false,
                "stderr": concat!(
                    "error: no live agent matches `@cleared-session`; ",
                    "see `rimz agents list` for live handles",
                ),
                "wait": null,
            })
        } else {
            serde_json::json!({
                "task": name,
                "success": true,
                "stderr": "",
                "wait": {
                    "kind": "claude",
                    "session": "owner-session",
                    "handle": receipt["target"],
                },
            })
        });
    }
    assert_eq!(actual, expected);
}

#[test]
fn wait_labels_persist_for_every_trigger_and_replace_watch_previews() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let pid = std::process::id().to_string();
    let unicode_label = "é".repeat(60);
    for (trigger, subject, label) in [
        ("--in", "5m", unicode_label.as_str()),
        ("--pid", pid.as_str(), "process"),
        ("--check", "false", "check"),
        ("--file", "pending.log", "file"),
        ("--run", "exec sleep 300", "run"),
    ] {
        let padded = format!("  {label}  ");
        let receipt: serde_json::Value = serde_json::from_str(&wait_ok(
            &env,
            &["wait", trigger, subject, "--label", &padded, "--json"],
        ))
        .unwrap();
        let name = receipt["name"].as_str().unwrap();
        let tasks = serde_json::to_value(wait_instances(&env)).unwrap();
        assert_eq!(
            tasks[name]["label"], label,
            "label must survive the instance write"
        );
        let rows: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .unwrap();
        assert_eq!(row["label"], label);
        if trigger != "--in" {
            assert_eq!(row["trigger"], label);
            let text = wait_ok(&env, &["wait", "list"]);
            assert!(text.contains(label), "{text}");
            if trigger == "--run" {
                assert!(!text.contains(subject), "{text}");
            }
        }
        let shown = wait_ok(&env, &["loop", "show", name]);
        assert!(
            shown
                .lines()
                .any(|line| line.trim_start().starts_with("label:") && line.contains(label)),
            "{shown}"
        );
        wait_ok(&env, &["wait", "cancel", name]);
    }
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "--in", "5m", "--json"])).unwrap();
    assert!(receipt["pending"][0].get("label").is_none());
    let tasks = serde_json::to_value(wait_instances(&env)).unwrap();
    assert!(
        tasks[receipt["name"].as_str().unwrap()]
            .get("label")
            .is_none()
    );
}

#[test]
fn clock_and_condition_wait_lines_keep_their_trigger_and_add_the_label_once() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let mut lines = Vec::new();
    for (args, trigger) in [
        (&["--at", "23:59"][..], "once at "),
        (&["--in", "5m"][..], "once at "),
        (&["--when", "pr=merged"][..], "when pr=merged"),
    ] {
        let label = format!("label-{}", lines.len());
        let mut argv = vec!["wait"];
        argv.extend_from_slice(args);
        argv.extend_from_slice(&["--label", &label, "--json"]);
        let receipt: serde_json::Value = serde_json::from_str(&wait_ok(&env, &argv)).unwrap();
        lines.push((receipt["name"].as_str().unwrap().to_owned(), trigger, label));
    }
    let shell = env.rimz().args(["wait", "list"]).output().unwrap();
    assert!(shell.status.success());
    for view in [
        wait_ok(&env, &["wait", "list"]),
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
            vec!["wait", "--in", "5m", "--label", &label],
            vec![
                "loop", "add", "labelled", "--every", "1h", "--check", "true", "--label", &label,
            ],
        ] {
            let output = env.rimz().args(args).output().unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "invalid label accepted");
            assert!(
                error.contains("label must be one line of 1–60 characters"),
                "{error}"
            );
            assert!(!loop_instances_path(&env).exists());
            assert!(!env.rimz_home().join("loop.toml").exists());
        }
    }
}

#[test]
fn loop_labels_persist_in_machine_project_and_delivery_tasks() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    for (name, extra) in [
        ("machine", vec!["--check", "true"]),
        ("project", vec!["--check", "true", "--project"]),
        ("delivery", vec!["--wait"]),
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
            "project" => {
                std::fs::read_to_string(env.project_root.join(".rimz/config.toml")).unwrap()
            }
            _ => std::fs::read_to_string(loop_instances_path(&env)).unwrap(),
        };
        let value: serde_json::Value = if name == "delivery" {
            serde_json::from_str(&text).unwrap()
        } else {
            serde_json::to_value(toml::from_str::<toml::Value>(&text).unwrap()).unwrap()
        };
        let task = if name == "delivery" {
            &value[name]
        } else {
            &value["tasks"][name]
        };
        assert_eq!(task["label"], "gate docs");
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
    let receipt = wait_ok(&env, &["wait", "--json", "--run", command]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["trigger"], format!("watch: {command}"));
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
        .args(["wait", "--run", "printf 'first line\\nsecond line\\n'"])
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
        .args(["wait", "--run", command])
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
            .args(["wait", "--run", command])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--run needs a command"), "{stderr}");
        assert!(!loop_instances_path(&env).exists());
    }
}

#[test]
fn wait_rejects_retired_timeout_with_fix() {
    let env = Env::new();
    for extra in [vec![], vec!["--run", "true"], vec!["--in", "5m"]] {
        let output = env
            .rimz()
            .args(["wait", "--timeout", "5m"])
            .args(extra)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "error: `--timeout` is now `--check-in`: it sends one still-running notice and never stops the command"
        );
    }
}

#[test]
fn wait_check_lists_elapsed_and_checkin_limit() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let receipt = wait_ok(&env, &["wait", "--check", "false", "--check-in", "12m"]);
    assert!(
        receipt.lines().next().unwrap().ends_with("(check-in 12m)"),
        "{receipt}"
    );
    wait_until(&env, "watcher holds its lock", || {
        let rows: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
        rows[0]["watcher_pid"].is_u64()
    });
    let rows: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
    let row = &rows[0];
    assert_eq!(row["type"], "check");
    assert_eq!(row["state"], "watching");
    assert_eq!(row["trigger"], "false");
    assert_eq!(row["check_in"], "12m");
    assert_eq!(row["check_in_s"], 720);
    assert!(row["elapsed_s"].is_u64());
    assert!(row["watcher_pid"].is_u64());
    assert!(row.get("age").is_none());
    assert!(row.get("timeout").is_none());
    assert!(row.get("timeout_s").is_none());
    let text = wait_ok(&env, &["wait", "list"]);
    assert!(text.starts_with("@planner#main · 1 wait\n"), "{text}");
    assert!(
        text.contains("watching") && text.contains("check-in in "),
        "{text}"
    );
    wait_ok(&env, &["wait", "cancel", "--all"]);
}

#[test]
fn lost_watch_keeps_elapsed_and_recorded_checkin_limit() {
    // Keepalive on with a provider TTL records no limit; off falls back to 30m.
    for (switch, timeout, timeout_s) in [
        ("false", serde_json::json!("30m"), serde_json::json!(1800)),
        ("true", serde_json::Value::Null, serde_json::Value::Null),
    ] {
        let env = Env::new();
        env.record(&env.project_root);
        register_calling_agent(&env);
        wait_ok(&env, &["config", "set", "harness.cache_keepalive", switch]);
        wait_ok(
            &env,
            &["config", "set", "harness.prompt_cache_ttl.claude", "1h"],
        );
        let armed = agent_wait(&env)
            .env("RIMZ_BIN", "/bin/true")
            .args(["wait", "--check", "false", "--json"])
            .output()
            .unwrap();
        assert!(
            armed.status.success(),
            "{}",
            String::from_utf8_lossy(&armed.stderr)
        );
        let rows: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
        assert_eq!(rows[0]["state"], "starting");
        assert!(wait_ok(&env, &["wait", "list"]).contains("starting"));
        let loops: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["loop", "list", "--json"])).unwrap();
        assert!(loops["rooms"][0]["tasks"][0].get("attention").is_none());
        let mut tasks = wait_instances(&env);
        for entry in tasks.0.values_mut() {
            entry.wait_meta.as_mut().unwrap().armed_at =
                jiff::Timestamp::now() - jiff::SignedDuration::from_secs(60);
        }
        std::fs::write(
            loop_instances_path(&env),
            serde_json::to_vec(&tasks).unwrap(),
        )
        .unwrap();
        let rows: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
        assert_eq!(rows[0]["state"], "lost");
        let loops: serde_json::Value =
            serde_json::from_str(&wait_ok(&env, &["loop", "list", "--json"])).unwrap();
        assert_eq!(loops["rooms"][0]["tasks"][0]["attention"], "lost");
        assert!(rows[0]["elapsed_s"].is_u64());
        assert_eq!(rows[0]["check_in"], timeout, "keepalive {switch}");
        assert_eq!(rows[0]["check_in_s"], timeout_s, "keepalive {switch}");
        assert!(rows[0].get("watcher_pid").is_none());
    }
}

#[test]
fn wait_delay_arms_instance_for_the_calling_agent() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent_with_launch(
        &env,
        LaunchParams {
            team: Some("forge".to_owned()),
            role: Some("planner".to_owned()),
            ..LaunchParams::default()
        },
    );
    assert_eq!(
        agents_wait_exit(&env),
        Some(124),
        "never-started agent finished"
    );
    let stdout = wait_ok(&env, &["wait", "--in", "5m"]);
    assert!(stdout.starts_with("armed wait-"), "{stdout}");
    assert!(stdout.contains("in 5m"), "{stdout}");
    assert!(stdout.contains("→ @planner"), "{stdout}");
    assert!(!stdout.lines().next().unwrap().contains("(check-in"));
    let listed = wait_ok(&env, &["wait", "list"]);
    assert_eq!(
        listed
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>(),
        ["@planner#main", "·", "1", "wait"]
    );
    let rows: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
    assert_eq!(rows[0]["type"], "timer");
    assert_eq!(rows[0]["state"], "pending");
    assert_eq!(rows[0]["owner"], "self");
    assert_eq!(rows[0].get("parent"), Some(&serde_json::Value::Null));
    assert_eq!(rows[0]["check_in"], "5m");
    assert_eq!(rows[0]["check_in_s"], 300);
    assert!(rows[0]["elapsed_s"].is_u64());
    assert!(rows[0].get("watcher_pid").is_none());
    let tasks = wait_instances(&env);
    assert_eq!(tasks.0.len(), 1);
    let (name, entry) = tasks.0.iter().next().unwrap();
    assert!(name.starts_with("wait-"));
    assert!(entry.at.is_none() && entry.fire_at.is_some());
    assert_eq!(
        entry
            .wait_meta
            .as_ref()
            .and_then(|meta| meta.delay.as_deref()),
        Some("5m")
    );
    assert!(entry.signal.is_none());
    assert!(entry.deadline.is_none());
    assert!(entry.prompt.is_none());
    assert!(entry.wait_meta.is_some());
    let target = entry.wait.as_ref().expect("pinned wait target");
    assert_eq!(target.kind.as_str(), "claude");
    assert_eq!(target.session.as_str(), "provider-session");
    assert_eq!(target.handle, "@planner#main");

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
            .expect("record the arming turn");
    }
    let report: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
            .expect("agent report");
    assert_eq!(report["agent"]["status"], "sleeping");
    assert_eq!(report["agent"]["pending_waits"][0]["name"], *name);
    let teams: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["teams", "list", "--json"])).expect("team report");
    assert_eq!(teams[0]["instances"][0]["state"], "sleeping", "{teams}");
    assert_eq!(agents_wait_exit(&env), Some(124), "sleeping agent finished");
    wait_ok(&env, &["wait", "cancel", name]);
    let report: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
            .expect("agent report after cancellation");
    assert_eq!(report["agent"]["status"], "success");
    let teams: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["teams", "list", "--json"]))
            .expect("team report after cancellation");
    assert_eq!(teams[0]["instances"][0]["state"], "idle");
    assert_eq!(agents_wait_exit(&env), Some(0));
    rimz::transcript::append(
        env.store().paths(),
        &rimz::transcript::TranscriptEntry::new(
            jiff::Timestamp::now(),
            AgentKind::new_unchecked("claude"),
            AgentSessionId::from("provider-session"),
            rimz::transcript::TranscriptKind::Assistant,
            "planned the rollout".to_owned(),
        ),
    )
    .expect("record the turn's final message");
    assert_eq!(
        wait_ok(&env, &["agents", "wait", "@planner", "--timeout", "1s"]),
        "planned the rollout\n"
    );
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
        let env = Env::new();
        env.record(&env.project_root);
        register_calling_agent(&env);
        wait_ok(&env, &["config", "set", "harness.cache_keepalive", switch]);
        wait_ok(
            &env,
            &["config", "set", "harness.prompt_cache_ttl.claude", ttl],
        );
        let mut args = vec!["wait", "--json"];
        if let Some(timeout) = explicit {
            args.extend(["--check-in", timeout]);
        }
        args.extend(["--run", "sleep 600"]);
        let receipt: serde_json::Value = serde_json::from_str(&wait_ok(&env, &args)).unwrap();
        let name = receipt["name"].as_str().unwrap();
        let tasks = wait_instances(&env);
        assert_eq!(
            tasks.0[name].timeout.as_deref(),
            expected,
            "keepalive {switch}, TTL {ttl}, explicit {explicit:?}"
        );
        wait_ok(&env, &["wait", "cancel", name]);
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
        ended_turn: false,
        class: TurnErrorClass::PausedRateLimit,
        at,
        label: None,
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
    let armed = env
        .rimz()
        .args([
            "loop",
            "add",
            "deployment",
            "--wait",
            "@planner",
            "--signal",
            "deploy.failed",
        ])
        .output()
        .expect("arm wait from human shell");
    assert!(
        armed.status.success(),
        "{}",
        String::from_utf8_lossy(&armed.stderr)
    );
    let name = "deployment";

    let listed = agent_wait(&env)
        .args(["wait", "list", "--json"])
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
        wait_ok(&env, &["wait", "list"]),
        "@planner#main · no waits · also woken by deploy.failed (loop deployment)\n"
    );

    let canceled = agent_wait(&env)
        .args(["wait", "cancel", name])
        .output()
        .expect("cancel wait as target agent");
    assert!(
        canceled.status.success(),
        "{}",
        String::from_utf8_lossy(&canceled.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&canceled.stdout),
        format!("canceled {name}\n@planner#main · no waits\n")
    );
}

#[test]
fn wait_arm_refuses_a_plain_shell() {
    let env = Env::new();
    env.record(&env.project_root);
    let output = env
        .rimz()
        .args(["wait", "--in", "5m"])
        .output()
        .expect("run wait from shell");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("arming a wait is only available to an agent"),
        "{stderr}"
    );
}

#[test]
fn wait_accepts_delays_of_a_day_or_more_as_an_absolute_instant() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let before = jiff::Timestamp::now();
    let stdout = wait_ok(&env, &["wait", "--in", "2d"]);
    assert!(stdout.contains("in 2d"), "{stdout}");
    let tasks = wait_instances(&env);
    let (_, entry) = tasks.0.iter().next().expect("armed wait row");
    let fire_at = entry.fire_at.expect("a timer stores fire-at");
    assert!(entry.at.is_none(), "{entry:?}");
    assert!(
        fire_at
            >= before
                .checked_add(jiff::SignedDuration::from_secs(2 * 86_400))
                .unwrap(),
        "{fire_at}"
    );
    assert_eq!(
        entry
            .wait_meta
            .as_ref()
            .and_then(|meta| meta.delay.as_deref()),
        Some("2d")
    );
}

#[test]
fn wait_rejects_watch_checkins_at_or_above_24_hours() {
    let env = Env::new();
    register_calling_agent(&env);
    for timeout in ["24h", "25h"] {
        let output = agent_wait(&env)
            .args(["wait", "--check-in", timeout, "--run", "true"])
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

#[test]
fn wait_pid_checks_in_then_delivers_after_process_disappears_without_output_file() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let mut process = std::process::Command::new("sleep")
        .arg("30")
        .env("HOME", &env.home_root)
        .spawn()
        .unwrap();
    let pid = process.id().to_string();
    let receipt = wait_ok(&env, &["wait", "--pid", &pid, "--check-in", "1s", "--json"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    assert_eq!(receipt["trigger"], format!("pid {pid}"));
    assert_eq!(receipt["pending"][0]["type"], "pid");
    assert_eq!(receipt["pending"][0]["trigger"], pid);
    let listed: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
    assert_eq!(listed[0]["trigger"], pid);
    let tasks = wait_instances(&env);
    let entry = &tasks.0[receipt["name"].as_str().unwrap()];
    assert_eq!(
        entry.watch,
        Some(rimz::config::WatchSpec::Pid { pid: process.id() })
    );
    assert_eq!(entry.on, Some(rimz::config::CheckOn::Any));
    let report: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
            .expect("agent report");
    assert_eq!(
        report["agent"]["pending_waits"][0]["trigger"],
        serde_json::json!({"kind": "pid", "pid": process.id()})
    );
    let checkin = wait_for_wait_messages(&env, 1);
    assert!(
        checkin[0]
            .text
            .contains(&format!("waited on pid {pid}\nstill not met after")),
        "{}",
        checkin[0].text
    );
    assert!(!checkin[0].text.contains("output"), "{}", checkin[0].text);
    assert!(process.try_wait().unwrap().is_none());
    assert_eq!(wait_instances(&env).0.len(), 1);
    process.kill().unwrap();
    process.wait().unwrap();
    wait_for_wait_records(&env, 2);
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    assert_eq!(
        messages
            .iter()
            .find(|message| message.message_id == checkin[0].message_id)
            .unwrap()
            .status,
        MessageStatus::Canceled
    );
    let completed = messages
        .iter()
        .find(|message| message.text.contains("\nmet after"))
        .expect("process disappearance delivered");
    assert!(completed.text.contains(&pid));
    assert!(!completed.text.contains("output"), "{}", completed.text);
    wait_for_no_wait_instances(&env);

    let receipt = wait_ok(&env, &["wait", "--pid", &pid]);
    assert!(receipt.starts_with("armed wait-"), "{receipt}");
    assert!(receipt.contains(&format!(": pid {pid} →")), "{receipt}");
    assert_eq!(wait_for_wait_messages(&env, 2).len(), 2);
    wait_for_no_wait_instances(&env);
}

#[test]
fn canceling_pid_wait_leaves_the_existing_process_running() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let mut process = std::process::Command::new("sleep")
        .arg("30")
        .env("HOME", &env.home_root)
        .spawn()
        .unwrap();
    let receipt = wait_ok(
        &env,
        &["wait", "--pid", &process.id().to_string(), "--json"],
    );
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    wait_until(&env, "process watcher did not start", || {
        rimz::harness::schedule::signal::watcher_info(env.store().runtime_paths(), name)
            .unwrap()
            .is_some()
    });
    wait_ok(&env, &["wait", "cancel", name]);
    assert!(process.try_wait().unwrap().is_none());
    assert!(wait_instances(&env).0.is_empty());
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    process.kill().unwrap();
    process.wait().unwrap();
}

#[test]
fn wait_pid_refuses_a_process_it_cannot_observe() {
    if nix::sys::signal::kill(nix::unistd::Pid::from_raw(1), None) != Err(nix::errno::Errno::EPERM)
    {
        crate::common::skip("pid 1 is not signal-protected from this user");
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let output = agent_wait(&env)
        .args(["wait", "--pid", "1"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot watch PID 1: permission denied"),
        "{stderr}"
    );
    assert!(
        stderr.contains("choose a process owned by your user"),
        "{stderr}"
    );
    assert!(!loop_instances_path(&env).exists());
}

#[test]
fn wait_pid_rejects_invalid_pids_and_conflicting_triggers() {
    let env = Env::new();
    for args in [
        vec!["wait", "--pid", "0"],
        vec!["wait", "--pid=-1"],
        vec!["wait", "--pid", "2147483648"],
        vec!["wait", "--pid", "not-a-pid"],
        vec!["wait", "--pid", "123;true"],
        vec!["wait", "--pid", "123", "--in", "5m"],
        vec!["wait", "--pid", "123", "--run", "true"],
        vec!["wait", "--pid", "123", "--on", "success"],
    ] {
        let output = agent_wait(&env).args(&args).output().unwrap();
        assert!(!output.status.success(), "accepted {args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("invalid value")
                || stderr.contains("choose exactly one wait trigger")
                || stderr.contains("--on requires --check or --run"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn wait_check_polls_until_the_command_succeeds_then_retires() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let flag = env.home_root.join("check-flag");
    let check = format!("echo probing; test -e '{}'", flag.display());
    let receipt: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["wait", "--check", &check, "--json"])).unwrap();
    assert_eq!(receipt["trigger"], format!("check: {check}"));
    assert_eq!(receipt["pending"][0]["type"], "check");
    assert_eq!(receipt["pending"][0]["trigger"], check);
    let tasks = wait_instances(&env);
    let entry = &tasks.0[receipt["name"].as_str().unwrap()];
    assert_eq!(
        entry.watch,
        Some(rimz::config::WatchSpec::Check {
            check: check.clone(),
            every: "1s".to_owned(),
            on: rimz::config::CheckOn::Success,
        })
    );
    assert_eq!(entry.on, Some(rimz::config::CheckOn::Any));
    let report: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
            .expect("agent report");
    assert_eq!(
        report["agent"]["pending_waits"][0]["trigger"],
        serde_json::json!({"kind": "check", "command": check})
    );
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    assert_eq!(wait_instances(&env).0.len(), 1);

    std::fs::write(&flag, "").unwrap();
    let messages = wait_for_wait_messages(&env, 1);
    let text = &messages[0].text;
    assert!(
        text.contains(&format!("waited on check `{check}`\nmet after ")),
        "{text}"
    );
    assert!(text.contains(" · output: "), "{text}");
    assert!(text.contains("(<1k tokens, 1 line)"), "{text}");
    wait_for_no_wait_instances(&env);
    assert_eq!(env.store().list_pending_messages().unwrap().len(), 1);
}

#[test]
fn wait_check_on_fail_checks_in_with_still_not_met() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let flag = env.home_root.join("check-flag");
    std::fs::write(&flag, "").unwrap();
    let check = format!("test -e '{}'", flag.display());
    let receipt = wait_ok(
        &env,
        &[
            "wait",
            "--check",
            &check,
            "--on",
            "fail",
            "--check-in",
            "1s",
        ],
    );
    assert!(
        receipt.contains(&format!(": check: {check} on fail →")),
        "{receipt}"
    );
    let checkin = wait_for_wait_messages(&env, 1);
    let text = &checkin[0].text;
    assert!(text.contains("\nstill not met after "), "{text}");
    assert!(text.contains(" · still watching"), "{text}");
    assert!(text.contains("Stop it: rimz wait cancel wait-"), "{text}");
    assert!(!text.contains("output"), "{text}");
    assert_eq!(wait_instances(&env).0.len(), 1);

    std::fs::remove_file(&flag).unwrap();
    wait_for_wait_records(&env, 2);
    let (mut messages, history) = env.store().list_messages_and_history().unwrap();
    messages.extend(history);
    assert_eq!(
        messages
            .iter()
            .find(|message| message.message_id == checkin[0].message_id)
            .unwrap()
            .status,
        MessageStatus::Canceled
    );
    assert!(
        messages
            .iter()
            .any(|message| message.text.contains("\nmet after ")),
        "{messages:?}"
    );
    wait_for_no_wait_instances(&env);
}

#[test]
fn wait_check_reports_a_command_it_cannot_run() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    wait_ok(&env, &["wait", "--check", "definitely-not-a-rimz-command"]);
    let messages = wait_for_wait_messages(&env, 1);
    assert!(
        messages[0].text.contains("\nexit 127 after "),
        "{}",
        messages[0].text
    );
    wait_for_no_wait_instances(&env);
}

#[test]
fn wait_check_rejects_shapes_without_a_meaning() {
    let env = Env::new();
    for (args, expected) in [
        (
            vec!["wait", "--check", "true", "--on", "any"],
            "--on any has no meaning with --check",
        ),
        (
            vec!["wait", "--every", "5s", "--run", "true"],
            "--every requires --check",
        ),
        (
            vec!["wait", "--check", "true", "--pid", "123"],
            "choose exactly one wait trigger: --in, --at, --after-reset, --when, --pid, --check, \
             --file, or --run",
        ),
        (vec!["wait", "--check", " "], "--check needs a command"),
        (
            vec!["wait", "--grep", "ready", "--run", "true"],
            "--grep requires --file",
        ),
        (
            vec!["wait", "--file", "app.log", "--grep", ""],
            "--grep needs a pattern",
        ),
        (
            vec!["wait", "--file", "app.log", "--in", "5m"],
            "choose exactly one wait trigger",
        ),
        (
            vec!["wait", "--in", "5m", "--check-in", "1m"],
            "--check-in requires --pid, --check, --file, or --run",
        ),
        (
            vec!["wait", "--check", "true", "--every", "24h"],
            "--every must be less than 24h",
        ),
    ] {
        let output = agent_wait(&env).args(&args).output().unwrap();
        assert!(!output.status.success(), "accepted {args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
    }
}

#[test]
fn wait_file_fires_on_any_change_including_creation() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let dir = env.project_root.join("logs");
    std::fs::create_dir_all(&dir).unwrap();
    let output = agent_wait(&env)
        .current_dir(&env.project_root)
        .args(["wait", "--file", "logs/app.log", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let path = canonical(&dir).join("app.log");
    assert_eq!(receipt["trigger"], format!("file: {}", path.display()));
    assert_eq!(receipt["pending"][0]["type"], "file");
    assert_eq!(receipt["pending"][0]["trigger"], path.display().to_string());
    let tasks = wait_instances(&env);
    assert_eq!(
        tasks.0[receipt["name"].as_str().unwrap()].watch,
        Some(rimz::config::WatchSpec::File {
            file: path.clone(),
            grep: None,
            mark: None,
        })
    );
    let report: serde_json::Value =
        serde_json::from_str(&wait_ok(&env, &["agents", "show", "@planner", "--json"]))
            .expect("agent report");
    assert_eq!(
        report["agent"]["pending_waits"][0]["trigger"],
        serde_json::json!({"kind": "file", "path": path, "grep": null})
    );
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    assert!(env.store().list_pending_messages().unwrap().is_empty());

    std::fs::write(&path, "started\n").unwrap();
    let messages = wait_for_wait_messages(&env, 1);
    assert!(
        messages[0]
            .text
            .starts_with(&format!("waited on file {}\nmet after ", path.display())),
        "{}",
        messages[0].text
    );
    wait_for_no_wait_instances(&env);
    let refused = agent_wait(&env)
        .args(["wait", "--file"])
        .arg(&dir)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("is a directory"));
}

#[test]
fn wait_file_grep_fires_only_on_a_line_after_the_arm_point() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    let path = canonical(&env.home_root).join("server.log");
    std::fs::write(&path, "booting\nlistening on :3000\n").unwrap();
    let file = path.to_str().unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&wait_ok(
        &env,
        &["wait", "--file", file, "--grep", "listening", "--json"],
    ))
    .unwrap();
    assert_eq!(receipt["trigger"], format!("file: {file} grep: listening"));
    assert_eq!(receipt["pending"][0]["type"], "file");
    assert_eq!(
        receipt["pending"][0]["trigger"],
        format!("{file} grep: listening")
    );
    let append = |text: &str| {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    };
    append("reloading\nlisten");
    std::thread::sleep(std::time::Duration::from_millis(1_500));
    assert!(env.store().list_pending_messages().unwrap().is_empty());
    append("ing on :3001\n");
    let messages = wait_for_wait_messages(&env, 1);
    let text = &messages[0].text;
    assert!(
        text.starts_with(&format!(
            "waited on file {file} for `listening`\nmet after "
        )),
        "{text}"
    );
    assert!(text.contains(": `listening on :3001` · output: "), "{text}");
    wait_for_no_wait_instances(&env);
    let output_path = text
        .split(" · output: ")
        .nth(1)
        .and_then(|rest| rest.split(" (").next())
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(output_path).unwrap(),
        "listening on :3001\n"
    );

    wait_ok(&env, &["wait", "--file", file, "--grep", "listening"]);
    std::fs::write(&path, "listening again\n").unwrap();
    let messages = wait_for_wait_messages(&env, 2);
    assert!(
        messages
            .iter()
            .any(|message| message.text.contains(": `listening again`")),
        "{messages:?}"
    );
    wait_for_no_wait_instances(&env);
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
            "wait",
            "--json",
            "--run",
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
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
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
            "wait",
            "--json",
            "--run",
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
        .args(["wait", "--json", "--run", "sleep 1; printf survived"])
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
        .args(["wait", "watch", "wait-missing"])
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
        &["wait", "--json", "--run", "printf started; exec sleep 30"],
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

#[test]
fn watch_retires_without_delivery_when_its_polarity_does_not_match() {
    for (on, command) in [("fail", "true"), ("success", "false")] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        wait_ok(&env, &["wait", "--on", on, "--run", command]);
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
        wait_ok(&env, &["wait", "--run", "printf self-wait-marker"]);
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
        .args(["wait", "--run", "printf wake-marker"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let records = wait_for_wait_records(&env, 1);
    let mut messages = env.store().list_messages().unwrap();
    messages.extend(env.store().list_message_history().unwrap());
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
        .args(["wait", "--json", "--check-in", "1s", "--run", &command])
        .output()
        .unwrap();
    assert!(output.status.success());
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let records = wait_for_wait_records(&env, 1);
    let mut messages = env.store().list_messages().unwrap();
    messages.extend(env.store().list_message_history().unwrap());
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
        serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
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
        .args(["wait", "--check-in", "1s", "--run", &command])
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
    assert_eq!(writes.len(), 2, "{trace}");
    assert!(
        writes[0].contains("\taction\twrite\t--pane-id\tterminal_3\t27\t91\t50\t48\t48\t126\t")
            && writes[0].ends_with("\t27\t91\t50\t48\t49\t126"),
        "{trace}"
    );
    assert!(
        writes[1].ends_with("\taction\twrite\t--pane-id\tterminal_3\t13"),
        "{trace}"
    );
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
        2,
        "{trace}"
    );
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
        hosted_agent_lineage: Vec::new(),
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
                "wait",
                "--json",
                "--on",
                on,
                "--check-in",
                "1s",
                "--run",
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
            serde_json::from_str(&wait_ok(&env, &["wait", "list", "--json"])).unwrap();
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
            notice.text.contains(&format!("rimz wait cancel {name}")),
            "{}",
            notice.text
        );
        assert!(notice.text.contains("rimz wait --in 1s"), "{}", notice.text);
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

#[test]
fn once_wait_subscriber_is_consumed_by_watcher_checkin() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    register_calling_agent(&env);
    wait_ok(
        &env,
        &[
            "loop", "add", "audit", "--signal", "wait.*", "--wait", "@me", "--once",
        ],
    );
    let release = env.home_root.join("release");
    let receipt = wait_ok(
        &env,
        &[
            "wait",
            "--json",
            "--check-in",
            "1s",
            "--run",
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
    wait_ok(
        &env,
        &[
            "loop", "add", "audit", "--signal", "wait.*", "--wait", "@me",
        ],
    );
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
            "wait",
            "--json",
            "--run",
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
    let receipt = wait_ok(&env, &["wait", "--in", "5m", "--json"]);
    let receipt: serde_json::Value = serde_json::from_str(&receipt).unwrap();
    let name = receipt["name"].as_str().unwrap();
    let mut tasks = wait_instances(&env);
    let entry = tasks.0.get_mut(name).unwrap();
    entry.fire_at = None;
    entry.watch = Some(rimz::config::WatchSpec::Command(
        "touch command-started".to_owned(),
    ));
    std::fs::write(
        loop_instances_path(&env),
        serde_json::to_vec(&tasks).unwrap(),
    )
    .unwrap();

    wait_ok(&env, &["wait", "cancel", name]);
    let output = env.rimz().args(["wait", "watch", name]).output().unwrap();
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
                "wait",
                "--run",
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
    let canceled = wait_ok(&env, &["wait", "cancel", "--all"]);
    assert!(canceled.starts_with("canceled "), "{canceled}");
    for name in names {
        assert!(canceled.contains(&name), "{canceled}");
    }
    assert!(
        canceled.ends_with("@planner#main · no waits\n"),
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
    let first = wait_ok(&env, &["wait", "--in", "5m", "--json"]);
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    let second = wait_ok(&env, &["wait", "--in", "10m", "--json"]);
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    let listed = wait_ok(&env, &["wait", "list", "--json"]);
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
    let held = wait_ok(&env, &["wait", "list", "--json"]);
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
        &["wait", "cancel", first["name"].as_str().unwrap(), "--json"],
    );
    let canceled: serde_json::Value = serde_json::from_str(&canceled).unwrap();
    assert_eq!(canceled["canceled"], serde_json::json!([first["name"]]));
    assert_eq!(canceled["pending"].as_array().unwrap().len(), 1);
    assert_eq!(canceled["pending"][0]["name"], second["name"]);
    let listed = wait_ok(&env, &["wait", "list", "--json"]);
    assert_eq!(
        stable_rows(canceled["pending"].clone()),
        stable_rows(serde_json::from_str::<serde_json::Value>(&listed).unwrap())
    );
    let human = wait_ok(&env, &["wait", "--in", "15m"]);
    assert!(human.starts_with("armed wait-"), "{human}");
    assert!(human.contains(second["name"].as_str().unwrap()), "{human}");
}

#[test]
fn wait_rejects_removed_target_prompt_and_signal_flags() {
    let env = Env::new();
    register_calling_agent(&env);
    for (args, refusal) in [
        (
            vec!["wait", "@planner", "--in", "5m"],
            "unrecognized subcommand",
        ),
        (
            vec!["wait", "--in", "5m", "--prompt", "note"],
            "unexpected argument",
        ),
        (
            vec!["wait", "--in", "5m", "--prompt-file", "note.txt"],
            "unexpected argument",
        ),
        (
            vec!["wait", "--signal", "deploy.failed"],
            "unexpected argument",
        ),
        (
            vec!["wait", "--in", "5m", "--match", "branch=feature"],
            "unexpected argument",
        ),
        (
            vec!["wait", "--wait=5s", "--run", "true"],
            "unexpected argument",
        ),
        (
            vec!["wait", "--", "cargo", "test"],
            "unrecognized subcommand",
        ),
    ] {
        let output = agent_wait(&env).args(&args).output().unwrap();
        assert!(
            !output.status.success(),
            "accepted removed arguments: {args:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(refusal), "{args:?}: {stderr}");
    }
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
    let output = agent_wait(env).arg("wait").args(args).output().unwrap();
    assert!(!output.status.success(), "accepted {args:?}");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn wait_at_after_reset_and_when_arm_one_shot_self_waits_in_the_arming_checkout() {
    let env = Env::new();
    env.record(&env.project_root);
    let Some(linked) = linked_worktree(&env) else {
        return;
    };
    register_calling_agent(&env);
    rest_calling_agent(&env);
    publish_claude_5h(&env, 30);
    let mut names = Vec::new();
    for (args, kind, label) in [
        (&["--at", "23:59"][..], "schedule", "clock"),
        (&["--after-reset", "5h"][..], "schedule", "reset"),
        (
            &["--when", "pr=merged", "--when", "ci=passed"][..],
            "condition",
            "state",
        ),
    ] {
        let output = agent_wait(&env)
            .current_dir(&linked)
            .arg("wait")
            .args(args)
            .args(["--label", label, "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let name = receipt["name"].as_str().unwrap().to_owned();
        assert!(name.starts_with("wait-"), "{receipt}");
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
            "provider-session"
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
        assert!(entry.every.is_none() && entry.cron.is_none() && entry.at.is_none());
        assert!(entry.signal.is_none() && entry.hold.is_none() && entry.prompt.is_none());
        if kind == "schedule" {
            assert!(entry.when.is_none(), "{entry:?}");
            let fire_at = entry.fire_at.expect("a clock wait stores its instant");
            assert!(fire_at > jiff::Timestamp::now(), "{fire_at}");
        } else {
            assert!(entry.fire_at.is_none(), "{entry:?}");
            assert_eq!(entry.once, Some(true));
            assert_eq!(
                entry.when.as_deref(),
                Some(&["pr=merged && ci=passed".to_owned()][..])
            );
            assert_eq!(row["trigger"], "when pr=merged && ci=passed");
        }
        names.push(name);
    }
    let reset = wait_instances(&env).0[&names[1]].fire_at.unwrap();
    assert!(
        reset > jiff::Timestamp::now() + jiff::SignedDuration::from_mins(90),
        "--after-reset fires at the window's reset, about 2h ahead: {reset}"
    );
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
    let listed = wait_ok(&env, &["wait", "list"]);
    for name in &names {
        assert!(listed.contains(name.as_str()), "{listed}");
    }
    wait_ok(&env, &["wait", "cancel", "--all"]);
    assert!(wait_instances(&env).0.is_empty());
}

#[test]
fn wait_receipts_name_the_clock_time_the_window_and_the_condition_reading() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let first_lines = |args: &[&str]| -> Vec<String> {
        let mut argv = vec!["wait"];
        argv.extend_from_slice(args);
        wait_ok(&env, &argv)
            .lines()
            .take(2)
            .map(ToOwned::to_owned)
            .collect()
    };
    let at = first_lines(&["--at", "23:59"]);
    assert!(
        at[0].starts_with("armed wait-") && at[0].contains(": once at "),
        "{at:?}"
    );
    assert!(at[0].ends_with(" 23:59 → @planner#main"), "{at:?}");
    publish_claude_5h(&env, 30);
    let reset = first_lines(&["--after-reset", "5h"]);
    assert!(
        reset[0].starts_with("armed wait-") && reset[0].contains(": once at "),
        "{reset:?}"
    );
    assert!(
        reset[1].starts_with("  window: claude 5h · 70% left · resets ")
            && reset[1].ends_with(" · read from the room's current claude account"),
        "{reset:?}"
    );
    publish_claude_5h_resetting(&env, 0, jiff::SignedDuration::from_hours(5));
    let fresh = first_lines(&["--after-reset", "5h"]);
    assert_eq!(
        fresh[1],
        "  window: claude 5h · not started · 100% left · fires on the next scheduler tick · read \
         from the room's current claude account"
    );
    let forge = first_lines(&["--when", "pr=merged"]);
    assert!(
        forge[0].starts_with("armed wait-")
            && forge[0].ends_with(": when pr=merged → @planner#main"),
        "{forge:?}"
    );
    assert_eq!(
        forge[1],
        "  now: unknown · pr: unknown · pr and ci are read by the room's sidebar, which is not \
         running"
    );
    publish_claude_5h(&env, 92);
    let window = first_lines(&["--when", "window.5h.left >= 40"]);
    assert!(
        window[0].ends_with(": when window.5h.left>=40 → @planner#main"),
        "{window:?}"
    );
    assert_eq!(window[1], "  now: waiting · window.5h.left: 8");
}

#[test]
fn wait_when_twice_names_the_existing_wait_while_other_triggers_arm_again() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let arm = |args: &[&str]| -> serde_json::Value {
        let mut argv = vec!["wait", "--json"];
        argv.extend_from_slice(args);
        serde_json::from_str(&wait_ok(&env, &argv)).unwrap()
    };
    let first = arm(&["--when", "pr=merged"]);
    let again = arm(&["--when", "pr=merged"]);
    assert_eq!(again["name"], first["name"], "{again}");
    assert_eq!(again["existing"], true, "{again}");
    assert_eq!(again["trigger"], "when pr=merged");
    assert_eq!(again["pending"].as_array().unwrap().len(), 1, "{again}");
    let human = wait_ok(&env, &["wait", "--when", "pr=merged"]);
    assert_eq!(
        human.lines().next().unwrap(),
        format!(
            "already waiting {}: when pr=merged → @planner#main",
            first["name"].as_str().unwrap()
        )
    );
    assert_eq!(wait_instances(&env).0.len(), 1);
    let other = arm(&["--when", "ci=passed"]);
    assert_ne!(other["name"], first["name"]);
    assert!(other.get("existing").is_none(), "{other}");
    let at = arm(&["--at", "23:59"]);
    let at_again = arm(&["--at", "23:59"]);
    assert_ne!(at["name"], at_again["name"]);
    assert_eq!(wait_instances(&env).0.len(), 4);
}

const LOOP_ADD_REBASE: [&str; 7] = [
    "loop",
    "add",
    "rebase",
    "--wait",
    "@me",
    "--when",
    "pr=merged",
];

#[test]
fn loop_subscription_after_a_self_wait_on_its_condition_is_added() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    wait_ok(&env, &["wait", "--when", "pr=merged"]);
    let added = wait_ok(&env, &LOOP_ADD_REBASE);
    assert!(added.contains("added loop task `rebase`"), "{added}");
    assert_eq!(wait_instances(&env).0.len(), 2);
    let again = wait_ok(
        &env,
        &[
            "loop",
            "add",
            "rebase-2",
            "--wait",
            "@me",
            "--when",
            "pr=merged",
        ],
    );
    assert!(again.contains("already subscribed as "), "{again}");
    assert!(!again.contains("already subscribed as wait-"), "{again}");
    assert_eq!(wait_instances(&env).0.len(), 2);
}

#[test]
fn self_wait_after_a_loop_subscription_on_its_condition_is_armed() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    wait_ok(&env, &LOOP_ADD_REBASE);
    let armed = wait_ok(&env, &["wait", "--when", "pr=merged"]);
    assert!(armed.starts_with("armed wait-"), "{armed}");
    assert_eq!(wait_instances(&env).0.len(), 2);
}

/// Arms `--<trigger>` with a label through `command`, publishing the window it reads, then
/// fires it the way the scheduler would.
fn arm_and_fire(env: &Env, trigger: &str, command: impl Fn() -> std::process::Command) -> String {
    let args: &[&str] = match trigger {
        "at" => &["--at", "23:59"],
        "after-reset" => {
            publish_claude_5h_resetting(env, 0, jiff::SignedDuration::from_hours(5));
            &["--after-reset", "5h"]
        }
        _ => {
            publish_claude_5h(env, 10);
            &["--when", "window.5h.left >= 40"]
        }
    };
    let output = command()
        .arg("wait")
        .args(args)
        .args(["--label", &format!("wake {trigger}"), "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{trigger}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let name = receipt["name"].as_str().unwrap().to_owned();
    let passes: &[&[&str]] = match trigger {
        "at" => &[&["loop", "run", &name]],
        "after-reset" => &[&["loop", "tick"]],
        _ => &[&["loop", "tick"], &["loop", "tick"]],
    };
    for (index, pass) in passes.iter().enumerate() {
        let output = command().args(*pass).output().unwrap();
        assert!(
            output.status.success(),
            "{pass:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if trigger == "when" && index == 0 {
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
fn new_self_waits_steer_a_wait_carrying_label_clock_and_readings() {
    for trigger in ["at", "after-reset", "when"] {
        let env = Env::new();
        env.record(&env.project_root);
        env.install_agent_hooks("claude");
        register_calling_agent(&env);
        rest_calling_agent(&env);
        let name = arm_and_fire(&env, trigger, || agent_wait(&env));
        let messages = wait_for_wait_messages(&env, 1);
        assert_eq!(messages.len(), 1, "{trigger}");
        let wake = &messages[0];
        assert_eq!(wake.agent_id.as_str(), "provider-session");
        assert_eq!(wake.gate, DeliveryGate::Any, "{trigger}");
        assert_eq!(
            wake.sender,
            MessageSender::Harness {
                notice: HarnessNotice::Wait
            }
        );
        let lines = wake.text.lines().collect::<Vec<_>>();
        if trigger == "when" {
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
        } else {
            assert!(
                lines[0].starts_with("waited until ")
                    && lines[0].ends_with(&format!(" · wake {trigger} [{name}]")),
                "{}",
                wake.text
            );
        }
        wait_for_no_wait_instances(&env);
    }
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
    let receipt = wait_ok(&env, &["wait", "--when", "pr=merged", "--label", "landed"]);
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

#[test]
fn new_self_waits_park_with_any_gate_at_an_open_prompt() {
    for trigger in ["at", "after-reset", "when"] {
        let env = Env::new();
        let pane_fixture = register_waiting_caller(&env);
        arm_and_fire(&env, trigger, || traced_wait(&env, &pane_fixture));
        wait_for_wait_records(&env, 1);
        let messages = wait_for_wait_messages(&env, 1);
        assert_eq!(messages.len(), 1, "{trigger}");
        let wake = &messages[0];
        assert_eq!(wake.status, MessageStatus::Queued, "{trigger}");
        assert_eq!(wake.gate, DeliveryGate::Any, "{trigger}");
        assert_eq!(
            wake.sender,
            MessageSender::Harness {
                notice: HarnessNotice::Wait
            }
        );
        assert_eq!(
            wake.last_error.as_deref(),
            Some("agent is waiting on input in its pane"),
            "{trigger}"
        );
        assert_no_wait_paste(&env);
        wait_for_no_wait_instances(&env);
    }
}

#[test]
fn new_wait_triggers_refuse_in_words_true_of_a_wait() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    for (args, expected) in [
        (
            &["--at", "nope"][..],
            "--at `nope` is not a time; use `HH:MM`, `YYYY-MM-DD HH:MM`, or `<weekday> HH:MM`",
        ),
        (
            &["--at", "2020-01-01 09:00"][..],
            "--at `2020-01-01 09:00` is in the past; choose a later time",
        ),
        (
            &["--after-reset", "3h"][..],
            "unknown window span `3h`; use 5h or 7d",
        ),
        (
            &["--after-reset", "5h"][..],
            "no current claude 5h window reading; open the room's sidebar or run `rimz accounts \
             --refresh`",
        ),
        (
            &["--when", "window.5h.left >= 40"][..],
            "no current claude 5h window reading; open the room's sidebar or run `rimz accounts \
             --refresh`",
        ),
        (&["--when", "pr=="][..], "--when: clause 1, position "),
        (
            &["--when", "team.stage=Bogus"][..],
            "no team defines stage Bogus; stages: Done",
        ),
    ] {
        let stderr = wait_refusal(&env, args);
        assert!(stderr.contains(expected), "{args:?}: {stderr}");
        for wrong in ["schedule `", "task", "--check-only", "--agent", "--surplus"] {
            assert!(!stderr.contains(wrong), "{args:?} names {wrong}: {stderr}");
        }
    }
    let stage = wait_refusal(&env, &["--when", "team.stage=Bogus"]);
    assert!(stage.contains("--when: clause 1, position "), "{stage}");
    assert!(!loop_instances_path(&env).exists());
}

#[test]
fn new_wait_triggers_take_exactly_one_trigger_and_no_watch_flags() {
    let env = Env::new();
    env.record(&env.project_root);
    register_calling_agent(&env);
    let new: [&[&str]; 3] = [
        &["--at", "09:00"],
        &["--after-reset", "5h"],
        &["--when", "pr=merged"],
    ];
    let old: [&[&str]; 5] = [
        &["--in", "5m"],
        &["--pid", "123"],
        &["--check", "true"],
        &["--file", "app.log"],
        &["--run", "true"],
    ];
    for (index, trigger) in new.iter().enumerate() {
        for other in old.iter().chain(new.iter().skip(index + 1)) {
            let args = [*trigger, *other].concat();
            let stderr = wait_refusal(&env, &args);
            assert!(
                stderr.contains(
                    "choose exactly one wait trigger: --in, --at, --after-reset, --when, --pid, \
                     --check, --file, or --run"
                ),
                "{args:?}: {stderr}"
            );
        }
        for (flag, expected) in [
            (
                &["--check-in", "1m"][..],
                "--check-in requires --pid, --check, --file, or --run",
            ),
            (&["--on", "success"][..], "--on requires --check or --run"),
            (&["--every", "5s"][..], "--every requires --check"),
            (&["--grep", "ready"][..], "--grep requires --file"),
        ] {
            let args = [*trigger, flag].concat();
            let stderr = wait_refusal(&env, &args);
            assert!(stderr.contains(expected), "{args:?}: {stderr}");
        }
    }
    assert!(!loop_instances_path(&env).exists());
    assert_eq!(wait_ok(&env, &["wait"]), wait_ok(&env, &["wait", "list"]));
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
            "timed out: {description}; pending messages: {:?}; run records: {:?}",
            env.store().list_pending_messages(),
            wait_records(env)
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
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

/// A delivering `rimz loop run` appends its terminal record before it releases
/// its run lock, and `loop show` reads that held lock with no row as a run in
/// flight, so a read after the record waits for the runner to exit.
#[cfg(target_os = "linux")]
fn wait_for_loop_runs_to_exit(env: &Env, task: &str) {
    let spec = rimz::testkit::sandbox::SandboxSpec {
        home_root: env.home_root.clone(),
        runtime_root: env.runtime_root.clone(),
    };
    wait_until(env, &format!("`rimz loop run {task}` did not exit"), || {
        !rimz::testkit::sandbox::sandbox_processes(&spec)
            .into_iter()
            .filter_map(rimz::proc::argv)
            .any(|argv| {
                ["loop", "run", task]
                    .iter()
                    .all(|word| argv.iter().any(|arg| arg == word))
            })
    });
}

fn loop_runs_path(env: &Env) -> std::path::PathBuf {
    env.rimz_home().join("logs/loop-runs.log.jsonl")
}

fn wait_records(env: &Env) -> Vec<rimz::harness::schedule::run_log::LoopRunRecord> {
    let path = loop_runs_path(env);
    std::fs::read_to_string(path)
        .unwrap_or_default()
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
