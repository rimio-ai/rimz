use super::*;

fn models() -> Vec<Model> {
    serde_json::from_value(json!([
        {"providerID":"opencode-go", "id":"deepseek-v4.1-flash"},
        {"providerID":"one", "id":"gpt-5"},
        {"providerID":"two", "id":"gpt-5"},
        {"providerID":"one", "id":"nested/model"}
    ]))
    .unwrap()
}

#[test]
fn model_matching_is_exact_or_a_unique_bare_id() {
    for (name, provider, id) in [
        (
            "opencode-go/deepseek-v4.1-flash",
            "opencode-go",
            "deepseek-v4.1-flash",
        ),
        ("deepseek-v4.1-flash", "opencode-go", "deepseek-v4.1-flash"),
        ("one/nested/model", "one", "nested/model"),
    ] {
        let found = match_model(name, &models());
        assert!(found.is_ok(), "{name} must match: {found:?}");
        let found = found.unwrap();
        assert_eq!(
            (found.provider_id.as_str(), found.id.as_str()),
            (provider, id)
        );
    }
}

#[test]
fn model_refusals_name_the_cause_and_the_correction() {
    for (name, reason) in [
        (
            "gpt-5",
            "ambiguous model: one/gpt-5, two/gpt-5; name one as provider/model",
        ),
        (
            "deepseek-4-1-flash",
            "unknown model; did you mean opencode-go/deepseek-v4.1-flash?",
        ),
        (
            "opencode-go/deepseek-4-1-flash",
            "unknown model; did you mean opencode-go/deepseek-v4.1-flash?",
        ),
        (
            "nested/model",
            "unknown model; did you mean one/nested/model?",
        ),
        (
            "unrelated",
            "unknown model; run `opencode models` to list configured models",
        ),
        (
            "one/gpt-5#high",
            "unknown model; run `opencode models` to list configured models",
        ),
    ] {
        let refusal = match_model(name, &models())
            .unwrap_err()
            .refusal(name)
            .to_string();
        assert_eq!(
            refusal,
            format!("OpenCode model check refused {name:?}: {reason}")
        );
        assert!(!refusal.contains("verified on"));
        assert_eq!(refusal.contains("ambiguous"), name == "gpt-5");
    }
}

#[test]
fn http_refusals_quote_opencode_and_keep_the_version_note_for_the_model_list() {
    let note = format!(
        "; model selection is verified on OpenCode 2.1.4; {}",
        OpencodeAdapter.version_upgrade_fix().unwrap()
    );
    let response = |status: u16, body: &str| {
        ureq::http::Response::builder()
            .status(status)
            .body(ureq::Body::builder().data(body))
            .unwrap()
    };
    let gone = r#"{"message":"Session not found: ses_gone"}"#;
    for (err, reason) in [
        (
            status_error(
                "model list",
                response(404, r#"{"message":"Not Found"}"#),
                CheckErr::Helper,
            ),
            format!("model list returned HTTP 404 Not Found: Not Found{note}"),
        ),
        (
            status_error("model list", response(503, "{}"), CheckErr::Helper),
            format!("model list returned HTTP 503 Service Unavailable{note}"),
        ),
        (
            status_error("session model", response(404, gone), CheckErr::Answer),
            "session model returned HTTP 404 Not Found: Session not found: ses_gone".into(),
        ),
        (
            status_error("session model", response(500, "<html>"), CheckErr::Answer),
            format!("session model returned HTTP 500 Internal Server Error{note}"),
        ),
    ] {
        assert_eq!(
            err.refusal("m").to_string(),
            format!("OpenCode model check refused \"m\": {reason}")
        );
    }
}

#[test]
fn helper_environment_is_private_and_keeps_authored_config() {
    let inherited = BTreeMap::from([
        ("RIMZ".into(), "1".into()),
        ("RIMZ_WORKSPACE_ID".into(), "room".into()),
        ("RIMZ_LAUNCH_REMINDERS".into(), "reminder".into()),
        ("RIMZ_AGENT_MODEL".into(), "not-the-requested-model".into()),
        ("HOME".into(), "/private/home".into()),
        ("OPENCODE_CONFIG_CONTENT".into(), "{/* keep */\"theme\":\"dark\",\"policies\":[{\"action\":\"tool.use\",\"resource\":\"bash\",\"effect\":\"deny\"},],}".into()),
    ]);
    let env = helper_env(&inherited);
    assert!(
        env.keys()
            .all(|key| key != "RIMZ" && !key.starts_with("RIMZ_"))
    );
    assert_eq!(env.get("HOME"), inherited.get("HOME"));
    for key in [
        "OPENCODE_DISABLE_MODELS_FETCH",
        "OPENCODE_FILEWATCHER_DISABLE",
        "OPENCODE_DISABLE_FFF",
    ] {
        assert_eq!(env[key], "1");
    }
    assert!(!env["OPENCODE_PASSWORD"].is_empty());
    assert_ne!(
        env["OPENCODE_PASSWORD"],
        helper_env(&inherited)["OPENCODE_PASSWORD"]
    );
    let content: Value = serde_json::from_str(&env["OPENCODE_CONFIG_CONTENT"]).unwrap();
    assert_eq!(
        content,
        json!({"theme":"dark", "policies":[
            {"action":"tool.use", "resource":"bash", "effect":"deny"},
            {"action":"integration.use", "resource":"plugin:*", "effect":"deny"},
            {"action":"integration.use", "resource":"mcp:*", "effect":"deny"}
        ]})
    );
    for raw in ["broken", "[]", "{\"policies\":false}"] {
        let env = helper_env(&BTreeMap::from([(
            "OPENCODE_CONFIG_CONTENT".into(),
            raw.into(),
        )]));
        let content: Value = serde_json::from_str(&env["OPENCODE_CONFIG_CONTENT"]).unwrap();
        assert_eq!(
            content,
            json!({"policies":[
                {"action":"integration.use", "resource":"plugin:*", "effect":"deny"},
                {"action":"integration.use", "resource":"mcp:*", "effect":"deny"}
            ]})
        );
    }
    assert_eq!(inherited["RIMZ_LAUNCH_REMINDERS"], "reminder");
}

#[test]
fn session_ids_have_opencodes_descending_shape() {
    let first = mint_session_id();
    let later = mint_session_id();
    for id in [&first, &later] {
        assert!(id.starts_with("ses_"));
        assert_eq!(id.len(), 30);
        assert!(id[4..16].bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(id[16..].bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }
    assert!(later < first, "{first} then {later}");
}

#[cfg(unix)]
#[test]
fn deadline_kills_and_reaps_the_helper_group() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let program = root.path().join("opencode");
    let pid_file = root.path().join("pid");
    std::fs::write(
        &program,
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$HELPER_PID\"\nexec sleep 30\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let argv = vec![program.display().to_string(), "--standalone".into()];
    let env = BTreeMap::from([("HELPER_PID".into(), pid_file.display().to_string())]);
    let started = Instant::now();
    let refusal = run_with_timeout(
        &SessionStepRequest {
            session: None,
            model: "one/model",
            provider_argv: &argv,
            cwd: root.path(),
            env: &env,
        },
        Duration::from_millis(500),
    )
    .unwrap_err();
    assert!(
        refusal.to_string().contains("deadline expired"),
        "{refusal}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    let pid: i32 = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    assert_eq!(
        nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
    assert_eq!(
        nix::sys::wait::waitpid(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ECHILD)
    );
}
