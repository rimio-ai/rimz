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
