//! `rimz reload` — stage a freshly-installed build and move every running
//! sidebar onto it without changing panes; `--repair` also repairs structure.
//!
//! User-scoped and cwd-independent (it runs from anywhere, even outside a rimz
//! session): the orchestration lives in [`rimz::reload`]. For each workspace with
//! a live mux session it re-execs the live sidebars onto the current binary,
//! preserves every terminal pane during the upgrade. The explicit repair pass
//! closes duplicates and replaces wedged renderers add-before-close. Held
//! `rimz stats --refresh` dashboards re-exec in place
//! before room enumeration. Workspaces whose session is gone have their
//! leftovers swept. Every step is best-effort and run-once.

use anyhow::Result;
use clap::Args;

use super::GlobalFlags;
use crate::cli::render;
use rimz::reload::{ReloadOutcome, reload_user_sidebars};

#[derive(Debug, Args)]
pub struct ReloadArgs {
    /// Repair missing, duplicate, or wedged sidebar panes after the upgrade.
    #[arg(long)]
    repair: bool,
}

pub fn run(args: ReloadArgs, _globals: &GlobalFlags) -> Result<()> {
    let outcome = reload_user_sidebars(args.repair)?;
    report(&outcome)
}

fn report(outcome: &ReloadOutcome) -> Result<()> {
    use std::io::Write;
    let mut out = render::out();
    // Each tally reads at a glance: the count carries the accent, the verb stays plain.
    let n = |count: usize, noun: &str| {
        render::paint(render::palette::ACCENT, &self::count(count, noun))
    };
    if outcome.sessions == 0 && outcome.dead_swept == 0 && outcome.stats_reloaded == 0 {
        writeln!(out, "No running sidebars to reload.")?;
        writeln!(out, "Launch one with `rimz start` or `rimz attach`.")?;
        return Ok(());
    }
    if outcome.reexeced > 0 {
        writeln!(
            out,
            "Reloaded {} across {}.",
            n(outcome.reexeced, "sidebar"),
            n(outcome.sessions, "session"),
        )?;
    }
    if outcome.stats_reloaded > 0 {
        writeln!(
            out,
            "Reloaded {}.",
            n(outcome.stats_reloaded, "stats dashboard")
        )?;
    }
    if outcome.presence_dead > 0 {
        writeln!(
            out,
            "No live presence channel for {}; sidebar reconcile skipped. Reattach or restart the session.",
            n(outcome.presence_dead, "session"),
        )?;
    }
    if outcome.plugin_upgraded > 0 {
        writeln!(
            out,
            "Upgraded {}.",
            n(outcome.plugin_upgraded, "presence plugin")
        )?;
    }
    if outcome.plugin_current > 0 {
        writeln!(
            out,
            "{} already current.",
            n(outcome.plugin_current, "presence plugin")
        )?;
    }
    if outcome.already_current > 0 {
        writeln!(
            out,
            "{} already on the current build.",
            n(outcome.already_current, "sidebar"),
        )?;
    }
    if outcome.unconverged > 0 {
        writeln!(
            out,
            "{} still converging; their supervisors will retry from the recorded build automatically.",
            n(outcome.unconverged, "sidebar"),
        )?;
    }
    if outcome.unverified > 0 {
        writeln!(
            out,
            "{} could not be build-verified.",
            n(outcome.unverified, "sidebar"),
        )?;
    }
    if outcome.recovered > 0 {
        writeln!(
            out,
            "Recovered {} in place.",
            n(outcome.recovered, "sidebar")
        )?;
    }
    if outcome.closed > 0 {
        writeln!(
            out,
            "Closed {}.",
            n(outcome.closed, "duplicate or unresponsive sidebar"),
        )?;
    }
    if outcome.redocked > 0 {
        writeln!(out, "Repaired {} geometry.", n(outcome.redocked, "sidebar"))?;
    }
    if outcome.misdocked > 0 {
        writeln!(
            out,
            "{} still working but not docked.",
            n(outcome.misdocked, "sidebar"),
        )?;
    }
    if outcome.reaped > 0 {
        writeln!(
            out,
            "Reaped {}.",
            n(outcome.reaped, "orphaned sidebar process")
        )?;
    }
    if outcome.dead_swept > 0 {
        writeln!(
            out,
            "Swept {} from stopped sessions.",
            n(outcome.dead_swept, "leftover process"),
        )?;
    }
    if outcome.deferred > 0 {
        writeln!(
            out,
            "Deferred {} (no attached client); attach and re-run `rimz reload --repair`.",
            n(outcome.deferred, "sidebar repair"),
        )?;
    }
    if outcome.failed > 0 {
        writeln!(
            out,
            "{} could not be repaired; attach and re-run `rimz reload --repair`.",
            n(outcome.failed, "sidebar"),
        )?;
    }
    Ok(())
}

/// `"1 sidebar"` / `"3 sidebars"`.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else if noun.ends_with("process") {
        format!("{n} {noun}es")
    } else {
        format!("{n} {noun}s")
    }
}
