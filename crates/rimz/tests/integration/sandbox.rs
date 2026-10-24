//! Agent mount-view coverage at the real exec boundary.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use rimz::agents::ManualSkill;
use rimz::config::Isolation;
use rimz::harness::launch::ExecRequest;
use rimz::ids::AgentKind;
use rimz::sandbox::{SandboxErr, SandboxInputs, SandboxPlan, SkillInputs, SkipReason};

use crate::common::{
    CommandTimeoutExt, Env, exec_args, path_with_front, write_env_dump_shim, write_fake_login_shell,
};

fn available() -> bool {
    match rimz::sandbox::preflight(Isolation::Scoped) {
        Ok(_) => true,
        Err(err) => {
            crate::common::skip(&format!("bubblewrap unusable: {err}"));
            false
        }
    }
}

fn enable(env: &Env) {
    let path = env.rimz_home().join("config.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "[agents]\nisolation = \"scoped\"\n").unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn scoped_exec_guard_restores_and_records_replacements() {
    scoped_exec_guard_replacement(false);
}

#[test]
#[cfg(target_os = "linux")]
fn scoped_exec_guard_records_helper_spawn_failure_in_doctor() {
    scoped_exec_guard_replacement(true);
}

#[cfg(target_os = "linux")]
fn scoped_exec_guard_replacement(fail_helper: bool) {
    use std::time::{Duration, Instant};

    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    enable(&env);
    env.install_agent_hooks("claude");
    crate::common::trust_claude_project(&env, &env.project_root);
    let path = env.home_root.join(".gitconfig");
    std::fs::write(&path, "original\n").unwrap();
    let bin = crate::common::write_long_lived_agent_shim(&env, "claude");
    crate::common::write_path_shim(
        &bin,
        "claude",
        r#"
if [ "$1" = --version ]; then echo 'claude 9.0.0'; exit 0; fi
printf ready > "$RIMZ_TEST_GUARD_READY"
while [ ! -f "$RIMZ_TEST_GUARD_GO" ]; do sleep 0.02; done
if (printf agent >> "$HOME/.gitconfig") 2>/dev/null; then result=writable; else result=readonly; fi
printf '%s' "$result" > "$RIMZ_TEST_GUARD_RESULT"
while [ ! -f "$RIMZ_TEST_GUARD_EXIT" ]; do sleep 0.02; done
"#,
    );
    let ready = env.project_root.join("guard-ready");
    let go = env.project_root.join("guard-go");
    let result = env.project_root.join("guard-result");
    let exit = env.project_root.join("guard-exit");
    let helper_dir = tempfile::tempdir_in(env.rimz_bin().parent().unwrap()).unwrap();
    let helper = helper_dir.path().join("rimz");
    std::fs::hard_link(env.rimz_bin(), &helper).unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.name = Some("guard-worker".into());
    let mut wrapper = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&bin))
        .env("SHELL", write_fake_login_shell(&env, "guard-shell", &[]))
        .env("RIMZ_BIN", &helper)
        .env("RIMZ_TEST_GUARD_READY", &ready)
        .env("RIMZ_TEST_GUARD_GO", &go)
        .env("RIMZ_TEST_GUARD_RESULT", &result)
        .env("RIMZ_TEST_GUARD_EXIT", &exit)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists()
        || !rimz::proc::argv(wrapper.id())
            .is_some_and(|argv| argv.iter().any(|arg| arg == "--supervise"))
    {
        assert!(
            wrapper.try_wait().unwrap().is_none(),
            "wrapper exited before park"
        );
        assert!(Instant::now() < deadline, "wrapper never entered park");
        std::thread::sleep(Duration::from_millis(10));
    }
    if fail_helper {
        std::fs::remove_file(&helper).unwrap();
    }
    let sink = rimz::diag::DiagSink::under(
        env.store().paths().root.clone(),
        env.workspace_id.clone(),
        "test",
        None,
    );
    let kind = if fail_helper {
        "scoped_bind_restore_failed"
    } else {
        "scoped_bind_restored"
    };
    std::fs::write(path.with_extension("lock"), "replacement\n").unwrap();
    std::fs::rename(path.with_extension("lock"), &path).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let event = loop {
        let log = std::fs::read_to_string(sink.log_path().unwrap()).unwrap_or_default();
        if let Some(event) = log
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .map(|record| record["event"].clone())
            .find(|event| event["kind"] == kind)
        {
            break event;
        }
        assert!(
            Instant::now() < deadline,
            "protected replacement must produce {kind}: {log}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(event["agent_kind"], "claude");
    assert_eq!(event["agent_name"], "guard-worker");
    assert_eq!(event["paths"], serde_json::json!([path]));
    if fail_helper {
        assert_eq!(
            event["error"],
            "spawning scoped bind helper: No such file or directory (os error 2)"
        );
        assert!(!event["error"].as_str().unwrap().contains("timed out"));
    }
    std::fs::write(&go, "go").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let observed = loop {
        if let Ok(value) = std::fs::read_to_string(&result)
            && !value.is_empty()
        {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "provider never attempted its write"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(observed, if fail_helper { "writable" } else { "readonly" });
    std::fs::write(&path, "host write\n").unwrap();
    let output = env.rimz().args(["doctor", "--json"]).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let row = report["diagnostics"]["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == kind)
        .expect("doctor must list the guard outcome");
    assert_eq!(
        row["state"],
        if fail_helper {
            "investigate"
        } else {
            "recovered"
        }
    );
    std::fs::write(exit, "exit").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = wrapper.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "wrapper must exit with its provider"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn scoped_exec_guard_arm_refusal_names_the_limit_without_spawning() {
    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    enable(&env);
    let bin = crate::common::write_long_lived_agent_shim(&env, "claude");
    crate::common::write_path_shim(
        &bin,
        "claude",
        "[ \"$1\" = --version ] && exit 1; printf spawned > \"$HOME/provider-spawned\"",
    );
    std::fs::write(env.home_root.join(".gitconfig"), "original").unwrap();
    for (errno, reason) in [
        (
            24,
            "scoped protected-file guard: Too many open files (os error 24); raise fs.inotify.max_user_instances",
        ),
        (
            28,
            "scoped protected-file guard: No space left on device (os error 28); raise fs.inotify.max_user_watches",
        ),
    ] {
        let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
        let output = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("PATH", path_with_front(&bin))
            .env("SHELL", write_fake_login_shell(&env, "guard-shell", &[]))
            .env("RIMZ_TEST_SCOPED_BIND_ARM_ERRNO", errno.to_string())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "unarmed scoped launch must refuse: {stderr}"
        );
        assert_eq!(stderr.lines().last().unwrap(), format!("error: {reason}"));
        assert!(
            !stderr.contains("running bwrap") && !stderr.contains("timed out"),
            "{stderr}"
        );
        assert!(!env.home_root.join("provider-spawned").exists());
    }
}

#[test]
fn provider_sandbox_admits_every_provider_without_a_view() {
    for kind in rimz::agents::known_kinds() {
        assert_eq!(
            rimz::sandbox::preflight_launch(
                Isolation::Sandbox,
                &AgentKind::new_unchecked(kind),
                true,
                ManualSkill::Unsupported,
            )
            .unwrap(),
            None,
            "{kind}"
        );
    }
}

#[test]
fn provider_sandbox_claude_launches_without_host_sandbox_tools() {
    let env = Env::new();
    env.record(&env.project_root);
    let shims = env.home_root.join("native-sandbox-bin");
    std::fs::create_dir(&shims).unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params.isolation = Some(Isolation::Sandbox);
    crate::common::write_path_shim(
        &shims,
        "claude",
        "printf '%s\\n' \"$@\" > \"$RIMZ_TEST_AGENT_ENV_DUMP\"",
    );
    let probe = env.home_root.join("provider-argv");
    let output = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("PATH", &shims)
        .env("SHELL", write_fake_login_shell(&env, "native-shell", &[]))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let dump = std::fs::read_to_string(probe).unwrap();
    let argv = dump.lines().collect::<Vec<_>>();
    let settings = argv
        .windows(2)
        .find(|pair| pair[0] == "--settings")
        .unwrap()[1];
    let settings: serde_json::Value = serde_json::from_str(settings).unwrap();
    let sandbox = &settings["sandbox"];
    assert_eq!(sandbox["enabled"], true);
    assert_eq!(sandbox.as_object().unwrap().len(), 3);
    assert!(
        sandbox["filesystem"]["allowWrite"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(env.store().paths().root))
    );
    assert!(
        sandbox["excludedCommands"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("rimz message *"))
    );
}

#[test]
fn provider_sandbox_config_set_does_not_probe() {
    let env = Env::new();
    let shims = env.home_root.join("no-sandbox-tools");
    std::fs::create_dir(&shims).unwrap();
    let output = env
        .rimz()
        .args(["config", "set", "agents.isolation", "sandbox"])
        .env("PATH", &shims)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        std::fs::read_to_string(env.rimz_home().join("config.toml"))
            .unwrap()
            .contains("sandbox")
    );
}

#[cfg(target_os = "linux")]
fn drainer_lease_command(env: &Env) -> std::process::Command {
    env.record(&env.project_root);
    enable(env);
    let shims = env.home_root.join("lease-bin");
    crate::common::write_path_shim(
        &shims,
        "bwrap",
        "while [ \"$1\" != -- ]; do shift; done; shift; exec \"$@\"",
    );
    crate::common::write_path_shim(
        &shims,
        "claude",
        "case \"$1\" in --version) echo 'claude 9.0.0'; exit 0;; auth) exit 0;; esac; printf ready > \"$RIMZ_TEST_PROVIDER_READY\"; while [ ! -f \"$RIMZ_TEST_PROVIDER_EXIT\" ]; do /usr/bin/sleep 0.02; done",
    );
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    let mut command = env.rimz();
    command
        .args(exec_args(env, &request))
        .env("PATH", path_with_front(&shims))
        .env("SHELL", write_fake_login_shell(env, "lease-shell", &[]))
        .env(
            "RIMZ_TEST_PROVIDER_READY",
            env.project_root.join("provider-ready"),
        )
        .env(
            "RIMZ_TEST_PROVIDER_EXIT",
            env.project_root.join("provider-exit"),
        )
        .env("RIMZ_TEST_HOOK_DRAIN_IDLE_MS", "100");
    command
}

#[test]
#[cfg(target_os = "linux")]
fn sandbox_wrapper_keeps_and_reconnects_the_host_drainer_until_provider_exit() {
    let env = Env::new();
    let ready = env.project_root.join("provider-ready");
    let exit = env.project_root.join("provider-exit");
    let mut wrapper = drainer_lease_command(&env).spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ready.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "provider did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let socket = env.store().runtime_paths().hook_drainer_socket_path();
    assert!(
        socket.exists(),
        "the host wrapper must establish a lease before starting the sandbox provider"
    );
    std::thread::sleep(std::time::Duration::from_millis(400));
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "a quiet provider must keep the drainer past its idle deadline"
    );
    let stream = std::os::unix::net::UnixStream::connect(&socket).unwrap();
    let pid = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)
        .unwrap()
        .pid();
    drop(stream);
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(&socket)
            && nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)
                .unwrap()
                .pid()
                != pid
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the wrapper must replace a dead drainer"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::fs::write(exit, "exit").unwrap();
    assert!(wrapper.wait().unwrap().success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while socket.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the exited wrapper must release the lease"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
#[cfg(target_os = "linux")]
fn sandbox_wrapper_holds_the_same_drainer_across_the_park_handoff() {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::{Duration, Instant};

    let env = Env::new();
    let mut command = drainer_lease_command(&env);
    let barrier = env.runtime_paths().sock_dir.join("lease-handoff.sock");
    let listener = UnixListener::bind(&barrier).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut wrapper = command
        .env("RIMZ_TEST_DRAINER_LEASE_HANDOVER", &barrier)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut release = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(
            wrapper.try_wait().unwrap().is_none(),
            "wrapper exited before the handoff"
        );
        assert!(
            Instant::now() < deadline,
            "parked image never reached the handoff"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let socket = env.runtime_paths().hook_drainer_socket_path();
    let probe = || {
        let stream = UnixStream::connect(&socket);
        assert!(
            stream.is_ok(),
            "the parked image must inherit a live drainer lease: {stream:?}"
        );
        let mut stream = stream.unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let pid = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)
            .unwrap()
            .pid();
        writeln!(
            stream,
            "{}",
            serde_json::json!({"through":0,"reply_for":null})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str::<rimz::harness::hook_drain::DrainReceipt>(&line).unwrap();
        pid as u32
    };
    let before = probe();
    // The probe is closed before this window: only the wrapper may keep the lease alive.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        rimz::proc::process_is_live(before, None),
        "the inherited lease must keep the same drainer alive while the parked image is held"
    );
    assert_eq!(
        probe(),
        before,
        "the same drainer must still answer requests"
    );
    release.write_all(&[1]).unwrap();
    std::fs::write(env.project_root.join("provider-exit"), "exit").unwrap();
    assert!(wrapper.wait().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(5);
    while rimz::disk::lock::WorkspaceLock::try_acquire(&env.runtime_paths().hook_drainer_lock())
        .unwrap()
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "provider exit must release the lease"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !socket.exists(),
        "the released drainer must unlink its socket"
    );
}

fn child_cap_launch(explicit_host: bool) {
    use rimz::agents::LaunchParams;
    use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

    if !explicit_host && !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    crate::common::write_definition(
        &env,
        "subagents",
        "sysadmin",
        "description: Host worker\nagent: claude\ntools: []\nisolation: host\nmode: yolo",
        "Work.",
    );
    env.install_agent_hooks("claude");
    let store = env.store();
    // The caller's `/tmp` is its own temp unit.
    let cwd = store.paths().temp_unit_dir(Some("parent")).join("clean");
    std::fs::create_dir_all(&cwd).unwrap();
    crate::common::trust_claude_project(&env, &cwd);
    let workspace = env.resolve_workspace(&env.project_root);
    store
        .append_event(&EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "parent-session".into(),
                launch_id: Some("parent-launch".into()),
                agent_name: "parent".to_owned(),
                agent_name_explicit: true,
                launch: LaunchParams::default(),
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: Some(rimz::ids::PaneId::from_parts(
                    rimz::ids::MuxName::Zellij,
                    "terminal_2",
                )),
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    crate::common::room::seed_live_zellij_room(
        store.runtime_paths(),
        &workspace.session_name,
        serde_json::from_value(serde_json::json!([
            {"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},
            {"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}
        ]))
        .unwrap(),
    );
    let shim = crate::common::write_failing_agent_shim(&env, "claude", 1);
    let shell = write_fake_login_shell(&env, "rimz-test-sh", &[]);
    let presence = env.project_root.join("presence.wasm");
    std::fs::write(&presence, b"test-presence").unwrap();
    let mut command = env.rimz();
    command.args(["--mux", "zellij", "subagents", "sysadmin", "work"]);
    if explicit_host {
        command.args(["--isolation", "host"]);
    } else {
        command.args(["--cwd", "/tmp/clean"]);
    }
    command
        .env("RIMZ_ISOLATION", "scoped")
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "parent-launch")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "parent")
        .env("SHELL", shell)
        .env("PATH", path_with_front(&shim))
        .env(
            "RIMZ_ZELLIJ_BIN",
            crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
        )
        .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("mux.log"))
        .env("RIMZ_PRESENCE_PLUGIN", presence)
        .env("ZELLIJ_PANE_ID", "2")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            crate::common::room::MATERIALIZED_ROOM_PANES,
        );
    let launched = std::sync::atomic::AtomicBool::new(false);
    let trace = env.project_root.join("mux.log");
    let output = std::thread::scope(|scope| {
        scope.spawn(|| {
            if explicit_host {
                return; // A scoped parent cannot launch a host child.
            }
            crate::common::room::bind_child_panes(
                &store,
                &trace,
                &workspace.session_name,
                &launched,
            );
        });
        let output = command.bounded_output();
        launched.store(true, std::sync::atomic::Ordering::Relaxed);
        output
    })
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if explicit_host {
        assert!(!output.status.success(), "{stderr}");
        assert!(
            stderr.contains("launch it from a host agent or a host shell"),
            "{stderr}"
        );
        assert_eq!(store.read_events().unwrap().len(), 1);
    } else {
        assert!(output.status.success(), "{stderr}");
        assert!(
            stderr.contains("sysadmin runs scoped: its definition asks for host isolation"),
            "{stderr}"
        );
        assert!(stderr.contains("sysadmin runs in auto mode: its definition asks for yolo, and a subagent never runs looser than its parent"), "{stderr}");
        let agents = store.snapshot().unwrap().agents;
        let child = agents
            .iter()
            .find(|agent| agent.profile.as_deref() == Some("sysadmin"))
            .unwrap();
        assert_eq!(child.isolation, Some(Isolation::Scoped));
        assert_eq!(child.mode, Some(rimz::agents::PermissionMode::Auto));
        let runs = rimz::harness::run::list(store.paths()).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].permission_mode, rimz::agents::PermissionMode::Auto);
        assert_eq!(child.worktree_path.as_deref(), cwd.to_str());
        let trace = std::fs::read_to_string(env.project_root.join("mux.log")).unwrap();
        assert!(
            trace.contains(&format!("\t--cwd\t{}\t", cwd.display())),
            "{trace}"
        );
    }
}

#[test]
fn scoped_parent_refuses_explicit_host_subagent_without_launch_event() {
    child_cap_launch(true);
}

#[test]
fn scoped_parent_clamps_host_profile_subagent_and_records_override() {
    child_cap_launch(false);
}

