use super::*;
use crate::harness::launch::{ExecAction, ExecIdentity, ProviderAccountState};
use crate::ids::AgentKind;

fn compile_prompt_request(
    project: &Path,
    request: &ExecRequest,
    baseline: Option<&str>,
    agent_shell: Option<&Path>,
) -> LaunchPlan {
    compile_isolated_request(project, request, baseline, agent_shell, Isolation::Host)
}

fn compile_isolated_request(
    project: &Path,
    request: &ExecRequest,
    baseline: Option<&str>,
    agent_shell: Option<&Path>,
    isolation: Isolation,
) -> LaunchPlan {
    let id = crate::WorkspaceId::from_project_root(project);
    let runtime = RuntimePaths::under(id.clone(), project).unwrap();
    let state = StatePaths::under(id, project).unwrap();
    let machine = crate::config::MachineConfig::default();
    compile(LaunchPlanInputs {
        request,
        cwd: project,
        project_root: project,
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: None,
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        isolation,
        bwrap: isolation.is_scoped().then_some(Path::new("/usr/bin/bwrap")),
        agents: Ok(&[]),
        ambient_env: &BTreeMap::new(),
        agent_shell,
        hibernate: false,
        recorded_system_prompt_digest: baseline,
    })
    .unwrap()
}

#[test]
fn provider_sandbox_settings_and_warnings_follow_the_resolved_level() {
    let project = tempfile::tempdir().unwrap();
    for action in ["launch", "resume", "fork"] {
        let invocation = request("codex", action_with_args(action, vec![]));
        let plan =
            compile_isolated_request(project.path(), &invocation, None, None, Isolation::Sandbox);
        let argv = &plan.process().provider_argv;
        let sandbox = argv
            .windows(2)
            .position(|pair| pair == ["--sandbox", "workspace-write"])
            .expect("default sandbox mode on every action");
        assert_eq!(argv[sandbox + 2], "-c");
        assert!(argv[sandbox + 3].starts_with("sandbox_workspace_write.writable_roots="));
    }
    for kind in ["claude", "codex", "muse"] {
        for isolation in [Isolation::Host, Isolation::Scoped, Isolation::Sandbox] {
            for mode in [
                crate::agents::PermissionMode::Auto,
                crate::agents::PermissionMode::Yolo,
            ] {
                let flags = crate::agents::find_definition(kind)
                    .unwrap()
                    .spec()
                    .launch
                    .permission_args(mode);
                let mut invocation = request(kind, action_with_args("launch", flags));
                invocation.identity.params.mode = Some(mode);
                let plan =
                    compile_isolated_request(project.path(), &invocation, None, None, isolation);
                assert_eq!(plan.process().env[Isolation::ENV], isolation.to_string());
                assert_eq!(plan.sandbox.is_some(), isolation.is_scoped());
                let warnings = plan
                    .warnings
                    .iter()
                    .filter(|warning| !matches!(warning, LaunchPlanWarning::DefaultReminders))
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                assert_eq!(
                    warnings.len(),
                    usize::from(isolation == Isolation::Sandbox && kind == "muse"),
                    "{kind} {isolation} {mode:?}: {warnings:?}"
                );
                if let Some(warning) = warnings.first() {
                    assert!(warning.contains(kind), "{warning}");
                    assert!(warning.contains("no sandbox flag"), "{warning}");
                    assert!(warning.contains("host"), "{warning}");
                }
                if kind == "muse" {
                    assert_eq!(
                        plan.process()
                            .provider_argv
                            .iter()
                            .any(|arg| arg == "--disable-sandbox"),
                        isolation.is_scoped()
                    );
                }
                if kind == "claude" {
                    let settings = plan
                        .process()
                        .provider_argv
                        .windows(2)
                        .find(|pair| pair[0] == "--settings")
                        .unwrap();
                    let settings: serde_json::Value = serde_json::from_str(&settings[1]).unwrap();
                    assert_eq!(
                        settings["sandbox"]["enabled"],
                        if isolation == Isolation::Sandbox {
                            true.into()
                        } else {
                            serde_json::Value::Null
                        }
                    );
                    assert_eq!(
                        settings["sandbox"].as_object().unwrap().len(),
                        2 + usize::from(isolation != Isolation::Host)
                    );
                    assert_eq!(
                        settings["sandbox"]["enableWeakerNestedSandbox"],
                        if isolation.is_scoped() {
                            true.into()
                        } else {
                            serde_json::Value::Null
                        }
                    );
                }
            }
        }
    }
}

#[test]
fn codex_host_profile_grants_room_roots_and_covers_temp_files() {
    if std::env::var_os("RIMZ_TEST_CODEX_ROOM_GRANTS").is_none() {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("config.toml");
        std::fs::write(&config, "").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "harness::launch_plan::tests::codex_host_profile_grants_room_roots_and_covers_temp_files", "--nocapture"])
            .env("RIMZ_CODEX_CONFIG", &config)
            .env("RIMZ_TEST_CODEX_ROOM_GRANTS", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let project = tempfile::tempdir().unwrap();
    let state = StatePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .unwrap();
    let runtime = RuntimePaths::under(state.workspace_id.clone(), project.path()).unwrap();
    let invocation = request("codex", action_with_args("launch", vec![]));
    let plan = compile_prompt_request(project.path(), &invocation, None, None);
    let grants: Vec<_> = plan
        .process()
        .provider_argv
        .iter()
        .filter_map(|arg| arg.strip_prefix("sandbox_workspace_write.writable_roots="))
        .collect();
    assert_eq!(grants.len(), 1);
    let table = format!("roots={}", grants[0])
        .parse::<toml::Table>()
        .unwrap();
    assert_eq!(
        table["roots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>(),
        [&state.root, &runtime.root, &runtime.shared_root].map(|path| path.to_str().unwrap())
    );
    assert!(Path::new(&plan.process().env["TMPDIR"]).starts_with(&state.root));
    assert!(Path::new(&plan.process().env["RIMZ_SHARED"]).starts_with(&state.root));
}

#[test]
fn workflow_env_pins_survive_both_isolations() {
    let project = tempfile::tempdir().unwrap();
    let metadata = tempfile::tempdir().unwrap();
    let common = metadata.path().join(".git");
    let admin = common.join("worktrees/judge");
    std::fs::create_dir_all(&admin).unwrap();
    std::fs::write(admin.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    std::fs::write(
        project.path().join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .unwrap();
    std::fs::write(admin.join("commondir"), "../..\n").unwrap();
    let state = StatePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .unwrap();
    let runtime = RuntimePaths::under(state.workspace_id.clone(), project.path()).unwrap();
    let machine = crate::config::MachineConfig::default();
    let workflow = crate::agents::WorkflowAttemptRef {
        run: crate::ids::WorkflowRunId::new(),
        seq: 3,
        role: crate::agents::WorkflowAttemptRole::Step,
    };
    for (sandboxed, role) in [
        (false, crate::agents::WorkflowAttemptRole::Step),
        (true, crate::agents::WorkflowAttemptRole::Step),
        (false, crate::agents::WorkflowAttemptRole::Judge),
        (true, crate::agents::WorkflowAttemptRole::Judge),
    ] {
        let mut request = request("claude", action_with_args("launch", vec![]));
        request.identity.params.workflow = Some(workflow.clone());
        request.identity.params.workflow.as_mut().unwrap().role = role;
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: if sandboxed {
                Isolation::Scoped
            } else {
                Isolation::Host
            },
            bwrap: sandboxed.then_some(Path::new("/bin/bwrap")),
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
            hibernate: false,
            recorded_system_prompt_digest: None,
        })
        .unwrap();
        assert_eq!(
            Isolation::ambient(&plan.process().env),
            Some(if sandboxed {
                Isolation::Scoped
            } else {
                Isolation::Host
            })
        );
        assert_eq!(
            plan.process().env.get("RIMZ_WORKFLOW_RUN"),
            Some(&workflow.run.to_string())
        );
        assert_eq!(
            plan.process().env.get("RIMZ_WORKFLOW_DIR"),
            Some(&state.workflow(&workflow.run).work.display().to_string())
        );
        if let Some(sandbox) = &plan.sandbox {
            let checkout_ro = |mount: &crate::sandbox::Mount| {
                matches!(mount,
                crate::sandbox::Mount::RoBind { source, target }
                if source == project.path() && target == project.path())
            };
            assert_eq!(
                sandbox.plan.mounts.iter().any(checkout_ro),
                role == crate::agents::WorkflowAttemptRole::Judge,
                "only a judge overlays the checkout read-only after the reach rebinds"
            );
            if role == crate::agents::WorkflowAttemptRole::Judge {
                for path in [&admin, &common] {
                    assert!(sandbox.plan.mounts.iter().any(|mount| matches!(mount,
                        crate::sandbox::Mount::RoBind { source, target } if source == path && target == path
                    )), "judge Git metadata must be read-only: {}", path.display());
                }
            }
        }
    }
}

#[test]
fn scoped_empty_directory_does_not_inherit_ancestor_git_protection() {
    for symlinked_home in [true, false] {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(project.path().join(".git/hooks")).unwrap();
        std::fs::create_dir(project.path().join(".codex")).unwrap();
        std::fs::write(project.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(project.path().join(".git/config"), "").unwrap();
        let id = crate::WorkspaceId::from_project_root(project.path());
        let alias = project.path().join("home-link");
        std::os::unix::fs::symlink(project.path(), &alias).unwrap();
        let home = if symlinked_home {
            alias.as_path()
        } else {
            project.path()
        };
        let state = StatePaths::under(id.clone(), home).unwrap();
        let runtime = RuntimePaths::under(id, project.path()).unwrap();
        let cwd = state.empty_agent_dir("reader");
        std::fs::create_dir_all(&cwd).unwrap();
        let cwd = cwd.canonicalize().unwrap();
        let request = request("codex", action_with_args("launch", vec![]));
        let machine = crate::config::MachineConfig::default();
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: &cwd,
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Scoped,
            bwrap: Some(Path::new("/usr/bin/bwrap")),
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
            hibernate: false,
            recorded_system_prompt_digest: None,
        })
        .unwrap();
        let mounts = &plan.sandbox.unwrap().plan.mounts;
        assert!(mounts.iter().any(|mount| matches!(mount,
            sandbox::Mount::RoBind { target, .. } if target == &project.path().join(".codex")
        )));
        for entry in [".git/hooks", ".git/config"] {
            assert!(
                !mounts.iter().any(|mount| matches!(mount,
                    sandbox::Mount::RoBind { target, .. } if target == &project.path().join(entry)
                )),
                "empty-dir launch must not protect ancestor {entry}"
            );
        }
    }
}

#[test]
fn unreadable_checkout_metadata_refuses_only_scoped_launches() {
    let project = tempfile::tempdir().unwrap();
    let cwd = project.path().join("nested");
    std::fs::create_dir(&cwd).unwrap();
    let dot_git = project.path().join(".git");
    std::os::unix::fs::symlink(".git", &dot_git).unwrap();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let state = StatePaths::under(id.clone(), project.path()).unwrap();
    let runtime = RuntimePaths::under(id, project.path()).unwrap();
    let request = request("codex", action_with_args("launch", vec![]));
    let machine = crate::config::MachineConfig::default();
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let result = compile(LaunchPlanInputs {
            request: &request,
            cwd: &cwd,
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation,
            bwrap: isolation.is_scoped().then_some(Path::new("/usr/bin/bwrap")),
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
            hibernate: false,
            recorded_system_prompt_digest: None,
        });
        if isolation == Isolation::Host {
            assert!(result.is_ok());
        } else {
            let error = result
                .err()
                .expect("unreadable .git must refuse scoped launch")
                .to_string();
            assert!(
                error.contains(&format!("scoped view cannot access {}:", dot_git.display())),
                "{error}"
            );
            assert!(error.contains("fix the path before launching"), "{error}");
        }
    }
}

