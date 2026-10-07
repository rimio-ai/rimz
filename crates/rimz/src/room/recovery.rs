//! Reconcile parked recovery on entry to an already-running room.

use super::RoomContext;
use crate::disk::lock::WorkspaceLock;
use crate::harness::rebirth::{RebirthDisposition, RebirthPlan};

#[derive(Default)]
pub struct ParkedRecoveryOutcome {
    pub parked: usize,
    pub labels: Vec<String>,
    pub ended: usize,
    pub warnings: Vec<String>,
}

fn parked_disposition(attended: bool, recovery_off: bool) -> RebirthDisposition {
    if attended && recovery_off {
        RebirthDisposition::Decline
    } else {
        RebirthDisposition::Defer
    }
}

impl RoomContext {
    /// Settle filled seats and an attended decline, leaving other agents parked.
    pub fn reconcile_parked_recovery(
        &self,
        no_resume: bool,
        attended: bool,
    ) -> anyhow::Result<ParkedRecoveryOutcome> {
        let _guard = WorkspaceLock::acquire(&self.runtime.lock_path("recovery.lock"))?;
        let plan = RebirthPlan::inspect_live(
            self.workspace_id(),
            &self.workspace.project_root,
            &self.machine_config,
            no_resume,
        )?;
        if plan.is_empty() {
            return Ok(ParkedRecoveryOutcome::default());
        }
        let preview = plan.preview();
        let disposition = parked_disposition(attended, preview.recovery_off());
        let seeded = plan.settle(disposition, self.session_name());
        let ended = seeded.declined_count();
        let resume = seeded.confirm(|_, _| Ok::<(), std::convert::Infallible>(()));
        Ok(ParkedRecoveryOutcome {
            parked: if disposition == RebirthDisposition::Decline {
                0
            } else {
                preview.candidate_count()
            },
            labels: preview.labels().to_vec(),
            ended,
            warnings: resume.warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_disposition_declines_only_an_attended_recovery_off_entry() {
        for (attended, recovery_off, expected) in [
            (true, true, RebirthDisposition::Decline),
            (true, false, RebirthDisposition::Defer),
            (false, true, RebirthDisposition::Defer),
            (false, false, RebirthDisposition::Defer),
        ] {
            assert_eq!(parked_disposition(attended, recovery_off), expected);
        }
    }
}
