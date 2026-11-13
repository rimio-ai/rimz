use super::*;

/// The notifications command log and the diagnostics log of the fixture's room.
fn logs(env: &Env, state: &rimz::StatePaths) -> (PathBuf, PathBuf) {
    let notices = env.project_root.join("notices.log");
    fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[notifications]\ncommand = '''printf '%s|%s|%s\\n' \"$RIMZ_NOTIFY_KIND\" \
             \"$RIMZ_NOTIFY_TITLE\" \"$RIMZ_NOTIFY_BODY\" >> '{}' '''\n",
            notices.display()
        ),
    )
    .unwrap();
    let diag = rimz::diag::DiagSink::under(
        state.root.clone(),
        state.workspace_id.clone(),
        "rimz-test",
        None,
    );
    (notices, diag.log_path().unwrap())
}

fn lines_with(path: &Path, needle: &str) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(needle))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_pass_ends_stranded_runs_moves_ended_ones_and_tells_once() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let active = start(&env);
    until(&env, &active, |value| value["record"]["wait"].is_object());
    let state = env.state_path_for(&env.project_root);
    let (notices, diag_log) = logs(&env, &state);
    let base: Value =
        serde_json::from_slice(&fs::read(state.workflow(&active.parse().unwrap()).record).unwrap())
            .unwrap();
    let variant = |edit: &dyn Fn(&mut Value)| {
        let mut record = base.clone();
        edit(&mut record);
        serde_json::to_vec(&record).unwrap()
    };
    let newer = u64::from(rimz::store::workflow::VERSION) + 1;
    let older = newer - 2;
    let seeds: Vec<(&str, Option<Vec<u8>>)> = vec![
        (
            "older-active",
            Some(variant(&|r| r["version"] = json!(older))),
        ),
        (
            "older-parked",
            Some(variant(&|r| {
                r["version"] = json!(older);
                r["state"] = json!("parked");
                r["terminal_at"] = json!("2026-01-02T03:04:05Z");
                r["kept_member"] = json!("kept");
            })),
        ),
        (
            "older-done",
            Some(variant(&|r| {
                r["version"] = json!(older);
                r["state"] = json!("done");
            })),
        ),
        (
            "newer",
            Some(serde_json::to_vec(&json!({"version": newer, "body": [1, 2]})).unwrap()),
        ),
        (
            "missing-version",
            Some(variant(&|r| {
                r.as_object_mut().unwrap().remove("version");
            })),
        ),
        (
            "version-four",
            Some(variant(&|r| r["version"] = json!("four"))),
        ),
        (
            "undecodable",
            Some(variant(&|r| {
                r.as_object_mut().unwrap().remove("spec_digest");
            })),
        ),
        ("not-object", Some(b"corrupt".to_vec())),
        ("recordless", None),
    ];
    let mut ids = std::collections::BTreeMap::new();
    for (name, contents) in seeds {
        let id = rimz::ids::WorkflowRunId::new();
        let paths = state.workflow(&id);
        fs::create_dir_all(&paths.home).unwrap();
        if let Some(contents) = contents {
            let contents = match serde_json::from_slice::<Value>(&contents) {
                Ok(mut value) if value.get("id").is_some() => {
                    value["id"] = json!(id.as_str());
                    serde_json::to_vec(&value).unwrap()
                }
                _ => contents,
            };
            fs::write(&paths.record, contents).unwrap();
        }
        ids.insert(name, id);
    }
    let finished_bytes = |name: &str| fs::read(state.finished_workflow(&ids[name]).record).unwrap();
    let done_before = fs::read(state.workflow(&ids["older-done"]).record).unwrap();

    success(command(&env, &["workflow", "reconcile"]));

    let live: std::collections::BTreeSet<String> = fs::read_dir(
        state
            .workflow(&active.parse().unwrap())
            .home
            .parent()
            .unwrap(),
    )
    .unwrap()
    .map(|entry| entry.unwrap().file_name().into_string().unwrap())
    .collect();
    let expected: std::collections::BTreeSet<String> = [
        active.clone(),
        ids["newer"].to_string(),
        ids["recordless"].to_string(),
    ]
    .into();
    assert_eq!(live, expected, "{ids:#?}");
    for name in [
        "older-active",
        "older-parked",
        "missing-version",
        "version-four",
        "undecodable",
    ] {
        let record: Value = serde_json::from_slice(&finished_bytes(name)).unwrap();
        assert_eq!(
            (
                &record["state"],
                &record["outcome"]["ending"]["kind"],
                &record["stranded"],
                &record["outcome"]["acted"],
            ),
            (
                &json!("failed"),
                &json!("failed"),
                &json!(true),
                &json!(false)
            ),
            "{name}: {record:#}"
        );
        let reason = record["outcome"]["reason"].as_str().unwrap();
        assert!(
            reason.starts_with("this RimZ cannot continue the run: ")
                && reason.ends_with(&format!(
                    "; it reads only record version {}",
                    rimz::store::workflow::VERSION
                )),
            "{name}: {reason}"
        );
        assert!(record["terminal_at"].is_string(), "{name}: {record:#}");
        assert!(
            state.finished_workflow(&ids[name]).history().exists(),
            "{name}"
        );
    }
    let parked: Value = serde_json::from_slice(&finished_bytes("older-parked")).unwrap();
    assert_eq!(
        (
            &parked["terminal_at"],
            &parked["kept_member"],
            &parked["version"]
        ),
        (
            &json!("2026-01-02T03:04:05Z"),
            &json!("kept"),
            &json!(older)
        )
    );
    assert_eq!(finished_bytes("older-done"), done_before);
    let history =
        fs::read_to_string(state.finished_workflow(&ids["older-done"]).history()).unwrap();
    assert!(
        history.contains(&format!("record version {older}")),
        "{history}"
    );
    assert_eq!(finished_bytes("not-object"), b"corrupt");
    assert!(
        !state
            .finished_workflow(&ids["not-object"])
            .history()
            .exists()
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let notice = loop {
        let found = lines_with(&notices, "workflow_ended|");
        if !found.is_empty() {
            break found;
        }
        assert!(Instant::now() < deadline, "no workflow_ended notice");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(notice.len(), 1, "{notice:#?}");
    assert!(
        notice[0].starts_with("workflow_ended|RimZ: 6 workflow runs ended|6 workflow runs")
            && notice[0].contains(&format!(
                "2 written by an older RimZ (record version {older})"
            ))
            && notice[0].contains("1 whose record holds no JSON object")
            // No run held an agent, so the notice names none left running.
            && !notice[0].contains("agents"),
        "{}",
        notice[0]
    );
    assert_eq!(
        lines_with(&diag_log, "workflow_runs_ended").len(),
        1,
        "{}",
        fs::read_to_string(&diag_log).unwrap_or_default()
    );

    success(command(&env, &["workflow", "reconcile"]));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(lines_with(&notices, "workflow_ended|").len(), 1);
    assert_eq!(lines_with(&diag_log, "workflow_runs_ended").len(), 1);
    success(command(&env, &["workflow", "cancel", &active]));
}

#[test]
fn a_home_set_aside_is_announced_by_the_pass_that_moves_it() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let state = env.state_path_for(&env.project_root);
    let (notices, diag_log) = logs(&env, &state);
    let id = rimz::ids::WorkflowRunId::new();
    let live = state.workflow(&id);
    fs::create_dir_all(&live.home).unwrap();
    fs::write(&live.record, "corrupt").unwrap();
    // A home of that id at the destination makes every move fail.
    let blocker = state.finished_workflow(&id).home;
    fs::create_dir_all(&blocker).unwrap();
    for _ in 0..2 {
        success(command(&env, &["workflow", "reconcile"]));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(live.home.exists());
    assert!(lines_with(&notices, "workflow_ended|").is_empty());
    assert!(lines_with(&diag_log, "workflow_runs_ended").is_empty());

    fs::remove_dir(&blocker).unwrap();
    success(command(&env, &["workflow", "reconcile"]));
    assert!(!live.home.exists());
    let deadline = Instant::now() + Duration::from_secs(10);
    while lines_with(&notices, "workflow_ended|").is_empty() {
        assert!(Instant::now() < deadline, "no workflow_ended notice");
        std::thread::sleep(Duration::from_millis(10));
    }
    success(command(&env, &["workflow", "reconcile"]));
    std::thread::sleep(Duration::from_millis(200));
    let notice = lines_with(&notices, "workflow_ended|");
    assert_eq!(notice.len(), 1, "{notice:#?}");
    assert!(
        notice[0].contains("1 whose record holds no JSON object"),
        "{}",
        notice[0]
    );
    assert_eq!(lines_with(&diag_log, "workflow_runs_ended").len(), 1);
}