fn permission_cap_command(env: &Env, mode: rimz::agents::PermissionMode) -> std::process::Command {
    use rimz::agents::LaunchParams;
    use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

    env.record(&env.project_root);
    env.install_agent_hooks("claude");
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    store
        .append_event(&EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "parent-session".into(),
                launch_id: Some("parent-launch".into()),
                agent_name: "parent".into(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    mode: Some(mode),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: Some(rimz::ids::PaneId::from_parts(
                    rimz::ids::MuxName::Zellij,
                    "terminal_2",
                )),
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    crate::common::room::seed_live_zellij_room(
        store.runtime_paths(),
        &workspace.session_name,
        serde_json::from_value(serde_json::json!([
            {"id":1,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar"},
            {"id":2,"is_plugin":false,"tab_id":1,"title":"sh"}
        ]))
        .unwrap(),
    );
    let mut command = env.rimz();
    command
        .args(["--mux", "zellij"])
        .env("RIMZ_ISOLATION", "host")
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_ID, "parent-launch")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "parent")
        .env(
            "RIMZ_ZELLIJ_BIN",
            crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
        )
        .env("RIMZ_TEST_ZELLIJ_LOG", env.project_root.join("mux.log"))
        .env("ZELLIJ_PANE_ID", "2")
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
            format!("{} [Created 1s ago]\n", workspace.session_name),
        )
        .env(
            "RIMZ_TEST_ZELLIJ_LIST_PANES",
            crate::common::room::MATERIALIZED_ROOM_PANES,
        );
    command
}

#[test]
fn auto_parent_refuses_explicit_yolo_subagent_without_launch_event() {
    let env = Env::new();
    let mut command = permission_cap_command(&env, rimz::agents::PermissionMode::Auto);
    let store = env.store();
    let before = store.read_events().unwrap().len();
    let output = command
        .args(["subagents", "claude", "work", "--yolo"])
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    for text in [
        "requested yolo mode",
        "parent",
        "auto",
        "a subagent never runs looser than its parent",
        "--yolo",
        "mode: yolo",
    ] {
        assert!(stderr.contains(text), "{stderr}");
    }
    assert_eq!(store.read_events().unwrap().len(), before);
}

#[test]
fn parent_message_refuses_to_resume_a_child_above_its_current_mode() {
    use rimz::agents::{AgentLifecycleObservation, LaunchParams, LifecycleSignal, PermissionMode};
    use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};

    let env = Env::new();
    let mut command = permission_cap_command(&env, PermissionMode::Ask);
    let store = env.store();
    let workspace = env.resolve_workspace(&env.project_root);
    store
        .append_event(&EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "child-session".into(),
                launch_id: Some("child-launch".into()),
                agent_name: "otter".into(),
                agent_name_explicit: true,
                launch: LaunchParams {
                    mode: Some(PermissionMode::Yolo),
                    parent_agent_id: Some("parent-session".into()),
                    parent_agent_kind: Some(AgentKind::new_unchecked("claude")),
                    launch_depth: Some(1),
                    ..Default::default()
                },
                state: AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    store
        .append_event(&EventEnvelope::agent_lifecycle(
            env.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "test-ended",
            &AgentLifecycleObservation::new(Some("child-session".into()), LifecycleSignal::Ended),
        ))
        .unwrap();
    let before = store.read_events().unwrap().len();
    let output = command
        .args(["message", "@otter", "Follow up."])
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    for text in ["cannot resume @otter", "yolo", "ask", "launch a new child"] {
        assert!(stderr.contains(text), "{stderr}");
    }
    assert!(
        store.read_events().unwrap()[before..]
            .iter()
            .all(|event| event.method != "agent.launched")
    );
}

fn environment(env: &Env) -> BTreeMap<String, String> {
    [
        ("HOME", env.home_root.clone()),
        ("XDG_CONFIG_HOME", env.config_root()),
        ("XDG_RUNTIME_DIR", env.runtime_root.clone()),
        ("RIMZ_HOME", env.rimz_home()),
        ("XDG_STATE_HOME", env.state_root()),
        ("TMPDIR", "/tmp".into()),
    ]
    .into_iter()
    .map(|(key, path)| (key.to_owned(), path.display().to_string()))
    .collect()
}

fn ssh_config_plan(env: &Env, source: &Path) -> SandboxPlan {
    let state = env.store();
    rimz::sandbox::plan(&SandboxInputs {
        env: &environment(env),
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        checkout: None,
        read_only: vec![env.home_root.clone()],
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: source,
        ssh_config_copies_dir: &state.paths().ssh_config_copies_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills: SkillInputs {
            kind: "claude",
            home: None,
            manual: ManualSkill::Frontmatter,
            callable: None,
        },
    })
    .unwrap()
}

