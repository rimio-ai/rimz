#![allow(clippy::print_stdout, clippy::print_stderr)]

use super::support::*;
use rimz::agents::{AgentStatus, LaunchParams};
use rimz::store::event::{AgentLaunchPayload, AgentLaunchState, EventEnvelope};
use rimz::store::writer::AgentLifecycleIntent;

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
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: Some(worktree.to_path_buf()),
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        subagent: false,
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
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
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
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: true,
        exit_on_run_completion: false,
        subagent: false,
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
            max: rimz::config::ResumeConfig::default().max,
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
fn self_wake_steers_to_live_consumer_when_idle_and_working() {
    require_tmux!();
    for status in [AgentStatus::Idle, AgentStatus::Running] {
        let env = Env::new();
        env.install_agent_hooks("claude");
        let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("workspace");
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
        let agent_id = "self-wake-provider";
        let launch_id = "self-wake-launch";
        let agent_bin = write_sleeping_agent_shim(&env, "claude");
        let ready = env.home_root.join("self-wake-ready");
        let command = tmux_direct_resume_command(&env, &agent_bin, &ready, "claude", agent_id);
        let (_stub_dir, stub) = sidebar_command_stub();
        server
            .backend
            .open_tab(&TabOptions {
                title: "#self-wake".to_owned(),
                panes: LayoutPanes {
                    columns: vec![tiled_column(vec![PaneCmd {
                        argv: command,
                        name: None,
                    }])],
                },
                focus: false,
                dock_sidebar: false,
                after: None,
                sidebar: SidebarPaneOptions {
                    workspace_id: workspace.workspace_id.clone(),
                    project_root: workspace.project_root.clone(),
                    cwd: env.project_root.clone(),
                    ..sidebar_opts(&workspace.session_name, stub, Some(160))
                },
            })
            .expect("open live agent pane");
        wait_for_path(&ready, "self-wake agent shim did not start");
        let target = format!("{}:#self-wake", workspace.session_name);
        let pane_id = PaneId::from_parts(MuxName::Tmux, server.display(&target, "#{pane_id}"));
        let store = env.store();
        let kind = AgentKind::new_unchecked("claude");
        store
            .append_event(&EventEnvelope::agent_launched(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                &kind,
                AgentLaunchPayload {
                    agent_id: agent_id.into(),
                    launch_id: Some(launch_id.into()),
                    agent_name: "planner".to_owned(),
                    agent_name_explicit: true,
                    launch: LaunchParams::default(),
                    state: AgentLaunchState::Bound,
                    run_id: None,
                    pane_id: Some(pane_id.clone()),
                    runtime_owner: None,
                    worktree_path: Some(env.project_root.display().to_string()),
                    worktree_branch: None,
                    prompt: None,
                    description: None,
                },
            ))
            .expect("seed bound live target");
        let mut observation =
            AgentLifecycleObservation::new(Some(agent_id.into()), LifecycleSignal::Registered);
        observation.agent_name = Some("planner".to_owned());
        observation.pane_id = Some(pane_id.clone());
        store
            .append_agent_lifecycle(AgentLifecycleIntent {
                session_name: &workspace.session_name,
                agent_kind: kind.clone(),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .expect("register live target");
        if status == AgentStatus::Running {
            let observation =
                AgentLifecycleObservation::new(Some(agent_id.into()), LifecycleSignal::TurnStarted);
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: &workspace.session_name,
                    agent_kind: kind,
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .expect("start working turn");
        }
        let assert_status = || {
            let projection = store
                .runtime_projection(rimz::RuntimeScope::Runtime)
                .expect("live target projection");
            let agent = projection
                .agents
                .iter()
                .find(|agent| agent.agent_id.as_str() == agent_id)
                .expect("live target remains registered");
            assert_eq!(agent.status, status);
            assert_eq!(
                agent.pane.as_ref().expect("live target pane").pane_id,
                pane_id
            );
        };
        assert_status();
        let output = env
            .rimz()
            .env("RIMZ_AGENT_KIND", "claude")
            .env("RIMZ_AGENT_ID", launch_id)
            .env("RIMZ_AGENT_NAME", "planner")
            .args(["--mux", "tmux", "wake", "--", "printf", "self-wake-marker"])
            .bounded_output()
            .expect("arm live self wake");
        assert!(
            output.status.success(),
            "{status:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let capture = capture_pane_until(
            &server.backend,
            &pane_id,
            "self-wake-marker",
            Duration::from_secs(15),
        );
        assert!(capture.contains("Type: WAKE"), "{status:?}: {capture}");
        assert!(
            capture.contains("self-wake-marker"),
            "{status:?}: {capture}"
        );
        assert_status();
    }
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
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: command,
                    name: None,
                }])],
            },
            focus: false,
            dock_sidebar: false,
            after: None,
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
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
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
        rimz::address::agent_handle(bound, &peers, true).starts_with("@coder"),
        "resumed session keeps its role address"
    );

    let message = rimz::store::message::MessageRecord::new_for_card(
        workspace.workspace_id,
        kind,
        agent_id.into(),
        bound.name.clone(),
        "anything".to_owned(),
        true,
        rimz::store::message::DeliveryGate::Done,
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
fn cohort_resume_selects_closed_profile_parent_over_live_child_and_dead_placeholder() {
    require_tmux!();
    if git_missing() {
        return;
    }
    let env = Env::new();
    std::fs::write(env.home_root.join(".zshrc"), "").expect("disable zsh first-run menu");
    init_repo(&env.project_root);
    let worktree = env.home_root.join("project-worktrees/resume");
    git(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-b",
            "resume",
            worktree.to_str().expect("worktree path"),
        ],
    );
    let config_dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    std::fs::write(
        config_dir.join("agents.toml"),
        "[agents.profiles.astra]\nagent = \"codex\"\n",
    )
    .expect("write astra profile");
    let agent_bin = write_sleeping_agent_shim(&env, "codex");
    let argv_path = env.home_root.join("resume-argv");
    let ready = env.home_root.join("resume-ready");
    std::fs::write(
        agent_bin.join("codex"),
        "#!/bin/bash\n\
         case \"$1\" in --version) printf 'codex 0.0.0\\n'; exit 0;; app-server) exit 0;; esac\n\
         printf '%s\\n' \"$@\" >> \"$RIMZ_TEST_AGENT_ARGV\"\n\
         printf ready > \"$RIMZ_TEST_AGENT_READY\"\n\
         exec -a codex sleep 300\n",
    )
    .expect("write argv-recording codex shim");
    let launch_command = || {
        let mut command = env.rimz();
        command
            .current_dir(&worktree)
            .env("PATH", path_with_front(&agent_bin))
            .env("SHELL", "/definitely/not/a/shell")
            .env("RIMZ_TEST_AGENT_ARGV", &argv_path)
            .env("RIMZ_TEST_AGENT_READY", &ready)
            .args(["--mux", "tmux", "agents", "astra", "--resume"])
            .stdin(std::process::Stdio::null());
        command
    };
    let workspace = WorkspaceResolver::resolve(&worktree, None).expect("resolve workspace");
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    let mut options = session_opts(
        &workspace.session_name,
        workspace.workspace_id.clone(),
        &workspace.project_root,
        &workspace.worktree_root,
        Some((160, 40)),
    );
    options
        .extra_env
        .extend(launch_command().get_envs().filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        }));
    server
        .backend
        .ensure_session(&options)
        .expect("ensure room");
    let child_pane = server.stdout(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &workspace.session_name,
        "-n",
        "child",
        "/bin/bash -c 'exec -a codex sleep 300'",
    ]);
    let child_pid = server
        .display(&child_pane, "#{pane_pid}")
        .parse()
        .expect("child pid");
    let parent_id = "cohort-parent-session";
    let child_id = "cohort-child-session";
    let placeholder_id = "launch_019f2cecea067320b667c5946d266e64";
    let store = env.store();
    let kind = AgentKind::new_unchecked("codex");
    for (index, (agent_id, profile)) in [
        (parent_id, "astra"),
        (child_id, "general"),
        (placeholder_id, "astra"),
    ]
    .into_iter()
    .enumerate()
    {
        let is_child = agent_id == child_id;
        let mut event = EventEnvelope::agent_launched(
            workspace.workspace_id.clone(),
            &workspace.session_name,
            &kind,
            AgentLaunchPayload {
                agent_id: agent_id.into(),
                launch_id: Some(agent_id.into()),
                agent_name: if agent_id == placeholder_id {
                    "empty-launch".to_owned()
                } else {
                    profile.to_owned()
                },
                agent_name_explicit: true,
                launch: LaunchParams {
                    profile: Some(profile.to_owned()),
                    parent_agent_id: is_child.then(|| parent_id.into()),
                    launch_depth: Some(if is_child { 2 } else { 1 }),
                    ..LaunchParams::default()
                },
                state: if agent_id == placeholder_id {
                    AgentLaunchState::Starting
                } else {
                    AgentLaunchState::Bound
                },
                run_id: None,
                pane_id: is_child.then(|| PaneId::from_parts(MuxName::Tmux, &child_pane)),
                runtime_owner: Some(rimz::pane::RuntimeOwner::new(
                    rimz::pane::RuntimeOwnerKind::Agent,
                    agent_id,
                    if is_child { child_pid } else { u32::MAX },
                    None,
                )),
                worktree_path: Some(worktree.display().to_string()),
                worktree_branch: Some("resume".to_owned()),
                prompt: None,
                description: None,
            },
        );
        event.timestamp = jiff::Timestamp::from_second(1_700_000_000 + index as i64 * 10).unwrap();
        store.append_event(&event).expect("seed cohort member");
        if agent_id == parent_id {
            let transcript = env.home_root.join("parent.jsonl");
            std::fs::write(&transcript, "{}\n").expect("write parent conversation");
            let mut observation =
                AgentLifecycleObservation::new(Some(parent_id.into()), LifecycleSignal::Ended);
            observation.transcript_path = Some(transcript.display().to_string());
            let mut ended = EventEnvelope::agent_lifecycle(
                workspace.workspace_id.clone(),
                &workspace.session_name,
                "codex",
                "SessionEnd",
                &observation,
            );
            ended.timestamp = jiff::Timestamp::from_second(1_700_000_001).unwrap();
            store
                .append_event(&ended)
                .expect("close parent conversation");
        }
    }
    let projection = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .expect("seeded cohort");
    let child = projection
        .agents
        .iter()
        .find(|agent| agent.agent_id == child_id)
        .expect("child");
    assert!(child.is_launched_child());
    assert!(matches!(
        rimz::store::runtime::agent_liveness(child),
        rimz::store::runtime::AgentLiveness::Live { .. }
    ));
    let parent = projection
        .agents
        .iter()
        .find(|agent| agent.agent_id == parent_id)
        .expect("closed parent");
    assert!(parent.parent_agent_id.is_none() && parent.ended_at.is_some());
    let placeholder = projection
        .agents
        .iter()
        .find(|agent| agent.agent_id == placeholder_id)
        .expect("dead placeholder");
    assert!(placeholder.agent_id.is_provisional());
    assert_eq!(
        rimz::store::runtime::agent_liveness(placeholder),
        rimz::store::runtime::AgentLiveness::Dead
    );
    assert!(parent.last_activity < child.last_activity);
    assert!(child.last_activity < placeholder.last_activity);

    let output = launch_command()
        .bounded_output()
        .expect("resume astra cohort");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    wait_for_path(&ready, "resumed provider did not start");
    let argv = std::fs::read_to_string(&argv_path).expect("provider argv");
    let args = argv.lines().collect::<Vec<_>>();
    assert!(args.starts_with(&["resume", parent_id]), "{argv}");
    assert!(!args.contains(&child_id));
    assert!(!args.contains(&placeholder_id));

    let output = launch_command()
        .bounded_output()
        .expect("refuse live parent resume");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("still live") && stderr.contains(parent_id),
        "{stderr}"
    );
    assert_eq!(
        std::fs::read_to_string(&argv_path).expect("provider argv"),
        argv
    );
}

