//! A task's fires in its record: the open mark each fire writes for its checkout before it
//! launches, the launch proof of each firing checkout's newest scheduled launch, the summary of
//! the task's quiet fires (whose runs leave no history row and no home), and the one write that
//! settles a fire's outcome.
//!
//! Reads take no lock, since the clock pass reads the launch proof from the sidebar's import
//! graph.

use std::path::Path;

use jiff::Timestamp;

use super::catalog::TaskSource;
use super::run_log::{LoopRunRecord, LoopRunResult, QuietFires};
use super::strikes::{self, Signal};
use super::task_record::{self, Binding, FireMark, TaskRecord, TaskRecordErr};
use super::task_record_store::{self, TaskStoreErr};
use crate::ids::{FireId, WorkflowRunId};

pub(super) type Result<T> = std::result::Result<T, TaskStoreErr>;

fn checkout_key(checkout: Option<&Path>) -> String {
    checkout.map_or_else(String::new, |path| path.display().to_string())
}

/// Whether a scheduled launch of the task bound to `key` (a one-off retired at its fire
/// included) was stamped at or after `since`: by `checkout` when one is given, by any checkout
/// otherwise.
pub(super) fn scheduled_since(
    key: &str,
    checkout: Option<&Path>,
    since: Timestamp,
) -> std::result::Result<bool, TaskRecordErr> {
    let records = task_record::load()?;
    let Some((_, record)) = records.named(key) else {
        return Ok(false);
    };
    let stamps = &record.scheduled;
    Ok(match checkout {
        Some(_) => stamps
            .get(&checkout_key(checkout))
            .is_some_and(|at| *at >= since),
        None => stamps.values().any(|at| *at >= since),
    })
}

/// What a fire records about itself before it launches.
pub(super) struct Opening<'a> {
    pub(super) checkout: &'a Path,
    pub(super) booked_by: Vec<crate::agents::LaunchedBy>,
    pub(super) once: bool,
    pub(super) scope: crate::store::workflow::ScopeKind,
    pub(super) at: Timestamp,
}

/// Bind the task, mint the fire's id and write its open mark for the checkout, in one write.
/// Returns the mark it displaced, which [`withdraw`] puts back for a fire that publishes no run.
pub(super) fn open(binding: &Binding, opening: Opening<'_>) -> Result<(FireId, Option<FireMark>)> {
    task_record_store::mutate(|records| {
        let (task, record) = records.bind(binding);
        let fire = FireId::new(&task);
        let displaced = record.marks.insert(
            opening.checkout.to_owned(),
            FireMark {
                fire: fire.clone(),
                at: opening.at,
                booked_by: opening.booked_by,
                once: opening.once,
                run: None,
                scope: opening.scope,
                result: None,
            },
        );
        (fire, displaced)
    })
}

/// Take back the open mark of a fire that published no run, restoring the mark it displaced;
/// a mark another fire has since written stays.
pub(super) fn withdraw(fire: &FireId, displaced: Option<FireMark>) -> Result<()> {
    task_record_store::mutate(|records| {
        let Some(record) = records.get_mut(&fire.task()) else {
            return;
        };
        let Some(checkout) = record
            .marks
            .iter()
            .find(|(_, mark)| &mark.fire == fire)
            .map(|(checkout, _)| checkout.clone())
        else {
            return;
        };
        match displaced {
            Some(mark) => record.marks.insert(checkout, mark),
            None => record.marks.remove(&checkout),
        };
    })
}

/// Note the run a fire published and, for a scheduled fire, stamp the task's launch proof.
pub(super) fn launched(
    fire: &FireId,
    run: &WorkflowRunId,
    scheduled: Option<(Option<&Path>, Timestamp)>,
) -> Result<()> {
    task_record_store::mutate(|records| {
        let Some(record) = records.get_mut(&fire.task()) else {
            return;
        };
        if let Some(mark) = mark_of(record, fire) {
            mark.run = Some(run.clone());
        }
        let Some((checkout, at)) = scheduled else {
            return;
        };
        let stamp = record.scheduled.entry(checkout_key(checkout)).or_insert(at);
        if *stamp < at {
            *stamp = at;
        }
    })
}

