//! The advance: owed stops, expiries, what the room shows of each step's work, a script pass, and
//! the starts, round after round until a round moves nothing.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Result;
use jiff::Timestamp;
use serde_json::{Value, json};

use super::pass::run_failure;
use super::room::{Admit, Ending as LaunchEnding, Launched, Pin, Seen};
use super::{
    Change, Run, STARTS, Status, block_of, failed, failure, files, first_attempt, pass, to_value,
};
use crate::harness::block::{
    AgentResult, BlockName, Cause, ScheduleResult, TeamResult, WatchResult,
};
use crate::store::workflow_run::{
    Ending, Entry, Event, Hold, OpenStep, Process, Record, Settled, Stop, Work,
};

pub(super) fn advance(run: &Run<'_>) -> Result<Status> {
    loop {
        let mut moved = carry_stops(run)?;
        if run.load()?.ended.is_some() {
            let seen = run.room.launches(run.id)?;
            let record = run.load()?;
            sweep(run, &record, &seen);
            if !moved && record.ended.is_some() {
                return Ok(status(&record));
            }
            continue;
        }
        moved |= expire(run)?;
        moved |= observe(run)?;
        moved |= pass::pass(run)?;
        // A stop owed this round, a settled team's seats among them, lands before a launch.
        moved |= carry_stops(run)?;
        moved |= begin(run)?;
        if !moved {
            return Ok(status(&run.load()?));
        }
    }
}

fn status(record: &Record) -> Status {
    let held = record.open.values().filter_map(|step| match step.work {
        Work::Held {
            hold: Hold::Room { .. },
            ..
        } => enclosing(record, step).0,
        _ => None,
    });
    let next = record
        .open
        .values()
        .filter_map(|step| step.expires)
        .chain(record.bounds.expires)
        .chain(held)
        .min();
    Status {
        ended: record.ended.as_ref().map(|ended| ended.ending.clone()),
        next: record.ended.is_none().then_some(next).flatten(),
    }
}

/// Carry out each owed stop off the lock, and strike those that succeeded: a process's once it
/// reads dead.
fn carry_stops(run: &Run<'_>) -> Result<bool> {
    let record = run.load()?;
    let struck: Vec<Stop> = record
        .owed
        .iter()
        .filter(|stop| {
            let (stopped, process) = match stop {
                Stop::Launches { launches } => (run.room.stop_launches(launches), None),
                Stop::Process { process } => (run.room.kill(process, false), Some(process)),
                Stop::Group { process } => (run.room.kill(process, true), Some(process)),
            };
            stopped
                .inspect_err(|error| tracing::warn!(%error, ?stop, "a workflow run's stop failed"))
                .is_ok()
                && process.is_none_or(|process| !run.room.alive(process))
        })
        .cloned()
        .collect();
    if struck.is_empty() {
        return Ok(false);
    }
    let now = run.room.now();
    run.publish(|_, change| {
        for stop in struck {
            if let Some(at) = change.next.owed.iter().position(|owed| *owed == stop) {
                change.next.owed.remove(at);
                change.entries.push(Entry {
                    at: now,
                    event: Event::Stop { stop },
                });
            }
        }
        Some(())
    })?;
    Ok(true)
}

/// Stop every live launch under a pin the record does not hold (R7): an older attempt's, a
/// settled step's, an ended run's. `record` is read after `seen` was listed: every launch is
/// published `Starting` before it is made, so such a record holds every pin a live process
/// launched under.
fn sweep(run: &Run<'_>, record: &Record, seen: &[Seen]) {
    let strays: Vec<String> = seen
        .iter()
        .filter(|seen| seen.alive && !held(record, &seen.pin))
        .map(|seen| seen.launch.clone())
        .collect();
    if !strays.is_empty()
        && let Err(error) = run.room.stop_launches(&strays)
    {
        tracing::warn!(%error, "stopping a workflow run's stray launches failed");
    }
}

