//! `workflow show|validate` resolve a loop task's command graph beside deployed workflows.

use super::*;

fn task_env(name: &str) -> Env {
    let env = Env::new();
    env.record(&env.project_root);
    let row = format!(
        "[tasks.{name}]\nargs = ['cargo', 'test', 'a b']\ntimeout = '10m'\nevery = '1h'\nroot = {:?}\n",
        env.project_root.display().to_string()
    );
    fs::write(env.rimz_home().join("loop.toml"), row).unwrap();
    env
}

#[test]
fn show_renders_a_tasks_command_as_one_check_node() {
    let env = task_env("nightly");
    let shown = success(command(&env, &["workflow", "show", "nightly"]));
    assert!(shown.contains("loop task nightly"), "{shown}");
    assert!(
        shown.contains("command") && shown.contains("fail @failed") && shown.contains("pass @done"),
        "{shown}"
    );
    let node = success(command(
        &env,
        &["workflow", "show", "task:nightly", "command"],
    ));
    assert!(node.contains("exec cargo test 'a b'"), "{node}");
}

#[test]
fn validate_accepts_a_tasks_command_graph() {
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
    assert_eq!(nodes, ["command"], "{report}");
}

#[test]
fn a_name_that_is_both_a_workflow_and_a_task_refuses_and_names_both_selectors() {
    let env = task_env("review");
    let dir = env.rimz_home().join("workflows/review");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("workflow.toml"),
        "[workflow]\nscope = 'worktree'\nstart = 'pr'\n[script.pr]\ntimeout = '365d'\nrun = 'true'\non.ok = '@done'\non.timeout = '@failed'\non.invalid = '@failed'\n",
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
