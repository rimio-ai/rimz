use std::fs;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::harness::block::digest;

const META: &str = "export const meta = { name: 'try', description: 'A script under test' }\n";

fn script(body: &str) -> String {
    format!("{META}{body}")
}

fn pass<'a>(text: &'a str, log: &'a Log) -> Pass<'a> {
    Pass {
        name: "workflow.js",
        text,
        args: None,
        log,
        caps: Caps::PASS,
        dir: None,
    }
}

/// Replay `body` over hand-written answers.
#[track_caller]
fn answered(body: &str, answers: Vec<Outcome>) -> Replay {
    let text = script(body);
    let log = Log::Answers(answers);
    replay(&pass(&text, &log)).unwrap_or_else(|error| panic!("{error}"))
}

#[track_caller]
fn ran(body: &str) -> End {
    answered(body, Vec::new()).end
}

fn result(value: Value) -> Outcome {
    Outcome::Result(value)
}

fn exit(code: i32) -> Outcome {
    Outcome::Failure(Failure {
        code: Some(code),
        stdout: Some("red\n".to_owned()),
        output: Some("/tmp/gate.log".to_owned()),
        ..pass_failure(Cause::Exit, format!("the command exited {code}"), None)
    })
}

/// The log a run of `body` over `answers` would have recorded, in call order.
#[track_caller]
fn recorded(body: &str, answers: Vec<Outcome>) -> Vec<Line> {
    let replay = answered(body, answers.clone());
    replay
        .calls
        .iter()
        .zip(answers)
        .map(|(call, outcome)| Line {
            number: call.number,
            block: call.block,
            digest: call.digest.clone(),
            outcome,
        })
        .collect()
}

#[track_caller]
fn first(body: &str, log: Vec<Line>, open_at_end: &[u64]) -> Option<u64> {
    let text = script(body);
    let log = Log::Recorded(log);
    first_to_run(&pass(&text, &log), open_at_end).unwrap_or_else(|error| panic!("{error}"))
}

#[track_caller]
fn refused(text: &str) -> String {
    let log = Log::Answers(Vec::new());
    match replay(&pass(text, &log)) {
        Ok(replay) => panic!("loaded, and ended {:?}", replay.end),
        Err(reason) => reason,
    }
}

fn returned(value: Value) -> End {
    End::Returned(value)
}

#[test]
fn weak_references_do_not_exist() {
    assert_eq!(
        ran("return [typeof WeakRef, typeof FinalizationRegistry]"),
        returned(json!(["undefined", "undefined"]))
    );
}

#[test]
fn weak_collections_do_not_exist() {
    assert_eq!(
        ran("return [typeof WeakMap, typeof WeakSet, typeof Map, typeof Set]"),
        returned(json!(["undefined", "undefined", "function", "function"]))
    );
}

#[test]
fn timers_do_not_exist() {
    assert_eq!(
        ran("return [typeof setTimeout, typeof setInterval, typeof setImmediate]"),
        returned(json!(["undefined", "undefined", "undefined"]))
    );
}

#[test]
fn no_host_module_reaches_the_file_system_network_or_process() {
    assert_eq!(
        ran(
            "return [typeof std, typeof os, typeof bjson, typeof require, typeof process, typeof fetch, typeof performance]"
        ),
        returned(json!([
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined",
            "undefined"
        ]))
    );
}

#[test]
fn a_dynamic_import_finds_no_module_loader() {
    assert_eq!(
        ran("try { await import('./checks/gate.js') } catch (error) { return String(error) }"),
        returned(json!(
            "ReferenceError: could not load module 'checks/gate.js'"
        ))
    );
}

#[test]
fn an_import_statement_is_refused_with_its_line() {
    assert_eq!(
        refused(&script("const a = 1\nimport { b } from './b.js'\nreturn b")),
        "line 3: a workflow script has no imports: its globals are the blocks, `pipeline`, `parallel`, `file`, and `args`"
    );
}

#[test]
fn a_second_export_is_refused_with_its_line() {
    assert_eq!(
        refused(&script("export const other = 1")),
        "line 2: a workflow script exports only `meta`, in its first statement"
    );
}

#[test]
fn the_clock_throws() {
    let no_clock =
        "a workflow script has no clock: take the time from `args`, or from a `shell` command";
    assert_eq!(
        ran(
            "const tries = [() => Date.now(), () => new Date(), () => Date(), () => Date(0), \
             () => new (new Date(0).constructor)()]\n\
             return tries.map(clock => { try { clock(); return 'ran' } catch (error) { return error.message } })"
        ),
        returned(json!([no_clock, no_clock, no_clock, no_clock, no_clock]))
    );
}

