//! Waiting on workflow runs: one look at a run's home, and the poll that repeats it until the
//! runs end, one needs a person, the caller interrupts, or the deadline passes. Nothing here
//! prints or exits; the caller renders what comes back.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::disk::paths::{self, StatePaths};
use crate::ids::WorkflowRunId;
use crate::store::workflow::{self as records, FoundRun, RunClass, RunEnding, RunRecord, RunState};

const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// What one look at a run's home found.
#[derive(Clone, Debug)]
pub enum Reading {
    Running {
        state: RunState,
        node: Option<String>,
    },
    /// The run waits on a person; the caller words why from its record.
    NeedsYou {
        record: Box<RunRecord>,
        definition_dir: PathBuf,
    },
    Ended(Ended),
}

/// A home this RimZ cannot read: a record of a newer version, or one holding no JSON object.
/// The caller words the refusal.
#[derive(Clone, Debug)]
pub struct Unreadable {
    pub state: StatePaths,
    pub id: WorkflowRunId,
    pub found: FoundRun,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub state: RunState,
    pub ending: Option<RunEnding>,
    pub checkout: Option<PathBuf>,
    pub notes: Option<PathBuf>,
    pub terminal_at: Option<jiff::Timestamp>,
}

impl Ended {
    /// A run whose home was deleted after the wait first found it: only a quiet task run's
    /// home is deleted at its end, and only after it ended done.
    pub fn quiet() -> Self {
        Self {
            state: RunState::Done,
            ending: None,
            checkout: None,
            notes: None,
            terminal_at: None,
        }
    }
}

/// One look at a run's home, or none once the home is gone. A record this RimZ cannot read
/// comes back as `Err(Unreadable)` rather than a guessed ending; a stranded run reads as
/// running until the pass that ends it.
pub fn look(id: &WorkflowRunId) -> Result<Option<Result<Reading, Box<Unreadable>>>> {
    let Some((state, found)) = records::find_under(&paths::rimz_home(), id)? else {
        return Ok(None);
    };
    let notes = |relative: Option<&PathBuf>| {
        relative.map(|relative| state.notes_run(paths::Class::AutomationNotes, relative))
    };
    let reading = match &found.class {
        RunClass::Current(record) if found.finished || record.state.is_terminal() => {
            Reading::Ended(Ended {
                state: record.state,
                ending: record
                    .outcome
                    .as_ref()
                    .map(|outcome| outcome.ending.clone()),
                checkout: Some(record.scope.path.clone()),
                notes: notes(record.notes.as_ref()),
                terminal_at: record.terminal_at,
            })
        }
        RunClass::Current(record) if record.needs_you() => Reading::NeedsYou {
            definition_dir: record.definition_dir(&found.paths),
            record: record.clone(),
        },
        RunClass::Current(record) => Reading::Running {
            state: record.state,
            node: Some(record.node.clone()),
        },
        RunClass::Ended(header) => {
            let Some(state) = header.state.filter(|state| state.is_terminal()) else {
                bail!("workflow run {id} has ended, and this RimZ cannot read how");
            };
            Reading::Ended(Ended {
                state,
                ending: header.ending.clone(),
                checkout: header.scope_path.clone(),
                notes: notes(header.notes.as_ref()),
                terminal_at: header.terminal_at,
            })
        }
        RunClass::Stranded { header, .. } => Reading::Running {
            state: header.state.unwrap_or(RunState::Active),
            node: header.node.clone(),
        },
        RunClass::Recordless => Reading::Running {
            state: RunState::Active,
            node: None,
        },
        RunClass::Newer { .. } | RunClass::NotObject => {
            return Ok(Some(Err(Box::new(Unreadable {
                state,
                id: id.clone(),
                found,
            }))));
        }
    };
    Ok(Some(Ok(reading)))
}

/// A run the join watches and its latest reading.
#[derive(Clone, Debug)]
pub struct JoinTarget {
    pub id: WorkflowRunId,
    pub reading: Reading,
}

/// Why [`join`] returned. The targets hold each run's latest reading.
#[derive(Debug)]
pub enum Joined<I> {
    /// Every run ended, or under `any` the run at this index ended first.
    Ended { first: Option<usize> },
    /// A run needs a person.
    NeedsYou,
    /// The caller's interrupt answered this.
    Interrupted(I),
    /// The deadline passed with runs still going; they are left running.
    StillRunning,
    /// A run's home became unreadable.
    Unreadable(Box<Unreadable>),
}

/// Reads each unended target every poll until they all end (under `any`, until one does), one
/// needs a person, `interrupt` answers, or `timeout` passes since the call. `interrupt` is
/// read once per poll, after the readings; nothing is held locked across the sleep.
pub fn join<I>(
    targets: &mut [JoinTarget],
    any: bool,
    timeout: Option<Duration>,
    interrupt: &mut dyn FnMut() -> Option<I>,
) -> Result<Joined<I>> {
    let started = Instant::now();
    loop {
        for target in targets.iter_mut() {
            if matches!(target.reading, Reading::Ended(_)) {
                continue;
            }
            target.reading = match look(&target.id)? {
                None => Reading::Ended(Ended::quiet()),
                Some(Ok(reading)) => reading,
                Some(Err(unreadable)) => return Ok(Joined::Unreadable(unreadable)),
            };
        }
        let first_ended = targets
            .iter()
            .enumerate()
            .filter_map(|(index, target)| match &target.reading {
                Reading::Ended(ended) => Some((index, ended.terminal_at)),
                _ => None,
            })
            .min_by_key(|(_, terminal_at)| (terminal_at.is_none(), *terminal_at));
        if any && let Some((index, _)) = first_ended {
            return Ok(Joined::Ended { first: Some(index) });
        }
        if targets
            .iter()
            .all(|target| matches!(target.reading, Reading::Ended(_)))
        {
            return Ok(Joined::Ended { first: None });
        }
        if targets
            .iter()
            .any(|target| matches!(target.reading, Reading::NeedsYou { .. }))
        {
            return Ok(Joined::NeedsYou);
        }
        if let Some(answer) = interrupt() {
            return Ok(Joined::Interrupted(answer));
        }
        if let Some(timeout) = timeout
            && started.elapsed() >= timeout
        {
            return Ok(Joined::StillRunning);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
