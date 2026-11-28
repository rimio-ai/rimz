//! A script pass: one replay of the run's script over its log, applied in one publish (R1), and
//! the end it reaches (R3); and the lines a resume keeps (R19).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Result, anyhow};
use jiff::Timestamp;
use serde_json::Value;

use super::{Change, Run, files, to_value};
use crate::harness::block::{BlockName, Cause, Failure, summary};
use crate::harness::workflow_script::{
    Call, Caps, End, Line, Log, Outcome, Pass, Progress, Replay, first_to_run, replay,
};
use crate::store::workflow_run::{
    self as store, AnswerLine, Ending, Entry, Event, OpenStep, Record, Settled, Work,
};

/// Run a pass when the log holds a line the last one did not cover, or before the first; true
/// when it published, or found the record moved and owes another round.
pub(super) fn pass(run: &Run<'_>) -> Result<bool> {
    let record = run.load()?;
    if record.ended.is_some() || record.covered >= Some(record.log.lines) {
        return Ok(false);
    }
    let now = run.room.now();
    let zone = run.room.zone();
    let moved = |on_disk: &Record| {
        on_disk.log != record.log
            || on_disk.files != record.files
            || on_disk.covered != record.covered
    };
    let at = files::generation(&run.paths.home, record.files);
    let read = store::answers(&run.paths, &record)
        .map_err(anyhow::Error::from)
        .and_then(|lines| {
            let replayed = replay_over(run, &record, &at, &lines, record.covered)?;
            Ok((lines, replayed))
        });
    let (lines, replayed) = match read {
        Ok(read) => read,
        // A resume removed the log or the copy this pass read the record for.
        Err(_) if moved(&run.load()?) => return Ok(true),
        Err(error) => return Err(error),
    };
    run.publish(|on_disk, change| {
        if on_disk.ended.is_some() || moved(on_disk) {
            return None;
        }
        apply(change, replayed, &lines, now, &zone);
        Some(())
    })?;
    Ok(true)
}

/// The replay of `record`'s script in the files generation directory `at` over `lines`: an
/// error when a copy could not be read, else the replay or the sentence of a script that does not
/// load.
fn replay_over(
    run: &Run<'_>,
    record: &Record,
    at: &Path,
    lines: &[AnswerLine],
    covered: Option<usize>,
) -> Result<std::result::Result<Replay, String>> {
    let log = Log::Recorded(recorded(lines)?);
    replay_logged(run, record, at, &log, covered)
}

fn replay_logged(
    run: &Run<'_>,
    record: &Record,
    at: &Path,
    log: &Log,
    covered: Option<usize>,
) -> Result<std::result::Result<Replay, String>> {
    let root = files::root(at)?;
    let (reader, broken) = files::reader(at, run.room);
    let replayed = replay(&Pass {
        name: "workflow.js",
        text: &root.text,
        args: record.args.as_ref(),
        log,
        caps: Caps::PASS,
        dir: root.dir.as_deref(),
        reader: Some(reader),
        covered,
        steps: Some(record.bounds.steps),
    });
    if let Some(error) = broken.borrow_mut().take() {
        return Err(error.context("reading a called workflow's script"));
    }
    Ok(replayed)
}

fn recorded(lines: &[AnswerLine]) -> Result<Vec<Line>> {
    lines
        .iter()
        .map(|line| {
            let block = serde_json::from_value(Value::String(line.block.clone()))?;
            let outcome = match &line.settled {
                Settled::Result(value) => Outcome::Result(value.clone()),
                Settled::Failure(value) => Outcome::Failure(serde_json::from_value(value.clone())?),
            };
            Ok(Line {
                number: line.number,
                block,
                digest: line.digest.clone(),
                outcome,
            })
        })
        .collect()
}

/// R1 and R3: the open map gains every call the replay left open that the record does not hold
/// and loses every step that is no longer open, a step under a settled `workflow` call included,
/// its work owed its stop; the journal gains the marks not replayed; the covered count becomes the
/// log's line count; an end of the script ends the run.
fn apply(
    change: &mut Change,
    replayed: std::result::Result<Replay, String>,
    lines: &[AnswerLine],
    now: Timestamp,
    zone: &jiff::tz::TimeZone,
) {
    change.next.covered = Some(lines.len());
    let replay = match replayed {
        Ok(replay) => replay,
        Err(error) => {
            let reason = format!("the script no longer loads: {error}");
            let failure = run_failure(Cause::Invalid, reason.clone(), None);
            change.end(
                Ending::Failed {
                    reason,
                    step: None,
                    failure: Some(to_value(&failure)),
                },
                now,
            );
            return;
        }
    };
    for mark in replay.marks.iter().filter(|mark| !mark.replayed) {
        let event = match &mark.progress {
            Progress::Phase(title) => Event::Phase {
                after: mark.after,
                title: title.to_string(),
            },
            Progress::Log(text) => Event::Log {
                after: mark.after,
                text: text.clone(),
            },
        };
        change.entries.push(Entry { at: now, event });
    }
    let open = still_open(&replay, lines);
    let gone: Vec<u64> = change
        .next
        .open
        .keys()
        .filter(|number| !open.contains(number))
        .copied()
        .collect();
    for number in gone {
        if let Some(step) = change.next.open.remove(&number) {
            change.owe(&step.work);
        }
    }
    let files = change.next.files;
    for call in replay
        .calls
        .iter()
        .filter(|call| open.contains(&call.number))
    {
        change
            .next
            .open
            .entry(call.number)
            .or_insert_with(|| opened(call, files, now, zone));
    }
    match replay.end {
        End::Pending => {}
        End::Returned(result) => change.end(Ending::Done { result }, now),
        End::Failed(failure) => change.end(
            Ending::Failed {
                reason: failure.reason.clone(),
                step: failure.step,
                failure: Some(to_value(&failure)),
            },
            now,
        ),
        End::Threw(reason) => change.end(
            Ending::Failed {
                reason,
                step: None,
                failure: None,
            },
            now,
        ),
    }
}

