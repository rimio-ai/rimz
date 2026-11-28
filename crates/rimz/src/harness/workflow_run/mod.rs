//! A run of a workflow script: the durable run that `harness::workflow_script` replays and the
//! steps its calls open, against the room the `Room` seam is.
//!
//! The record of `store::workflow_run` is the only truth. Every change to it is one publish under
//! the workspace lock, compared there with what it was computed from and dropped when that moved;
//! everything that launches, stops, books, or starts a host runs off the lock. A step's work starts
//! only after a publish shows the step open at an attempt with its starter, and a later attempt
//! starts only once everything the record names for the earlier one is found dead with no ending,
//! so one attempt never has two live copies.
//!
//! The entries are `start`, `advance`, `host` (a command host's body), `cancel`, and `resume`.

use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{Result, anyhow, bail};
use jiff::Timestamp;
use serde_json::Value;

use crate::disk::lock::WorkspaceLock;
use crate::disk::paths::{StatePaths, WorkflowRunPaths};
use crate::harness::block::{BlockName, Cause, Failure};
use crate::ids::WorkflowRunId;
use crate::store::workflow_run::{
    self as store, AnswerLine, Ended, Ending, Entry, Event, OpenStep, Origin, Record, Settled,
    Stop, Work,
};

mod command;
mod files;
mod pass;
mod room;
mod step;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

use room::Room;

/// The version of this engine, which a run's record keeps.
const ENGINE: u32 = 1;
/// The launch cap of a start that sets none: a loop of launches stops within tens of them.
const LAUNCHES: u32 = 50;
/// The step cap: the run length link 1 measured.
const STEPS: u64 = 10_000;
/// How many of a run's agent and team steps may hold a launch at once; a start does not set it.
const PARALLEL: u32 = 8;
/// How many times a step is started before its work gone with no ending fails it `died`.
const STARTS: u32 = 3;

/// The attempt a step opens at under the run's copy `files`. Each copy's steps open past the
/// last attempt the copy before could start, so a pin names one attempt for the run's life and a
/// resume never reads a launch of an earlier copy as its own.
fn first_attempt(files: u32) -> u32 {
    (files - 1) * STARTS
}
/// How much of a command's stdout its result keeps.
const STDOUT_CAP: usize = 64 * 1024;

/// What starts a run.
pub(crate) struct Start {
    pub id: WorkflowRunId,
    pub origin: Origin,
    pub args: Option<Value>,
    pub checkout: PathBuf,
    pub max_launches: Option<u32>,
    pub timeout: Option<String>,
    pub give_up: Option<String>,
    pub skip_capacity: bool,
}

/// Where an entry left the run: its ending once it has one, and the earliest instant at which it
/// must be advanced again without a wake.
#[derive(Debug, PartialEq)]
pub(crate) struct Status {
    pub ended: Option<Ending>,
    pub next: Option<Timestamp>,
}

