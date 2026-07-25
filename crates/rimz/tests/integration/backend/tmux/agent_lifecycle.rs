#![allow(clippy::print_stdout, clippy::print_stderr)]

use super::support::*;

fn write_sleeping_agent_shim(env: &Env, agent: &str) -> PathBuf {
    let dir = env.home_root.join("agent-bin");
    std::fs::create_dir_all(&dir).expect("mkdir agent bin");
    let path = dir.join(agent);
    std::fs::write(
        &path,
        "#!/bin/bash\n\
         printf ready > \"$RIMZ_TEST_AGENT_READY\"\n\
         exec -a \"${0##*/}\" sleep 300\n",
    )
    .expect("write agent shim");
    chmod_executable(&path);
    dir
}

fn tmux_agent_exec_command(
    env: &Env,
    agent_bin: &Path,
    ready: &Path,
    agent_id: &str,
    worktree: &Path,
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
        worktree_path: Some(worktree.to_path_buf()),
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        identity: rimz::harness::launch::ExecIdentity::default(),
    };
    let exec = rimz::harness::launch::exec_argv(&env.rimz_bin(), &request).expect("exec argv");
    let mut argv = vec![
        "/usr/bin/env".to_owned(),
        format!("XDG_STATE_HOME={}", env.state_root().display()),
        format!("XDG_RUNTIME_DIR={}", env.runtime_root.display()),
        format!("XDG_CONFIG_HOME={}", env.config_root().display()),
        format!("HOME={}", env.home_root.display()),
        "SHELL=/definitely/not/a/shell".to_owned(),
        format!("PATH={path}"),
        format!("RIMZ_TEST_AGENT_READY={}", ready.display()),
        rimz_bin,
        "--mux".to_owned(),
        "tmux".to_owned(),
    ];
    argv.extend(exec.into_iter().skip(1));
    argv
}

fn tmux_direct_resume_command(
    env: &Env,
    agent_bin: &Path,
    ready: &Path,
    kind: &str,
    agent_id: &str,
) -> Vec<String> {
    let path = path_with_front(agent_bin);
    let request = rimz::harness::launch::ExecRequest {
        kind: rimz::ids::AgentKind::new_unchecked(kind),
        action: rimz::harness::launch::ExecAction::Resume {
            session_id: agent_id.to_owned(),
            extra_args: Vec::new(),
        },
        provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: rimz::harness::launch::ExecIdentity::default(),
    };
    let exec = rimz::harness::launch::exec_argv(&env.rimz_bin(), &request).expect("exec argv");
    let mut argv = vec![
        "/usr/bin/env".to_owned(),
        format!("XDG_STATE_HOME={}", env.state_root().display()),
        format!("XDG_RUNTIME_DIR={}", env.runtime_root.display()),
        format!("XDG_CONFIG_HOME={}", env.config_root().display()),
        format!("HOME={}", env.home_root.display()),
        "SHELL=/definitely/not/a/shell".to_owned(),
        format!("PATH={path}"),
        format!("RIMZ_TEST_AGENT_READY={}", ready.display()),
        env.rimz_bin().to_string_lossy().into_owned(),
        "--root".to_owned(),
        env.project_root.to_string_lossy().into_owned(),
        "--mux".to_owned(),
        "tmux".to_owned(),
    ];
    argv.extend(exec.into_iter().skip(1));
    argv
}

fn tmux_failing_agent_exec_command(env: &Env, agent_bin: &Path, launch_id: &str) -> Vec<String> {
    let path = path_with_front(agent_bin);
    let rimz_bin = env.rimz_bin().to_string_lossy().into_owned();
    let request = rimz::harness::launch::ExecRequest {
        kind: rimz::ids::AgentKind::new_unchecked("codex"),
        action: rimz::harness::launch::ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        identity: rimz::harness::launch::ExecIdentity {
            name: Some("pruner".to_owned()),
            launch_id: Some(launch_id.to_owned()),
            params: rimz::agents::LaunchParams {
                team: Some("trim".to_owned()),
                role: Some("pruner".to_owned()),
                ..rimz::agents::LaunchParams::default()
            },
            ..rimz::harness::launch::ExecIdentity::default()
        },
    };
    let exec = rimz::harness::launch::exec_argv(&env.rimz_bin(), &request).expect("exec argv");
    let mut argv = vec![
        "/usr/bin/env".to_owned(),
        format!("XDG_STATE_HOME={}", env.state_root().display()),
        format!("XDG_RUNTIME_DIR={}", env.runtime_root.display()),
        format!("XDG_CONFIG_HOME={}", env.config_root().display()),
        format!("HOME={}", env.home_root.display()),
        "SHELL=/definitely/not/a/shell".to_owned(),
        format!("PATH={path}"),
        rimz_bin,
        "--mux".to_owned(),
        "tmux".to_owned(),
    ];
    argv.extend(exec.into_iter().skip(1));
    argv
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
    wait_for_path_state(path, true, message);
}

