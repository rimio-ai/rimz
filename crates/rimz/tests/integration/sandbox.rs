//! Agent mount-view coverage at the real exec boundary.

use std::collections::BTreeMap;
use std::path::Path;

use predicates::str::contains;
use rimz::config::Isolation;
use rimz::harness::launch::ExecRequest;
use rimz::ids::AgentKind;
use rimz::sandbox::{SandboxErr, SandboxInputs};

use crate::common::{
    CommandTimeoutExt, Env, exec_args, path_with_front, write_env_dump_shim, write_fake_login_shell,
};

#[expect(clippy::print_stderr, reason = "optional bubblewrap test dependency")]
fn available() -> bool {
    match rimz::sandbox::preflight(Isolation::Sandbox) {
        Ok(()) => true,
        Err(err) => {
            eprintln!("skipping bubblewrap execution: {err}");
            false
        }
    }
}

fn enable(env: &Env) {
    let path = env.config_root().join("rimz/agents.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "[agents]\nisolation = \"sandbox\"\n").unwrap();
}

fn environment(env: &Env) -> BTreeMap<String, String> {
    [
        ("HOME", env.home_root.clone()),
        ("XDG_RUNTIME_DIR", env.runtime_root.clone()),
        ("XDG_STATE_HOME", env.state_root()),
        ("TMPDIR", "/tmp".into()),
    ]
    .into_iter()
    .map(|(key, path)| (key.to_owned(), path.display().to_string()))
    .collect()
}

#[test]
fn sandbox_prepare_resolves_symlinked_skill_sources() {
    let env = Env::new();
    let root = env.home_root.join(".agents/skills");
    let source = env.home_root.join("library/visible");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(root.join("hidden")).unwrap();
    std::os::unix::fs::symlink(&source, root.join("visible")).unwrap();
    std::os::unix::fs::symlink("missing", root.join("broken")).unwrap();
    let mut vars = environment(&env);
    let claude_home = env.home_root.join(".claude").display().to_string();
    vars.insert("CLAUDE_CONFIG_DIR".to_owned(), claude_home.clone());
    let specs = ["hidden:off".parse().unwrap()];
    let state = env.store();
    let mut inputs = SandboxInputs {
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        scratch_dir: &state.paths().scratch_dir,
        provider_home: None,
        provider_home_env_keys: &["CODEX_HOME"],
        skills: &specs,
    };
    let plan = rimz::sandbox::prepare(&inputs).unwrap();
    assert_eq!(plan.pins["CODEX_HOME"], rimz::sandbox::EnvPin::Unset);
    assert_eq!(
        plan.pins["CLAUDE_CONFIG_DIR"],
        rimz::sandbox::EnvPin::Set(claude_home)
    );
    assert_eq!(
        plan.pins["HOME"],
        rimz::sandbox::EnvPin::Set(env.home_root.display().to_string())
    );
    assert_eq!(
        plan.pins["TMPDIR"],
        rimz::sandbox::EnvPin::Set("/tmp".to_owned())
    );
    assert!(!plan.pins.contains_key("PATH"));
    let argv = rimz::sandbox::bwrap_argv(&plan.plan, inputs.cwd, &["true".into()]);
    assert!(argv.windows(3).any(|args| args
        == [
            "--ro-bind",
            source.to_str().unwrap(),
            root.join("visible").to_str().unwrap()
        ]));
    assert!(
        !argv
            .iter()
            .any(|arg| arg.ends_with("/hidden") || arg.ends_with("/broken"))
    );
    let unknown = ["absent:off".parse().unwrap()];
    inputs.skills = &unknown;
    let err = rimz::sandbox::prepare(&inputs).err().unwrap();
    assert!(matches!(err, SandboxErr::UnknownSkill { .. }));
    assert!(err.to_string().contains(root.to_str().unwrap()));
}

#[test]
fn sandbox_prepare_rebinds_tmp_rooted_runtime() {
    let env = Env::new();
    let vars = environment(&env);
    let state = env.store();
    let mut inputs = SandboxInputs {
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        scratch_dir: &state.paths().scratch_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        skills: &[],
    };
    let plan = rimz::sandbox::prepare(&inputs).unwrap();
    let argv = rimz::sandbox::bwrap_argv(&plan.plan, inputs.cwd, &[]);
    assert!(argv.windows(3).any(|args| args
        == [
            "--bind",
            env.runtime_root.to_str().unwrap(),
            env.runtime_root.to_str().unwrap()
        ]));
    inputs.cwd = Path::new("/tmp");
    assert!(matches!(
        rimz::sandbox::prepare(&inputs),
        Err(SandboxErr::ScratchCollision)
    ));
}