#[test]
fn fresh_cohort_relaunch_preserves_dirty_checkout_and_does_not_duplicate_live_agents() {
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;

    require_tmux!();
    if git_missing() {
        return;
    }
    let env = Env::new();
    std::fs::write(env.home_root.join(".zshrc"), "").expect("disable zsh first-run menu");
    init_repo(&env.project_root);
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let agent_bin = write_sleeping_agent_shim(&env, "claude");
    let ready = env.home_root.join("cohort-ready");
    std::fs::create_dir(&ready).expect("mkdir readiness records");
    std::fs::write(
        agent_bin.join("claude"),
        format!(
            "#!/bin/bash\nset -e\n\
             printf '{{\"hook_event_name\":\"SessionStart\",\"session_id\":\"cohort-%s\"}}\\n' \"$$\" | \
             RIMZ_AGENT_PID=$$ \"$RIMZ_TEST_RIMZ_BIN\" hooks feed --source claude >/dev/null\n\
             printf ready > '{}/'$$\nexec -a claude sleep 300\n",
            ready.display()
        ),
    )
    .expect("write cohort sleeping shim");
    let launch_command = || {
        let mut command = env.rimz();
        command
            .env("PATH", path_with_front(&agent_bin))
            .env("SHELL", "/definitely/not/a/shell")
            .env("RIMZ_TEST_RIMZ_BIN", env.rimz_bin())
            .args(["--mux", "tmux", "agents", "claude,claude", "-w", "fresh"]);
        command
    };
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    let mut options = session_opts(
        &workspace.session_name,
        workspace.workspace_id.clone(),
        &workspace.project_root,
        &workspace.worktree_root,
        Some((160, 40)),
    );
    options
        .extra_env
        .extend(launch_command().get_envs().filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        }));
    server
        .backend
        .ensure_session(&options)
        .expect("ensure room");
    let run_launch = |fresh: bool| {
        let mut command = launch_command();
        if fresh {
            command.arg("--fresh");
        }
        let output = command
            .stdin(std::process::Stdio::null())
            .bounded_output()
            .expect("run cohort launch");
        assert!(
            output.status.success(),
            "cohort launch failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    };
    let run_prompt = |answer: &str| {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 40,
                cols: 160,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open launch PTY");
        let command = launch_command();
        let mut cmd = CommandBuilder::new(env.rimz_bin());
        env.pin_pty_command(&mut cmd);
        cmd.args(command.get_args());
        cmd.cwd(&env.project_root);
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => cmd.env(key, value),
                None => cmd.env_remove(key),
            }
        }
        let mut child = pair.slave.spawn_command(cmd).expect("spawn launch prompt");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("prompt reader");
        let output = thread::spawn(move || {
            let mut output = Vec::new();
            let _ = reader.read_to_end(&mut output);
            output
        });
        let mut writer = pair.master.take_writer().expect("prompt writer");
        writer.write_all(answer.as_bytes()).expect("answer prompt");
        writer.flush().expect("flush answer");
        drop(writer);
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll prompt") {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            thread::sleep(Duration::from_millis(25));
        };
        drop(pair.master);
        let output =
            String::from_utf8_lossy(&output.join().expect("join prompt reader")).into_owned();
        assert!(status.is_some_and(|status| status.success()), "{output}");
        output
    };
    let agents = || {
        env.store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .expect("audit cohort")
            .agents
    };
    let wait_for_launch = |count: usize| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let rows = agents();
            let ready_count = std::fs::read_dir(&ready)
                .expect("read readiness records")
                .count();
            if ready_count == count
                && rows.iter().filter(|agent| agent.ended_at.is_none()).count() == 2
                && rows.iter().all(|agent| !agent.agent_id.is_provisional())
            {
                return rows;
            }
            assert!(
                Instant::now() < deadline,
                "cohort did not launch: ready={ready_count}, expected={count}, panes={}",
                server.stdout(&[
                    "capture-pane",
                    "-p",
                    "-t",
                    &format!("{}:#fresh", workspace.session_name)
                ])
            );
            thread::sleep(Duration::from_millis(25));
        }
    };

    run_launch(false);
    let original = wait_for_launch(2);
    assert_eq!(original.len(), 2);
    let worktree = env.home_root.join("project-worktrees/fresh");
    std::fs::write(worktree.join("README.md"), "unfinished work\n").expect("dirty checkout");
    std::fs::write(worktree.join("board.md"), "keep this scratch memory\n").expect("write scratch");
    let checkout_inode = std::fs::metadata(&worktree)
        .expect("checkout metadata")
        .ino();
    let git_file = std::fs::read(worktree.join(".git")).expect("checkout git pointer");
    let target = format!("{}:#fresh", workspace.session_name);
    server.tmux(&["kill-window", "-t", &target]);
    for agent in &original {
        wait_for_agent_end_observation(&env, agent.agent_id.as_str());
    }
    let closed = agents();
    assert!(closed.iter().all(|agent| agent.ended_at.is_some()));
    let windows = server.window_names(&workspace.session_name);

    let hint = run_launch(false);
    assert!(hint.contains("has work in progress"), "{hint}");
    assert!(
        hint.contains("rimz agents claude,claude -w fresh --resume"),
        "{hint}"
    );
    assert!(
        hint.contains("rimz agents claude,claude -w fresh --fresh"),
        "{hint}"
    );
    assert_eq!(agents().len(), closed.len(), "hint must not launch agents");
    assert_eq!(server.window_names(&workspace.session_name), windows);

    for answer in ["c\n", "n\n"] {
        let canceled = run_prompt(answer);
        assert!(
            canceled.contains("(resume/fresh/cancel) [resume]"),
            "{canceled}"
        );
        assert!(
            canceled.contains("canceled; nothing launched"),
            "{canceled}"
        );
        assert_eq!(agents().len(), closed.len());
        assert_eq!(server.window_names(&workspace.session_name), windows);
    }

    let fresh = run_prompt("f\n");
    assert!(fresh.contains("fresh in worktree `fresh`"), "{fresh}");
    let relaunched = wait_for_launch(4);
    let live: Vec<_> = relaunched
        .iter()
        .filter(|agent| agent.ended_at.is_none())
        .collect();
    assert_eq!(relaunched.len(), 4, "closed identities remain in the store");
    for agent in &live {
        assert!(!agent.agent_id.is_empty());
        assert!(agent.name.is_some());
        assert!(
            original
                .iter()
                .all(|old| old.agent_id != agent.agent_id && old.name != agent.name)
        );
        let cwd = Path::new(agent.worktree_path.as_deref().expect("agent checkout"));
        assert_eq!(
            cwd.canonicalize().expect("agent cwd"),
            worktree.canonicalize().expect("checkout")
        );
    }
    for old in &closed {
        let retained = relaunched
            .iter()
            .find(|agent| agent.agent_id == old.agent_id)
            .expect("retained closed identity");
        assert_eq!(retained.ended_at, old.ended_at);
    }
    assert_eq!(
        std::fs::metadata(&worktree)
            .expect("checkout metadata")
            .ino(),
        checkout_inode
    );
    assert_eq!(
        std::fs::read(worktree.join(".git")).expect("git pointer"),
        git_file
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).expect("dirty file"),
        "unfinished work\n"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("board.md")).expect("scratch file"),
        "keep this scratch memory\n"
    );

    let panes_before = server.stdout(&[
        "list-panes",
        "-s",
        "-t",
        &workspace.session_name,
        "-F",
        "#{pane_id}",
    ]);
    let already_live = run_launch(true);
    assert!(already_live.contains("already running"), "{already_live}");
    assert_eq!(
        server.stdout(&[
            "list-panes",
            "-s",
            "-t",
            &workspace.session_name,
            "-F",
            "#{pane_id}"
        ]),
        panes_before
    );
    let identities = |rows: &[rimz::agents::AgentState]| {
        rows.iter()
            .map(|agent| (agent.agent_id.clone(), agent.ended_at))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&agents()), identities(&relaunched));

    server.tmux(&["kill-window", "-t", &target]);
    for agent in live {
        wait_for_agent_end_observation(&env, agent.agent_id.as_str());
    }
    let fresh = run_launch(true);
    assert!(fresh.contains("fresh in worktree `fresh`"), "{fresh}");
    let relaunched = wait_for_launch(6);
    assert_eq!(relaunched.len(), 6);

    server.tmux(&["kill-window", "-t", &target]);
    for agent in relaunched.iter().filter(|agent| agent.ended_at.is_none()) {
        wait_for_agent_end_observation(&env, agent.agent_id.as_str());
    }
    std::fs::copy(
        env.project_root.join("README.md"),
        worktree.join("README.md"),
    )
    .expect("restore clean checkout");
    std::fs::remove_file(worktree.join("board.md")).expect("remove test scratch");
    let canceled = run_prompt("\n");
    assert!(
        canceled.contains("(remove/fresh/cancel) [cancel]"),
        "{canceled}"
    );
    assert!(
        canceled.contains("canceled; nothing launched"),
        "{canceled}"
    );
    assert_eq!(agents().len(), 6);
    let fresh = run_prompt("f\n");
    assert!(fresh.contains("fresh in worktree `fresh`"), "{fresh}");
    assert_eq!(wait_for_launch(8).len(), 8);
    assert_eq!(
        std::fs::metadata(&worktree)
            .expect("checkout metadata")
            .ino(),
        checkout_inode
    );
}

