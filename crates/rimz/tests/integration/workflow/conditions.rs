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
        text.contains("holding ready at pr") && text.contains("of 2m, then @done at"),
        "{text}"
    );
    let stage_board = env
        .project_root
        .join(".rimz/team")
        .canonicalize()
        .unwrap()
        .join("blackboard.md");
    wakes::wake(&env, "team.stage", json!({"board": stage_board}));
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

const CI_CONDITION: &str = "[checks.pr]\nelse='pending'\ntimeout='1h'\non={ ready='@done', pending='@wait' }\n[[checks.pr.cases]]\nwhen=['ci=passed']\noutcome='ready'\n";

fn open_forge(env: &Env, scope: &Path, pr_head: &str, ci_sha: &str) {
    let path = env.runtime_paths().lane_path("pr-state.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        serde_json::to_vec(&json!({
            "branch_ci":{},
            "path_repos":{(scope.to_string_lossy().as_ref()):"repo"},
            "repos":{"repo":{"refreshed_at_ms":jiff::Timestamp::now().as_millisecond(), "ok":true}},
            "states":{(scope.to_string_lossy().as_ref()):{
                "state":"open", "ci":"passing", "ci_sha":ci_sha, "open":{"head":pr_head}
            }}
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn condition_mergeability_waits_for_a_current_head_answer() {
    let spec = format!(
        "{HEADER}[checks.pr]\nelse='pending'\ntimeout='1h'\non={{ conflict='@done', pending='@wait' }}\n[[checks.pr.cases]]\nwhen=['pr.mergeable=conflicting']\noutcome='conflict'\n"
    );
    let (env, _) = fixture(&spec, "");
    let id = start(&env);
    let first = until(&env, &id, |value| {
        value["record"]["wait"]["outcome"] == "unknown" && value["record"]["attempt"].is_null()
    });
    let scope = Path::new(first["record"]["scope"]["path"].as_str().unwrap());
    let conflict = |settled_head| {
        open_forge(&env, scope, "head-b", "head-b");
        let path = env.runtime_paths().lane_path("pr-state.json");
        let mut cache: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        cache["states"][scope.to_string_lossy().as_ref()]["open"]["mergeability"] =
            json!({"state": "conflicting", "head": settled_head});
        fs::write(path, serde_json::to_vec(&cache).unwrap()).unwrap();
    };
    conflict("head-a");
    success(command(&env, &["workflow", "advance", &id]));
    let stale = show(&env, &id);
    conflict("head-b");
    success(command(&env, &["workflow", "advance", &id]));
    let current = show(&env, &id);
    assert_eq!(
        (&stale["record"]["input"], &current["record"]["input"]),
        (
            &json!({"outcome": "unknown", "readings": {"pr.mergeable": null},
                "reason": "pr.mergeable (not reported for this checkout's open PR)"}),
            &json!({"outcome": "conflict", "case": 0,
                "readings": {"pr.mergeable": "conflicting"}}),
        )
    );
    settled(&env, &id, "done");
}

#[test]
fn condition_ci_waits_for_local_commits_and_the_pr_head_verdict() {
    use crate::common::git::{commit_file, git, git_stdout, init_repo};

    let (env, _) = fixture(&format!("{HEADER}{CI_CONDITION}"), "");
    init_repo(&env.project_root);
    let parent = git_stdout(&env.project_root, &["rev-parse", "HEAD"]);
    let id = start(&env);
    let first = until(&env, &id, |v| {
        v["record"]["wait"]["outcome"] == "unknown" && v["record"]["attempt"].is_null()
    });
    let scope = PathBuf::from(first["record"]["scope"]["path"].as_str().unwrap());
    assert_eq!(scope, env.project_root.canonicalize().unwrap());
    commit_file(&scope, "local.txt", "local\n", "local commit during wait");
    let local = git_stdout(&scope, &["rev-parse", "HEAD"]);
    open_forge(&env, &scope, &parent, &parent);
    success(command(&env, &["workflow", "advance", &id]));
    let waiting = show(&env, &id);
    let reason = format!(
        "ci (local HEAD {} is not on the PR, PR head {})",
        &local[..7],
        &parent[..7]
    );
    assert_eq!(waiting["record"]["input"]["outcome"], "unknown");
    assert_eq!(waiting["record"]["input"]["readings"], json!({"ci":null}));
    assert_eq!(waiting["record"]["input"]["reason"], reason);
    assert_eq!(waiting["record"]["wait"]["detail"], reason);
    let text = success(command(&env, &["workflow", "show", &id]));
    assert!(text.contains(&format!("unknown: {reason}")), "{text}");

    open_forge(&env, &scope, &local, &parent);
    success(command(&env, &["workflow", "advance", &id]));
    let waiting = show(&env, &id);
    let reason = format!("ci (read at {}, PR head {})", &parent[..7], &local[..7]);
    assert_eq!(waiting["record"]["input"]["outcome"], "unknown");
    assert_eq!(waiting["record"]["input"]["readings"], json!({"ci":null}));
    assert_eq!(waiting["record"]["wait"]["detail"], reason);
    let text = success(command(&env, &["workflow", "show", &id]));
    assert!(text.contains(&format!("unknown: {reason}")), "{text}");

    commit_file(&scope, "remote.txt", "remote\n", "another push");
    let remote = git_stdout(&scope, &["rev-parse", "HEAD"]);
    git(&scope, &["reset", "--hard", &local]);
    open_forge(&env, &scope, &remote, &remote);
    success(command(&env, &["workflow", "advance", &id]));
    let done = settled(&env, &id, "done");
    assert_eq!(
        done["record"]["input"],
        json!({"outcome":"ready", "case":0, "readings":{"ci":"passed"}})
    );
}

#[test]
fn condition_ci_accepts_a_pr_head_absent_from_the_checkout() {
    let (env, _) = fixture(&format!("{HEADER}{CI_CONDITION}"), "");
    crate::common::git::init_repo(&env.project_root);
    let id = start(&env);
    let first = until(&env, &id, |v| {
        v["record"]["wait"]["outcome"] == "unknown" && v["record"]["attempt"].is_null()
    });
    let scope = Path::new(first["record"]["scope"]["path"].as_str().unwrap());
    let missing = "0123456789abcdef0123456789abcdef01234567";
    open_forge(&env, scope, missing, missing);
    success(command(&env, &["workflow", "advance", &id]));
    let done = settled(&env, &id, "done");
    assert_eq!(
        done["record"]["input"],
        json!({"outcome":"ready", "case":0, "readings":{"ci":"passed"}})
    );
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
    assert_eq!(reason, "pr (forge lookup failing)");
    forge(&env, jiff::Timestamp::now(), true);
    success(command(
        &env,
        &[
            "events",
            "emit",
            "pr.opened",
            "--source",
            "forge",
            "--json",
            &json!({"path":env.project_root}).to_string(),
        ],
    ));
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
