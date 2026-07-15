//! Room reset and recovery presentation.

use std::io::Write;

use anyhow::Result;

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
