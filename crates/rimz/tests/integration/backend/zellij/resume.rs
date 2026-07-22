use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, PaneId};
use rimz::mux::{
    LayoutPanes, MuxBackend, PaneCmd, SidebarPaneOptions, SidebarWidth, TabOptions, ZellijBackend,
};

use crate::common::{CommandTimeoutExt, Env};

use super::support::*;

#[test]
fn closing_agent_pane_records_end_trace_when_session_survives_without_sidebar() {
    require_zellij!();

    let env = Env::new();
    let workspace = rimz::workspace::WorkspaceResolver::resolve(&env.project_root, None)
        .expect("resolve workspace");
    let worktree = env.project_root.join("rimz-zellij");
    std::fs::create_dir_all(&worktree).expect("mkdir worktree");
    let agent_id = "sess-zellij-closed";
    append_registered_agent(&env, &workspace, agent_id, &worktree);

    let before = plan_from_env(&env);
    assert_eq!(before.tabs.len(), 1, "seeded agent should be recoverable");

    let xdg = scoped_runtime_dir();
    let _cleanup = ScopedSessionCleanup {
        name: workspace.session_name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = SidebarPaneOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        extra_env: Default::default(),
        cwd: workspace.project_root.clone(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(160)),
        detected_view_size: None,
        width_override: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    publish_room_bin(xdg.path(), &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &workspace.session_name, 2);

    let _client = AttachedClient::attach(xdg.path(), &workspace.session_name, 160, 40);

    let agent_bin = write_sleeping_agent_shim(&env, "claude");
    let ready = env.home_root.join("zellij-agent-ready");
    let command = zellij_agent_exec_command(&env, xdg.path(), &agent_bin, &ready, agent_id);
    let tab_name = "#rimz-zellij";
    backend
        .open_tab(&TabOptions {
            title: tab_name.to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd { argv: command }])],
            },
            focus: true,
            dock_sidebar: true,
            sidebar,
        })
        .expect("open agent tab");
    wait_for_path(&ready, "agent shim did not start");

    close_all_sidebar_panes(xdg.path(), &workspace.session_name);
    assert!(
        wait_for_live_session(&backend, &workspace.session_name).contains(&workspace.session_name),
        "session should stay live after removing every sidebar",
    );

    let work = wait_for_named_work_pane_count(xdg.path(), &workspace.session_name, tab_name, 1);
    let pane_id = format!("terminal_{}", work[0].id);
    let closed = scoped_zellij(xdg.path())
        .args([
            "--session",
            &workspace.session_name,
            "action",
            "close-pane",
            "--pane-id",
            &pane_id,
        ])
        .bounded_output()
        .expect("close agent pane");
    assert!(
        closed.status.success(),
        "close-pane failed: {}",
        String::from_utf8_lossy(&closed.stderr),
    );
    assert!(
        wait_for_live_session(&backend, &workspace.session_name).contains(&workspace.session_name),
        "closing one agent pane must leave the room alive",
    );
    wait_for_agent_end_observation(&env, agent_id);

    let after = plan_from_env(&env);
    assert!(
        after.is_empty(),
        "a closed-pane end trace removes the agent from resume candidates",
    );
}

fn append_registered_agent(
    env: &Env,
    workspace: &rimz::ResolvedWorkspace,
    agent_id: &str,
    worktree: &Path,
) {
    let mut observation = rimz::agents::AgentLifecycleObservation::new(
        Some(agent_id.into()),
        rimz::agents::LifecycleSignal::Registered,
    );
    observation.agent_name = Some("zellij-closed-lane".to_owned());
    observation.worktree_path = Some(worktree.display().to_string());
    observation.worktree_branch = Some("zellij-fixes".to_owned());
    observation.pane_id = Some(PaneId::from_parts(MuxName::Zellij, "terminal_99"));
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .expect("append registered agent");
}

fn write_sleeping_agent_shim(env: &Env, agent: &str) -> PathBuf {
    let dir = env.home_root.join("zellij-agent-bin");
    std::fs::create_dir_all(&dir).expect("mkdir agent bin");
    let path = dir.join(agent);
    std::fs::write(
        &path,
        "#!/bin/sh\n\
         printf ready > \"$RIMZ_TEST_AGENT_READY\"\n\
         trap 'exit 0' HUP TERM INT\n\
         while :; do sleep 1; done\n",
    )
    .expect("write agent shim");
    chmod_executable(&path);
    dir
}

