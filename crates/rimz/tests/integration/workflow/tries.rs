//! `rimz workflow run --at`, `--input`, and `--once`: a start placed by hand, and a try of one
//! node.

use super::*;
use std::io::{BufRead, BufReader};
use std::process::Stdio;

/// `pr` waits on CI; `lint` answers what the definition folder's `answer.json` holds (a check
/// runs outside that folder, so `tries` writes the script with its absolute path); `slow` runs
/// until it is stopped.
const TRIES: &str = "[workflow]\nscope = 'worktree'\nstart = 'pr'\n\
    [script.pr]\nrun = 'sh checks/pr.sh'\non = { pending = '@wait', done = '@done' }\ntimeout = '1h'\n\
    [script.lint]\nrun = 'sh checks/lint.sh'\non = { red = 'pr', bad = '@failed' }\ntimeout = '1h'\n\
    [script.slow]\nrun = 'sleep 347'\non = { done = '@done' }\ntimeout = '1h'\n";

fn tries() -> (Env, PathBuf) {
    let (env, dir) = fixture(TRIES, PENDING);
    let answers = dir.join("answer.json");
    fs::write(
        dir.join("checks/lint.sh"),
        format!("cat '{}'\n", answers.display()),
    )
    .unwrap();
    (env, dir)
}

fn id_of(stdout: &str) -> String {
    stdout
        .split_whitespace()
        .find(|word| word.starts_with("wf_"))
        .expect("the receipt names the run")
        .into()
}

/// The finished work dir appears once in a try's output, whether the try ended in its first
/// pass or after it.
fn assert_names_finished_work_once(env: &Env, id: &str, stdout: &str) {
    let work = run_home(env, id).work;
    let tail = work
        .strip_prefix(work.parent().unwrap().parent().unwrap().parent().unwrap())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        stdout.lines().filter(|line| line.ends_with(&tail)).count(),
        1,
        "{tail}: {stdout}"
    );
}

fn finished_record(env: &Env, id: &str) -> Value {
    let paths = run_home(env, id);
    assert!(
        paths
            .home
            .to_string_lossy()
            .contains("/finished-workflows/"),
        "{}",
        paths.home.display()
    );
    serde_json::from_slice(&fs::read(&paths.record).unwrap()).unwrap()
}

#[test]
fn a_try_prints_its_answer_and_the_route_it_did_not_take_and_exits_zero_whatever_the_route() {
    for (answer, target) in [("red", "pr"), ("bad", "@failed")] {
        let (env, dir) = tries();
        fs::write(
            dir.join("answer.json"),
            format!("{{\"outcome\":\"{answer}\"}}"),
        )
        .unwrap();
        let output = command(
            &env,
            &[
                "workflow",
                "run",
                "review",
                "--at",
                "lint",
                "--input",
                r#"{"outcome":"pass","key":"a"}"#,
                "--once",
            ],
        );
        let stdout = success(output);
        let id = id_of(&stdout);
        assert!(
            stdout.contains(&format!("→ rimz workflow show {id}\n")),
            "{stdout}"
        );
        assert!(
            !stdout
                .lines()
                .any(|line| line.contains("rimz workflow ") && line.contains("'review#")),
            "a lone try's commands name it by id: {stdout}"
        );
        assert!(
            stdout
                .lines()
                .any(|line| line.split_whitespace().next() == Some("tried")),
            "the finished view opens with tried: {stdout}"
        );
        assert!(
            stdout.contains(&format!(
                "terminal tried · stopped after one node; the run would go → {target}"
            )),
            "{stdout}"
        );
        assert!(stdout.contains(&format!("said {answer}")), "{stdout}");
        assert!(stdout.contains("start by hand · input "), "{stdout}");
        assert_names_finished_work_once(&env, &id, &stdout);
        let record = finished_record(&env, &id);
        assert_eq!(
            (
                &record["once"],
                &record["outcome"]["ending"]["kind"],
                &record["state"],
                &record["node"],
                &record["key"],
            ),
            (
                &json!(true),
                &json!("tried"),
                &json!("done"),
                &json!("lint"),
                &json!("a")
            ),
            "{answer}"
        );
        let canceled = success(command(&env, &["workflow", "cancel", &id]));
        assert!(
            canceled.starts_with("review#") && canceled.contains(" is already tried; unchanged\n"),
            "{canceled}"
        );
        // The ended try's home back in the live directory, as a failed move leaves it.
        let finished = run_home(&env, &id).home;
        let live = env
            .state_path_for(&env.project_root)
            .workflow(&id.parse().unwrap())
            .home;
        fs::rename(&finished, &live).unwrap();
        let shown = success(command(&env, &["workflow", "show", &id]));
        let trail = shown
            .lines()
            .find(|line| line.contains("trail"))
            .unwrap_or_else(|| panic!("a trail: {shown}"));
        assert!(trail.ends_with(" tried"), "{trail}");
        for args in [
            &["workflow", "restart", &id][..],
            &["workflow", "restart", &id, "--continue"],
        ] {
            let output = command(&env, args);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !output.status.success()
                    && stderr.contains(&format!("workflow run {id} is tried; cannot restart")),
                "{args:?}: {stderr}"
            );
        }
    }
}