#[test]
fn configured_prompt_changes_control_snapshot_reuse() {
    use std::os::unix::fs::PermissionsExt;

    if std::env::var_os("RIMZ_TEST_SNAPSHOT_CLI").is_none() {
        let bin = tempfile::tempdir().unwrap();
        let cli = bin.path().join("claude");
        std::fs::write(
            &cli,
            "#!/bin/sh\nprintf 'probe\\n' >> \"$RIMZ_TEST_SNAPSHOT_PROBES\"\nprintf '2.1.296 (Claude Code)\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        let paths = std::iter::once(bin.path().to_path_buf())
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ))
            .collect::<Vec<_>>();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "harness::launch_plan::tests::configured_prompt_changes_control_snapshot_reuse",
                "--nocapture",
            ])
            .env("RIMZ_TEST_SNAPSHOT_CLI", "1")
            .env(crate::proc::USER_SHELL_ENV, "/definitely/not/a/shell")
            .env("RIMZ_TEST_SNAPSHOT_PROBES", bin.path().join("probes"))
            .env("PATH", std::env::join_paths(paths).unwrap())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let project = tempfile::tempdir().unwrap();
    let probes = std::path::PathBuf::from(std::env::var_os("RIMZ_TEST_SNAPSHOT_PROBES").unwrap());
    let cli = probes.with_file_name("claude");
    let fresh = request("claude", action_with_args("launch", vec![]));
    let baseline = compile_prompt_request(project.path(), &fresh, None, None).system_prompt_digest;
    for (action, recorded, expected) in [
        ("launch", None, false),
        ("resume", Some(baseline.as_str()), false),
        ("resume", Some("different"), true),
        ("resume", None, true),
        ("fork", Some("different"), true),
        ("fork", Some(baseline.as_str()), false),
    ] {
        // Growing the stub changes its file stamp, so the per-binary version
        // memo cannot hide a probe this compile makes.
        let mut stub = std::fs::OpenOptions::new().append(true).open(&cli).unwrap();
        std::io::Write::write_all(&mut stub, b"#\n").unwrap();
        drop(stub);
        let request = request("claude", action_with_args(action, vec![]));
        let plan = compile_prompt_request(project.path(), &request, recorded, None);
        let flags: Vec<_> = plan
            .process()
            .provider_argv
            .windows(2)
            .filter(|args| args[0] == "--system-prompt-snapshot")
            .collect();
        assert_eq!(
            flags.len(),
            usize::from(expected),
            "{action} with baseline {recorded:?}"
        );
        assert!(flags.iter().all(|args| args[1] == "off"));
        assert_eq!(plan.system_prompt_digest, baseline);
    }
    let probes = std::fs::read_to_string(&probes).unwrap();
    assert_eq!(
        probes.lines().count(),
        3,
        "only changed resumed prompts probe the CLI"
    );

    for (version, expected) in [("2.1.256", false), ("2.1.257", true), ("unknown", false)] {
        let bin = tempfile::tempdir().unwrap();
        let pinned = bin.path().join("claude");
        std::fs::write(&pinned, format!("#!/bin/sh\nprintf '{version}\\n'\n")).unwrap();
        std::fs::set_permissions(&pinned, std::fs::Permissions::from_mode(0o755)).unwrap();
        let shell = bin.path().join("bash");
        std::fs::write(&shell, format!(
            "#!/bin/sh\nexport PATH='{}':\"$PATH\"\nwhile [ \"$#\" -gt 0 ]; do\ncase \"$1\" in\n-c) shift; script=$1; shift; exec /bin/sh -c \"$script\" \"$@\" ;;\n*) shift ;;\nesac\ndone\nexit 127\n",
            bin.path().display()
        )).unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let request = request("claude", action_with_args("resume", vec![]));
        let plan = compile_prompt_request(project.path(), &request, None, Some(&shell));
        assert_eq!(
            plan.process().resolve_program_after_shell_rc().unwrap(),
            Some(pinned)
        );
        let has_snapshot_flag = plan
            .process()
            .provider_argv
            .windows(2)
            .any(|args| args == ["--system-prompt-snapshot", "off"]);
        assert_eq!(
            has_snapshot_flag, expected,
            "launch binary {version}, PATH claude 2.1.296"
        );
    }
    let probes =
        std::fs::read_to_string(std::env::var_os("RIMZ_TEST_SNAPSHOT_PROBES").unwrap()).unwrap();
    assert_eq!(
        probes.lines().count(),
        3,
        "pinned launches must not probe PATH claude"
    );
}

#[test]
fn configured_prompt_fingerprint_ignores_reminders_and_reads_files() {
    let project = tempfile::tempdir().unwrap();
    let base = project.path().join("base.md");
    std::fs::write(&base, "configured base").unwrap();
    let mut launch = request(
        "claude",
        action_with_args(
            "launch",
            vec!["--append-system-prompt".into(), "configured append".into()],
        ),
    );
    launch.system_prompt_file = Some(base.clone().into());
    let fresh = compile_prompt_request(project.path(), &launch, None, None);
    let baseline = fresh.system_prompt_digest.clone();
    let mut resume = launch.clone();
    resume.action = action_with_args("resume", launch.action.extra_args().to_vec());
    resume.loop_reminder = Some("changed runtime reminder".into());
    let resumed = compile_prompt_request(project.path(), &resume, Some(&baseline), None);
    assert_ne!(fresh.process().reminder, resumed.process().reminder);
    assert_eq!(resumed.system_prompt_digest, baseline);
    std::fs::write(&base, "edited configured base").unwrap();
    assert_ne!(
        compile_prompt_request(project.path(), &launch, None, None).system_prompt_digest,
        baseline
    );
    assert_eq!(baseline.len(), 64);
    assert!(
        baseline
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );

    for flag in ["--system-prompt-file", "--append-system-prompt-file"] {
        std::fs::write(&base, "raw file").unwrap();
        let native = request(
            "claude",
            action_with_args("launch", vec![flag.into(), base.display().to_string()]),
        );
        let before =
            compile_prompt_request(project.path(), &native, None, None).system_prompt_digest;
        std::fs::write(&base, "edited raw file").unwrap();
        assert_ne!(
            compile_prompt_request(project.path(), &native, None, None).system_prompt_digest,
            before,
            "{flag}"
        );
    }
    for flag in ["--system-prompt", "--append-system-prompt"] {
        let native = request(
            "claude",
            action_with_args("launch", vec![format!("{flag}=raw text")]),
        );
        let before =
            compile_prompt_request(project.path(), &native, None, None).system_prompt_digest;
        let native = request(
            "claude",
            action_with_args("launch", vec![flag.into(), "changed raw text".into()]),
        );
        assert_ne!(
            compile_prompt_request(project.path(), &native, None, None).system_prompt_digest,
            before,
            "{flag}"
        );
    }
}

fn exec_launch_reminders(
    request: &ExecRequest,
    machine: &crate::config::MachineConfig,
    project: &Path,
    config: &Path,
) -> LaunchReminders {
    let effective = crate::config::effective::load_with_roots(machine, project, config);
    reminders(request, effective.as_ref().ok(), &machine.agents.commands).0
}

fn request(kind: &str, action: ExecAction) -> ExecRequest {
    ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked(kind),
        action,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        follow_up_message: None,
        loop_reminder: None,
        headless: None,
        identity: ExecIdentity::default(),
    }
}

#[test]
#[cfg(target_os = "linux")]
fn hibernation_scopes_ready_providers_outside_bwrap() {
    let project = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let machine = crate::config::MachineConfig::default();
    let request = request("claude", action_with_args("launch", vec![]));
    for isolation in [Isolation::Host, Isolation::Scoped] {
        let bwrap = isolation.is_scoped().then_some(Path::new("/usr/bin/bwrap"));
        for enabled in [false, true] {
            let plan = compile(LaunchPlanInputs {
                recorded_system_prompt_digest: None,
                request: &request,
                cwd: project.path(),
                project_root: project.path(),
                rimz_bin: Path::new("/bin/rimz"),
                runtime: &runtime,
                state: &state,
                effective: None,
                commands: &machine.agents.commands,
                accounts: &machine.accounts,
                isolation,
                bwrap,
                agents: Ok(&[]),
                ambient_env: &BTreeMap::new(),
                agent_shell: None,
                hibernate: enabled,
            })
            .unwrap();
            let argv = &plan.process().argv;
            assert_eq!(argv[0] == "systemd-run", enabled);
            if enabled {
                assert!(
                    argv[8]
                        .starts_with(&super::super::hibernate::scope_prefix(&state.workspace_id))
                );
                if let Some(bwrap) = bwrap {
                    assert_eq!(argv[12], bwrap.to_str().unwrap());
                }
            }
        }
    }
}

#[test]
#[cfg(target_os = "linux")]
fn hibernation_leaves_qwen_reentry_outside_the_final_provider_scope() {
    let project = tempfile::tempdir().unwrap();
    let home = project.path().join("qwen-work");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("settings.json"), r#"{
        "security":{"auth":{"selectedType":"openai"}},
        "model":{"name":"qwen3-coder-plus"},
        "modelProviders":{"openai":[{"id":"qwen3-coder-plus","baseUrl":"https://coding-intl.dashscope.aliyuncs.com/v1","envKey":"BAILIAN_CODING_PLAN_API_KEY"}]},
        "env":{"BAILIAN_CODING_PLAN_API_KEY":"test-credential"}
    }"#).unwrap();
    if std::env::var_os("RIMZ_TEST_QWEN_HIBERNATION").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "harness::launch_plan::tests::hibernation_leaves_qwen_reentry_outside_the_final_provider_scope", "--nocapture"])
            .env("RIMZ_TEST_QWEN_HIBERNATION", "1")
            .env("QWEN_HOME", &home)
            .env_remove("RIMZ_QWEN_SETTINGS")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let machine = crate::config::MachineConfig::default();
    let state = StatePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .unwrap();
    let runtime = RuntimePaths::under(state.workspace_id.clone(), project.path()).unwrap();
    let env = BTreeMap::from([("QWEN_HOME".into(), home.display().to_string())]);
    let binding = crate::agents::find_definition("qwen")
        .unwrap()
        .resolve_managed_launch(
            project.path(),
            &env,
            Some("qwen3-coder-plus"),
            &["qwen".into()],
        )
        .binding()
        .unwrap()
        .clone();
    let mut request = request("qwen", action_with_args("launch", vec![]));
    request.identity.params.model = Some("qwen3-coder-plus".into());
    for enabled in [false, true] {
        for finalized in [false, true] {
            request.provider_account = if finalized {
                ProviderAccountState::Finalized {
                    binding: binding.clone(),
                }
            } else {
                ProviderAccountState::Pending {
                    binding: binding.clone(),
                }
            };
            let plan = compile(LaunchPlanInputs {
                recorded_system_prompt_digest: None,
                request: &request,
                cwd: project.path(),
                project_root: project.path(),
                rimz_bin: Path::new("/bin/rimz"),
                runtime: &runtime,
                state: &state,
                effective: None,
                commands: &machine.agents.commands,
                accounts: &machine.accounts,
                isolation: Isolation::Host,
                bwrap: None,
                agents: Ok(&[]),
                ambient_env: &env,
                agent_shell: None,
                hibernate: enabled,
            })
            .unwrap();
            match plan.stage {
                AgentProcessStage::LoginShellReentry { argv, .. } => {
                    assert!(!finalized);
                    assert!(!argv.iter().any(|arg| arg == "systemd-run"));
                }
                AgentProcessStage::Ready(process) => {
                    assert!(finalized);
                    assert_eq!(process.argv[0] == "systemd-run", enabled);
                }
            }
        }
    }
}

fn action_with_args(action: &str, args: Vec<String>) -> ExecAction {
    match action {
        "launch" => ExecAction::Launch {
            prompt: None,
            extra_args: args,
        },
        "resume" => ExecAction::Resume {
            session_id: "session".to_owned(),
            extra_args: args,
        },
        "fork" => ExecAction::Fork {
            session_id: "session".to_owned(),
            extra_args: args,
        },
        _ => unreachable!("test action is known"),
    }
}

fn recorded_session(model: &str) -> crate::agents::AgentState {
    let mut agent = crate::testkit::agent_state("codex", "session", jiff::Timestamp::now());
    agent.model = Some(model.into());
    agent
}

#[test]
fn recorded_alias_resume_ignores_observed_session_model() {
    check_alias_resume(false);
}

#[test]
fn explicit_alias_resume_ignores_legacy_observed_model() {
    check_alias_resume(true);
}

fn check_alias_resume(explicit: bool) {
    let root = tempfile::tempdir().unwrap();
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(root.path()),
        root.path(),
    )
    .unwrap();
    let machine = toml::from_str("[models.codex]\nsol = 'gpt-6-sol'").unwrap();
    let mut req = request(
        "codex",
        action_with_args("resume", vec!["--model=sol".into()]),
    );
    req.identity.params.model = Some("sol".into());
    req.identity.params.record = Some(Box::new(crate::agents::LaunchRecord {
        model: Some("sol".into()),
        ..Default::default()
    }));
    req.identity.resume_model_override = explicit;
    let session = crate::agents::AgentState {
        record: req.identity.params.record.clone().filter(|_| !explicit),
        ..recorded_session("observed-switch")
    };
    resolve_model(
        &mut req,
        &machine,
        &runtime,
        &ProviderLogin::default_for(AgentKind::new_unchecked("codex")),
        Some(&session),
        None,
        &Default::default(),
    )
    .unwrap();
    assert_eq!(req.identity.params.model.as_deref(), Some("gpt-6-sol"));
    assert_eq!(
        req.identity.params.record.unwrap().model.as_deref(),
        Some("sol")
    );
}

