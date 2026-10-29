use super::*;

#[test]
fn session_steps_follow_launch_and_explicit_resume_models_only() {
    let root = tempfile::tempdir().unwrap();
    for (label, kind, action, model, override_model, prompt, expected) in [
        (
            "fresh",
            "opencode",
            "launch",
            Some("provider/model"),
            false,
            None,
            true,
        ),
        ("bare", "opencode", "launch", None, false, None, false),
        ("empty", "opencode", "launch", Some(""), false, None, false),
        (
            "child",
            "opencode",
            "launch",
            Some("provider/model"),
            false,
            Some("task; text"),
            true,
        ),
        (
            "resume",
            "opencode",
            "resume",
            Some("provider/model"),
            false,
            None,
            false,
        ),
        (
            "override",
            "opencode",
            "resume",
            Some("provider/model"),
            true,
            None,
            true,
        ),
        (
            "empty override",
            "opencode",
            "resume",
            None,
            true,
            None,
            false,
        ),
        (
            "restart",
            "opencode",
            "resume",
            Some("provider/model"),
            false,
            None,
            false,
        ),
        (
            "other adapter",
            "claude",
            "launch",
            Some("provider/model"),
            false,
            None,
            false,
        ),
    ] {
        let mut invocation = request(kind, action_with_args(action, vec![]));
        invocation.identity.params.model = model.map(str::to_owned);
        invocation.identity.params.effort = Some("high".into());
        invocation.identity.resume_model_override = override_model;
        if let ExecAction::Launch { prompt: text, .. } = &mut invocation.action {
            *text = prompt.map(str::to_owned);
        }
        invocation.subagent = prompt.is_some();
        let plan = compile_prompt_request(root.path(), &invocation, None, None);
        let process = plan.process();
        let step = process
            .argv
            .windows(3)
            .position(|args| args == ["agents", "session-step", "opencode"]);
        assert_eq!(step.is_some(), expected, "{label}: {:?}", process.argv);
        assert_eq!(process.provider_program, kind);
        assert_eq!(process.provider_argv[0], kind);
        let (validation, _) = launch::compile_managed_agent_process(
            root.path(),
            &invocation,
            root.path(),
            &crate::agents::ManagedLaunchState::PendingResolution,
            None,
        )
        .unwrap();
        assert!(!validation.argv.iter().any(|arg| arg == "session-step"));
        if kind != "opencode" {
            continue;
        }
        assert_eq!(validation.provider_argv, process.provider_argv);
        if !expected {
            assert_eq!(
                process.argv,
                process.login_shell_argv(&validation.provider_argv),
                "{label}"
            );
            continue;
        }
        let mut expected = vec![
            "/bin/rimz",
            "agents",
            "session-step",
            "opencode",
            "--model",
            "provider/model",
            "--effort",
            "high",
        ];
        if action == "resume" {
            expected.extend(["--session", "session"]);
        }
        expected.push("--");
        let mut expected = expected.into_iter().map(str::to_owned).collect::<Vec<_>>();
        expected.extend(process.provider_argv.iter().cloned());
        assert_eq!(&process.argv[step.unwrap() - 1..], expected);
        let mut pinned = process.clone();
        pinned.pin_env(BTreeMap::from([(
            "STEP_PIN".into(),
            crate::sandbox::EnvPin::Set("kept".into()),
        )]));
        assert!(pinned.argv.ends_with(&expected));
        assert!(!process.provider_argv.contains(&"--model".into()));
        if let Some(prompt) = prompt {
            assert!(
                process
                    .provider_argv
                    .windows(2)
                    .any(|args| args == ["--prompt", prompt])
            );
        }
    }
}

#[test]
fn plain_opencode_resume_clears_the_model_but_keeps_its_launch_record() {
    let machine = crate::config::MachineConfig::default();
    for explicit in [false, true] {
        let mut invocation = host_request("opencode", action_with_args("resume", vec![]));
        invocation.identity.params.model = Some("provider/model".into());
        invocation.identity.params.effort = Some("high".into());
        invocation.identity.params.record = Some(Box::new(crate::agents::LaunchRecord {
            model: Some("provider/model".into()),
            effort: Some("high".into()),
            ..Default::default()
        }));
        invocation.identity.resume_model_override = explicit;
        let prepared = prepare(&invocation, None, &machine, None, &|| Ok(Vec::new()));
        let (plan, _) = prepared.outcome.unwrap();
        assert_eq!(
            plan.request
                .identity
                .params
                .record
                .as_ref()
                .unwrap()
                .effort
                .as_deref(),
            Some("high")
        );
        assert!(prepared.model_warnings.is_empty());
        assert_eq!(plan.request.identity.params.model.is_some(), explicit);
        assert_eq!(
            plan.request
                .identity
                .params
                .record
                .as_ref()
                .unwrap()
                .model
                .as_deref(),
            Some("provider/model")
        );
        assert_eq!(
            plan.process().argv.iter().any(|arg| arg == "session-step"),
            explicit
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn the_step_is_inside_the_login_shell_scoped_view_and_hibernation_scope() {
    let root = tempfile::tempdir().unwrap();
    let id = crate::WorkspaceId::from_project_root(root.path());
    let runtime = RuntimePaths::under(id.clone(), root.path()).unwrap();
    let state = StatePaths::under(id, root.path()).unwrap();
    let machine = crate::config::MachineConfig::default();
    let mut invocation = request("opencode", action_with_args("launch", vec![]));
    invocation.identity.params.model = Some("provider/model".into());
    for isolation in [Isolation::Host, Isolation::Scoped] {
        for hibernate in [false, true] {
            let plan = compile(LaunchPlanInputs {
                request: &invocation,
                cwd: root.path(),
                project_root: root.path(),
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
                agent_shell: Some(Path::new("/bin/sh")),
                hibernate,
                recorded_system_prompt_digest: None,
            })
            .unwrap();
            let argv = &plan.process().argv;
            let step = argv.iter().position(|arg| arg == "session-step");
            assert!(
                step.is_some(),
                "{isolation:?} hibernate={hibernate}: {argv:?}"
            );
            let step = step.unwrap();
            let shell = argv.iter().position(|arg| arg == "/bin/sh").unwrap();
            let assignment = argv
                .iter()
                .position(|arg| arg == "RIMZ_AGENT_MODEL=provider/model")
                .unwrap();
            assert!(shell < assignment && assignment < step);
            assert_eq!(argv[0] == "systemd-run", hibernate);
            if isolation.is_scoped() {
                assert!(argv.iter().position(|arg| arg == "/usr/bin/bwrap").unwrap() < shell);
            }
            assert!(argv.ends_with(&["--".into(), "opencode".into(), "--standalone".into()]));
        }
    }
}
