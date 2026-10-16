use super::*;
use crate::config::definitions::LoadedDefinitions;
use std::fs;

const VALID: &str = r#"
[workflow]
scope = "worktree"
start = "pick"
[checks.pick]
run = "python3 checks/pick.py"
on = { ready = "build", done = "@done", pending = "@wait" }
timeout = "1d"
[steps.build]
agent = "claude"
prompt-file = "prompts/build.md"
then = "pick"
max-visits = 3
"#;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("example");
    fs::create_dir_all(dir.join("checks")).unwrap();
    fs::create_dir_all(dir.join("prompts")).unwrap();
    fs::write(dir.join("checks/pick.py"), "print('{}')").unwrap();
    fs::write(dir.join("prompts/build.md"), "Build it").unwrap();
    (home, dir)
}

#[test]
fn validation_rejects_each_invalid_spec() {
    let cases = [
        (
            "unknown field",
            VALID.replace("timeout = \"1d\"", "surprise = true"),
            "checks.pick",
        ),
        (
            "start step",
            VALID.replace("start = \"pick\"", "start = \"build\""),
            "workflow.start",
        ),
        (
            "unknown edge",
            VALID.replace("ready = \"build\"", "ready = \"missing\""),
            "checks.pick.on.ready",
        ),
        (
            "duplicate node",
            format!("{VALID}\n[checks.build]\nrun = 'true'\non.ok = '@done'\n"),
            "build",
        ),
        (
            "no runner",
            VALID.replace("run = \"python3 checks/pick.py\"", ""),
            "checks.pick",
        ),
        (
            "two runners",
            VALID.replace(
                "run = \"python3 checks/pick.py\"",
                "run = 'true'\nagent = 'claude'",
            ),
            "checks.pick",
        ),
        (
            "judge prompt",
            VALID.replace("run = \"python3 checks/pick.py\"", "agent = 'claude'"),
            "checks.pick.prompt-file",
        ),
        (
            "then missing",
            VALID.replace("then = \"pick\"", ""),
            "steps.build.then",
        ),
        (
            "then step",
            VALID.replace("then = \"pick\"", "then = 'build'"),
            "steps.build.then",
        ),
        (
            "then terminal",
            VALID.replace("then = \"pick\"", "then = '@done'"),
            "steps.build.then",
        ),
        (
            "step outcome",
            format!("{VALID}\non.done = '@done'\n"),
            "build",
        ),
        (
            "unknown profile",
            VALID.replace("agent = \"claude\"", "agent = 'missing-profile'"),
            "steps.build.agent",
        ),
        (
            "script window",
            VALID.replace("timeout = \"1d\"", "timeout = '1d'\nwindow = '5h.left>=42'"),
            "checks.pick.window",
        ),
        (
            "bad window",
            format!("{VALID}\nwindow = '5h.left=>42'\n"),
            "steps.build.window",
        ),
        (
            "non-window",
            format!("{VALID}\nwindow = 'ci=passed'\n"),
            "steps.build.window",
        ),
        (
            "missing prompt",
            VALID.replace("prompts/build.md", "prompts/missing.md"),
            "steps.build.prompt-file",
        ),
        (
            "missing script",
            VALID.replace("checks/pick.py", "checks/missing.py"),
            "checks.pick.run",
        ),
        (
            "absolute path",
            VALID.replace("prompts/build.md", "/etc/passwd"),
            "steps.build.prompt-file",
        ),
        (
            "parent path",
            VALID.replace("prompts/build.md", "prompts/../build.md"),
            "steps.build.prompt-file",
        ),
        (
            "unbounded step cycle",
            VALID.replace("max-visits = 3", ""),
            "steps.build",
        ),
        (
            "hot check cycle",
            VALID
                .replace("ready = \"build\"", "ready = 'pick'")
                .replace(", pending = \"@wait\"", ""),
            "checks.pick",
        ),
        (
            "wait timeout",
            VALID.replace("timeout = \"1d\"", ""),
            "checks.pick.timeout",
        ),
        (
            "timeout cannot wait",
            VALID.replace("timeout = \"1d\"", "timeout = '1s'").replace(
                "pending = \"@wait\"",
                "pending = '@wait', timeout = '@wait'",
            ),
            "checks.pick.on.timeout",
        ),
        (
            "step then cannot wait",
            VALID.replace("then = \"pick\"", "then = '@wait'"),
            "steps.build.then",
        ),
        (
            "cap cannot wait",
            format!("{VALID}\non.cap = '@wait'\n"),
            "steps.build.on.cap",
        ),
        (
            "asked cannot wait",
            format!("{VALID}\non.asked = '@wait'\n"),
            "steps.build.on.asked",
        ),
        (
            "cap cannot target itself",
            format!("{VALID}\non.cap = 'build'\n"),
            "steps.build.on.cap",
        ),
        (
            "zero visits",
            VALID.replace("max-visits = 3", "max-visits = 0"),
            "steps.build.max-visits",
        ),
        (
            "bad duration",
            VALID.replace("timeout = \"1d\"", "timeout = 'tomorrow'"),
            "checks.pick.timeout",
        ),
        (
            "bad deadline",
            VALID.replace("start = \"pick\"", "start = 'pick'\ndeadline = 'never'"),
            "workflow.deadline",
        ),
        (
            "header outcome",
            VALID.replace("start = \"pick\"", "start = 'pick'\non.other = '@done'"),
            "workflow",
        ),
        (
            "bad node name",
            VALID.replace("checks.pick", "checks.'bad name'"),
            "bad name",
        ),
        (
            "bad command",
            VALID.replace("python3 checks/pick.py", "'unclosed"),
            "checks.pick.run",
        ),
    ];
    let mut missed = Vec::new();
    for (label, text, path) in cases {
        let (home, dir) = fixture();
        fs::write(dir.join("workflow.toml"), text).unwrap();
        let report = validate(&dir, home.path(), &LoadedDefinitions::default());
        if !report
            .errors
            .iter()
            .any(|issue| issue.path.contains(path) || issue.message.contains(path))
        {
            missed.push(format!("{label}: expected {path}; {report:?}"));
        }
    }
    assert!(missed.is_empty(), "{}", missed.join("\n"));
}