#[test]
fn sandbox_ssh_config_captures_regular_targets_without_writes() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let env = Env::new();
    let state = env.store();
    let real = env.home_root.join("system-ssh");
    std::fs::create_dir(&real).unwrap();
    let source = env.home_root.join("ssh_config");
    let root = env.home_root.join("system-ssh.conf");
    symlink(&root, &source).unwrap();
    std::fs::write(
        &root,
        "# root\r\ninclude=\"system-ssh/*.conf\" missing.conf\nInclude ~/.ssh/config\nInclude absent-*.conf",
    )
    .unwrap();
    let regular = real.join("10-regular.conf");
    let nested_dir = env.home_root.join("nested config");
    std::fs::create_dir(&nested_dir).unwrap();
    let nested = nested_dir.join("10-regular.conf");
    let regular_text = format!(
        "Host nomatch.invalid\n  INCLUDE \"{}\"\nInclude ssh_config\n",
        nested.display()
    );
    std::fs::write(&regular, &regular_text).unwrap();
    std::fs::set_permissions(&regular, std::fs::Permissions::from_mode(0o664)).unwrap();
    let outside = env.home_root.join("outside.conf");
    let outside_text = "include='relative file.conf'\n";
    std::fs::write(&outside, outside_text).unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&nested, "Include outside.conf\n").unwrap();
    std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o644)).unwrap();
    let relative = env.home_root.join("relative file.conf");
    std::fs::write(&relative, "Include system-ssh/10-regular.conf\r\n").unwrap();
    std::fs::set_permissions(&relative, std::fs::Permissions::from_mode(0o644)).unwrap();
    symlink(&outside, real.join("20-linked.conf")).unwrap();
    symlink(real.join("absent"), real.join("30-dangling.conf")).unwrap();
    std::fs::create_dir(real.join("40-directory.conf")).unwrap();
    nix::unistd::mkfifo(
        &real.join("50-fifo.conf"),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();

    let plan = ssh_config_plan(&env, &source);
    assert_eq!(plan.copies.len(), 1, "the Include graph needs one mirror");
    let copy = &plan.copies[0];
    assert_eq!(copy.source, source);
    assert_eq!(
        copy.target.parent().unwrap(),
        state.paths().ssh_config_copies_dir
    );
    let mirrors: BTreeMap<_, _> = plan
        .plan
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            rimz::sandbox::Mount::RoBind { source, target }
                if source.parent() == Some(copy.target.as_path()) =>
            {
                Some((target.clone(), source.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(mirrors.len(), 1, "only the rewritten root must be bound");
    let root_copy = &mirrors[&root.canonicalize().unwrap()];
    assert_eq!(
        plan.plan.mounts.last(),
        Some(&rimz::sandbox::Mount::RoBind {
            source: root_copy.clone(),
            target: root.canonicalize().unwrap(),
        })
    );
    assert!(!state.paths().ssh_config_copies_dir.exists());
    assert!(!plan.plan.host_bound_paths().contains(&copy.target));
    let repeated = ssh_config_plan(&env, &source);
    assert_eq!(repeated.copies[0].target, copy.target);
    assert_eq!(repeated.plan.mounts, plan.plan.mounts);
    let extra_dangling = real.join("60-new-dangling.conf");
    symlink(real.join("absent"), &extra_dangling).unwrap();
    assert_ne!(ssh_config_plan(&env, &source).copies[0].target, copy.target);
    std::fs::remove_file(extra_dangling).unwrap();
    std::fs::set_permissions(&regular, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(ssh_config_plan(&env, &source).copies[0].target, copy.target);
    std::fs::write(&regular, "Host edited-after-plan\n").unwrap();
    assert_ne!(ssh_config_plan(&env, &source).copies[0].target, copy.target);

    rimz::sandbox::apply(&plan).unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    assert_eq!(std::fs::read_dir(&copy.target).unwrap().count(), 5);
    let root_bytes = std::fs::read_to_string(root_copy).unwrap();
    let root_paths = shlex::split(root_bytes.lines().nth(1).unwrap()).unwrap();
    assert_eq!(root_paths.len(), 6);
    let regular_copy = Path::new(&root_paths[1]);
    let outside_copy = Path::new(&root_paths[2]);
    let nested_copy = shlex::split(
        std::fs::read_to_string(regular_copy)
            .unwrap()
            .lines()
            .nth(1)
            .unwrap(),
    )
    .unwrap()[1]
        .clone();
    let relative_copy =
        shlex::split(std::fs::read_to_string(outside_copy).unwrap().trim_end()).unwrap()[1].clone();
    assert_eq!(
        root_bytes,
        format!(
            "# root\r\nInclude \"{}\" \"{}\" \"{}\" \"{}\" \"{}\"\nInclude \"~/.ssh/config\"\n# Include absent-*.conf",
            regular_copy.display(),
            outside_copy.display(),
            real.join("30-dangling.conf").display(),
            real.join("40-directory.conf").display(),
            real.join("50-fifo.conf").display(),
        )
    );
    for (path, bytes, mode) in [
        (root_copy.as_path(), root_bytes, 0o644),
        (
            regular_copy,
            format!(
                "Host nomatch.invalid\nInclude \"{nested_copy}\"\nInclude \"{}\"\n",
                root_copy.display()
            ),
            0o644,
        ),
        (
            outside_copy,
            format!("Include \"{relative_copy}\"\n"),
            0o600,
        ),
        (
            Path::new(&nested_copy),
            format!("Include \"{}\"\n", outside_copy.display()),
            0o644,
        ),
        (
            Path::new(&relative_copy),
            format!("Include \"{}\"\n", regular_copy.display()),
            0o644,
        ),
    ] {
        assert_eq!(path.parent(), Some(copy.target.as_path()));
        assert!(std::fs::symlink_metadata(path).unwrap().is_file());
        assert_eq!(std::fs::read_to_string(path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            mode
        );
    }

    let empty = env.home_root.join("empty-ssh");
    std::fs::write(&empty, "Host *\n  Port 22\n").unwrap();
    let skipped = env.home_root.join("skipped-ssh");
    std::fs::create_dir(&skipped).unwrap();
    symlink(real.join("absent"), skipped.join("dangling.conf")).unwrap();
    std::fs::create_dir(skipped.join("directory.conf")).unwrap();
    let skipped_root = env.home_root.join("skipped-ssh.conf");
    std::fs::write(
        &skipped_root,
        "Include skipped-ssh/* system-ssh/50-fifo.conf ~/.ssh/config\n",
    )
    .unwrap();
    for source in [env.home_root.join("missing-ssh"), empty, real, skipped_root] {
        let plan = ssh_config_plan(&env, &source);
        assert!(plan.copies.is_empty(), "{}", source.display());
        assert!(!plan.plan.mounts.iter().any(|mount| matches!(mount,
            rimz::sandbox::Mount::RoBind { target, .. } if target == &source
        )));
    }
}

#[test]
fn sandbox_ssh_config_skips_unreadable_sources() {
    use std::os::unix::fs::PermissionsExt;

    if nix::unistd::getuid().is_root() {
        crate::common::skip("mode 000 files remain readable as root");
        return;
    }
    let env = Env::new();
    let source = env.home_root.join("ssh_config");
    std::fs::write(&source, "Include *.conf\n").unwrap();
    let entry = env.home_root.join("unreadable.conf");
    std::fs::write(&entry, "Host unreadable\n").unwrap();
    std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = env.home_root.join("readable.conf");
    std::fs::write(&readable, "Host readable\n").unwrap();
    let plan = ssh_config_plan(&env, &source);
    assert_eq!(
        plan.copies.len(),
        1,
        "readable includes must survive a bad sibling"
    );
    rimz::sandbox::apply(&plan).unwrap();
    assert_eq!(
        std::fs::read_dir(&plan.copies[0].target).unwrap().count(),
        2
    );
    let Some(rimz::sandbox::Mount::RoBind {
        source: root_copy,
        target,
    }) = plan.plan.mounts.last()
    else {
        panic!("missing root mirror");
    };
    assert_eq!(target, &source.canonicalize().unwrap());
    let root_bytes = std::fs::read_to_string(root_copy).unwrap();
    let paths = shlex::split(root_bytes.trim_end()).unwrap();
    assert_eq!(paths.len(), 3);
    let readable_copy = Path::new(&paths[1]);
    assert_eq!(
        readable_copy.parent(),
        Some(plan.copies[0].target.as_path())
    );
    assert_eq!(
        std::fs::read_to_string(readable_copy).unwrap(),
        "Host readable\n"
    );
    assert_eq!(
        root_bytes,
        format!(
            "Include \"{}\" \"{}\"\n",
            readable_copy.display(),
            entry.display()
        )
    );
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
    let plan = ssh_config_plan(&env, &source);
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(plan.copies.is_empty());
}

#[cfg(target_os = "linux")]
fn ssh_config_available() -> bool {
    if !available() {
        return false;
    }
    match std::process::Command::new("ssh").arg("-V").output() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            crate::common::skip("ssh not on PATH");
            return false;
        }
        result => assert!(result.unwrap().status.success()),
    }
    true
}

#[test]
#[cfg(target_os = "linux")]
fn sandbox_ssh_config_accepts_root_owned_host_includes() {
    use std::os::unix::fs::MetadataExt;

    if !ssh_config_available() {
        return;
    }
    let source = Path::new("/etc/ssh/ssh_config");
    let env = Env::new();
    let plan = ssh_config_plan(&env, source);
    let uid = nix::unistd::getuid().as_raw();
    if plan.copies.is_empty() || std::fs::metadata(source).unwrap().uid() == uid {
        crate::common::skip("no system SSH include owned by another uid");
        return;
    }
    rimz::sandbox::apply(&plan).unwrap();
    let entries: Vec<_> = std::fs::read_dir(&plan.copies[0].target)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let mut command = vec![
        "/bin/sh".into(),
        "-c".into(),
        "ssh -G rimz-scoped-ssh.invalid >/dev/null && cat /etc/ssh/ssh_config && printf '\\0' && stat -Lc '%u %a' -- \"$@\"".into(),
        "ssh-config-view".into(),
    ];
    command.extend(entries.iter().map(|path| path.display().to_string()));
    let argv = rimz::sandbox::bwrap_argv(
        &rimz::sandbox::preflight(Isolation::Scoped)
            .unwrap()
            .unwrap(),
        &plan.plan,
        &env.project_root,
        &command,
    );
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let (root_bytes, stats) = stdout.split_once('\0').unwrap();
    let included_copies: Vec<_> = root_bytes
        .lines()
        .filter_map(shlex::split)
        .filter(|args| args.first().is_some_and(|keyword| keyword == "Include"))
        .flat_map(|args| args.into_iter().skip(1))
        .collect();
    assert!(!included_copies.is_empty());
    assert!(
        included_copies
            .iter()
            .all(|path| entries.contains(&PathBuf::from(path)))
    );
    assert_eq!(stats.lines().count(), entries.len());
    for line in stats.lines() {
        let (owner, mode) = line.split_once(' ').unwrap();
        assert_eq!(owner.parse::<u32>().unwrap(), uid);
        assert_eq!(u32::from_str_radix(mode, 8).unwrap() & 0o022, 0);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn sandbox_ssh_config_accepts_nested_root_owned_include_in_inactive_host() {
    use std::os::unix::fs::MetadataExt;

    if !ssh_config_available() {
        return;
    }
    let host_config = Path::new("/etc/ssh/ssh_config");
    if !host_config.is_file()
        || std::fs::metadata(host_config).unwrap().uid() == nix::unistd::getuid().as_raw()
    {
        crate::common::skip("no system SSH include owned by another uid");
        return;
    }
    let env = Env::new();
    let root = env.home_root.join("ssh_config");
    let nested = env.home_root.join("nested.conf");
    std::fs::write(&root, format!("Include {}\n", nested.display())).unwrap();
    std::fs::write(
        &nested,
        "Host nomatch.invalid\n  Include /etc/ssh/ssh_config\n",
    )
    .unwrap();
    let plan = ssh_config_plan(&env, &root);
    rimz::sandbox::apply(&plan).unwrap();
    let argv = rimz::sandbox::bwrap_argv(
        &rimz::sandbox::preflight(Isolation::Scoped)
            .unwrap()
            .unwrap(),
        &plan.plan,
        &env.project_root,
        &[
            "ssh".into(),
            "-G".into(),
            "-F".into(),
            root.display().to_string(),
            "rimz-scoped-ssh.invalid".into(),
        ],
    );
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(target_os = "linux")]
fn sandbox_ssh_config_survives_include_replacement_in_a_running_view() {
    use std::os::unix::fs::{MetadataExt, symlink};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    if !ssh_config_available() {
        return;
    }
    let host_config = Path::new("/etc/ssh/ssh_config");
    if !host_config.is_file()
        || std::fs::metadata(host_config).unwrap().uid() == nix::unistd::getuid().as_raw()
    {
        crate::common::skip("no system SSH include owned by another uid");
        return;
    }
    let env = Env::new();
    let root = env.home_root.join("ssh_config");
    let nested = env.home_root.join("nested.conf");
    let alias = env.home_root.join("included.conf");
    symlink(&nested, &alias).unwrap();
    std::fs::write(&root, format!("Include {}\n", alias.display())).unwrap();
    std::fs::write(&nested, "Host nomatch.invalid\n  User snapshot-user\n").unwrap();
    let plan = ssh_config_plan(&env, &root);
    rimz::sandbox::apply(&plan).unwrap();
    let unit = env.store().paths().temp_unit_dir(None);
    let argv = rimz::sandbox::bwrap_argv(
        &rimz::sandbox::preflight(Isolation::Scoped)
            .unwrap()
            .unwrap(),
        &plan.plan,
        &env.project_root,
        &[
            "/bin/sh".into(),
            "-c".into(),
            ": > /tmp/ssh-ready; while [ ! -e /tmp/ssh-go ]; do sleep 0.01; done; exec ssh -G -F \"$1\" rimz-scoped-ssh.invalid".into(),
            "ssh-config-rename".into(),
            root.display().to_string(),
        ],
    );
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env("HOME", &env.home_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !unit.join("ssh-ready").exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "view exited before ready"
        );
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("view did not signal readiness");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let replacement = nested.with_extension("new");
    symlink(host_config, &replacement).unwrap();
    std::fs::rename(replacement, &nested).unwrap();
    std::fs::write(unit.join("ssh-go"), "").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "ssh exit {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn skill_prepare(
    env: &Env,
    vars: &BTreeMap<String, String>,
    skills: SkillInputs<'_>,
) -> Result<SandboxPlan, SandboxErr> {
    let state = env.store();
    let plan = rimz::sandbox::plan(&SandboxInputs {
        read_only: Vec::new(),
        env: vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        checkout: None,
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills,
    })?;
    rimz::sandbox::apply(&plan)?;
    Ok(plan)
}

fn skill_argv(
    env: &Env,
    vars: &BTreeMap<String, String>,
    skills: SkillInputs<'_>,
) -> Result<Vec<String>, SandboxErr> {
    let prepared = skill_prepare(env, vars, skills)?;
    Ok(rimz::sandbox::bwrap_argv(
        Path::new("/usr/bin/bwrap"),
        &prepared.plan,
        &env.project_root,
        &[],
    ))
}

fn skill_bind_source(argv: &[String], target: &Path) -> PathBuf {
    let args = argv
        .windows(3)
        .find(|args| args[0] == "--ro-bind" && Path::new(&args[2]) == target)
        .unwrap();
    PathBuf::from(&args[1])
}

fn guard_fixture(env: &Env, checkout: &Path) -> Vec<PathBuf> {
    for args in [
        vec!["init"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
        vec![
            "worktree",
            "add",
            "-b",
            "linked",
            checkout.to_str().unwrap(),
        ],
    ] {
        let output = std::process::Command::new("git")
            .env("HOME", &env.home_root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(&env.project_root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut entries = Vec::new();
    for name in [
        ".profile",
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".bash_logout",
        ".zshenv",
        ".zprofile",
        ".zshrc",
        ".zlogin",
        ".zlogout",
        ".config/fish",
        ".gitconfig",
        ".ssh",
        ".config/systemd/user",
    ] {
        let path = env.home_root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if [".config/fish", ".ssh", ".config/systemd/user"].contains(&name) {
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("sentinel"), "original").unwrap();
        } else {
            std::fs::write(&path, "original").unwrap();
        }
        entries.push(path);
    }
    let hooks = env.project_root.join(".git/hooks");
    std::fs::write(hooks.join("sentinel"), "original").unwrap();
    entries.extend([hooks, env.project_root.join(".git/config")]);
    for root in [checkout, &env.project_root] {
        for name in [".claude", ".codex", ".agents", ".mcp.json"] {
            let path = root.join(name);
            if name == ".mcp.json" {
                std::fs::write(&path, "original").unwrap();
            } else {
                std::fs::create_dir(&path).unwrap();
                std::fs::write(path.join("sentinel"), "original").unwrap();
            }
            entries.push(path);
        }
    }
    entries
}

#[test]
fn sandbox_guarded_entries_follow_protected_identity_binds() {
    use std::os::unix::fs::symlink;

    let env = Env::new();
    let checkout = env.home_root.join("linked");
    let entries = guard_fixture(&env, &checkout);
    let target = env.home_root.join("startup-target");
    std::fs::rename(&entries[0], &target).unwrap();
    symlink(&target, &entries[0]).unwrap();
    std::fs::remove_file(&entries[1]).unwrap();
    symlink(&target, &entries[1]).unwrap();
    let missing = env.home_root.join(".bash_logout");
    std::fs::remove_file(&missing).unwrap();
    let provider = checkout.join(".codex");
    let provider_entry = env.home_root.join(".zshrc");
    std::fs::remove_file(&provider_entry).unwrap();
    symlink(provider.join("sentinel"), &provider_entry).unwrap();
    let overlay = checkout.join(".agents");
    let skills = overlay.join("skills");
    std::fs::create_dir(&skills).unwrap();
    let library = env.rimz_home().join("skills/library");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(library.join("SKILL.md"), "library body").unwrap();
    let requested = env.home_root.join("requested-ro");
    std::fs::create_dir(&requested).unwrap();
    let vars = environment(&env);
    let state = env.store();
    let inputs = SandboxInputs {
        read_only: vec![requested],
        env: &vars,
        cwd: &checkout,
        project_root: &env.project_root,
        worktree: Some(&checkout),
        checkout: Some(&checkout),
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: Some(provider.clone()),
        skills: SkillInputs {
            kind: "codex",
            home: Some(skills),
            manual: ManualSkill::OpenAiPolicy,
            callable: Some(&[]),
        },
    };
    let plan = rimz::sandbox::plan(&inputs).unwrap();
    let mut planned_paths: Vec<_> = entries
        .into_iter()
        .filter(|p| p != &missing && p != &provider && p != &provider_entry && p != &overlay)
        .collect();
    planned_paths.sort();
    let mut expected = Vec::new();
    for path in planned_paths {
        let canonical = path.canonicalize().unwrap();
        if !expected.contains(&canonical) {
            expected.push(canonical);
        }
    }
    assert_eq!(plan.guarded_entries(), expected);

    let plan = rimz::sandbox::plan(&SandboxInputs {
        project_root: &checkout,
        ..inputs
    })
    .unwrap();
    let mut unique = plan.guarded_entries().to_vec();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        plan.guarded_entries().len(),
        "duplicate roots or aliases must not duplicate guards"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn sandbox_guard_restores_replaced_entries_in_a_running_view() {
    use rimz::sandbox::BindGuard;
    use std::os::unix::fs::symlink;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    if !available() {
        return;
    }
    let env = Env::new();
    let checkout = env.home_root.join("linked");
    let mut entries = guard_fixture(&env, &checkout);
    let target = env.home_root.join("startup-target");
    std::fs::rename(&entries[0], &target).unwrap();
    symlink("startup-target", &entries[0]).unwrap();
    entries[0] = target;
    let provider = env.home_root.join("provider");
    std::fs::create_dir(&provider).unwrap();
    let vars = environment(&env);
    let state = env.store();
    let unit = state.paths().temp_unit_dir(None);
    let plan = rimz::sandbox::plan(&SandboxInputs {
        read_only: Vec::new(),
        env: &vars,
        cwd: &checkout,
        project_root: &env.project_root,
        worktree: Some(&checkout),
        checkout: Some(&checkout),
        tmp_dir: &unit,
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: Some(provider.clone()),
        skills: SkillInputs {
            kind: "codex",
            home: None,
            manual: ManualSkill::OpenAiPolicy,
            callable: None,
        },
    })
    .unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    let mut guarded = plan.guarded_entries().to_vec();
    guarded.sort();
    let mut fixture = entries.clone();
    fixture.sort();
    assert_eq!(guarded, fixture, "the plan must guard every fixture entry");
    let (tx, rx) = mpsc::channel();
    let mut guard = BindGuard::arm(plan.guarded_entries(), &env.rimz_bin(), move |outcome| {
        tx.send(outcome).unwrap();
    })
    .unwrap()
    .unwrap();
    let argv = rimz::sandbox::bwrap_argv(
        &rimz::sandbox::preflight(Isolation::Scoped).unwrap().unwrap(), &plan.plan, &checkout,
        &["/bin/sh".into(), "-c".into(), r#"
            : > /tmp/guard-ready
            while [ ! -e /tmp/guard-go ]; do sleep 0.01; done
            provider=$1; shift
            for path do
                value=$path
                if [ -d "$path" ]; then value=$path/sentinel; fi
                test "$(cat "$value")" = replacement-host || exit 10
                if (printf bad >> "$value") 2>/dev/null; then exit 11; fi
                rm -rf /tmp/rename-source
                if [ -d "$path" ]; then mkdir /tmp/rename-source; else : > /tmp/rename-source; fi
                if mv -T /tmp/rename-source "$path" 2>/dev/null; then exit 12; fi
                test "$(awk -v p="$path" '$5 == p { n++ } END { print n+0 }' /proc/self/mountinfo)" = 1 || exit 13
            done
            printf writable > checkout-write
            printf writable > "$provider/provider-write"
        "#.into(), "guard-test".into(), provider.display().to_string()],
    );
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .args(&entries)
        .env("HOME", &env.home_root)
        .env("RIMZ_HOME", env.rimz_home())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !unit.join("guard-ready").exists() {
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            panic!(
                "view exited before readiness: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert!(Instant::now() < deadline, "view did not signal readiness");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The queue must survive the park handoff even before the first point.
    let first = &entries[0];
    std::fs::write(first.with_extension("lock"), "replacement").unwrap();
    std::fs::rename(first.with_extension("lock"), first).unwrap();
    let fd_flags: BTreeMap<_, _> = std::fs::read_dir("/proc/self/fdinfo")
        .unwrap()
        .flatten()
        .filter_map(|entry| {
            let fd: i32 = entry.file_name().to_str()?.parse().ok()?;
            let info = std::fs::read_to_string(entry.path()).ok()?;
            let flags = info
                .lines()
                .find_map(|line| line.strip_prefix("flags:\t"))?;
            Some((fd, i32::from_str_radix(flags, 8).ok()?))
        })
        .collect();
    let fd = guard.prepare_handoff().unwrap();
    let (tx, handed_rx) = mpsc::channel();
    let handed = BindGuard::handover(
        Some(fd),
        plan.guarded_entries(),
        &env.rimz_bin(),
        move |outcome| {
            tx.send(outcome).unwrap();
        },
    )
    .unwrap();
    // A real exec discards the old owner without running its destructor.
    std::mem::forget(guard);
    let guard = handed.unwrap();
    guard.point(child.id());
    let restored = handed_rx.recv_timeout(Duration::from_secs(30));
    assert!(
        restored
            .as_ref()
            .is_ok_and(|o| o.paths.contains(first) && o.result.is_ok()),
        "queued replacement must be repaired after handoff: {restored:?}"
    );
    assert!(
        rx.try_recv().is_err(),
        "an unpointed guard must retain events"
    );
    assert_ne!(
        fd_flags[&fd] & nix::fcntl::OFlag::O_CLOEXEC.bits(),
        0,
        "the armed fd must be CLOEXEC"
    );

    for path in entries.iter().skip(1).chain(std::iter::once(first)) {
        if path.is_dir() {
            std::fs::rename(path, path.with_extension("old")).unwrap();
            std::fs::create_dir(path).unwrap();
            std::fs::write(path.join("sentinel"), "replacement").unwrap();
        } else {
            std::fs::write(path.with_extension("lock"), "replacement").unwrap();
            std::fs::rename(path.with_extension("lock"), path).unwrap();
        }
        let restored = handed_rx.recv_timeout(Duration::from_secs(30));
        assert!(
            restored
                .as_ref()
                .is_ok_and(|o| o.paths.contains(path) && o.result.is_ok()),
            "replacement of {path:?} must be repaired: {restored:?}"
        );
    }
    for path in &entries {
        let value = if path.is_dir() {
            path.join("sentinel")
        } else {
            path.clone()
        };
        use std::io::Write;
        write!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(value)
                .unwrap(),
            "-host"
        )
        .unwrap();
    }
    // A second helper pass must not stack mounts over already repaired entries.
    let view = rimz::proc::children(child.id())[0];
    let output = env
        .rimz()
        .arg("--scoped-bind-restore")
        .arg(view.to_string())
        .args(&entries)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let dangling = [
        env.home_root.join("dangling-one"),
        env.home_root.join("dangling-two"),
    ];
    for path in &dangling {
        symlink(env.home_root.join("absent-target"), path).unwrap();
    }
    let output = env
        .rimz()
        .arg("--scoped-bind-restore")
        .arg(view.to_string())
        .arg(env.home_root.join("absent-entry"))
        .args(&dangling)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "the helper must not write to stdout"
    );
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        dangling
            .iter()
            .map(|path| format!("{path:?}: No such file or directory (os error 2)\n"))
            .collect::<String>()
    );
    std::fs::write(unit.join("guard-go"), "").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "view exit {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let mut guard = guard;
    let fd = guard.prepare_handoff().unwrap();
    drop(guard);
    assert!(
        !Path::new(&format!("/proc/self/fd/{fd}")).exists(),
        "drop must close the guard fd before returning"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("checkout-write")).unwrap(),
        "writable"
    );
    assert_eq!(
        std::fs::read_to_string(provider.join("provider-write")).unwrap(),
        "writable"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn sandbox_guard_refuses_unwatchable_entries_and_empty_sets_need_no_helper() {
    use rimz::sandbox::{BindGuard, BindGuardErr};

    assert!(
        BindGuard::arm(&[], Path::new("/no/helper"), |_| {})
            .unwrap()
            .is_none()
    );
    let env = Env::new();
    let path = env.home_root.join("missing-parent/entry");
    let result = BindGuard::arm(&[path], &env.rimz_bin(), |_| {});
    assert!(
        matches!(result, Err(BindGuardErr::Watch { .. })),
        "a missing watched parent must refuse the guard"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn sandbox_guard_bounds_view_lookup_and_reaps_timed_out_helpers() {
    use rimz::sandbox::{BindGuard, Mount, MountPlan};
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    if !available() {
        return;
    }
    let env = Env::new();
    let path = env.home_root.join(".gitconfig");
    std::fs::write(&path, "original").unwrap();
    let helper = env.home_root.join("slow-helper");
    let helper_pid = helper.with_extension("pid");
    std::fs::write(
        &helper,
        "#!/bin/sh\nprintf '%s\\n' $$ >> \"$0.pid\"\nexec sleep 120\n",
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let entries = vec![path.clone()];
    let (tx, rx) = mpsc::channel();
    let guard = BindGuard::arm(&entries, &helper, move |outcome| {
        tx.send(outcome).unwrap();
    })
    .unwrap()
    .unwrap();
    // A live process in our own namespace must time out, not run the helper.
    guard.point(std::process::id());
    std::fs::write(path.with_extension("lock"), "replacement").unwrap();
    std::fs::rename(path.with_extension("lock"), &path).unwrap();
    let outcome = rx.recv_timeout(Duration::from_secs(30));
    assert!(
        outcome.as_ref().is_ok_and(
            |o| o.paths == entries && o.result == Err("scoped view lookup timed out".into())
        ),
        "a live provider without an assembled view must report its bounded lookup failure: {outcome:?}"
    );
    assert!(
        !helper_pid.exists(),
        "an unassembled view must not spawn a repair"
    );

    let ready = env.home_root.join("view-ready");
    let plan = MountPlan {
        mounts: vec![Mount::RoBind {
            source: path.clone(),
            target: path.clone(),
        }],
    };
    let argv = rimz::sandbox::bwrap_argv(
        &rimz::sandbox::preflight(Isolation::Scoped)
            .unwrap()
            .unwrap(),
        &plan,
        &env.project_root,
        &[
            "/bin/sh".into(),
            "-c".into(),
            ": > \"$1\"; read answer".into(),
            "guard-errors".into(),
            ready.display().to_string(),
        ],
    );
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env("HOME", &env.home_root)
        .env("RIMZ_HOME", env.rimz_home())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready.exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "view exited before readiness"
        );
        assert!(Instant::now() < deadline, "view did not signal readiness");
        std::thread::sleep(Duration::from_millis(10));
    }
    guard.point(child.id());
    std::fs::write(path.with_extension("lock"), "replacement").unwrap();
    std::fs::rename(path.with_extension("lock"), &path).unwrap();
    let outcome = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(outcome.paths, entries);
    assert_eq!(outcome.result, Err("scoped bind helper timed out".into()));
    drop(guard);
    let pids: Vec<u32> = std::fs::read_to_string(helper_pid)
        .unwrap()
        .lines()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 1, "one helper per repair batch: {pids:?}");
    assert!(
        !rimz::proc::process_is_live(pids[0], None),
        "timed-out helper must be killed and reaped"
    );
    let missing = env.home_root.join("missing-helper");
    let (tx, rx) = mpsc::channel();
    let guard = BindGuard::arm(&entries, &missing, move |outcome| {
        tx.send(outcome).unwrap();
    })
    .unwrap()
    .unwrap();
    guard.point(child.id());
    std::fs::write(path.with_extension("lock"), "replacement").unwrap();
    std::fs::rename(path.with_extension("lock"), &path).unwrap();
    let outcome = rx.recv_timeout(Duration::from_secs(30)).unwrap();
    assert_eq!(outcome.paths, entries);
    assert_eq!(
        outcome.result,
        Err("spawning scoped bind helper: No such file or directory (os error 2)".into())
    );
    assert!(!outcome.result.unwrap_err().contains("timed out"));
    drop(guard);
    writeln!(child.stdin.take().unwrap(), "done").unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
#[cfg(target_os = "linux")]
fn scoped_protects_startup_git_and_project_configuration_in_linked_checkouts() {
    if !available() {
        return;
    }
    let env = Env::new();
    let checkout = env.home_root.join("linked");
    for args in [
        vec!["init"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
        vec![
            "worktree",
            "add",
            "-b",
            "linked",
            checkout.to_str().unwrap(),
        ],
    ] {
        let output = std::process::Command::new("git")
            .env("HOME", &env.home_root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .arg("-C")
            .arg(&env.project_root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut protected = Vec::new();
    for entry in [
        ".profile",
        ".bashrc",
        ".bash_profile",
        ".bash_login",
        ".bash_logout",
        ".zshenv",
        ".zprofile",
        ".zshrc",
        ".zlogin",
        ".zlogout",
        ".gitconfig",
        ".config/fish/config.fish",
        ".ssh/config",
        ".config/systemd/user/test.service",
    ] {
        let path = env.home_root.join(entry);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "original").unwrap();
        protected.push(path);
    }
    for root in [&checkout, &env.project_root] {
        for entry in [
            ".claude/settings.local.json",
            ".codex/config.toml",
            ".agents/agent.md",
            ".mcp.json",
        ] {
            let path = root.join(entry);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "original").unwrap();
            protected.push(path);
        }
    }
    protected.extend([
        env.project_root.join(".git/config"),
        env.project_root.join(".git/hooks/x"),
    ]);
    let skills = checkout.join(".agents/skills");
    std::fs::create_dir_all(&skills).unwrap();
    let library = env.rimz_home().join("skills/library");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(library.join("SKILL.md"), "library body").unwrap();
    protected.push(skills.join("added"));
    let cwd = checkout.join("nested");
    std::fs::create_dir(&cwd).unwrap();
    let vars = environment(&env);
    let state = env.store();
    let inputs = SandboxInputs {
        read_only: Vec::new(),
        env: &vars,
        cwd: &cwd,
        project_root: &env.project_root,
        worktree: Some(&checkout),
        checkout: Some(&checkout),
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills: SkillInputs {
            kind: "codex",
            home: Some(skills.clone()),
            manual: ManualSkill::OpenAiPolicy,
            callable: None,
        },
    };
    let plan = rimz::sandbox::plan(&inputs).unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    let argv = rimz::sandbox::bwrap_argv(Path::new("/usr/bin/bwrap"), &plan.plan, &cwd, &[]);
    let common_hooks = env.project_root.join(".git/hooks").display().to_string();
    assert!(argv.contains(&common_hooks), "{argv:?}");
    assert!(!argv.iter().any(|arg| arg.contains("/../")), "{argv:?}");
    let output = std::process::Command::new(&argv[0]).args(&argv[1..])
        .args(["sh", "-c", "test -f ../.agents/skills/library/SKILL.md || exit 2; for path do if (printf changed >> \"$path\") 2>/dev/null; then printf 'wrote %s\\n' \"$path\"; exit 1; fi; done; printf ok > writable", "protected"])
        .args(&protected).output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_to_string(cwd.join("writable")).unwrap(), "ok");

    // Absent entries are not created, and cannot prevent launching the view.
    for root in [&checkout, &env.project_root] {
        std::fs::remove_file(root.join(".mcp.json")).unwrap();
    }
    std::fs::remove_dir_all(env.home_root.join(".ssh")).unwrap();
    let plan = rimz::sandbox::plan(&inputs).unwrap();
    let argv = rimz::sandbox::bwrap_argv(Path::new("/usr/bin/bwrap"), &plan.plan, &cwd, &[]);
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .args(["true"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!checkout.join(".mcp.json").exists());
    assert!(!env.home_root.join(".ssh").exists());
}

#[test]
#[cfg(target_os = "linux")]
fn scoped_protection_keeps_provider_homes_writable() {
    if !available() {
        return;
    }
    let env = Env::new();
    let home = &env.home_root;
    let output = std::process::Command::new("git")
        .args(["init", home.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let native = home.join("accounts/codex/default");
    let account = home.join("accounts/codex/work");
    std::fs::create_dir_all(native.join("sessions")).unwrap();
    std::fs::create_dir_all(&account).unwrap();
    std::os::unix::fs::symlink(&native, home.join(".codex")).unwrap();
    std::os::unix::fs::symlink(home.join(".codex/sessions"), account.join("sessions")).unwrap();
    std::fs::write(home.join(".mcp.json"), "original").unwrap();
    std::fs::create_dir(home.join(".claude")).unwrap();
    let state = env.store();
    let vars = environment(&env);
    let mut inputs = SandboxInputs {
        env: &vars,
        cwd: home,
        project_root: home,
        worktree: None,
        checkout: Some(home),
        read_only: Vec::new(),
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: Some(rimz::sandbox::ProviderHome {
            source: account.clone(),
            target: account.clone(),
        }),
        provider_home_env_keys: &["CODEX_HOME"],
        default_home: Some(home.join(".codex")),
        skills: SkillInputs {
            kind: "codex",
            home: None,
            manual: ManualSkill::Unsupported,
            callable: None,
        },
    };
    for contains_home in [false, true] {
        if contains_home {
            std::fs::remove_file(home.join(".codex")).unwrap();
            std::fs::create_dir_all(home.join(".codex/account")).unwrap();
            let account = home.join(".codex/account");
            inputs.provider_home = Some(rimz::sandbox::ProviderHome {
                source: account.clone(),
                target: account,
            });
            inputs.default_home = None;
        }
        let skills = home.join(".codex/skills");
        std::fs::create_dir_all(skills.join("hidden")).unwrap();
        std::fs::write(skills.join("hidden/SKILL.md"), "skill body\n").unwrap();
        inputs.skills.home = Some(skills.clone());
        inputs.skills.manual = ManualSkill::OpenAiPolicy;
        inputs.skills.callable = Some(&[]);
        let plan = rimz::sandbox::plan(&inputs).unwrap();
        rimz::sandbox::apply(&plan).unwrap();
        let argv = rimz::sandbox::bwrap_argv(Path::new("/usr/bin/bwrap"), &plan.plan, home, &[]);
        let provider = &inputs.provider_home.as_ref().unwrap().target;
        let mut writes = vec![
            provider.join("written"),
            home.join(".codex/direct"),
            skills.join("added"),
        ];
        if !contains_home {
            writes.extend([account.join("sessions/rollout"), native.join("direct")]);
        }
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .args([
                "sh",
                "-c",
                "for path do printf ok > \"$path\" || exit 1; done",
                "writable",
            ])
            .args(&writes)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let protected = if contains_home {
            home.join(".codex")
        } else {
            native.clone()
        };
        for mount in &plan.plan.mounts {
            if let rimz::sandbox::Mount::RoBind { target, .. }
            | rimz::sandbox::Mount::RemountRo { target } = mount
            {
                assert_ne!(
                    target.canonicalize().unwrap(),
                    protected.canonicalize().unwrap(),
                    "provider home must stay writable"
                );
            }
        }
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .args([
                "sh",
                "-c",
                "for path do if (printf bad >> \"$path\") 2>/dev/null; then exit 1; fi; done",
                "protected",
            ])
            .args([
                home.join(".mcp.json"),
                home.join(".claude/added"),
                home.join(".git/hooks/x"),
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(!home.join(".agents").exists());
    }
}

#[test]
#[cfg(target_os = "linux")]
fn scoped_protects_separate_git_directory() {
    if !available() {
        return;
    }
    let env = Env::new();
    let git = env.home_root.join("git-admin");
    let output = std::process::Command::new("git")
        .env("HOME", &env.home_root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .arg("-C")
        .arg(&env.project_root)
        .args(["init", "--separate-git-dir"])
        .arg(&git)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let hook = git.join("hooks/test-hook");
    std::fs::write(&hook, "original").unwrap();
    let vars = environment(&env);
    let state = env.store();
    let plan = rimz::sandbox::plan(&SandboxInputs {
        read_only: Vec::new(),
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        checkout: Some(&env.project_root),
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills: SkillInputs {
            kind: "codex",
            home: None,
            manual: ManualSkill::Unsupported,
            callable: None,
        },
    })
    .unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    let argv = rimz::sandbox::bwrap_argv(
        Path::new("/usr/bin/bwrap"),
        &plan.plan,
        &env.project_root,
        &[],
    );
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .args(["sh", "-c", "for path do if (printf changed >> \"$path\") 2>/dev/null; then printf 'wrote %s\\n' \"$path\"; exit 1; fi; done", "protected"])
        .args([git.join("config"), hook])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn sandbox_unconfigured_skills_without_library_add_no_mounts() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    std::fs::create_dir_all(root.join("native")).unwrap();
    // Even a shadowed library entry leaves the native view unchanged.
    let library = env.rimz_home().join("skills/native");
    for shadowed in [false, true] {
        if shadowed {
            std::fs::create_dir_all(&library).unwrap();
        }
        let argv = skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "codex",
                home: Some(root.clone()),
                manual: ManualSkill::OpenAiPolicy,
                callable: None,
            },
        )
        .unwrap();
        assert!(
            !argv
                .iter()
                .any(|arg| arg == "--tmpfs" || arg == "--ro-bind")
        );
        assert!(!env.store().paths().skills_dir.exists());
    }
}

#[test]
fn sandbox_unconfigured_unreadable_skills_keep_native_launch_behavior() {
    let execute = available();
    for invalid_name in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        let root = env.home_root.join(".agents/skills");
        std::fs::create_dir_all(root.parent().unwrap()).unwrap();
        if invalid_name {
            std::fs::create_dir(&root).unwrap();
            std::fs::write(
                root.join(std::ffi::OsString::from_vec(vec![0xff])),
                "native",
            )
            .unwrap();
        } else {
            std::os::unix::fs::symlink("skills", &root).unwrap();
        }
        let vars = environment(&env);
        for callable in [None, Some(&[][..])] {
            let result = skill_argv(
                &env,
                &vars,
                SkillInputs {
                    kind: "codex",
                    home: Some(root.clone()),
                    manual: ManualSkill::OpenAiPolicy,
                    callable,
                },
            );
            if callable.is_some() {
                assert!(result.is_err());
            } else {
                assert!(
                    !result
                        .unwrap()
                        .iter()
                        .any(|arg| arg == "--tmpfs" || arg == "--ro-bind")
                );
            }
        }
        if !execute {
            continue;
        }
        enable(&env);
        let shim_dir = write_env_dump_shim(&env, "codex");
        std::fs::write(
            shim_dir.join("codex"),
            "#!/bin/sh\nprintf ran > /tmp/skill-probe\n",
        )
        .unwrap();
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
        let probe = env.store().paths().temp_unit_dir(None).join("skill-probe");
        env.rimz()
            .args(exec_args(&env, &request))
            .env("PATH", path_with_front(&shim_dir))
            .assert_success_within_timeout("unconfigured native skill discovery");
        assert_eq!(std::fs::read_to_string(&probe).unwrap(), "ran");
        std::fs::remove_file(&probe).unwrap();
        request.skills = Some(Vec::new());
        let output = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("PATH", path_with_front(&shim_dir))
            .bounded_output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            !probe.exists(),
            "configured discovery errors must refuse before provider exec"
        );
    }
}

#[test]
fn sandbox_unusable_unlisted_skill_is_omitted_with_warning() {
    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    for name in ["listed", "plain", "bad"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("SKILL.md"), "body\n").unwrap();
    }
    let metadata = "---\nname: bad\ndescription: &d x\nsummary: *d\n---\nbody\n";
    let bad = root.join("bad/SKILL.md");
    std::fs::write(&bad, metadata).unwrap();
    std::os::unix::fs::symlink("bad", root.join("alias")).unwrap();
    let library = env.rimz_home().join("skills");
    std::fs::create_dir_all(&library).unwrap();
    std::os::unix::fs::symlink(root.join("bad"), library.join("library-alias")).unwrap();
    std::fs::write(root.join("listed/SKILL.md"), metadata).unwrap();
    let vars = environment(&env);
    let state = env.store();
    let prepared = rimz::sandbox::plan(&SandboxInputs {
        read_only: Vec::new(),
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        checkout: None,
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills: SkillInputs {
            kind: "claude",
            home: Some(root.clone()),
            manual: ManualSkill::Frontmatter,
            callable: Some(&["listed".parse().unwrap()]),
        },
    })
    .unwrap();
    assert!(!state.paths().skills_dir.exists());
    assert!(!state.paths().tmp_dir.exists());
    assert_eq!(prepared.skipped.len(), 3);
    for (skipped, name) in prepared
        .skipped
        .iter()
        .zip(["alias", "bad", "library-alias"])
    {
        assert_eq!(skipped.name, name);
        assert_eq!(skipped.path, bad);
        assert!(matches!(&skipped.reason, SkipReason::Metadata(_)));
        assert!(
            skipped
                .to_string()
                .contains(&format!("starting without skill {name:?}"))
        );
    }
    let argv = rimz::sandbox::bwrap_argv(
        Path::new("/usr/bin/bwrap"),
        &prepared.plan,
        &env.project_root,
        &[],
    );
    assert!(
        argv.windows(2)
            .any(|args| args[0] == "--tmpfs" && Path::new(&args[1]) == root)
    );
    assert_eq!(
        skill_bind_source(&argv, &root.join("listed")),
        root.join("listed").canonicalize().unwrap()
    );
    let copy = skill_bind_source(&argv, &root.join("plain"));
    assert!(copy.starts_with(&state.paths().skills_dir));
    assert!(!copy.starts_with(&state.paths().tmp_dir));
    assert!(!copy.exists());
    rimz::sandbox::apply(&prepared).unwrap();
    assert!(state.paths().tmp_dir.is_dir());
    assert_eq!(
        std::fs::read_to_string(copy.join("SKILL.md")).unwrap(),
        "---\ndisable-model-invocation: true\n---\nbody\n"
    );
    assert!(!argv.iter().any(|arg| Path::new(arg) == root.join("bad")));
    for name in ["alias", "library-alias"] {
        assert!(!argv.iter().any(|arg| Path::new(arg) == root.join(name)));
    }
    assert_eq!(std::fs::read(&bad).unwrap(), metadata.as_bytes());
    assert!(
        std::fs::read_dir(&state.paths().skills_dir)
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .as_encoded_bytes()
                .starts_with(b"."))
    );
}

#[test]
fn sandbox_unreadable_unlisted_skill_is_omitted_with_warning() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    std::fs::create_dir_all(root.join("bad/scripts")).unwrap();
    std::fs::write(root.join("bad/SKILL.md"), "body\n").unwrap();
    let unreadable = root.join("bad/scripts/run.sh");
    std::fs::write(&unreadable, "echo original\n").unwrap();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read(&unreadable).is_ok() {
        crate::common::skip("mode 000 files remain readable");
        return;
    }
    let vars = environment(&env);
    let prepared = skill_prepare(
        &env,
        &vars,
        SkillInputs {
            kind: "claude",
            home: Some(root.clone()),
            manual: ManualSkill::Frontmatter,
            callable: Some(&[]),
        },
    )
    .unwrap();
    assert_eq!(prepared.skipped.len(), 1);
    let skipped = &prepared.skipped[0];
    assert_eq!(skipped.name, "bad");
    assert_eq!(skipped.path, unreadable);
    assert!(matches!(&skipped.reason, SkipReason::Unreadable(_)));
    assert!(
        skipped
            .to_string()
            .contains("starting without skill \"bad\"")
    );
    let argv = rimz::sandbox::bwrap_argv(
        Path::new("/usr/bin/bwrap"),
        &prepared.plan,
        &env.project_root,
        &[],
    );
    assert!(
        argv.windows(2)
            .any(|args| args[0] == "--tmpfs" && Path::new(&args[1]) == root)
    );
    assert!(!argv.iter().any(|arg| Path::new(arg) == root.join("bad")));
}

#[test]
fn sandbox_skills_state_failure_still_refuses() {
    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    std::fs::create_dir_all(root.join("plain")).unwrap();
    std::fs::write(root.join("plain/SKILL.md"), "body\n").unwrap();
    let state = env.store();
    std::fs::create_dir_all(state.paths().skills_dir.parent().unwrap()).unwrap();
    std::fs::write(&state.paths().skills_dir, "not a directory").unwrap();
    assert!(
        skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "claude",
                home: Some(root),
                manual: ManualSkill::Frontmatter,
                callable: Some(&[]),
            },
        )
        .is_err()
    );
}

#[test]
fn sandboxed_exec_merges_library_without_native_skill_root() {
    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    enable(&env);
    let root = env.home_root.join(".agents/skills");
    let library = env.rimz_home().join("skills/library-only");
    std::fs::create_dir_all(&library).unwrap();
    std::fs::write(library.join("SKILL.md"), "library body").unwrap();
    assert!(!root.exists());
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        shim_dir.join("codex"),
        r#"#!/bin/sh
set -eu
test "$(cat "$HOME/.agents/skills/library-only/SKILL.md")" = 'library body'
if touch "$HOME/.agents/skills/library-only/changed" 2>/dev/null; then exit 1; fi
printf consumed > /tmp/library-probe
"#,
    )
    .unwrap();
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    env.rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .assert_success_within_timeout("library-only skill discovery");
    assert_eq!(
        std::fs::read_to_string(
            env.store()
                .paths()
                .temp_unit_dir(None)
                .join("library-probe")
        )
        .unwrap(),
        "consumed"
    );
    assert!(root.is_dir());
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_to_string(library.join("SKILL.md")).unwrap(),
        "library body"
    );
    assert!(!library.join("changed").exists());
}

#[test]
fn sandbox_merges_rimz_library_and_provider_root_wins() {
    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    std::fs::create_dir_all(root.join("shared")).unwrap();
    std::fs::write(root.join("shared/SKILL.md"), "provider\n").unwrap();
    for source in ["rimz_home", "home", "override"] {
        let mut vars = environment(&env);
        let library = match source {
            "rimz_home" => env.rimz_home().join("skills"),
            "home" => {
                vars.remove("RIMZ_HOME");
                env.home_root.join(".rimz/skills")
            }
            _ => {
                let root = env.home_root.join("relocated");
                vars.insert("RIMZ_AGENTS_HOME".to_owned(), root.display().to_string());
                root.join("skills")
            }
        };
        for name in ["shared", "library-only"] {
            std::fs::create_dir_all(library.join(name)).unwrap();
            std::fs::write(library.join(name).join("SKILL.md"), "library\n").unwrap();
        }
        let argv = skill_argv(
            &env,
            &vars,
            SkillInputs {
                kind: "claude",
                home: Some(root.clone()),
                manual: ManualSkill::Frontmatter,
                callable: None,
            },
        )
        .unwrap();
        assert_eq!(
            skill_bind_source(&argv, &root.join("shared")),
            root.join("shared").canonicalize().unwrap()
        );
        assert_eq!(
            skill_bind_source(&argv, &root.join("library-only")),
            library.join("library-only").canonicalize().unwrap()
        );
        assert_eq!(
            argv.windows(2).filter(|args| args[0] == "--tmpfs").count(),
            1
        );
        assert!(
            argv.windows(2)
                .any(|args| args[0] == "--tmpfs" && Path::new(&args[1]) == root)
        );
        assert!(!root.join("library-only").exists());
        assert!(!env.store().paths().skills_dir.exists());
    }
}

#[test]
fn sandbox_empty_skill_list_makes_every_skill_manual() {
    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    let library = env.rimz_home().join("skills");
    for (source, name) in [(&root, "native"), (&library, "library-only")] {
        std::fs::create_dir_all(source.join(name)).unwrap();
        std::fs::write(source.join(name).join("SKILL.md"), format!("{name}\n")).unwrap();
    }
    let argv = skill_argv(
        &env,
        &environment(&env),
        SkillInputs {
            kind: "claude",
            home: Some(root.clone()),
            manual: ManualSkill::Frontmatter,
            callable: Some(&[]),
        },
    )
    .unwrap();
    assert_eq!(
        argv.windows(3)
            .filter(|args| args[0] == "--ro-bind")
            .count(),
        2,
        "library copies bind only at the discovery path, not over the host library"
    );
    for (source, name) in [(&root, "native"), (&library, "library-only")] {
        let copy = skill_bind_source(&argv, &root.join(name));
        assert!(copy.starts_with(&env.store().paths().skills_dir));
        assert_eq!(
            std::fs::read_to_string(copy.join("SKILL.md")).unwrap(),
            format!("---\ndisable-model-invocation: true\n---\n{name}\n")
        );
        assert_eq!(
            std::fs::read_to_string(source.join(name).join("SKILL.md")).unwrap(),
            format!("{name}\n")
        );
    }
}

#[test]
fn sandbox_manual_copies_are_content_addressed() {
    use std::os::unix::fs::PermissionsExt;

    let env = Env::new();
    let root = env.home_root.join(".claude/skills");
    let source = root.join("manual");
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    std::fs::write(source.join("SKILL.md"), "manual body\n").unwrap();
    std::fs::write(source.join("scripts/run.sh"), "echo original\n").unwrap();
    std::fs::set_permissions(
        source.join("scripts/run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let companion = env.home_root.join("companion.bin");
    std::fs::write(&companion, [0, 0xff, 1]).unwrap();
    std::os::unix::fs::symlink(&companion, source.join("companion.bin")).unwrap();
    std::os::unix::fs::symlink("missing", source.join("broken")).unwrap();
    let prepare = || {
        let argv = skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "claude",
                home: Some(root.clone()),
                manual: ManualSkill::Frontmatter,
                callable: Some(&[]),
            },
        )
        .unwrap();
        skill_bind_source(&argv, &source)
    };
    let barrier = std::sync::Barrier::new(4);
    let first = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    prepare()
                })
            })
            .collect();
        let copies: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(copies.iter().all(|copy| copy == &copies[0]));
        copies[0].clone()
    });
    assert_eq!(first, prepare());
    let state = env.store();
    assert_eq!(
        std::fs::read_dir(&state.paths().skills_dir)
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read(first.join("companion.bin")).unwrap(),
        [0, 0xff, 1]
    );
    assert!(!first.join("companion.bin").is_symlink());
    assert!(!first.join("broken").exists());
    assert_eq!(
        std::fs::metadata(first.join("scripts/run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::read_to_string(first.join("scripts/run.sh")).unwrap(),
        "echo original\n"
    );
    std::fs::write(source.join("scripts/run.sh"), "echo edited\n").unwrap();
    let second = prepare();
    assert_ne!(first, second);
    assert_eq!(
        std::fs::read_dir(&state.paths().skills_dir)
            .unwrap()
            .count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(first.join("scripts/run.sh")).unwrap(),
        "echo original\n"
    );
    assert_eq!(
        std::fs::read_to_string(second.join("scripts/run.sh")).unwrap(),
        "echo edited\n"
    );
    assert_eq!(
        std::fs::read_to_string(source.join("SKILL.md")).unwrap(),
        "manual body\n"
    );
    assert!(source.join("companion.bin").is_symlink());
    assert!(source.join("broken").is_symlink());
    assert_eq!(std::fs::read(companion).unwrap(), [0, 0xff, 1]);
}

#[test]
fn sandbox_codex_manual_writes_openai_policy() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    let metadata =
        "interface:\n  display_name: Keep me\npolicy:\n  allow_implicit_invocation: true\n";
    for name in ["existing", "missing", "callable"] {
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("SKILL.md"), "codex body\n").unwrap();
    }
    std::fs::create_dir_all(root.join("existing/agents")).unwrap();
    std::fs::write(root.join("existing/agents/openai.yaml"), metadata).unwrap();
    let argv = skill_argv(
        &env,
        &environment(&env),
        SkillInputs {
            kind: "codex",
            home: Some(root.clone()),
            manual: ManualSkill::OpenAiPolicy,
            callable: Some(&["callable".parse().unwrap()]),
        },
    )
    .unwrap();
    for name in ["existing", "missing"] {
        let copy = skill_bind_source(&argv, &root.join(name));
        let expected = if name == "existing" {
            metadata.replace("true", "false")
        } else {
            "policy:\n  allow_implicit_invocation: false\n".to_owned()
        };
        assert_eq!(
            std::fs::read_to_string(copy.join("agents/openai.yaml")).unwrap(),
            expected
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("SKILL.md")).unwrap(),
            "codex body\n"
        );
    }
    assert_eq!(
        skill_bind_source(&argv, &root.join("callable")),
        root.join("callable").canonicalize().unwrap()
    );
    assert_eq!(
        std::fs::read_to_string(root.join("existing/agents/openai.yaml")).unwrap(),
        metadata
    );
    assert!(!root.join("missing/agents").exists());
    assert!(!root.join("callable/agents").exists());
}

