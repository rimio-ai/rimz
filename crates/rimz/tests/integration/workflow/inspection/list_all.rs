use super::*;

/// A finished home holding `record` as JSON, whatever version wrote it.
fn finish(env: &Env, record: Value) -> String {
    let id = rimz::ids::WorkflowRunId::new();
    let paths = env.state_path_for(&env.project_root).finished_workflow(&id);
    fs::create_dir_all(&paths.home).unwrap();
    let mut record = record;
    record["id"] = json!(id);
    fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
    id.to_string()
}

fn ended(env: &Env, version: u64, origin: &str, once: bool, at: &str) -> Value {
    json!({
        "version":version, "workflow":"review", "origin":origin, "once":once,
        "project_root":env.project_root, "scope":{"kind":"project", "path":env.project_root},
        "created_at":at, "entered_at":at, "terminal_at":at, "state":"failed", "node":"gate",
    })
}

#[test]
fn all_adds_finished_runs_started_by_name_whatever_version_wrote_them() {
    let env = setup();
    let mut live = seed(
        &env,
        &env.project_root,
        &env.project_root.join("lane"),
        "active",
    );
    // An age that reads `1h` for half an hour holds the live text still across every command.
    live.entered_at = jiff::Timestamp::now() - jiff::SignedDuration::from_mins(90);
    save(&env, &live);
    let plain = success(command(&env, &["workflow", "list"]));
    let plain_json = success(command(&env, &["workflow", "list", "--json"]));
    assert_eq!(
        success(command(&env, &["workflow", "list", "--all"])),
        plain,
        "no finished runs add no section"
    );
    // A terminal record still in the live directory ended too.
    let owed = seed(
        &env,
        &env.project_root,
        &env.project_root.join("owed"),
        "done",
    );
    let old = finish(
        &env,
        ended(&env, 3, "deployed", false, "2026-01-02T00:00:00Z"),
    );
    let mut tried = ended(&env, 3, "deployed", true, "2026-01-03T00:00:00Z");
    tried["state"] = json!("done");
    tried["outcome"] = json!({"ending":{"kind":"tried"}});
    let tried = finish(&env, tried);
    let inline = finish(
        &env,
        ended(&env, 3, "inline", false, "2026-01-04T00:00:00Z"),
    );
    let paths = env
        .state_path_for(&env.project_root)
        .finished_workflow(&rimz::ids::WorkflowRunId::new());
    fs::create_dir_all(&paths.home).unwrap();
    fs::write(&paths.record, "not-json").unwrap();

    assert_eq!(success(command(&env, &["workflow", "list"])), plain);
    assert_eq!(
        success(command(&env, &["workflow", "list", "--json"])),
        plain_json
    );
    let text = success(command(&env, &["workflow", "list", "--all"]));
    assert!(text.starts_with(&plain), "{text}");
    let section = &text[plain.len()..];
    for id in [owed.id.as_str(), &old, &tried] {
        assert_eq!(section.matches(id).count(), 1, "{id}: {text}");
    }
    assert!(!text.contains(&inline), "an inline run stays out: {text}");
    let row = |id: &str| section.lines().find(|line| line.contains(id)).unwrap();
    assert!(
        row(&tried).contains(" tried ") && !row(&tried).contains("once"),
        "a try that ended done says so once: {text}"
    );
    assert!(
        row(&old).contains("review#main") && row(&old).contains("failed"),
        "{text}"
    );
    let order = [owed.id.as_str(), &tried, &old].map(|id| section.find(id).unwrap());
    assert!(order.is_sorted(), "newest ending first: {text}");

    let text = success(command(&env, &["workflow", "list", "--all", "-n", "1"]));
    assert!(
        text.ends_with("  2 more finished runs; rimz workflow list --all -n 3 shows them\n"),
        "{text}"
    );
    let rows: Value = serde_json::from_str(&success(command(
        &env,
        &["workflow", "list", "--all", "--json", "-n", "2"],
    )))
    .unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 3, "{rows:#?}");
    assert_eq!(rows[0]["id"], json!(live.id));
    assert_eq!(rows[1]["id"], json!(owed.id));
    assert_eq!(rows[2]["id"], json!(tried));
    assert_eq!(rows[2]["once"], json!(true));
    assert_eq!(rows[2]["ended"], json!("2026-01-03T00:00:00Z"));
}

#[test]
fn a_limit_without_all_is_a_usage_error() {
    let env = setup();
    let output = command(&env, &["workflow", "list", "-n", "5"]);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
}