fn held(record: &Record, pin: &Pin) -> bool {
    record.ended.is_none()
        && pin.run == record.id
        && record.open.get(&pin.step).is_some_and(|step| {
            step.attempt == pin.attempt
                && matches!(step.work, Work::Starting { .. } | Work::Launch { .. })
        })
}

/// R16 and R17: the run's expiry ends it; a step's writes its `timeout` failure and owes its
/// stop; a `workflow` call's, only when every line is covered, also drops the steps under it.
fn expire(run: &Run<'_>) -> Result<bool> {
    let record = run.load()?;
    let now = run.room.now();
    if record.bounds.expires.is_some_and(|at| at <= now) {
        run.publish(|on_disk, change| {
            let reason = "the run ran past its timeout".to_owned();
            let failure = run_failure(Cause::Timeout, reason.clone(), None);
            let past = on_disk.bounds.expires.is_some_and(|at| at <= now);
            (on_disk.ended.is_none() && past).then(|| {
                change.end(
                    Ending::Failed {
                        reason,
                        step: None,
                        failure: Some(to_value(&failure)),
                    },
                    now,
                );
            })
        })?;
        return Ok(true);
    }
    let quiet = |record: &Record| record.covered == Some(record.log.lines);
    let due: Vec<OpenStep> = record
        .open
        .values()
        .filter(|step| step.expires.is_some_and(|at| at <= now))
        .filter(|step| block_of(step) != Some(BlockName::Workflow) || quiet(&record))
        .cloned()
        .collect();
    if due.is_empty() {
        return Ok(false);
    }
    run.publish(|on_disk, change| {
        if on_disk.ended.is_some() {
            return None;
        }
        for step in &due {
            if change.next.open.get(&step.number) != Some(step) {
                continue;
            }
            let workflow = block_of(step) == Some(BlockName::Workflow);
            if workflow {
                if !quiet(on_disk) {
                    continue;
                }
                drop_under(change, step.number);
            }
            let timeout = step
                .object
                .get("timeout")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let reason = format!("the call ran past its timeout of {timeout}");
            change.owe(&step.work);
            change.settle(
                step.number,
                failed(&failure(step, Cause::Timeout, reason)),
                now,
            );
        }
        Some(())
    })?;
    Ok(true)
}

/// Drop every step open under the `workflow` call `number`, at any depth, its work owed.
fn drop_under(change: &mut Change, number: u64) {
    let under: Vec<u64> = change
        .next
        .open
        .keys()
        .copied()
        .filter(|step| ancestors(&change.next, *step).contains(&number))
        .collect();
    for step in under {
        if let Some(step) = change.next.open.remove(&step) {
            change.owe(&step.work);
        }
    }
}

fn ancestors(record: &Record, number: u64) -> Vec<u64> {
    let mut found = Vec::new();
    let mut at = record.open.get(&number).and_then(|step| step.parent);
    while let Some(parent) = at {
        found.push(parent);
        at = record.open.get(&parent).and_then(|step| step.parent);
    }
    found
}

/// What the room shows of a step's work.
enum Decision {
    /// Its line, and the stops its work is owed.
    Settle(Settled, Vec<Stop>),
    /// Its work is now this.
    Work(Work),
}