#[test]
fn model_alias_resolution_at_the_process_boundary() {
    use crate::agents::capabilities::{ModelCatalogEntry, ModelCatalogErr, ModelCatalogSource};
    struct Catalog(usize, bool);
    impl ModelCatalogSource for Catalog {
        fn fetch(
            &mut self,
            _: &RuntimePaths,
            _: &BTreeMap<String, String>,
        ) -> Result<Vec<ModelCatalogEntry>, ModelCatalogErr> {
            self.0 += 1;
            if self.1 {
                return Err(ModelCatalogErr::Unavailable("offline".into()));
            }
            Ok(vec![ModelCatalogEntry {
                id: "gpt-6.1-sol".into(),
                hidden: false,
                upgrade: None,
                efforts: vec!["high".into()],
            }])
        }
    }
    let root = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(root.path());
    let runtime = RuntimePaths::under(id.clone(), root.path()).unwrap();
    let state = StatePaths::under(id, root.path()).unwrap();
    let login = ProviderLogin::default_for(AgentKind::new_unchecked("codex"));
    let machine = crate::config::MachineConfig::default();
    let ambient = BTreeMap::new();
    let mut catalog = Catalog(0, false);
    for (action, requested, recorded, expected) in [
        ("launch", "sol", None, "gpt-6.1-sol"),
        ("resume", "sol", Some("gpt-6-sol"), "gpt-6-sol"),
        ("fork", "sol", Some("gpt-6-sol"), "gpt-6-sol"),
        ("resume", "gpt-6-sol", Some("gpt-6.1-sol"), "gpt-6-sol"),
        ("resume", "sol", None, "gpt-6.1-sol"),
        ("resume", "sol", Some("sol"), "gpt-6.1-sol"),
    ] {
        let mut req = request(
            "codex",
            action_with_args(action, vec![format!("--model={requested}")]),
        );
        req.identity.params.model = Some(requested.into());
        let (warnings, movement, refresh_failure) = resolve_model(
            &mut req,
            &machine,
            &runtime,
            &login,
            recorded.map(recorded_session).as_ref(),
            Some(&mut catalog),
            &ambient,
        )
        .unwrap();
        assert!(refresh_failure.is_none());
        assert_eq!(req.identity.params.model.as_deref(), Some(expected));
        if requested == "sol" {
            assert_eq!(req.action.extra_args(), ["--model", expected]);
        }
        if action == "launch" {
            assert_eq!(movement.unwrap().to, expected);
        }
        if action == "resume" && requested == "sol" && recorded.is_none() {
            assert!(!warnings.is_empty());
        }
        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &req,
            cwd: root.path(),
            project_root: root.path(),
            rimz_bin: Path::new("rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
            hibernate: false,
        })
        .unwrap();
        assert_eq!(
            plan.process()
                .env
                .get("RIMZ_AGENT_MODEL")
                .map(String::as_str),
            Some(expected)
        );
    }
    assert_eq!(catalog.0, 1);
    let pinned: crate::config::MachineConfig =
        toml::from_str("[models.codex]\nsol = 'gpt-6-sol'").unwrap();
    let mut req = request(
        "codex",
        action_with_args("launch", vec!["--model".into(), "sol".into()]),
    );
    req.identity.params.model = Some("sol".into());
    let (warnings, movement, refresh_failure) = resolve_model(
        &mut req,
        &pinned,
        &runtime,
        &login,
        None,
        Some(&mut catalog),
        &ambient,
    )
    .unwrap();
    assert!(refresh_failure.is_none());
    assert_eq!(req.identity.params.model.as_deref(), Some("gpt-6-sol"));
    assert!(!warnings.is_empty());
    assert!(movement.is_none());
    assert_eq!(catalog.0, 1);
    let cache_path = runtime.shared_model_catalog_path(&login.key());
    let mut cache: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    cache["fetched_at"] = (crate::utils::time::unix_now_ms() - 2 * 60 * 60 * 1000).into();
    std::fs::write(&cache_path, serde_json::to_vec(&cache).unwrap()).unwrap();
    catalog.1 = true;
    for (requested, expected, calls, warned) in [
        ("gpt-6-sol", "gpt-6-sol", 1, false),
        ("sol", "gpt-6.1-sol", 2, false),
        ("sol", "gpt-6-sol", 3, true),
    ] {
        req.identity.params.model = Some(requested.into());
        *req.action.extra_args_mut() = vec!["--model".into(), requested.into()];
        let (warnings, movement, refresh_failure) = resolve_model(
            &mut req,
            &machine,
            &runtime,
            &login,
            None,
            Some(&mut catalog),
            &ambient,
        )
        .unwrap();
        assert_eq!(req.identity.params.model.as_deref(), Some(expected));
        assert_eq!(req.action.extra_args(), ["--model", expected]);
        assert_eq!(!warnings.is_empty(), warned);
        assert!(movement.is_none());
        assert_eq!(catalog.0, calls);
        if requested == "sol" {
            let failure = refresh_failure.expect("a failed refresh reaches the wrapper");
            assert_eq!(failure.login, login.key());
            assert_eq!(failure.alias, "sol");
            assert_eq!(failure.reason, "offline");
            assert_eq!(
                failure.rung,
                if calls == 2 {
                    crate::agents::capabilities::ModelAliasRung::CachedCatalog
                } else {
                    crate::agents::capabilities::ModelAliasRung::Baked
                }
            );
        } else {
            assert!(refresh_failure.is_none());
        }
        if calls == 2 {
            std::fs::remove_file(&cache_path).unwrap();
        }
    }
}

#[test]
fn team_archive_grant_follows_a_childs_checkout_and_rejects_foreign_runs() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let nested = project.path().join("src");
    std::fs::create_dir(&nested).unwrap();
    let output = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(project.path())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let state = StatePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        config.path(),
    )
    .unwrap();
    let runtime = RuntimePaths::for_state_under(&state, config.path());
    let run = state.root.join("team-notes/run");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("run.json"), r#"{"team":"forge"}"#).unwrap();
    std::fs::create_dir(project.path().join(".rimz")).unwrap();
    let link = project.path().join(".rimz/team");
    std::os::unix::fs::symlink(&run, &link).unwrap();
    let machine: crate::config::MachineConfig =
        toml::from_str("[agents]\nallow-routine-rimz = false").unwrap();
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), config.path()).unwrap();
    let mut request = request("claude", action_with_args("launch", Vec::new()));
    request.subagent = true;
    let dirs = |team: Option<&str>| {
        let mut request = request.clone();
        request.identity.params.team = team.map(str::to_owned);
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: &nested,
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
            hibernate: false,
            recorded_system_prompt_digest: None,
        })
        .unwrap();
        let argv = &plan.process().provider_argv;
        let settings = argv
            .windows(2)
            .find(|pair| pair[0] == "--settings")
            .unwrap();
        let settings: serde_json::Value = serde_json::from_str(&settings[1]).unwrap();
        settings["permissions"]["additionalDirectories"].clone()
    };
    assert_eq!(dirs(None), serde_json::json!([run.canonicalize().unwrap()]));
    let nested_run = state.team_notes_dir.join("nested-run");
    std::fs::create_dir(&nested_run).unwrap();
    std::fs::write(nested_run.join("run.json"), r#"{"team":"mill"}"#).unwrap();
    std::fs::create_dir(nested.join(".rimz")).unwrap();
    let nested_link = nested.join(".rimz/team");
    std::os::unix::fs::symlink(&nested_run, &nested_link).unwrap();
    assert_eq!(
        dirs(None),
        serde_json::json!([nested_run.canonicalize().unwrap()]),
        "a subdirectory launch must grant its nearest run"
    );
    assert!(
        dirs(Some("forge")).is_null(),
        "do not skip a foreign nearest run to grant the checkout root's run"
    );
    assert_eq!(
        dirs(Some("mill")),
        serde_json::json!([nested_run.canonicalize().unwrap()])
    );
    std::fs::remove_file(&nested_link).unwrap();
    assert_eq!(
        dirs(Some("forge")),
        serde_json::json!([run.canonicalize().unwrap()])
    );
    std::fs::remove_file(run.join("run.json")).unwrap();
    assert!(
        dirs(Some("forge")).is_null(),
        "an unowned run grants no tagged launch"
    );
    std::fs::remove_dir_all(project.path().join(".git")).unwrap();
    assert_eq!(dirs(None), serde_json::json!([run.canonicalize().unwrap()]));
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(config.path(), &link).unwrap();
    assert!(dirs(None).is_null());
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(config.path().join("missing"), &link).unwrap();
    assert!(dirs(None).is_null());
    std::fs::remove_file(&link).unwrap();
    assert!(dirs(None).is_null());
}

