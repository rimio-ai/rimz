use std::path::{Path, PathBuf};

use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};

use crate::common::{Env, write_path_shim};

pub(in crate::backend::zellij) fn write_sleeping_agent_shim(env: &Env, agent: &str) -> PathBuf {
    let dir = env.home_root.join("zellij-agent-bin");
    write_path_shim(
        &dir,
        agent,
        "printf ready > \"$RIMZ_TEST_AGENT_READY\"\n\
         trap 'exit 0' HUP TERM INT\n\
         while :; do sleep 1; done",
    );
    dir
}

pub(in crate::backend::zellij) fn zellij_agent_exec_command(
    env: &Env,
    zellij_runtime: &Path,
    agent_bin: &Path,
    ready: &Path,
    action: ExecAction,
) -> Vec<String> {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![agent_bin.to_path_buf()];
    paths.extend(std::env::split_paths(&original));
    let path = std::env::join_paths(paths).expect("join PATH");
    let request = ExecRequest {
        isolation_default: None,
        kind: rimz::ids::AgentKind::new_unchecked("claude"),
        action,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        subagent: false,
        loop_reminder: None,
        headless: None,
        identity: ExecIdentity::default(),
    };
    let exec = rimz::harness::launch::exec_argv(&env.rimz_bin(), &env.runtime_paths(), &request)
        .expect("exec argv");
    let mut argv = vec![
        "/usr/bin/env".to_owned(),
        format!("RIMZ_HOME={}", env.rimz_home().display()),
        format!("XDG_STATE_HOME={}", env.state_root().display()),
        format!("XDG_RUNTIME_DIR={}", zellij_runtime.display()),
        format!("XDG_CONFIG_HOME={}", env.config_root().display()),
        format!("HOME={}", env.home_root.display()),
        "SHELL=/definitely/not/a/shell".to_owned(),
        format!("PATH={}", path.to_string_lossy()),
        format!("RIMZ_TEST_AGENT_READY={}", ready.display()),
        env.rimz_bin().to_string_lossy().into_owned(),
        "--mux".to_owned(),
        "zellij".to_owned(),
    ];
    argv.extend(exec.into_iter().skip(1));
    argv
}
