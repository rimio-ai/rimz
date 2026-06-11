use super::*;

fn ensure_clean_room(
    backend: &dyn MuxBackend,
    target: &RoomTarget<'_>,
    daemon: Option<&DaemonView>,
    resume_panes: &[rimz::mux::ResumePane],
) -> Result<SessionHealth> {
    let opts = match build_sidebar_opts(target, resume_panes.to_vec()) {
        Ok(opts) => opts,
        Err(err) => {
            tracing::warn!(error = %err, "session health gate skipped; attaching as-is");
            return Ok(SessionHealth::Healthy);
        }
    };
    match backend.ensure_clean_session(&opts, daemon) {
        Ok(health) => Ok(health),
        Err(
            err @ (rimz::mux::MuxErr::SocketPathTooLong { .. }
            | rimz::mux::MuxErr::SocketPathReportedTooLong { .. }),
        ) => Err(err.into()),
        Err(err) => {
            tracing::warn!(error = %err, "session health gate failed; attaching as-is");
            Ok(SessionHealth::Healthy)
        }
    }
}

/// Run the pre-attach health gate and, if the room cannot self-heal, handle the
/// stuck case (offer a reset, or fail fast). The single entry the attach flows
/// call before building the attach command.
pub(super) fn gate_room_before_attach(
    backend: &dyn MuxBackend,
    target: &RoomTarget<'_>,
    daemon: Option<&DaemonView>,
    resume_panes: &[rimz::mux::ResumePane],
) -> Result<()> {
    if let SessionHealth::Stuck = ensure_clean_room(backend, target, daemon, resume_panes)? {
        recover_stuck_room(backend, target, daemon, resume_panes)?;
    }
    Ok(())
}

/// Handle a room the pre-attach gate could not make clean. Interactively, offer
/// the destructive reset and, on consent, run it and re-gate once. Without a
/// terminal to confirm, fail fast with the fix — never destroy a room unattended.
fn recover_stuck_room(
    backend: &dyn MuxBackend,
    target: &RoomTarget<'_>,
    daemon: Option<&DaemonView>,
    resume_panes: &[rimz::mux::ResumePane],
) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        return Err(ResetRequired {
            session: target.session_name.to_owned(),
        }
        .into());
    }
    if !confirm_reset(target.session_name)? {
        anyhow::bail!("room left untouched; run `rimz reset` when ready");
    }
    let runtime = RuntimePaths::for_workspace(target.workspace_id.clone())?;
    let report = rimz::mux::recovery::teardown_room(
        backend,
        target.workspace_id,
        target.session_name,
        &runtime,
    );
    let records = reset_room_records(target.workspace_id, target.session_name, false)?;
    print_reset_report(&report, Some(&records))?;
    match ensure_clean_room(backend, target, daemon, resume_panes)? {
        SessionHealth::Stuck => {
            anyhow::bail!("the room is still stuck after a reset; inspect with `rimz doctor`")
        }
        SessionHealth::Healthy | SessionHealth::Reborn => Ok(()),
    }
}

/// Single y/N confirmation, modelled on the hook consent prompt: written to
/// stderr (stdout stays clean for scripting), default No. The caller is expected
/// to have already checked `stdin().is_terminal()`.
fn confirm_reset(session: &str) -> Result<bool> {
    confirm(&format!(
        "Rimz must reset the '{session}' room to clear a stuck or uninspectable \
         mux session. Reset now?"
    ))
}

/// Rebuild and attach the room from scratch — the rebirth half of `rimz reset`,
/// run after teardown so the session comes up clean and running.
pub(crate) fn rebirth_room(path: PathBuf, globals: &GlobalFlags) -> Result<()> {
    start(
        StartArgs {
            attach: AttachFlags::default(),
            path,
            // A reset fixes the room; the agents are ledger truth, so the rebirth
            // still resumes them. `rimz reset --hard` clears the ledger resume
            // seed before this rebirth; reset itself does not force one.
            no_resume: false,
            refresh_ms: None,
        },
        globals,
    )
}

/// Report what a teardown removed, to stderr (diagnostic, not stdout output).
pub(crate) fn print_reset_report(
    report: &rimz::mux::recovery::TeardownReport,
    records: Option<&rimz::ledger::ResetRecordsOutcome>,
) -> Result<()> {
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "Reset: session {}, {} cache entr{} removed, {} orphan process{} swept.",
        if report.session_killed {
            "deleted"
        } else {
            "absent"
        },
        report.cache_removed.len(),
        if report.cache_removed.len() == 1 {
            "y"
        } else {
            "ies"
        },
        report.processes_swept.len(),
        if report.processes_swept.len() == 1 {
            ""
        } else {
            "es"
        },
    )?;
    if let Some(records) = records {
        match &records.rotation {
            rimz::ledger::event_log::RotationOutcome::Rotated {
                archive_path,
                bytes_rotated,
            } => {
                writeln!(
                    stderr,
                    "Records: archived {bytes_rotated} byte{} to {}.",
                    if *bytes_rotated == 1 { "" } else { "s" },
                    archive_path.display(),
                )?;
            }
            rimz::ledger::event_log::RotationOutcome::Skipped { current_bytes } => {
                writeln!(
                    stderr,
                    "Records: no active event log archived ({current_bytes} byte{}).",
                    if *current_bytes == 1 { "" } else { "s" },
                )?;
            }
        }
        writeln!(
            stderr,
            "Records: abandoned {} pending item{}, cleared {} feed item{}, canceled {} run{}, removed {} debug entr{}, runtime {}.",
            records.abandoned_pending,
            if records.abandoned_pending == 1 {
                ""
            } else {
                "s"
            },
            records.feed_items_cleared,
            if records.feed_items_cleared == 1 {
                ""
            } else {
                "s"
            },
            records.runs_canceled,
            if records.runs_canceled == 1 { "" } else { "s" },
            records.state_entries_removed,
            if records.state_entries_removed == 1 {
                "y"
            } else {
                "ies"
            },
            if records.runtime_removed {
                "removed"
            } else {
                "already clean"
            },
        )?;
        if records.hard {
            writeln!(stderr, "Records: prior agent rollup cleared.")?;
        } else {
            writeln!(
                stderr,
                "Records: prior agent rollup kept ({} agent{}).",
                records.carryover_agents,
                if records.carryover_agents == 1 {
                    ""
                } else {
                    "s"
                },
            )?;
        }
    }
    Ok(())
}

pub(crate) fn reset_room_records(
    workspace_id: &rimz::WorkspaceId,
    session_name: &str,
    hard: bool,
) -> Result<rimz::ledger::ResetRecordsOutcome> {
    let paths = StatePaths::for_workspace(workspace_id.clone())
        .context("preparing ledger paths for reset")?;
    let runtime = RuntimePaths::for_workspace(workspace_id.clone())
        .context("preparing runtime paths for reset")?;
    let ledger = Ledger::open(paths, runtime).context("opening ledger for reset")?;
    ledger
        .reset_records(session_name, hard)
        .context("resetting workspace records")
}

/// No terminal is available to confirm a destructive reset of a stuck room.
/// `Display` carries the fix, mirroring [`rimz::remote_control::PreflightError`].
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