#[test]
fn routine_permissions_cover_actions_children_and_isolations() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let run = state.root.join("team-notes/run");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    std::os::unix::fs::symlink(&run, project.path().join(".rimz/team")).unwrap();
    let home = project.path().join("home");
    for name in ["commit", "rimz-pin"] {
        std::fs::create_dir_all(home.join("skills").join(name)).unwrap();
        std::fs::write(
            home.join("skills").join(name).join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test\n---\nSkill."),
        )
        .unwrap();
    }
    let ambient = BTreeMap::from([
        ("CLAUDE_CONFIG_DIR".into(), home.display().to_string()),
        (
            "RIMZ_AGENTS_HOME".into(),
            project.path().join("agents").display().to_string(),
        ),
        ("HOME".into(), home.display().to_string()),
    ]);
    for (enabled, listed, rules) in [
        (false, false, false),
        (false, false, true),
        (false, true, false),
        (false, true, true),
        (true, false, false),
        (true, false, true),
        (true, true, false),
        (true, true, true),
    ] {
        let machine: crate::config::MachineConfig =
            toml::from_str(&format!("[agents]\nallow-routine-rimz = {enabled}")).unwrap();
        let effective =
            crate::config::effective::load_with_roots(&machine, project.path(), config.path())
                .unwrap();
        for action in ["launch", "resume", "fork"] {
            for isolation in [Isolation::Host, Isolation::Scoped, Isolation::Sandbox] {
                let sandboxed = isolation.is_scoped();
                for subagent in [false, true] {
                    let mut request = request("claude", action_with_args(action, Vec::new()));
                    request.subagent = subagent;
                    request.identity.name = Some("otter".into());
                    request.skills = listed.then(|| vec!["commit".parse().unwrap()]);
                    request.allowed_tools = rules.then(|| vec!["Bash(git *)".parse().unwrap()]);
                    let plan = compile(LaunchPlanInputs {
                        recorded_system_prompt_digest: None,
                        request: &request,
                        cwd: project.path(),
                        project_root: project.path(),
                        rimz_bin: Path::new("/bin/rimz"),
                        runtime: &runtime,
                        state: &state,
                        effective: Some(&effective),
                        commands: &machine.agents.commands,
                        accounts: &machine.accounts,
                        isolation,
                        bwrap: sandboxed.then_some(Path::new("/bin/bwrap")),
                        agents: Ok(&[]),
                        ambient_env: &ambient,
                        agent_shell: None,
                        hibernate: false,
                    })
                    .unwrap();
                    let settings = plan
                        .process()
                        .provider_argv
                        .windows(2)
                        .find(|pair| pair[0] == "--settings");
                    assert!(
                        settings.is_some(),
                        "sandbox room grants require settings: {action} sandbox={sandboxed} child={subagent}"
                    );
                    if let Some(settings) = settings {
                        let value: serde_json::Value = serde_json::from_str(&settings[1]).unwrap();
                        assert_eq!(
                            value["permissions"]["allow"].as_array().is_some_and(
                                |allow| allow.contains(&serde_json::json!("Bash(git *)"))
                            ),
                            rules
                        );
                        let mut dirs = vec![
                            if sandboxed {
                                PathBuf::from("/tmp")
                            } else {
                                state.temp_unit_dir(Some("otter"))
                            },
                            state.room_shared_dir.clone(),
                        ];
                        if !enabled {
                            dirs.clear();
                        }
                        dirs.push(run.clone());
                        assert_eq!(
                            value["permissions"]["additionalDirectories"],
                            serde_json::json!(dirs)
                        );
                        assert_eq!(
                            value["permissions"]["allow"]
                                .as_array()
                                .is_some_and(|allow| allow
                                    .contains(&serde_json::json!("Bash(rimz agents *)"))),
                            enabled
                        );
                        assert_eq!(
                            value["permissions"]["allow"].as_array().is_some_and(
                                |allow| allow.contains(&serde_json::json!("Skill(rimz-pin)"))
                            ),
                            enabled,
                            "{action} sandbox={sandboxed} child={subagent}"
                        );
                        assert_eq!(
                            value["permissions"]["allow"].as_array().is_some_and(
                                |allow| allow.contains(&serde_json::json!("Skill(commit)"))
                            ),
                            listed
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn sandbox_settings_artifact_is_written_only_on_apply() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    std::fs::write(
        project.path().join("profile.json"),
        r#"{"env":{"TOKEN":"private-secret"}}"#,
    )
    .unwrap();
    let request = request(
        "claude",
        action_with_args("launch", vec!["--settings".into(), "profile.json".into()]),
    );
    let plan = compile(LaunchPlanInputs {
        recorded_system_prompt_digest: None,
        request: &request,
        cwd: project.path(),
        project_root: project.path(),
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: None,
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        isolation: Isolation::Scoped,
        bwrap: Some(Path::new("/bin/bwrap")),
        agents: Ok(&[]),
        ambient_env: &BTreeMap::new(),
        agent_shell: None,
        hibernate: false,
    })
    .unwrap();
    let args = &plan.process().provider_argv;
    let settings = args
        .windows(2)
        .find(|pair| pair[0] == "--settings")
        .unwrap();
    let path = Path::new(&settings[1]);
    assert_eq!(path.parent(), Some(runtime.prompt_dir().as_path()));
    assert!(!path.exists());
    assert!(!args.join(" ").contains("private-secret"));
    apply(&plan).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(value["env"]["TOKEN"], "private-secret");
    assert!(value["permissions"]["allow"].is_array());
}

#[test]
fn allowed_tools_warn_on_unsupported_kind_without_changing_argv() {
    let project = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let machine = crate::config::MachineConfig::default();
    let ambient = BTreeMap::from([("HOME".into(), project.path().display().to_string())]);
    let compile_request = |request: &ExecRequest| {
        compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
            hibernate: false,
        })
        .unwrap()
    };
    let mut request = request("codex", action_with_args("launch", Vec::new()));
    let baseline = compile_request(&request);
    for rules in [Some(vec!["Read".parse().unwrap()]), Some(Vec::new()), None] {
        request.allowed_tools = rules;
        let plan = compile_request(&request);
        assert_eq!(
            plan.process().provider_argv,
            baseline.process().provider_argv
        );
        let warnings: Vec<_> = plan
            .warnings
            .iter()
            .map(ToString::to_string)
            .filter(|text| text.contains("allowed-tools"))
            .collect();
        if request
            .allowed_tools
            .as_ref()
            .is_some_and(|rules| !rules.is_empty())
        {
            assert_eq!(
                warnings,
                [
                    "codex has no per-launch permission rules; allowed-tools is not applied and the agent prompts as usual: remove allowed-tools from the definition or run it on claude"
                ]
            );
        } else {
            assert!(warnings.is_empty());
        }
    }
}

#[test]
fn host_settings_are_private_artifacts_written_only_on_apply() {
    use std::os::unix::fs::PermissionsExt;
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).unwrap();
    let state = StatePaths::under(workspace_id, project.path()).unwrap();
    let settings = project.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"env":{"ANTHROPIC_API_KEY":"sk-secret-123"}}"#,
    )
    .unwrap();
    let mut request = ExecRequest::bare_launch(
        AgentKind::new_unchecked("claude"),
        vec!["--settings".into(), settings.display().to_string()],
    );
    request.skills = Some(Vec::new());
    let plan = compile(LaunchPlanInputs {
        recorded_system_prompt_digest: None,
        request: &request,
        cwd: project.path(),
        project_root: project.path(),
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: None,
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        isolation: Isolation::Host,
        bwrap: None,
        agents: Ok(&[]),
        ambient_env: &BTreeMap::new(),
        agent_shell: None,
        hibernate: false,
    })
    .unwrap();
    let process = plan.process();
    assert!(!process.provider_argv.join(" ").contains("sk-secret-123"));
    assert!(!process.argv.join(" ").contains("sk-secret-123"));
    assert!(!format!("{process:?}").contains("sk-secret-123"));
    let values: Vec<_> = process
        .provider_argv
        .windows(2)
        .filter(|pair| pair[0] == "--settings")
        .collect();
    assert_eq!(values.len(), 1);
    let path = Path::new(&values[0][1]);
    assert_eq!(path.parent(), Some(runtime.prompt_dir().as_path()));
    assert!(!path.exists());
    std::fs::create_dir_all(runtime.prompt_dir()).unwrap();
    std::fs::set_permissions(runtime.prompt_dir(), std::fs::Permissions::from_mode(0o755)).unwrap();
    apply(&plan).unwrap();
    let merged: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(merged["env"]["ANTHROPIC_API_KEY"], "sk-secret-123");
    assert!(merged["skillOverrides"].is_object());
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(runtime.prompt_dir())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn shared_lsp_configuration_reaches_child_reminders_without_a_live_server() {
    let machine: crate::config::MachineConfig = toml::from_str("[lsp.servers.rust]\ncommand = ['rust-analyzer']\nextensions = ['rs']\nroot-markers = ['Cargo.toml']").unwrap();
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), config.path()).unwrap();
    for subagent in [false, true] {
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
        request.subagent = subagent;
        let (reminder, _) = reminders(&request, Some(&effective), &machine.agents.commands);
        assert!(reminder.lsp_configured);
        assert!(reminder.lsp_servers.is_empty());
    }
}

#[test]
fn broken_effective_config_skips_launch_reminder() {
    let project = tempfile::tempdir().expect("project");
    let config = tempfile::tempdir().expect("config");
    let project_config = project.path().join(".rimz");
    std::fs::create_dir_all(&project_config).expect("create project config dir");
    std::fs::write(
        project_config.join("config.toml"),
        "[profiles.child]\nagent = \"unknown-base\"\n",
    )
    .expect("write broken project config");
    crate::trust::grant_with_roots(project.path(), config.path()).expect("trust project");
    let machine_config = crate::config::MachineConfig::default();
    let request = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );

    let reminders = exec_launch_reminders(&request, &machine_config, project.path(), config.path());
    assert!(reminders.subagent_catalog.is_none());
    assert!(reminders.team.is_none());
    assert!(reminders.model);
}

#[test]
fn profile_model_reminder_flag_reaches_launch_reminders() {
    let project = tempfile::tempdir().expect("project");
    let config = tempfile::tempdir().expect("config");
    let mut machine_config = crate::config::MachineConfig::default();
    for (name, agent, model_reminder) in [
        ("quiet", "claude", false),
        ("loud", "quiet", true),
        ("team-child", "claude", false),
    ] {
        machine_config.agents.profiles.0.insert(
            name.to_owned(),
            crate::config::Profile {
                empty_dir: None,
                definition_renders: None,
                tools: None,
                auto_compact_default: None,
                model_tier: None,
                tier_stamp: None,
                agent: agent.to_owned(),
                isolation: None,
                model_reminder: Some(model_reminder),
                keep_warm: None,
                idle_compact: None,
                description: None,
                subagents: None,
                mode: None,
                model: None,
                effort: None,
                fast: None,
                budget: None,
                auto_compact: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                skills: None,
                allowed_tools: None,
                args: None,
            },
        );
    }
    for (name, model_reminder) in [("quiet", true), ("child", false)] {
        machine_config.subagents.profiles.0.insert(
            name.to_owned(),
            crate::config::Profile {
                empty_dir: None,
                definition_renders: None,
                tools: None,
                auto_compact_default: None,
                model_tier: None,
                tier_stamp: None,
                agent: "claude".to_owned(),
                isolation: None,
                model_reminder: Some(model_reminder),
                keep_warm: None,
                idle_compact: None,
                description: None,
                subagents: None,
                mode: None,
                model: None,
                effort: None,
                fast: None,
                budget: None,
                auto_compact: None,
                system_prompt_file: None,
                append_system_prompt_files: Vec::new(),
                skills: None,
                allowed_tools: None,
                args: None,
            },
        );
    }
    for (profile, subagent, expected) in [
        (Some("quiet"), false, false),
        (Some("loud"), false, true),
        (Some("quiet"), true, true),
        (Some("child"), true, false),
        (Some("team-child"), true, false),
        (None, false, true),
        (None, true, true),
    ] {
        let mut request = request(
            "claude",
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
        );
        request.identity.params.profile = profile.map(str::to_owned);
        request.subagent = subagent;
        let reminders =
            exec_launch_reminders(&request, &machine_config, project.path(), config.path());
        assert_eq!(
            reminders.model, expected,
            "{profile:?}, subagent={subagent}"
        );
    }
}

#[test]
fn malformed_opencode_delegation_config_surfaces_as_a_launch_warning() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("opencode"), Vec::new());
    request.subagent = true;
    let mut actual = Vec::new();
    for raw in ["not JSON", "[]", r#"{"plugins":false}"#] {
        let ambient_env = BTreeMap::from([("OPENCODE_CONFIG_CONTENT".into(), raw.into())]);
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient_env,
            agent_shell: None,
            hibernate: false,
            recorded_system_prompt_digest: None,
        })
        .unwrap();
        actual.push(
            plan.warnings
                .iter()
                .filter(|warning| matches!(warning, LaunchPlanWarning::LaunchEnv(_)))
                .count(),
        );
    }
    assert_eq!(actual, [1, 1, 1]);
}

#[test]
fn malformed_opencode_cli_config_warns_on_every_launch_after_delegation_config() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let mut actual = Vec::new();
    for subagent in [false, true] {
        let mut request =
            ExecRequest::bare_launch(AgentKind::new_unchecked("opencode"), Vec::new());
        request.subagent = subagent;
        let mut ambient_env =
            BTreeMap::from([("OPENCODE_CLI_CONFIG_CONTENT".into(), "not JSON".into())]);
        if subagent {
            ambient_env.insert("OPENCODE_CONFIG_CONTENT".into(), "not JSON".into());
        }
        let plan = compile(LaunchPlanInputs {
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient_env,
            agent_shell: None,
            hibernate: false,
            recorded_system_prompt_digest: None,
        })
        .unwrap();
        actual.push(
            plan.warnings
                .iter()
                .filter(|warning| !matches!(warning, LaunchPlanWarning::DefaultReminders))
                .map(|warning| {
                    (
                        matches!(warning, LaunchPlanWarning::LaunchEnv(_)),
                        warning
                            .to_string()
                            .split_whitespace()
                            .next()
                            .unwrap_or_default()
                            .to_owned(),
                    )
                })
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        actual,
        [
            vec![(true, "OPENCODE_CLI_CONFIG_CONTENT".to_owned())],
            vec![
                (true, "OPENCODE_CONFIG_CONTENT".to_owned()),
                (true, "OPENCODE_CLI_CONFIG_CONTENT".to_owned()),
            ],
        ]
    );
}

#[test]
fn resolves_team_for_launch_context_from_effective_config() {
    let project = tempfile::tempdir().expect("project");
    let config = tempfile::tempdir().expect("config");
    let mut machine_config = crate::config::MachineConfig::default();
    machine_config.subagents.profiles = toml::from_str("[explorer]\nagent = 'claude'\n").unwrap();
    machine_config.agents.teams.0.insert(
        "forge".to_owned(),
        crate::config::Team {
            keep_warm: None,
            roles: vec![
                crate::config::RoleBinding {
                    signals: Vec::new(),
                    owns: Vec::new(),
                    flip_compact: None,
                    idle_compact: None,
                    keep_warm: None,
                    auto_compact: None,
                    role: "planner".to_owned(),
                    profile: "claude".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    fast: None,
                    budget: None,
                    system_prompt_file: None,
                    append_system_prompt_files: Vec::new(),
                    args: None,
                },
                crate::config::RoleBinding {
                    signals: Vec::new(),
                    owns: Vec::new(),
                    flip_compact: None,
                    idle_compact: None,
                    keep_warm: None,
                    auto_compact: None,
                    role: "coder".to_owned(),
                    profile: "codex".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    fast: None,
                    budget: None,
                    system_prompt_file: None,
                    append_system_prompt_files: Vec::new(),
                    args: None,
                },
            ],
            leader: Some("planner".to_owned()),
            layout: None,
            scratch_files: Some(vec!["blackboard.md".to_owned()]),
            consensus_file: None,
            append_system_prompt_files: Vec::new(),
            stages: Vec::new(),
        },
    );
    let mut request = request(
        "codex",
        ExecAction::Resume {
            session_id: "session".to_owned(),
            extra_args: Vec::new(),
        },
    );
    request.identity.params = crate::agents::LaunchParams {
        team: Some("forge".to_owned()),
        role: Some("coder".to_owned()),
        profile: Some("forge.coder".to_owned()),
        channel: Some("feature".to_owned()),
        ..crate::agents::LaunchParams::default()
    };

    let reminders = exec_launch_reminders(&request, &machine_config, project.path(), config.path());
    assert!(matches!(
        reminders.subagent_catalog,
        Some(super::super::subagent_policy::SubagentCatalog::Available(ref profiles))
            if profiles.len() == 1 && profiles[0].name == "explorer"
    ));
    let team = reminders.team.expect("team");
    assert_eq!(team.team.leader.as_deref(), Some("planner"));
    assert_eq!(
        team.team
            .roles
            .iter()
            .map(|role| role.role.as_str())
            .collect::<Vec<_>>(),
        ["planner", "coder"]
    );
    assert_eq!(team.team.scratch_patterns(), ["blackboard.md"]);
    // The effective team supplies the member identities, not model names.
    let context = crate::harness::launch_context::team_launch_context(
        &request.identity.params,
        &request.action,
        &team,
        project.path(),
    )
    .expect("team context");
    assert!(crate::harness::launch_context::reminder(&context).contains(
        "Resumed session; your earlier context continues.\n\nMembers: @planner, @coder (you)."
    ));
}

