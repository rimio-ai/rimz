use super::*;

#[test]
fn finalized_stages_keep_the_step_and_provider_identity_across_rebuilds() {
    let root = tempfile::tempdir().unwrap();
    let runtime = runtime(root.path());
    let mut invocation = request(
        "opencode",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    invocation.identity.params.model = Some("provider/model".into());
    invocation.identity.params.effort = Some("high".into());
    let AgentProcessStage::Ready(process) = compile_agent_process_stage_with_extra_env(
        root.path(),
        &invocation,
        root.path(),
        Path::new("/bin/rimz"),
        &runtime,
        &BTreeMap::new(),
        &LaunchReminders::default(),
    )
    .unwrap() else {
        panic!("unbound process is ready")
    };
    let binding = provider_binding("owner");
    invocation.provider_account = ProviderAccountState::Finalized {
        binding: binding.clone(),
    };
    // Finalization's composer is generic even though only Qwen binds accounts today.
    let AgentProcessStage::Ready(mut process) = finalize_agent_process_stage(
        process,
        &invocation,
        Some(&crate::agents::ManagedLaunchState::Bound(binding)),
        Path::new("/bin/rimz"),
        &runtime,
    )
    .unwrap() else {
        panic!("finalized process is ready")
    };
    let mut expected = argv(&[
        "/bin/rimz",
        "agents",
        "session-step",
        "opencode",
        "--model",
        "provider/model",
        "--effort",
        "high",
        "--",
        "opencode",
        "--standalone",
    ]);
    assert_eq!(process.argv, expected);
    assert_eq!(process.provider_program, "opencode");
    assert_eq!(process.provider_argv, argv(&["opencode", "--standalone"]));
    process.pin_env(BTreeMap::from([(
        "AFTER_RC".into(),
        crate::sandbox::EnvPin::Set("kept".into()),
    )]));
    assert_eq!(
        process.argv, expected,
        "finalized stages must not re-enter a login shell"
    );
    process.provider_argv.push("--later-provider-arg".into());
    process.rebuild_argv();
    expected.push("--later-provider-arg".into());
    assert_eq!(process.argv, expected);
}

#[test]
fn session_step_argv_passes_run_id_only_when_the_launch_has_a_run() {
    let run_id = crate::ids::RunId::new();
    let mut without_run = request(
        "opencode",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    without_run.identity.params.model = Some("provider/model".into());
    let plain_argv = session_step_argv(&without_run, Path::new("/bin/rimz"));

    let mut with_run = without_run;
    with_run.run_id = Some(run_id.clone());
    let run_argv = session_step_argv(&with_run, Path::new("/bin/rimz"));

    assert_eq!(
        (
            plain_argv.iter().any(|arg| arg == "--run-id"),
            run_argv
                .windows(3)
                .any(|args| args == ["--run-id", run_id.as_str(), "--"]),
        ),
        (false, true),
        "plain={plain_argv:?}\nwith_run={run_argv:?}"
    );
}
