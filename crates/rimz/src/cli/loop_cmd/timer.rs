//! `rimz loop timer` presentation and hidden external tick entry point.

use super::*;
use crate::cli::loop_timer::{self as os_timer, TimerStatus};

pub(super) fn run(command: TimerSubcmd) -> Result<()> {
    match command {
        TimerSubcmd::Install => install(),
        TimerSubcmd::Status => status(),
        TimerSubcmd::Remove => remove(),
    }
}

pub(super) fn tick() -> Result<()> {
    let now = Timestamp::now().to_zoned(MachineConfig::load_lenient().time_zone());
    os_timer::tick(&now);
    Ok(())
}

pub(super) fn active() -> bool {
    os_timer::status().is_ok_and(|status| status.active())
}

fn install() -> Result<()> {
    let report = os_timer::install()?;
    let mut out = ui::out();
    writeln!(
        out,
        "loop timer: {} ({}; every 1m) → {}",
        ui::paint(ui::palette::good(), "installed"),
        report.backend.label(),
        display_path(&report.path),
    )?;
    Ok(())
}

fn status() -> Result<()> {
    let timer = os_timer::status()?;
    let mut out = ui::out();
    match timer {
        TimerStatus::NotInstalled => {
            writeln!(out, "loop timer: not installed")?;
        }
        TimerStatus::Installed {
            backend,
            exec,
            active,
        } => {
            let state = if active { "active" } else { "inactive" };
            let style = if active {
                ui::palette::good()
            } else {
                ui::palette::warn()
            };
            writeln!(
                out,
                "loop timer: {} ({}) → {}",
                ui::paint(style, state),
                backend.label(),
                display_path(&exec),
            )?;
            let uncovered = os_timer::uncovered_task_roots();
            writeln!(out, "task roots without a room: {uncovered}")?;
        }
    }
    Ok(())
}

fn remove() -> Result<()> {
    let report = os_timer::remove()?;
    let mut out = ui::out();
    if report.changed {
        writeln!(
            out,
            "loop timer: {} ({})",
            ui::paint(ui::palette::good(), "removed"),
            report.backend.label(),
        )?;
    } else {
        writeln!(out, "loop timer: already absent")?;
    }
    Ok(())
}

fn display_path(path: &Path) -> String {
    ui::home_relative(path.to_string_lossy().as_ref())
}