#[test]
fn omits_team_context_without_team_identity_and_catalog_for_children() {
    let project = tempfile::tempdir().expect("project");
    let config = tempfile::tempdir().expect("config");
    let machine_config = crate::config::MachineConfig::default();
    let request = request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    let reminders = exec_launch_reminders(&request, &machine_config, project.path(), config.path());
    assert!(reminders.subagent_catalog.is_some());
    assert!(reminders.team.is_none());
    assert!(reminders.model);

    let mut child = request;
    child.subagent = true;
    let reminders = exec_launch_reminders(&child, &machine_config, project.path(), config.path());
    assert!(reminders.subagent_catalog.is_none());
    assert!(reminders.team.is_none());
    assert!(reminders.model);
}

#[test]
fn headless_requests_keep_native_delegation_without_a_catalog() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let mut machine = crate::config::MachineConfig::default();
    machine.subagents.profiles = toml::from_str("[explorer]\nagent = 'claude'\n").unwrap();
    let mut request = request("claude", action_with_args("launch", vec![]));
    assert!(matches!(
        exec_launch_reminders(&request, &machine, project.path(), config.path()).subagent_catalog,
        Some(super::super::subagent_policy::SubagentCatalog::Available(_))
    ));
    request.headless = Some(crate::agents::HeadlessRequest {
        schema_open: false,
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: "/check/schema.json".into(),
        verdict_file: "/check/verdict.json".into(),
    });

    let reminders = exec_launch_reminders(&request, &machine, project.path(), config.path());
    assert!(reminders.subagent_catalog.is_none());
    assert!(!reminders.disable_native_subagents(&request));
}

#[test]
fn headless_judge_reminder_omits_subagents_with_profiles_configured() {
    let project = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let mut machine = crate::config::MachineConfig::default();
    machine.subagents.profiles = toml::from_str("[explorer]\nagent = 'claude'\n").unwrap();
    let mut request = request("claude", action_with_args("launch", vec![]));
    request.headless = Some(crate::agents::HeadlessRequest {
        schema_open: false,
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: "/check/schema.json".into(),
        verdict_file: "/check/verdict.json".into(),
    });
    request.loop_reminder = Some("Decide without a user or pane.".into());
    let reminders = exec_launch_reminders(&request, &machine, project.path(), config.path());
    let text = super::super::launch_reminders::render(&request, &reminders, project.path(), None);

    assert!(text.contains("### Judge\n\nDecide without a user or pane."));
    assert!(!text.contains("### Subagents"), "{text}");
}

#[test]
fn materialized_prompt_replaces_raw_prompt_args_for_every_action() {
    let materialized = MaterializedSystemPrompt {
        args: vec![
            "--system-prompt-file".to_owned(),
            "/runtime/prompt/sys.composed.md".to_owned(),
        ],
        env: BTreeMap::new(),
    };
    for action in ["launch", "resume", "fork"] {
        let mut request = request(
            "claude",
            action_with_args(
                action,
                vec![
                    "--system-prompt-file".to_owned(),
                    "/raw.md".to_owned(),
                    "--verbose".to_owned(),
                ],
            ),
        );
        apply_materialized_system_prompt(&mut request, &materialized);
        assert_eq!(
            request.action.extra_args(),
            [
                "--verbose",
                "--system-prompt-file",
                "/runtime/prompt/sys.composed.md"
            ],
            "{action}"
        );
    }

    let mut codex = request(
        "codex",
        action_with_args(
            "resume",
            vec![
                "-c".to_owned(),
                "model_instructions_file=/raw.md".to_owned(),
            ],
        ),
    );
    apply_materialized_system_prompt(
        &mut codex,
        &MaterializedSystemPrompt {
            args: vec![
                "-c".to_owned(),
                "model_instructions_file=/runtime/prompt/sys.composed.md".to_owned(),
            ],
            env: BTreeMap::new(),
        },
    );
    assert_eq!(
        codex.action.extra_args(),
        [
            "-c",
            "model_instructions_file=/runtime/prompt/sys.composed.md"
        ]
    );

    let mut pi = request(
        "pi",
        action_with_args(
            "fork",
            vec!["--system-prompt".to_owned(), "raw text".to_owned()],
        ),
    );
    apply_materialized_system_prompt(
        &mut pi,
        &MaterializedSystemPrompt {
            args: vec![
                "--system-prompt".to_owned(),
                "base text\n\nfragment text\n".to_owned(),
            ],
            env: BTreeMap::new(),
        },
    );
    assert_eq!(
        pi.action.extra_args(),
        ["--system-prompt", "base text\n\nfragment text\n"]
    );
}

#[test]
fn prompt_environment_reaches_qwen_without_entering_argv() {
    let project = tempfile::tempdir().expect("project");
    let base = project.path().join("base.md");
    let fragment = project.path().join("fragment.md");
    std::fs::write(&base, "base\n\n").unwrap();
    std::fs::write(&fragment, "fragment").unwrap();
    let machine = crate::config::MachineConfig::default();
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    for kind in ["qwen", "claude", "pi", "amp"] {
        let root = project.path().join(kind);
        let workspace_id = crate::WorkspaceId::from_project_root(&root);
        let runtime = RuntimePaths::under(workspace_id.clone(), &root).unwrap();
        let state = StatePaths::under(workspace_id, &root).unwrap();
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        request.identity.params.model = Some("test-model".to_owned());
        request.identity.name = Some("swift-otter".to_owned());
        if kind != "amp" {
            request.system_prompt_file = Some(base.clone().into());
            request.append_system_prompt_files = vec![fragment.clone().into()];
        }
        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
            hibernate: false,
        })
        .unwrap();
        assert!(!root.exists(), "{kind} compilation must not write");
        assert!(plan.sandbox.is_none());
        assert!(!plan.process().env.contains_key("SCCACHE_CLIENT_SIDE"));
        let unit = state.temp_unit_dir(Some("swift-otter"));
        assert_eq!(
            plan.process().env.get("TMPDIR").map(PathBuf::from),
            Some(unit.clone())
        );
        assert_eq!(
            plan.process().env.get("RIMZ_SHARED").map(PathBuf::from),
            Some(state.room_shared_dir.clone())
        );
        assert!(!plan.process().env.keys().any(|key| key.contains("SCRATCH")));
        let reminder = &plan.process().reminder;
        assert!(reminder.contains("<system_reminder>"));
        assert_eq!(
            plan.reminder_channel.is_some(),
            matches!(kind, "claude" | "qwen" | "pi")
        );
        if kind == "amp" {
            continue;
        }
        assert_eq!(plan.prompt.composed.as_deref(), Some("base\n\nfragment\n"));
        let artifact = plan.prompt.artifact.as_ref().unwrap();
        assert!(!artifact.exists());
        let process = plan.process();
        if kind == "qwen" {
            assert_eq!(
                process.env.get("QWEN_SYSTEM_MD"),
                artifact.to_str().map(str::to_owned).as_ref()
            );
            assert!(!process.provider_argv.iter().any(|arg| arg.contains("sys.")));
        }
        if kind == "claude" {
            assert!(
                process.provider_argv.windows(2).any(
                    |args| args[0] == "--system-prompt-file" && Path::new(&args[1]) == artifact
                )
            );
            assert!(
                process
                    .provider_argv
                    .windows(2)
                    .any(|args| args[0] == "--append-system-prompt" && &args[1] == reminder)
            );
        }
        apply(&plan).unwrap();
        assert_eq!(
            std::fs::read_to_string(artifact).unwrap(),
            plan.prompt.composed.as_deref().unwrap()
        );
        assert!(
            unit.is_dir() && state.room_shared_dir.is_dir(),
            "{kind}: host launches create their temp unit and the shared dir"
        );
    }
}

#[test]
fn env_reminder_compile_resolves_worktree_from_launch_cwd() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path().canonicalize().unwrap();
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.test",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let created = crate::worktree::create(
        &repo,
        &crate::config::WorktreeConfig {
            dir: root.join("worktrees").display().to_string(),
            ..Default::default()
        },
        Some("demo"),
        None,
        None,
        false,
        crate::worktree::LaunchBase::Configured,
    )
    .unwrap();
    let cwd = &created.marker.worktree_path;
    let machine = crate::config::MachineConfig::default();
    let mut effective = crate::config::effective::load_with_roots(&machine, &root, &root).unwrap();
    let id = crate::WorkspaceId::from_project_root(&root);
    let runtime = RuntimePaths::under(id.clone(), &root).unwrap();
    let state = StatePaths::under(id, &root).unwrap();
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    for subagent in [false, true] {
        request.subagent = subagent;
        for enabled in [true, false] {
            effective.env_reminder = enabled;
            let plan = compile(LaunchPlanInputs {
                recorded_system_prompt_digest: None,
                request: &request,
                cwd,
                project_root: &root,
                rimz_bin: Path::new("/bin/rimz"),
                runtime: &runtime,
                state: &state,
                effective: Some(&effective),
                commands: &machine.agents.commands,
                accounts: &machine.accounts,
                isolation: Isolation::Host,
                bwrap: None,
                agents: Ok(&[]),
                ambient_env: &BTreeMap::new(),
                agent_shell: None,
                hibernate: false,
            })
            .unwrap();
            let reminder = &plan.process().reminder;
            assert_eq!(
                reminder.contains(&format!(
                    "- cwd: {}\n- worktree: branched from main; primary checkout at {}",
                    cwd.display(),
                    repo.display()
                )),
                enabled
            );
            assert_eq!(reminder.contains("- worktree:"), enabled);
            assert!(plan.warnings.is_empty());
        }
    }
}

#[test]
fn env_reminder_compile_uses_launch_cwd_and_effective_switch_for_children_too() {
    let project = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let mut effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).unwrap();
    let state = StatePaths::under(workspace_id, project.path()).unwrap();
    for subagent in [false, true] {
        for enabled in [false, true] {
            effective.env_reminder = enabled;
            for cwd in [project.path(), other.path()] {
                let mut request =
                    ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
                request.subagent = subagent;
                let plan = compile(LaunchPlanInputs {
                    recorded_system_prompt_digest: None,
                    request: &request,
                    cwd,
                    project_root: project.path(),
                    rimz_bin: Path::new("/bin/rimz"),
                    runtime: &runtime,
                    state: &state,
                    effective: Some(&effective),
                    commands: &machine.agents.commands,
                    accounts: &machine.accounts,
                    isolation: Isolation::Host,
                    bwrap: None,
                    agents: Ok(&[]),
                    ambient_env: &BTreeMap::new(),
                    agent_shell: None,
                    hibernate: false,
                })
                .unwrap();
                assert_eq!(
                    plan.process()
                        .reminder
                        .contains(&format!("### Environment\n\n- cwd: {}", cwd.display())),
                    enabled
                );
                assert!(!plan.process().reminder.contains("git status"));
                assert!(plan.warnings.is_empty());
            }
        }
    }
}