#[test]
fn a_try_whose_definition_stops_fitting_it_ends_failed_and_exits_one() {
    let (env, dir) = tries();
    let edited = dir.join("edited.toml");
    fs::write(
        &edited,
        TRIES.replace(
            "[script.lint]\nrun = 'sh checks/lint.sh'\non = { red = 'pr', bad = '@failed' }",
            "[command.lint]\nshell = 'true'\nthen = 'pr'",
        ),
    )
    .unwrap();
    fs::write(dir.join("answer.json"), r#"{"outcome":"red"}"#).unwrap();
    fs::write(
        dir.join("checks/lint.sh"),
        format!(
            "cp '{}' '{}'\ncat '{}'\n",
            edited.display(),
            dir.join("workflow.toml").display(),
            dir.join("answer.json").display()
        ),
    )
    .unwrap();
    let output = command(
        &env,
        &["workflow", "run", "review", "--at", "lint", "--once"],
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stdout}{stderr}");
    let id = id_of(&stdout);
    assert_names_finished_work_once(&env, &id, &stdout);
    assert_eq!(
        stderr.trim_end(),
        format!("error: workflow run {id} ended failed, not tried")
    );
    let record = finished_record(&env, &id);
    assert_eq!(
        (&record["once"], &record["state"]),
        (&json!(true), &json!("failed"))
    );
}

fn signal(child: &std::process::Child, signal: &str) {
    let sent = std::process::Command::new("kill")
        .args([signal, &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
}

/// A first pass returns as soon as a command attempt is published, so the try can end and its
/// home can move on either side of the receipt's work-dir line.
#[test]
fn a_try_names_its_finished_work_once_whether_its_home_moves_before_or_after_the_receipt() {
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;

    for moved_first in [true, false] {
        let (env, dir) = tries();
        let answer = dir.join("answer.json");
        let go = dir.join("go");
        fs::write(&answer, r#"{"outcome":"red"}"#).unwrap();
        fs::write(
            dir.join("checks/lint.sh"),
            format!(
                "while [ ! -e '{}' ]; do sleep 0.02; done\ncat '{}'\n",
                go.display(),
                answer.display()
            ),
        )
        .unwrap();
        let socket = dir.join("work-dir.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = env
            .rimz()
            .args(["workflow", "run", "review", "--at", "lint", "--once"])
            .env("RIMZ_TEST_WORKFLOW_BEFORE_WORK_DIR", &socket)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let id = lines.next().unwrap().unwrap();
        let accept_by = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut held = loop {
            if let Ok((stream, _)) = listener.accept() {
                break stream;
            }
            assert!(
                std::time::Instant::now() < accept_by,
                "the receipt never held"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let state = env.state_path_for(&env.project_root);
        let run = id.parse().unwrap();
        let mut stdout = format!("{id}\n");
        if moved_first {
            // Stopped, the command cannot reach the hold's limit however long the move takes.
            signal(&child, "-STOP");
            fs::write(&go, "").unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while !state.finished_workflow(&run).work.is_dir() {
                assert!(std::time::Instant::now() < deadline, "the home never moved");
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        held.write_all(&[1]).unwrap();
        if moved_first {
            signal(&child, "-CONT");
        } else {
            for line in lines.by_ref() {
                let line = line.unwrap();
                stdout.push_str(&line);
                stdout.push('\n');
                if line.starts_with("→ ") {
                    break;
                }
            }
            let live = state.workflow(&run).work;
            assert!(
                stdout.contains(&format!("{}\n", live.display())),
                "{stdout}"
            );
            fs::write(&go, "").unwrap();
        }
        for line in lines {
            stdout.push_str(&line.unwrap());
            stdout.push('\n');
        }
        assert!(child.wait().unwrap().success(), "{stdout}");
        assert_names_finished_work_once(&env, &id, &stdout);
    }
}

#[test]
fn a_try_and_a_plain_run_start_beside_each_other_but_a_mid_graph_start_is_refused() {
    let (env, _dir) = tries();
    let plain = start(&env);
    until(&env, &plain, |value| value["record"]["wait"].is_object());
    let (try_id, mut child, _lines) = live_try(&env);
    let refused = command(&env, &["workflow", "run", "review", "--at", "lint"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("workflow review already has run {plain} for ")),
        "{stderr}"
    );
    let listed: Value =
        serde_json::from_str(&success(command(&env, &["workflow", "list", "--json"]))).unwrap();
    let once = |id: &str| {
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .map(|row| row.get("once").cloned())
    };
    assert_eq!(
        (once(&plain), once(&try_id)),
        (Some(None), Some(Some(json!(true))))
    );
    stop(&env, &try_id, &mut child);
}

#[test]
fn an_unknown_start_node_refuses_and_leaves_no_home() {
    let (env, _dir) = tries();
    let output = command(
        &env,
        &["workflow", "run", "review", "--at", "nowhere", "--once"],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert_eq!(stderr.trim_end(), "error: unknown workflow node nowhere");
    let state = env.state_path_for(&env.project_root);
    assert_eq!(
        (
            state.workflow_runs().unwrap_or_default().len(),
            state.finished_workflow_runs().unwrap_or_default().len()
        ),
        (0, 0)
    );
}

#[test]
fn a_malformed_input_is_a_usage_error_naming_the_flag() {
    let (env, _dir) = tries();
    for (input, why) in [
        ("[1]", "expected one JSON object"),
        (
            r#"{"key":"a"}"#,
            "expected an object with a string `outcome`",
        ),
        (
            r#"{"outcome":1}"#,
            "expected an object with a string `outcome`",
        ),
        (
            r#"{"outcome":"red","key":[1]}"#,
            "expected a `key` that is a string or an integer",
        ),
    ] {
        let output = command(&env, &["workflow", "run", "review", "--input", input]);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{input}: {stderr}");
        assert!(
            stderr.contains(&format!(
                "invalid value '{input}' for '--input <OBJECT>': {why}"
            )),
            "{input}: {stderr}"
        );
    }
}

#[test]
fn resume_at_refuses_a_live_try_that_an_interrupted_run_once_left_running() {
    let (env, _dir) = tries();
    let (id, mut child, _lines) = live_try(&env);
    let refused = command(&env, &["workflow", "resume", &id, "--at", "pr"]);
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert_eq!(refused.status.code(), Some(1), "{stderr}");
    assert_eq!(
        stderr.trim_end(),
        format!(
            "error: workflow run {id} is a try, which ends at its node and cannot be moved; \
             cancel it with `rimz workflow cancel {id}`"
        )
    );
    let value = show(&env, &id);
    assert_eq!(
        (&value["record"]["state"], &value["record"]["node"]),
        (&json!("active"), &json!("slow"))
    );
    stop(&env, &id, &mut child);
}

#[test]
fn a_bare_address_selects_the_real_run_and_the_try_answers_to_the_command_it_prints() {
    let (env, _dir) = tries();
    let plain = start(&env);
    until(&env, &plain, |value| value["record"]["wait"].is_object());
    let (try_id, mut child, receipt) = live_try(&env);
    let shown = receipt
        .iter()
        .find_map(|line| line.strip_prefix("→ rimz workflow show "))
        .expect("the receipt prints its show command")
        .to_owned();
    assert_eq!(shown, try_id);
    let human = success(command(&env, &["workflow", "show", &shown]));
    assert!(human.contains(" · once"), "{human}");
    let address = show(&env, &plain)["address"].as_str().unwrap().to_owned();
    assert_eq!(show(&env, &address)["record"]["id"], json!(plain));
    stop(&env, &try_id, &mut child);
}

/// A `run --once` at `slow`, blocking on the try, with the try's id and its receipt lines so
/// far: the try stays live until it is canceled.
fn live_try(env: &Env) -> (String, std::process::Child, Vec<String>) {
    let mut child = env
        .rimz()
        .args(["workflow", "run", "review", "--at", "slow", "--once"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let id = lines.next().unwrap().unwrap();
    assert!(id.starts_with("wf_"), "{id}");
    let mut receipt = vec![id.clone()];
    for line in lines.by_ref() {
        let line = line.unwrap();
        let done = line.starts_with("→ ");
        receipt.push(line);
        if done {
            break;
        }
    }
    // The reader stays with the child, so the waiter never writes into a closed pipe.
    std::thread::spawn(move || lines.for_each(drop));
    until(env, &id, |value| {
        value["record"]["attempt"]["child"]["pid"].is_number()
    });
    (id, child, receipt)
}

/// Interrupt the blocking `run --once` as Ctrl-C would, see the try still live, and cancel it.
fn stop(env: &Env, id: &str, child: &mut std::process::Child) {
    let signaled = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(signaled.success());
    child.wait().unwrap();
    assert_eq!(show(env, id)["record"]["state"], json!("active"));
    success(command(env, &["workflow", "cancel", id]));
    settled(env, id, "canceled");
}