#[test]
fn sandbox_unsupported_provider_refuses_skill_list() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    for installed in [false, true] {
        if installed {
            std::fs::create_dir_all(root.join("native")).unwrap();
        }
        let callable = ["native".parse().unwrap()];
        for list in [&[][..], &callable[..]] {
            let err = skill_argv(
                &env,
                &environment(&env),
                SkillInputs {
                    kind: "amp",
                    home: Some(root.clone()),
                    manual: ManualSkill::Unsupported,
                    callable: Some(list),
                },
            )
            .unwrap_err();
            assert!(matches!(&err, SandboxErr::ManualSkillsUnsupported { kind } if kind == "amp"));
            assert!(err.to_string().ends_with("remove the profile skills list"));
            assert!(!env.store().paths().tmp_dir.exists());
            assert!(!env.store().paths().skills_dir.exists());
        }
    }
    let err = skill_argv(
        &env,
        &environment(&env),
        SkillInputs {
            kind: "plugin",
            home: None,
            manual: ManualSkill::Unsupported,
            callable: Some(&[]),
        },
    )
    .unwrap_err();
    assert!(matches!(err, SandboxErr::SkillsNeedRoot { .. }));
}

#[test]
fn sandbox_prepare_preserves_symlinked_skill_sources() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    let source = env.home_root.join("library/visible");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(root.join("hidden")).unwrap();
    std::fs::write(root.join("hidden/SKILL.md"), "manual body\n").unwrap();
    std::os::unix::fs::symlink(&source, root.join("visible")).unwrap();
    std::os::unix::fs::symlink("missing", root.join("broken")).unwrap();
    let mut vars = environment(&env);
    vars.insert(
        "CLAUDE_CONFIG_DIR".to_owned(),
        env.home_root.join(".claude").display().to_string(),
    );
    let callable = ["visible".parse().unwrap()];
    let state = env.store();
    let mut inputs = SandboxInputs {
        read_only: Vec::new(),
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        checkout: None,
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: None,
        provider_home_env_keys: &["CODEX_HOME"],
        default_home: None,
        skills: SkillInputs {
            kind: "codex",
            home: Some(root.clone()),
            manual: ManualSkill::OpenAiPolicy,
            callable: Some(&callable),
        },
    };
    let plan = rimz::sandbox::plan(&inputs).unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    assert_eq!(plan.pins["CODEX_HOME"], rimz::sandbox::EnvPin::Unset);
    assert!(!plan.pins.contains_key("CLAUDE_CONFIG_DIR"));
    assert_eq!(
        plan.pins["HOME"],
        rimz::sandbox::EnvPin::Set(env.home_root.display().to_string())
    );
    assert_eq!(
        plan.pins["TMPDIR"],
        rimz::sandbox::EnvPin::Set("/tmp".to_owned())
    );
    assert_eq!(
        plan.pins["SCCACHE_CLIENT_SIDE"],
        rimz::sandbox::EnvPin::Set("1".to_owned())
    );
    assert!(!plan.pins.contains_key("PATH"));
    let argv = rimz::sandbox::bwrap_argv(
        Path::new("/usr/bin/bwrap"),
        &plan.plan,
        inputs.cwd,
        &["true".into()],
    );
    assert!(argv.windows(3).any(|args| args
        == [
            "--symlink",
            source.to_str().unwrap(),
            root.join("visible").to_str().unwrap()
        ]));
    assert!(argv.iter().any(|arg| arg.ends_with("/hidden")));
    let copy = skill_bind_source(&argv, &root.join("hidden"));
    assert!(copy.starts_with(&state.paths().skills_dir));
    assert_eq!(
        std::fs::read_to_string(copy.join("agents/openai.yaml")).unwrap(),
        "policy:\n  allow_implicit_invocation: false\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("hidden/SKILL.md")).unwrap(),
        "manual body\n"
    );
    assert!(!root.join("hidden/agents").exists());
    assert!(!argv.iter().any(|arg| arg.ends_with("/broken")));
    let unknown = ["absent".parse().unwrap()];
    inputs.skills.callable = Some(&unknown);
    let err = rimz::sandbox::plan(&inputs).err().unwrap();
    assert!(matches!(err, SandboxErr::UnknownSkill { .. }));
    assert!(err.to_string().contains(root.to_str().unwrap()));
}

