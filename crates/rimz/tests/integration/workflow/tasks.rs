//! `workflow show|validate` resolve a loop task's compiled graph beside deployed workflows.

use super::*;

fn task_env(name: &str) -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    let row = format!(
        "[tasks.{name}]\nagent = 'claude'\nprompt = 'fix it'\nevery = '1h'\non = 'fail'\nroot = {:?}\njudge = {{ agent = 'claude', prompt = 'is it broken?' }}\n",
        env.project_root.display().to_string()
    );
    fs::write(env.rimz_home().join("loop.toml"), row).unwrap();
    env
}

#[test]
fn show_renders_a_tasks_compiled_graph_with_the_judge_run_anyway_routes() {
    let env = task_env("nightly");
    let shown = success(command(&env, &["workflow", "show", "nightly"]));
    assert!(shown.contains("loop task nightly"), "{shown}");
    assert!(
        shown.contains("judge-1  check  claude, headless · waits ≤300s, then → agent")
            && shown.contains("fail → agent · pass @done · died → agent · invalid → agent")
            && !shown.contains("(default)"),
        "the watchdog's run-anyway routes read as plain routes: {shown}"
    );
    let node = success(command(
        &env,
        &["workflow", "show", "task:nightly", "agent"],
    ));
    assert!(node.contains("agent"), "{node}");
}

#[test]
fn validate_accepts_a_tasks_compiled_graph() {
    let env = task_env("nightly");
    let report: Value = serde_json::from_str(&success(command(
        &env,
        &["workflow", "validate", "nightly", "--json"],
    )))
    .unwrap();
    assert_eq!(report["errors"], json!([]), "{report}");
    let nodes = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["node"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(nodes, ["judge-1", "agent"], "{report}");
}

#[test]
fn a_name_that_is_both_a_workflow_and_a_task_refuses_and_names_both_selectors() {
    let env = task_env("review");
    let dir = env.rimz_home().join("workflows/review");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("workflow.toml"),
        "[workflow]\nscope = 'worktree'\nstart = 'pr'\n[script.pr]\nrun = 'true'\non.ok = '@done'\non.timeout = '@failed'\non.invalid = '@failed'\n",
    )
    .unwrap();
    for verb in ["show", "validate"] {
        let output = command(&env, &["workflow", verb, "review"]);
        assert_eq!(output.status.code(), Some(2), "{verb}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr).trim(),
            "error: review names a deployed workflow and a loop task; use workflow:review or task:review",
            "{verb}"
        );
    }
    for verb in ["show", "validate"] {
        success(command(&env, &["workflow", verb, "workflow:review"]));
        success(command(&env, &["workflow", verb, "task:review"]));
    }
}

#[test]
fn loop_eject_publishes_a_resident_tasks_graph_and_points_its_machine_row_at_it() {
    let env = Env::new();
    env.record(&env.project_root);
    let loop_toml = env.rimz_home().join("loop.toml");
    fs::write(
        &loop_toml,
        format!(
            "# kept\n[tasks.tend]\nagent = 'claude'\nprompt = 'settle'\nstay = true\neach-worktree = true\nevery = '1h'\nroot = {:?}\ncheck = 'true'\non = 'success'\nsubscribe = [{{ signal = 'ci.failed', prompt = 'repair' }}]\nmax-strikes = 4\n",
            env.project_root.display().to_string()
        ),
    )
    .unwrap();
    let shown = success(command(&env, &["workflow", "show", "tend"]));
    let ejected = success(command(&env, &["loop", "eject", "tend", "tended"]));
    let dir = env.rimz_home().join("workflows/tended");
    assert_eq!(
        ejected,
        format!(
            "ejected loop task `tend` to workflow `tended` at {}\nthe task now runs `workflow = \"tended\"`; edit the workflow to change what it does\n",
            dir.display()
        )
    );
    assert_eq!(
        fs::read_to_string(dir.join("prompts/prompt.md")).unwrap(),
        "settle"
    );
    let row: toml::Table = toml::from_str(&fs::read_to_string(&loop_toml).unwrap()).unwrap();
    let mut keys = row["tasks"]["tend"]
        .as_table()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        [
            "each-worktree",
            "every",
            "max-strikes",
            "once-per-checkout",
            "root",
            "workflow"
        ]
    );
    assert_eq!(
        row["tasks"]["tend"]["once-per-checkout"].as_bool(),
        Some(true)
    );
    assert_eq!(row["tasks"]["tend"]["workflow"].as_str(), Some("tended"));
    assert!(
        fs::read_to_string(&loop_toml)
            .unwrap()
            .starts_with("# kept\n")
    );
    let report: Value = serde_json::from_str(&success(command(
        &env,
        &["workflow", "validate", "tended", "--json"],
    )))
    .unwrap();
    assert_eq!(report["errors"], json!([]), "{report}");
    let redrawn = success(command(&env, &["workflow", "show", "tended"]));
    let rail = |text: &str| {
        text.lines()
            .skip(1)
            .take_while(|line| !line.starts_with("one node in full"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert!(rail(&shown).len() > 2, "{shown}\n---\n{redrawn}");
    assert_eq!(rail(&redrawn), rail(&shown), "{shown}\n---\n{redrawn}");

    let again = command(&env, &["loop", "eject", "tend"]);
    assert!(!again.status.success());
    assert_eq!(
        String::from_utf8_lossy(&again.stderr).trim(),
        "error: loop task `tend` already runs workflow `tended`; edit that workflow instead"
    );
    let kept = command(&env, &["loop", "fire", "tend", "--keep"]);
    assert!(!kept.status.success());
    assert_eq!(
        String::from_utf8_lossy(&kept.stderr).trim(),
        "error: workflow task `tend` cannot use --keep; set `keep = true` on the workflow's `[agent.<name>]`"
    );
    assert_eq!(String::from_utf8_lossy(&kept.stdout), "");
}
