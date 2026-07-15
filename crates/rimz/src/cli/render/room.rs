//! Room reset and recovery presentation.

use std::io::Write;

use anyhow::Result;

pub(crate) fn present_mux_pick(
    pick: std::result::Result<rimz::room::session::MuxPick, rimz::room::session::MuxPickErr>,
) -> Result<rimz::ids::MuxName> {
    let (mux, notices) = match pick {
        Ok(pick) => (Ok(pick.mux), pick.notices),
        Err(err) => (Err(err.source), err.notices),
    };
    print_notices(notices)?;
    Ok(mux?)
}

pub(crate) fn print_notices(notices: Vec<String>) -> Result<()> {
    let mut stderr = super::err();
    for notice in notices {
        writeln!(stderr, "note: {notice}")?;
    }
    Ok(())
}

pub(crate) fn present_birth_outcome(
    outcome: Result<rimz::room::BirthOutcome>,
    session_name: &str,
) -> Result<rimz::room::BirthOutcome> {
    match outcome {
        Ok(outcome) => {
            if let Some(reset) = outcome.reset.as_ref() {
                print_automatic_reset(session_name, reset)?;
            }
            Ok(outcome)
        }
        Err(err) => {
            if let Some(reset) = err.downcast_ref::<rimz::room::ResetRecoveryError>() {
                print_automatic_reset(session_name, &reset.report)?;
            }
            Err(err)
        }
    }
}

pub(crate) fn print_automatic_reset(
    session_name: &str,
    report: &rimz::room::RoomResetReport,
) -> Result<()> {
    writeln!(
        std::io::stderr().lock(),
        "rimz: resetting the '{session_name}' room to clear a wedged mux session...",
    )?;
    print_reset_report(report)
}

pub(crate) fn print_reset_report(report: &rimz::room::RoomResetReport) -> Result<()> {
    let teardown = &report.teardown;
    let records = &report.records;
    let mut stderr = std::io::stderr().lock();
    writeln!(
        stderr,
        "Reset: session {}, {} cache entr{} removed, {} orphan process{} swept.",
        if teardown.session_killed {
            "deleted"
        } else {
            "absent"
        },
        teardown.cache_removed.len(),
        if teardown.cache_removed.len() == 1 {
            "y"
        } else {
            "ies"
        },
        teardown.processes_swept.len(),
        if teardown.processes_swept.len() == 1 {
            ""
        } else {
            "es"
        },
    )?;
    match &records.rotation {
        rimz::store::event_log::RotationOutcome::Rotated {
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
        rimz::store::event_log::RotationOutcome::Skipped { current_bytes } => {
            writeln!(
                stderr,
                "Records: no active event log archived ({current_bytes} byte{}).",
                if *current_bytes == 1 { "" } else { "s" },
            )?;
        }
    }
    writeln!(
        stderr,
        "Records: canceled {} run{}, removed {} debug entr{}, runtime {}.",
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
    Ok(())
}