#[test]
fn sandbox_symlinked_manual_skills_resolve_shared_modules() {
    let env = Env::new();
    env.record(&env.project_root);
    let root = env.home_root.join(".claude/skills");
    let source = env.home_root.join(".agents/skills/manual");
    let shared = source.parent().unwrap().join("_shared");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(source.join("scripts")).unwrap();
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(source.join("SKILL.md"), "manual body\n").unwrap();
    std::fs::write(shared.join("probe.py"), "value = 'shared module'\n").unwrap();
    std::fs::write(
        source.join("scripts/run.py"),
        "from pathlib import Path\nimport sys\nroot = Path(__file__).resolve().parents[2]\nsys.path.insert(0, str(root / '_shared'))\nfrom probe import value\nassert 'disable-model-invocation: true' in (root / 'manual/SKILL.md').read_text()\nprint(value)\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("../../.agents/skills/manual", root.join("relative")).unwrap();
    std::os::unix::fs::symlink(&source, root.join("absolute")).unwrap();
    std::fs::write(root.join("AGENTS.md"), "root instructions\n").unwrap();
    std::os::unix::fs::symlink("AGENTS.md", root.join("CLAUDE.md")).unwrap();
    let execute = available() && {
        let python3 = which::which("python3").is_ok();
        if !python3 {
            crate::common::skip("python3 not on PATH");
        }
        python3
    };
    for workaround in [false, true] {
        if workaround {
            std::os::unix::fs::symlink("../../.agents/skills/_shared", root.join("_shared"))
                .unwrap();
        }
        let argv = skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "claude",
                home: Some(root.clone()),
                manual: ManualSkill::Frontmatter,
                callable: Some(&[]),
            },
        )
        .unwrap();
        for (name, target) in [
            ("relative", Path::new("../../.agents/skills/manual")),
            ("absolute", source.as_path()),
            ("CLAUDE.md", Path::new("AGENTS.md")),
        ] {
            assert!(argv.windows(3).any(|args| args[0] == "--symlink"
                && Path::new(&args[1]) == target
                && Path::new(&args[2]) == root.join(name)));
        }
        let copy = skill_bind_source(&argv, &source);
        assert!(copy.starts_with(&env.store().paths().skills_dir));
        assert_eq!(
            argv.windows(3)
                .filter(|args| args[0] == "--ro-bind" && Path::new(&args[2]) == source)
                .count(),
            1
        );
        assert!(!argv.windows(3).any(|args| args[0] == "--ro-bind"
            && (Path::new(&args[2]) == shared || Path::new(&args[2]) == root.join("_shared"))));
        assert_eq!(
            std::fs::read_dir(&env.store().paths().skills_dir)
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(source.join("SKILL.md")).unwrap(),
            "manual body\n"
        );
        if !execute {
            continue;
        }
        enable(&env);
        let shim_dir = write_env_dump_shim(&env, "claude");
        std::fs::write(
            shim_dir.join("claude"),
            "#!/bin/sh\nset -eu\ntest -L \"$HOME/.claude/skills/relative\"\ntest -L \"$HOME/.claude/skills/absolute\"\ntest -L \"$HOME/.claude/skills/CLAUDE.md\"\ntest \"$(cat \"$HOME/.claude/skills/CLAUDE.md\")\" = 'root instructions'\npython3 \"$HOME/.claude/skills/relative/scripts/run.py\" > /tmp/probe-shared\n",
        )
        .unwrap();
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
        request.skills = Some(vec![]);
        env.rimz()
            .args(exec_args(&env, &request))
            .env("PATH", path_with_front(&shim_dir))
            .env_remove("CLAUDE_CONFIG_DIR")
            .assert_success_within_timeout("symlinked skill shared module import");
        assert_eq!(
            std::fs::read_to_string(env.store().paths().temp_unit_dir(None).join("probe-shared"))
                .unwrap(),
            "shared module\n"
        );
    }
}

#[test]
fn sandbox_skill_aliases_require_matching_invocation_policies() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    let source = root.join("native");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("SKILL.md"), "native body\n").unwrap();
    std::os::unix::fs::symlink("native", root.join("alias")).unwrap();
    for names in [
        vec!["alias"],
        vec!["native"],
        vec![],
        vec!["alias", "native"],
    ] {
        let callable: Vec<_> = names.iter().map(|name| name.parse().unwrap()).collect();
        let result = skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "codex",
                home: Some(root.clone()),
                manual: ManualSkill::OpenAiPolicy,
                callable: Some(&callable),
            },
        );
        if names.len() == 1 {
            let err = result.unwrap_err();
            assert!(
                matches!(&err, SandboxErr::ConflictingSkillAliases { listed, unlisted, path }
                if listed == names[0] && unlisted != listed && path == &source)
            );
            assert!(
                err.to_string()
                    .ends_with("list both names or neither in the profile skills list")
            );
            assert!(!env.store().paths().skills_dir.exists());
            continue;
        }
        let argv = result.unwrap();
        if names.is_empty() {
            assert!(skill_bind_source(&argv, &source).starts_with(&env.store().paths().skills_dir));
            assert_eq!(
                argv.windows(3)
                    .filter(|args| args[0] == "--ro-bind")
                    .count(),
                1
            );
        } else {
            assert!(!argv.iter().any(|arg| arg == "--tmpfs"));
        }
    }
}

#[test]
fn sandbox_non_skill_entries_are_never_materialized() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    std::fs::create_dir_all(root.join("_shared")).unwrap();
    std::fs::write(root.join("_shared/module.py"), "shared\n").unwrap();
    std::fs::write(root.join("AGENTS.md"), "instructions\n").unwrap();
    std::os::unix::fs::symlink("AGENTS.md", root.join("CLAUDE.md")).unwrap();
    for overlay in [false, true] {
        if overlay {
            let library = env.rimz_home().join("skills/library-only");
            std::fs::create_dir_all(&library).unwrap();
            std::fs::write(library.join("SKILL.md"), "library\n").unwrap();
        }
        let callable = if overlay {
            vec!["library-only".parse().unwrap()]
        } else {
            vec![]
        };
        let argv = skill_argv(
            &env,
            &environment(&env),
            SkillInputs {
                kind: "codex",
                home: Some(root.clone()),
                manual: ManualSkill::OpenAiPolicy,
                callable: Some(&callable),
            },
        )
        .unwrap();
        assert!(!env.store().paths().skills_dir.exists());
        assert!(!root.join("_shared/agents").exists());
        if overlay {
            assert_eq!(
                skill_bind_source(&argv, &root.join("_shared")),
                root.join("_shared")
            );
            assert_eq!(
                skill_bind_source(&argv, &root.join("AGENTS.md")),
                root.join("AGENTS.md")
            );
        } else {
            assert!(!argv.iter().any(|arg| arg == "--tmpfs"));
        }
    }
}