fn wait_for_path_absent(path: &Path, message: &str) {
    wait_for_path_state(path, false, message);
}

fn wait_for_path_state(path: &Path, exists: bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() == exists {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("{message}: {}", path.display());
}

fn wait_for_agent_end_observation(env: &Env, agent_id: &str) {
    let key = (AgentKind::new_unchecked("claude"), agent_id.into());
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let projection = env
            .store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .expect("audit projection");
        if projection.ended.contains(&key) {
            return;
        }
        thread::sleep(Duration::from_millis(25));
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

fn git_missing() -> bool {
    Command::new("git").arg("--version").output().is_err()
}

fn init_repo(path: &Path) {
    git(path, &["init", "-b", "main"]);
    git(path, &["config", "user.email", "rimz@example.com"]);
    git(path, &["config", "user.name", "RimZ Test"]);
    std::fs::write(path.join("README.md"), "fixture\n").expect("write fixture");
    git(path, &["add", "README.md"]);
    git(path, &["commit", "-m", "initial"]);
}

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {} failed\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn resumed_lazy_agent_is_addressable_before_provider_registration() {
    require_tmux!();
    let env = Env::new();
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let kind = AgentKind::new_unchecked("codex");
    let agent_id = "sess-reborn-codex";
    let mut observation =
        AgentLifecycleObservation::new(Some(agent_id.into()), LifecycleSignal::Registered);
    observation.agent_name = Some("reborn-coder".to_owned());
    observation.launch.role = Some("coder".to_owned());
    observation.worktree_path = Some(env.project_root.display().to_string());
    observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%99"));
    observation.runtime_owner = Some(rimz::pane::RuntimeOwner::new(
        rimz::pane::RuntimeOwnerKind::Agent,
        agent_id,
        u32::MAX,
        None,
    ));
    let store = env.store();
    rimz::store::live_roster::publish(
        &store.paths().live_roster,
        [(kind.clone(), agent_id.into())].into_iter().collect(),
    )
    .expect("publish pre-crash live roster");
    store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            kind.as_str(),
            "SessionStart",
            &observation,
        ))
        .expect("append pre-crash agent");

    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    server
        .backend
        .ensure_session(&session_opts(
            &workspace.session_name,
            workspace.workspace_id.clone(),
            &workspace.project_root,
            &workspace.worktree_root,
            Some((160, 40)),
        ))
        .expect("ensure pre-crash session");
    server.tmux(&["kill-server"]);
    store
        .append_event(&rimz::EventEnvelope::session_rebirth(
            workspace.workspace_id.clone(),
            &workspace.session_name,
        ))
        .expect("append rebirth boundary");
    std::fs::remove_file(&store.paths().live_roster).expect("consume pre-crash live roster");
    assert!(
        store
            .runtime_projection(rimz::RuntimeScope::Runtime)
            .expect("pre-attach runtime projection")
            .agents
            .is_empty(),
        "dead pre-crash owner should expel the silent lazy session"
    );

    server
        .backend
        .ensure_session(&session_opts(
            &workspace.session_name,
            workspace.workspace_id.clone(),
            &workspace.project_root,
            &workspace.worktree_root,
            Some((160, 40)),
        ))
        .expect("ensure reborn session");
    let agent_bin = write_sleeping_agent_shim(&env, "codex");
    let ready = env.home_root.join("reborn-codex-ready");
    let command = tmux_direct_resume_command(&env, &agent_bin, &ready, "codex", agent_id);
    let (_stub_dir, stub) = sidebar_command_stub();
    server
        .backend
        .open_tab(&TabOptions {
            title: "#reborn".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd { argv: command }])],
            },
            focus: false,
            dock_sidebar: false,
            sidebar: SidebarPaneOptions {
                workspace_id: workspace.workspace_id.clone(),
                project_root: workspace.project_root.clone(),
                cwd: workspace.worktree_root.clone(),
                ..sidebar_opts(&workspace.session_name, stub, Some(160))
            },
        })
        .expect("open resumed agent tab");
    wait_for_path(&ready, "resumed codex shim did not start");

    let projection = store
        .runtime_projection(rimz::RuntimeScope::Runtime)
        .expect("post-attach runtime projection");
    let attached = projection
        .agents
        .iter()
        .find(|agent| agent.agent_id == agent_id)
        .expect("attached session remains runtime-visible");
    let attached_pane = attached
        .pane
        .as_ref()
        .expect("attached pane")
        .pane_id
        .clone();
    assert_eq!(
        attached.pane.as_ref().and_then(|pane| pane.pane_pid),
        attached.runtime_owner.as_ref().map(|owner| owner.pid)
    );

    let mut panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some(workspace.session_name.clone()),
            ..Default::default()
        })
        .expect("list reborn panes")
        .panes;
    for pane in &mut panes {
        let Some(hosted) = pane
            .pane_pid
            .and_then(rimz::proc::hosted_agent_process_for_root)
        else {
            continue;
        };
        pane.hosted_agent_kind = Some(hosted.kind);
        pane.hosted_agent_process_start = Some(hosted.started_at);
    }
    let live_pane = panes
        .iter()
        .find(|pane| pane.pane_id == attached_pane)
        .expect("attached live pane");
    let snapshot = rimz::SidebarSnapshot::build_with_agents(
        workspace.workspace_id.clone(),
        projection.agents.clone(),
        jiff::Timestamp::now(),
    )
    .with_live_panes(panes.clone(), None);
    let bound = snapshot.agent_bound_to_pane(live_pane).unwrap_or_else(|| {
        panic!("resumed pane classifies as agent: attached={attached:?}, live={live_pane:?}")
    });
    let peers: Vec<&rimz::agents::AgentState> = snapshot.pane_bound_roots().collect();
    assert!(
        rimz::harness::target::agent_handle(bound, &peers, true).starts_with("@coder"),
        "resumed session keeps its role address"
    );

    let message = rimz::message::MessageRecord::new_for_card(
        workspace.workspace_id,
        kind,
        agent_id.into(),
        bound.name.clone(),
        "anything".to_owned(),
        true,
        rimz::message::DeliveryGate::Done,
    );
    assert_eq!(
        rimz::message::deliver::explain(
            &message,
            std::slice::from_ref(&message),
            &snapshot,
            jiff::Timestamp::now(),
        )
        .verdict(),
        rimz::message::deliver::DeliveryVerdict::Ready,
        "queued text can wake the lazy resumed provider without a human prompt"
    );
}

