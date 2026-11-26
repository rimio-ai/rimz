//! `rimz workflow try`: the four example workflows walked over answers written by hand, and the
//! refusals and caps of the script engine in the real binary.

use super::*;

/// Run `rimz workflow try` on a fixture workflow over one of its `tries/` answers files.
fn walk(name: &str, args: Option<&str>, answers: &str) -> (Vec<String>, String) {
    let env = Env::new();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/workflows")
        .join(name);
    let mut command = env.rimz();
    command.arg("workflow").arg("try").arg(&dir);
    if let Some(args) = args {
        command.args(["--args", args]);
    }
    let output = command
        .arg("--answers")
        .arg(dir.join("tries").join(format!("{answers}.jsonl")))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = stdout
        .lines()
        .filter(|line| line.starts_with(|char: char| char.is_ascii_digit()))
        .map(str::to_owned)
        .collect();
    let end = stdout.lines().last().unwrap_or_default().to_owned();
    (calls, end)
}

/// The condition land's `FORGE` watches.
const FORGE: &str = "pr=merged,closed || (pr=open && pr.queue!=queued && (pr.queue=dequeued || pr.mergeable=conflicting || ci=failed || (ci=passed && pr.queue=none)))";

#[test]
fn land_repairs_a_red_gate_and_lands() {
    let (calls, end) = walk("land", None, "red-gate");
    let calls: Vec<String> = calls
        .into_iter()
        .map(|call| call.replace(FORGE, "FORGE"))
        .collect();
    assert_eq!(
        calls,
        [
            "1 shell uv run --script checks/team.py close",
            "2 shell uv run --script checks/send.py",
            "3 watch FORGE",
            "4 agent fixer",
            "5 shell uv run --script checks/send.py resend",
            "6 watch FORGE",
            "7 shell cargo clean",
            "8 shell uv run --script checks/notes.py sift",
        ]
    );
    assert_eq!(end, r#"returns {"pr":"merged"}"#);
}

#[test]
fn land_lands_a_merged_pr() {
    let (calls, end) = walk("land", None, "clean");
    assert_eq!(calls.len(), 5, "{calls:?}");
    assert_eq!(end, r#"returns {"pr":"merged"}"#);
}

#[test]
fn ralph_replans_out_of_an_item() {
    let (calls, end) = walk("ralph", Some(r#"{"goal":"/goals/tokens"}"#), "replan");
    assert_eq!(
        calls,
        [
            "1 shell uv run --script checks/ralph.py todo",
            "2 shell uv run --script checks/ralph.py start",
            "3 agent opus",
            "4 shell uv run --script checks/ralph.py gate",
            "5 agent judge",
            "6 shell uv run --script checks/ralph.py todo",
            "7 agent fable",
            "8 shell uv run --script checks/ralph.py todo",
            "9 agent judge",
            "10 agent opus",
        ]
    );
    assert_eq!(end, r#"returns {"outcome":"complete"}"#);
}

#[test]
fn ralph_reports_a_built_goal() {
    let (calls, end) = walk("ralph", Some(r#"{"goal":"/goals/tokens"}"#), "clean");
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert_eq!(end, r#"returns {"outcome":"complete"}"#);
}

#[test]
fn steward_halts_one_lane_and_the_other_goes_on() {
    let (calls, end) = walk(
        "steward",
        Some(r#"{"job":"/jobs/rename","batch":1}"#),
        "halted-lane",
    );
    assert_eq!(
        calls,
        [
            "1 shell uv run --script checks/steward.py lanes",
            "2 agent opus",
            "3 agent opus",
            "4 agent judge",
            "5 shell uv run --script checks/steward.py gate",
            "6 shell uv run --script checks/steward.py halt",
            "7 shell uv run --script checks/steward.py due",
            "8 agent opus",
        ]
    );
    assert_eq!(
        end,
        r#"returns {"halted":true,"lanes":[{"lane":"l1","units":{"u1":"halted"}},{"lane":"l2","units":{"u3":"landed"}}]}"#
    );
}

#[test]
fn steward_lands_every_unit() {
    let (calls, end) = walk(
        "steward",
        Some(r#"{"job":"/jobs/rename","batch":1}"#),
        "clean",
    );
    assert_eq!(calls.len(), 5, "{calls:?}");
    assert_eq!(
        end,
        r#"returns {"halted":false,"lanes":[{"lane":"l1","units":{"u1":"landed"}}]}"#
    );
}

#[test]
fn review_change_merges_the_reviews_that_answered() {
    let (calls, end) = walk(
        "review-change",
        Some(r#"{"base":"origin/main"}"#),
        "failed-reviewer",
    );
    assert_eq!(
        calls,
        [
            "1 agent judge (correctness)",
            "2 agent judge (tests)",
            "3 agent judge (docs)",
            "4 agent judge (merge)",
            "5 agent opus",
            "6 shell cargo xtask check",
            "7 agent opus",
            "8 shell cargo xtask check",
        ]
    );
    assert_eq!(
        end,
        r#"returns {"blocking":["a test misses the empty case"],"found":"fix"}"#
    );
}

#[test]
fn review_change_returns_a_clean_review() {
    let (calls, end) = walk("review-change", Some(r#"{"base":"origin/main"}"#), "clean");
    assert_eq!(calls.len(), 4, "{calls:?}");
    assert_eq!(end, r#"returns {"blocking":[],"found":"clean"}"#);
}

fn try_script(env: &Env, text: &str) -> std::process::Output {
    let script = env.project_root.join("script.js");
    fs::write(&script, text).unwrap();
    env.rimz()
        .args(["workflow", "try"])
        .arg(&script)
        .output()
        .unwrap()
}

/// The release allocator (jemalloc on Linux) also serves the engine's heap, whose cap holds.
#[test]
fn the_memory_cap_stops_an_allocating_script_in_the_binary() {
    let env = Env::new();
    let output = try_script(
        &env,
        "export const meta = { name: 'grow', description: 'Allocates until a cap stops it' }\n\
         const kept = []\nwhile (true) kept.push('x'.repeat(65536) + kept.length)\n",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert_eq!(
        stdout,
        "grow: Allocates until a cap stops it\nfails: invalid: the script passed the 256 MiB memory cap of one pass\n"
    );
}

#[test]
fn a_script_that_cannot_load_exits_nonzero_with_its_line() {
    let env = Env::new();
    let output = try_script(
        &env,
        "export const meta = { name: 'bad', description: 'Imports' }\nimport x from './x.js'\n",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.ends_with(
            "script.js: line 2: a workflow script has no imports: its globals are the blocks, `pipeline`, `parallel`, `file`, and `args`\n"
        ),
        "{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn types_prints_the_declarations_of_a_scripts_globals() {
    let env = Env::new();
    let output = env.rimz().args(["workflow", "types"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.starts_with(
            "// The globals of a RimZ workflow script, printed by `rimz workflow types`.\n"
        ) && stdout.contains("declare function shell(block: Shell): Promise<ShellResult>;\n"),
        "{stdout}"
    );
}

fn try_with(env: &Env, args: &[&str]) -> std::process::Output {
    let script = env.project_root.join("script.js");
    fs::write(
        &script,
        "export const meta = { name: 'echo', description: 'Returns its args' }\nreturn args\n",
    )
    .unwrap();
    env.rimz()
        .args(["workflow", "try"])
        .arg(&script)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn try_takes_any_json_value_as_its_args() {
    let env = Env::new();
    let output = try_with(&env, &["--args", "-1"]);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout, "echo: Returns its args\nreturns -1\n");
}

#[test]
fn try_refuses_args_that_are_not_json_as_a_usage_error() {
    let env = Env::new();
    let output = try_with(&env, &["--args", "{goal"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.ends_with("`--args` is not JSON: key must be a string at line 1 column 2\n"),
        "{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn try_exits_nonzero_for_an_answers_file_it_cannot_read() {
    let env = Env::new();
    let missing = env.project_root.join("missing.jsonl");
    let output = try_with(&env, &["--answers", missing.to_str().unwrap()]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("reading {}", missing.display()))
            && stderr.contains("No such file or directory"),
        "{stderr}"
    );
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