#[test]
fn sandbox_prepare_rebinds_tmp_rooted_runtime() {
    let env = Env::new();
    let vars = environment(&env);
    let state = env.store();
    let mut inputs = SandboxInputs {
        read_only: Vec::new(),
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        checkout: None,
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills: SkillInputs {
            kind: "codex",
            home: Some(env.home_root.join(".agents/skills")),
            manual: ManualSkill::OpenAiPolicy,
            callable: None,
        },
    };
    let plan = rimz::sandbox::plan(&inputs).unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    let argv = rimz::sandbox::bwrap_argv(Path::new("/usr/bin/bwrap"), &plan.plan, inputs.cwd, &[]);
    assert!(argv.windows(3).any(|args| args
        == [
            "--bind",
            env.runtime_root.to_str().unwrap(),
            env.runtime_root.to_str().unwrap()
        ]));
    let var_tmp = tempfile::tempdir_in("/var/tmp").unwrap();
    inputs.cwd = var_tmp.path();
    let plan = rimz::sandbox::plan(&inputs).unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    let argv = rimz::sandbox::bwrap_argv(Path::new("/usr/bin/bwrap"), &plan.plan, inputs.cwd, &[]);
    let cwd = var_tmp.path().to_str().unwrap();
    assert!(
        argv.windows(3).any(|args| args == ["--bind", cwd, cwd]),
        "a required path under /var/tmp stays reachable"
    );
    for mount in ["/tmp", "/var/tmp"] {
        inputs.cwd = Path::new(mount);
        assert!(matches!(
            rimz::sandbox::plan(&inputs),
            Err(SandboxErr::TmpCollision { path }) if path == Path::new(mount)
        ));
    }
}

#[test]
fn sandboxed_exec_shows_profile_skill_view_and_shared_temp_unit() {
    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    enable(&env);
    let cwd = env
        .store()
        .paths()
        .temp_unit_dir(Some("scout"))
        .join("clean");
    std::fs::create_dir_all(&cwd).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(cwd.parent().unwrap())
            .status()
            .unwrap()
            .success()
    );
    for dir in [
        ".claude/skills/a",
        ".agents/skills/b",
        ".agents/skills/c",
        ".agents/skills/d/agents",
        ".codex",
    ] {
        std::fs::create_dir_all(env.home_root.join(dir)).unwrap();
    }
    for name in ["b", "c", "d"] {
        std::fs::write(
            env.home_root
                .join(".agents/skills")
                .join(name)
                .join("SKILL.md"),
            "skill body\n",
        )
        .unwrap();
    }
    std::fs::write(env.home_root.join(".codex/config.toml"), "sandbox-test").unwrap();
    let bad = env.home_root.join(".agents/skills/d/agents/openai.yaml");
    let metadata = "policy: *alias\n";
    std::fs::write(&bad, metadata).unwrap();
    std::os::unix::fs::symlink(
        env.home_root.join(".agents/skills/b"),
        env.home_root.join(".claude/skills/b"),
    )
    .unwrap();
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        shim_dir.join("codex"),
        r#"#!/bin/sh
set -eu
ls "$HOME/.claude/skills" > /tmp/claude-skills
printf '%s\n' "$PWD" > /tmp/provider-cwd
ls "$HOME/.agents/skills" > /tmp/agent-skills
printf '%s\n' "$TMPDIR" > /tmp/tmpdir
printf '%s\n' "$HOME" > /tmp/provider-home
printf '%s' "$RIMZ_TEST_NON_UTF8" > /tmp/non-utf8
test "$CLAUDE_CONFIG_DIR" = "$HOME/elsewhere"
test "$SCCACHE_CLIENT_SIDE" = 1
test "${CODEX_HOME+x}" != x
test ! -e "$HOME/.agents/skills/b/agents/openai.yaml"
test "$(cat "$HOME/.agents/skills/c/agents/openai.yaml")" = 'policy:
  allow_implicit_invocation: false'
test -d "$XDG_RUNTIME_DIR/rimz/ws/$RIMZ_TEST_WORKSPACE_DIR"
test -c /dev/null
test "$RIMZ_SHARED" = "$RIMZ_TEST_SHARED"
test -d "$RIMZ_SHARED"
printf parent > /tmp/same-name
test "$(cat /var/tmp/same-name)" = parent
test "$(cat "$HOME/.codex/config.toml")" = sandbox-test
if touch "$HOME/.agents/skills/b/changed" 2>/dev/null; then exit 1; fi
test ! -e "$RIMZ_TEST_HOST_TMP_FILE"
test ! -e "$RIMZ_TEST_HOST_VAR_TMP_FILE"
"#,
    )
    .unwrap();
    let shell = write_fake_login_shell(
        &env,
        "sandbox-shell",
        &[("TMPDIR", "wrong-shell-tmp"), ("SCCACHE_CLIENT_SIDE", "0")],
    );
    let shell_body = std::fs::read_to_string(&shell).unwrap();
    std::fs::write(&shell, shell_body.replacen("#!/bin/sh\n", "#!/bin/sh\nexport CLAUDE_CONFIG_DIR=$HOME/elsewhere\nexport CODEX_HOME=$HOME/elsewhere\nexport XDG_RUNTIME_DIR=/tmp/wrong-runtime\nexport HOME=/tmp/evil\n", 1)).unwrap();
    let host_tmp = tempfile::NamedTempFile::new_in("/tmp").unwrap();
    let host_var_tmp = tempfile::NamedTempFile::new_in("/var/tmp").unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    request.skills = Some(vec!["b".parse().unwrap()]);
    request.identity.name = Some("scout".to_owned());
    let output = env
        .rimz()
        .args(exec_args(&env, &request))
        .current_dir(&cwd)
        .envs(rimz::workspace::pin_env(
            &env.workspace_id,
            &env.project_root,
        ))
        .env("PATH", path_with_front(&shim_dir))
        .env("SHELL", shell)
        .env("RIMZ_TEST_HOST_TMP_FILE", host_tmp.path())
        .env("RIMZ_TEST_HOST_VAR_TMP_FILE", host_var_tmp.path())
        .env("RIMZ_TEST_SHARED", &env.store().paths().room_shared_dir)
        .env(
            "RIMZ_TEST_WORKSPACE_DIR",
            env.state_path_for(&env.project_root).dir_name.as_str(),
        )
        .env(
            "RIMZ_TEST_NON_UTF8",
            std::ffi::OsString::from_vec(vec![0xff, 0xfe]),
        )
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "sandbox skill view: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("rimz: starting without skill \"d\""));
    assert_eq!(std::fs::read(&bad).unwrap(), metadata.as_bytes());
    let store = env.store();
    let tmp = &store.paths().temp_unit_dir(Some("scout"));
    assert_eq!(
        std::fs::read_to_string(tmp.join("provider-cwd")).unwrap(),
        "/tmp/clean\n"
    );
    assert_eq!(std::fs::read(tmp.join("non-utf8")).unwrap(), [0xff, 0xfe]);
    assert_eq!(
        std::fs::read_to_string(tmp.join("claude-skills")).unwrap(),
        "a\nb\n"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.join("agent-skills")).unwrap(),
        "b\nc\n"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.join("tmpdir")).unwrap(),
        "/tmp\n"
    );
    assert!(env.home_root.join(".agents/skills/c").is_dir());
    assert!(!env.home_root.join(".agents/skills/c/agents").exists());
    assert!(env.home_root.join(".claude/skills/b").is_symlink());
    assert_eq!(
        std::fs::read_to_string(tmp.join("provider-home"))
            .unwrap()
            .trim(),
        env.home_root.to_str().unwrap()
    );

    assert_eq!(
        std::fs::read_to_string(tmp.join("same-name")).unwrap(),
        "parent"
    );

    // A launched child shares its parent's unit, found through the parent's row.
    let workspace = env.resolve_workspace(&env.project_root);
    store
        .append_event(&rimz::store::event::EventEnvelope::agent_launched(
            env.workspace_id.clone(),
            &workspace.session_name,
            &AgentKind::new_unchecked("codex"),
            rimz::store::event::AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "scout-session".into(),
                launch_id: Some("scout-launch".into()),
                agent_name: "scout".to_owned(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams::default(),
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: None,
                runtime_owner: None,
                worktree_path: None,
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    std::fs::write(
        shim_dir.join("codex"),
        "#!/bin/sh\nset -eu\ntest \"$(cat /tmp/same-name)\" = parent\nprintf child > /tmp/child-file\n",
    )
    .unwrap();
    request.subagent = true;
    request.identity.name = Some("otter".to_owned());
    request.identity.params.parent_agent_id = Some("scout-launch".into());
    request.identity.params.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
    env.rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .assert_success_within_timeout("sandbox subagent shares its parent's tmp");
    assert_eq!(
        std::fs::read_to_string(tmp.join("child-file")).unwrap(),
        "child"
    );

    // A peer gets a unit of its own.
    std::fs::write(
        shim_dir.join("codex"),
        "#!/bin/sh\nset -eu\ntest ! -e /tmp/same-name\ntest ! -e /var/tmp/child-file\nprintf peer > /tmp/peer-file\n",
    )
    .unwrap();
    request.subagent = false;
    request.identity.name = Some("fox".to_owned());
    request.identity.params.parent_agent_id = None;
    request.identity.params.parent_agent_kind = None;
    env.rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .assert_success_within_timeout("sandbox peer sees its own tmp");
    assert_eq!(
        std::fs::read_to_string(store.paths().temp_unit_dir(Some("fox")).join("peer-file"))
            .unwrap(),
        "peer"
    );
}

#[test]
fn sandboxed_exec_uses_probed_bwrap_with_trusted_path() {
    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    enable(&env);
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        shim_dir.join("codex"),
        "#!/bin/sh\nprintf '%s' \"$PATH\" > /tmp/provider-path\n",
    )
    .unwrap();
    env.write_config(
        &env.project_root,
        &format!(
            "[[agents]]\nname = \"codex\"\nenv = {{ PATH = {:?} }}\n",
            shim_dir.to_str().unwrap()
        ),
    );
    env.rimz()
        .args(["trust", "grant"])
        .assert_success_within_timeout("grant trusted provider PATH");
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    env.rimz()
        .args(exec_args(&env, &request))
        .env("SHELL", "/definitely/not/a/shell")
        .assert_success_within_timeout("sandbox with provider-only PATH");
    assert_eq!(
        std::fs::read_to_string(
            env.store()
                .paths()
                .temp_unit_dir(None)
                .join("provider-path")
        )
        .unwrap(),
        shim_dir.to_str().unwrap()
    );
}

#[test]
fn a_shared_account_writes_its_history_into_the_default_home_under_both_isolations() {
    for (isolation, empty_dir) in [("host", false), ("scoped", false), ("scoped", true)] {
        if isolation == "scoped" && !available() {
            continue;
        }
        let env = Env::new();
        env.record(&env.project_root);
        let work = env.home_root.join("work");
        let native = if empty_dir {
            std::fs::create_dir(env.home_root.join(".git")).unwrap();
            let native = env.home_root.join("accounts/codex/default");
            std::fs::create_dir_all(&native).unwrap();
            std::os::unix::fs::symlink(&native, env.home_root.join(".codex")).unwrap();
            native
        } else {
            env.home_root.join(".codex")
        };
        std::fs::create_dir_all(native.join("sessions")).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("auth.json"), "work").unwrap();
        let config = env.rimz_home().join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            config,
            format!(
                "[agents]\nisolation = {isolation:?}\n[accounts.codex.work]\nhome = {:?}\n",
                work.to_str().unwrap()
            ),
        )
        .unwrap();
        let shim_dir = write_env_dump_shim(&env, "codex");
        std::fs::write(
            shim_dir.join("codex"),
            "#!/bin/sh\nprintf turn > \"$CODEX_HOME/sessions/rollout.jsonl\"\n\
             printf '%s' \"$CODEX_SQLITE_HOME\" > \"$CODEX_HOME/sessions/databases\"\n",
        )
        .unwrap();
        env.write_config(
            &env.project_root,
            &format!(
                "[[agents]]\nname = \"codex\"\nenv = {{ PATH = {:?} }}\n",
                path_with_front(&shim_dir).to_str().unwrap()
            ),
        );
        env.rimz()
            .args(["trust", "grant"])
            .assert_success_within_timeout("grant trusted provider PATH");
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
        request.identity.params.login = Some("work".parse().unwrap());
        let cwd = if empty_dir {
            let cwd = env.store().paths().empty_agent_dir("reader");
            std::fs::create_dir_all(&cwd).unwrap();
            cwd
        } else {
            env.project_root.clone()
        };
        env.rimz()
            .current_dir(&cwd)
            .arg("--root")
            .arg(&env.project_root)
            .args(exec_args(&env, &request))
            .env_remove("CODEX_HOME")
            .env("SHELL", "/definitely/not/a/shell")
            .assert_success_within_timeout("launch on a shared account");
        assert_eq!(
            std::fs::read_link(work.join("sessions")).unwrap(),
            Path::new("../.codex/sessions"),
            "{isolation}"
        );
        assert_eq!(
            std::fs::read_to_string(native.join("sessions/rollout.jsonl")).unwrap(),
            "turn",
            "{isolation}"
        );
        assert_eq!(
            std::fs::read_to_string(native.join("sessions/databases")).unwrap(),
            env.home_root.join(".codex").to_str().unwrap(),
            "{isolation}"
        );
        assert!(!work.join("auth.json").is_symlink(), "{isolation}");
        assert!(!native.join("auth.json").exists(), "{isolation}");
    }
}

#[test]
fn a_sandboxed_named_account_reaches_a_default_home_under_tmp_outside_home() {
    if !available() {
        return;
    }
    // A shared account links its history into the provider's own home, and a
    // standalone one its settings: both reach through the account home.
    for history in ["shared", "standalone"] {
        let env = Env::new();
        env.record(&env.project_root);
        let work = env.home_root.join("work");
        // The provider's own home, moved by its native override to a `/tmp`
        // path no rebound root contains: the temp unit mounted over `/tmp`
        // hides it unless the mount plan rebinds it.
        let native_root = tempfile::tempdir_in("/tmp").unwrap();
        let native = native_root.path().join("codex-home");
        assert!(!native.starts_with(&env.home_root));
        std::fs::create_dir_all(native.join("sessions")).unwrap();
        std::fs::write(native.join("AGENTS.md"), "native manual").unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let config = env.rimz_home().join("config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(
            config,
            format!(
                "[agents]\nisolation = \"scoped\"\n[accounts.codex.work]\nhome = {:?}\nhistory = {history:?}\n",
                work.to_str().unwrap()
            ),
        )
        .unwrap();
        let shim_dir = write_env_dump_shim(&env, "codex");
        std::fs::write(
            shim_dir.join("codex"),
            "#!/bin/sh\ncat \"$CODEX_HOME/AGENTS.md\" > \"$CODEX_HOME/seen\"\n\
             if [ -d \"$CODEX_HOME/sessions\" ]; then printf turn > \"$CODEX_HOME/sessions/rollout.jsonl\"; fi\n",
        )
        .unwrap();
        env.write_config(
            &env.project_root,
            &format!(
                "[[agents]]\nname = \"codex\"\nenv = {{ PATH = {:?} }}\n",
                path_with_front(&shim_dir).to_str().unwrap()
            ),
        );
        env.rimz()
            .args(["trust", "grant"])
            .assert_success_within_timeout("grant trusted provider PATH");
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
        request.identity.params.login = Some("work".parse().unwrap());
        env.rimz()
            .args(exec_args(&env, &request))
            .env("CODEX_HOME", &native)
            .env("SHELL", "/definitely/not/a/shell")
            .assert_success_within_timeout("launch on a named account");
        assert_eq!(
            std::fs::read_link(work.join("AGENTS.md")).unwrap(),
            native.join("AGENTS.md"),
            "{history}"
        );
        assert_eq!(
            std::fs::read_to_string(work.join("seen")).unwrap(),
            "native manual",
            "{history}"
        );
        if history == "shared" {
            assert_eq!(
                std::fs::read_link(work.join("sessions")).unwrap(),
                native.join("sessions")
            );
            assert_eq!(
                std::fs::read_to_string(native.join("sessions/rollout.jsonl")).unwrap(),
                "turn"
            );
        } else {
            assert!(!work.join("sessions").is_symlink());
        }
    }
}