/// Start a run: seal its script, write its record, and advance it. A script that does not load
/// refuses the start and leaves no home.
pub(crate) fn start(state: &StatePaths, room: &Rc<dyn Room>, input: Start) -> Result<Status> {
    let run = Run::new(state, room, &input.id);
    let now = room.now();
    let timeout = input.timeout.as_deref().map(duration).transpose()?;
    let give_up = input
        .give_up
        .as_deref()
        .map(|raw| resolve_when(raw, now, &room.zone()))
        .transpose()?;
    let staged = state.workflow_runs_dir().join(format!(
        ".{}.{}.staging",
        input.id.as_str(),
        uuid::Uuid::now_v7().simple()
    ));
    let checkout = input.checkout.clone();
    let first = files::generation(&staged, 1);
    let root = files::seal_root(&first, &input.origin, &checkout, &**room);
    let root = root.and_then(|()| files::check_loads(&first));
    if let Err(error) = root {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(error);
    }
    let record = Record {
        version: store::VERSION,
        id: input.id.clone(),
        origin: input.origin,
        checkout,
        args: input.args,
        engine: ENGINE,
        bounds: store::Bounds {
            launches: input.max_launches.unwrap_or(LAUNCHES),
            steps: STEPS,
            parallel: PARALLEL,
            expires: timeout.map(|timeout| now + timeout),
        },
        give_up,
        skip_capacity: input.skip_capacity,
        launches: 0,
        files: 1,
        log: store::LogCursor {
            generation: 1,
            length: 0,
            lines: 0,
        },
        journal: 0,
        covered: None,
        open: Default::default(),
        owed: Vec::new(),
        ended: None,
        created_at: now,
    };
    let created = WorkspaceLock::acquire(&state.workspace_lock)
        .map_err(anyhow::Error::from)
        .and_then(|guard| Ok(store::create(&staged, &run.paths, &record, &guard)?));
    if let Err(error) = created {
        let _ = std::fs::remove_dir_all(&staged);
        return Err(error);
    }
    step::advance(&run)
}

/// Advance a run as far as it goes: carry out owed stops, settle and take up work, enforce
/// expiries, run script passes, and start what may start. It returns once the run has ended, or
/// once the log holds no line a pass has not covered and nothing more can be done until the
/// world changes. Safe to call at any time from any process.
pub(crate) fn advance(
    state: &StatePaths,
    room: &Rc<dyn Room>,
    id: &WorkflowRunId,
) -> Result<Status> {
    step::advance(&Run::new(state, room, id))
}

/// End a run canceled, and stop its work.
pub(crate) fn cancel(
    state: &StatePaths,
    room: &Rc<dyn Room>,
    id: &WorkflowRunId,
) -> Result<Status> {
    let run = Run::new(state, room, id);
    let now = room.now();
    run.publish(|on_disk, change| {
        on_disk
            .ended
            .is_none()
            .then(|| change.end(Ending::Canceled, now))
    })?;
    step::advance(&run)
}

/// Run an ended run again from the first call that must run, with a new `timeout`, and a new
/// `max_launches` when one is given: refused for a run that has not ended, for one that ended
/// done, and while any launch or command of it is alive. The fresh copy and the kept lines are
/// worked out in a staging directory of this resume's own, and only a resume that finds the record
/// as it read it, under the lock, publishes them and then removes what the record no longer names.
pub(crate) fn resume(
    state: &StatePaths,
    room: &Rc<dyn Room>,
    id: &WorkflowRunId,
    timeout: Option<&str>,
    max_launches: Option<u32>,
) -> Result<Status> {
    let run = Run::new(state, room, id);
    let record = run.load()?;
    match &record.ended {
        None => bail!("workflow run {id} has not ended, so it does not resume"),
        Some(Ended {
            ending: Ending::Done { .. },
            ..
        }) => bail!("workflow run {id} ended done, so it does not resume"),
        Some(_) => {}
    }
    let alive = room.launches(id)?.into_iter().find(|seen| seen.alive);
    if let Some(seen) = alive {
        bail!(
            "workflow run {id} still has launch {} alive; stop it before the run resumes",
            seen.launch
        );
    }
    if let Some(stop) = record.owed.iter().find(|stop| match stop {
        Stop::Launches { .. } => false,
        Stop::Process { process } | Stop::Group { process } => room.alive(process),
    }) {
        bail!(
            "workflow run {id} still has a command alive ({stop:?}); stop it before the run resumes"
        );
    }
    let timeout = timeout.map(duration).transpose()?;
    let files = record.files + 1;
    let staged = run.paths.home.join("files").join(format!(
        ".{files}.{}.staging",
        uuid::Uuid::now_v7().simple()
    ));
    let planned = files::seal_root(&staged, &record.origin, &record.checkout, &**room)
        .and_then(|()| pass::kept_lines(&run, &record, &staged))
        .and_then(|kept| {
            crate::disk::atomic::sync_tree(&staged)?;
            Ok(kept)
        });
    let kept = match planned {
        Ok(kept) => kept,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staged);
            // A resume that won meanwhile removed the files this one read.
            if run.load()? != record {
                bail!("workflow run {id} changed while it resumed; resume it again");
            }
            return Err(error);
        }
    };
    let now = room.now();
    let published = WorkspaceLock::acquire(&state.workspace_lock)
        .map_err(anyhow::Error::from)
        .and_then(|guard| {
            apply_resume(&run, &record, &staged, kept, &guard, |next| {
                next.files = files;
                next.ended = None;
                next.covered = None;
                if let Some(launches) = max_launches {
                    next.bounds.launches = launches;
                }
                next.bounds.expires = timeout.map(|timeout| now + timeout);
            })
        });
    let _ = std::fs::remove_dir_all(&staged);
    if !published? {
        bail!("workflow run {id} changed while it resumed; resume it again");
    }
    step::advance(&run)
}

