use super::*;

/// The real launch gate: a spent fleet budget holds the step with no agent launched, the run
/// and restart receipts, `list` and `show` name the wait, and the advance after the cap is
/// lifted launches it.
#[test]
fn a_spent_fleet_budget_holds_the_step_until_the_cap_is_lifted() {
    let room = Room::new("");
    let spec = room.env.rimz_home().join("workflows/review/workflow.toml");
    fs::write(
        &spec,
        fs::read_to_string(&spec)
            .unwrap()
            .replace("start = 'pr'", "start = 'build'"),
    )
    .unwrap();
    let config = room.env.rimz_home().join("config.toml");
    fs::write(&config, "[harness]\nbudget = '0/day'\n").unwrap();
    let receipt = success(
        room.command(&["workflow", "run", "review", "--prompt", "Review this"])
            .output()
            .unwrap(),
    );
    let lines: Vec<_> = receipt.lines().collect();
    let id = lines[0].to_owned();
    let waiting = until(&room.env, &id, |v| {
        v["record"]["wait"]["outcome"] == "budget"
    });
    let record = &waiting["record"];
    assert_eq!(record["node"], "build", "{waiting:#}");
    assert!(record["attempt"].is_null(), "{waiting:#}");
    assert_eq!(record["visits"].get("build"), None, "{waiting:#}");
    let detail = record["wait"]["detail"].as_str().unwrap();
    assert!(
        detail.starts_with("fleet budget exhausted ("),
        "{waiting:#}"
    );
    let state = room.env.state_path_for(&room.env.project_root);
    assert!(rimz::harness::run::list(&state).unwrap().is_empty());
    let phrase = format!("budget {detail}");
    assert_eq!(lines.len(), 4, "{receipt}");
    assert_eq!(lines[3], format!("build {phrase}"), "{receipt}");
    let restart = success(
        room.command(&["workflow", "restart", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(
        restart.lines().last(),
        Some(format!("build {phrase}").as_str()),
        "{restart}"
    );
    let listed = success(room.command(&["workflow", "list"]).output().unwrap());
    assert!(listed.contains(&phrase), "{listed}");
    let shown = success(room.command(&["workflow", "show", &id]).output().unwrap());
    assert!(shown.contains(&phrase), "{shown}");
    fs::write(&config, "").unwrap();
    success(
        room.command(&["workflow", "advance", &id])
            .output()
            .unwrap(),
    );
    let run = room.attempt(&id);
    assert!(run.workflow.is_some());
    let live = show(&room.env, &id);
    assert!(live["record"]["wait"].is_null(), "{live:#}");
    assert_eq!(live["record"]["visits"]["build"], 1, "{live:#}");
    success(room.command(&["workflow", "cancel", &id]).output().unwrap());
}
