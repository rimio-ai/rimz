use std::fs;
use std::rc::Rc;
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
        reader: None,
        covered: None,
        steps: None,
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
    let replayed = replay(&pass(&text, &log)).unwrap_or_else(|error| panic!("{error}"));
    first_to_run(&replayed, &log, open_at_end)
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
        "line 3: a workflow script has no imports: its globals are the blocks, `pipeline`, `parallel`, `cases`, `file`, `ms`, `reads`, `phase`, `log`, and `args`"
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
        "a workflow script has no clock: `await now()` gives the time, which the run's log keeps";
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
fn now_is_a_call_the_log_answers_and_takes_no_object() {
    let replay = answered(
        "const at = await now()\n\
         const tried = []\n\
         for (const call of [() => now({}), () => now(5)]) {\n\
           try { await call() } catch (failure) { tried.push([failure.block, failure.cause, failure.step, failure.reason]) }\n\
         }\n\
         return [at, tried]",
        vec![result(json!(1_764_268_800_000_u64))],
    );
    let refused = json!([
        "now",
        "invalid",
        null,
        "a `now` call takes no object: call `now()`"
    ]);
    assert_eq!(
        (replay.calls, replay.end),
        (
            vec![Call {
                number: 1,
                block: BlockName::Now,
                object: json!({}),
                digest: digest(BlockName::Now, &json!({})),
                parent: None,
                phase: None,
            }],
            returned(json!([1_764_268_800_000_u64, [refused, refused]]))
        )
    );
}

#[test]
fn ms_reads_the_duration_grammar_of_a_timeout_and_makes_no_call() {
    let replay = answered(
        "const thrown = text => { try { return ms(text) } catch (error) { return error.message } }\n\
         return [ms('90s'), ms('30m'), ms('2h'), ms('3d'), ms('0s'), thrown('366d'), thrown('')]",
        Vec::new(),
    );
    assert_eq!(
        (replay.calls, replay.end),
        (
            Vec::new(),
            returned(json!([
                90_000,
                1_800_000,
                7_200_000,
                259_200_000,
                0,
                "`ms`: duration `366d` is longer than the 365d maximum",
                "`ms`: duration is empty",
            ]))
        )
    );
}

#[test]
fn reads_decides_a_condition_on_the_watchs_three_valued_walk_and_makes_no_call() {
    let replay = answered(
        "const read = { ci: 'passed', pr: null }\n\
         const thrown = (when, readings) => { try { return reads(when, readings) } catch (error) { return error.message } }\n\
         return [reads('ci=passed', read), reads('pr=open || ci=passed', read),\n\
           reads('pr=open && ci=failed', read), reads('!pr=open', read), reads('pr=open', {}),\n\
           reads('pr=merged,closed || (pr=open && ci=failed)', { pr: 'closed' }),\n\
           thrown('pr=', read), thrown('pr=open', { pr: 1 })]",
        Vec::new(),
    );
    assert_eq!(
        (replay.calls, replay.end),
        (
            Vec::new(),
            returned(json!([
                true,
                true,
                false,
                null,
                null,
                true,
                "`reads`: clause 1, position 4: empty term; use key=value",
                "`reads`: the readings are not an object of strings and nulls: invalid type: integer `1`, expected a string",
            ]))
        )
    );
}

