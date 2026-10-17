use crate::common::Env;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod attempts;
mod judges;
mod ticks;

fn fixture(spec: &str, script: &str) -> (Env, PathBuf) {
    let env = Env::new();
    env.record(&env.project_root);
    let dir = env.rimz_home().join("workflows/review");
    fs::create_dir_all(dir.join("checks")).unwrap();
    fs::write(dir.join("workflow.toml"), spec).unwrap();
    fs::write(dir.join("checks/pr.sh"), script).unwrap();
    fs::write(dir.join("prompt.md"), "Build it").unwrap();
    (env, dir)
}

const HEADER: &str = "[workflow]\nscope = 'worktree'\nstart = 'pr'\n";
const WAIT: &str = "[checks.pr]\nrun = 'sh checks/pr.sh'\non = { pending = '@wait', done = '@done' }\ntimeout = '1h'\n";
const PENDING: &str = "printf '%s\\n' '{\"outcome\":\"pending\",\"reason\":\"CI pending\",\"recheck_at\":\"2099-01-01T00:00:00Z\"}'\n";

fn command(env: &Env, args: &[&str]) -> std::process::Output {
    env.rimz().args(args).output().unwrap()
}

fn success(output: std::process::Output) -> String {
    assert!(
        output.status.success(),
        "workflow operation must succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn start(env: &Env) -> String {
    let mut cmd = env.rimz();
    cmd.args(["workflow", "run", "review", "--prompt", "Review this"]);
    let output = success(cmd.output().unwrap());
    output
        .split_whitespace()
        .find(|s| s.starts_with("wf_"))
        .expect("run receipt must include its id")
        .into()
}

fn show(env: &Env, id: &str) -> Value {
    serde_json::from_str(&success(command(env, &["workflow", "show", id, "--json"]))).unwrap()
}

fn until(env: &Env, id: &str, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let value = show(env, id);
        if predicate(&value) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "workflow did not settle: {value:#}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn settled(env: &Env, id: &str, state: &str) -> Value {
    until(env, id, |v| {
        v["record"]["state"] == state && v["record"]["attempt"].is_null()
    })
}

fn events(value: &Value, event: &str) -> Vec<Value> {
    value["journal"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["event"] == event)
        .cloned()
        .collect()
}

fn context(value: &Value) -> Value {
    serde_json::from_slice(
        &fs::read(Path::new(value["dir"].as_str().unwrap()).join("context.json")).unwrap(),
    )
    .unwrap()
}

fn evaluation_script(outcome: &str) -> String {
    format!("printf '%s\\n' '{{\"outcome\":\"{outcome}\"}}'\n")
}

fn assists(env: &Env) -> Value {
    serde_json::from_str(&success(command(
        env,
        &["stats", "--assists", "--json", "-n", "0"],
    )))
    .unwrap()
}

#[test]
fn workflow_accounting_tracks_routes_parks_and_terminal_states() {
    let (env, _) = fixture(
        &format!("{HEADER}[checks.pr]\nrun='sh checks/pr.sh'\non.ok='@user'\n"),
        &evaluation_script("ok"),
    );
    let id = start(&env);
    settled(&env, &id, "parked");
    let stats = assists(&env);
    assert_eq!(stats["rollup"]["workflow_routes"], 1);
    assert_eq!(stats["rollup"]["workflow_parks"], 1);
    let routes: Vec<_> = stats["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["assist"] == "workflow_route")
        .collect();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["run"], id);
    assert_eq!(routes[0]["node"], "pr");
    assert_eq!(routes[0]["to"], "@user");
    success(command(&env, &["workflow", "cancel", &id]));
    let stats = assists(&env);
    assert_eq!(stats["rollup"]["workflow_ends"], 1);
    assert!(
        stats["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["assist"] == "workflow_end" && event["state"] == "canceled")
    );
}

#[test]
fn happy_path_context_and_cross_directory_lookup() {
    let spec = format!(
        "{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non.ok = 'next'\n[checks.next]\nrun = 'sh checks/pr.sh'\non.ok = '@done'\n"
    );
    let (env, _) = fixture(&spec, &evaluation_script("ok"));
    let id = start(&env);
    let value = settled(&env, &id, "done");
    assert_eq!(events(&value, "evaluation").len(), 2);
    assert_eq!(events(&value, "terminal").len(), 1);
    let ctx = context(&value);
    assert_eq!(ctx["run"], id);
    assert_eq!(ctx["node"], "next");
    assert_eq!(ctx["seq"], 2);
    assert_eq!(ctx["prompt"], "Review this");
    assert_eq!(ctx["input"], json!({"outcome":"ok"}));
    assert!(ctx.get("visit").is_none());
    let outside = env
        .rimz()
        .current_dir(&env.home_root)
        .args(["workflow", "show", &id, "--json"])
        .output()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&success(outside)).unwrap()["record"],
        value["record"]
    );
    let listed: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    assert_eq!(listed, json!([]));
}