#[test]
fn validation_resolves_edges_and_warns_about_judge_independence() {
    let (home, dir) = fixture();
    let text = VALID.replace(
        "run = \"python3 checks/pick.py\"",
        "agent = 'claude'\nprompt-file = 'prompts/build.md'\nwindow = '5h.left>=42 && 7d.left>=10'",
    );
    fs::write(dir.join("workflow.toml"), text).unwrap();
    let report = validate(&dir, home.path(), &LoadedDefinitions::default());
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(report.rows.len(), 2);
    assert_eq!(report.warnings.len(), 1);
    assert!(report.warnings[0].message.contains("pick"));
    let row = report.rows.iter().find(|row| row.node == "pick").unwrap();
    assert!(row.edges.iter().any(|edge| edge.outcome == "invalid"
        && edge.target == "@user"
        && edge.source == "default"));
    assert!(row.edges.iter().any(|edge| edge.outcome == "ready"
        && edge.target == "build"
        && edge.source == "explicit"));
}

#[test]
fn validation_confines_directories_and_symlinks_and_reports_broken_profiles() {
    let (home, dir) = fixture();
    fs::write(dir.join("workflow.toml"), VALID).unwrap();
    let report = validate(
        &dir.join("checks/.."),
        home.path(),
        &LoadedDefinitions::default(),
    );
    assert!(
        report.errors.is_empty(),
        "a confined directory alias is valid: {report:?}"
    );
    assert!(report.rows.iter().all(|row| row.workflow == "example"));
    let outside = tempfile::tempdir().unwrap();
    assert!(
        !validate(&dir, outside.path(), &LoadedDefinitions::default())
            .errors
            .is_empty()
    );
    std::os::unix::fs::symlink(outside.path(), dir.join("escape")).unwrap();
    fs::write(outside.path().join("prompt.md"), "outside").unwrap();
    fs::write(
        dir.join("workflow.toml"),
        VALID.replace("prompts/build.md", "escape/prompt.md"),
    )
    .unwrap();
    assert!(
        validate(&dir, home.path(), &LoadedDefinitions::default())
            .errors
            .iter()
            .any(|issue| issue.path == "steps.build.prompt-file")
    );
    fs::write(
        dir.join("workflow.toml"),
        VALID.replace("agent = \"claude\"", "agent = 'broken'"),
    )
    .unwrap();
    let mut definitions = LoadedDefinitions::default();
    definitions
        .failed
        .insert("broken".into(), [dir.join("broken.md")].into());
    assert!(
        validate(&dir, home.path(), &definitions)
            .errors
            .iter()
            .any(|issue| issue.message.contains("broken definition"))
    );
}

#[test]
fn schema_is_derived_from_the_loader_types() {
    let value = serde_json::to_value(schema()).unwrap();
    assert_eq!(value["additionalProperties"], false);
    assert!(value["properties"]["workflow"].is_object());
    insta::assert_json_snapshot!("schema", value);
}