#[test]
fn phase_and_log_make_no_call_and_each_call_carries_its_phase() {
    let text = script(
        "const said = log('starting')\n\
         phase('Gate')\n\
         const gate = await shell({ command: 'gate' })\n\
         phase('Gate')\n\
         phase('Fix')\n\
         log(`the gate exited ${gate.code}`)\n\
         await shell({ command: 'fix' })\n\
         return [said, phase('Done')]",
    );
    let log = Log::Answers(vec![result(json!({"code": 3})), result(json!({"code": 0}))]);
    // The marks a pass made before the last line are those an earlier pass over one line made.
    let replay = replay(&Pass {
        covered: Some(1),
        ..pass(&text, &log)
    })
    .unwrap();
    let titles: Vec<_> = replay
        .marks
        .iter()
        .filter_map(|mark| match &mark.progress {
            Progress::Phase(title) => Some(title.as_ptr()),
            Progress::Log(_) => None,
        })
        .collect();
    // A call shares its title with the phase's mark, whose charge covers it, and copies nothing.
    assert_eq!(
        replay
            .calls
            .iter()
            .map(|call| call.phase.as_deref().map(str::as_ptr))
            .collect::<Vec<_>>(),
        vec![Some(titles[0]), Some(titles[1])]
    );
    let call = |number, command: &str, phase: &str| {
        (
            number,
            digest(BlockName::Shell, &json!({ "command": command })),
            Some(phase.into()),
        )
    };
    let mark = |after, replayed, progress| Mark {
        after,
        replayed,
        progress,
    };
    let phase = |title: &str| Progress::Phase(title.into());
    let log = |line: &str| Progress::Log(line.to_owned());
    assert_eq!(
        (
            replay
                .calls
                .into_iter()
                .map(|call| (call.number, call.digest, call.phase))
                .collect::<Vec<_>>(),
            replay.marks,
            replay.end
        ),
        (
            vec![call(1, "gate", "Gate"), call(2, "fix", "Fix")],
            vec![
                mark(0, true, log("starting")),
                mark(0, true, phase("Gate")),
                mark(1, true, phase("Fix")),
                mark(1, true, log("the gate exited 3")),
                mark(2, false, phase("Done")),
            ],
            returned(json!([null, null]))
        )
    );
}

#[test]
fn a_renamed_phase_keeps_the_logs_answers() {
    let body = "phase('Gate')\nawait shell({ command: 'a' })\nphase('Fix')\nreturn await shell({ command: 'b' })";
    let text = script(&body.replace("'Fix'", "'Repair'"));
    let log = Log::Recorded(recorded(body, vec![result(json!(1)), result(json!(2))]));
    let replay = replay(&pass(&text, &log)).unwrap();
    assert_eq!((replay.used, replay.end), (2, returned(json!(2))));
}

/// Lines arrive in the order calls settle, so a mark is told new by the lines handed back before
/// it, not by the calls made before it.
#[test]
fn a_mark_is_replayed_when_the_runs_last_pass_covered_the_lines_before_it() {
    let body = "const a = shell({ command: 'a' })\n\
                await shell({ command: 'b' })\n\
                log('b settled')\n\
                await a\n\
                log('a settled')";
    let marks = |lines: Vec<Line>, covered| {
        let text = script(body);
        let log = Log::Recorded(lines);
        replay(&Pass {
            covered,
            ..pass(&text, &log)
        })
        .unwrap()
        .marks
    };
    let settled = |line: &str, replayed| Mark {
        after: 2,
        replayed,
        progress: Progress::Log(line.to_owned()),
    };
    let both = || {
        vec![
            line(2, "b", result(json!(2))),
            line(1, "a", result(json!(1))),
        ]
    };
    assert_eq!(
        [
            marks(vec![line(2, "b", result(json!(2)))], None),
            marks(vec![line(2, "b", result(json!(2)))], Some(0)),
            marks(both(), Some(1)),
            // Two lines arrived since a pass over none: neither mark was made before.
            marks(both(), Some(0)),
            marks(both(), Some(2)),
        ],
        [
            vec![settled("b settled", false)],
            vec![settled("b settled", false)],
            vec![settled("b settled", true), settled("a settled", false)],
            vec![settled("b settled", false), settled("a settled", false)],
            vec![settled("b settled", true), settled("a settled", true)],
        ]
    );
}

#[test]
fn phase_and_log_take_text() {
    let replay = answered(
        "const thrown = mark => { try { mark() } catch (error) { return error.message } }\n\
         return [thrown(() => phase()), thrown(() => phase(3)), thrown(() => log({ line: 'a' }))]",
        Vec::new(),
    );
    assert_eq!(
        (replay.marks, replay.end),
        (
            Vec::new(),
            returned(json!([
                "`phase` takes its title as text",
                "`phase` takes its title as text",
                "`log` takes its line as text",
            ]))
        )
    );
}

/// A PR's cases as a script hands them to `cases`: a queued PR waits, and a green one counts once
/// it has stayed green half an hour.
const PR_CASES: &str = "const PR = [\n\
  { outcome: 'merged', when: 'pr=merged' },\n\
  { when: 'pr.queue=queued' },\n\
  { outcome: 'red', when: 'ci=failed' },\n\
  { outcome: 'green', when: 'ci=passed', for: '30m' },\n\
]\n";