/// Read what the room shows of each step's work and settle, take up, or restart it: the
/// starters read dead first, then the run's launches listed (R5).
fn observe(run: &Run<'_>) -> Result<bool> {
    let record = run.load()?;
    let lost: BTreeSet<u64> = record
        .open
        .values()
        .filter(|step| matches!(&step.work, Work::Starting { starter } if !run.room.alive(starter)))
        .map(|step| step.number)
        .collect();
    let seen = run.room.launches(run.id)?;
    sweep(run, &run.load()?, &seen);
    let mut decisions = Vec::new();
    for step in record.open.values() {
        let block = block_of(step);
        let decision = match (&step.work, block) {
            (Work::Starting { .. }, Some(BlockName::Agent | BlockName::Team))
                if lost.contains(&step.number) =>
            {
                launched(run, step, record.files, &seen)
            }
            (Work::Starting { .. }, Some(BlockName::Shell)) if lost.contains(&step.number) => {
                Some(restart(step, record.files))
            }
            (Work::Launch { .. }, Some(BlockName::Agent | BlockName::Team)) => {
                match launched(run, step, record.files, &seen) {
                    Some(Decision::Work(Work::Launch { .. })) => None,
                    decision => decision,
                }
            }
            (Work::Command { host, child, .. }, _) => {
                command(run, step, record.files, host, child.as_ref())
            }
            (Work::Watch, _) => watch(run, &record, step),
            _ => None,
        };
        decisions.extend(decision.map(|decision| (step.clone(), decision)));
    }
    if decisions.is_empty() {
        return Ok(false);
    }
    let now = run.room.now();
    run.publish(|on_disk, change| {
        if on_disk.ended.is_some() {
            return None;
        }
        for (step, decision) in decisions {
            if on_disk.open.get(&step.number) == Some(&step) {
                decide(change, &step, decision, now);
            }
        }
        Some(())
    })?;
    Ok(true)
}

fn decide(change: &mut Change, step: &OpenStep, decision: Decision, now: Timestamp) {
    match decision {
        Decision::Settle(settled, owed) => {
            change.next.owed.extend(owed);
            change.settle(step.number, settled, now);
        }
        Decision::Work(work) => {
            if let Some(open) = change.next.open.get_mut(&step.number) {
                open.work = work;
            }
        }
    }
}

/// What the launches under an agent or team step's pin show (R9, R10): alive, the launch to take
/// up; ended, its settlement; gone with no ending, a restart.
fn launched(run: &Run<'_>, step: &OpenStep, files: u32, seen: &[Seen]) -> Option<Decision> {
    let pin = pin(run, step);
    let mine: Vec<&Seen> = seen.iter().filter(|seen| seen.pin == pin).collect();
    let alive: Vec<String> = mine
        .iter()
        .filter(|seen| seen.alive)
        .map(|seen| seen.launch.clone())
        .collect();
    let handle = mine.iter().find_map(|seen| seen.handle.clone());
    if block_of(step) == Some(BlockName::Team) {
        let report = |message: String| {
            let stops = (!alive.is_empty()).then(|| Stop::Launches {
                launches: alive.clone(),
            });
            Decision::Settle(
                Settled::Result(to_value(&TeamResult { report: message })),
                stops.into_iter().collect(),
            )
        };
        if let Some(message) = run.room.board(&pin) {
            return Some(report(message));
        }
        if !alive.is_empty() {
            return Some(Decision::Work(Work::Launch {
                launches: mine.iter().map(|seen| seen.launch.clone()).collect(),
                handle: None,
            }));
        }
        if let Some(message) = run.room.board(&pin) {
            return Some(report(message));
        }
        if mine.iter().any(|seen| seen.ending.is_some()) {
            let reason = "the team's seats ended and its board never read done";
            return Some(Decision::Settle(
                failed(&failure(step, Cause::Died, reason)),
                Vec::new(),
            ));
        }
        return Some(restart(step, files));
    }
    if !alive.is_empty() {
        return Some(Decision::Work(Work::Launch {
            launches: alive,
            handle,
        }));
    }
    match mine.iter().find_map(|seen| seen.ending.as_ref()) {
        Some(ending) => Some(Decision::Settle(
            agent_ended(step, handle, ending),
            Vec::new(),
        )),
        None => Some(restart(step, files)),
    }
}

