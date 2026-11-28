//! A command step's host (R11, R12): it claims its step under the lock, holds the lock across
//! the spawn and the publication of the child's identity, runs the command through `sh -c` to
//! its exit or its expiry, settles its own step, and advances the run.

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::Value;

use super::{Room, Run, STDOUT_CAP, Status, failed, failure, files, to_value};
use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::StatePaths;
use crate::harness::block::{Cause, ShellResult};
use crate::harness::check_run::{CheckEcho, WatchDeadline, run_command};
use crate::ids::WorkflowRunId;
use crate::store::workflow_run::{self as store, Process, Settled, Work};

/// The body of the host the room started for step `number` at `attempt`: claim, run, settle its
/// own step, then advance. It runs nothing when the record holds another attempt or another live
/// host.
pub(crate) fn host(
    state: &StatePaths,
    room: &std::rc::Rc<dyn Room>,
    id: &WorkflowRunId,
    number: u64,
    attempt: u32,
) -> Result<Status> {
    let run = Run::new(state, room, id);
    run_step(&run, number, attempt)?;
    super::step::advance(&run)
}

fn mine(record: &store::Record, number: u64, attempt: u32, me: &Process) -> bool {
    record.ended.is_none()
        && record.open.get(&number).is_some_and(|open| {
            open.attempt == attempt
                && matches!(&open.work, Work::Command { host, .. } if host == me)
        })
}

fn run_step(run: &Run<'_>, number: u64, attempt: u32) -> Result<()> {
    let me = run.room.me();
    let guard = WorkspaceLock::acquire(&run.state.workspace_lock)?;
    let dir = run.paths.step(number);
    let claim = run.publish_held(&guard, |on_disk, change| {
        if on_disk.ended.is_some() {
            return None;
        }
        let open = change.next.open.get_mut(&number)?;
        let free = match &open.work {
            Work::Starting { .. } => true,
            Work::Command { host, child, .. } => *host == me && child.is_none(),
            _ => false,
        };
        if open.attempt != attempt || !free {
            return None;
        }
        open.work = Work::Command {
            host: me.clone(),
            child: None,
            dir: dir.clone(),
            files: files::copy_of(&run.paths.home, on_disk.files, open.parent),
        };
        Some((open.clone(), on_disk.checkout.clone()))
    })?;
    let Some((step, checkout)) = claim else {
        return Ok(());
    };
    let Work::Command { files, .. } = &step.work else {
        return Ok(());
    };
    let command = step
        .object
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let cwd = match step.object.get("cwd").and_then(Value::as_str) {
        Some(cwd) => checkout.join(cwd),
        None => checkout,
    };
    let env = BTreeMap::from([
        ("RIMZ_WORKFLOW_DIR".to_owned(), dir.display().to_string()),
        (
            "RIMZ_WORKFLOW_FILES".to_owned(),
            files.display().to_string(),
        ),
        ("RIMZ_WORKFLOW_RUN".to_owned(), run.id.as_str().to_owned()),
    ]);
    let log = dir.join("output.log");
    let file = std::fs::File::create(&log)?;
    let expired = || step.expires.is_some_and(|at| run.room.now() >= at);
    // A record that does not read now is no sign the step moved: the next poll reads it again.
    let moved = || match store::load(&run.paths) {
        Ok(record) => !record.is_some_and(|record| mine(&record, number, attempt, &me)),
        Err(_) => false,
    };
    let interrupted = || expired() || moved();
    let mut guard = Some(guard);
    let started = std::cell::Cell::new(false);
    let mut spawned = |pid: u32| -> std::io::Result<()> {
        started.set(true);
        let guard = guard
            .take()
            .ok_or_else(|| std::io::Error::other("the spawn lock is held once"))?;
        let child = Process {
            pid,
            start: crate::proc::process_start_token(pid),
        };
        let published = run.publish_held(&guard, |on_disk, change| {
            if !mine(on_disk, number, attempt, &me) {
                return None;
            }
            if let Some(Work::Command { child: slot, .. }) =
                change.next.open.get_mut(&number).map(|open| &mut open.work)
            {
                *slot = Some(child);
            }
            Some(())
        });
        match published {
            Ok(Some(())) => Ok(()),
            Ok(None) => Err(std::io::Error::other(
                "the step moved before its child was published",
            )),
            Err(error) => Err(std::io::Error::other(error)),
        }
    };
    let outcome = run_command(
        &cwd,
        command,
        WatchDeadline::Hosted {
            argv: None,
            interrupted: &interrupted,
            spawned: &mut spawned,
        },
        CheckEcho::TeeApart {
            file,
            stdout_cap: STDOUT_CAP,
        },
        &env,
        |_, _| {},
        &mut None,
    );
    drop(guard);
    let output = log.display().to_string();
    let settled = match outcome {
        Err(_) if moved() => return Ok(()),
        Err(error) => {
            let reason = if started.get() {
                format!("the host lost the command it started: {error:#}")
            } else {
                format!("the command did not run: {error:#}")
            };
            failed(&failure(&step, Cause::Exit, reason))
        }
        Ok(outcome) if outcome.interrupted() => {
            if !expired() {
                return Ok(());
            }
            let timeout = step
                .object
                .get("timeout")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let reason = format!("the call ran past its timeout of {timeout}");
            failed(&failure(&step, Cause::Timeout, reason))
        }
        Ok(outcome) => {
            let stdout = outcome.stdout().unwrap_or_default().to_owned();
            match (outcome.code(), outcome.signal()) {
                (Some(0), _) => Settled::Result(to_value(&ShellResult {
                    code: 0,
                    stdout,
                    output,
                })),
                (code, signal) => {
                    let reason = match (code, signal) {
                        (Some(code), _) => format!("the command exited {code}"),
                        (None, Some(signal)) => {
                            format!("the command was killed by signal {signal}")
                        }
                        (None, None) => "the command ended with no exit code".to_owned(),
                    };
                    let mut failure = failure(&step, Cause::Exit, reason);
                    failure.code = code;
                    failure.stdout = Some(stdout);
                    failure.output = Some(output);
                    failed(&failure)
                }
            }
        }
    };
    let now = run.room.now();
    run.publish(|on_disk, change| {
        mine(on_disk, number, attempt, &me).then(|| change.settle(number, settled, now))
    })?;
    Ok(())
}
