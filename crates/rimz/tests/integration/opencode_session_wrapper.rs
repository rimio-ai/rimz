use super::*;
use rimz::store::run::RunRecord;

fn wrapper_request(env: &Env, model: Option<&str>, subagent: bool) -> (ExecRequest, RunRecord) {
    env.record(&env.project_root);
    write_startup_relaunch_config(env, "isolation = \"host\"\nstartup-relaunch-wait = \"0s\"");
    let mut request = fresh_exec("opencode", subagent.then_some("a task; $(not a command)"));
    request.close_pane_on_exit = true;
    request.subagent = subagent;
    request.identity.name = Some("model-worker".into());
    request.identity.launch_id = Some("launch_opencode_model".into());
    request.identity.params.model = model.map(str::to_owned);
    let workspace = env.resolve_workspace(&env.project_root);
    let mut run = RunRecord::new(
        workspace.workspace_id.clone(),
        request.kind.clone(),
        rimz::agents::PermissionMode::Auto,
        "task".into(),
        env.project_root.clone(),
    );
    run.subagent = subagent;
    run.agent_name = request.identity.name.clone();
    run.pane_id = Some(rimz::ids::PaneId::parse("tmux:%9").unwrap());
    rimz::harness::run::create(&env.state_path_for(&env.project_root), &run).unwrap();
    request.run_id = Some(run.run_id.clone());
    env.store()
        .append_event(&EventEnvelope::agent_launched(
            workspace.workspace_id,
            workspace.session_name,
            &request.kind,
            AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "launch_opencode_model".into(),
                launch_id: None,
                agent_name: "model-worker".into(),
                agent_name_explicit: false,
                launch: request.identity.params.clone(),
                state: AgentLaunchState::Starting,
                run_id: request.run_id.clone(),
                pane_id: run.pane_id.clone(),
                runtime_owner: None,
                worktree_path: Some(env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    (request, run)
}

fn wrapper_command(env: &Env, request: &ExecRequest, origin: &str) -> std::process::Command {
    let dir = fake_opencode(env);
    crate::common::write_path_shim(
        &dir,
        "tmux",
        r#"
for arg in "$@"; do
  if [ "$arg" = -V ]; then echo 'tmux 3.5'; exit 0; fi
  if [ "$arg" = capture-pane ]; then cat "$CHECK_LOG/pane-tail"; exit 0; fi
done
exit 0
"#,
    );
    crate::common::write_path_shim(
        &dir,
        "pane",
        r#"exec "$CHECK_RIMZ" "$@" 2>"$CHECK_LOG/pane-tail""#,
    );
    let shell = write_fake_login_shell(env, "opencode-shell", &[]);
    let mut command = env.rimz_at(&dir.join("pane"));
    command
        .args(exec_args(env, request))
        .arg("--tmux")
        .env("CHECK_RIMZ", env.rimz_bin())
        .env("CHECK_LOG", &env.home_root)
        .env("CHECK_URL", origin)
        .env("SHELL", shell)
        .env("PATH", path_with_front(&dir))
        .env("TMUX_PANE", "%9");
    command
}

fn pane_tail(env: &Env) -> String {
    std::fs::read_to_string(env.home_root.join("pane-tail")).unwrap()
}

#[test]
fn wrapper_model_launches_create_before_exec_and_model_less_launches_stay_bare() {
    for (model, child) in [
        (Some("deepseek-v4.1-flash"), false),
        (Some("deepseek-v4.1-flash"), true),
        (None, false),
    ] {
        let env = Env::new();
        let (origin, server) = crate::oauth_usage::serve_http_routes(
            vec![
                ("GET /api/model?", 200, CATALOG.into()),
                ("POST /api/session ", 204, String::new()),
            ],
            if model.is_some() { 2 } else { 0 },
        );
        let (request, run) = wrapper_request(&env, model, child);
        let output = wrapper_command(&env, &request, &origin)
            .bounded_output()
            .unwrap();
        assert!(output.status.success(), "{}", pane_tail(&env));
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), if model.is_some() { 2 } else { 0 });
        let mut expected = Vec::new();
        if model.is_some() {
            let helper_env = std::fs::read_to_string(env.home_root.join("helper-env")).unwrap();
            let password = helper_env
                .lines()
                .find_map(|line| line.strip_prefix("OPENCODE_PASSWORD="))
                .unwrap();
            let auth = format!(
                "authorization: Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
            );
            assert!(
                requests
                    .iter()
                    .all(|request| request.lines().any(|line| line.eq_ignore_ascii_case(&auth)))
            );
            let (target, _) = request_parts(&requests[0]);
            let url = url::Url::parse(&format!("{origin}{target}")).unwrap();
            assert_eq!(
                url.query_pairs().collect::<Vec<_>>(),
                [(
                    "location[directory]".into(),
                    env.project_root.to_str().unwrap().into()
                )]
            );
            let (_, body) = request_parts(&requests[1]);
            assert_eq!(
                body["location"]["directory"],
                env.project_root.to_str().unwrap()
            );
            assert_eq!(
                body["model"],
                json!({"providerID":"opencode-go", "id":"deepseek-v4.1-flash"})
            );
            let id = body["id"].as_str().unwrap();
            assert!(id.starts_with("ses_"));
            assert_eq!(id.len(), 30);
            expected.extend(["--session".to_owned(), id.to_owned()]);
        } else {
            assert!(!env.home_root.join("serve-pids").exists());
        }
        expected.push("--standalone".into());
        if child {
            expected.extend(["--prompt".into(), "a task; $(not a command)".into()]);
        }
        assert_eq!(final_args(&env.home_root), expected);
        assert!(!env.home_root.join("helper-alive").exists());
        let run =
            rimz::harness::run::load(&env.state_path_for(&env.project_root), &run.run_id).unwrap();
        let pid: u32 = std::fs::read_to_string(env.home_root.join("final-pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            run.provider_pid,
            Some(pid),
            "the step must exec at the wrapper's provider PID"
        );
    }
}

#[test]
fn explain_shows_the_model_step_without_changing_the_provider_command() {
    let env = Env::new();
    env.record(&env.project_root);
    write_startup_relaunch_config(&env, "isolation = \"host\"");
    crate::common::write_definition(
        &env,
        "agents",
        "model-worker",
        "description: Worker\nagent: opencode\nmodel: opencode-go/deepseek-v4.1-flash",
        "",
    );
    let dir = fake_opencode(&env);
    let output = env
        .rimz()
        .args(["agents", "explain", "model-worker", "--json"])
        .env("PATH", path_with_front(&dir))
        .env("SHELL", write_fake_login_shell(&env, "opencode-shell", &[]))
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["program"], "opencode");
    assert_eq!(report["provider_argv"], json!(["opencode", "--standalone"]));
    let argv: Vec<_> = report["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect();
    assert!(
        argv.windows(6).any(|args| args
            == [
                "agents",
                "session-step",
                "opencode",
                "--model",
                "opencode-go/deepseek-v4.1-flash",
                "--"
            ]),
        "{argv:?}"
    );
    assert!(
        !env.home_root.join("serve-pids").exists(),
        "explain must not execute the step"
    );
}