#[test]
fn runtime_env_compile_stamps_the_switch_and_moves_the_listing_out_of_the_reminder() {
    use crate::harness::launch::ENV_RUNTIME_ENV;

    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let mut effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    effective.teams.0.insert(
        "forge".to_owned(),
        toml::from_str(
            "leader = 'planner'\n[[roles]]\nrole = 'planner'\nprofile = 'claude'\nowns = ['Plan']",
        )
        .unwrap(),
    );
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).unwrap();
    let state = StatePaths::under(workspace_id, project.path()).unwrap();
    #[derive(Clone, Copy, Debug)]
    enum Hooks {
        Absent,
        Untrusted,
        Wired,
    }
    for (kind, flag, configured, hooks, runtime_env) in [
        ("claude", true, true, Hooks::Wired, true),
        ("codex", true, true, Hooks::Wired, true),
        ("claude", true, false, Hooks::Wired, true),
        ("claude", false, true, Hooks::Wired, false),
        ("droid", true, true, Hooks::Wired, false),
        ("claude", true, true, Hooks::Absent, false),
        ("codex", true, true, Hooks::Absent, false),
        ("codex", true, true, Hooks::Untrusted, false),
    ] {
        let home = tempfile::tempdir().unwrap();
        let ambient = BTreeMap::from(
            [
                (ENV_RUNTIME_ENV, "1".to_owned()),
                ("HOME", home.path().display().to_string()),
                ("CLAUDE_CONFIG_DIR", home.path().display().to_string()),
                ("CODEX_HOME", home.path().display().to_string()),
            ]
            .map(|(key, value)| (key.to_owned(), value)),
        );
        let adapter = crate::agents::find_definition(kind).unwrap();
        if !matches!(hooks, Hooks::Absent) {
            adapter.install_hooks(&ambient, Default::default()).unwrap();
        }
        if matches!(hooks, Hooks::Wired) && kind == "codex" {
            let config = home.path().join("config.toml");
            crate::agents::trust_hook_config_for_test(&config, &["Interrupt"]).unwrap();
        }
        effective.runtime_env = flag;
        let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        request.identity.params.team = Some("forge".to_owned());
        request.identity.params.role = Some("planner".to_owned());
        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: configured.then_some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
            hibernate: false,
        })
        .unwrap();
        let process = plan.process();
        let case = format!("{kind} flag={flag} configured={configured} hooks={hooks:?}");
        assert_eq!(
            process.env.get(ENV_RUNTIME_ENV).map(String::as_str),
            runtime_env.then_some("1"),
            "{case}"
        );
        assert_eq!(
            process.unset.contains(ENV_RUNTIME_ENV),
            !runtime_env,
            "{case}"
        );
        assert_eq!(
            process.reminder.contains("$ ls .rimz/team/*.md"),
            configured && !runtime_env,
            "{case}: the listing is in the reminder or at prompt submit, never both"
        );
    }
}

/// A room whose `workspace.json` freezes `logins`, with the machine config
/// that declares those accounts.
fn room_with_accounts(
    root: &Path,
    accounts_toml: &str,
    logins: &[(&str, &str)],
) -> (crate::config::MachineConfig, StatePaths) {
    let machine = crate::config::MachineConfig {
        accounts: toml::from_str(accounts_toml).expect("accounts config"),
        ..crate::config::MachineConfig::default()
    };
    let workspace = crate::workspace::WorkspaceResolver::resolve(root, None).expect("workspace");
    let state = StatePaths::under(workspace.workspace_id.clone(), root).expect("state paths");
    state.ensure_dirs().expect("state dirs");
    let mut record = crate::workspace::record::WorkspaceRecord::from_resolved(&workspace);
    record.pins = logins
        .iter()
        .map(|(kind, name)| {
            (
                AgentKind::new_unchecked(*kind),
                name.parse().expect("login name"),
            )
        })
        .collect();
    crate::workspace::record::write(&state, &record).expect("write record");
    (machine, state)
}

#[test]
fn the_stamped_account_sets_the_provider_home_after_the_room_switches() {
    let project = tempfile::tempdir().expect("project");
    let home = project.path().join("claude-work");
    let (machine, state) = room_with_accounts(
        project.path(),
        &format!(
            "[claude.work]\nhome = {:?}\n[claude.personal]\nhome = {:?}",
            home.display().to_string(),
            project.path().join("personal").display().to_string()
        ),
        &[("claude", "personal")],
    );
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .expect("runtime paths");
    for action in ["launch", "resume", "fork"] {
        let mut request = request("claude", action_with_args(action, Vec::new()));
        request.identity.params.login = Some("work".parse().unwrap());

        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
            hibernate: false,
        })
        .expect("compile");

        assert_eq!(plan.login.key().to_string(), "claude@work");
        assert_eq!(
            plan.process()
                .env
                .get("CLAUDE_CONFIG_DIR")
                .map(String::as_str),
            Some(home.to_string_lossy().as_ref())
        );
        assert_eq!(plan.process().env.get("CODEX_HOME"), None);
    }
}

#[test]
fn the_default_account_leaves_the_provider_home_to_the_provider() {
    let project = tempfile::tempdir().expect("project");
    let machine = crate::config::MachineConfig::default();
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).expect("runtime");
    let state = StatePaths::under(workspace_id, project.path()).expect("state");

    for kind in ["claude", "codex"] {
        let request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &BTreeMap::new(),
            agent_shell: None,
            hibernate: false,
        })
        .expect("compile");

        assert!(plan.login.is_default(), "{kind}");
        assert_eq!(plan.process().env.get("CLAUDE_CONFIG_DIR"), None);
        assert_eq!(plan.process().env.get("CODEX_HOME"), None);
        assert_eq!(
            plan.process().env.get(Isolation::ENV).map(String::as_str),
            Some("host"),
            "{kind}"
        );
    }
}

#[test]
fn default_launch_unsets_only_an_exported_declared_home() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig {
        accounts: toml::from_str("[claude.work]\nhome = '/srv/work'\n").unwrap(),
        ..Default::default()
    };
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).unwrap();
    let state = StatePaths::under(workspace_id, project.path()).unwrap();
    for (exported, unset) in [("/srv/work", true), ("/srv/elsewhere", false)] {
        let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
        let ambient = BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), exported.to_owned())]);
        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
            hibernate: false,
        })
        .unwrap();
        assert_eq!(
            plan.process().unset.contains("CLAUDE_CONFIG_DIR"),
            unset,
            "{exported}"
        );
    }
}

#[test]
fn default_launch_keeps_a_trusted_project_home_over_an_exported_declared_one() {
    if std::env::var_os("RIMZ_TEST_PROJECT_HOME").is_none() {
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "harness::launch_plan::tests::default_launch_keeps_a_trusted_project_home_over_an_exported_declared_one",
                "--nocapture",
            ])
            .env("RIMZ_HOME", home.path().join(".rimz"))
            .env("RIMZ_TEST_PROJECT_HOME", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".rimz")).unwrap();
    std::fs::write(
        project.path().join(".rimz/config.toml"),
        "[[agents]]\nname = \"claude\"\nenv = { CLAUDE_CONFIG_DIR = \"/srv/project\" }\n",
    )
    .unwrap();
    crate::trust::grant_with_roots(project.path(), &crate::disk::paths::rimz_home()).unwrap();
    let machine = crate::config::MachineConfig {
        accounts: toml::from_str("[claude.work]\nhome = '/srv/work'\n").unwrap(),
        ..Default::default()
    };
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    let workspace_id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), project.path()).unwrap();
    let state = StatePaths::under(workspace_id, project.path()).unwrap();
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    let ambient = BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), "/srv/work".to_owned())]);
    let plan = compile(LaunchPlanInputs {
        recorded_system_prompt_digest: None,
        request: &request,
        cwd: project.path(),
        project_root: project.path(),
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: Some(&effective),
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        isolation: Isolation::Host,
        bwrap: None,
        agents: Ok(&[]),
        ambient_env: &ambient,
        agent_shell: None,
        hibernate: false,
    })
    .unwrap();
    assert!(!plan.process().unset.contains("CLAUDE_CONFIG_DIR"));
    assert_eq!(
        plan.process()
            .env
            .get("CLAUDE_CONFIG_DIR")
            .map(String::as_str),
        Some("/srv/project")
    );
}

#[test]
fn a_stamped_account_the_config_no_longer_declares_fails_the_launch() {
    let project = tempfile::tempdir().expect("project");
    let (machine, state) = room_with_accounts(project.path(), "", &[("claude", "work")]);
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .expect("runtime");
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.params.login = Some("work".parse().unwrap());

    let error = compile(LaunchPlanInputs {
        recorded_system_prompt_digest: None,
        request: &request,
        cwd: project.path(),
        project_root: project.path(),
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: Some(&effective),
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        isolation: Isolation::Host,
        bwrap: None,
        agents: Ok(&[]),
        ambient_env: &BTreeMap::new(),
        agent_shell: None,
        hibernate: false,
    });

    let error = match error {
        Err(error) => error,
        Ok(_) => panic!("an undeclared account cannot launch"),
    };
    assert!(
        error.to_string().contains("rimz accounts add claude work"),
        "{error}"
    );
}

#[test]
fn the_scoped_view_binds_and_pins_the_stamped_account_home() {
    let project = tempfile::tempdir().expect("project");
    let home = project.path().join("codex-personal");
    std::fs::create_dir_all(&home).expect("account home");
    let (machine, state) = room_with_accounts(
        project.path(),
        &format!("[codex.personal]\nhome = {:?}", home.display().to_string()),
        &[("codex", "personal")],
    );
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .expect("runtime paths");
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("codex"), Vec::new());
    request.identity.params.login = Some("personal".parse().unwrap());

    for (cwd, expected) in [
        (project.path().to_path_buf(), project.path().to_path_buf()),
        (
            state.temp_unit_dir(None).join("clean"),
            PathBuf::from("/tmp/clean"),
        ),
    ] {
        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: &cwd,
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Scoped,
            bwrap: Some(Path::new("/usr/bin/bwrap")),
            agents: Ok(&[]),
            ambient_env: &BTreeMap::from([(
                "HOME".to_owned(),
                project.path().display().to_string(),
            )]),
            agent_shell: None,
            hibernate: false,
        })
        .expect("compile cwd");
        assert_eq!(plan.cwd, cwd);
        assert!(
            plan.process()
                .argv
                .windows(2)
                .any(|pair| pair[0] == "--chdir" && Path::new(&pair[1]) == expected)
        );
    }

    let plan = compile(LaunchPlanInputs {
        recorded_system_prompt_digest: None,
        request: &request,
        cwd: project.path(),
        project_root: project.path(),
        rimz_bin: Path::new("/bin/rimz"),
        runtime: &runtime,
        state: &state,
        effective: Some(&effective),
        commands: &machine.agents.commands,
        accounts: &machine.accounts,
        isolation: Isolation::Scoped,
        bwrap: Some(Path::new("/usr/bin/bwrap")),
        agents: Ok(&[]),
        ambient_env: &BTreeMap::from([
            ("HOME".to_owned(), project.path().display().to_string()),
            ("SCCACHE_CLIENT_SIDE".to_owned(), "0".to_owned()),
            (
                crate::child_process::TEMP_ROOT_KEYS_ENV.to_owned(),
                "CLAUDE_CODE_TMPDIR".to_owned(),
            ),
        ]),
        agent_shell: None,
        hibernate: false,
    })
    .expect("compile");

    assert!(
        !plan
            .process()
            .argv
            .windows(2)
            .any(|pair| pair == ["--sandbox", "danger-full-access"])
    );
    assert_eq!(
        plan.process().env.get(Isolation::ENV).map(String::as_str),
        Some("scoped")
    );
    assert_eq!(
        plan.process().env.get("TMPDIR").map(String::as_str),
        Some("/tmp")
    );
    assert!(
        !plan
            .process()
            .env
            .contains_key(crate::child_process::USER_TMPDIR_ENV),
        "a mux child inside the view keeps /tmp"
    );
    assert!(
        plan.process().unset.contains("CLAUDE_CODE_TMPDIR"),
        "a parent's temp root stays unset after the sandbox pins"
    );
    assert_eq!(
        plan.process().env.get("RIMZ_SHARED").map(PathBuf::from),
        Some(state.room_shared_dir.clone())
    );
    assert_eq!(
        plan.process()
            .env
            .get("SCCACHE_CLIENT_SIDE")
            .map(String::as_str),
        Some("1"),
        "the view compiles client-side over an ambient opt-out"
    );
    let sandbox = plan.sandbox.as_ref().expect("sandbox plan");
    assert!(
        sandbox.plan.mounts.contains(&crate::sandbox::Mount::Bind {
            source: state.temp_unit_dir(None),
            target: "/tmp".into(),
        }),
        "a launch without a handle binds the unnamed unit"
    );
    assert!(
        sandbox.plan.mounts.iter().any(
            |mount| matches!(mount, crate::sandbox::Mount::Bind { source, .. } if source == &home)
        ),
        "the named account home is bind-mounted"
    );
    assert_eq!(
        sandbox.pins.get("CODEX_HOME"),
        Some(&crate::sandbox::EnvPin::Set(
            home.to_string_lossy().into_owned()
        ))
    );
}