/// Publish a resume of `record` under `guard`, unless the record moved since it was read: the
/// copy staged at `staged` becomes the next files generation, `kept` the next log generation,
/// and `change` the rest of the record; then every file the new record does not name goes, or
/// stays for the next resume to remove when it cannot.
fn apply_resume(
    run: &Run<'_>,
    record: &Record,
    staged: &std::path::Path,
    kept: Option<Vec<AnswerLine>>,
    guard: &WorkspaceLock,
    change: impl FnOnce(&mut Record),
) -> Result<bool> {
    if run.load()? != *record {
        return Ok(false);
    }
    let generation = files::generation(&run.paths.home, record.files + 1);
    if generation.exists() {
        std::fs::remove_dir_all(&generation)?;
    }
    crate::disk::atomic::publish_staged_no_replace(staged, &generation)?;
    let log = kept
        .map(|lines| store::write_log(&run.paths, record.log.generation + 1, &lines, guard))
        .transpose()?;
    let published = run.publish_held(guard, |on_disk, building| {
        if on_disk != record {
            return None;
        }
        if let Some(log) = log {
            building.next.log = log;
        }
        change(&mut building.next);
        Some(building.next.clone())
    })?;
    let Some(next) = published else {
        return Ok(false);
    };
    // The resume is published: a file left behind is removed by the next one.
    if let Err(error) = store::remove_stale(&run.paths, &next, guard) {
        tracing::warn!(%error, "removing what a resumed workflow run no longer names failed");
    }
    Ok(true)
}

/// One run, and what reaches it.
struct Run<'a> {
    state: &'a StatePaths,
    room: &'a Rc<dyn Room>,
    id: &'a WorkflowRunId,
    paths: WorkflowRunPaths,
}

/// A publish being built from the record on disk: the next record, and the lines and entries
/// it appends.
struct Change {
    next: Record,
    lines: Vec<AnswerLine>,
    entries: Vec<Entry>,
}

impl<'a> Run<'a> {
    fn new(state: &'a StatePaths, room: &'a Rc<dyn Room>, id: &'a WorkflowRunId) -> Self {
        Self {
            paths: state.workflow_run(id),
            state,
            room,
            id,
        }
    }

    fn load(&self) -> Result<Record> {
        store::load(&self.paths)?.ok_or_else(|| anyhow!("no workflow run {} in this room", self.id))
    }

    /// One publish: `change` reads the record on disk under the lock and builds the next one, or
    /// answers none to drop the publish.
    fn publish<T>(
        &self,
        change: impl FnOnce(&Record, &mut Change) -> Option<T>,
    ) -> Result<Option<T>> {
        let guard = WorkspaceLock::acquire(&self.state.workspace_lock)?;
        self.publish_held(&guard, change)
    }

