//! Finish an attended recovery after the session acquires a client.

use std::fs::OpenOptions;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{RoomContext, RoomSizing};
use crate::StatePaths;
use crate::config::MachineConfig;
use crate::disk::lock::WorkspaceLock;
use crate::harness::rebirth::{RebirthDisposition, RebirthPlan, RecoveryConsent};
use crate::harness::resume::ResumePlan;
use crate::ids::{MuxName, WorkspaceId};

const ATTACH_WAIT: Duration = Duration::from_secs(60);

/// A previously answered prompt, scoped to the agents it offered.
#[derive(Serialize, Deserialize)]
pub struct DeferredRecovery {
    workspace_id: WorkspaceId,
    mux: MuxName,
    consent: RecoveryConsent,
}

impl RoomContext {
    pub fn defer_parked_recovery(
        &self,
        plan: RebirthPlan,
        disposition: RebirthDisposition,
    ) -> std::io::Result<()> {
        let request = DeferredRecovery {
            workspace_id: self.workspace_id().clone(),
            mux: self.mux_name(),
            consent: plan.consent(disposition),
        };
        let paths = StatePaths::for_workspace(self.workspace_id().clone())
            .map_err(std::io::Error::other)?;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.recovery_log())?;
        let mut command =
            crate::child_process::detached_rimz_command(self.rimz_bin.clone(), &self.runtime);
        command
            .args([
                "recover-parked",
                "--request",
                &serde_json::to_string(&request)?,
            ])
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        crate::child_process::spawn_detached_reaped(&mut command, "parked-recovery")?;
        Ok(())
    }

    pub fn complete_deferred_recovery(
        request: DeferredRecovery,
        machine: Arc<MachineConfig>,
    ) -> anyhow::Result<ResumePlan> {
        let paths = StatePaths::for_workspace(request.workspace_id)?;
        let record = crate::workspace::record::read(&paths.workspace_record)?;
        let context = Self::from_record(&record, machine, request.mux, RoomSizing::OrdinaryTab)?;
        let deadline = Instant::now() + ATTACH_WAIT;
        while Instant::now() < deadline {
            if context.backend.can_open_tab(context.session_name()) {
                return context.settle_recovery_consent(request.consent);
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Ok(ResumePlan {
            warnings: vec![
                "recovery timed out waiting for a client; agents stay parked for the next start"
                    .into(),
            ],
            ..Default::default()
        })
    }

    pub(super) fn settle_recovery_consent(
        &self,
        consent: RecoveryConsent,
    ) -> anyhow::Result<ResumePlan> {
        // Serialize inline and detached settlements, but never hold the store's
        // write lock while opening panes or appending lifecycle observations.
        let _guard = WorkspaceLock::acquire(&self.runtime.lock_path("recovery.lock"))?;
        let plan = consent.inspect(
            StatePaths::for_workspace(self.workspace_id().clone())?,
            self.runtime.clone(),
            &self.workspace.project_root,
            &self.machine_config,
        )?;
        Ok(plan
            .settle(consent.disposition, self.session_name())
            .confirm(|_, tab| self.open_resume_tab(tab.clone(), false)))
    }
}
