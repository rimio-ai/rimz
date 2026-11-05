use super::*;

#[test]
fn validate_warns_about_ignored_fields_in_text_and_json() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}future=true\n"), PENDING);
    let output = command(&env, &["workflow", "validate", "review"]);
    assert!(
        output.status.success(),
        "an unknown optional field must only warn: {output:?}"
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("warning: review: checks.pr.future:")
            && text.contains("ignored")
            && text.contains("unknown field"),
        "{text}"
    );
    let value: Value = serde_json::from_str(&success(command(
        &env,
        &["workflow", "validate", "review", "--json"],
    )))
    .unwrap();
    assert_eq!(value["warnings"][0]["path"], "checks.pr.future");
    assert_eq!(value["errors"], json!([]));
}

#[test]
fn delayed_route_warnings_keep_the_target_in_validate_text_and_json() {
    let step = "[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nthen='pr'\nmax-visits=3\n";
    for (route, lifecycle, target) in [
        ("{to='@done',delay=42}", "until", "@done"),
        ("{to='@done',delay='soon'}", "until", "@done"),
        ("{to='@done',delay='0s'}", "until", "@done"),
        ("{to='@done',delay='30m'}", "turn", "@done"),
        ("{to='@wait',delay='30m'}", "until", "@wait"),
        ("{to='build',delay='30m'}", "until", "build"),
    ] {
        let text = format!(
            "{HEADER}{}{step}lifecycle='{lifecycle}'\n",
            WAIT.replace("done = '@done'", &format!("done = {route}"))
        );
        let (env, _) = fixture(&text, PENDING);
        let output = command(&env, &["workflow", "validate", "review"]);
        assert!(
            output.status.success(),
            "an optional delay must only warn: {output:?}"
        );
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            text.contains("warning: review: checks.pr.on.done.delay: ignored field:"),
            "{text}"
        );
        let report: Value = serde_json::from_str(&success(command(
            &env,
            &["workflow", "validate", "review", "--json"],
        )))
        .unwrap();
        assert_eq!(report["errors"], json!([]));
        assert!(
            report["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| { warning["path"] == "checks.pr.on.done.delay" }),
            "{report}"
        );
        let edge = report["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["node"] == "pr")
            .unwrap()["edges"]
            .as_array()
            .unwrap()
            .iter()
            .find(|edge| edge["outcome"] == "done")
            .unwrap();
        assert_eq!(edge["target"], target);
        assert!(edge.get("delay").is_none(), "{edge}");
    }
}

#[test]
fn delayed_routes_render_in_definition_node_and_validate_views() {
    let spec = format!(
        "{HEADER}{}[steps.build]\nagent='claude'\nprompt-file='prompt.md'\nlifecycle='until'\nthen='pr'\nmax-visits=3\n",
        WAIT.replace("done = '@done'", "done = {to='@done',delay='30m'}")
    );
    let (env, _) = fixture(&spec, PENDING);
    for (args, edge) in [
        (
            vec!["workflow", "validate", "review"],
            "done → @done after 30m",
        ),
        (vec!["workflow", "show", "review"], "done @done after 30m"),
        (
            vec!["workflow", "show", "review", "pr"],
            "done → @done after 30m",
        ),
    ] {
        let text = success(command(&env, &args));
        assert_eq!(text.matches(edge).count(), 1, "{args:?}: {text}");
        if args.len() == 4 {
            assert!(
                text.contains(
                    "Delays apply only while an until session is held; otherwise routes go at once."
                ),
                "{text}"
            );
        }
    }
    let report: Value = serde_json::from_str(&success(command(
        &env,
        &["workflow", "validate", "review", "--json"],
    )))
    .unwrap();
    let edges = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["node"] == "pr")
        .unwrap()["edges"]
        .as_array()
        .unwrap();
    assert_eq!(
        edges.iter().find(|edge| edge["outcome"] == "done").unwrap()["delay"],
        "30m"
    );
    assert!(
        edges
            .iter()
            .filter(|edge| edge["outcome"] != "done")
            .all(|edge| edge.get("delay").is_none())
    );
}