/// An agent's ending as its call's answer (R9).
fn agent_ended(step: &OpenStep, handle: Option<String>, ending: &LaunchEnding) -> Settled {
    let fail = |cause: Cause, reason: String| {
        let mut failure = failure(step, cause, reason);
        failure.handle = handle.clone();
        failed(&failure)
    };
    match ending {
        LaunchEnding::Completed { message, answer } => {
            let schema = step
                .object
                .get("schema")
                .is_some_and(|schema| !schema.is_null());
            match (schema, answer) {
                (true, Some(answer)) => Settled::Result(answer.clone()),
                (true, None) => fail(
                    Cause::Invalid,
                    "the agent ended without handing in the answer its schema asks for".to_owned(),
                ),
                (false, _) => Settled::Result(to_value(&AgentResult {
                    report: message.clone(),
                    handle: handle.clone().unwrap_or_default(),
                })),
            }
        }
        LaunchEnding::Failed(reason) => fail(Cause::Died, format!("the agent failed: {reason}")),
        LaunchEnding::Canceled(reason) => {
            fail(Cause::Died, format!("the agent was canceled: {reason}"))
        }
    }
}

/// A step whose work is gone with no ending: due again, or `died` after its last start (R6).
fn restart(step: &OpenStep, files: u32) -> Decision {
    if step.attempt - first_attempt(files) >= STARTS {
        let reason = format!("its work was gone with no ending after {STARTS} starts");
        return Decision::Settle(failed(&failure(step, Cause::Died, reason)), Vec::new());
    }
    Decision::Work(Work::Due)
}

/// R11: host and child dead with no line, a restart; a child alive without its host is killed
/// first, by its identity.
fn command(
    run: &Run<'_>,
    step: &OpenStep,
    files: u32,
    host: &Process,
    child: Option<&Process>,
) -> Option<Decision> {
    if run.room.alive(host) {
        return None;
    }
    if let Some(child) = child {
        // The group may outlive its leader, so the kill by identity goes out whether or not
        // the leader reads alive.
        if let Err(error) = run.room.kill(child, true) {
            tracing::warn!(%error, "killing a workflow command whose host died failed");
        }
        if run.room.alive(child) {
            return None;
        }
    }
    Some(restart(step, files))
}

/// R13: one look; true, the readings of that look.
fn watch(run: &Run<'_>, record: &Record, step: &OpenStep) -> Option<Decision> {
    let when = step.object.get("when").and_then(Value::as_str)?;
    let look = run.room.look(when, &place(record, step));
    (look.truth == Some(true)).then(|| {
        Decision::Settle(
            Settled::Result(to_value(&WatchResult {
                readings: look.readings,
            })),
            Vec::new(),
        )
    })
}

/// A step's `cwd`, absolute or relative to the checkout, else the checkout.
fn place(record: &Record, step: &OpenStep) -> PathBuf {
    match step.object.get("cwd").and_then(Value::as_str) {
        Some(cwd) => record.checkout.join(cwd),
        None => record.checkout.clone(),
    }
}

fn pin(run: &Run<'_>, step: &OpenStep) -> Pin {
    Pin {
        run: run.id.clone(),
        step: step.number,
        attempt: step.attempt,
    }
}

/// The `give_up` instant and `skip_capacity` of the nearest enclosing `workflow` call that
/// set one, else the run's.
fn enclosing(record: &Record, step: &OpenStep) -> (Option<Timestamp>, bool) {
    let calls: Vec<&Work> = ancestors(record, step.number)
        .into_iter()
        .filter_map(|number| record.open.get(&number).map(|step| &step.work))
        .collect();
    let give_up = calls
        .iter()
        .find_map(|work| match work {
            Work::Workflow { give_up, .. } => *give_up,
            _ => None,
        })
        .or(record.give_up);
    let skip = calls
        .iter()
        .find_map(|work| match work {
            Work::Workflow { skip_capacity, .. } => *skip_capacity,
            _ => None,
        })
        .unwrap_or(record.skip_capacity);
    (give_up, skip)
}