#[test]
fn sandbox_skills_under_host_use_provider_switches() {
    let env = Env::new();
    env.record(&env.project_root);
    let shell = write_fake_login_shell(&env, "host-skills-shell", &[]);
    let probe = env.home_root.join("provider-env");
    let library_skill = env.agents_home().join("skills/unlisted-dir");
    std::fs::create_dir_all(&library_skill).unwrap();
    std::fs::write(
        library_skill.join("SKILL.md"),
        "---\nname: provider-name\n---\nSkill.\n",
    )
    .unwrap();
    let settings = env.home_root.join("settings.json");
    std::fs::write(
        &settings,
        r#"{"env":{"ANTHROPIC_API_KEY":"sk-secret-123"},"theme":"dark","skillOverrides":{"kept":"enabled"}}"#,
    )
    .unwrap();
    for kind in ["codex", "amp", "claude"] {
        if kind == "claude" {
            for name in ["rimz-probe", "commit"] {
                let dir = env.agents_home().join("skills").join(name);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(
                    dir.join("SKILL.md"),
                    format!("---\nname: {name}\ndescription: test\n---\nSkill."),
                )
                .unwrap();
            }
        }
        let shim_dir = write_env_dump_shim(&env, kind);
        let args = if kind == "claude" {
            vec!["--settings".into(), settings.display().to_string()]
        } else {
            Vec::new()
        };
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), args);
        request.identity.name = Some(format!("host-{kind}"));
        let unit = env
            .store()
            .paths()
            .temp_unit_dir(request.identity.name.as_deref());
        let mut lists = vec![vec!["missing-skill"], vec![]];
        if kind == "claude" {
            lists.push(vec!["commit"]);
        }
        for skills in lists {
            let missing = skills.contains(&"missing-skill");
            let commit_listed = skills.contains(&"commit");
            request.skills = Some(skills.iter().map(|name| name.parse().unwrap()).collect());
            // The listed-skill Claude launch is also fast: its rendered
            // `fastMode` member lands in the same one settings file.
            let fast = commit_listed;
            if kind == "claude" {
                let mut args = vec!["--settings".to_owned(), settings.display().to_string()];
                if fast {
                    args.extend(["--settings".to_owned(), r#"{"fastMode":true}"#.to_owned()]);
                }
                *request.action.extra_args_mut() = args;
            }
            let output = env
                .rimz()
                .args(exec_args(&env, &request))
                .env("SHELL", &shell)
                .env("PATH", path_with_front(&shim_dir))
                .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
                .bounded_output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if kind != "amp" && missing {
                assert!(!output.status.success(), "missing host skill must refuse");
                assert!(stderr.contains("missing-skill"), "{stderr}");
                assert!(
                    stderr.contains(if kind == "claude" {
                        ".claude/skills"
                    } else {
                        ".agents/skills"
                    }),
                    "{stderr}"
                );
                assert!(
                    stderr.contains(&env.agents_home().join("skills").display().to_string()),
                    "{stderr}"
                );
                continue;
            }
            if kind == "amp" {
                assert!(stderr.contains("no per-launch skill switch"), "{stderr}");
            }
            assert!(
                output.status.success(),
                "host launch applies profile skills for {kind}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let dump = std::fs::read_to_string(&probe).unwrap();
            let argv: Vec<_> = dump
                .lines()
                .filter(|line| line.starts_with("ARGV_"))
                .map(|line| line.split_once('=').unwrap().1)
                .collect();
            if kind == "codex" {
                assert!(
                    argv.contains(&"skills.config=[{name=\"provider-name\",enabled=false}]"),
                    "{argv:?}"
                );
            } else if kind == "claude" {
                use std::os::unix::fs::PermissionsExt;
                assert!(!dump.contains("sk-secret-123"));
                assert_eq!(argv.iter().filter(|arg| **arg == "--settings").count(), 1);
                let index = argv.iter().position(|arg| *arg == "--settings").unwrap();
                let path = Path::new(argv[index + 1]);
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                let mut settings: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                let routine = settings.as_object_mut().unwrap();
                let permissions = routine.remove("permissions").unwrap();
                let allow = permissions["allow"].as_array().unwrap();
                assert!(allow.contains(&serde_json::json!("Skill(rimz-probe)")));
                assert_eq!(
                    allow.contains(&serde_json::json!("Skill(commit)")),
                    commit_listed
                );
                assert!(!allow.contains(&serde_json::json!("Skill(unlisted-dir)")));
                assert!(routine.remove("autoMode").is_some(), "{routine:?}");
                assert!(
                    routine.contains_key("sandbox"),
                    "every launch needs sandbox room grants: {routine:?}"
                );
                let sandbox = routine.remove("sandbox").unwrap();
                assert!(
                    sandbox["filesystem"]["allowWrite"]
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!(env.store().paths().root))
                );
                assert!(
                    sandbox["excludedCommands"]
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!("rimz message *"))
                );
                assert!(sandbox.get("enableWeakerNestedSandbox").is_none());
                let mut overrides = serde_json::json!({"kept":"enabled", "unlisted-dir":"user-invocable-only", "rimz-probe":"user-invocable-only"});
                if !commit_listed {
                    overrides["commit"] = serde_json::json!("user-invocable-only");
                }
                let mut expected = serde_json::json!({"env":{"ANTHROPIC_API_KEY":"sk-secret-123"}, "theme":"dark", "skillOverrides":overrides});
                if fast {
                    expected["fastMode"] = serde_json::json!(true);
                }
                assert_eq!(settings, expected);
            }
            assert!(
                dump.lines()
                    .any(|line| line == format!("TMPDIR={}", unit.display())),
                "host launch exports its temp unit's host path for {kind}"
            );
            assert_eq!(
                dump.lines()
                    .any(|line| line == format!("CLAUDE_CODE_TMPDIR={}", unit.display())),
                kind == "claude",
                "only Claude's own temp root follows the unit, {kind}"
            );
            assert!(unit.is_dir());
            std::fs::remove_file(&probe).unwrap();
            assert!(!env.store().paths().skills_dir.exists());
        }
    }
}

/// A launch inside another agent's tree unsets the temp root that agent listed
/// unless it owns the key, so a nested provider never writes into the parent's
/// unit. `SHELL` is set, so the unset also survives shell startup.
#[test]
fn a_nested_launch_drops_a_parent_temp_root_it_does_not_own() {
    let env = Env::new();
    env.record(&env.project_root);
    let shell = write_fake_login_shell(&env, "nested-temp-shell", &[]);
    let probe = env.home_root.join("provider-env");
    let parent = env.home_root.join("parent-unit");
    std::fs::create_dir_all(&parent).unwrap();
    for kind in ["codex", "claude"] {
        let shim_dir = write_env_dump_shim(&env, kind);
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        request.identity.name = Some(format!("nested-{kind}"));
        let unit = env
            .store()
            .paths()
            .temp_unit_dir(request.identity.name.as_deref());
        let output = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&shim_dir))
            .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
            .env("CLAUDE_CODE_TMPDIR", &parent)
            .env("RIMZ_TEMP_ROOT_KEYS", "CLAUDE_CODE_TMPDIR")
            .bounded_output()
            .unwrap();
        assert!(
            output.status.success(),
            "{kind}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let dump = std::fs::read_to_string(&probe).unwrap();
        let line = |key: &str| {
            dump.lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .map(str::to_owned)
        };
        if kind == "codex" {
            assert_eq!(line("CLAUDE_CODE_TMPDIR"), None, "{dump}");
            assert_eq!(line("RIMZ_TEMP_ROOT_KEYS").as_deref(), Some(""), "{dump}");
        } else {
            assert_eq!(
                line("CLAUDE_CODE_TMPDIR"),
                Some(unit.display().to_string()),
                "{dump}"
            );
            assert_eq!(
                line("RIMZ_TEMP_ROOT_KEYS").as_deref(),
                Some("CLAUDE_CODE_TMPDIR"),
                "{dump}"
            );
        }
        std::fs::remove_file(&probe).unwrap();
    }
}

#[test]
fn provider_sandbox_no_switch_warning_reaches_stderr_and_agent_card() {
    for from_config in [false, true] {
        let env = Env::new();
        env.record(&env.project_root);
        if from_config {
            std::fs::write(
                env.rimz_home().join("config.toml"),
                "[agents]\nisolation = 'sandbox'\n",
            )
            .unwrap();
        }
        let shell = write_fake_login_shell(&env, "warning-shell", &[]);
        let shim = write_env_dump_shim(&env, "amp");
        let probe = env.home_root.join("warning-provider-env");
        let workspace = env.resolve_workspace(&env.project_root);
        env.store()
            .append_event(&rimz::store::event::EventEnvelope::new(
                env.workspace_id.clone(),
                &workspace.session_name,
                "amp",
                "agent",
                "agent.launched",
                serde_json::json!({"agent_id": "launch-warning", "launch_id": "launch-warning",
            "agent_name": "warning-agent", "agent_name_explicit": true, "state": "starting"}),
            ))
            .unwrap();
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("amp"), Vec::new());
        request.identity.launch_id = Some("launch-warning".into());
        request.identity.name = Some("warning-agent".into());
        request.identity.params.isolation = (!from_config).then_some(Isolation::Sandbox);
        let output = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&shim))
            .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
            .bounded_output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        let dump = std::fs::read_to_string(&probe).unwrap();
        assert!(
            dump.lines().any(|line| line == "RIMZ_ISOLATION=sandbox"),
            "{dump}"
        );
        let warnings: Vec<_> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("rimz: "))
            .filter(|text| !text.starts_with("agent "))
            .collect();
        assert_eq!(warnings.len(), 1, "{stderr}");
        let warning = warnings[0];
        for text in ["amp", "no sandbox flag", "host"] {
            assert!(warning.contains(text), "{warning}");
        }
        let shown = env
            .rimz()
            .args(["agents", "show", "@warning-agent", "--json"])
            .bounded_output()
            .unwrap();
        assert!(shown.status.success(), "{shown:?}");
        let report: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(
            report["agent"]["launch_warnings"],
            serde_json::json!(warnings)
        );
    }
}

#[test]
fn launch_warnings_reach_agents_show_and_clean_relaunch_clears_them() {
    let env = Env::new();
    env.record(&env.project_root);
    let shell = write_fake_login_shell(&env, "warning-shell", &[]);
    let shim = write_env_dump_shim(&env, "amp");
    let probe = env.home_root.join("warning-provider-env");
    let workspace = env.resolve_workspace(&env.project_root);
    env.store()
        .append_event(&rimz::store::event::EventEnvelope::new(
            env.workspace_id.clone(),
            &workspace.session_name,
            "amp",
            "agent",
            "agent.launched",
            serde_json::json!({"agent_id": "launch-warning", "launch_id": "launch-warning",
            "agent_name": "warning-agent", "agent_name_explicit": true, "state": "starting"}),
        ))
        .unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("amp"), Vec::new());
    request.identity.launch_id = Some("launch-warning".into());
    request.identity.name = Some("warning-agent".into());
    request.identity.params.isolation = Some(Isolation::Host);
    for skills in [Some(Vec::new()), None] {
        request.skills = skills;
        let output = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("SHELL", &shell)
            .env("PATH", path_with_front(&shim))
            .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
            .bounded_output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stderr}");
        let warnings: Vec<_> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix("rimz: "))
            // The exit hint shares the prefix but is no launch warning.
            .filter(|text| !text.starts_with("agent "))
            .collect();
        if request.skills.is_some() {
            assert!(
                warnings
                    .iter()
                    .any(|text| text.contains("no per-launch skill switch")),
                "{stderr}"
            );
        } else {
            assert!(warnings.is_empty(), "{stderr}");
        }
        let shown = env
            .rimz()
            .args(["agents", "show", "@warning-agent", "--json"])
            .bounded_output()
            .unwrap();
        assert!(
            shown.status.success(),
            "{}",
            String::from_utf8_lossy(&shown.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_eq!(
            report["agent"]["launch_warnings"],
            serde_json::json!(warnings)
        );
    }
}

#[test]
fn unseen_resume_launch_warnings_reach_agents_show() {
    let env = Env::new();
    env.record(&env.project_root);
    let shell = write_fake_login_shell(&env, "warning-shell", &[]);
    let shim = write_env_dump_shim(&env, "amp");
    let probe = env.home_root.join("warning-provider-env");
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("amp"), Vec::new());
    request.action = rimz::harness::launch::ExecAction::Resume {
        session_id: "unseen-warning-session".into(),
        extra_args: Vec::new(),
    };
    request.identity.launch_id = Some("resume-warning-launch".into());
    request.identity.params.isolation = Some(Isolation::Host);
    request.skills = Some(Vec::new());
    let output = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("SHELL", &shell)
        .env("PATH", path_with_front(&shim))
        .env("RIMZ_TEST_AGENT_ENV_DUMP", &probe)
        .env("TMUX_PANE", "%4")
        .bounded_output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    let warnings: Vec<_> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("rimz: "))
        // The exit hint shares the prefix but is no launch warning.
        .filter(|text| !text.starts_with("agent "))
        .collect();
    assert!(
        warnings
            .iter()
            .any(|text| text.contains("no per-launch skill switch")),
        "{stderr}"
    );
    let shown = env
        .rimz()
        .args(["agents", "show", "unseen-warning-session", "--json"])
        .bounded_output()
        .unwrap();
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        report["agent"]["launch_warnings"],
        serde_json::json!(warnings)
    );
}

#[test]
fn host_skill_links_reconcile_only_owned_entries() {
    use rimz::agents::skill_links::{self, Desired, SkillLinkAction};
    use std::os::unix::fs::symlink;

    let env = Env::new();
    let library = env.agents_home().join("skills");
    let root = env.home_root.join(".agents/skills");
    let empty = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert!(empty.is_empty());
    skill_links::apply(&empty).unwrap();
    assert!(!root.exists());
    assert!(!library.exists());
    for name in ["created", "directory", "foreign", "wrong"] {
        std::fs::create_dir_all(library.join(name)).unwrap();
        std::fs::write(library.join(name).join("SKILL.md"), "skill").unwrap();
    }
    std::fs::create_dir_all(library.join("not-a-skill")).unwrap();
    symlink("created", library.join("alias")).unwrap();
    std::fs::create_dir_all(root.join("directory")).unwrap();
    std::fs::write(root.join("directory/keep"), "mine").unwrap();
    symlink("/foreign/missing", root.join("foreign")).unwrap();
    symlink(library.join("gone"), root.join("stale")).unwrap();
    symlink(library.join("created"), root.join("wrong")).unwrap();
    let plan = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert_eq!(plan.shadowed(), ["directory", "foreign"]);
    assert!(plan.actions().windows(2).any(|actions| matches!(actions,
        [SkillLinkAction::Unlink { name }, SkillLinkAction::Link { name: next, .. }]
            if name == "wrong" && next == name
    )));
    let outcome = skill_links::apply(&plan).unwrap();
    assert_eq!((outcome.linked, outcome.unlinked), (3, 2));
    assert!(outcome.shadowed.is_empty());
    for name in ["created", "alias", "wrong"] {
        assert_eq!(
            std::fs::read_link(root.join(name)).unwrap(),
            library.join(name)
        );
    }
    assert!(!root.join("stale").is_symlink());
    assert!(!root.join("not-a-skill").exists());
    let repeated = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert!(!repeated.has_owned_changes());
    assert_eq!(repeated.shadowed(), ["directory", "foreign"]);
    for name in ["directory", "foreign"] {
        std::fs::remove_dir_all(library.join(name)).unwrap();
    }
    assert!(
        skill_links::plan(&root, &library, Desired::Library)
            .unwrap()
            .is_empty()
    );
    let remove = skill_links::plan(&root, &library, Desired::None).unwrap();
    assert!(remove.shadowed().is_empty());
    assert_eq!(skill_links::apply(&remove).unwrap().unlinked, 3);
    assert_eq!(skill_links::apply(&remove).unwrap().unlinked, 0);
    assert_eq!(
        std::fs::read_to_string(root.join("directory/keep")).unwrap(),
        "mine"
    );
    assert_eq!(
        std::fs::read_link(root.join("foreign")).unwrap(),
        Path::new("/foreign/missing")
    );
    symlink(library.join("gone"), root.join("stale")).unwrap();
    std::fs::remove_dir_all(&library).unwrap();
    let stale = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert_eq!(skill_links::apply(&stale).unwrap().unlinked, 1);
}

#[test]
fn host_skill_links_leave_a_library_overlapping_the_provider_root_untouched() {
    use rimz::agents::skill_links::{self, Desired};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    std::fs::create_dir_all(root.join("created")).unwrap();
    std::fs::write(root.join("created/SKILL.md"), "skill").unwrap();
    symlink("created", root.join("alias")).unwrap();
    symlink("loop", root.join("loop")).unwrap();
    std::fs::write(root.join(std::ffi::OsStr::from_bytes(b"\xff")), "mine").unwrap();

    let library = env.home_root.join(".agents/skills");
    for desired in [Desired::Library, Desired::None] {
        let plan = skill_links::plan(&root, &library, desired).unwrap();
        assert!(plan.is_empty(), "{plan:?}");
        skill_links::apply(&plan).unwrap();
    }
    let nested = skill_links::plan(&root, &root.join("created"), Desired::Library).unwrap();
    assert!(nested.is_empty(), "{nested:?}");
    assert_eq!(
        std::fs::read_link(root.join("alias")).unwrap(),
        Path::new("created")
    );

    let separate = env.agents_home().join("skills");
    std::fs::create_dir_all(separate.join("shared")).unwrap();
    std::fs::write(separate.join("shared/SKILL.md"), "skill").unwrap();
    symlink("loop", separate.join("loop")).unwrap();
    let plan = skill_links::plan(&root, &separate, Desired::Library).unwrap();
    assert_eq!(plan.shadowed(), Vec::<String>::new());
    assert_eq!(plan.actions().len(), 1, "{plan:?}");
}