#[test]
fn a_launch_saves_the_user_tmpdir_it_replaces() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    let other_unit = state.temp_unit_dir(Some("otter")).display().to_string();
    let saved = |ambient: &[(&str, &str)]| {
        let ambient = ambient
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            agent_shell: None,
            hibernate: false,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
        })
        .unwrap();
        assert_eq!(
            plan.process().env.get("TMPDIR").map(PathBuf::from),
            Some(state.temp_unit_dir(None))
        );
        plan.process()
            .env
            .get(crate::child_process::USER_TMPDIR_ENV)
            .cloned()
    };
    let user = crate::child_process::USER_TMPDIR_ENV;

    assert_eq!(
        saved(&[("TMPDIR", "/var/folders/t")]).as_deref(),
        Some("/var/folders/t")
    );
    assert_eq!(saved(&[]).as_deref(), Some(""), "present and empty is none");
    assert_eq!(saved(&[("TMPDIR", "")]).as_deref(), Some(""));
    assert_eq!(
        saved(&[(user, "/var/folders/t"), ("TMPDIR", &other_unit)]).as_deref(),
        Some("/var/folders/t"),
        "a launch inside an agent's tree carries the user's value forward"
    );
    assert_eq!(
        saved(&[(user, ""), ("TMPDIR", &other_unit)]).as_deref(),
        Some("")
    );
}

#[test]
fn a_provider_temp_root_follows_tmpdir() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let keys = crate::child_process::TEMP_ROOT_KEYS_ENV;
    for (kind, ambient, listed, unset) in [
        ("claude", None, "CLAUDE_CODE_TMPDIR", &[][..]),
        ("codex", None, "", &[]),
        (
            "codex",
            Some("CLAUDE_CODE_TMPDIR"),
            "",
            &["CLAUDE_CODE_TMPDIR"],
        ),
        (
            "claude",
            Some("CLAUDE_CODE_TMPDIR X_TMPDIR"),
            "CLAUDE_CODE_TMPDIR",
            &["X_TMPDIR"],
        ),
    ] {
        let request = ExecRequest::bare_launch(AgentKind::new_unchecked(kind), Vec::new());
        let ambient = ambient
            .map(|list| BTreeMap::from([(keys.to_owned(), list.to_owned())]))
            .unwrap_or_default();
        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: None,
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            agent_shell: None,
            hibernate: false,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
        })
        .unwrap();
        let env = &plan.process().env;
        let expected = (kind == "claude").then(|| env["TMPDIR"].clone());
        assert_eq!(env.get("CLAUDE_CODE_TMPDIR").cloned(), expected, "{kind}");
        assert_eq!(
            env.get(keys).map(String::as_str),
            Some(listed),
            "{kind} over {ambient:?}"
        );
        assert_eq!(
            plan.process()
                .unset
                .iter()
                .map(String::as_str)
                // Identity pins and launch switches have their own rules, not
                // temp-root policy.
                .filter(|key| {
                    !matches!(
                        *key,
                        "RIMZ_WORKFLOW_RUN" | "RIMZ_WORKFLOW_DIR" | "RIMZ_LAUNCH_FAST"
                    )
                        && *key != crate::harness::launch::ENV_RUNTIME_ENV
                        && *key != crate::harness::launch::ENV_AGENT_MODEL
                        && *key != crate::harness::launch::ENV_TEAM
                        && *key != crate::harness::launch::ENV_AGENT_ROLE
                })
                .collect::<Vec<_>>(),
            unset,
            "{kind} over {ambient:?}"
        );
    }
}

#[test]
fn a_launched_child_shares_its_parent_unit_across_restart() {
    let project = tempfile::tempdir().unwrap();
    let machine = crate::config::MachineConfig::default();
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .unwrap();
    let id = crate::WorkspaceId::from_project_root(project.path());
    let runtime = RuntimePaths::under(id.clone(), project.path()).unwrap();
    let state = StatePaths::under(id, project.path()).unwrap();
    let mut parent =
        crate::testkit::agent_state("claude", "parent-session", jiff::Timestamp::UNIX_EPOCH);
    parent.name = Some("otter".to_owned());
    // The unit follows the durable parent stamp, not the `subagent` flag.
    let mut request = ExecRequest::bare_launch(AgentKind::new_unchecked("claude"), Vec::new());
    request.identity.name = Some("fox".to_owned());
    request.identity.params.parent_agent_id = Some("parent-session".into());
    request.identity.params.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
    request.identity.params.checkout = Some("/parent-checkout".to_owned());
    let empty_cwd = project.path().join("empty-agent");
    let ambient = BTreeMap::from([("XDG_RUNTIME_DIR".to_owned(), "/run/user/1000".to_owned())]);
    let compile_with = |agents: Result<&[crate::agents::AgentState], &str>| {
        compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: &empty_cwd,
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            agent_shell: None,
            hibernate: false,
            isolation: Isolation::Host,
            bwrap: None,
            agents,
            ambient_env: &ambient,
        })
        .unwrap()
    };
    let parented = |plan: &LaunchPlan| {
        plan.warnings
            .iter()
            .any(|warning| warning.to_string().contains("parent-session"))
    };

    let plan = compile_with(Ok(std::slice::from_ref(&parent)));
    assert_eq!(plan.cwd, empty_cwd);
    assert_eq!(
        plan.process()
            .env
            .get(crate::workspace::ENV_WORKTREE_PATH)
            .map(String::as_str),
        Some("/parent-checkout")
    );
    assert_eq!(
        plan.process().env.get("RIMZ_SHARED").map(PathBuf::from),
        Some(state.room_shared_dir.clone())
    );
    assert_eq!(
        plan.process().env.get("TMPDIR").map(PathBuf::from),
        Some(state.temp_unit_dir(Some("otter")))
    );
    assert!(
        plan.process()
            .reminder
            .contains("every temporary file you make. You share it with your caller;")
    );
    assert!(!parented(&plan));
    if cfg!(target_os = "linux") {
        assert_eq!(
            plan.process()
                .env
                .get("ZELLIJ_SOCKET_DIR")
                .map(String::as_str),
            Some("/run/user/1000/zellij"),
            "moving TMPDIR pins the socket base the wrapper resolved"
        );
    }

    let orphan = compile_with(Ok(&[]));
    assert_eq!(
        orphan.process().env.get("TMPDIR").map(PathBuf::from),
        Some(state.temp_unit_dir(Some("fox")))
    );
    assert!(
        orphan
            .process()
            .reminder
            .contains("every temporary file you make. You share it with your caller;"),
        "a child is never told it has subagents"
    );
    assert!(parented(&orphan), "a lost parent row warns");
    let unread = compile_with(Err("snapshot unreadable"));
    let warning = unread
        .warnings
        .iter()
        .map(ToString::to_string)
        .find(|warning| warning.contains("parent-session"))
        .expect("an unread room warns");
    assert!(
        warning.contains("snapshot unreadable") && !warning.contains("has no row"),
        "{warning}"
    );
    assert_eq!(
        unread.process().env.get("TMPDIR").map(PathBuf::from),
        Some(state.temp_unit_dir(Some("fox")))
    );

    let mut child =
        crate::testkit::agent_state("claude", "child-session", jiff::Timestamp::UNIX_EPOCH);
    child.name = Some("fox".to_owned());
    child.parent_agent_id = Some("parent-session".into());
    assert_eq!(
        agent_temp_unit(&child, std::slice::from_ref(&parent), &state),
        state.temp_unit_dir(Some("otter")),
        "agents show names the unit the launch used"
    );
}

#[test]
fn indexed_temp_owners_match_single_agent_selection() {
    let at = jiff::Timestamp::UNIX_EPOCH;
    let parent = |id: &str, name: Option<&str>, launch: &str, ended: bool| {
        let mut row = crate::testkit::agent_state("claude", id, at);
        row.name = name.map(str::to_owned);
        row.launch_id = Some(launch.into());
        row.ended_at = ended.then_some(at);
        row
    };
    let mut restarted = parent("restarted", Some("restart-owner"), "current-launch", false);
    restarted.prior_launch_ids = vec!["prior-launch".into()];
    let mut rows = vec![
        parent(
            "legacy-alias",
            Some("old-alias-owner"),
            "alias-launch",
            true,
        ),
        parent("alias-current", Some("alias-owner"), "alias-launch", false),
        parent(
            "prior-parent",
            Some("old-restart-owner"),
            "prior-launch",
            true,
        ),
        restarted,
        parent("many-ended", Some("ended-owner"), "many-launch", true),
        parent("many-live-a", Some("live-owner-a"), "many-launch", false),
        parent("many-live-b", Some("live-owner-b"), "many-launch", false),
        parent("nameless-parent", None, "nameless-launch", false),
    ];
    let cases = [
        ("alias-child", Some("legacy-alias")),
        ("prior-child", Some("prior-launch")),
        ("current-child", Some("current-launch")),
        ("many-child", Some("many-launch")),
        ("missing-child", Some("missing-parent")),
        ("unparented", None),
        ("nameless-child", Some("nameless-launch")),
    ];
    for (name, parent) in cases {
        let mut row = crate::testkit::agent_state("codex", name, at);
        row.name = Some(name.to_owned());
        row.parent_agent_id = parent.map(Into::into);
        row.parent_agent_kind = parent.map(|_| AgentKind::new_unchecked("claude"));
        rows.push(row);
    }
    let indexed = AgentTempOwners::new(&rows);
    for row in &rows {
        assert_eq!(
            indexed.owner(row),
            agent_temp_owner(row, &rows),
            "owner of {}",
            row.agent_id
        );
    }
    for (name, expected) in [
        ("alias-child", Some("alias-owner")),
        ("prior-child", Some("restart-owner")),
        ("current-child", Some("restart-owner")),
        ("missing-child", Some("missing-child")),
        ("unparented", Some("unparented")),
        ("nameless-child", None),
    ] {
        let row = rows
            .iter()
            .find(|row| row.agent_id.as_str() == name)
            .unwrap();
        assert_eq!(indexed.owner(row), expected, "{name}");
    }
}

#[test]
fn a_shared_codex_account_points_its_databases_at_the_default_home_without_touching_either() {
    let project = tempfile::tempdir().expect("project");
    let work = project.path().join("codex-work");
    let solo = project.path().join("codex-solo");
    let native = project.path().join("home/.codex");
    for dir in [&work, &solo, &native] {
        std::fs::create_dir_all(dir).expect("home");
    }
    std::fs::write(native.join("config.toml"), "").expect("default config");
    let (machine, state) = room_with_accounts(
        project.path(),
        &format!(
            "[codex.work]\nhome = {:?}\n[codex.solo]\nhome = {:?}\nhistory = \"standalone\"",
            work.display().to_string(),
            solo.display().to_string()
        ),
        &[("codex", "work")],
    );
    let effective =
        crate::config::effective::load_with_roots(&machine, project.path(), project.path())
            .expect("effective config");
    let runtime = RuntimePaths::under(
        crate::WorkspaceId::from_project_root(project.path()),
        project.path(),
    )
    .expect("runtime paths");
    // The pane this launch starts from runs on the other account.
    let ambient = BTreeMap::from([
        (
            "HOME".to_owned(),
            project.path().join("home").display().to_string(),
        ),
        ("CODEX_HOME".to_owned(), solo.display().to_string()),
    ]);
    for (login, databases) in [("work", Some(&native)), ("solo", None)] {
        let mut request = request("codex", action_with_args("launch", Vec::new()));
        request.identity.params.login = Some(login.parse().unwrap());
        let plan = compile(LaunchPlanInputs {
            recorded_system_prompt_digest: None,
            request: &request,
            cwd: project.path(),
            project_root: project.path(),
            rimz_bin: Path::new("/bin/rimz"),
            runtime: &runtime,
            state: &state,
            effective: Some(&effective),
            commands: &machine.agents.commands,
            accounts: &machine.accounts,
            isolation: Isolation::Host,
            bwrap: None,
            agents: Ok(&[]),
            ambient_env: &ambient,
            agent_shell: None,
            hibernate: false,
        })
        .expect("compile");
        assert_eq!(
            plan.process()
                .env
                .get("CODEX_SQLITE_HOME")
                .map(PathBuf::from),
            databases.cloned(),
            "{login}"
        );
    }
    for home in [&work, &solo] {
        assert_eq!(std::fs::read_dir(home).expect("home").count(), 0);
    }
}