#[test]
fn a_date_from_a_value_works() {
    assert_eq!(
        ran(
            "const day = new Date(0)\nreturn [day.toISOString(), day instanceof Date, Date.UTC(1970, 0, 2)]"
        ),
        returned(json!(["1970-01-01T00:00:00.000Z", true, 86_400_000]))
    );
}

#[test]
fn the_time_zone_is_refused() {
    let zone = |sentence: &str| format!("a workflow script has no time zone: {sentence}");
    let text = zone("write a date as text with `toISOString` or `toUTCString`");
    let parts = zone("build a date from its parts with `new Date(Date.UTC(year, month, ...))`");
    let tries = [
        (
            "day.getHours()",
            zone("use `getUTCHours` in place of `getHours`"),
        ),
        ("day.getDay()", zone("use `getUTCDay` in place of `getDay`")),
        (
            "day.getFullYear()",
            zone("use `getUTCFullYear` in place of `getFullYear`"),
        ),
        (
            "day.setMinutes(5)",
            zone("use `setUTCMinutes` in place of `setMinutes`"),
        ),
        (
            "day.setDate(2)",
            zone("use `setUTCDate` in place of `setDate`"),
        ),
        (
            "day.getTimezoneOffset()",
            zone("every date is read in UTC, which has no offset"),
        ),
        ("day.toString()", text.clone()),
        ("day.toDateString()", text.clone()),
        ("day.toTimeString()", text.clone()),
        ("day.toLocaleString()", text.clone()),
        ("day.toLocaleDateString()", text.clone()),
        ("day.toLocaleTimeString()", text.clone()),
        ("String(day)", text.clone()),
        ("`${day}`", text.clone()),
        ("'at ' + day", text.clone()),
        ("new Date(2026, 0, 1)", parts.clone()),
        ("new Date(2026, 0)", parts.clone()),
        (
            "new Date('2026-01-01T00:00')",
            zone("give '2026-01-01T00:00' its zone, such as `Z` or `+02:00`"),
        ),
        (
            "Date.parse('2026-01-01T00:00')",
            zone("give '2026-01-01T00:00' its zone, such as `Z` or `+02:00`"),
        ),
        (
            "new Date('Jan 1 2026')",
            zone("give 'Jan 1 2026' its zone, such as `Z` or `+02:00`"),
        ),
        (
            "new Date({ valueOf: () => 0 })",
            "a date is built from milliseconds, a string with its zone, or another date".to_owned(),
        ),
    ];
    let body = tries
        .iter()
        .map(|(expression, _)| format!("() => {expression}"))
        .collect::<Vec<_>>()
        .join(", ");
    let expected: Vec<&str> = tries
        .iter()
        .map(|(_, sentence)| sentence.as_str())
        .collect();
    assert_eq!(
        ran(&format!(
            "const day = new Date(0)\n\
             return [{body}].map(zoned => {{ try {{ zoned(); return 'ran' }} catch (error) {{ return error.message }} }})"
        )),
        returned(json!(expected))
    );
}

#[test]
fn a_date_in_utc_works() {
    assert_eq!(
        ran("const day = new Date(Date.UTC(2026, 0, 1, 9))\n\
             const copy = new Date(day)\n\
             copy.setUTCHours(10)\n\
             copy.setTime(copy.getTime() + 1000)\n\
             return [day.getUTCHours(), day.getTime(), day.valueOf(), day.toISOString(), day.toJSON(), \
             day.toUTCString(), JSON.stringify({ day }), copy - day, copy > day, copy.getUTCSeconds(), \
             Date.parse('2026-01-01'), new Date('2026-01-01T00:00Z').getTime(), \
             Date.parse('2026-01-01T09:00:00+09:00'), new Date('Thu, 01 Jan 2026 00:00:00 GMT').getTime()]"),
        returned(json!([
            9,
            1_767_258_000_000_u64,
            1_767_258_000_000_u64,
            "2026-01-01T09:00:00.000Z",
            "2026-01-01T09:00:00.000Z",
            "Thu, 01 Jan 2026 09:00:00 GMT",
            "{\"day\":\"2026-01-01T09:00:00.000Z\"}",
            3_601_000,
            true,
            1,
            1_767_225_600_000_u64,
            1_767_225_600_000_u64,
            1_767_225_600_000_u64,
            1_767_225_600_000_u64,
        ]))
    );
}

#[test]
fn randomness_throws() {
    assert_eq!(
        ran("Math.random()"),
        End::Threw(
            "a workflow script has no randomness: take a seed from `args` (workflow.js:2)"
                .to_owned()
        )
    );
}

