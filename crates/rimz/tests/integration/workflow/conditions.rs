use super::*;

const CONDITION: &str = "[checks.pr]\nelse='pending'\ntimeout='1h'\non={ ready='@done', pending='@wait' }\n[[checks.pr.cases]]\nwhen=['team.stage=Done']\noutcome='ready'\nfor='2m'\nkey=7\n";

fn board(env: &Env, stage: &str) {
    fs::create_dir_all(env.project_root.join(".rimz/team")).unwrap();
    fs::write(
        env.project_root.join(".rimz/team/blackboard.md"),
        format!("# Board\n\nStage: {stage}\n"),
    )
    .unwrap();
}

fn update_record(env: &Env, value: &Value, edit: impl FnOnce(&mut Value)) {
    let mut record = value["record"].clone();
    edit(&mut record);
    let state =
        rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
    let paths = state.workflow(&record["id"].as_str().unwrap().parse().unwrap());
    fs::write(paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
}

#[test]
fn condition_holds_merge_survive_wakes_and_restart_after_another_answer() {
    let (env, _) = fixture(&format!("{HEADER}{CONDITION}"), "");
    board(&env, "Done");
    let id = start(&env);
    let first = until(&env, &id, |v| v["record"]["wait"]["outcome"] == "hold");
    assert_eq!(first["record"]["wait"]["case"], 0);
    assert_eq!(first["record"]["wait"]["detail"], "ready");
    assert_eq!(first["record"]["key"], Value::Null);
    assert_eq!(first["record"]["input"], Value::Null);
    let began: jiff::Timestamp = first["record"]["wait"]["first"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let due: jiff::Timestamp = first["record"]["recheck_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(due.duration_since(began).as_secs(), 120);
    let text = success(command(&env, &["workflow", "show", &id]));
    assert!(
        text.contains("holding ready ") && text.contains("/2m"),
        "{text}"
    );
    success(command(&env, &["events", "emit", "condition.sample"]));
    let woke = until(&env, &id, |v| {
        v["record"]["wait"]["count"].as_u64().unwrap_or(0) >= 2
    });
    assert_eq!(
        woke["record"]["wait"]["first"],
        first["record"]["wait"]["first"]
    );
    assert_eq!(woke["record"]["recheck_at"], first["record"]["recheck_at"]);

    board(&env, "Other");
    success(command(&env, &["workflow", "advance", &id]));
    let other = show(&env, &id);
    assert_eq!(other["record"]["wait"]["outcome"], "pending");
    assert_eq!(events(&other, "wait")[0]["outcome"], "hold");
    board(&env, "Done");
    success(command(&env, &["workflow", "advance", &id]));
    let restarted = show(&env, &id);
    assert_eq!(restarted["record"]["wait"]["outcome"], "hold");
    assert_ne!(
        restarted["record"]["wait"]["first"],
        first["record"]["wait"]["first"]
    );
    update_record(&env, &restarted, |record| {
        record["wait"]["first"] =
            json!((jiff::Timestamp::now() - jiff::SignedDuration::from_secs(121)).to_string());
    });
    success(command(&env, &["workflow", "advance", &id]));
    let done = settled(&env, &id, "done");
    assert_eq!(
        done["record"]["input"],
        json!({"outcome":"ready", "case":0, "key":7, "readings":{"team.stage":"Done"}})
    );
    assert_eq!(
        events(&done, "evaluation").last().unwrap()["origin"],
        "check"
    );
}

fn forge(env: &Env, at: jiff::Timestamp, ok: bool) {
    let runtime = env.runtime_paths();
    fs::create_dir_all(runtime.lane_path("pr-state.json").parent().unwrap()).unwrap();
    fs::write(
        runtime.lane_path("pr-state.json"),
        serde_json::to_vec(&json!({
            "branch_ci":{},
            "path_repos":{(env.project_root.to_string_lossy().as_ref()):"repo"},
            "repos":{"repo":{"refreshed_at_ms":at.as_millisecond(), "ok":ok}},
            "states":{(env.project_root.to_string_lossy().as_ref()):{"state":"merged"}}
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn condition_unknown_waits_for_a_successful_post_entry_forge_probe() {
    let spec = format!(
        "{HEADER}[checks.pr]\nelse='pending'\ntimeout='1h'\non={{ merged='@done', pending='@wait' }}\n[[checks.pr.cases]]\nwhen=['pr=merged']\noutcome='merged'\n"
    );
    let (env, _) = fixture(&spec, "");
    forge(
        &env,
        jiff::Timestamp::now() - jiff::SignedDuration::from_secs(1),
        true,
    );
    let id = start(&env);
    let unread = until(&env, &id, |v| v["record"]["wait"]["outcome"] == "unknown");
    assert_eq!(unread["record"]["input"]["readings"], json!({"pr":null}));
    assert_eq!(
        unread["record"]["input"]["reason"],
        "pr (probed before this check was entered)"
    );
    let text = success(command(&env, &["workflow", "show", &id]));
    assert!(
        text.contains("unknown: pr (probed before this check was entered)"),
        "{text}"
    );
    forge(&env, jiff::Timestamp::now(), false);
    success(command(&env, &["workflow", "advance", &id]));
    let failed = show(&env, &id);
    assert_eq!(failed["record"]["wait"]["outcome"], "unknown");
    let reason = failed["record"]["input"]["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("pr (forge lookup failing, last tried "),
        "{reason}"
    );
    forge(&env, jiff::Timestamp::now(), true);
    success(command(&env, &["events", "emit", "condition.fresh"]));
    let done = settled(&env, &id, "done");
    assert_eq!(
        done["record"]["input"],
        json!({"outcome":"merged", "case":0, "readings":{"pr":"merged"}})
    );
    assert_eq!(
        events(&done, "evaluation").last().unwrap()["digests"],
        json!({})
    );
}

#[test]
fn condition_stage_admission_uses_the_run_project_and_validation_warns_without_one() {
    let spec = format!(
        "{HEADER}[checks.pr]\nelse='pending'\ntimeout='1h'\non={{ ready='@done', pending='@wait' }}\n[[checks.pr.cases]]\nwhen=['team.stage=Imaginary']\noutcome='ready'\n"
    );
    let (env, _) = fixture(&spec, "");
    let output = command(&env, &["workflow", "run", "review"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no team defines stage Imaginary"));
    let output = env
        .rimz()
        .current_dir(&env.home_root)
        .args(["workflow", "validate", "--json"])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["errors"], json!([]));
    assert!(report["warnings"].as_array().unwrap().iter().any(|w| {
        w["message"]
            .as_str()
            .unwrap()
            .contains("stage names were not checked")
    }));
}