fn host_request(kind: &str, action: ExecAction) -> ExecRequest {
    let mut request = request(kind, action);
    request.identity.params.isolation = Some(Isolation::Host);
    request
}

fn envelope(request: &ExecRequest, prompt_file: Option<&Path>) -> launch::ExecEnvelope {
    let mut wire = serde_json::to_value(request).unwrap();
    if let Some(path) = prompt_file {
        wire["prompt_file"] = path.display().to_string().into();
    }
    launch::decode_exec_envelope(request.kind.as_str(), None, &wire.to_string()).unwrap()
}

fn prepare(
    request: &ExecRequest,
    prompt_file: Option<&Path>,
    machine: &crate::config::MachineConfig,
    effective: Option<&LaunchAgents>,
    room_agents: &dyn Fn() -> Result<Vec<crate::agents::AgentState>, String>,
) -> ExecPreparation {
    let root = tempfile::tempdir().unwrap();
    prepare_exec(
        envelope(request, prompt_file),
        root.path(),
        root.path(),
        Path::new("/bin/rimz"),
        machine,
        effective,
        room_agents,
        |_, _| Ok(Vec::new()),
    )
}

#[test]
fn pi_resume_and_fork_restore_the_session_model_unless_overridden() {
    let machine: crate::config::MachineConfig =
        toml::from_str("[models.pi]\nsonnet = 'pinned-model'\n").unwrap();
    for action in ["resume", "fork"] {
        for override_model in [false, true] {
            for recorded in [false, true] {
                for typed in [false, true] {
                    let mut request = host_request(
                        "pi",
                        action_with_args(
                            action,
                            vec![
                                "--provider".into(),
                                "anthropic".into(),
                                "--model".into(),
                                "sonnet".into(),
                                "--model=raw-model".into(),
                                "--thinking".into(),
                                "high".into(),
                            ],
                        ),
                    );
                    request.identity.params.model = typed.then(|| "sonnet".into());
                    request.identity.params.record = recorded.then(|| {
                        Box::new(crate::agents::LaunchRecord {
                            model: Some("sonnet".into()),
                            ..Default::default()
                        })
                    });
                    request.identity.resume_model_override = override_model;
                    let prepared = prepare(&request, None, &machine, None, &|| Ok(Vec::new()));
                    let (plan, _) = prepared.outcome.expect("Pi session compiles");
                    let args = plan.request.action.extra_args();
                    let matcher = crate::agents::find_definition("pi")
                        .unwrap()
                        .spec()
                        .launch
                        .preset_arg_matcher(crate::agents::PresetField::Model)
                        .unwrap();
                    let models = matcher.occurrences(args);
                    let providers =
                        crate::agents::PresetArgMatcher::Flag(vec!["--provider".into()])
                            .occurrences(args);
                    assert_eq!(
                        providers.len(),
                        usize::from(override_model),
                        "{action} record={recorded} typed={typed}: {args:?}"
                    );
                    if override_model && typed {
                        assert_eq!(
                            models
                                .iter()
                                .map(|model| model.value.as_str())
                                .collect::<Vec<_>>(),
                            ["pinned-model"]
                        );
                    } else if override_model {
                        assert_eq!(
                            models
                                .iter()
                                .map(|model| model.value.as_str())
                                .collect::<Vec<_>>(),
                            ["sonnet", "raw-model"]
                        );
                    } else {
                        assert!(
                            models.is_empty(),
                            "{action} record={recorded} typed={typed}: {args:?}"
                        );
                        assert!(prepared.model_warnings.is_empty());
                    }
                    assert!(args.windows(2).any(|pair| pair == ["--thinking", "high"]));
                    assert_eq!(
                        plan.request
                            .identity
                            .params
                            .record
                            .as_deref()
                            .unwrap()
                            .model
                            .as_deref(),
                        (recorded || typed).then_some("sonnet")
                    );
                }
            }
        }
    }
}

#[test]
fn pi_session_restore_does_not_claim_the_requested_model_in_the_reminder() {
    let machine: crate::config::MachineConfig =
        toml::from_str("[models.pi]\nsonnet = 'pinned-model'\n").unwrap();
    let mut session = crate::testkit::agent_state("pi", "session", jiff::Timestamp::now());
    session.model = Some("observed-switch".into());
    for action in ["resume", "fork"] {
        for override_model in [false, true] {
            let mut request = host_request("pi", action_with_args(action, Vec::new()));
            request.identity.params.model = Some("sonnet".into());
            request.identity.resume_model_override = override_model;
            let prepared = prepare(&request, None, &machine, None, &|| {
                Ok(vec![session.clone()])
            });
            let (plan, _) = prepared.outcome.unwrap();
            let reminder = &plan.process().reminder;
            assert_eq!(
                reminder.contains("running on"),
                override_model,
                "{action} override={override_model}: {reminder}"
            );
        }
    }
}

#[test]
fn pi_session_restore_does_not_seed_the_requested_model_as_running() {
    let machine: crate::config::MachineConfig =
        toml::from_str("[models.pi]\nsonnet = 'pinned-model'\n").unwrap();
    let mut session = crate::testkit::agent_state("pi", "session", jiff::Timestamp::now());
    session.model = Some("observed-switch".into());
    for action in ["resume", "fork"] {
        for override_model in [false, true] {
            let mut request = host_request("pi", action_with_args(action, Vec::new()));
            request.identity.params.model = Some("sonnet".into());
            request.identity.resume_model_override = override_model;
            let prepared = prepare(&request, None, &machine, None, &|| {
                Ok(vec![session.clone()])
            });
            let (plan, _) = prepared.outcome.unwrap();
            assert_eq!(
                plan.process()
                    .env
                    .get(launch::ENV_AGENT_MODEL)
                    .map(String::as_str),
                override_model.then_some("pinned-model"),
                "{action} override={override_model}"
            );
            assert_eq!(
                plan.process().unset.contains(launch::ENV_AGENT_MODEL),
                !override_model,
                "{action} override={override_model}: inherited model must be cleared"
            );
        }
    }
}

#[test]
fn exec_scope_requires_cached_admission_even_when_explicitly_enabled() {
    let mut machine = crate::config::MachineConfig::default();
    machine.agents.hibernate.enabled = Some(true);
    let request = host_request("claude", action_with_args("launch", Vec::new()));
    let root = tempfile::tempdir().unwrap();
    let state = StatePaths::for_project_root(root.path()).unwrap();
    let runtime = RuntimePaths::for_state(&state).unwrap();
    for available in [false, true] {
        if available {
            crate::disk::atomic::write_temp_then_rename_cache(
                &runtime.live_path("hibernate").join("admission.json"),
                &super::super::hibernate::Admission {
                    available,
                    reason: None,
                },
            )
            .unwrap();
        }
        let prepared = prepare_exec(
            envelope(&request, None),
            root.path(),
            root.path(),
            Path::new("/bin/rimz"),
            &machine,
            None,
            &|| Ok(Vec::new()),
            |_, _| Ok(Vec::new()),
        );
        let (plan, _) = prepared.outcome.unwrap();
        assert_eq!(
            plan.process().argv[0] == "systemd-run",
            available && cfg!(target_os = "linux")
        );
    }
}

#[test]
fn exec_refuses_only_fresh_launches_of_an_unshadowed_failed_profile() {
    let root = tempfile::tempdir().unwrap();
    let mut machine = crate::config::MachineConfig::default();
    let path = PathBuf::from("/tmp/.agents/agents/worker.md");
    machine
        .notices
        .failed_definitions
        .insert("worker".to_owned(), [path.clone()].into());
    machine
        .notices
        .definition_errors
        .push(crate::config::definitions::DefinitionErr {
            path,
            message: "invalid frontmatter".to_owned(),
            cause: crate::config::definitions::DefinitionCause::Invalid,
        });
    let request = |action| {
        let mut request = request("codex", action);
        request.identity.params.profile = Some("worker".to_owned());
        request
    };
    let launch = request(ExecAction::Launch {
        prompt: None,
        extra_args: Vec::new(),
    });
    let resume = request(ExecAction::Resume {
        session_id: "s".to_owned(),
        extra_args: Vec::new(),
    });
    let fork = request(ExecAction::Fork {
        session_id: "s".to_owned(),
        extra_args: Vec::new(),
    });
    let mut effective =
        crate::config::effective::load_with_roots(&machine, root.path(), &root.path().join("home"))
            .unwrap();

    let detail = "/tmp/.agents/agents/worker.md: invalid frontmatter";
    assert_eq!(
        definition_failure(&launch, &machine, Some(&effective)).as_deref(),
        Some(detail)
    );
    let refused = prepare(&launch, None, &machine, Some(&effective), &|| {
        panic!("the definition gate reads no room")
    })
    .outcome
    .err()
    .expect("a failed definition refuses a fresh launch");
    assert_eq!(refused.to_string(), detail);
    assert!(std::error::Error::source(&refused).is_none());
    assert!(definition_failure(&launch, &machine, None).is_some());
    assert_eq!(
        definition_failure(&resume, &machine, Some(&effective)),
        None
    );
    assert_eq!(definition_failure(&fork, &machine, Some(&effective)), None);

    let shadow: crate::config::Profile = toml::from_str("agent = 'claude'").unwrap();
    effective.profiles.0.insert("worker".to_owned(), shadow);
    assert_eq!(
        definition_failure(&launch, &machine, Some(&effective)),
        None
    );
}

#[test]
fn exec_preparation_reads_the_room_only_for_a_resume_or_a_child() {
    let machine = crate::config::MachineConfig::default();
    let reads = std::cell::Cell::new(0);
    let unreadable = || {
        reads.set(reads.get() + 1);
        Err("snapshot unreadable".to_owned())
    };

    let fresh = host_request("codex", action_with_args("launch", Vec::new()));
    let prepared = prepare(&fresh, None, &machine, None, &|| {
        panic!("a fresh launch reads no room")
    });
    let (plan, isolation) = prepared.outcome.expect("a fresh launch compiles");
    assert_eq!(isolation, Isolation::Host);
    assert!(
        plan.warnings
            .iter()
            .all(|warning| !matches!(warning, LaunchPlanWarning::ParentUnitUnread { .. }))
    );

    let resume = host_request("codex", action_with_args("resume", Vec::new()));
    let prepared = prepare(&resume, None, &machine, None, &unreadable);
    assert!(
        prepared.outcome.is_ok(),
        "an unread room never fails a resume"
    );
    assert_eq!(reads.get(), 1);

    let mut child = host_request("codex", action_with_args("launch", Vec::new()));
    child.identity.name = Some("fox".to_owned());
    child.identity.params.parent_agent_id = Some("parent-session".into());
    let prepared = prepare(&child, None, &machine, None, &unreadable);
    let (plan, _) = prepared.outcome.expect("a child compiles without its room");
    assert_eq!(reads.get(), 2);
    assert!(plan.warnings.iter().any(|warning| matches!(
        warning,
        LaunchPlanWarning::ParentUnitUnread { error, .. } if error == "snapshot unreadable"
    )));
}

#[test]
fn exec_preparation_names_a_missing_launch_prompt() {
    let missing = tempfile::tempdir().unwrap().path().join("task.md");
    let launch = host_request("codex", action_with_args("launch", Vec::new()));
    let err = prepare(
        &launch,
        Some(&missing),
        &crate::config::MachineConfig::default(),
        None,
        &|| panic!("a fresh launch reads no room"),
    )
    .outcome
    .err()
    .expect("a missing prompt artifact fails the launch");
    assert_eq!(err.to_string(), "materializing launch prompt");
    assert!(matches!(
        std::error::Error::source(&err)
            .and_then(|source| source.downcast_ref::<launch::ExecWireErr>()),
        Some(launch::ExecWireErr::PromptRead { path, .. }) if *path == missing
    ));
}

#[test]
fn exec_preparation_returns_model_warnings_when_compile_fails() {
    let machine: crate::config::MachineConfig =
        toml::from_str("[models.claude]\nsol = 'claude-sol-1'").unwrap();
    let mut resume = host_request("claude", action_with_args("resume", Vec::new()));
    resume.identity.params.model = Some("sol".into());
    resume.system_prompt_file = Some(crate::config::PromptSource::File(
        tempfile::tempdir().unwrap().path().join("missing.md"),
    ));
    let prepared = prepare(&resume, None, &machine, None, &|| Ok(Vec::new()));
    assert!(
        prepared.outcome.is_err(),
        "the missing prompt fails compile"
    );
    assert!(
        prepared
            .model_warnings
            .iter()
            .any(|warning| warning.contains("has no recorded session model")),
        "{:?}",
        prepared.model_warnings
    );
}