/// What `cases` over `PR_CASES` watches: an outcome case while no waiting case before it reads
/// true.
const PR_ANY: &str =
    "(pr=merged) || ((ci=failed) && !(pr.queue=queued)) || ((ci=passed) && !(pr.queue=queued))";

/// What the hold of the green case watches: green no longer the first case that reads true.
const PR_NOT_GREEN: &str = "!((ci=passed) && !((pr=merged) || (pr.queue=queued) || (ci=failed)))";

fn readings(pr: &str, queue: &str, ci: &str) -> Outcome {
    result(json!({ "readings": { "ci": ci, "pr": pr, "pr.queue": queue } }))
}

fn timed_out() -> Outcome {
    Outcome::Failure(pass_failure(
        Cause::Timeout,
        "the watch ran out its timeout".to_owned(),
        None,
    ))
}

/// The objects of the calls a replay made, and how it ended.
fn objects(replay: Replay) -> (Vec<Value>, End) {
    let objects = replay.calls.into_iter().map(|call| call.object).collect();
    (objects, replay.end)
}

#[test]
fn cases_watches_each_outcome_case_behind_the_waiting_cases_before_it() {
    let replay = answered(
        &format!("{PR_CASES}return await cases(PR, {{ label: 'the PR', cwd: '/work' }})"),
        vec![result(
            json!({ "readings": { "ci": "failed", "pr": null, "pr.queue": "none" } }),
        )],
    );
    assert_eq!(
        objects(replay),
        (
            vec![json!({ "when": PR_ANY, "label": "the PR", "cwd": "/work" })],
            returned(json!({
                "outcome": "red",
                "readings": { "ci": "failed", "pr": null, "pr.queue": "none" }
            }))
        )
    );
    let queued = answered(
        &format!(
            "{PR_CASES}try {{ return await cases(PR) }} catch (error) {{ return error.message }}"
        ),
        vec![readings("open", "queued", "failed")],
    );
    assert_eq!(
        queued.end,
        returned(json!(
            "`cases`: the watch fired on readings no outcome case reads true on"
        ))
    );
}

/// A held case counts only when its hold ran out and the look after it still has it win: a PR
/// merged mid-hold breaks the hold though it still reads green, and a case that wins the look
/// after a kept hold counts in its place.
#[test]
fn cases_counts_a_held_case_once_it_held_and_still_wins() {
    let walk = |answers: Vec<Outcome>| {
        objects(answered(
            &format!("{PR_CASES}return (await cases(PR)).outcome"),
            answers,
        ))
    };
    let any = json!({ "when": PR_ANY });
    let hold = json!({ "when": PR_NOT_GREEN, "timeout": "30m" });
    let green = || readings("open", "none", "passed");
    assert_eq!(
        walk(vec![green(), timed_out(), green()]),
        (
            vec![any.clone(), hold.clone(), any.clone()],
            returned(json!("green"))
        )
    );
    assert_eq!(
        walk(vec![
            green(),
            readings("merged", "none", "passed"),
            readings("merged", "none", "passed")
        ]),
        (
            vec![any.clone(), hold.clone(), any.clone()],
            returned(json!("merged"))
        )
    );
    assert_eq!(
        walk(vec![
            green(),
            timed_out(),
            readings("open", "none", "failed")
        ]),
        (
            vec![any.clone(), hold.clone(), any.clone()],
            returned(json!("red"))
        )
    );
    assert_eq!(
        walk(vec![
            green(),
            readings("open", "none", "failed"),
            readings("open", "none", "failed")
        ]),
        (vec![any.clone(), hold, any], returned(json!("red")))
    );
}