    fn publish_held<T>(
        &self,
        guard: &WorkspaceLock,
        change: impl FnOnce(&Record, &mut Change) -> Option<T>,
    ) -> Result<Option<T>> {
        let on_disk = self.load()?;
        let mut building = Change {
            next: on_disk.clone(),
            lines: Vec::new(),
            entries: Vec::new(),
        };
        let Some(out) = change(&on_disk, &mut building) else {
            return Ok(None);
        };
        store::commit(
            &self.paths,
            &on_disk,
            building.next,
            &building.lines,
            &building.entries,
            guard,
        )?;
        Ok(Some(out))
    }
}

impl Change {
    /// Answer open step `number` with `settled`: its line, and the step closed.
    fn settle(&mut self, number: u64, settled: Settled, at: Timestamp) {
        let Some(step) = self.next.open.remove(&number) else {
            return;
        };
        self.lines.push(AnswerLine {
            number,
            block: step.block,
            digest: step.digest,
            phase: step.phase,
            summary: step.summary,
            at,
            settled,
        });
    }

    fn owe(&mut self, work: &Work) {
        self.next.owed.extend(stops(work));
    }

    /// End the run: every open step's work owed its stop, and the numbers open at the end kept.
    fn end(&mut self, ending: Ending, at: Timestamp) {
        let open = std::mem::take(&mut self.next.open);
        for step in open.values() {
            self.owe(&step.work);
        }
        self.next.ended = Some(Ended {
            ending: ending.clone(),
            at,
            open: open.into_keys().collect(),
        });
        self.entries.push(Entry {
            at,
            event: Event::End { ending },
        });
    }
}

/// The stops a step's work needs once nothing waits on it.
fn stops(work: &Work) -> Vec<Stop> {
    match work {
        Work::Launch { launches, .. } => vec![Stop::Launches {
            launches: launches.clone(),
        }],
        Work::Command { host, child, .. } => child
            .iter()
            .map(|child| Stop::Group {
                process: child.clone(),
            })
            .chain([Stop::Process {
                process: host.clone(),
            }])
            .collect(),
        Work::Due
        | Work::Held { .. }
        | Work::Starting { .. }
        | Work::Watch
        | Work::Workflow { .. } => Vec::new(),
    }
}

/// The block a step was opened for; the engine wrote its name from a `BlockName`.
fn block_of(step: &OpenStep) -> Option<BlockName> {
    [
        BlockName::Agent,
        BlockName::Team,
        BlockName::Shell,
        BlockName::Watch,
        BlockName::Workflow,
        BlockName::Schedule,
        BlockName::Now,
    ]
    .into_iter()
    .find(|block| block.as_str() == step.block)
}

/// A step's failure, which carries an agent's handle.
fn failure(step: &OpenStep, cause: Cause, reason: impl Into<String>) -> Failure {
    let handle = match &step.work {
        Work::Launch { handle, .. } => handle.clone(),
        _ => None,
    };
    Failure {
        block: block_of(step),
        cause,
        reason: reason.into(),
        step: Some(step.number),
        code: None,
        stdout: None,
        output: None,
        handle,
    }
}

fn failed(failure: &Failure) -> Settled {
    Settled::Failure(to_value(failure))
}

fn to_value(value: &impl serde::Serialize) -> Value {
    // Every type serialized here is plain data: strings, numbers, maps with string keys.
    serde_json::to_value(value).expect("plain data serializes")
}

fn duration(raw: &str) -> Result<jiff::SignedDuration> {
    let duration = crate::harness::duration::duration(raw)?;
    Ok(jiff::SignedDuration::try_from(duration)?)
}

fn resolve_when(raw: &str, now: Timestamp, zone: &jiff::tz::TimeZone) -> Result<Timestamp> {
    let when =
        crate::harness::duration::parse_when(raw).map_err(|error| anyhow!("`give_up`: {error}"))?;
    when.resolve(now, zone)
        .map_err(|error| anyhow!("`give_up`: {error}"))
}