/// Start what may start, lowest number first.
fn begin(run: &Run<'_>) -> Result<bool> {
    let numbers: Vec<u64> = run.load()?.open.into_keys().collect();
    let mut moved = false;
    for number in numbers {
        let record = run.load()?;
        if record.ended.is_some() {
            break;
        }
        let Some(step) = record.open.get(&number) else {
            continue;
        };
        if !matches!(step.work, Work::Due | Work::Held { .. }) {
            continue;
        }
        moved |= match block_of(step) {
            Some(BlockName::Now) => now(run, step)?,
            Some(BlockName::Schedule) => schedule(run, step)?,
            Some(BlockName::Shell) => start_command(run, &record, step)?,
            Some(BlockName::Agent | BlockName::Team) => start_launch(run, &record, step)?,
            _ => false,
        };
    }
    Ok(moved)
}

/// Settle `step` at `now` if the record still holds it as it was read.
fn settle_unchanged(
    run: &Run<'_>,
    step: &OpenStep,
    settled: Settled,
    now: Timestamp,
) -> Result<bool> {
    run.publish(|on_disk, change| {
        (on_disk.ended.is_none() && on_disk.open.get(&step.number) == Some(step))
            .then(|| change.settle(step.number, settled, now))
    })?;
    Ok(true)
}

/// R2: the room's time as epoch milliseconds.
fn now(run: &Run<'_>, step: &OpenStep) -> Result<bool> {
    let now = run.room.now();
    settle_unchanged(run, step, Settled::Result(json!(now.as_millisecond())), now)
}

/// R2: a booking, under the call's name, so a booking again replaces it.
fn schedule(run: &Run<'_>, step: &OpenStep) -> Result<bool> {
    let settled = match run.room.book(run.id, &step.object) {
        Ok(id) => Settled::Result(to_value(&ScheduleResult { id })),
        Err(reason) => failed(&failure(step, Cause::Invalid, reason)),
    };
    settle_unchanged(run, step, settled, run.room.now())
}

