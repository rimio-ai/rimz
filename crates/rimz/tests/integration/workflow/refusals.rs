use super::*;

#[test]
fn validate_refuses_an_unknown_field_at_its_dotted_path_in_text_and_json() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}future=true\n"), PENDING);
    let output = command(&env, &["workflow", "validate", "review"]);
    assert!(
        !output.status.success(),
        "an unknown field refuses: {output:?}"
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("script.pr.future") && text.contains("unknown field"),
        "{text}"
    );
    assert!(!text.contains("ignored"), "{text}");
    let output = command(&env, &["workflow", "validate", "review", "--json"]);
    assert!(!output.status.success(), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|issue| issue["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["script.pr.future"]
    );
    assert_eq!(value["warnings"], json!([]));
}

#[test]
fn run_refuses_an_unknown_field_and_starts_no_run() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}future=true\n"), PENDING);
    let output = command(&env, &["workflow", "run", "review"]);
    assert!(
        !output.status.success(),
        "an unknown field refuses a run: {output:?}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("script.pr.future") && stderr.contains("unknown field"),
        "{stderr}"
    );
    assert!(output.stdout.is_empty(), "no receipt for a refused run");
    let listed: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    assert_eq!(listed, json!([]));
}

fn changed_sealed_spec() -> (Env, String, PathBuf) {
    let text = format!(
        "{HEADER}{}",
        WAIT.replace("[script.pr]", "[script.pr]\nwake=['review.ready']")
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
fn reread_of_an_invalid_sealed_spec_refuses_at_its_dotted_path_and_keeps_the_run() {
    let (env, id, home) = changed_sealed_spec();
    fs::write(
        env.rimz_home().join("workflows/review/checks/pr.sh"),
        evaluation_script("done"),
    )
    .unwrap();
    let output = command(&env, &["workflow", "advance", &id]);
    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("script.pr.future") && stderr.contains("unknown field"),
        "{stderr}"
    );
    let value = show(&env, &id);
    assert_eq!(value["record"]["state"], "active", "{value}");
    assert_eq!(
        events(&value, "evaluation").last().unwrap()["outcome"],
        "pending",
        "the refused reread runs no check: {value}"
    );
    assert!(value.get("warnings").is_none(), "{value}");
    let output = command(&env, &["workflow", "show", &id]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("script.pr.future"),
        "the text view names the refusal"
    );
    fs::write(home.join("workflow.toml"), "not a valid spec").unwrap();
    let broken = show(&env, &id);
    assert!(broken["record"].is_object() && broken["journal"].is_array());
}

#[test]
fn clean_runs_carry_no_warning_surfaces() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"].is_object());
    assert!(value.get("warnings").is_none(), "{value}");
    let listed: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    assert!(listed[0].get("warnings").is_none());
    let text = success(command(&env, &["workflow", "list"]));
    assert!(!text.contains("ignored"), "{text}");
    success(command(&env, &["workflow", "cancel", &id]));
}
