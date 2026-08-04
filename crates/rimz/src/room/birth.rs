//! Room birth, health recovery, and reset transitions.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::harness::rebirth::{RebirthChoice, RebirthPlan};
use crate::harness::resume::ResumePlan;
use crate::mux::{
    BackgroundViewLaunch, BackgroundViewOptions, DaemonView, SessionHealth, SidebarPaneOptions,
};
use crate::remote_control::ReadinessSnapshot;
use crate::{StatePaths, Store};

use super::RoomContext;

/// Selected normal-room recovery state from the CLI's two-phase inspection.
pub enum NormalRebirth {
    /// Existing healthy room: preserve its durable incarnation.
    Live,
    /// Inspection failed best-effort; record a fresh boundary after session ensure.
    Fresh,
    /// Inspected recovery plan plus the user's selected disposition.
    Selected {
        plan: Box<RebirthPlan>,
        choice: RebirthChoice,
    },
}

/// What an attended caller permits when health recovery reports a stuck room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttendedRecovery {
    Reset,
    RequireExplicitReset,
}

/// Two real room birth policies.
pub enum RoomBirth {
    Normal {
        cwd: PathBuf,
        rebirth: NormalRebirth,
        /// `None` requests no configured background-view launch.
        background_view: Option<ReadinessSnapshot>,
        refresh_ms: Option<u16>,
        recovery: AttendedRecovery,
    },
    Supervised {
        cwd: PathBuf,
        recovery: AttendedRecovery,
    },
}

/// Reset details returned for CLI presentation.
#[derive(Debug)]
pub struct RoomResetReport {
    pub teardown: crate::mux::recovery::TeardownReport,
    pub records: crate::store::writer::ResetRecordsOutcome,
}

/// Health retry failed after an attended reset already changed room state.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct ResetRecoveryError {
    pub report: RoomResetReport,
    message: String,
}

/// Birth results rendered by the command boundary.
#[derive(Default)]
pub struct BirthOutcome {
    pub resume: ResumePlan,
    pub reset: Option<RoomResetReport>,
}

impl RoomContext {
    /// Execute shared room birth ordering for a normal or supervised caller.
    pub fn birth(&mut self, birth: RoomBirth) -> Result<BirthOutcome> {
        let pre_existed = match self.backend.list_sessions() {
            Ok(sessions) => sessions
                .iter()
                .any(|name| name == &self.workspace.session_name),
            Err(err) => {
                tracing::debug!(
                    session = %self.workspace.session_name,
                    error = %err,
                    "could not prove session is absent before birth; using non-destructive sidebar split",
                );
                true
            }
        };
        if !pre_existed {
            crate::sidebar::purge_rebirth_heartbeats(&self.runtime);
            if let Err(err) = crate::sidebar::width_target::clear(&self.runtime) {
                tracing::debug!(
                    workspace = %self.workspace.workspace_id,
                    error = %err,
                    "clearing room-runtime sidebar width target failed",
                );
            }
            if let Err(err) = crate::sidebar::body_filter::clear(&self.runtime) {
                tracing::debug!(
                    workspace = %self.workspace.workspace_id,
                    error = %err,
                    "clearing room-runtime sidebar body filter failed",
                );
            }
        }

        let (cwd, refresh_ms, rebirth, background_view, recovery, supervised) = match birth {
            RoomBirth::Normal {
                cwd,
                rebirth,
                background_view,
                refresh_ms,
                recovery,
            } => (
                cwd,
                refresh_ms,
                Some(rebirth),
                background_view,
                recovery,
                false,
            ),
            RoomBirth::Supervised { cwd, recovery } => (cwd, None, None, None, recovery, true),
        };
        self.backend.ensure_session(&self.session_options(&cwd))?;
        if supervised && pre_existed {
            self.detected_size = None;
        }

        let background_view = background_view
            .as_ref()
            .map(|readiness| self.background_view(readiness, refresh_ms));

        let resume = match rebirth {
            Some(rebirth) => match rebirth {
                NormalRebirth::Live => ResumePlan::default(),
                NormalRebirth::Fresh => {
                    crate::harness::rebirth::record_boundary(
                        &self.workspace.workspace_id,
                        &self.workspace.session_name,
                    );
                    ResumePlan::default()
                }
                NormalRebirth::Selected { plan, choice } => {
                    (*plan)
                        .materialize(choice, &self.workspace.session_name)
                        .resume
                }
            },
            None => {
                if !pre_existed {
                    crate::harness::rebirth::record_boundary(
                        &self.workspace.workspace_id,
                        &self.workspace.session_name,
                    );
                }
                ResumePlan::default()
            }
        };

        let sidebar = self.sidebar_options(&cwd, resume.tabs.clone(), refresh_ms);

        self.register_focus_key();

        let background_view = background_view.as_ref();
        let daemon = background_view.map(|options| &options.view);
        let mut birth_sidebar = sidebar.clone();
        birth_sidebar.pristine_birth = !pre_existed;
        let _ = crate::sidebar::launch_sidebar_if_needed(
            self.backend.as_ref(),
            &self.runtime,
            &birth_sidebar,
            daemon,
        );

        if let Some(options) = background_view {
            self.launch_background_view(options);
        }

        let reset = self.ensure_healthy(&sidebar, daemon, recovery)?;
        self.load_presence();
        Ok(BirthOutcome { resume, reset })
    }