/// `timeout` is one deadline for the whole call: each watch gets the time left, read with a `now`
/// call before it and rounded up to the second, and a hold longer than the time left watches that
/// long and fails the call with the watch's timeout. A script that replaces the globals and
/// methods `cases` uses changes none of this.
#[test]
fn cases_gives_each_watch_the_time_left_before_its_deadline() {
    let body = format!(
        "{PR_CASES}globalThis.watch = globalThis.now = globalThis.ms = globalThis.reads = () => {{ throw new Error('replaced') }}\n\
         Math.ceil = () => 0\n\
         try {{ return (await cases(PR, {{ timeout: '1h' }})).outcome }} catch (failure) {{ return [failure.cause, failure.step] }}"
    );
    let at = 1_764_268_800_000_u64;
    let walk = |answers: Vec<Outcome>| objects(answered(&body, answers));
    let green = || readings("open", "none", "passed");
    let any = |timeout: &str| json!({ "when": PR_ANY, "timeout": timeout });
    let hold = |timeout: &str| json!({ "when": PR_NOT_GREEN, "timeout": timeout });
    assert_eq!(
        walk(vec![
            result(json!(at)),
            green(),
            result(json!(at + 1_000_500)),
            timed_out(),
            result(json!(at + 2_800_250)),
            green(),
        ]),
        (
            vec![
                json!({}),
                any("3600s"),
                json!({}),
                hold("30m"),
                json!({}),
                any("800s")
            ],
            returned(json!("green"))
        )
    );
    assert_eq!(
        walk(vec![
            result(json!(at)),
            green(),
            result(json!(at + 3_000_000)),
            timed_out(),
        ]),
        (
            vec![json!({}), any("3600s"), json!({}), hold("600s")],
            returned(json!(["timeout", 4]))
        )
    );
}

