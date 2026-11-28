//! The four example workflows of link 1, each run to its end as a run against the fake room, over
//! the answers of its `tries/clean.jsonl`: call n gets line n, through the room as its block
//! would get it.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::super::room::{Ending as LaunchEnding, Look};
use super::*;

fn run_fixture(name: &str, args: Option<Value>) {
    let h = Harness::new();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/workflows")
        .join(name);
    let answers: Vec<Value> = std::fs::read_to_string(dir.join("tries/clean.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let answer = |number: u64| answers[usize::try_from(number).unwrap() - 1].clone();
    // A `now` reads the time its line holds.
    let (state, id, fake) = (h.state.clone(), h.id.clone(), std::rc::Rc::clone(&h.fake));
    let times = answers.clone();
    h.fake.hook("now", move || {
        if let Ok(Some(record)) = store::load(&state.workflow_run(&id)) {
            for step in record
                .open
                .values()
                .filter(|step| step.block == "now" && step.work == Work::Due)
            {
                if let Some(millis) =
                    times[usize::try_from(step.number).unwrap() - 1]["result"].as_i64()
                {
                    fake.set_now(jiff::Timestamp::from_millisecond(millis).unwrap());
                }
            }
        }
        false
    });
    let mut input = h.input(Origin::Path(dir));
    input.args = args;
    h.start_with(input);
    for _ in 0..100 {
        if h.record().ended.is_some() {
            break;
        }
        h.fake.take_hosts();
        for step in h.record().open.values() {
            let line = answer(step.number);
            let settled = match (&line["result"], &line["failure"]) {
                (Value::Null, failure) => Settled::Failure(failure.clone()),
                (result, _) => Settled::Result(result.clone()),
            };
            match (&step.work, step.block.as_str()) {
                (Work::Command { .. }, _) => h.settle_as_host(step.number, settled),
                (Work::Launch { launches, .. }, "agent") => {
                    let ending = match settled {
                        Settled::Result(result) if step.object.get("schema").is_some() => {
                            LaunchEnding::Completed {
                                message: String::new(),
                                answer: Some(result),
                            }
                        }
                        Settled::Result(result) => LaunchEnding::Completed {
                            message: result["report"].as_str().unwrap_or_default().to_owned(),
                            answer: None,
                        },
                        Settled::Failure(failure) => LaunchEnding::Failed(
                            failure["reason"].as_str().unwrap_or("failed").to_owned(),
                        ),
                    };
                    h.fake.end(&launches[0], Some(ending));
                }
                (Work::Launch { .. }, "team") => {
                    let report = line["result"]["report"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                    h.fake.board_done(step.number, step.attempt, &report);
                }
                (Work::Watch, _) => {
                    let readings: BTreeMap<String, Option<String>> =
                        serde_json::from_value(line["result"]["readings"].clone()).unwrap();
                    let when = step.object["when"].as_str().unwrap();
                    h.fake.set_look(
                        when,
                        Look {
                            truth: Some(true),
                            readings,
                        },
                    );
                }
                _ => {}
            }
        }
        h.advance();
    }
    let result = h.done();
    assert_eq!(
        h.lines().len(),
        answers.len(),
        "{name} ended {result} after {:#?}",
        h.lines()
    );
}

#[test]
fn land_runs_to_its_end() {
    run_fixture("land", None);
}

#[test]
fn ralph_runs_to_its_end() {
    run_fixture("ralph", Some(json!({ "goal": "/goals/tokens" })));
}

#[test]
fn steward_runs_to_its_end() {
    run_fixture(
        "steward",
        Some(json!({ "job": "/jobs/rename", "batch": 1 })),
    );
}

#[test]
fn review_change_runs_to_its_end() {
    run_fixture("review-change", Some(json!({ "base": "origin/main" })));
}