#[test]
fn the_time_cap_stops_an_endless_loop() {
    let text = script("await shell({ command: 'true' })\nwhile (true) {}");
    let log = Log::Answers(vec![result(
        json!({"code": 0, "stdout": "", "output": "/tmp/log"}),
    )]);
    let caps = Caps {
        time: Duration::from_millis(200),
        ..Caps::PASS
    };
    let replay = replay(&Pass {
        caps,
        ..pass(&text, &log)
    })
    .unwrap();
    assert_eq!(
        replay.end,
        End::Failed(pass_failure(
            Cause::Invalid,
            "the script ran past the 0.2s time cap of one pass".to_owned(),
            None
        ))
    );
}

#[test]
fn the_memory_cap_stops_an_allocating_loop() {
    let text = script("const kept = []\nwhile (true) kept.push('x'.repeat(4096) + kept.length)");
    let log = Log::Answers(Vec::new());
    let caps = Caps {
        memory: 16 << 20,
        ..Caps::PASS
    };
    let replay = replay(&Pass {
        caps,
        ..pass(&text, &log)
    })
    .unwrap();
    assert_eq!(
        replay.end,
        End::Failed(pass_failure(
            Cause::Invalid,
            "the script passed the 16 MiB memory cap of one pass".to_owned(),
            None
        ))
    );
}

#[test]
fn meta_is_read_without_running_the_body() {
    let loaded = load::load(&script("while (true) {}")).unwrap();
    assert_eq!(
        loaded.meta,
        Meta {
            name: "try".to_owned(),
            description: "A script under test".to_owned()
        }
    );
    assert_eq!(loaded.body.lines().count(), 2, "{}", loaded.body);
    assert!(loaded.body.starts_with(&" ".repeat(70)), "{}", loaded.body);
}

#[test]
fn meta_takes_comments_before_it_and_a_literal_of_any_quote() {
    let loaded = load::load(
        "// land: a header\n/* a block\n comment */\nexport const meta = {\n  name: \"land\",\n  'description': `Lands a PR\\u0021`, // why\n};\nreturn 1\n",
    )
    .unwrap();
    assert_eq!(loaded.meta.description, "Lands a PR!");
    assert_eq!(
        loaded.body.lines().nth(7),
        Some("return 1"),
        "{}",
        loaded.body
    );
}

#[test]
fn a_missing_meta_is_refused_with_its_line() {
    assert_eq!(
        refused("// a header\n\nconst meta = { name: 'a', description: 'b' }\n"),
        "line 3: a workflow script starts with `export const meta = { name, description }`"
    );
}

#[test]
fn a_meta_that_is_not_a_literal_is_refused_with_its_line() {
    assert_eq!(
        refused("\nexport const meta = {\n  name: 'a',\n  description: describe() }\n"),
        "line 4: `meta` is a plain literal of strings, numbers, booleans, null, arrays, and objects, and `describe()` is not one"
    );
    assert_eq!(
        refused("export const meta = { name: 'a', description: `${x}` }\n"),
        "line 1: `meta` is a plain literal of strings, numbers, booleans, null, arrays, and objects, and `${x}`` is not one"
    );
    assert_eq!(
        refused("export const meta = { name: 'a' }\n"),
        "line 1: `meta`: missing field `description`"
    );
}

#[test]
fn an_error_carries_the_script_files_line() {
    assert_eq!(
        ran("\n\nthrow new Error('still red after three fixes')"),
        End::Threw("still red after three fixes (workflow.js:4)".to_owned())
    );
    assert_eq!(
        ran("const pr = null\nreturn pr.outcome"),
        End::Threw("TypeError: cannot read property 'outcome' of null (workflow.js:3)".to_owned())
    );
}

#[test]
fn args_and_files_reach_the_script() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("prompts")).unwrap();
    fs::write(dir.path().join("prompts/fix.md"), "Fix it.").unwrap();
    let text = script(
        "const read = path => { try { return file(path) } catch (error) { return error.message } }\n\
         return [args.base, read('./prompts/fix.md'), read('../outside.md'), read('/etc/hostname')]",
    );
    let log = Log::Answers(Vec::new());
    let args = json!({"base": "origin/main"});
    let replay = replay(&Pass {
        args: Some(&args),
        dir: Some(dir.path()),
        ..pass(&text, &log)
    })
    .unwrap();
    let leaves = |path: &str| {
        format!(
            "`file('{path}')` leaves the workflow's directory: name a file inside it, such as `prompts/fix.md`"
        )
    };
    assert_eq!(
        replay.end,
        returned(json!([
            "origin/main",
            "Fix it.",
            leaves("../outside.md"),
            leaves("/etc/hostname")
        ]))
    );
    assert_eq!(
        ran("try { file('prompts/fix.md') } catch (error) { return error.message }"),
        returned(json!(
            "a workflow that is one file has no directory for `file` to read"
        ))
    );
}