#[test]
fn list_skips_unreadable_homes_but_named_commands_fail() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"].is_object());
    let bad_ids = unreadable_homes(&env, &value);
    let output = command(&env, &["workflow", "list", "--json"]);
    assert!(
        output.status.success(),
        "unreadable homes must not block listing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["id"], id);
    for bad_id in bad_ids {
        for verb in ["show", "advance", "cancel", "resume"] {
            let output = command(&env, &["workflow", verb, bad_id.as_str()]);
            assert_eq!(output.status.code(), Some(1), "{verb} {bad_id}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("record.json"));
        }
    }
    success(command(&env, &["workflow", "cancel", &id]));
}

#[test]
fn unreadable_homes_do_not_block_run_creation() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"].is_object());
    success(command(&env, &["workflow", "cancel", &id]));
    unreadable_homes(&env, &value);
    let output = command(&env, &["workflow", "run", "review"]);
    assert!(
        output.status.success(),
        "unreadable homes must not block run creation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt = String::from_utf8(output.stdout).unwrap();
    let new_id = receipt.lines().next().unwrap();
    until(&env, new_id, |v| v["record"]["wait"].is_object());
    success(command(&env, &["workflow", "cancel", new_id]));
}

fn unreadable_homes(env: &Env, value: &Value) -> Vec<rimz::ids::WorkflowRunId> {
    let state =
        rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
    let mut future = value["record"].clone();
    future["version"] = json!(2);
    let mut ids = Vec::new();
    for contents in [
        None,
        Some(b"corrupt".to_vec()),
        Some(serde_json::to_vec(&future).unwrap()),
    ] {
        let id = rimz::ids::WorkflowRunId::new();
        let paths = state.workflow(&id);
        fs::create_dir_all(&paths.home).unwrap();
        fs::write(&paths.spec, format!("{HEADER}{WAIT}")).unwrap();
        if let Some(contents) = contents {
            fs::write(&paths.record, contents).unwrap();
        }
        ids.push(id);
    }
    ids
}

#[test]
fn wait_is_merged_clamped_and_cleared_by_signal() {
    let script = format!("printf x >> \"$RIMZ_WORKFLOW_DIR/count\"\n{PENDING}");
    let (env, dir) = fixture(&format!("{HEADER}{WAIT}"), &script);
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"]["count"] == 1);
    let recheck: jiff::Timestamp = value["record"]["recheck_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let entered: jiff::Timestamp = value["record"]["entered_at"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(recheck.as_second() - entered.as_second(), 3600);
    let counter = Path::new(value["dir"].as_str().unwrap()).join("count");
    assert_eq!(fs::read(&counter).unwrap(), b"x");
    success(command(&env, &["workflow", "advance", &id]));
    let value = show(&env, &id);
    assert_eq!(
        events(&value, "evaluation").len(),
        1,
        "early advance must not execute the script"
    );
    assert_eq!(value["record"]["wait"]["count"], 2);
    assert_eq!(
        fs::read(&counter).unwrap(),
        b"x",
        "early advance must not run the script"
    );
    fs::write(dir.join("checks/pr.sh"), evaluation_script("done")).unwrap();
    success(command(&env, &["events", "emit", "review.ready"]));
    let value = settled(&env, &id, "done");
    assert_eq!(events(&value, "wake")[0]["signal"], "review.ready");
    assert_eq!(events(&value, "wait")[0]["count"], 2);
    assert_eq!(events(&value, "evaluation").len(), 2);
}

#[test]
fn signal_during_evaluation_rechecks_the_result_without_backoff() {
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    let script = format!(
        "printf x >> \"$RIMZ_WORKFLOW_DIR/count\"\nif test \"$(wc -c < \"$RIMZ_WORKFLOW_DIR/count\")\" -eq 1; then\n{PENDING}\nelse\n{}\nfi",
        evaluation_script("done")
    );
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), &script);
    let socket = env.runtime_root.join("wake-result.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let receipt = success(
        env.rimz()
            .args(["workflow", "run", "review"])
            .env("RIMZ_TEST_WORKFLOW_RESULT_GATE", &socket)
            .output()
            .unwrap(),
    );
    let id = receipt.lines().next().unwrap();
    for evaluation in 1..=2 {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut gate = loop {
            if let Ok((stream, _)) = listener.accept() {
                break stream;
            }
            assert!(
                Instant::now() < deadline,
                "wake during evaluation must trigger evaluation {evaluation} without backoff"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        if evaluation == 1 {
            success(command(&env, &["events", "emit", "review.ready"]));
            until(&env, id, |v| !events(v, "wake").is_empty());
        }
        gate.write_all(&[1]).unwrap();
    }
    let value = settled(&env, id, "done");
    assert_eq!(
        fs::read(Path::new(value["dir"].as_str().unwrap()).join("count")).unwrap(),
        b"xx"
    );
    assert_eq!(events(&value, "evaluation").len(), 2);
}

#[test]
fn signal_wake_skips_a_bad_sealed_spec_and_continues() {
    let spec = format!("{HEADER}{WAIT}");
    let (env, first_dir) = fixture(&spec, PENDING);
    let second_dir = first_dir.with_file_name("another");
    fs::create_dir_all(second_dir.join("checks")).unwrap();
    fs::write(second_dir.join("workflow.toml"), &spec).unwrap();
    fs::write(second_dir.join("checks/pr.sh"), PENDING).unwrap();
    let first = start(&env);
    let receipt = success(command(&env, &["workflow", "run", "another"]));
    let second = receipt.lines().next().unwrap();
    let first_value = until(&env, &first, |v| v["record"]["wait"].is_object());
    until(&env, second, |v| v["record"]["wait"].is_object());
    let state =
        rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
    let first_id = rimz::ids::WorkflowRunId::parse(&first).unwrap();
    fs::write(state.workflow(&first_id).spec, "bad sealed spec").unwrap();
    fs::write(second_dir.join("checks/pr.sh"), evaluation_script("done")).unwrap();
    success(command(&env, &["events", "emit", "review.ready"]));
    let deadline = Instant::now() + Duration::from_secs(3);
    while show(&env, second)["record"]["state"] != "done" {
        assert!(
            Instant::now() < deadline,
            "a bad sealed spec must not block later runs' wakes"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(Path::new(first_value["dir"].as_str().unwrap()).is_dir());
}

#[test]
fn run_dot_resolves_the_current_definition_directory() {
    let (env, dir) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    for name in [".", "./"] {
        let output = env
            .rimz()
            .current_dir(&dir)
            .args([
                "--root",
                env.project_root.to_str().unwrap(),
                "workflow",
                "run",
                name,
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name} must resolve as a path: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let receipt = String::from_utf8(output.stdout).unwrap();
        let id = receipt.lines().next().unwrap();
        until(&env, id, |v| v["record"]["wait"].is_object());
        success(command(&env, &["workflow", "cancel", id]));
    }
}

#[test]
fn signal_wakes_every_run_without_waiting_for_another_script() {
    let spec = format!("{HEADER}{WAIT}");
    let (env, first_dir) = fixture(&spec, PENDING);
    let second_dir = first_dir.with_file_name("another");
    fs::create_dir_all(second_dir.join("checks")).unwrap();
    fs::write(second_dir.join("workflow.toml"), spec).unwrap();
    fs::write(second_dir.join("checks/pr.sh"), PENDING).unwrap();
    let first = start(&env);
    let receipt = success(command(&env, &["workflow", "run", "another"]));
    let second = receipt.lines().next().unwrap();
    for id in [first.as_str(), second] {
        until(&env, id, |v| v["record"]["wait"].is_object());
    }
    for dir in [first_dir, second_dir] {
        fs::write(dir.join("checks/pr.sh"), "exec sleep 60").unwrap();
    }
    success(command(&env, &["events", "emit", "review.ready"]));
    let deadline = Instant::now() + Duration::from_secs(3);
    let both_started = loop {
        if [first.as_str(), second]
            .iter()
            .all(|id| show(&env, id)["record"]["attempt"].is_object())
        {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    for id in [first.as_str(), second] {
        success(command(&env, &["workflow", "cancel", id]));
    }
    assert!(
        both_started,
        "a slow script must not block another run's signal wake"
    );
}

#[test]
fn sealed_spec_live_scripts_and_hot_loop_guard() {
    let spec = format!(
        "{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non = {{ again = 'next', pause = '@wait' }}\ntimeout = '1h'\n[checks.next]\nrun = 'sh checks/pr.sh'\non.again = 'pr'\n"
    );
    let (env, dir) = fixture(&spec, &evaluation_script("again"));
    let id = start(&env);
    let value = until(&env, &id, |v| {
        events(v, "evaluation").len() == 2 && v["record"]["attempt"].is_null()
    });
    assert_eq!(value["record"]["state"], "active");
    assert_eq!(value["record"]["node"], "pr");
    fs::write(dir.join("workflow.toml"), "not valid TOML").unwrap();
    fs::write(dir.join("checks/pr.sh"), evaluation_script("pause")).unwrap();
    success(command(&env, &["workflow", "advance", &id]));
    let value = show(&env, &id);
    let rows = events(&value, "evaluation");
    assert_eq!(rows.len(), 3);
    assert_ne!(rows[0]["digests"], rows[2]["digests"]);
    assert_eq!(value["record"]["wait"]["outcome"], "pause");
}

#[test]
fn malformed_and_unrouted_objects_are_invalid() {
    let bodies = [
        "exit 3",
        "printf nope",
        "printf '%s' '{}'",
        "printf '%s' '{\"outcome\":\"unknown\"}'",
        "printf '%s' '{\"outcome\":\"timeout\"}'",
        "printf '%s' '{\"outcome\":\"ok\",\"key\":[]}'",
        "printf '%s' '{\"outcome\":\"ok\",\"reason\":3}'",
        "printf '%s' '{\"outcome\":\"ok\",\"recheck_at\":\"bad\"}'",
    ];
    for body in bodies {
        let spec = format!(
            "{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non = {{ ok = '@done', invalid = 'report' }}\n[checks.report]\nrun = 'sh checks/report.sh'\non.ok = '@done'\n"
        );
        let (env, dir) = fixture(&spec, body);
        fs::write(dir.join("checks/report.sh"), evaluation_script("ok")).unwrap();
        let id = start(&env);
        let value = settled(&env, &id, "done");
        assert_eq!(context(&value)["input"]["outcome"], "invalid", "{body}");
        assert_eq!(events(&value, "route")[0]["outcome"], "invalid");
        assert_eq!(events(&value, "evaluation")[0]["outcome"], "invalid");
    }
}

#[test]
fn invalid_defaults_park_and_deleted_script_is_invalid() {
    let (env, dir) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    until(&env, &id, |v| v["record"]["wait"].is_object());
    fs::remove_file(dir.join("checks/pr.sh")).unwrap();
    success(command(&env, &["events", "emit", "review.ready"]));
    let value = settled(&env, &id, "parked");
    assert_eq!(value["record"]["park"]["reason"], "invalid");
    assert!(
        value["record"]["park"]["text"]
            .as_str()
            .unwrap()
            .contains("pr.sh")
    );
}

fn steps_spec(cap: &str) -> String {
    format!(
        "{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non = {{ work = 'build', done = '@done' }}\n[steps.build]\nagent = 'claude'\nprompt-file = 'prompt.md'\nthen = 'pr'\nmax-visits = 1\n{cap}\n"
    )
}

#[test]
fn cap_only_self_cycle_is_refused_before_launch() {
    let (env, dir) = fixture(&steps_spec("on.cap = 'build'"), &evaluation_script("work"));
    let output = env
        .rimz()
        .args(["workflow", "run", "review"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "a cap self-cycle must be refused before launch"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("steps.build.on.cap"));
    assert!(dir.exists());
}

#[test]
fn deadline_routes_once_and_can_be_rearmed() {
    let spec = format!(
        "[workflow]\nscope = 'worktree'\nstart = 'pr'\ndeadline = '2000-01-01T00:00:00Z'\non.deadline = 'report'\n{WAIT}[checks.report]\nrun = 'sh checks/pr.sh'\non = {{ pending = '@wait', done = '@done' }}\ntimeout = '1h'\n"
    );
    let (env, dir) = fixture(&spec, PENDING);
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"].is_object());
    assert_eq!(
        context(&value)["input"],
        json!({"outcome":"deadline","deadline":"2000-01-01T00:00:00Z"})
    );
    success(command(&env, &["workflow", "advance", &id]));
    assert_eq!(events(&show(&env, &id), "route").len(), 1);
    success(command(
        &env,
        &[
            "workflow",
            "resume",
            &id,
            "--extend",
            "deadline=2001-01-01T00:00:00Z",
        ],
    ));
    let value = until(&env, &id, |v| {
        v["record"]["deadline"] == "2001-01-01T00:00:00Z"
            && v["record"]["deadline_fired"] == true
            && v["record"]["wait"].is_object()
            && v["record"]["attempt"].is_null()
    });
    assert_eq!(events(&value, "route").len(), 2);
    assert_eq!(value["record"]["deadline_fired"], true);
    fs::write(dir.join("checks/pr.sh"), evaluation_script("done")).unwrap();
    success(command(&env, &["events", "emit", "review.ready"]));
    let value = settled(&env, &id, "done");
    success(command(&env, &["workflow", "advance", &id]));
    assert_eq!(show(&env, &id)["record"], value["record"]);
}

#[test]
fn timeout_kills_script_and_routes_or_parks() {
    for edge in ["", "on.timeout = 'report'"] {
        let spec = format!(
            "{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non.ok = '@done'\ntimeout = '1s'\n{edge}\n[checks.report]\nrun = 'sh checks/report.sh'\non.ok = '@done'\n"
        );
        let (env, dir) = fixture(
            &spec,
            "echo $$ > \"$RIMZ_WORKFLOW_DIR/script.pid\"\nexec sleep 60\n",
        );
        fs::write(dir.join("checks/report.sh"), evaluation_script("ok")).unwrap();
        let id = start(&env);
        let value = settled(&env, &id, if edge.is_empty() { "parked" } else { "done" });
        let input = if edge.is_empty() {
            value["record"]["input"].clone()
        } else {
            context(&value)["input"].clone()
        };
        assert_eq!(
            input,
            json!({"outcome":"timeout","node":"pr","timeout":"1s"})
        );
        assert!(
            events(&value, "evaluation")[0]["digests"]["checks/pr.sh"].is_string(),
            "a timed-out evaluation must retain the script digest it ran"
        );
        let pid: u32 =
            fs::read_to_string(Path::new(value["dir"].as_str().unwrap()).join("script.pid"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
        assert!(!rimz::proc::process_is_live(pid, None));
    }
}

#[test]
fn waiting_timeout_and_live_deadline_use_original_clock() {
    let (env, _) = fixture(&format!("{HEADER}{}", WAIT.replace("1h", "1s")), PENDING);
    let id = start(&env);
    until(&env, &id, |v| v["record"]["wait"].is_object());
    std::thread::sleep(Duration::from_millis(1100));
    success(command(&env, &["workflow", "advance", &id]));
    assert_eq!(show(&env, &id)["record"]["park"]["reason"], "timeout");
    let spec = "[workflow]\nscope = 'worktree'\nstart = 'pr'\ndeadline = '2099-01-01T00:00:00Z'\non.deadline = 'report'\n[checks.pr]\nrun = 'sh checks/pr.sh'\non.ok = '@done'\n[checks.report]\nrun = 'sh checks/report.sh'\non.ok = '@done'\n";
    let (env, dir) = fixture(spec, "exec sleep 60");
    fs::write(dir.join("checks/report.sh"), evaluation_script("ok")).unwrap();
    let id = start(&env);
    until(&env, &id, |v| v["record"]["attempt"].is_object());
    let deadline = jiff::Timestamp::now()
        .checked_add(Duration::from_secs(1))
        .unwrap();
    success(command(
        &env,
        &[
            "workflow",
            "resume",
            &id,
            "--extend",
            &format!("deadline={deadline}"),
        ],
    ));
    let value = settled(&env, &id, "done");
    assert_eq!(context(&value)["input"]["outcome"], "deadline");
    assert_eq!(events(&value, "route").len(), 1);
}

#[test]
fn cancel_interrupts_live_evaluation_and_first_terminal_wins() {
    let spec = format!("{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non.ok = '@done'\n");
    let (env, _) = fixture(
        &spec,
        "echo $$ > \"$RIMZ_WORKFLOW_DIR/script.pid\"\nexec sleep 60\n",
    );
    let id = start(&env);
    let value = until(&env, &id, |v| {
        Path::new(v["dir"].as_str().unwrap())
            .join("script.pid")
            .exists()
    });
    let pid: u32 = fs::read_to_string(Path::new(value["dir"].as_str().unwrap()).join("script.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    success(command(&env, &["workflow", "cancel", &id]));
    let deadline = Instant::now() + Duration::from_secs(3);
    while rimz::proc::process_is_live(pid, None) {
        assert!(
            Instant::now() < deadline,
            "cancel must kill the script group"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let value = show(&env, &id);
    assert_eq!(value["record"]["state"], "canceled");
    let evaluations = events(&value, "evaluation");
    assert_eq!(
        evaluations.len(),
        1,
        "cancel must journal the live evaluation"
    );
    assert_eq!(evaluations[0]["outcome"], "canceled");
    assert_eq!(evaluations[0]["seq"], 1);
    let before = value.clone();
    success(command(&env, &["workflow", "cancel", &id]));
    success(command(&env, &["workflow", "advance", &id]));
    assert!(!command(&env, &["workflow", "resume", &id]).status.success());
    assert_eq!(show(&env, &id), before);
}

#[test]
fn another_process_kills_a_stalled_hosts_script_on_cancel_or_bound() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    for bounded in [false, true] {
        let spec = format!(
            "{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non.ok = '@done'\ntimeout = '1h'\n"
        );
        let (env, _) = fixture(&spec, "exec sleep 60\n");
        let id = start(&env);
        let value = until(&env, &id, |value| {
            value["record"]["attempt"]["child"]["pid"].is_number()
        });
        let host = value["record"]["attempt"]["host"]["pid"].as_u64().unwrap() as i32;
        let child = value["record"]["attempt"]["child"]["pid"].as_u64().unwrap() as u32;
        let state =
            rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
        // Publication precedes unlock; stop only after the host released that lock.
        let guard = rimz::disk::lock::WorkspaceLock::acquire(&state.workspace_lock).unwrap();
        kill(Pid::from_raw(host), Signal::SIGSTOP).unwrap();
        struct ResumeHost(i32);
        impl Drop for ResumeHost {
            fn drop(&mut self) {
                let _ = kill(Pid::from_raw(self.0), Signal::SIGCONT);
            }
        }
        let resume = ResumeHost(host);
        if bounded {
            let mut record = value["record"].clone();
            record["entered_at"] = json!("2000-01-01T00:00:00Z");
            let path = Path::new(value["dir"].as_str().unwrap())
                .parent()
                .unwrap()
                .join("record.json");
            fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
        }
        drop(guard);
        success(command(
            &env,
            &["workflow", if bounded { "advance" } else { "cancel" }, &id],
        ));
        let deadline = Instant::now() + Duration::from_secs(3);
        while rimz::proc::process_is_live(child, None) {
            assert!(
                Instant::now() < deadline,
                "the second process must kill the group while its host is stopped"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let value = show(&env, &id);
        assert_eq!(
            events(&value, "evaluation")[0]["outcome"],
            if bounded { "timeout" } else { "canceled" }
        );
        drop(resume);
    }
}

#[test]
fn concurrent_advance_does_not_mutate_live_attempt() {
    let (env, _) = fixture(
        &format!("{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non.ok = '@done'\n"),
        "exec sleep 60",
    );
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["attempt"]["child"].is_object());
    success(command(&env, &["workflow", "advance", &id]));
    assert_eq!(
        show(&env, &id),
        value,
        "a second advance must neither launch nor write"
    );
    success(command(&env, &["workflow", "cancel", &id]));
}

#[test]
fn stale_host_cannot_overwrite_resumed_context() {
    use std::io::Write;
    use std::os::unix::net::UnixListener;

    let spec = "[workflow]\nscope = 'worktree'\nstart = 'pr'\ndeadline = '2099-01-01T00:00:00Z'\n[checks.pr]\nrun = 'sh checks/pr.sh'\non.ok = '@done'\n";
    let (env, _) = fixture(
        spec,
        "while ! test -f \"$RIMZ_WORKFLOW_DIR/consume\"; do sleep 0.01; done\ncp \"$RIMZ_WORKFLOW_DIR/context.json\" \"$RIMZ_WORKFLOW_DIR/consumed.tmp\"\nmv \"$RIMZ_WORKFLOW_DIR/consumed.tmp\" \"$RIMZ_WORKFLOW_DIR/consumed.json\"\nexec sleep 60",
    );
    let socket = env.runtime_root.join("context-gate.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let receipt = success(
        env.rimz()
            .args(["workflow", "run", "review"])
            .env("RIMZ_TEST_WORKFLOW_CONTEXT_GATE", &socket)
            .output()
            .unwrap(),
    );
    let id = receipt.lines().next().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut gate = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "host never reached context publication"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let host = show(&env, id)["record"]["attempt"]["host"]["pid"]
        .as_u64()
        .unwrap() as u32;
    success(command(
        &env,
        &[
            "workflow",
            "resume",
            id,
            "--extend",
            "deadline=2000-01-01T00:00:00Z",
        ],
    ));
    settled(&env, id, "parked");
    success(command(
        &env,
        &[
            "workflow",
            "resume",
            id,
            "--extend",
            "deadline=2099-01-01T00:00:00Z",
        ],
    ));
    let value = until(&env, id, |v| {
        v["record"]["attempt"]["seq"] == 2
            && Path::new(v["dir"].as_str().unwrap())
                .join("context.json")
                .exists()
            && context(v)["seq"] == 2
    });
    let expected = context(&value);
    gate.write_all(&[1]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while rimz::proc::process_is_live(host, None) {
        assert!(Instant::now() < deadline, "stale host did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    let work = Path::new(value["dir"].as_str().unwrap());
    fs::write(work.join("consume"), "").unwrap();
    until(&env, id, |_| work.join("consumed.json").exists());
    let observed: Value =
        serde_json::from_slice(&fs::read(work.join("consumed.json")).unwrap()).unwrap();
    success(command(&env, &["workflow", "cancel", id]));
    assert_eq!(
        observed, expected,
        "a stale host must not overwrite a newer attempt's context"
    );
}

#[test]
fn restart_between_evaluation_and_transition_reruns_check_free() {
    use std::os::unix::net::UnixListener;
    let (env, _) = fixture(
        &format!("{HEADER}[checks.pr]\nrun = 'sh checks/pr.sh'\non.ok = '@done'\n"),
        &evaluation_script("ok"),
    );
    let socket = env.runtime_root.join("workflow-gate.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let output = success(
        env.rimz()
            .args(["workflow", "run", "review"])
            .env("RIMZ_TEST_WORKFLOW_RESULT_GATE", &socket)
            .output()
            .unwrap(),
    );
    let id = output
        .split_whitespace()
        .find(|s| s.starts_with("wf_"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let _stream = loop {
        if let Ok((stream, _)) = listener.accept() {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "advance never reached its result boundary"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let value = show(&env, id);
    let host = value["record"]["attempt"]["host"]["pid"].as_u64().unwrap() as u32;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(host as i32),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while rimz::proc::process_is_live(host, None) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    success(command(&env, &["workflow", "advance", id]));
    let value = settled(&env, id, "done");
    let evaluations = events(&value, "evaluation");
    assert_eq!(evaluations.len(), 2);
    assert_eq!(evaluations[0]["outcome"], "canceled");
    assert_eq!(evaluations[1]["outcome"], "ok");
    assert_eq!(events(&value, "terminal").len(), 1);
    assert_eq!(value["record"]["visits"], json!({}));
}

#[test]
fn run_refuses_duplicates_missing_rooms_unsupported_nodes_and_outside_dirs() {
    let (env, dir) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    until(&env, &id, |v| v["record"]["wait"].is_object());
    let duplicate = command(&env, &["workflow", "run", "review"]);
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains(&id));
    let mut record = show(&env, &id)["record"].clone();
    record["state"] = json!("parked");
    record["park"] = json!({"reason":"user", "text":"Review", "at":record["created_at"]});
    let paths = env
        .state_path_for(&env.project_root)
        .workflow(&id.parse().unwrap());
    fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
    let duplicate = command(&env, &["workflow", "run", "review"]);
    assert_eq!(duplicate.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&duplicate.stderr)
            .contains(&format!("workflow review already has run {id} for"))
    );
    let outside = command(
        &env,
        &["workflow", "run", env.project_root.to_str().unwrap()],
    );
    assert_eq!(outside.status.code(), Some(1));
    success(command(&env, &["workflow", "cancel", &id]));
    fs::write(
        dir.join("workflow.toml"),
        format!("[workflow]\nscope='project'\nstart='pr'\n{WAIT}"),
    )
    .unwrap();
    let agent = command(&env, &["workflow", "run", "review"]);
    assert_eq!(agent.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&agent.stderr).contains("scope = project"));
    fs::write(dir.join("workflow.toml"), format!("{HEADER}{WAIT}")).unwrap();
    let empty = Env::new();
    let missing = empty
        .rimz()
        .args(["workflow", "run", dir.to_str().unwrap()])
        .env("RIMZ_HOME", env.rimz_home())
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("rimz start"));
}

#[test]
fn human_show_wait_and_journal_are_readable() {
    let (env, dir) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"].is_object());
    let text = success(command(&env, &["workflow", "show", &id]));
    assert_eq!(text.lines().next().unwrap(), value["dir"].as_str().unwrap());
    assert!(text.contains("waiting: pr, said pending"));
    assert!(text.contains("×1"));
    assert!(text.contains("evaluation"));
    assert!(text.contains("CI pending"));
    assert!(text.contains("Review this"));
    let local_time = |at: &Value| {
        at.as_str()
            .unwrap()
            .parse::<jiff::Timestamp>()
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%H:%M")
            .to_string()
    };
    let wait = &value["record"]["wait"];
    assert!(text.contains(&format!(
        "waiting: pr, said pending, since {}, ×1, recheck {}",
        local_time(&wait["first"]),
        local_time(&wait["recheck_at"])
    )));
    let mut normalized = text
        .replace(value["dir"].as_str().unwrap(), "<work>")
        .replace(env.project_root.to_str().unwrap(), "<checkout>");
    for at in [
        &value["record"]["entered_at"],
        &value["record"]["wait"]["first"],
        &value["record"]["recheck_at"],
        &value["journal"][0]["at"],
        &value["journal"][0]["object"]["recheck_at"],
    ] {
        normalized = normalized.replace(at.as_str().unwrap(), "<time>");
    }
    normalized = normalized
        .replace(&local_time(&wait["first"]), "<clock>")
        .replace(&local_time(&wait["recheck_at"]), "<clock>");
    insta::assert_snapshot!("show_wait", normalized);
    fs::write(dir.join("checks/pr.sh"), "printf 'not-json'").unwrap();
    success(command(&env, &["events", "emit", "review.ready"]));
    settled(&env, &id, "parked");
    let parked = success(command(&env, &["workflow", "show", &id]));
    assert!(parked.contains("pr wake review.ready"));
    assert!(parked.contains("pr park invalid"));
    fs::write(dir.join("checks/pr.sh"), evaluation_script("done")).unwrap();
    success(command(&env, &["workflow", "resume", &id]));
    settled(&env, &id, "done");
    let terminal = success(command(&env, &["workflow", "show", &id]));
    assert!(terminal.contains("pr terminal done"));
}

#[test]
fn human_window_wait_includes_evidence_and_omits_missing_recheck() {
    let (env, _) = fixture(&format!("{HEADER}{WAIT}"), PENDING);
    let id = start(&env);
    let value = until(&env, &id, |v| v["record"]["wait"].is_object());
    let mut record = value["record"].clone();
    record["wait"] = json!({"node":"pr", "outcome":"window", "detail":"5h.left=12<42",
        "first":"2026-01-01T15:02:00Z", "last":"2026-01-01T15:02:00Z", "count":40, "recheck_at":null});
    record["recheck_at"] = Value::Null;
    let paths = env
        .state_path_for(&env.project_root)
        .workflow(&id.parse().unwrap());
    fs::write(&paths.record, serde_json::to_vec(&record).unwrap()).unwrap();
    let text = success(command(&env, &["workflow", "show", &id]));
    let clock = "2026-01-01T15:02:00Z"
        .parse::<jiff::Timestamp>()
        .unwrap()
        .to_zoned(jiff::tz::TimeZone::system())
        .strftime("%H:%M")
        .to_string();
    let line = format!("waiting: pr, window 5h.left=12<42, since {clock}, ×40");
    assert!(text.lines().any(|shown| shown == line), "{text}");
    let listed = success(command(&env, &["workflow", "list"]));
    assert!(listed.contains(&line), "{listed}");
    success(command(&env, &["workflow", "cancel", &id]));
}

#[test]
fn validate_json_shape_errors_and_exit_status() {
    let env = Env::new();
    let dir = env.rimz_home().join("workflows/review");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("workflow.toml"), "[workflow]\nscope = 'worktree'\nstart = 'pr'\n[checks.pr]\nrun = 'true'\non.ok = 'missing'\n").unwrap();
    let output = env
        .rimz()
        .args(["workflow", "validate", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["warnings"], json!([]));
    assert_eq!(
        report["rows"],
        json!([{
            "workflow": "review", "node": "pr", "kind": "check", "runner": "run", "run": "true", "agent": null, "do_background": false,
            "edges": [
                {"outcome":"deadline", "target":"@user", "source":"default"},
                {"outcome":"invalid", "target":"@user", "source":"default"},
                {"outcome":"ok", "target":"missing", "source":"explicit"},
                {"outcome":"timeout", "target":"@user", "source":"default"}
            ]
        }])
    );
    assert_eq!(report["errors"].as_array().unwrap().len(), 1);
    assert_eq!(report["errors"][0]["path"], "checks.pr.on.ok");
    assert!(
        report["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("pr")
    );
    fs::write(dir.join("workflow.toml"), "[workflow]\nscope = 'worktree'\nstart = 'pr'\n[checks.pr]\nrun = 'true'\non.ok = '@done'\n").unwrap();
    assert!(
        env.rimz()
            .args(["workflow", "validate"])
            .status()
            .unwrap()
            .success()
    );
    let schema = env.rimz().args(["workflow", "schema"]).output().unwrap();
    assert!(schema.status.success());
    let schema: Value = serde_json::from_slice(&schema.stdout).unwrap();
    assert!(schema["properties"]["checks"].is_object());
    assert!(
        !env.rimz_home().join("ws").exists(),
        "validation must not birth a room"
    );
}

#[test]
fn babysit_fixture_validates_capabilities_and_missing_script() {
    let env = Env::new();
    let dir = env.rimz_home().join("workflows/babysit-pr");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/workflows/babysit-pr");
    for relative in [
        "workflow.toml",
        "checks/pr.py",
        "checks/notes.py",
        "checks/triage-judge.md",
        "prompts/sweep.md",
        "prompts/triage.md",
    ] {
        let target = dir.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(source.join(relative), target).unwrap();
    }
    for (name, kind) in [
        ("haiku", "claude"),
        ("sweeper", "codex"),
        ("yagni", "claude"),
    ] {
        crate::common::write_definition(
            &env,
            "agents",
            name,
            &format!("description: Fixture {name}\nagent: {kind}\ntools: []"),
            "",
        );
    }
    let value: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "validate", "--json"]))).unwrap();
    assert_eq!(value["errors"], json!([]));
    let rows = value["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(
        rows.iter().find(|row| row["node"] == "sweep").unwrap()["do_background"],
        false
    );
    assert_eq!(
        rows.iter().find(|row| row["node"] == "triage").unwrap()["do_background"],
        true
    );
    assert_eq!(value["warnings"].as_array().unwrap().len(), 4);
    fs::remove_file(dir.join("checks/notes.py")).unwrap();
    let failed = command(&env, &["workflow", "validate", "--json"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(
        String::from_utf8(failed.stdout)
            .unwrap()
            .contains("checks.notes.run")
    );
    assert!(!env.rimz_home().join("ws").exists());
}
