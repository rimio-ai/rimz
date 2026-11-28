//! The message each fire owes the agent that booked its task, the booking chain's first link:
//! the fire's outcome, once, under the message id its fire mark keeps across reports, narrowed
//! by the row's `on`. The booker is found by launch id when the report lands, so a molted
//! booker gets it at its successor and one whose launch has ended is sent nothing. A send that
//! fails is warned and never retried, and moves no strike: the loop log holds the outcome
//! either way. A fire that launched its agent
//! or team sends nothing here: the launch's own report reaches the booker through
//! `harness::fleet::report_recipient`.

use std::path::PathBuf;

use super::{LoopRunRecord, LoopRunResult, RunPolarity};
use crate::agents::LaunchedBy;
use crate::config::CheckOn;
use crate::harness::signal::WatchVerdict;
use crate::ids::{MessageId, WorkflowRunId};
use crate::store::message::HarnessNotice;
use crate::store::workflow::{RunHeader, RunOrigin};

/// Who a fire's outcome goes to, and which of its outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::harness::schedule) struct Booker {
    to: LaunchedBy,
    on: Option<CheckOn>,
    /// The id the outcome is queued under, the same on every report of one fire.
    message: MessageId,
}

impl Booker {
    pub(in crate::harness::schedule) fn of(
        booked_by: &[LaunchedBy],
        on: Option<CheckOn>,
        message: MessageId,
    ) -> Option<Self> {
        booked_by.first().map(|to| Self {
            to: to.clone(),
            on,
            message,
        })
    }

    fn wants(&self, polarity: RunPolarity) -> bool {
        match self.on {
            Some(CheckOn::Fail) => polarity == RunPolarity::Failure,
            Some(CheckOn::Success) => polarity == RunPolarity::Good,
            Some(CheckOn::Any) | None => true,
        }
    }
}

/// What a fire ran, which picks the wording of its outcome.
pub(super) enum Ran {
    /// A launch, or a fire that ended before its work started.
    Launch,
    /// A command, in the `shell` tool's wording.
    Command,
    /// A workflow run, in the starter report's wording.
    Workflow(WorkflowRunId, Box<RunHeader>),
}

impl Ran {
    pub(super) fn of(run: &WorkflowRunId, header: &RunHeader) -> Self {
        match header.origin {
            RunOrigin::Inline => Self::Command,
            _ => Self::Workflow(run.clone(), Box::new(header.clone())),
        }
    }
}

/// A fire's outcome owed to its booker, sent once the report's locks are down.
pub(in crate::harness) struct Owed {
    root: PathBuf,
    booker: Booker,
    record: LoopRunRecord,
    ran: Ran,
}

impl Owed {
    /// The outcome `record` of a fire in the room at `root`, when it is one `booker` wants. A
    /// delivery row's own message, and a row that is no outcome (a skip, a deferral), owe none,
    /// nor does a launch: the launched agent's or team's own report is the message
    /// (`harness::fleet::report_recipient`). A command or workflow run the fire started that is
    /// canceled or gives up on capacity is a failure here, as the starter report words it.
    pub(super) fn new(
        root: &std::path::Path,
        booker: Option<Booker>,
        record: &LoopRunRecord,
        ran: Ran,
    ) -> Option<Self> {
        let booker = booker?;
        let polarity = match (&ran, record.result) {
            (
                Ran::Command | Ran::Workflow(..),
                LoopRunResult::Canceled | LoopRunResult::ThrottleSkipped,
            ) => RunPolarity::Failure,
            _ => record.polarity()?,
        };
        let launched = record.result == LoopRunResult::Launched;
        (!launched && record.watch.is_none() && record.wakes.is_none() && booker.wants(polarity))
            .then(|| Self {
                root: root.to_path_buf(),
                booker,
                record: record.clone(),
                ran,
            })
    }
}

/// Queue `owed` for its booker's live row; a failure is warned, since nothing retries it.
pub(super) fn send(owed: Option<Owed>) {
    let Some(owed) = owed else {
        return;
    };
    let sent = crate::StatePaths::for_project_root(&owed.root)
        .map_err(anyhow::Error::from)
        .and_then(|state| {
            super::starter::queue_notice(
                &state,
                &owed.booker.to,
                text(&state, &owed.record, &owed.ran),
                HarnessNotice::Wait,
                &owed.booker.message,
            )
        });
    if let Err(error) = sent {
        tracing::warn!(task = owed.record.task, error = %format!("{error:#}"), "a fire's outcome not sent to its booker; the loop log holds it");
    }
}

/// The booking's id, then how the fire ended.
fn text(state: &crate::StatePaths, record: &LoopRunRecord, ran: &Ran) -> String {
    let booking = format!("schedule:{}", record.task);
    match ran {
        Ran::Workflow(run, header) => {
            return format!("{booking}\n{}", super::starter::text(state, run, header));
        }
        Ran::Command => {
            if let Some(check) = &record.check {
                let elapsed_ms = check.duration_ms.or(record.duration_ms).unwrap_or(0);
                let verdict = if check.timed_out {
                    WatchVerdict::TimedOut { elapsed_ms }
                } else {
                    WatchVerdict::Exited {
                        code: check.code,
                        elapsed_ms,
                    }
                };
                let mut line = format!("{booking} · {}", verdict.label());
                if let Some(output) = &check.output_path {
                    line.push_str(&format!(" · output: {}", output.display()));
                }
                return line;
            }
        }
        Ran::Launch => {}
    }
    let mut head = format!("{booking} · {}", record.result.label());
    if let Some(target) = &record.target {
        head.push_str(&format!(" {target}"));
    }
    std::iter::once(head)
        .chain(record.error.clone())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::schedule::run_log::LoopRunMode;

    /// A run a fire started that is canceled or gives up on capacity reaches its booker, as a
    /// failure under `on`; a skip that started no work stays silent.
    #[test]
    fn a_canceled_or_given_up_run_reaches_its_booker_as_a_failure() {
        let to = LaunchedBy {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            agent_id: "booker".into(),
        };
        let owes = |on: Option<CheckOn>, result: LoopRunResult, ran: Ran| {
            let booker = Booker::of(std::slice::from_ref(&to), on, MessageId::new());
            let record = LoopRunRecord::new("nightly", result, LoopRunMode::Scheduled, 0);
            Owed::new(std::path::Path::new("/repo"), booker, &record, ran).is_some()
        };
        let workflow = || Ran::Workflow(WorkflowRunId::new(), Box::default());
        for result in [LoopRunResult::Canceled, LoopRunResult::ThrottleSkipped] {
            for on in [None, Some(CheckOn::Fail)] {
                assert!(owes(on, result, Ran::Command), "{on:?} {result:?}");
                assert!(owes(on, result, workflow()), "{on:?} {result:?}");
            }
            assert!(
                !owes(Some(CheckOn::Success), result, workflow()),
                "{result:?}"
            );
            assert!(!owes(None, result, Ran::Launch), "{result:?}");
        }
    }
}