fn zellij_agent_exec_command(
    env: &Env,
    zellij_runtime: &Path,
    agent_bin: &Path,
    ready: &Path,
    agent_id: &str,
) -> Vec<String> {
    let path = path_with_front(agent_bin);
    let rimz_bin = env.rimz_bin().to_string_lossy().into_owned();
    let request = rimz::harness::launch::ExecRequest {
        kind: rimz::ids::AgentKind::new_unchecked("claude"),
        action: rimz::harness::launch::ExecAction::Resume {
            session_id: agent_id.to_owned(),
            extra_args: Vec::new(),
        },
        provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        identity: rimz::harness::launch::ExecIdentity::default(),
    };
    let exec = rimz::harness::launch::exec_argv(&env.rimz_bin(), &request).expect("exec argv");
    let mut argv = vec![
        "/usr/bin/env".to_owned(),
        format!("XDG_STATE_HOME={}", env.state_root().display()),
        format!("XDG_RUNTIME_DIR={}", zellij_runtime.display()),
        format!("XDG_CONFIG_HOME={}", env.config_root().display()),
        format!("HOME={}", env.home_root.display()),
        "SHELL=/definitely/not/a/shell".to_owned(),
        format!("PATH={path}"),
        format!("RIMZ_TEST_AGENT_READY={}", ready.display()),
        rimz_bin,
        "--mux".to_owned(),
        "zellij".to_owned(),
    ];
    argv.extend(exec.into_iter().skip(1));
    argv
}

fn close_all_sidebar_panes(xdg: &Path, session: &str) {
    let panes = expect_list_panes(xdg, session);
    let sidebar_ids: Vec<String> = panes
        .panes
        .iter()
        .filter(|pane| pane.is_sidebar())
        .map(|pane| pane.id)
        .map(|id| format!("terminal_{id}"))
        .collect();
    assert!(
        !sidebar_ids.is_empty(),
        "test setup should create at least one sidebar pane",
    );
    for pane_id in sidebar_ids {
        let output = scoped_zellij(xdg)
            .args([
                "--session",
                session,
                "action",
                "close-pane",
                "--pane-id",
                &pane_id,
            ])
            .bounded_output()
            .expect("close sidebar pane");
        assert!(
            output.status.success(),
            "close sidebar pane failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }
    wait_for_no_sidebar_panes(xdg, session);
}

fn wait_for_no_sidebar_panes(xdg: &Path, session: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let has_sidebar = expect_list_panes(xdg, session)
            .panes
            .iter()
            .any(|pane| pane.is_sidebar());
        if !has_sidebar {
            return;
        }
        if Instant::now() >= deadline {
            panic!("session {session} still has a rimz-sidebar pane");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn chmod_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }
}

fn path_with_front(dir: &Path) -> String {
    let original = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(&original));
    std::env::join_paths(paths)
        .expect("join PATH")
        .to_string_lossy()
        .into_owned()
}

fn wait_for_path(path: &Path, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("{message}: {}", path.display());
}

fn wait_for_agent_end_observation(env: &Env, agent_id: &str) {
    let key = (
        rimz::ids::AgentKind::new_unchecked("claude"),
        agent_id.into(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let projection = env
            .store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .expect("audit projection");
        if projection.ended.contains(&key) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("agent.ended observation was not recorded for {agent_id}");
}

fn plan_from_env(env: &Env) -> rimz::harness::resume::ResumePlan {
    let projection = env
        .store()
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("audit projection");
    rimz::harness::resume::plan_resume(
        &projection.agents,
        &projection.ended,
        rimz::harness::resume::ResumeContext {
            project_root: Some(&env.project_root),
            rimz_bin: &env.rimz_bin(),
            profiles: &rimz::config::ProfilesConfig::default(),
            max: rimz::harness::resume::DEFAULT_RESUME_MAX,
        },
        |path| path.is_dir(),
        |_| true,
    )
}