#[test]
fn run_warns_on_stderr_and_keeps_its_receipt_lines() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}future=true\n"), PENDING);
    let output = command(&env, &["workflow", "run", "review"]);
    assert!(
        output.status.success(),
        "ignored fields must not refuse a run: {output:?}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("warning: review: checks.pr.future:") && stderr.contains("ignored"),
        "{stderr}"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout.lines().collect::<Vec<_>>();
    assert!(lines[0].starts_with("wf_"), "{stdout}");
    let value = until(&env, lines[0], |v| v["record"]["wait"].is_object());
    assert_eq!(lines[1], value["dir"].as_str().unwrap());
    success(command(&env, &["workflow", "cancel", lines[0]]));
}

fn changed_sealed_spec() -> (Env, String, PathBuf) {
    let text = format!(
        "{HEADER}{}",
        WAIT.replace("[checks.pr]", "[checks.pr]\nwake=['review.ready']")
    );
    let (env, _) = fixture(&text, PENDING);
    let id = start(&env);
    until(&env, &id, |v| {
        v["record"]["wait"].is_object() && v["record"]["attempt"].is_null()
    });
    let paths = env
        .state_path_for(&env.project_root)
        .workflow(&id.parse().unwrap());
    fs::write(
        paths.home.join("workflow.toml"),
        text.replace("timeout = '1h'", "timeout = 'soon'\nfuture = true"),
    )
    .unwrap();
    (env, id, paths.home)
}

#[test]
fn reread_ignores_new_and_invalid_fields_without_readmission() {
    let (env, id, _home) = changed_sealed_spec();
    fs::write(
        env.rimz_home().join("workflows/review/checks/pr.sh"),
        evaluation_script("done"),
    )
    .unwrap();
    wakes::wake(&env, "review.ready", json!({"path":env.project_root}));
    let value = settled(&env, &id, "done");
    assert_eq!(value["warnings"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        events(&value, "evaluation").last().unwrap()["outcome"],
        "done"
    );
    let text = success(command(&env, &["workflow", "show", &id]));
    assert!(
        text.contains("checks.pr.future")
            && text.contains("checks.pr.timeout")
            && text.contains("ignored"),
        "{text}"
    );
}

#[test]
fn ignored_fields_mark_run_views_but_do_not_change_json_detail() {
    let (env, id, home) = changed_sealed_spec();
    let value = show(&env, &id);
    let mut paths = value["warnings"]
        .as_array()
        .map(|warnings| {
            warnings
                .iter()
                .map(|i| i["path"].as_str().unwrap())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    paths.sort_unstable();
    assert_eq!(paths, ["checks.pr.future", "checks.pr.timeout"]);
    for args in [
        vec!["workflow", "show", &id],
        vec!["workflow", "show", &id, "--history"],
    ] {
        let text = success(command(&env, &args));
        assert!(
            text.contains("checks.pr.future")
                && text.contains("checks.pr.timeout")
                && text.contains("ignored"),
            "{text}"
        );
        let warning = text.find("warning:").unwrap();
        if args.len() == 3 {
            assert!(text.rfind("│\n").unwrap() < warning, "{text}");
            assert!(warning < text.find("trail  ").unwrap(), "{text}");
        } else {
            assert!(
                text.lines().nth(2).unwrap().starts_with("warning:"),
                "{text}"
            );
        }
    }
    for args in [vec!["workflow", "list"], vec!["workflow", "show", "review"]] {
        let text = success(command(&env, &args));
        assert!(text.contains("2 ignored"), "{text}");
    }
    let listed: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    assert_eq!(listed[0]["warnings"], value["warnings"]);
    assert!(!listed[0]["detail"].as_str().unwrap().contains("ignored"));
    success(command(&env, &["workflow", "cancel", &id]));
    let text = success(command(&env, &["workflow", "show", &id]));
    assert!(
        text.contains("checks.pr.future") && text.contains("checks.pr.timeout"),
        "{text}"
    );
    fs::write(home.join("workflow.toml"), "not a valid spec").unwrap();
    let broken = show(&env, &id);
    assert!(broken["record"].is_object() && broken["journal"].is_array());
    assert!(broken.get("warnings").is_none());
}

#[test]
fn clean_runs_have_empty_show_warnings_and_no_list_mark() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"].is_object());
    assert_eq!(value["warnings"], json!([]));
    let listed: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    assert!(listed[0].get("warnings").is_none());
    let text = success(command(&env, &["workflow", "list"]));
    assert!(!text.contains("ignored"), "{text}");
    success(command(&env, &["workflow", "cancel", &id]));
}