fn mark_of<'a>(record: &'a mut TaskRecord, fire: &FireId) -> Option<&'a mut FireMark> {
    record.marks.values_mut().find(|mark| &mark.fire == fire)
}

/// A fire's outcome, settled in one write of its task's record.
pub(super) struct Outcome<'a> {
    pub(super) fire: &'a FireId,
    pub(super) result: LoopRunResult,
    pub(super) signal: Signal,
    /// The row a quiet fire leaves in the task's quiet summary instead of in history.
    pub(super) quiet: Option<LoopRunRecord>,
    /// A failure row ends the quiet streak; its count and last output stay.
    pub(super) ends_streak: bool,
    /// The strike threshold and the task's source, for a task that can strike out.
    pub(super) strike_out: Option<(u32, TaskSource)>,
    pub(super) now: Timestamp,
}

/// What a settled outcome did to the task's count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Settled {
    pub(super) count: u32,
    /// Whether this report moved the count: a fire's second report moves nothing.
    pub(super) counted: bool,
    pub(super) disabled: bool,
}

/// Move the strike count once per fire id, note the quiet summary, close the fire's mark and
/// disable a task that struck out, in one write. `None` when the task has no record.
pub(super) fn settle(outcome: Outcome<'_>) -> Result<Option<Settled>> {
    task_record_store::mutate(|records| {
        records
            .get_mut(&outcome.fire.task())
            .map(|record| settle_record(record, outcome))
    })
}

fn settle_record(record: &mut TaskRecord, outcome: Outcome<'_>) -> Settled {
    if let Some(mark) = mark_of(record, outcome.fire) {
        mark.result.get_or_insert(outcome.result);
    }
    let (count, counted) = strikes::apply(record, Some(outcome.fire), outcome.signal);
    if !counted {
        return Settled {
            count,
            counted,
            disabled: false,
        };
    }
    if let Some(last) = outcome.quiet {
        note_quiet(&mut record.quiet, last);
    }
    if outcome.ends_streak
        && let Some(quiet) = record.quiet.as_mut()
    {
        quiet.streak = 0;
    }
    let disabled = outcome.signal == Signal::Strike
        && outcome.strike_out.is_some_and(|(max, source)| {
            count >= max
                && super::arming::disabled_if_live(
                    &mut record.arming,
                    source,
                    Some(count),
                    outcome.now,
                )
        });
    Settled {
        count,
        counted,
        disabled,
    }
}

/// Count one more quiet fire, whose row `last` replaces the stored one unless that is newer: a
/// collection that lands late still counts.
fn note_quiet(quiet: &mut Option<QuietFires>, last: LoopRunRecord) {
    let (count, streak) = quiet
        .as_ref()
        .map_or((0, 0), |quiet| (quiet.count, quiet.streak));
    let last = match quiet.take() {
        Some(stored) if stored.last.at > last.at => stored.last,
        _ => last,
    };
    *quiet = Some(QuietFires {
        count: count.saturating_add(1),
        streak: streak.saturating_add(1),
        last,
    });
}

/// A failure row with no fire ends the task's quiet streak; a failed write leaves the streak to
/// the next one.
pub(super) fn reset_streak(binding: &Binding) -> Result<()> {
    let streaking = task_record::load()?
        .live(&binding.key())
        .and_then(|(_, record)| record.quiet.as_ref())
        .is_some_and(|quiet| quiet.streak != 0);
    if !streaking {
        return Ok(());
    }
    task_record_store::mutate(|records| {
        if let Some(quiet) = records
            .live_mut(&binding.key())
            .and_then(|record| record.quiet.as_mut())
        {
            quiet.streak = 0;
        }
    })
}

#[cfg(test)]
#[path = "fires/tests.rs"]
mod tests;