/// R15 and R8: the launch cap, the run's parallel cap, and the room's `admit`, then the start
/// published with its starter before the launch.
fn start_launch(run: &Run<'_>, record: &Record, step: &OpenStep) -> Result<bool> {
    let number = step.number;
    let now = run.room.now();
    if expired(record, step, now) {
        return Ok(true);
    }
    if record.launches >= record.bounds.launches {
        let cap = record.bounds.launches;
        let reason = format!("call {number} would launch past the run's launch cap of {cap}");
        let failure = run_failure(Cause::Launches, reason.clone(), Some(number));
        run.publish(|on_disk, change| {
            (on_disk.ended.is_none() && on_disk.open.get(&number) == Some(step)).then(|| {
                change.end(
                    Ending::Failed {
                        reason,
                        step: Some(number),
                        failure: Some(to_value(&failure)),
                    },
                    now,
                );
            })
        })?;
        return Ok(true);
    }
    if holding(record) >= record.bounds.parallel {
        return hold(run, step, Hold::Parallel, now);
    }
    let (give_up, skip) = enclosing(record, step);
    // Some: this is a launch step (`begin` matched its block).
    let block = block_of(step).expect("a launch step names its block");
    if !skip {
        match run.room.admit(block, &step.object, &record.checkout) {
            Admit::Go => {}
            Admit::Hold(reason) if give_up.is_some_and(|at| at <= now) => {
                let reason = format!("the room held the launch past its give_up: {reason}");
                let settled = failed(&failure(step, Cause::Capacity, reason));
                return settle_unchanged(run, step, settled, now);
            }
            Admit::Hold(reason) => return hold(run, step, Hold::Room { reason }, now),
            Admit::Refuse(reason) => {
                let settled = failed(&failure(step, Cause::Invalid, reason));
                return settle_unchanged(run, step, settled, now);
            }
        }
    }
    let me = run.room.me();
    let attempt = step.attempt + 1;
    let mut stopping = false;
    let started = run.publish(|on_disk, change| {
        let fits = on_disk.launches < on_disk.bounds.launches
            && holding(on_disk) < on_disk.bounds.parallel;
        // A stop of launches still owed, a settled team's seats among them, lands first.
        stopping = on_disk
            .owed
            .iter()
            .any(|stop| matches!(stop, Stop::Launches { .. }));
        if on_disk.ended.is_some() || on_disk.open.get(&number) != Some(step) || !fits || stopping {
            return None;
        }
        starting(change, number, attempt, &me, now);
        change.next.launches += 1;
        Some(())
    })?;
    if started.is_none() {
        // A step held back for a stop moved nothing, so a stop that keeps failing ends the
        // advance instead of spinning it.
        return Ok(!stopping);
    }
    let pin = Pin {
        run: run.id.clone(),
        step: number,
        attempt,
    };
    let starting = Work::Starting { starter: me };
    let mine = |on_disk: &Record| {
        on_disk.ended.is_none()
            && on_disk
                .open
                .get(&number)
                .is_some_and(|open| open.attempt == attempt && open.work == starting)
    };
    match run.room.launch(block, &step.object, &record.checkout, &pin) {
        Launched::Started { launches, handle } => {
            let kept = run.publish(|on_disk, change| {
                mine(on_disk).then(|| {
                    decide(
                        change,
                        &on_disk.open[&number],
                        Decision::Work(Work::Launch {
                            launches: launches.clone(),
                            handle,
                        }),
                        now,
                    );
                })
            })?;
            if kept.is_none()
                && let Err(error) = run.room.stop_launches(&launches)
            {
                tracing::warn!(%error, "stopping a late workflow launch failed");
            }
        }
        Launched::Refused(reason) => {
            run.publish(|on_disk, change| {
                mine(on_disk).then(|| {
                    let open = &on_disk.open[&number];
                    change.settle(number, failed(&failure(open, Cause::Invalid, reason)), now);
                })
            })?;
        }
        Launched::Unknown(error) => {
            tracing::warn!(%error, "a workflow launch's outcome is unknown");
            let seen = match run.room.launches(run.id) {
                Ok(seen) => seen,
                Err(error) => {
                    // Nothing shows what the launch did, so this live starter hands it back.
                    run.publish(|on_disk, change| {
                        mine(on_disk).then(|| {
                            let open = &on_disk.open[&number];
                            decide(change, open, restart(open, on_disk.files), now);
                        })
                    })?;
                    return Err(error);
                }
            };
            run.publish(|on_disk, change| {
                if !mine(on_disk) {
                    return None;
                }
                let open = on_disk.open[&number].clone();
                let decision = launched(run, &open, on_disk.files, &seen)?;
                decide(change, &open, decision, now);
                Some(())
            })?;
        }
    }
    Ok(true)
}

/// Whether `step`, a `workflow` call it runs under, or the run is past its expiry: the next
/// round's `expire` settles it, so no cap or start reads it first.
fn expired(record: &Record, step: &OpenStep, now: Timestamp) -> bool {
    let calls = ancestors(record, step.number)
        .into_iter()
        .filter_map(|number| record.open.get(&number)?.expires);
    step.expires
        .into_iter()
        .chain(calls)
        .chain(record.bounds.expires)
        .any(|at| at <= now)
}