    fn launch_background_view(&self, options: &BackgroundViewOptions) {
        crate::agents::runtime_control::ensure(
            "codex",
            self.machine_config.remote_control.enabled_for("codex"),
        );
        match self.backend.open_background_view(options) {
            Ok(BackgroundViewLaunch::Launched) => tracing::info!(
                session = %self.workspace.session_name,
                view = crate::daemon_view::VIEW_NAME,
                "launched the daemon view",
            ),
            Ok(BackgroundViewLaunch::AlreadyRunning) => {
                tracing::debug!(
                    session = %self.workspace.session_name,
                    "daemon view already present; repairing missing managed panes",
                );
                crate::daemon_view::repair_daemon_view(
                    self.backend.as_ref(),
                    &self.workspace.session_name,
                    &self.workspace.workspace_id,
                    &options.view,
                );
            }
            Err(crate::mux::MuxErr::SessionNotFound { session }) => tracing::debug!(
                session = %session,
                "daemon view deferred; session not addressable yet (pre-attach gate will rebirth it)",
            ),
            Err(err) => tracing::warn!(
                session = %self.workspace.session_name,
                error = %err,
                "daemon view launch failed; continuing without it",
            ),
        }
    }

    fn ensure_healthy(
        &self,
        sidebar: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
        recovery: AttendedRecovery,
    ) -> Result<Option<RoomResetReport>> {
        if self.clean_session(sidebar, daemon)? != SessionHealth::Stuck {
            return Ok(None);
        }
        if recovery == AttendedRecovery::RequireExplicitReset {
            anyhow::bail!(
                "The '{}' Zellij room is stuck or cannot be inspected safely enough to self-heal \
                 without a destructive reset.\n\
                 No terminal is available to confirm one. Run `rimz reset` to rebuild it cleanly.",
                self.workspace.session_name,
            );
        }
        let reset = self.reset(false)?;
        match self.clean_session(sidebar, daemon) {
            Ok(SessionHealth::Healthy | SessionHealth::Reborn) => Ok(Some(reset)),
            Ok(SessionHealth::Stuck) => Err(ResetRecoveryError {
                report: reset,
                message: "the room is still stuck after a reset; inspect with `rimz doctor`"
                    .to_owned(),
            }
            .into()),
            Err(err) => Err(ResetRecoveryError {
                report: reset,
                message: err.to_string(),
            }
            .into()),
        }
    }

    fn clean_session(
        &self,
        options: &SidebarPaneOptions,
        daemon: Option<&DaemonView>,
    ) -> Result<SessionHealth> {
        match self.backend.ensure_clean_session(options, daemon) {
            Ok(health) => Ok(health),
            Err(
                err @ (crate::mux::MuxErr::SocketPathTooLong { .. }
                | crate::mux::MuxErr::SocketPathReportedTooLong { .. }),
            ) => Err(err.into()),
            Err(err) => {
                tracing::warn!(error = %err, "session health gate failed; attaching as-is");
                Ok(SessionHealth::Healthy)
            }
        }
    }

    /// Tear down mux runtime and reset durable room records.
    pub fn reset(&self, hard: bool) -> Result<RoomResetReport> {
        let teardown = crate::mux::recovery::teardown_room(
            self.backend.as_ref(),
            &self.workspace.workspace_id,
            &self.workspace.session_name,
            &self.runtime,
        );
        let paths = StatePaths::for_workspace(self.workspace.workspace_id.clone())
            .context("preparing store paths for reset")?;
        let store = Store::open(paths, self.runtime.clone()).context("opening store for reset")?;
        store
            .record_workspace(&self.workspace)
            .context("recording workspace metadata for reset")?;
        let records = store
            .reset_records(hard)
            .context("resetting workspace records")?;
        Ok(RoomResetReport { teardown, records })
    }
}