#[test]
fn closing_agent_tab_records_end_and_disposes_clean_worktree() {
    require_tmux!();
    if git_missing() {
        return;
    }
    let env = Env::new();
    init_repo(&env.project_root);
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let created = env
        .rimz()
        .args(["worktree", "new", "rimz-clean"])
        .output()
        .expect("spawn worktree new");
    assert!(
        created.status.success(),
        "worktree new failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&created.stdout),
        String::from_utf8_lossy(&created.stderr),
    );
    let worktree = env.home_root.join("project-worktrees").join("rimz-clean");
    assert!(
        worktree.is_dir(),
        "worktree should exist before agent close"
    );
    let agent_id = "sess-clean-worktree";
    let mut observation =
        AgentLifecycleObservation::new(Some(agent_id.into()), LifecycleSignal::Registered);
    observation.agent_name = Some("rimz-clean".to_owned());
    observation.worktree_path = Some(worktree.display().to_string());
    observation.worktree_branch = Some("rimz-clean".to_owned());
    observation.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%99"));
    env.store()
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            "claude",
            "SessionStart",
            &observation,
        ))
        .expect("append registered agent");
    assert_eq!(
        plan_from_env(&env).tabs.len(),
        1,
        "seeded agent should be recoverable",
    );
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    server
        .backend
        .ensure_session(&session_opts(
            &workspace.session_name,
            workspace.workspace_id.clone(),
            &workspace.project_root,
            &workspace.worktree_root,
            Some((160, 40)),
        ))
        .expect("ensure session");
    let agent_bin = write_sleeping_agent_shim(&env, "claude");
    let ready = env.home_root.join("agent-ready-clean");
    let command = tmux_agent_exec_command(&env, &agent_bin, &ready, agent_id, &worktree);
    let (_stub_dir, stub) = sidebar_command_stub();
    server
        .backend
        .open_tab(&TabOptions {
            title: "#rimz-clean".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd { argv: command }])],
            },
            focus: false,
            dock_sidebar: true,
            sidebar: SidebarPaneOptions {
                workspace_id: workspace.workspace_id.clone(),
                project_root: workspace.project_root.clone(),
                cwd: worktree.clone(),
                ..sidebar_opts(&workspace.session_name, stub, Some(160))
            },
        })
        .expect("open agent tab");
    wait_for_path(&ready, "agent shim did not start");
    let target = format!("{}:#rimz-clean", workspace.session_name);
    server.tmux(&["kill-window", "-t", target.as_str()]);
    assert!(
        server
            .backend
            .list_sessions()
            .expect("list sessions")
            .contains(&workspace.session_name),
        "closing one tab must leave the room alive"
    );
    wait_for_agent_end_observation(&env, agent_id);
    assert!(
        plan_from_env(&env).is_empty(),
        "closed agent is removed from resume plan",
    );
    wait_for_path_absent(
        &worktree,
        "clean worktree was not removed after agent tab close",
    );
}

