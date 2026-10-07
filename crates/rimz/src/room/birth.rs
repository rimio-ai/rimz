//! Room birth, health recovery, and reset transitions.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::config::Isolation;
use crate::disk::lock::WorkspaceLock;
use crate::harness::rebirth::{RebirthDisposition, RebirthPlan};
use crate::harness::resume::ResumePlan;
use crate::mux::{
    BackgroundViewLaunch, BackgroundViewOptions, DaemonView, SessionHealth, SidebarPaneOptions,
};
use crate::proc::ProcInfo;
use crate::remote_control::ReadinessSnapshot;
use crate::{StatePaths, Store};

use super::RoomContext;

fn birth_session_name(
    recorded: &str,
    dir_name: &str,
    sessions: crate::mux::Result<Vec<String>>,
) -> (String, bool) {
    match sessions {
        Ok(live) if live.iter().any(|name| name == recorded) => (recorded.to_owned(), true),
        Ok(live) => (
            dir_name.to_owned(),
            live.iter().any(|name| name == dir_name),
        ),
        Err(err) => {
            tracing::debug!(session = recorded, error = %err, "could not prove session is absent before birth; using non-destructive sidebar split");
            (recorded.to_owned(), true)
        }
    }
}

/// Selected normal-room recovery state from the CLI's two-phase inspection.
pub enum NormalRebirth {
    /// Existing healthy room: preserve its durable incarnation.
    Live,
    /// Inspection failed best-effort; record a fresh boundary after session ensure.
    Fresh,
    /// Inspected recovery plan plus the user's selected disposition.
    Selected {
        plan: Box<RebirthPlan>,
        disposition: RebirthDisposition,
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
        /// Health already proved for a session that was live before birth.
        preflight_health: Option<SessionHealth>,
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
    pub teardown: crate::room::teardown::TeardownReport,
    /// Processes a hard reset ended because they still ran inside one of the
    /// room's sandbox views.
    pub view_processes_ended: Vec<u32>,
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
        let (cwd, refresh_ms, rebirth, preflight_health, background_view, recovery, supervised) =
            match birth {
                RoomBirth::Normal {
                    cwd,
                    rebirth,
                    preflight_health,
                    background_view,
                    refresh_ms,
                    recovery,
                } => (
                    cwd,
                    refresh_ms,
                    Some(rebirth),
                    preflight_health,
                    background_view,
                    recovery,
                    false,
                ),
                RoomBirth::Supervised { cwd, recovery } => {
                    (cwd, None, None, None, None, recovery, true)
                }
            };
        let (session_name, pre_existed) = birth_session_name(
            &self.workspace.session_name,
            self.runtime.dir_name.as_str(),
            self.backend.list_sessions(),
        );
        let renamed = session_name != self.workspace.session_name;
        self.workspace.session_name = session_name;
        if !pre_existed {
            if Isolation::ambient(&crate::agents::ambient_env()) == Some(Isolation::Sandbox) {
                bail!("this pane runs inside a RimZ sandbox; start the room from a host shell");
            }
            crate::sidebar::purge_rebirth_heartbeats(&self.runtime);
            if let Err(err) = crate::mux::width_target::clear(&self.runtime) {
                tracing::debug!(
                    workspace = %self.workspace.workspace_id,
                    error = %err,
                    "clearing room-runtime sidebar width target failed",
                );
            }
            if let Err(err) = crate::sidebar::body_filter::write(&self.runtime, &Default::default())
            {
                tracing::debug!(
                    workspace = %self.workspace.workspace_id,
                    error = %err,
                    "clearing room-runtime sidebar body filter failed",
                );
            }
        }

        #[cfg(unix)]
        {
            let home = crate::disk::paths::rimz_home();
            let link = home.join("run");
            let target = crate::disk::paths::runtime_rimz_root();
            // A link left by another runtime root (an SSH login without
            // XDG_RUNTIME_DIR) is repointed; anything but a link stays put.
            let linked =
                std::fs::create_dir_all(&home).and_then(|()| match std::fs::read_link(&link) {
                    Ok(current) if current == target => Ok(()),
                    Ok(_) => std::fs::remove_file(&link)
                        .and_then(|()| std::os::unix::fs::symlink(&target, &link)),
                    Err(_) => std::os::unix::fs::symlink(&target, &link),
                });
            if let Err(err) = linked
                && err.kind() != std::io::ErrorKind::AlreadyExists
            {
                tracing::debug!(error = %err, "creating home runtime link failed");
            }
        }
        if renamed {
            let paths = StatePaths::for_project_root(&self.workspace.project_root)
                .context("preparing store paths for birth")?;
            Store::open(paths, self.runtime.clone())
                .context("opening store for birth")?
                .record_workspace(&self.workspace)
                .context("recording the born session name")?;
        }
        if matches!(
            rebirth,
            Some(NormalRebirth::Fresh | NormalRebirth::Selected { .. })
        ) || (supervised && !pre_existed)
        {
            let paths = StatePaths::for_workspace(self.workspace.workspace_id.clone())?;
            crate::harness::rebirth::park_roster(&paths)?;
        }
        self.backend.ensure_session(&self.session_options(&cwd))?;
        if supervised && pre_existed {
            self.detected_size = None;
        }

        let background_view = background_view
            .as_ref()
            .map(|readiness| self.background_view(readiness, refresh_ms));

        // Resumed agents stay pending until confirmation; the lock keeps an
        // attended start on the now-live room from settling them a second time.
        let _recovery = match rebirth {
            Some(NormalRebirth::Selected { .. }) => Some(WorkspaceLock::acquire(
                &self.runtime.lock_path("recovery.lock"),
            )?),
            _ => None,
        };
        let seeded = match rebirth {
            Some(rebirth) => match rebirth {
                NormalRebirth::Live => None,
                NormalRebirth::Fresh => {
                    crate::harness::rebirth::record_boundary(
                        &self.workspace.workspace_id,
                        &self.workspace.session_name,
                    );
                    None
                }
                NormalRebirth::Selected { plan, disposition } => {
                    Some((*plan).settle(disposition, &self.workspace.session_name))
                }
            },
            None => {
                if !pre_existed {
                    crate::harness::rebirth::record_boundary(
                        &self.workspace.workspace_id,
                        &self.workspace.session_name,
                    );
                }
                None
            }
        };

        let resume_tabs = seeded
            .iter()
            .flat_map(|seeded| seeded.tabs())
            .cloned()
            .collect();
        let sidebar = self.sidebar_options_with_resume(&cwd, resume_tabs, refresh_ms);

        self.register_room_keys();

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

        let health = self.ensure_healthy(&sidebar, daemon, recovery, preflight_health);
        // Every seeding site has run by now, on a failed health gate too, so
        // the session itself says which resume tabs opened.
        let resume = seeded
            .map(|seeded| {
                let mut outcomes = self
                    .backend
                    .confirm_resume_tabs(&self.workspace.session_name, seeded.tabs())
                    .into_iter();
                seeded.confirm(|_, _| {
                    outcomes
                        .next()
                        .unwrap_or(Err(crate::mux::ResumeTabUnconfirmed::Absent))
                })
            })
            .unwrap_or_default();
        let reset = health?;
        self.load_presence();
        Ok(BirthOutcome { resume, reset })
    }