/// How many of the run's agent and team steps hold a launch.
fn holding(record: &Record) -> u32 {
    let count = record
        .open
        .values()
        .filter(|step| matches!(block_of(step), Some(BlockName::Agent | BlockName::Team)))
        .filter(|step| matches!(step.work, Work::Starting { .. } | Work::Launch { .. }))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Hold `step` by `hold`, publishing only a change.
fn hold(run: &Run<'_>, step: &OpenStep, hold: Hold, now: Timestamp) -> Result<bool> {
    if matches!(&step.work, Work::Held { hold: current, .. } if *current == hold) {
        return Ok(false);
    }
    let published = run.publish(|on_disk, change| {
        if on_disk.ended.is_some() || on_disk.open.get(&step.number) != Some(step) {
            return None;
        }
        let open = change.next.open.get_mut(&step.number)?;
        open.work = Work::Held { since: now, hold };
        Some(())
    })?;
    Ok(published.is_some())
}

fn starting(change: &mut Change, number: u64, attempt: u32, me: &Process, now: Timestamp) {
    if let Some(open) = change.next.open.get_mut(&number) {
        open.attempt = attempt;
        open.work = Work::Starting {
            starter: me.clone(),
        };
    }
    change.entries.push(Entry {
        at: now,
        event: Event::Start {
            step: number,
            attempt,
        },
    });
}

/// R11 and R12: the start published, `context.json` written, the host started, and its
/// identity published.
fn start_command(run: &Run<'_>, record: &Record, step: &OpenStep) -> Result<bool> {
    let number = step.number;
    let now = run.room.now();
    if expired(record, step, now) {
        return Ok(true);
    }
    let me = run.room.me();
    let attempt = step.attempt + 1;
    let started = run.publish(|on_disk, change| {
        (on_disk.ended.is_none() && on_disk.open.get(&number) == Some(step))
            .then(|| starting(change, number, attempt, &me, now))
    })?;
    if started.is_none() {
        return Ok(true);
    }
    let starting = Work::Starting { starter: me };
    let mine = |on_disk: &Record| {
        on_disk
            .open
            .get(&number)
            .filter(|open| {
                on_disk.ended.is_none() && open.attempt == attempt && open.work == starting
            })
            .cloned()
    };
    let dir = run.paths.step(number);
    let args = match step.parent {
        Some(parent) => record
            .open
            .get(&parent)
            .and_then(|call| call.object.get("args").cloned()),
        None => record.args.clone(),
    };
    let context = json!({
        "run": run.id.as_str(),
        "step": number,
        "args": args.unwrap_or(Value::Null),
        "input": step.object.get("input").cloned().unwrap_or(Value::Null),
    });
    let written = crate::disk::atomic::create_dir_all_mode(&dir, 0o700).and_then(|()| {
        crate::disk::atomic::write_temp_then_rename(&dir.join("context.json"), &context)
    });
    if let Err(error) = written {
        // No host will claim the step, so this live starter hands it back.
        run.publish(|on_disk, change| {
            let open = mine(on_disk)?;
            decide(change, &open, restart(&open, on_disk.files), now);
            Some(())
        })?;
        return Err(error.into());
    }
    let pin = Pin {
        run: run.id.clone(),
        step: number,
        attempt,
    };
    let host = run.room.start_host(&pin);
    let published = run.publish(|on_disk, change| {
        let open = mine(on_disk)?;
        let work = match &host {
            Ok(host) => Decision::Work(Work::Command {
                host: host.clone(),
                child: None,
                dir: dir.clone(),
                files: files::copy_of(&run.paths.home, on_disk.files, open.parent),
            }),
            Err(_) => restart(&open, on_disk.files),
        };
        decide(change, &open, work, now);
        Some(())
    })?;
    match host {
        Ok(host) if published.is_none() && unclaimed(run, number, attempt)? => {
            if let Err(error) = run.room.kill(&host, false) {
                tracing::warn!(%error, "stopping a late workflow command host failed");
            }
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "starting a workflow command host failed"),
    }
    Ok(true)
}

/// Whether step `number` is still unclaimed at `attempt`: a host that claimed it, which a step
/// settled or moved past the attempt shows too, is left alone.
fn unclaimed(run: &Run<'_>, number: u64, attempt: u32) -> Result<bool> {
    Ok(run
        .load()?
        .open
        .get(&number)
        .is_some_and(|open| open.attempt == attempt && matches!(open.work, Work::Starting { .. })))
}