/// The replay's open calls the log does not answer and no settled `workflow` call holds.
fn still_open(replay: &Replay, lines: &[AnswerLine]) -> BTreeSet<u64> {
    let parents: BTreeMap<u64, Option<u64>> = replay
        .calls
        .iter()
        .map(|call| (call.number, call.parent))
        .collect();
    let settled: BTreeSet<u64> = replay
        .calls
        .iter()
        .filter(|call| call.block == BlockName::Workflow && !replay.open.contains(&call.number))
        .map(|call| call.number)
        .collect();
    let under_settled = |number: u64| {
        let mut at = parents.get(&number).copied().flatten();
        while let Some(parent) = at {
            if settled.contains(&parent) {
                return true;
            }
            at = parents.get(&parent).copied().flatten();
        }
        false
    };
    replay
        .open
        .iter()
        .copied()
        .filter(|number| !under_settled(*number))
        .filter(|number| !lines.iter().any(|line| line.number == *number))
        .collect()
}

/// A step for `call`, opened now: its expiry fixed from its `timeout`, and a `workflow`
/// call's `give_up` resolved once.
fn opened(call: &Call, files: u32, now: Timestamp, zone: &jiff::tz::TimeZone) -> OpenStep {
    let field = |key: &str| call.object.get(key).and_then(Value::as_str);
    let expires = field("timeout")
        .and_then(|raw| super::duration(raw).ok())
        .and_then(|timeout| now.checked_add(timeout).ok());
    let work = match call.block {
        BlockName::Watch => Work::Watch,
        BlockName::Workflow => Work::Workflow {
            give_up: field("give_up").and_then(|raw| super::resolve_when(raw, now, zone).ok()),
            skip_capacity: call.object.get("skip_capacity").and_then(Value::as_bool),
        },
        BlockName::Agent
        | BlockName::Team
        | BlockName::Shell
        | BlockName::Schedule
        | BlockName::Now => Work::Due,
    };
    OpenStep {
        number: call.number,
        parent: call.parent,
        block: call.block.as_str().to_owned(),
        object: call.object.clone(),
        digest: call.digest.clone(),
        phase: call.phase.as_deref().map(str::to_owned),
        summary: summary(call.block, &call.object),
        opened: now,
        attempt: super::first_attempt(files),
        expires,
        work,
    }
}

/// A failure of the run that no call carries.
pub(super) fn run_failure(cause: Cause, reason: String, step: Option<u64>) -> Failure {
    Failure {
        block: None,
        cause,
        reason,
        step,
        code: None,
        stdout: None,
        output: None,
        handle: None,
    }
}

/// R19: the lines a resume of `record` keeps over the files generation it seals at `at`, those
/// numbered below the first call to run less the timeout line of each `workflow` call it was made
/// under; none when every call keeps its answer. The copies its replay sealed at or past that call
/// are removed: those numbers belong to the calls the resumed run makes there.
pub(super) fn kept_lines(
    run: &Run<'_>,
    record: &Record,
    at: &Path,
) -> Result<Option<Vec<AnswerLine>>> {
    let lines = store::answers(&run.paths, record)?;
    let open_at_end = record
        .ended
        .as_ref()
        .map(|ended| ended.open.clone())
        .unwrap_or_default();
    let log = Log::Recorded(recorded(&lines)?);
    let loads = |error: String| anyhow!("the workflow's script does not load: {error}");
    let replayed = replay_logged(run, record, at, &log, None)?.map_err(loads)?;
    let Some(first) = first_to_run(&replayed, &log, &open_at_end) else {
        return Ok(None);
    };
    for entry in std::fs::read_dir(at)? {
        let entry = entry?;
        let past = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
            .is_some_and(|number| number >= first);
        if past {
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    let calls = replayed.calls;
    let parent = |number: u64| {
        calls
            .iter()
            .find(|call| call.number == number)
            .and_then(|call| call.parent)
    };
    let mut under = BTreeSet::new();
    let mut at = parent(first);
    while let Some(number) = at {
        under.insert(number);
        at = parent(number);
    }
    Ok(Some(
        lines
            .into_iter()
            .filter(|line| line.number < first && !under.contains(&line.number))
            .collect(),
    ))
}
