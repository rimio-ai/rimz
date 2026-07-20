//! Room birth, health recovery, and reset transitions.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::harness::rebirth::{RebirthChoice, RebirthPlan};
use crate::harness::resume::ResumePlan;
use crate::mux::{BackgroundViewLaunch, BackgroundViewOptions, DaemonView, SessionHealth};
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

/// Whether normal room birth carries the configured daemon view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundViewBirth {
    Launch,
    Skip,
}

/// Normal start/attach birth inputs.
pub struct NormalBirth {
    pub cwd: PathBuf,
    pub rebirth: NormalRebirth,
    pub background_view: BackgroundViewBirth,
    pub refresh_ms: Option<u16>,
    pub recovery: AttendedRecovery,
}

/// Supervised-run birth inputs.
pub struct SupervisedBirth {
    pub cwd: PathBuf,
    pub recovery: AttendedRecovery,
}

/// Two real room birth policies.
pub enum RoomBirth {
    Normal(NormalBirth),
    Supervised(SupervisedBirth),
}

/// Reset details returned for CLI presentation.
#[derive(Debug)]
pub struct RoomResetReport {
    pub teardown: crate::mux::recovery::TeardownReport,
    pub records: crate::store::ResetRecordsOutcome,
}

/// Health retry failed after an attended reset already changed room state.
#[derive(Debug)]
pub struct ResetRecoveryError {
    pub report: RoomResetReport,
    message: String,
}

impl std::fmt::Display for ResetRecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ResetRecoveryError {}

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
            if let Err(err) = crate::sidebar::width_override::clear(&self.runtime) {
                tracing::debug!(
                    workspace = %self.workspace.workspace_id,
                    error = %err,
                    "clearing room-runtime sidebar width override failed",
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
            RoomBirth::Normal(normal) => (
                normal.cwd,
                normal.refresh_ms,
                Some(normal.rebirth),
                normal.background_view,
                normal.recovery,
                false,
            ),
            RoomBirth::Supervised(supervised) => (
                supervised.cwd,
                None,
                None,
                BackgroundViewBirth::Skip,
                supervised.recovery,
                true,
            ),
        };
        self.backend.ensure_session(&self.session_options(&cwd))?;
        if supervised && pre_existed {
            self.detected_size = None;
        }

        let background_view = match background_view {
            BackgroundViewBirth::Launch => Some(self.background_view(refresh_ms)),
            BackgroundViewBirth::Skip => None,
        };

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

        self.register_focus_key();

        let background_view = background_view.as_ref();
        let daemon = background_view.map(|options| &options.view);
        self.launch_sidebar(&cwd, refresh_ms, !pre_existed, &resume, daemon);

        if let Some(options) = background_view {
            self.launch_background_view(options);
        }

        let reset = self.ensure_healthy(&cwd, refresh_ms, &resume, daemon, recovery)?;
        self.load_presence();
        Ok(BirthOutcome { resume, reset })
    }

    fn launch_sidebar(
        &self,
        cwd: &std::path::Path,
        refresh_ms: Option<u16>,
        pristine_birth: bool,
        resume: &ResumePlan,
        daemon: Option<&DaemonView>,
    ) {
        let mut opts = self.sidebar_options(cwd, resume.tabs.clone(), refresh_ms);
        opts.pristine_birth = pristine_birth;
        let _ = crate::sidebar::launch_sidebar_if_needed(
            self.backend.as_ref(),
            &self.runtime,
            &opts,
            daemon,
        );
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
        cwd: &std::path::Path,
        refresh_ms: Option<u16>,
        resume: &ResumePlan,
        daemon: Option<&DaemonView>,
        recovery: AttendedRecovery,
    ) -> Result<Option<RoomResetReport>> {
        if self.clean_session(cwd, refresh_ms, resume, daemon)? != SessionHealth::Stuck {
            return Ok(None);
        }
        if recovery == AttendedRecovery::RequireExplicitReset {
            return Err(ResetRequired {
                session: self.workspace.session_name.clone(),
            }
            .into());
        }
        let reset = self.reset(false)?;
        match self.clean_session(cwd, refresh_ms, resume, daemon) {
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
        cwd: &std::path::Path,
        refresh_ms: Option<u16>,
        resume: &ResumePlan,
        daemon: Option<&DaemonView>,
    ) -> Result<SessionHealth> {
        let opts = self.sidebar_options(cwd, resume.tabs.clone(), refresh_ms);
        match self.backend.ensure_clean_session(&opts, daemon) {
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
            .reset_records(&self.workspace.session_name, hard)
            .context("resetting workspace records")?;
        Ok(RoomResetReport { teardown, records })
    }
}

/// No terminal is available to confirm destructive reset of a stuck room.
#[derive(Debug)]
struct ResetRequired {
    session: String,
}

impl std::fmt::Display for ResetRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The '{}' Zellij room is stuck or cannot be inspected safely enough to self-heal \
             without a destructive reset.\n\
             No terminal is available to confirm one. Run `rimz reset` to rebuild it cleanly.",
            self.session,
        )
    }
}

impl std::error::Error for ResetRequired {}
