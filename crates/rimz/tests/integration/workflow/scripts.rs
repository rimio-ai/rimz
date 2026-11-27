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

/// The condition `cases` watches for land's `FORGE`: an outcome case while the queued case, which
/// waits, does not read true.
const FORGE: &str = "(pr=merged) || (pr=closed) || (pr=open && pr.queue=dequeued) || ((pr=open && pr.mergeable=conflicting) && !(pr=open && pr.queue=queued)) || ((pr=open && ci=failed) && !(pr=open && pr.queue=queued)) || ((pr=open && ci=passed && pr.queue=none) && !(pr=open && pr.queue=queued))";

/// The condition land's hold watches: the PR no longer reads unqueued, its case false or a case
/// before it true.
const BROKEN: &str = "!((pr=open && ci=passed && pr.queue=none) && !((pr=merged) || (pr=closed) || (pr=open && pr.queue=dequeued) || (pr=open && pr.queue=queued) || (pr=open && pr.mergeable=conflicting) || (pr=open && ci=failed)))";

/// A walk of land with its two long conditions named.
fn land(answers: &str) -> (Vec<String>, String) {
    let (calls, end) = walk("land", None, answers);
    let calls = calls
        .into_iter()
        .map(|call| call.replace(BROKEN, "BROKEN").replace(FORGE, "FORGE"))
        .collect();
    (calls, end)
}

#[test]
fn land_repairs_a_red_gate_and_lands() {
    let (calls, end) = land("red-gate");
    assert_eq!(
        calls,
        [
            "1 shell uv run --script checks/team.py close",
            "2 shell uv run --script checks/send.py",
            "3 now",
            "4 watch FORGE (timeout 259200s)",
            "5 agent fixer",
            "6 shell uv run --script checks/send.py resend",
            "7 now",
            "8 watch FORGE (timeout 259200s)",
            "9 shell cargo clean",
            "10 shell uv run --script checks/notes.py sift",
        ]
    );
    assert_eq!(end, r#"returns {"pr":"merged"}"#);
}

/// A green PR that stays unqueued through the hold counts once the forge reads it again. Each
/// round's watches share its 3-day deadline, and the hold gets its whole 30 minutes.
#[test]
fn land_holds_a_green_pr_then_reads_the_forge_again_before_the_fixer_goes() {
    let (calls, end) = land("held");
    assert_eq!(
        calls,
        [
            "1 shell uv run --script checks/team.py close",
            "2 shell uv run --script checks/send.py",
            "3 now",
            "4 watch FORGE (timeout 259200s)",
            "5 now",
            "6 watch BROKEN (timeout 30m)",
            "7 now",
            "8 watch FORGE (timeout 257340s)",
            "9 agent fixer",
            "10 shell uv run --script checks/send.py resend",
            "11 now",
            "12 watch FORGE (timeout 259200s)",
            "13 shell cargo clean",
            "14 shell uv run --script checks/notes.py sift",
        ]
    );
    assert_eq!(end, r#"returns {"pr":"merged"}"#);
}

/// A PR merged mid-hold can still read green and unqueued, so the hold breaks on the earlier
/// case and no fixer goes to the merged PR.
#[test]
fn land_sends_no_fixer_to_a_pr_merged_mid_hold() {
    let (calls, end) = land("merged-mid-hold");
    assert_eq!(
        calls,
        [
            "1 shell uv run --script checks/team.py close",
            "2 shell uv run --script checks/send.py",
            "3 now",
            "4 watch FORGE (timeout 259200s)",
            "5 now",
            "6 watch BROKEN (timeout 30m)",
            "7 now",
            "8 watch FORGE (timeout 258480s)",
            "9 shell cargo clean",
            "10 shell uv run --script checks/notes.py sift",
        ]
    );
    assert_eq!(end, r#"returns {"pr":"merged"}"#);
}

/// A hold that breaks on a red CI reads the forge from the top, and the fixer goes to the red PR.
#[test]
fn land_repairs_a_pr_whose_hold_broke_on_a_red_ci() {
    let (calls, end) = land("broken-then-red");
    assert_eq!(
        calls,
        [
            "1 shell uv run --script checks/team.py close",
            "2 shell uv run --script checks/send.py",
            "3 now",
            "4 watch FORGE (timeout 259200s)",
            "5 now",
            "6 watch BROKEN (timeout 30m)",
            "7 now",
            "8 watch FORGE (timeout 258660s)",
            "9 agent fixer",
            "10 shell uv run --script checks/send.py resend",
            "11 now",
            "12 watch FORGE (timeout 259200s)",
            "13 shell cargo clean",
            "14 shell uv run --script checks/notes.py sift",
        ]
    );
    assert_eq!(end, r#"returns {"pr":"merged"}"#);
}

#[test]
fn land_lands_a_merged_pr() {
    let (calls, end) = walk("land", None, "clean");
    assert_eq!(calls.len(), 6, "{calls:?}");
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
            "script.js: line 2: a workflow script has no imports: its globals are the blocks, `pipeline`, `parallel`, `cases`, `file`, `ms`, `reads`, and `args`\n"
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

/// The clock is a call the answers file answers, and the call answers run out at gets a template
/// of its answer: its result's fields, a watch's keys unread, and the failure form.
#[test]
fn try_answers_now_and_offers_a_template_for_the_call_it_waits_on() {
    let env = Env::new();
    let script = env.project_root.join("script.js");
    fs::write(
        &script,
        "export const meta = { name: 'wait', description: 'Waits on a PR' }\n\
         const since = await now()\n\
         return [since, await watch({ when: 'pr=merged || ci=failed', timeout: '1h' })]\n",
    )
    .unwrap();
    let answers = env.project_root.join("answers.jsonl");
    fs::write(&answers, "{\"result\":1764268800000}\n").unwrap();
    let output = env
        .rimz()
        .args(["workflow", "try"])
        .arg(&script)
        .arg("--answers")
        .arg(&answers)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout,
        "wait: Waits on a PR\n\
         1 now\n  answer 1764268800000\n\
         2 watch pr=merged || ci=failed (timeout 1h)\n  no answer\n\
         waits on call 2, the first with no answer; answer it with a line like one of these:\n\
         \x20 {\"result\":{\"readings\":{\"ci\":null,\"pr\":null}}}\n\
         \x20 {\"failure\":{\"cause\":\"...\"}}\n"
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