#[test]
fn failing_close_pane_agent_drops_to_shell() {
    require_tmux!();
    let env = Env::new();
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    server
        .backend
        .ensure_session(&session_opts(
            &workspace.session_name,
            workspace.workspace_id.clone(),
            &workspace.project_root,
            &workspace.worktree_root,
            Some((160, 40)),
        ))
        .expect("ensure session");
    let agent_bin = write_failing_agent_shim(&env, "codex", 7);
    let command = tmux_failing_agent_exec_command(&env, &agent_bin, "launch_tmux_failure");
    let shell_marker = env.home_root.join("tmux-failure-shell.marker");
    let (_stub_dir, stub) = sidebar_command_stub();
    server
        .backend
        .open_tab(&TabOptions {
            title: "#rimz-fail".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd { argv: command }])],
            },
            focus: false,
            dock_sidebar: true,
            sidebar: SidebarPaneOptions {
                workspace_id: workspace.workspace_id.clone(),
                project_root: workspace.project_root.clone(),
                cwd: workspace.worktree_root.clone(),
                ..sidebar_opts(&workspace.session_name, stub, Some(160))
            },
        })
        .expect("open agent tab");
    let panes = server.wait_for_panes(&format!("{}:#rimz-fail", workspace.session_name), 1);
    let pane_ids: Vec<PaneId> = panes
        .iter()
        .map(|pane| PaneId::from_parts(MuxName::Tmux, pane.id.clone()))
        .collect();
    assert!(!pane_ids.is_empty(), "expected an agent pane: {panes:?}");
    let (pane_id, capture) = find_pane_with_capture_until(
        &server.backend,
        &pane_ids,
        "rimz agents trim.pruner",
        Duration::from_secs(5),
    );
    assert!(capture.contains("failed to start"), "{capture:?}");
    assert!(capture.contains("rimz agents trim.pruner"), "{capture:?}");
    server
        .backend
        .send_keys(
            &pane_id,
            &format!("printf rimz-shell-ready > {}\n", shell_marker.display()),
        )
        .expect("send shell marker command");
    wait_for_path(&shell_marker, "dropped shell did not run marker command");
    assert!(
        list_session_panes(&server, &workspace.session_name)
            .iter()
            .any(|pane| pane.pane_id == pane_id),
        "clean startup failure should leave the pane open as a shell"
    );
}