#[test]
fn sandboxed_exec_shows_profile_skill_view_and_room_tmp() {
    if !available() {
        return;
    }
    let env = Env::new();
    enable(&env);
    for dir in [
        ".claude/skills/a",
        ".agents/skills/b",
        ".agents/skills/c",
        ".codex",
    ] {
        std::fs::create_dir_all(env.home_root.join(dir)).unwrap();
    }
    std::fs::write(env.home_root.join(".codex/config.toml"), "sandbox-test").unwrap();
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
ls "$HOME/.agents/skills" > /tmp/agent-skills
printf '%s\n' "$TMPDIR" > /tmp/tmpdir
printf '%s\n' "$HOME" > /tmp/provider-home
test "${CLAUDE_CONFIG_DIR+x}" != x
test "${CODEX_HOME+x}" != x
test -d "$XDG_RUNTIME_DIR/rimz/$RIMZ_TEST_WORKSPACE_ID"
test -c /dev/null
test "$(cat "$HOME/.codex/config.toml")" = sandbox-test
if touch "$HOME/.agents/skills/b/changed" 2>/dev/null; then exit 1; fi
test ! -e "$RIMZ_TEST_HOST_TMP_FILE"
printf '%s\n' shared > /tmp/team-file
"#,
    )
    .unwrap();
    let shell = write_fake_login_shell(&env, "sandbox-shell", &[("TMPDIR", "wrong-shell-tmp")]);
    let shell_body = std::fs::read_to_string(&shell).unwrap();
    std::fs::write(&shell, shell_body.replacen("#!/bin/sh\n", "#!/bin/sh\nexport CLAUDE_CONFIG_DIR=$HOME/elsewhere\nexport CODEX_HOME=$HOME/elsewhere\nexport XDG_RUNTIME_DIR=/tmp/wrong-runtime\nexport HOME=/tmp/evil\n", 1)).unwrap();
    let host_tmp = tempfile::NamedTempFile::new_in("/tmp").unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    request.skills = vec!["c:off".parse().unwrap()];
    let output = env
        .rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .env("SHELL", shell)
        .env("RIMZ_TEST_HOST_TMP_FILE", host_tmp.path())
        .env("RIMZ_TEST_WORKSPACE_ID", env.workspace_id.as_str())
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "sandbox skill view: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let store = env.store();
    let scratch = &store.paths().scratch_dir;
    assert_eq!(
        std::fs::read_to_string(scratch.join("claude-skills")).unwrap(),
        "a\nb\n"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("agent-skills")).unwrap(),
        "b\n"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("tmpdir")).unwrap(),
        "/tmp\n"
    );
    assert!(env.home_root.join(".agents/skills/c").is_dir());
    assert!(env.home_root.join(".claude/skills/b").is_symlink());
    assert_eq!(
        std::fs::read_to_string(scratch.join("provider-home"))
            .unwrap()
            .trim(),
        env.home_root.to_str().unwrap()
    );

    std::fs::write(shim_dir.join("codex"), "#!/bin/sh\nset -eu\ntest \"$(cat /tmp/team-file)\" = shared\nprintf child > /tmp/child-file\n").unwrap();
    request.subagent = true;
    env.rimz()
        .args(exec_args(&env, &request))
        .env("PATH", path_with_front(&shim_dir))
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .assert_success_within_timeout("sandbox subagent shared scratch");
    assert_eq!(
        std::fs::read_to_string(scratch.join("child-file")).unwrap(),
        "child"
    );
}

#[test]
fn sandbox_skills_under_host_refuse_before_provider_exec() {
    use assert_cmd::assert::OutputAssertExt;
    let env = Env::new();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    request.skills = vec!["merge:off".parse().unwrap()];
    env.rimz()
        .args(exec_args(&env, &request))
        .assert()
        .failure()
        .stderr(contains(
            "profile skills need agents.isolation = \"sandbox\"",
        ));
    assert!(!env.store().paths().scratch_dir.exists());
}

#[test]
fn sandbox_skill_root_symlink_keeps_its_filtered_view() {
    if !available() {
        return;
    }
    let env = Env::new();
    let real = env.home_root.join("skill-library");
    for name in ["visible", "hidden"] {
        std::fs::create_dir_all(real.join(name)).unwrap();
    }
    std::fs::create_dir_all(env.home_root.join(".agents")).unwrap();
    let root = env.home_root.join(".agents/skills");
    std::os::unix::fs::symlink(&real, &root).unwrap();
    let vars = environment(&env);
    let state = env.store();
    let plan = rimz::sandbox::prepare(&SandboxInputs {
        env: &vars,
        cwd: &env.project_root,
        project_root: &env.project_root,
        worktree: None,
        scratch_dir: &state.paths().scratch_dir,
        provider_home: None,
        provider_home_env_keys: &[],
        skills: &["hidden:off".parse().unwrap()],
    })
    .unwrap();
    let argv = rimz::sandbox::bwrap_argv(
        &plan.plan,
        &env.project_root,
        &[
            "/bin/sh".into(),
            "-c".into(),
            "test -d \"$1/visible\" && test ! -e \"$1/hidden\"".into(),
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
    assert!(real.join("hidden").exists());
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
    let pid_path = store.paths().scratch_dir.join("provider-pid");
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