#[test]
fn cases_refuses_a_list_it_cannot_decide_before_any_call() {
    let tries = [
        ("[]", "`cases`: takes a list of one or more cases"),
        ("'ci=failed'", "`cases`: takes a list of one or more cases"),
        (
            "[{ when: 'ci=pending' }]",
            "`cases`: no case has an `outcome`, so none can win",
        ),
        (
            "[{ when: 'ci=pending', for: '5m' }, { outcome: 'red', when: 'ci=failed' }]",
            "`cases`: case 1 has `for` but no `outcome`: a waiting case has nothing to hold",
        ),
        (
            "[{ outcome: 'red', when: 'ci=failed', fro: '5m' }]",
            "`cases`: case 1 takes `when`, `outcome`, and `for`, not `fro`",
        ),
        (
            "[{ outcome: 'red', when: 'ci=failed' }, { outcome: 'green' }]",
            "`cases`: case 2 has no `when` condition",
        ),
        ("[null]", "`cases`: case 1 has no `when` condition"),
        (
            "[{ outcome: 'red', when: 'ci=' }]",
            "`cases`: case 1 `when`: clause 1, position 4: empty term; use key=value",
        ),
        (
            "[{ outcome: 'red', when: 'ci=failed', for: '366d' }]",
            "`cases`: case 1 `for`: duration `366d` is longer than the 365d maximum",
        ),
        (
            "[{ outcome: 'red', when: 'ci=failed', for: 5 }]",
            "`cases`: case 1 `for`: a duration is text, such as `30m`",
        ),
        (
            "[{ outcome: 'red', when: 'ci=failed' }], { timout: '3d' }",
            "`cases`: the options take `label`, `cwd`, and `timeout`, not `timout`",
        ),
        (
            "[{ outcome: 'red', when: 'ci=failed' }], { timeout: '' }",
            "`cases`: `timeout`: duration is empty",
        ),
    ];
    let body = tries
        .iter()
        .map(|(arguments, _)| {
            format!("async () => {{ try {{ await cases({arguments}) }} catch (error) {{ return error.message }} }}")
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let replay = answered(
        &format!(
            "Array.isArray = () => true; String.prototype.slice = () => ''\n\
             const tries = [{body}]\n\
             const messages = []\n\
             for (const one of tries) messages.push(await one())\n\
             return messages"
        ),
        Vec::new(),
    );
    let messages: Vec<&str> = tries.iter().map(|(_, message)| *message).collect();
    assert_eq!(
        (replay.calls, replay.end),
        (Vec::new(), returned(json!(messages)))
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
            "this walk of the script does not follow a call into another workflow"
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

/// A pass keeps its marks outside the engine's heap, so their text counts against the cap.
#[test]
fn the_memory_cap_counts_the_marks_a_pass_keeps() {
    let mark = "const line = 'x'.repeat(4096)\nwhile (true) log(line)";
    assert_eq!(
        [
            capped(mark),
            capped(&format!("try {{ {mark} }} catch {{}}\nreturn 'went on'")),
        ],
        [memory_capped(), memory_capped()]
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
            answers("{\"failure\": {\"cause\": \"launches\"}}"),
            answers("{\"failure\": {\"cause\": \"steps\"}}"),
        ],
        [
            Err("line 1: a call cannot fail with `replay`: a call fails with `exit`, `died`, `timeout`, `capacity`, or `invalid`".to_owned()),
            Err("line 2: a call cannot fail with `stuck`: a call fails with `exit`, `died`, `timeout`, `capacity`, or `invalid`".to_owned()),
            Err("line 1: a call cannot fail with `launches`: a call fails with `exit`, `died`, `timeout`, `capacity`, or `invalid`".to_owned()),
            Err("line 1: a call cannot fail with `steps`: a call fails with `exit`, `died`, `timeout`, `capacity`, or `invalid`".to_owned()),
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
        format!("{meta}\n++globalThis.count\nreturn 7"),
        format!("{meta}\n--globalThis.count\nreturn 7"),
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

/// A reader of called workflows: each `value` a script names, as (value, file name, text), and
/// any other value refused.
fn reading(scripts: &[(&str, &str, &str)]) -> Rc<Reader> {
    let scripts: Vec<(String, String, String)> = scripts
        .iter()
        .map(|(value, name, text)| ((*value).to_owned(), (*name).to_owned(), (*text).to_owned()))
        .collect();
    Rc::new(move |calling: &Calling<'_>| {
        if calling.source == WorkflowSource::Script {
            return Ok(Called {
                name: "script.js".to_owned(),
                text: calling.value.to_owned(),
                dir: None,
            });
        }
        scripts
            .iter()
            .find(|(value, _, _)| value == calling.value)
            .map(|(_, name, text)| Called {
                name: name.clone(),
                text: text.clone(),
                dir: None,
            })
            .ok_or_else(|| format!("no workflow is installed as `{}`", calling.value))
    })
}

/// Replay `body` over `lines` with `reader`.
#[track_caller]
fn called(body: &str, lines: Vec<Line>, reader: Rc<Reader>) -> Replay {
    let text = script(body);
    let log = Log::Recorded(lines);
    replay(&Pass {
        reader: Some(reader),
        ..pass(&text, &log)
    })
    .unwrap_or_else(|error| panic!("{error}"))
}

fn logged(number: u64, block: BlockName, object: Value, outcome: Outcome) -> Line {
    Line {
        number,
        block,
        digest: digest(block, &object),
        outcome,
    }
}

/// Each call's number, block, and parent.
fn made(replay: &Replay) -> Vec<(u64, BlockName, Option<u64>)> {
    replay
        .calls
        .iter()
        .map(|call| (call.number, call.block, call.parent))
        .collect()
}

const CHILD: &str = "export const meta = { name: 'child', description: 'A called workflow' }\n\
    const x = await shell({ command: 'x' })\n\
    const won = await cases([{ when: 'pr=merged', outcome: 'merged' }])\n\
    return [x, won.outcome]";

#[test]
fn a_called_workflows_calls_take_the_callers_numbers_and_name_their_parent() {
    let replay = called(
        "const a = await shell({ command: 'a' })\n\
         const b = await workflow({ source: 'name', value: 'child' })\n\
         return [a, b, await shell({ command: 'c' })]",
        vec![
            line(1, "a", result(json!(1))),
            line(3, "x", result(json!("x"))),
            logged(
                4,
                BlockName::Watch,
                json!({ "when": "(pr=merged)" }),
                result(json!({ "readings": { "pr": "merged" } })),
            ),
            line(5, "c", result(json!("c"))),
        ],
        reading(&[("child", "child.js", CHILD)]),
    );
    use BlockName::{Shell, Watch, Workflow};
    assert_eq!(
        made(&replay),
        [
            (1, Shell, None),
            (2, Workflow, None),
            (3, Shell, Some(2)),
            (4, Watch, Some(2)),
            (5, Shell, None),
        ]
    );
    assert_eq!(
        (replay.open, replay.end),
        (vec![], returned(json!([1, ["x", "merged"], "c"])))
    );
}

const LANE: &str = "export const meta = { name: 'lane', description: 'One lane' }\n\
    phase('checking ' + args.lane)\n\
    return await shell({ command: 'check ' + args.lane })";

#[test]
fn called_workflows_side_by_side_keep_their_numbers_and_phases_on_a_second_replay() {
    let body = "return await pipeline(['p', 'q'], lane => workflow({ source: 'name', value: 'lane', args: { lane } }))";
    let lines = || {
        vec![
            line(4, "check q", result(json!("q ok"))),
            line(2, "check p", result(json!("p ok"))),
        ]
    };
    let reader = reading(&[("lane", "lane.js", LANE)]);
    let one = called(body, lines(), Rc::clone(&reader));
    let two = called(body, lines(), reader);
    assert_eq!(one.calls, two.calls);
    let phases: Vec<_> = one
        .calls
        .iter()
        .map(|call| {
            (
                call.number,
                call.parent,
                call.phase.as_deref().map(str::to_owned),
            )
        })
        .collect();
    assert_eq!(
        phases,
        [
            (1, None, None),
            (2, Some(1), Some("checking p".to_owned())),
            (3, None, None),
            (4, Some(3), Some("checking q".to_owned())),
        ]
    );
    assert_eq!(one.end, returned(json!(["p ok", "q ok"])));
}

#[test]
fn a_called_body_settles_its_call_with_what_it_returns_or_throws() {
    let replay = called(
        "const got = await workflow({ source: 'script', value: 'return args.n + 1', args: { n: 1 } })\n\
         let thrown\n\
         try { await workflow({ source: 'script', value: \"throw { why: 'no' }\" }) } catch (value) { thrown = value }\n\
         return [got, thrown]",
        Vec::new(),
        reading(&[]),
    );
    assert_eq!(
        (made(&replay), replay.open, replay.end),
        (
            vec![
                (1, BlockName::Workflow, None),
                (2, BlockName::Workflow, None)
            ],
            vec![],
            returned(json!([2, { "why": "no" }]))
        )
    );
}

#[test]
fn a_called_body_reads_its_own_files_and_throws_at_its_own_line() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("brief.md"), "Brief.").unwrap();
    let files = dir.path().to_path_buf();
    let reader: Rc<Reader> = Rc::new(move |_: &Calling<'_>| {
        Ok(Called {
            name: "child.js".to_owned(),
            text: format!("{META}log(file('brief.md'))\nthrow new Error('boom')"),
            dir: Some(files.clone()),
        })
    });
    let replay = called(
        "await workflow({ source: 'path', value: 'child' })",
        Vec::new(),
        reader,
    );
    assert_eq!(replay.end, End::Threw("boom (child.js:3)".to_owned()));
    assert_eq!(
        replay.marks.first().map(|mark| &mark.progress),
        Some(&Progress::Log("Brief.".to_owned()))
    );
}

#[test]
fn a_called_workflow_that_cannot_be_read_fails_at_its_number() {
    let replay = called(
        "const tried = []\n\
         for (const value of ['ghost', 'bare', 'broken']) {\n\
           try { await workflow({ source: 'name', value }) } catch (failure) { tried.push([failure.block, failure.cause, failure.step, failure.reason]) }\n\
         }\n\
         return tried",
        Vec::new(),
        reading(&[
            ("bare", "bare.js", "return 1"),
            ("broken", "broken.js", &script("return 1 +* 2")),
        ]),
    );
    assert_eq!(replay.open, Vec::<u64>::new());
    let End::Returned(tried) = replay.end else {
        panic!("{:?}", replay.end);
    };
    assert_eq!(
        [tried[0].clone(), tried[1].clone()],
        [
            json!([
                "workflow",
                "invalid",
                1,
                "no workflow is installed as `ghost`"
            ]),
            json!([
                "workflow",
                "invalid",
                2,
                "bare.js: line 1: a workflow script starts with `export const meta = { name, description }`"
            ]),
        ]
    );
    assert!(
        tried[2][3]
            .as_str()
            .is_some_and(|reason| reason.starts_with("broken.js: line 2: ")),
        "{tried}"
    );
}

#[test]
fn a_line_answers_a_workflow_call_only_with_a_failure_before_its_body_ends() {
    let slow = "export const meta = { name: 'slow', description: 'Waits' }\n\
                await shell({ command: 'x' })\n\
                return 'late'";
    let quick = "export const meta = { name: 'quick', description: 'Returns' }\nreturn 'quick'";
    let call = |value: &str| json!({ "source": "name", "value": value, "timeout": "1h" });
    let body = |value: &str| {
        format!(
            "try {{ return await workflow({{ source: 'name', value: '{value}', timeout: '1h' }}) }}\n\
             catch (failure) {{ return [failure.cause, failure.step] }}"
        )
    };
    let timeout = Outcome::Failure(pass_failure(
        Cause::Timeout,
        "the workflow ran past its timeout".to_owned(),
        None,
    ));
    let reader = reading(&[("slow", "slow.js", slow), ("quick", "quick.js", quick)]);
    let timed = called(
        &body("slow"),
        vec![logged(
            1,
            BlockName::Workflow,
            call("slow"),
            timeout.clone(),
        )],
        Rc::clone(&reader),
    );
    assert_eq!(
        (made(&timed).len(), timed.open, timed.end),
        (2, vec![], returned(json!(["timeout", 1])))
    );
    let ended = called(
        "const got = await workflow({ source: 'name', value: 'quick', timeout: '1h' })\n\
         return [got, await shell({ command: 'y' })]",
        vec![logged(1, BlockName::Workflow, call("quick"), timeout)],
        Rc::clone(&reader),
    );
    let answered = called(
        &body("slow"),
        vec![logged(
            1,
            BlockName::Workflow,
            call("slow"),
            result(json!("late")),
        )],
        reader,
    );
    assert_eq!(
        [ended.end, answered.end],
        [
            End::Failed(pass_failure(
                Cause::Replay,
                "the log answers call 1, a workflow whose body has ended".to_owned(),
                Some(1),
            )),
            End::Failed(pass_failure(
                Cause::Replay,
                "the log answers call 1, a workflow, with a result: a workflow's body gives its result".to_owned(),
                Some(1),
            )),
        ]
    );
}

#[test]
fn a_step_left_open_under_a_settled_workflow_call_is_never_settled() {
    let replay = called(
        "return await workflow({ source: 'script', value: \"shell({ command: 'left' })\\nreturn 'done'\" })",
        Vec::new(),
        reading(&[]),
    );
    assert_eq!(
        (made(&replay).len(), replay.open, replay.end),
        (2, vec![], returned(json!("done")))
    );
}

#[test]
fn a_body_makes_no_call_once_its_workflow_call_has_settled() {
    let replay = called(
        "await workflow({ source: 'script', value: \"(async () => { await null; await null; await null; shell({ command: 'late' }) })()\\nreturn 'done'\" })\nawait new Promise(() => {})",
        Vec::new(),
        reading(&[]),
    );
    assert_eq!(
        (made(&replay).len(), replay.open, replay.end),
        (
            1,
            vec![],
            End::Failed(pass_failure(
                Cause::Stuck,
                "the script awaits something no call will settle".to_owned(),
                None
            ))
        )
    );
}

#[test]
fn a_script_waiting_only_on_a_workflow_call_is_stuck() {
    let replay = called(
        "await workflow({ source: 'script', value: 'await new Promise(() => {})' })",
        Vec::new(),
        reading(&[]),
    );
    assert_eq!(
        (replay.open, replay.end),
        (
            vec![1],
            End::Failed(pass_failure(
                Cause::Stuck,
                "the script awaits something no call will settle".to_owned(),
                None
            ))
        )
    );
}

#[test]
fn a_workflow_call_past_eight_deep_fails_at_its_number() {
    let deep = "export const meta = { name: 'deep', description: 'Calls itself' }\n\
                return await workflow({ source: 'name', value: 'deep' })";
    let replay = called(
        "return await workflow({ source: 'name', value: 'deep' })",
        Vec::new(),
        reading(&[("deep", "deep.js", deep)]),
    );
    assert_eq!(replay.calls.len(), 9);
    assert_eq!(
        replay.end,
        End::Failed(Failure {
            block: Some(BlockName::Workflow),
            ..pass_failure(
                Cause::Invalid,
                "call 9 would run a workflow 9 calls deep, past the bound of 8".to_owned(),
                Some(9),
            )
        })
    );
}

#[test]
fn the_call_past_the_step_cap_is_not_made_and_ends_the_pass() {
    let text = script(
        "try { for (let i = 0; i < 5; i++) await now() } catch { return 'caught' } finally { log('finally') }",
    );
    let log = Log::Answers(vec![result(json!(1)), result(json!(2)), result(json!(3))]);
    let replay = replay(&Pass {
        steps: Some(3),
        ..pass(&text, &log)
    })
    .unwrap();
    assert_eq!(
        (replay.calls.len(), replay.marks.len(), replay.end),
        (
            3,
            0,
            End::Failed(pass_failure(
                Cause::Steps,
                "the script would make call 4, past the run's step cap of 3".to_owned(),
                Some(4),
            ))
        )
    );
}

#[test]
fn the_first_call_to_run_is_never_an_open_workflow_call() {
    let text =
        script("await workflow({ source: 'script', value: \"await shell({ command: 'x' })\" })");
    let log = Log::Recorded(Vec::new());
    let first = |open_at_end: &[u64]| {
        let replayed = replay(&Pass {
            reader: Some(reading(&[])),
            ..pass(&text, &log)
        })
        .unwrap();
        first_to_run(&replayed, &log, open_at_end)
    };
    assert_eq!([first(&[]), first(&[1])], [Some(2), Some(2)]);
}

#[test]
fn a_line_for_a_call_under_a_workflow_call_that_settled_earlier_in_the_pass_is_consumed() {
    let body = "const got = await workflow({ source: 'script', value: 'const a = now()\\nconst b = now()\\nreturn await a' })\n\
                return [got, await now()]";
    let now = |number: u64, value: Value| logged(number, BlockName::Now, json!({}), result(value));
    let consumed = called(
        body,
        vec![now(2, json!(1)), now(3, json!(2)), now(4, json!(3))],
        reading(&[]),
    );
    assert_eq!(
        (consumed.used, consumed.open, consumed.end),
        (3, vec![], returned(json!([1, 3])))
    );
    let unmade = called(
        body,
        vec![now(2, json!(1)), now(3, json!(2)), now(5, json!(3))],
        reading(&[]),
    );
    assert_eq!(
        unmade.end,
        End::Failed(pass_failure(
            Cause::Replay,
            "the log answers call 5, which the script has not made".to_owned(),
            Some(5),
        ))
    );
}

#[test]
fn an_error_whose_stack_getter_logs_is_described() {
    assert_eq!(
        ran("const error = new Error('boom')\n\
             Object.defineProperty(error, 'stack', { get() { log('read'); return '' } })\n\
             throw error"),
        End::Threw("boom".to_owned())
    );
}

#[test]
fn a_called_script_whose_braces_do_not_balance_fails_at_its_number() {
    let replay = called(
        "const tried = []\n\
         for (const value of ['object', 'number']) {\n\
           try { await workflow({ source: 'name', value }) } catch (failure) { tried.push([failure.cause, failure.step, failure.reason]) }\n\
         }\n\
         return tried",
        Vec::new(),
        reading(&[
            ("object", "object.js", &script("} }); ({ a: function () {")),
            (
                "number",
                "number.js",
                &script("} }); (function () { return 1 || function () {"),
            ),
        ]),
    );
    let unbalanced = "the body's braces do not balance: it closes the function it runs in";
    assert_eq!(
        replay.end,
        returned(json!([
            ["invalid", 1, format!("object.js: {unbalanced}")],
            ["invalid", 2, format!("number.js: {unbalanced}")],
        ]))
    );
}

#[test]
fn a_line_for_a_dropped_workflow_call_answers_it_only_with_a_failure() {
    let body = "const got = await workflow({ source: 'script', value: \"const w = workflow({ source: 'script', value: 'return await now()' })\\nreturn 1\" })\n\
                return [got, await now()]";
    let inner = json!({ "source": "script", "value": "return await now()" });
    let timeout = Outcome::Failure(pass_failure(
        Cause::Timeout,
        "the workflow ran past its timeout".to_owned(),
        None,
    ));
    let line = |outcome: Outcome| logged(2, BlockName::Workflow, inner.clone(), outcome);
    let now = logged(4, BlockName::Now, json!({}), result(json!(9)));
    let consumed = called(body, vec![line(timeout), now.clone()], reading(&[]));
    assert_eq!((consumed.used, consumed.end), (2, returned(json!([1, 9]))));
    let answered = called(body, vec![line(result(json!(5))), now], reading(&[]));
    assert_eq!(
        answered.end,
        End::Failed(pass_failure(
            Cause::Replay,
            "the log answers call 2, a workflow, with a result: a workflow's body gives its result"
                .to_owned(),
            Some(2),
        ))
    );
}
