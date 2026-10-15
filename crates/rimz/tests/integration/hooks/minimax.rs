use super::*;

// Run this test binary as the embedded MiniMax runtime so the real feed can verify its parent comm and read that parent's launch environment on Linux.
#[test]
fn scrubbed_runner_fixture() {
    let Ok(command) = std::env::var("RIMZ_MINIMAX_TEST_HOOK") else {
        return;
    };
    #[cfg(target_os = "linux")]
    assert_eq!(
        std::fs::read_to_string(format!("/proc/{}/comm", std::process::id()))
            .unwrap()
            .trim(),
        "minimax-code",
    );
    let mut hook = Command::new("/bin/sh");
    hook.args(["-c", &command])
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in [
        "PATH", "HOME", "LANG", "TERM", "SHELL", "USER", "TMPDIR", "TEMP", "TMP",
    ] {
        if let Some(value) = std::env::var_os(key) {
            hook.env(key, value);
        }
    }
    for key in [
        "PLUGIN_ROOT",
        "MINIMAX_PLUGIN_ROOT",
        "CLAUDE_PLUGIN_ROOT",
        "CODEX_PLUGIN_ROOT",
        "PLUGIN_DATA",
        "CLAUDE_PLUGIN_DATA",
        "MINIMAX_PROJECT_DIR",
        "CLAUDE_PROJECT_DIR",
        "CODEX_PROJECT_DIR",
    ] {
        hook.env(key, "/fixture");
    }
    let payload = std::env::var("RIMZ_MINIMAX_TEST_PAYLOAD").unwrap();
    let mut child = hook.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.stderr.is_empty(), "{output:?}");
    assert_hook_succeeded_neutral("minimax", output);
    std::io::stdout()
        .write_all(b"minimax-hook-complete\n")
        .unwrap();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut Vec::new()).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn scrubbed_hook_restores_the_room_pane_and_supervised_run() {
    let env = Env::new();
    env.record(&env.project_root);
    env.install_agent_hooks("minimax");
    let document: Value = serde_json::from_slice(
        &std::fs::read(env.home_root.join(".minimax/plugins/rimz/hooks/rimz.json")).unwrap(),
    )
    .unwrap();
    let runner = env.home_root.join("minimax-code");
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &runner).unwrap();
    let mut run = rimz::store::run::RunRecord::new(
        env.workspace_id.clone(),
        rimz::ids::AgentKind::new_unchecked("minimax"),
        rimz::agents::PermissionMode::Auto,
        "go".into(),
        env.project_root.clone(),
    );
    run.agent_id = Some("mvs_root".into());
    rimz::harness::run::create(env.store().paths(), &run).unwrap();
    for (event, extra) in [
        ("SessionStart", json!({"source":"startup"})),
        (
            "UserPromptSubmit",
            json!({"prompt":"go", "turn_id":"turn-1"}),
        ),
        (
            "Stop",
            json!({"turn_id":"turn-1", "last_assistant_message":"done"}),
        ),
    ] {
        let mut payload =
            json!({"hook_event_name":event, "session_id":"mvs_root", "cwd":env.project_root});
        payload
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let command = document["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let template = env.rimz();
        let mut parent = Command::new(&runner);
        for (key, value) in template.get_envs() {
            match value {
                Some(value) => {
                    parent.env(key, value);
                }
                None => {
                    parent.env_remove(key);
                }
            }
        }
        let path = std::env::join_paths(
            std::iter::once(env.rimz_bin().parent().unwrap().to_path_buf()).chain(
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
            ),
        )
        .unwrap();
        parent
            .args([
                "--exact",
                "hooks::minimax::scrubbed_runner_fixture",
                "--nocapture",
            ])
            .current_dir(&env.project_root)
            .env("PATH", path)
            .env("TMUX_PANE", "%77")
            .env("RIMZ_WORKSPACE_ID", env.workspace_id.as_str())
            .env("RIMZ_PROJECT_ROOT", &env.project_root)
            .env("RIMZ_RUN_ID", run.run_id.as_str())
            .env("RIMZ_MINIMAX_TEST_HOOK", command)
            .env("RIMZ_MINIMAX_TEST_PAYLOAD", payload.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = parent.spawn().unwrap();
        let owner_pid = child.id();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let ready = std::io::BufRead::lines(&mut stdout)
            .any(|line| line.unwrap() == "minimax-hook-complete");
        child.stdout = Some(stdout.into_inner());
        if !ready {
            drop(child.stdin.take());
            panic!(
                "{event}: hook runner failed: {:?}",
                child.wait_with_output().unwrap()
            );
        }
        env.drain_hooks();
        let agents = env.store().snapshot_cached().unwrap().agents;
        let agent = agents
            .iter()
            .find(|agent| agent.agent_id.as_str() == "mvs_root")
            .expect("scrubbed hook must land in this room");
        assert_eq!(agent.pane.as_ref().unwrap().pane_id.as_str(), "tmux:%77");
        assert_eq!(
            agent
                .runtime_owner
                .as_ref()
                .expect("hook PID must be attributed to its verified runtime owner")
                .pid,
            owner_pid
        );
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{event}: {output:?}");
    }
    let settled = rimz::harness::run::load(env.store().paths(), &run.run_id).unwrap();
    assert!(
        settled.status.is_terminal(),
        "restored RIMZ_RUN_ID must settle the supervised run"
    );
    assert_eq!(settled.last_message.as_deref(), Some("done"));
}

#[test]
fn installed_detection_ignores_the_mux_config_home_pin() {
    for key in [None, Some("MINIMAX_DATA_DIR"), Some("MAVIS_DATA_DIR")] {
        let env = Env::new();
        let mut install = env.rimz();
        install
            .args(["hooks", "install", "minimax"])
            .env_remove("XDG_CONFIG_HOME");
        let mut login_env = BTreeMap::from([("HOME".into(), env.home_root.display().to_string())]);
        if let Some(key) = key {
            let data = env.home_root.join("selected");
            install.env(key, &data);
            login_env.insert(key.into(), data.display().to_string());
        }
        let output = install.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        login_env.insert(
            "XDG_CONFIG_HOME".into(),
            env.home_root.join(".config").display().to_string(),
        );
        let definition = rimz::agents::registry::find_definition("minimax").unwrap();
        assert!(
            definition.hooks_installed(&login_env),
            "installed() must agree across the runtime env pin: {key:?}"
        );
    }
}

use std::collections::BTreeMap;