#[test]
fn agents_existing_unmanaged_worktree_requires_consent_and_preserves_checkout() {
    require_tmux!();
    if git_missing() {
        return;
    }
    existing_unmanaged_worktree_launch("agents", "claude,claude");
}

#[test]
fn teams_existing_unmanaged_worktree_requires_consent_and_preserves_checkout() {
    require_tmux!();
    if git_missing() {
        return;
    }
    existing_unmanaged_worktree_launch("teams", "duo");
}

fn existing_unmanaged_worktree_launch(doorway: &str, spec: &str) {
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;

    let env = Env::new();
    std::fs::write(env.home_root.join(".zshrc"), "").expect("disable zsh first-run menu");
    init_repo(&env.project_root);
    let worktree = env.home_root.join("project-worktrees/existing");
    git(
        &env.project_root,
        &[
            "worktree",
            "add",
            "-b",
            "user-owned",
            worktree.to_str().expect("checkout path"),
        ],
    );
    std::fs::write(worktree.join("README.md"), "unfinished work\n").expect("dirty checkout");
    std::fs::write(worktree.join("notes.txt"), "user scratch\n").expect("write scratch");
    let checkout_inode = std::fs::metadata(&worktree)
        .expect("checkout metadata")
        .ino();
    let git_file = std::fs::read(worktree.join(".git")).expect("checkout git pointer");
    let config_dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&config_dir).expect("mkdir config");
    std::fs::write(
        config_dir.join("agents.toml"),
        "[agents.profiles.worker]\nagent = \"claude\"\n\
         [agents.teams.duo]\nlayout = \"lead+helper\"\n\
         [[agents.teams.duo.roles]]\nrole = \"lead\"\nprofile = \"worker\"\n\
         [[agents.teams.duo.roles]]\nrole = \"helper\"\nprofile = \"worker\"\n",
    )
    .expect("write configured team");
    let workspace = WorkspaceResolver::resolve(&env.project_root, None).expect("resolve workspace");
    let agent_bin = write_sleeping_agent_shim(&env, "claude");
    let ready = env.home_root.join("existing-ready");
    std::fs::create_dir(&ready).expect("mkdir readiness records");
    std::fs::write(
        agent_bin.join("claude"),
        format!(
            "#!/bin/bash\nprintf ready > '{}/'$$\nexec -a claude sleep 300\n",
            ready.display()
        ),
    )
    .expect("write cohort sleeping shim");
    let launch_command = || {
        let mut command = env.rimz();
        command
            .env("PATH", path_with_front(&agent_bin))
            .env("SHELL", "/definitely/not/a/shell")
            .args(["--mux", "tmux", doorway, spec, "-w", "existing"]);
        command
    };
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    let mut options = session_opts(
        &workspace.session_name,
        workspace.workspace_id.clone(),
        &workspace.project_root,
        &workspace.worktree_root,
        Some((160, 40)),
    );
    options
        .extra_env
        .extend(launch_command().get_envs().filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        }));
    server
        .backend
        .ensure_session(&options)
        .expect("ensure room");
    let agents = || {
        env.store()
            .runtime_projection(rimz::RuntimeScope::Audit)
            .expect("audit agents")
            .agents
    };
    let pane_ids = || {
        server.stdout(&[
            "list-panes",
            "-s",
            "-t",
            &workspace.session_name,
            "-F",
            "#{pane_id}",
        ])
    };
    let original_panes = pane_ids();
    let assert_unmanaged = || {
        assert_eq!(
            std::fs::metadata(&worktree)
                .expect("checkout metadata")
                .ino(),
            checkout_inode
        );
        assert_eq!(
            std::fs::read(worktree.join(".git")).expect("git pointer"),
            git_file
        );
        assert!(
            rimz::worktree::read_marker_for_worktree(&worktree)
                .expect("read marker")
                .is_none()
        );
        git(
            &worktree,
            &["show-ref", "--verify", "refs/heads/user-owned"],
        );
    };
    let assert_dirty_files = || {
        assert_eq!(
            std::fs::read_to_string(worktree.join("README.md")).expect("dirty file"),
            "unfinished work\n"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("notes.txt")).expect("scratch file"),
            "user scratch\n"
        );
    };
    let output = launch_command()
        .stdin(std::process::Stdio::null())
        .bounded_output()
        .expect("run nonterminal launch");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(stderr.contains("rerun in a terminal"), "{stderr}");
    assert!(agents().is_empty());
    assert_eq!(pane_ids(), original_panes);
    assert_unmanaged();
    assert_dirty_files();

    let run_prompt = |answer: &str| {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 40,
                cols: 160,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open launch PTY");
        let command = launch_command();
        let mut cmd = CommandBuilder::new(env.rimz_bin());
        env.pin_pty_command(&mut cmd);
        cmd.args(command.get_args());
        cmd.cwd(&env.project_root);
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => cmd.env(key, value),
                None => cmd.env_remove(key),
            }
        }
        let mut child = pair.slave.spawn_command(cmd).expect("spawn launch prompt");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("prompt reader");
        let output = thread::spawn(move || {
            let mut output = Vec::new();
            let _ = reader.read_to_end(&mut output);
            output
        });
        let mut writer = pair.master.take_writer().expect("prompt writer");
        writer.write_all(answer.as_bytes()).expect("answer prompt");
        writer.flush().expect("flush answer");
        drop(writer);
        let deadline = Instant::now() + Duration::from_secs(15);
        let status = loop {
            if let Some(status) = child.try_wait().expect("poll prompt") {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            thread::sleep(Duration::from_millis(25));
        };
        drop(pair.master);
        let output =
            String::from_utf8_lossy(&output.join().expect("join prompt reader")).into_owned();
        assert!(status.is_some_and(|status| status.success()), "{output}");
        assert!(output.contains("not RimZ-managed"), "{output}");
        assert!(output.contains("[y/N]"), "{output}");
        output
    };
    for answer in ["no\n", "\n"] {
        let output = run_prompt(answer);
        assert!(
            output.contains("Launch aborted; nothing changed."),
            "{output}"
        );
        assert!(agents().is_empty());
        assert_eq!(pane_ids(), original_panes);
        assert_eq!(
            std::fs::read_dir(&ready).expect("read readiness").count(),
            0
        );
        assert_unmanaged();
        assert_dirty_files();
    }

    run_prompt("yes\n");
    let deadline = Instant::now() + Duration::from_secs(10);
    let launched = loop {
        let rows = agents();
        if rows.len() == 2 && std::fs::read_dir(&ready).expect("read readiness").count() == 2 {
            break rows;
        }
        assert!(
            Instant::now() < deadline,
            "{doorway} did not launch both agents: {rows:?}"
        );
        thread::sleep(Duration::from_millis(25));
    };
    for agent in &launched {
        assert!(agent.ended_at.is_none());
        if doorway == "teams" {
            assert_eq!(agent.team.as_deref(), Some("duo"));
        }
        let pane = &agent.pane.as_ref().expect("agent pane").pane_id;
        let cwd = server.display(pane.raw(), "#{pane_current_path}");
        assert_eq!(
            Path::new(&cwd).canonicalize().expect("pane cwd"),
            worktree.canonicalize().expect("checkout")
        );
    }
    assert_unmanaged();
    assert_dirty_files();

    let live_panes = pane_ids();
    let relaunch = launch_command()
        .stdin(std::process::Stdio::null())
        .bounded_output()
        .expect("relaunch live unmanaged cohort");
    let stderr = String::from_utf8_lossy(&relaunch.stderr);
    assert!(relaunch.status.success(), "{stderr}");
    assert!(stderr.contains("already running"), "{stderr}");
    assert_eq!(
        agents().len(),
        launched.len(),
        "must not duplicate the cohort"
    );
    assert_eq!(pane_ids(), live_panes, "must focus the existing panes");

    // Make the checkout clean so dirtiness cannot mask accidental ownership and cleanup.
    std::fs::copy(
        env.project_root.join("README.md"),
        worktree.join("README.md"),
    )
    .expect("restore clean checkout");
    std::fs::remove_file(worktree.join("notes.txt")).expect("remove test scratch");
    server.tmux(&[
        "kill-window",
        "-t",
        &format!("{}:#existing", workspace.session_name),
    ]);
    for agent in &launched {
        wait_for_agent_end_observation(&env, agent.agent_id.as_str());
    }
    let cleanup = env
        .rimz()
        .args(["--mux", "tmux", "worktree", "cleanup"])
        .arg(&worktree)
        .arg("--non-interactive")
        .bounded_output()
        .expect("run cleanup against clean unmanaged checkout");
    assert!(
        cleanup.status.success(),
        "{}",
        String::from_utf8_lossy(&cleanup.stderr)
    );
    assert_unmanaged();
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).expect("retained clean file"),
        "fixture\n"
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
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: command,
                    name: None,
                }])],
            },
            focus: false,
            dock_sidebar: true,
            after: None,
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
        plan_from_env(&env).tabs.is_empty(),
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
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: command,
                    name: None,
                }])],
            },
            focus: false,
            dock_sidebar: true,
            after: None,
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
    let live_pane = list_session_panes(&server, &workspace.session_name)
        .into_iter()
        .find(|pane| pane.pane_id == pane_id)
        .expect("clean startup failure should leave the pane open as a shell");
    assert!(
        live_pane
            .spawn_command
            .as_deref()
            .is_some_and(|command| command.contains("agents exec codex")),
        "tmux should retain the agent wrapper as immutable birth argv: {live_pane:?}",
    );
    let mut snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        workspace.workspace_id,
        Vec::new(),
        jiff::Timestamp::now(),
    );
    snapshot.wired_kinds = vec!["codex".to_owned()];
    let snapshot = snapshot.with_live_panes(vec![live_pane.clone()], None);
    let row = snapshot.rows().next().expect("shell process row");
    assert!(
        row.is_process(),
        "stale birth argv must not synthesize an agent: {row:?}"
    );
    assert_eq!(
        row.name,
        live_pane.command.as_deref().expect("live shell command")
    );
}