    fn launch_background_view(&self, options: &BackgroundViewOptions) {
        match StatePaths::for_project_root(&self.workspace.project_root) {
            Ok(state) => match crate::agents::room_account(
                &state.workspace_record,
                &self.machine_config,
                &crate::ids::AgentKind::new_unchecked("codex"),
            )
            .map(|login| login.env(&crate::agents::ambient_env()))
            {
                Ok(login_env) => crate::agents::runtime_control::ensure(
                    "codex",
                    self.machine_config.remote_control.enabled_for("codex"),
                    &login_env,
                ),
                Err(err) => {
                    tracing::warn!(workspace = %self.workspace.workspace_id, error = %err, "Codex daemon account unavailable; skipping ensure")
                }
            },
            Err(err) => {
                tracing::warn!(workspace = %self.workspace.workspace_id, error = %err, "Codex daemon state paths unavailable; skipping ensure")
            }
        }
        match self.backend.open_background_view(options) {
            Ok(BackgroundViewLaunch::Launched) => tracing::info!(
                session = %self.workspace.session_name,
                view = crate::pane::VIEW_NAME,
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
        preflight_health: Option<SessionHealth>,
    ) -> Result<Option<RoomResetReport>> {
        let health = match preflight_health {
            Some(health) => health,
            None => self.clean_session(sidebar, daemon)?,
        };
        match health {
            SessionHealth::Healthy | SessionHealth::Reborn => return Ok(None),
            SessionHealth::Unresponsive => {
                return Err(super::unresponsive_error(
                    &self.workspace.session_name,
                    super::SessionOwnership::Managed,
                ));
            }
            SessionHealth::Stuck => {}
        }
        if recovery == AttendedRecovery::RequireExplicitReset {
            anyhow::bail!(
                "The '{}' Zellij room is stuck or cannot be inspected safely enough to self-heal \
                 without a destructive reset.\n\
                 No terminal is available to confirm one. Run `rimz reset` to rebuild it cleanly.",
                self.workspace.session_name,
            );
        }
        // A recovery reset rebuilds the room this birth already froze accounts
        // for; unlike `rimz reset`, it must not unfreeze them.
        let reset = self.reset_with(false, Store::reset_records_keeping_logins)?;
        match self.clean_session(sidebar, daemon) {
            Ok(SessionHealth::Healthy | SessionHealth::Reborn) => Ok(Some(reset)),
            Ok(SessionHealth::Stuck | SessionHealth::Unresponsive) => Err(ResetRecoveryError {
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

    /// Tear down mux runtime and reset durable room records. A hard reset
    /// first ends the processes still inside the room's sandbox views, and
    /// refuses before any store write when one survives.
    pub fn reset(&self, hard: bool) -> Result<RoomResetReport> {
        self.reset_with(hard, |store| store.reset_records(hard))
    }

    fn reset_with(
        &self,
        end_view_processes: bool,
        reset_records: impl FnOnce(
            &Store,
        )
            -> crate::store::Result<crate::store::writer::ResetRecordsOutcome>,
    ) -> Result<RoomResetReport> {
        let paths = StatePaths::for_project_root(&self.workspace.project_root)
            .context("preparing store paths for reset")?;
        let teardown = crate::room::teardown::teardown_room(
            self.backend.as_ref(),
            &self.workspace.workspace_id,
            &self.workspace.session_name,
            &self.runtime,
        );
        let units = end_view_processes.then(|| paths.temp_unit_dirs());
        let reset_store = || {
            let store =
                Store::open(paths, self.runtime.clone()).context("opening store for reset")?;
            store
                .record_workspace(&self.workspace)
                .context("recording workspace metadata for reset")?;
            reset_records(&store).context("resetting workspace records")
        };
        let (view_processes_ended, records) = if let Some(units) = &units {
            end_view_holders_then(
                || crate::mux::recovery::temp_unit_holders(units),
                |pids| {
                    crate::mux::recovery::kill_pids(pids, crate::mux::recovery::SWEEP_GRACE);
                },
                reset_store,
            )?
        } else {
            (Vec::new(), reset_store()?)
        };
        Ok(RoomResetReport {
            teardown,
            view_processes_ended,
            records,
        })
    }
}

/// End the processes `holders` reports inside the room's sandbox views, then
/// run `reset_records`. `kill` only warns about survivors, so a second look
/// decides: a process that is still a holder refuses the reset before
/// `reset_records` runs, leaving the store untouched for a rerun.
fn end_view_holders_then<T>(
    holders: impl Fn() -> Vec<ProcInfo>,
    kill: impl FnOnce(&[u32]),
    reset_records: impl FnOnce() -> Result<T>,
) -> Result<(Vec<u32>, T)> {
    let ended: Vec<u32> = holders().iter().map(|proc| proc.pid).collect();
    if !ended.is_empty() {
        kill(&ended);
        let survivors = holders();
        if !survivors.is_empty() {
            let listed: String = survivors
                .iter()
                .map(|proc| format!("\n  pid {}: {}", proc.pid, proc.cmdline))
                .collect();
            let one = survivors.len() == 1;
            bail!(
                "The session is gone, but {} process{} still run{} inside the room's sandbox view \
                 and keep{} its /tmp in use:{listed}\n\
                 Stop {}, then run `rimz reset --hard` again.",
                survivors.len(),
                if one { "" } else { "es" },
                if one { "s" } else { "" },
                if one { "s" } else { "" },
                if one { "it" } else { "them" },
            );
        }
    }
    Ok((ended, reset_records()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(pid: u32) -> ProcInfo {
        ProcInfo {
            pid,
            ppid: 1,
            real_uid: 1000,
            cmdline: format!("writer-{pid}"),
        }
    }

    #[test]
    fn hard_reset_ends_view_holders_before_the_record_reset() {
        let live = std::cell::RefCell::new(vec![holder(41), holder(42)]);
        let (ended, ()) = end_view_holders_then(
            || live.borrow().clone(),
            |pids| live.borrow_mut().retain(|proc| !pids.contains(&proc.pid)),
            || {
                assert!(live.borrow().is_empty(), "records reset under a holder");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(ended, [41, 42]);

        let killed = std::cell::Cell::new(false);
        let (ended, ()) = end_view_holders_then(Vec::new, |_| killed.set(true), || Ok(())).unwrap();
        assert!(ended.is_empty());
        assert!(!killed.get(), "no holder, nothing to signal");
    }

    #[test]
    fn hard_reset_refuses_before_the_record_reset_when_a_holder_survives() {
        let live = std::cell::RefCell::new(vec![holder(41), holder(42)]);
        let records_reset = std::cell::Cell::new(false);
        let err = end_view_holders_then(
            || live.borrow().clone(),
            |pids| {
                assert_eq!(pids, [41, 42]);
                live.borrow_mut().retain(|proc| proc.pid != 41);
            },
            || {
                records_reset.set(true);
                Ok(())
            },
        )
        .unwrap_err();
        assert!(!records_reset.get(), "refusal must precede the store");
        assert_eq!(
            err.to_string(),
            "The session is gone, but 1 process still runs inside the room's sandbox view and \
             keeps its /tmp in use:\n  pid 42: writer-42\n\
             Stop it, then run `rimz reset --hard` again."
        );
    }

    #[test]
    fn birth_keeps_live_recorded_name_and_migrates_dead_name() {
        for (recorded, live, expected, existed) in [
            (
                "rimz-old-123456",
                vec!["rimz-old-123456"],
                "rimz-old-123456",
                true,
            ),
            ("rimz-old-123456", vec![], "repo-abcd", false),
            ("rimz-old-123456", vec!["repo-abcd"], "repo-abcd", true),
            ("repo-abcd", vec![], "repo-abcd", false),
        ] {
            let sessions = Ok(live.into_iter().map(str::to_owned).collect());
            assert_eq!(
                birth_session_name(recorded, "repo-abcd", sessions),
                (expected.to_owned(), existed)
            );
        }
        let failure = Err(crate::mux::MuxErr::Timeout {
            program: "mux".to_owned(),
            args: "list".to_owned(),
            seconds: 1,
        });
        assert_eq!(
            birth_session_name("rimz-old-123456", "repo-abcd", failure),
            ("rimz-old-123456".to_owned(), true)
        );
    }
}