#[test]
fn host_skill_links_apply_tolerates_siblings_and_reports_foreign_races() {
    use rimz::agents::skill_links::{self, Desired};
    use std::os::unix::fs::symlink;

    let env = Env::new();
    let root = env.home_root.join("provider/skills");
    let library = env.agents_home().join("skills");
    std::fs::create_dir_all(library.join("skill")).unwrap();
    std::fs::write(library.join("skill/SKILL.md"), "skill").unwrap();
    let plan = skill_links::plan(&root, &library, Desired::Library).unwrap();
    assert_eq!(skill_links::apply(&plan).unwrap().linked, 1);
    let sibling = skill_links::apply(&plan).unwrap();
    assert_eq!(sibling.linked, 0);
    assert!(sibling.shadowed.is_empty());
    std::fs::remove_file(root.join("skill")).unwrap();
    std::fs::create_dir(root.join("skill")).unwrap();
    assert_eq!(skill_links::apply(&plan).unwrap().shadowed, ["skill"]);
    std::fs::remove_dir(root.join("skill")).unwrap();
    symlink(library.join("gone"), root.join("skill")).unwrap();
    let replace = skill_links::plan(&root, &library, Desired::Library).unwrap();
    std::fs::remove_file(root.join("skill")).unwrap();
    symlink("/foreign/skill", root.join("skill")).unwrap();
    assert_eq!(skill_links::apply(&replace).unwrap().shadowed, ["skill"]);
    assert_eq!(
        std::fs::read_link(root.join("skill")).unwrap(),
        Path::new("/foreign/skill")
    );
}

#[test]
fn host_exec_links_library_into_codex_and_claude_account_roots_once() {
    for kind in ["codex", "claude"] {
        let env = Env::new();
        env.record(&env.project_root);
        let library = env.agents_home().join("skills");
        std::fs::create_dir_all(library.join("shared")).unwrap();
        std::fs::write(library.join("shared/SKILL.md"), "shared skill").unwrap();
        let account = env.home_root.join("named-claude");
        let root = if kind == "claude" {
            account.join("skills")
        } else {
            env.home_root.join(".agents/skills")
        };
        let shell = write_fake_login_shell(&env, "host-skills-shell", &[]);
        let shim_dir = write_env_dump_shim(&env, kind);
        let request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        for first in [true, false] {
            let output = env
                .rimz()
                .args(exec_args(&env, &request))
                .env("CLAUDE_CONFIG_DIR", &account)
                .env("SHELL", &shell)
                .env("PATH", path_with_front(&shim_dir))
                .env(
                    "RIMZ_TEST_AGENT_ENV_DUMP",
                    env.home_root.join("provider-env"),
                )
                .bounded_output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{stderr}");
            assert_eq!(
                stderr.contains(&format!("linked 1 skill(s) into {}", root.display())),
                first,
                "{stderr}"
            );
            assert_eq!(
                std::fs::read_link(root.join("shared")).unwrap(),
                library.join("shared")
            );
            assert!(!env.store().paths().skills_dir.exists());
        }
        assert!(!env.home_root.join(".claude/skills").exists());
    }
}

#[test]
fn host_skill_links_explain_is_read_only_and_reports_shadowed() {
    let env = Env::new();
    let library = env.agents_home().join("skills");
    let root = env.home_root.join(".agents/skills");
    let empty = env
        .rimz()
        .args(["agents", "explain", "codex", "--json"])
        .bounded_output()
        .unwrap();
    assert!(empty.status.success());
    let empty: serde_json::Value = serde_json::from_slice(&empty.stdout).unwrap();
    assert!(empty.get("skill_links").is_none());
    assert!(!root.exists());
    for name in ["shared", "foreign"] {
        std::fs::create_dir_all(library.join(name)).unwrap();
        std::fs::write(library.join(name).join("SKILL.md"), "skill").unwrap();
    }
    std::fs::create_dir_all(root.join("foreign")).unwrap();
    let output = env
        .rimz()
        .args(["agents", "explain", "codex", "--json"])
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["skill_links"]["root"], root.to_str().unwrap());
    assert_eq!(report["skill_links"]["library"], library.to_str().unwrap());
    assert_eq!(
        report["skill_links"]["shadowed"],
        serde_json::json!(["foreign"])
    );
    assert_eq!(
        report["skill_links"]["actions"][0]["Link"]["name"],
        "shared"
    );
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("these entries are yours"))
    );
    let human = env
        .rimz()
        .args(["agents", "explain", "codex"])
        .bounded_output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8_lossy(&human.stdout);
    assert!(human.contains("Skill links"));
    assert!(human.contains("link: shared"));
    assert!(human.contains("shadowed: foreign"));
    assert!(!root.join("shared").exists());
    assert!(!env.store().paths().tmp_dir.exists());
    assert!(!env.store().paths().skills_dir.exists());
}

#[test]
fn sandbox_preserves_host_library_link_without_duplicate_bind() {
    let env = Env::new();
    let library = env.agents_home().join("skills");
    let root = env.home_root.join(".agents/skills");
    std::fs::create_dir_all(library.join("shared")).unwrap();
    std::fs::write(library.join("shared/SKILL.md"), "shared skill").unwrap();
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(library.join("shared"), root.join("shared")).unwrap();
    let native = skill_prepare(
        &env,
        &environment(&env),
        SkillInputs {
            kind: "codex",
            home: Some(root.clone()),
            manual: ManualSkill::OpenAiPolicy,
            callable: None,
        },
    )
    .unwrap();
    assert!(!native.plan.mounts.iter().any(|mount| matches!(mount,
        rimz::sandbox::Mount::RoBind { target, .. } | rimz::sandbox::Mount::Bind { target, .. } if target == &root.join("shared")
    )));
    assert_eq!(
        std::fs::read_link(root.join("shared")).unwrap(),
        library.join("shared")
    );
    std::fs::create_dir_all(library.join("library-only")).unwrap();
    std::fs::write(library.join("library-only/SKILL.md"), "library skill").unwrap();
    let plan = skill_prepare(
        &env,
        &environment(&env),
        SkillInputs {
            kind: "codex",
            home: Some(root.clone()),
            manual: ManualSkill::OpenAiPolicy,
            callable: None,
        },
    )
    .unwrap();
    assert_eq!(plan.plan.mounts.iter().filter(|mount| matches!(mount,
        rimz::sandbox::Mount::Symlink { target, path } if target == &library.join("shared") && path == &root.join("shared")
    )).count(), 1);
    assert!(!plan.plan.mounts.iter().any(|mount| matches!(mount,
        rimz::sandbox::Mount::RoBind { target, .. } | rimz::sandbox::Mount::Bind { target, .. } if target == &root.join("shared")
    )));
    assert!(plan.copies.is_empty());
}

#[test]
fn sandbox_skill_root_symlink_keeps_its_manual_view() {
    if !available() {
        return;
    }
    let env = Env::new();
    let real = env.home_root.join("skill-library");
    for name in ["visible", "manual"] {
        std::fs::create_dir_all(real.join(name)).unwrap();
        std::fs::write(real.join(name).join("SKILL.md"), "skill body\n").unwrap();
    }
    std::fs::create_dir_all(env.home_root.join(".agents")).unwrap();
    let root = env.home_root.join(".agents/skills");
    std::os::unix::fs::symlink(&real, &root).unwrap();
    let vars = environment(&env);
    let state = env.store();
    let plan = rimz::sandbox::plan(&SandboxInputs {
        read_only: Vec::new(),
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        checkout: None,
        tmp_dir: &state.paths().temp_unit_dir(None),
        skills_dir: &state.paths().skills_dir,
        ssh_config_file: &env.home_root.join("missing-ssh-config"),
        ssh_config_copies_dir: &env.home_root.join("ssh-config-copies"),
        provider_home: None,
        provider_home_env_keys: &[],
        default_home: None,
        skills: SkillInputs {
            kind: "claude",
            home: Some(root.clone()),
            manual: ManualSkill::Frontmatter,
            callable: Some(&["visible".parse().unwrap()]),
        },
    })
    .unwrap();
    rimz::sandbox::apply(&plan).unwrap();
    let argv = rimz::sandbox::bwrap_argv(
        &rimz::sandbox::preflight(Isolation::Scoped)
            .unwrap()
            .unwrap(),
        &plan.plan,
        &env.project_root,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "test \"$(cat \"$1/visible/SKILL.md\")\" = 'skill body' && test \"$(cat \"$1/manual/SKILL.md\")\" = '---\ndisable-model-invocation: true\n---\nskill body'".into(),
            "skill-view".into(),
            root.display().to_string(),
        ],
    );
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .envs(&vars)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.is_symlink());
    assert_eq!(
        std::fs::read_to_string(real.join("manual/SKILL.md")).unwrap(),
        "skill body\n"
    );
}

#[test]
fn sandboxed_run_timeout_stops_provider() {
    use rimz::store::run::{RunRecord, RunStatus};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    if !available() {
        return;
    }
    let env = Env::new();
    enable(&env);
    env.record(&env.project_root);
    let store = env.store();
    let mut run = RunRecord::new(
        env.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        rimz::agents::PermissionMode::Auto,
        "timeout task".to_owned(),
        env.project_root.clone(),
    );
    run.status = RunStatus::Running;
    run.subagent = true;
    run.keep = true;
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let shim_dir = write_env_dump_shim(&env, "codex");
    std::fs::write(
        shim_dir.join("codex"),
        "#!/bin/sh\nprintf '%s' \"$$\" > /tmp/provider-pid\nexec /usr/bin/sleep 60\n",
    )
    .unwrap();
    let mut request = ExecRequest::bare_launch(run.kind.clone(), Vec::new());
    request.run_id = Some(run.run_id.clone());
    request.exit_on_run_completion = true;
    let mut child = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid_path = store.paths().temp_unit_dir(None).join("provider-pid");
    let deadline = Instant::now() + Duration::from_secs(10);
    let provider = loop {
        run = rimz::harness::run::load(store.paths(), &run.run_id).unwrap();
        if let Some(provider) = std::fs::read_to_string(&pid_path)
            .ok()
            .and_then(|text| text.parse::<u32>().ok())
            && run.provider_pid.is_some()
        {
            break provider;
        }
        assert!(Instant::now() < deadline, "provider did not become ready");
        assert!(
            child.try_wait().unwrap().is_none(),
            "wrapper exited before provider startup"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let start = rimz::proc::process_start_token(provider).unwrap();
    assert_ne!(
        run.provider_pid,
        Some(provider),
        "supervisor records the bubblewrap wrapper, not the provider child"
    );
    run.deadline_at = Some(jiff::Timestamp::now() - Duration::from_secs(1));
    rimz::harness::run::create(store.paths(), &run).unwrap();
    let timeout = rimz::harness::run_timeout::RunTimeoutRequest {
        workspace_id: env.workspace_id.clone(),
        run_id: run.run_id.clone(),
    };
    env.rimz()
        .args(rimz::child_process::agent_helper_argv(
            "run-timeout",
            &timeout,
        ))
        .assert_success_within_timeout("sandbox provider timeout");
    let deadline = Instant::now() + Duration::from_secs(5);
    while rimz::proc::process_is_live(provider, Some(&start)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let live = rimz::proc::process_is_live(provider, Some(&start));
    let _ = child.kill();
    let _ = child.wait();
    assert!(!live, "provider survived timeout of its bubblewrap parent");
    assert_eq!(
        rimz::harness::run::load(store.paths(), &run.run_id)
            .unwrap()
            .status,
        RunStatus::TimedOut
    );
}

/// A sandboxed agent's detached writer on its `/tmp`, alive after the agent's
/// wrapper and bubblewrap are gone.
struct ViewWriter {
    env: Env,
    pid: u32,
    start: String,
    unit: PathBuf,
}

impl ViewWriter {
    fn start() -> Option<Self> {
        use rimz::store::run::{RunRecord, RunStatus};
        use std::process::Stdio;
        use std::time::{Duration, Instant};

        if !available() {
            return None;
        }
        let env = Env::new();
        enable(&env);
        env.record(&env.project_root);
        let store = env.store();
        let mut run = RunRecord::new(
            env.workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            rimz::agents::PermissionMode::Auto,
            "detached tmp writer".to_owned(),
            env.project_root.clone(),
        );
        run.status = RunStatus::Running;
        run.subagent = true;
        run.keep = true;
        rimz::harness::run::create(store.paths(), &run).unwrap();
        let shim_dir = write_env_dump_shim(&env, "codex");
        // The wrapper probes `--version` on the host, outside the view, and a
        // writer that inherits the shim's stdio would hang the wrapper.
        std::fs::write(
            shim_dir.join("codex"),
            "#!/bin/sh\n\
             [ \"$1\" = --version ] && exit 0\n\
             setsid sh -c 'echo $$ > /tmp/writer-pid; i=0; while :; do i=$((i+1)); mkdir -p /tmp/d$((i%50)); : > /tmp/d$((i%50))/f$i; done' </dev/null >/dev/null 2>&1 &\n\
             echo $$ > /tmp/agent-pid\n\
             exec /usr/bin/sleep 60\n",
        )
        .unwrap();
        let mut request = ExecRequest::bare_launch(run.kind.clone(), Vec::new());
        request.run_id = Some(run.run_id.clone());
        request.exit_on_run_completion = true;
        let mut wrapper = env
            .rimz()
            .args(exec_args(&env, &request))
            .env("PATH", path_with_front(&shim_dir))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let unit = store.paths().temp_unit_dir(None);
        let read_pid = |name: &str| {
            std::fs::read_to_string(unit.join(name))
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        let (pid, agent) = loop {
            if let (Some(pid), Some(agent)) = (read_pid("writer-pid"), read_pid("agent-pid"))
                && unit.join("d3").is_dir()
            {
                break (pid, agent);
            }
            assert!(Instant::now() < deadline, "writer did not start");
            assert!(
                wrapper.try_wait().unwrap().is_none(),
                "wrapper exited before writer startup"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let start = rimz::proc::process_start_token(pid).unwrap();
        wrapper.kill().unwrap();
        wrapper.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while rimz::proc::process_is_live(agent, None) {
            assert!(Instant::now() < deadline, "agent outlived its wrapper");
            std::thread::sleep(Duration::from_millis(20));
        }
        let writer = Self {
            env,
            pid,
            start,
            unit,
        };
        assert!(writer.is_live(), "writer died with its agent");
        Some(writer)
    }

    fn is_live(&self) -> bool {
        rimz::proc::process_is_live(self.pid, Some(&self.start))
    }

    fn reset(&self, hard: bool) -> std::process::Output {
        let mut args = vec!["--mux", "zellij", "reset", "--no-start", "--yes"];
        if hard {
            args.push("--hard");
        }
        self.env.rimz().args(args).bounded_output().unwrap()
    }

    fn assert_hard_reset_ends_it(&self) {
        let output = self.reset(true);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "hard reset failed: {stderr}");
        assert!(stderr.contains("Scoped views: ended "), "{stderr}");
        assert!(!self.is_live(), "writer survived the hard reset");
        let tmp = &self.env.state_path_for(&self.env.project_root).tmp_dir;
        assert!(!tmp.exists(), "tmp/ survived the hard reset");
        assert!(
            !tmp.with_extension("reset").exists(),
            "tmp.reset/ survived the hard reset"
        );
    }
}

#[test]
fn hard_reset_ends_a_detached_writer_inside_a_sandbox_view() {
    let Some(writer) = ViewWriter::start() else {
        return;
    };
    let mut bystander = std::process::Command::new("/usr/bin/sleep");
    bystander.arg("60");
    for (key, value) in writer.env.rimz().get_envs() {
        match value {
            Some(value) => bystander.env(key, value),
            None => bystander.env_remove(key),
        };
    }
    let mut bystander = bystander.spawn().unwrap();
    writer.assert_hard_reset_ends_it();
    let spared = bystander.try_wait().unwrap().is_none();
    let _ = bystander.kill();
    let _ = bystander.wait();
    assert!(spared, "hard reset ended a host process outside any view");
}

#[test]
fn hard_reset_ends_a_writer_an_earlier_failed_reset_left_detached() {
    let Some(writer) = ViewWriter::start() else {
        return;
    };
    let tmp = writer.env.state_path_for(&writer.env.project_root).tmp_dir;
    std::fs::rename(&tmp, tmp.with_extension("reset")).unwrap();
    writer.assert_hard_reset_ends_it();
}

#[test]
fn soft_reset_spares_a_detached_writer_inside_a_sandbox_view() {
    let Some(writer) = ViewWriter::start() else {
        return;
    };
    let output = writer.reset(false);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "soft reset failed: {stderr}");
    assert!(!stderr.contains("Scoped views"), "{stderr}");
    assert!(writer.is_live(), "soft reset ended the writer");
    assert!(writer.unit.join("writer-pid").is_file());
}

#[test]
fn sandboxed_caller_prompt_file_under_tmp_resolves_to_its_temp_unit() {
    if !available() {
        return;
    }
    let env = Env::new();
    env.record(&env.project_root);
    let unit = env.store().paths().temp_unit_dir(Some("scout"));
    std::fs::create_dir_all(&unit).unwrap();
    std::fs::write(unit.join("brief.md"), "Brief.\n").unwrap();
    // The caller writes `/tmp/brief.md` in its view; the launch reads it on the host.
    let output = env
        .rimz()
        .args([
            "agents",
            "explain",
            "codex",
            "--system-prompt-file",
            "/tmp/brief.md",
            "--json",
        ])
        .envs(rimz::workspace::pin_env(
            &env.workspace_id,
            &env.project_root,
        ))
        .env("RIMZ_ISOLATION", "scoped")
        .env(rimz::harness::launch::ENV_AGENT_KIND, "claude")
        .env(rimz::harness::launch::ENV_AGENT_NAME, "scout")
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["prompt"]["sources"][0]["path"],
        unit.join("brief.md")
            .canonicalize()
            .unwrap()
            .to_str()
            .unwrap()
    );
}