const LANES: &str = "const lanes = await pipeline(['a', 'b', 'c'],\n\
    lane => agent({ profile: 'opus', stay: false, prompt: 'build ' + lane }),\n\
    (built, lane) => shell({ command: 'check ' + lane }))\n\
    return lanes";

#[test]
fn a_second_replay_makes_the_same_calls_with_the_same_numbers() {
    let answers = vec![
        result(json!({"report": "a", "handle": "opus"})),
        result(json!({"report": "b", "handle": "opus-2"})),
        result(json!({"report": "c", "handle": "opus-3"})),
        result(json!({"code": 0, "stdout": "a ok", "output": "/tmp/a"})),
    ];
    let one = answered(LANES, answers.clone());
    let two = answered(LANES, answers);
    assert_eq!(one.calls, two.calls);
    let made: Vec<(u64, &str, &str)> = one
        .calls
        .iter()
        .map(|call| {
            let what = call
                .object
                .get("prompt")
                .or_else(|| call.object.get("command"));
            (
                call.number,
                call.block.as_str(),
                what.and_then(Value::as_str).unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        made,
        [
            (1, "agent", "build a"),
            (2, "agent", "build b"),
            (3, "agent", "build c"),
            (4, "shell", "check a"),
            (5, "shell", "check b"),
            (6, "shell", "check c"),
        ]
    );
    assert_eq!((one.open, one.used, one.end), (vec![5, 6], 4, End::Pending));
}

#[test]
fn pipeline_leaves_null_for_a_failed_lane_and_the_others_go_on() {
    let replay = answered(
        LANES,
        vec![
            result(json!({"report": "a", "handle": "opus"})),
            Outcome::Failure(pass_failure(Cause::Died, "gone".to_owned(), None)),
            result(json!({"report": "c", "handle": "opus-3"})),
            result(json!({"code": 0, "stdout": "a ok", "output": "/tmp/a"})),
            result(json!({"code": 0, "stdout": "c ok", "output": "/tmp/c"})),
        ],
    );
    assert_eq!(
        replay.end,
        returned(json!([
            {"code": 0, "stdout": "a ok", "output": "/tmp/a"},
            null,
            {"code": 0, "stdout": "c ok", "output": "/tmp/c"},
        ]))
    );
}

#[test]
fn parallel_leaves_null_for_a_failed_task_and_the_others_go_on() {
    let replay = answered(
        "return parallel([() => shell({ command: 'a' }), () => shell({ command: 'b' }), () => { throw new Error('no') }])",
        vec![
            exit(1),
            result(json!({"code": 0, "stdout": "b", "output": "/tmp/b"})),
        ],
    );
    assert_eq!(
        replay.end,
        returned(json!([null, {"code": 0, "stdout": "b", "output": "/tmp/b"}, null]))
    );
}

#[test]
fn promise_all_throws_the_first_failure_and_the_other_calls_go_on() {
    let replay = answered(
        "try { await Promise.all([shell({ command: 'a' }), shell({ command: 'b' }), shell({ command: 'c' })]) }\n\
         catch (failure) { return [failure.step, failure.cause] }",
        vec![
            result(json!({"code": 0, "stdout": "", "output": "/tmp/a"})),
            exit(2),
        ],
    );
    assert_eq!(
        (replay.end, replay.open),
        (returned(json!([2, "exit"])), vec![3])
    );
}

#[test]
fn a_caught_failure_is_handed_back_as_it_was() {
    assert_eq!(
        answered(
            "try { await shell({ command: 'cargo xtask check' }) } catch (failure) {\n\
             return [failure instanceof Error, failure.message, JSON.parse(JSON.stringify(failure)), { ...failure }.code] }",
            vec![exit(3)],
        )
        .end,
        returned(json!([true, "the command exited 3", {
            "block": "shell", "cause": "exit", "reason": "the command exited 3", "step": 1,
            "code": 3, "stdout": "red\n", "output": "/tmp/gate.log",
        }, 3]))
    );
}

#[test]
fn an_uncaught_failure_ends_the_run_failed_with_it() {
    let Outcome::Failure(failure) = exit(1) else {
        unreachable!("exit builds a failure");
    };
    assert_eq!(
        answered(
            "await shell({ command: 'a' })\nreturn 'green'",
            vec![exit(1)]
        )
        .end,
        End::Failed(Failure {
            block: Some(BlockName::Shell),
            step: Some(1),
            ..failure
        })
    );
}

#[test]
fn a_failed_call_never_awaited_ends_the_run_failed() {
    let replay = answered(
        "shell({ command: 'a' })\nawait shell({ command: 'b' })\nreturn 'done'",
        vec![
            exit(1),
            result(json!({"code": 0, "stdout": "", "output": "/tmp/b"})),
        ],
    );
    assert!(
        matches!(&replay.end, End::Failed(failure) if failure.step == Some(1) && failure.cause == Cause::Exit),
        "{:?}",
        replay.end
    );
}

#[test]
fn a_script_that_awaits_what_no_call_settles_is_stuck() {
    assert_eq!(
        ran("await new Promise(() => {})"),
        End::Failed(pass_failure(
            Cause::Stuck,
            "the script awaits something no call will settle".to_owned(),
            None
        ))
    );
}

#[test]
fn a_call_that_differs_from_the_log_is_replay() {
    let body = "await shell({ command: 'a' })\nawait shell({ command: 'b' })";
    let text = script(body);
    let mut lines = recorded(body, vec![result(json!(1)), result(json!(2))]);
    lines[1].digest = digest(BlockName::Shell, &json!({"command": "B"}));
    let log = Log::Recorded(lines);
    assert_eq!(
        replay(&pass(&text, &log)).unwrap().end,
        End::Failed(pass_failure(
            Cause::Replay,
            "call 2 passes `shell` another object than the log's".to_owned(),
            Some(2)
        ))
    );
    let mut lines = recorded(body, vec![result(json!(1))]);
    lines[0].block = BlockName::Agent;
    let log = Log::Recorded(lines);
    assert_eq!(
        replay(&pass(&text, &log)).unwrap().end,
        End::Failed(pass_failure(
            Cause::Replay,
            "call 1 is `shell` here and `agent` in the log".to_owned(),
            Some(1)
        ))
    );
}

#[test]
fn a_refused_call_fails_with_invalid_and_takes_no_number() {
    let replay = answered(
        "const tried = []\n\
         for (const call of [() => agent({ profile: 'opus', prompt: 'x', colour: 'red' }),\n\
                             () => shell({ command: 'a', on: 'fail' }),\n\
                             () => workflow({ source: 'name', value: 'land' })]) {\n\
           try { await call() } catch (failure) { tried.push([failure.block, failure.cause, failure.step, failure.reason]) }\n\
         }\n\
         return tried",
        Vec::new(),
    );
    assert_eq!(replay.calls, []);
    let End::Returned(tried) = replay.end else {
        panic!("{:?}", replay.end);
    };
    assert_eq!(
        tried[1],
        json!([
            "shell",
            "invalid",
            null,
            "a script awaits its calls and owns its run, so its `shell` does not take `on`"
        ])
    );
    assert_eq!(
        tried[2],
        json!([
            "workflow",
            "invalid",
            null,
            "a workflow called from a script runs in its caller's run, which is not built yet"
        ])
    );
    assert!(
        tried[0][3]
            .as_str()
            .is_some_and(|reason| reason.starts_with("unknown field `colour`")),
        "{tried}"
    );
}

#[test]
fn a_hand_written_failure_may_leave_out_its_reason() {
    assert_eq!(
        answers(
            "{\"result\": {\"outcome\": \"sent\"}}\n\n{\"failure\": {\"cause\": \"exit\", \"code\": 1}}\n"
        ),
        Ok(vec![
            result(json!({"outcome": "sent"})),
            Outcome::Failure(Failure {
                code: Some(1),
                ..pass_failure(
                    Cause::Exit,
                    "the answers file fails this call with `exit`".to_owned(),
                    None
                )
            }),
        ])
    );
    let refused = answers("{\"result\": 1}\n{\"failure\": {\"cause\": \"late\"}}").unwrap_err();
    assert!(
        refused.starts_with("line 2: unknown variant `late`"),
        "{refused}"
    );
}

const STEPS: &str =
    "await shell({ command: 'a' })\nawait shell({ command: 'b' })\nawait shell({ command: 'c' })";

#[test]
fn the_first_call_to_run_is_the_first_that_differs() {
    let log = recorded(
        STEPS,
        vec![result(json!(1)), result(json!(2)), result(json!(3))],
    );
    let edited = STEPS.replace("'b'", "'B'");
    assert_eq!(first(&edited, log, &[]), Some(2));
}

#[test]
fn the_first_call_to_run_is_one_still_open() {
    let body = "const [a, b, c] = [shell({ command: 'a' }), shell({ command: 'b' }), shell({ command: 'c' })]\n\
                return [await a, await b, await c]";
    let mut log = recorded(
        body,
        vec![result(json!(1)), result(json!(2)), result(json!(3))],
    );
    log.remove(1);
    assert_eq!(first(body, log, &[2]), Some(2));
}

#[test]
fn the_first_call_to_run_is_the_one_whose_failure_ended_the_run() {
    let log = recorded(STEPS, vec![result(json!(1)), exit(1)]);
    assert_eq!(first(STEPS, log, &[]), Some(2));
}

#[test]
fn a_caught_failure_keeps_its_answer_on_a_resume() {
    let body = "try { await shell({ command: 'a' }) } catch {}\nawait shell({ command: 'b' })\nawait shell({ command: 'c' })";
    let log = recorded(body, vec![exit(1), result(json!(2))]);
    assert_eq!(first(body, log, &[3]), Some(3));
}

#[test]
fn a_null_keeps_its_place_on_a_resume() {
    let body = "await pipeline(['a'], lane => shell({ command: lane }))\nawait shell({ command: 'b' })\nawait shell({ command: 'c' })";
    let log = recorded(body, vec![exit(1), result(json!(2))]);
    assert_eq!(first(body, log, &[3]), Some(3));
}

/// A replay of `body` under small caps: a 300 ms pass and a 16 MiB heap.
#[track_caller]
fn capped(body: &str) -> End {
    let text = script(body);
    let log = Log::Answers(Vec::new());
    let caps = Caps {
        time: Duration::from_millis(300),
        memory: 16 << 20,
    };
    replay(&Pass {
        caps,
        ..pass(&text, &log)
    })
    .unwrap_or_else(|error| panic!("{error}"))
    .end
}

fn time_capped() -> End {
    End::Failed(pass_failure(
        Cause::Invalid,
        "the script ran past the 0.3s time cap of one pass".to_owned(),
        None,
    ))
}

fn memory_capped() -> End {
    End::Failed(pass_failure(
        Cause::Invalid,
        "the script passed the 16 MiB memory cap of one pass".to_owned(),
        None,
    ))
}

const ALLOCATE: &str = "const kept = []; while (true) kept.push('x'.repeat(4096) + kept.length)";

#[test]
fn a_cap_hit_in_a_host_conversion_ends_the_pass_invalid_whatever_the_script_caught() {
    let call = |work: &str| {
        format!(
            "try {{ await shell({{ command: 'a', toJSON() {{ {work} }} }}) }} catch (error) {{ return 'caught ' + error.message }}"
        )
    };
    let returned = |work: &str| format!("return {{ toJSON() {{ {work} }} }}");
    assert_eq!(
        [
            capped(&call("while (true) {}")),
            capped(&returned("while (true) {}")),
            capped(&call(ALLOCATE)),
            capped(&returned(ALLOCATE)),
        ],
        [
            time_capped(),
            time_capped(),
            memory_capped(),
            memory_capped()
        ]
    );
}

#[test]
fn a_memory_cap_hit_ends_the_pass_invalid_whatever_the_script_caught_and_did_after() {
    assert_eq!(
        [
            capped(&format!(
                "try {{ {ALLOCATE} }} catch {{}}\nreturn 'went on'"
            )),
            capped(&format!(
                "try {{ {ALLOCATE} }} catch {{}}\nthrow new Error('something else')"
            )),
            capped(&format!(
                "return await pipeline([1, 2], lane => {{ if (lane === 1) {{ {ALLOCATE} }} return 'ok' }})"
            )),
            capped(&format!(
                "return await parallel([() => {{ {ALLOCATE} }}, () => 'ok'])"
            )),
        ],
        [
            memory_capped(),
            memory_capped(),
            memory_capped(),
            memory_capped()
        ]
    );
}

#[test]
fn a_scripts_own_out_of_memory_error_is_thrown_as_any_error() {
    assert_eq!(
        capped("throw new InternalError('out of memory')"),
        End::Threw("InternalError: out of memory (workflow.js:2)".to_owned())
    );
}

#[test]
fn no_script_action_reaches_the_native_date_or_the_host_zone() {
    let zone = |text: &str| {
        format!(
            "a workflow script has no time zone: give '{text}' its zone, such as `Z` or `+02:00`"
        )
    };
    let other = "a date is built from milliseconds, a string with its zone, or another date";
    let tries = [
        (
            "Reflect.construct = (target) => target; return String(new Date(0).constructor === Date)",
            "true".to_owned(),
        ),
        (
            "Reflect.construct = (target) => new target(); return String(new Date(0).getTime() !== 0)",
            "false".to_owned(),
        ),
        (
            "Reflect.apply = () => 0; return String(new Date(new Date(5)).getTime())",
            "5".to_owned(),
        ),
        ("new Date(' 1970-01-01 ')", zone(" 1970-01-01 ")),
        ("Date.parse(' 1970-01-01 ')", zone(" 1970-01-01 ")),
        (
            "new Date(Object.setPrototypeOf({ [Symbol.toPrimitive]() { return '2024-01-01T10:00' } }, Date.prototype))",
            other.to_owned(),
        ),
        ("new Date(new Proxy(new Date(0), {}))", other.to_owned()),
        (
            "RegExp.prototype.test = () => true; RegExp.prototype.exec = () => [''];\nnew Date('2024-01-01T10:00')",
            zone("2024-01-01T10:00"),
        ),
        (
            "String.prototype.trim = () => '1970-01-01Z'; String.prototype.replace = () => '1970-01-01Z';\nnew Date('2024-01-01T10:00')",
            zone("2024-01-01T10:00"),
        ),
        ("new Date('+2026')", zone("+2026")),
        ("new Date('2026-01-01 (UTC)')", zone("2026-01-01 (UTC)")),
        ("new Date('Jan 1 2026 +0000')", zone("Jan 1 2026 +0000")),
        (
            "Date.parse('2026-01-01T10:00' + ' '.repeat(130) + 'Z')",
            zone(&format!("2026-01-01T10:00{}Z", " ".repeat(130))),
        ),
        (
            "Date.parse({ toString: () => '1970-01-01' })",
            "a date is parsed from a string with its zone".to_owned(),
        ),
        (
            "const day = new Date(0); Function.prototype.call = () => 0; return String(day - new Date(7))",
            "-7".to_owned(),
        ),
    ];
    let body = tries
        .iter()
        .map(|(statements, _)| format!("() => {{ {statements} }}"))
        .collect::<Vec<_>>()
        .join(",\n");
    let expected: Vec<&str> = tries
        .iter()
        .map(|(_, sentence)| sentence.as_str())
        .collect();
    assert_eq!(
        ran(&format!(
            "return [{body}].map(probe => {{ try {{ return probe() ?? 'ran' }} catch (error) {{ return error.message }} }})"
        )),
        returned(json!(expected))
    );
}

#[test]
fn a_date_string_with_its_zone_or_a_date_only_iso_form_works() {
    assert_eq!(
        ran(
            "return ['2026', '2026-01', '2026-01-01', '+002026-01-01', '2026-01-01T09:00Z', \
             '2026-01-01T09:00:30Z', '2026-01-01T09:00:30.5Z', '2026-01-01T09:00:30.125Z', \
             '2026-01-01T18:00+09:00', '2026-01-01T04:00:00-05:00', \
             'Thu, 01 Jan 2026 09:00:00 GMT'].map(text => Date.parse(text))"
        ),
        returned(json!([
            1_767_225_600_000_u64,
            1_767_225_600_000_u64,
            1_767_225_600_000_u64,
            1_767_225_600_000_u64,
            1_767_258_000_000_u64,
            1_767_258_030_000_u64,
            1_767_258_030_500_u64,
            1_767_258_030_125_u64,
            1_767_258_000_000_u64,
            1_767_258_000_000_u64,
            1_767_258_000_000_u64,
        ]))
    );
}

fn line(number: u64, command: &str, outcome: Outcome) -> Line {
    Line {
        number,
        block: BlockName::Shell,
        digest: digest(BlockName::Shell, &json!({ "command": command })),
        outcome,
    }
}

#[test]
fn an_earlier_call_answered_later_in_the_log_keeps_its_answer_on_a_resume() {
    let body = "const a = shell({ command: 'a' })\nawait shell({ command: 'b' })\nawait shell({ command: 'c' })\nreturn await a";
    let log = || {
        vec![
            line(2, "b", result(json!(2))),
            line(3, "c", result(json!(3))),
            line(1, "a", result(json!(1))),
        ]
    };
    assert_eq!(first(body, log(), &[]), None);
    assert_eq!(first(&body.replace("'c'", "'C'"), log(), &[]), Some(3));
}

#[test]
fn a_call_beside_an_edited_one_keeps_its_answer_on_a_resume() {
    let body = "const [a, b] = [shell({ command: 'a' }), shell({ command: 'b' })]\nreturn [await a, await b]";
    let log = vec![
        line(2, "b", result(json!(2))),
        line(1, "a", result(json!(1))),
    ];
    assert_eq!(first(&body.replace("'b'", "'B'"), log, &[]), Some(2));
}

#[test]
fn a_call_answered_after_the_failure_that_ended_the_run_keeps_its_answer() {
    let body = "const a = shell({ command: 'a' })\nawait shell({ command: 'b' })\nreturn await a";
    let log = vec![line(2, "b", exit(1)), line(1, "a", result(json!(1)))];
    assert_eq!(first(body, log, &[]), Some(2));
}

#[test]
fn a_line_for_a_call_the_script_has_not_made_is_replay_with_nothing_open() {
    let ended = |body: &str, log: Vec<Line>| {
        let text = script(body);
        let log = Log::Recorded(log);
        replay(&pass(&text, &log)).unwrap().end
    };
    let unmade = |number: u64| {
        End::Failed(pass_failure(
            Cause::Replay,
            format!("the log answers call {number}, which the script has not made"),
            Some(number),
        ))
    };
    assert_eq!(
        [
            ended(
                "await new Promise(() => {})",
                vec![line(1, "a", result(json!(1)))]
            ),
            ended(
                "await shell({ command: 'a' })\nawait new Promise(() => {})",
                vec![
                    line(1, "a", result(json!(1))),
                    line(3, "c", result(json!(3)))
                ],
            ),
            ended(
                "return await shell({ command: 'a' })",
                vec![
                    line(1, "A", result(json!(0))),
                    line(1, "a", result(json!(1)))
                ],
            ),
            ended(
                "return await Promise.race([shell({ command: 'a' }), shell({ command: 'b' })])",
                vec![
                    line(1, "a", result(json!(1))),
                    line(2, "b", result(json!(2)))
                ],
            ),
        ],
        [
            unmade(1),
            unmade(3),
            End::Failed(pass_failure(
                Cause::Replay,
                "the log answers call 1 twice".to_owned(),
                Some(1),
            )),
            returned(json!(1)),
        ]
    );
}

#[test]
fn an_answers_file_cannot_fail_a_call_with_a_cause_that_ends_a_run_without_one() {
    assert_eq!(
        [
            answers("{\"failure\": {\"cause\": \"replay\"}}"),
            answers("{\"result\": 1}\n{\"failure\": {\"cause\": \"stuck\"}}"),
        ],
        [
            Err("line 1: a call cannot fail with `replay`: a call fails with `exit`, `died`, `timeout`, `capacity`, or `invalid`".to_owned()),
            Err("line 2: a call cannot fail with `stuck`: a call fails with `exit`, `died`, `timeout`, `capacity`, or `invalid`".to_owned()),
        ]
    );
}

#[test]
fn the_meta_statement_ends_at_its_literal() {
    let meta = "export const meta = { name: 'w', description: 'd' }";
    let follows = |line: usize, token: &str| {
        format!(
            "line {line}: the `meta` statement ends at its literal, so `{token}` cannot continue it: end it with `;` or a new line"
        )
    };
    assert_eq!(
        [
            refused(&format!("{meta} + 1; return 7")),
            refused(&format!("{meta} return 2")),
            refused(&format!("{meta} /* x */ + 1")),
            refused(&format!("{meta}\n+ 1; return 7")),
            refused(&format!("{meta}\n(globalThis.x = 5); return x")),
            refused(&format!("{meta} // note\n  instanceof Object")),
            refused(&format!("{meta}\n\n`tag`")),
            refused(&format!("{meta}\n!= 1")),
        ],
        [
            follows(1, "+"),
            follows(1, "return"),
            follows(1, "+"),
            follows(2, "+"),
            follows(2, "("),
            follows(2, "instanceof"),
            follows(3, "`"),
            follows(2, "!="),
        ]
    );
    for text in [
        format!("{meta}; return 7"),
        format!("{meta}\nreturn 7"),
        format!("{meta}  \nreturn 7"),
        format!("{meta}\r\nreturn 7"),
        format!("{meta} // note\nreturn 7"),
        format!("{meta} /* note */\nreturn 7"),
        format!("{meta} /* note */; return 7"),
        format!("{meta}\n// note\n; return 7"),
        format!("{meta}\ninner: for (;;) {{ break inner }}\nreturn 7"),
        format!("{meta}\nif (true) return 7"),
    ] {
        let log = Log::Answers(Vec::new());
        assert_eq!(
            replay(&pass(&text, &log)).unwrap().end,
            returned(json!(7)),
            "{text}"
        );
    }
}

#[test]
fn a_meta_nested_past_the_bound_is_refused_with_its_line() {
    let deep = "[".repeat(1_000_000);
    assert_eq!(
        refused(&format!("\nexport const meta = {deep}")),
        "line 2: `meta` nests deeper than 64 levels"
    );
}

#[test]
fn a_name_that_starts_with_import_is_not_read_as_an_import() {
    let reason = refused(&script("imported = ;"));
    assert!(
        reason.starts_with("line 2: ") && !reason.contains("imports"),
        "{reason}"
    );
}
