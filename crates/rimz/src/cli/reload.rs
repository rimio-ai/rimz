//! `rimz reload` — pick up a freshly-installed build, restore every running
//! sidebar to a healthy state, and refresh held stats dashboards.
//!
//! User-scoped and cwd-independent (it runs from anywhere, even outside a rimz
//! session): the orchestration lives in [`rimz::reload`]. For each workspace with
//! a live mux session it re-execs the live sidebars onto the current binary,
//! reconciles to one live sidebar per working view — closing duplicates and
//! unresponsive panes, reaping orphaned processes — and adds one to any working
//! view left without. Held `rimz stats --refresh` dashboards re-exec in place
//! before room enumeration. Workspaces whose session is gone have their
//! leftovers swept. Every step is best-effort and run-once.

use anyhow::Result;
use clap::Args;

use super::GlobalFlags;
use crate::cli::render;
use rimz::reload::{ReloadOutcome, reload_user_sidebars};

#[derive(Debug, Args)]
pub struct ReloadArgs {}

pub fn run(_args: ReloadArgs, _globals: &GlobalFlags) -> Result<()> {
    report(&reload_user_sidebars())
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
    if outcome.already_current > 0 {
        writeln!(
            out,
            "{} already on the current build.",
            n(outcome.already_current, "sidebar"),
        )?;
    }
    if outcome.restarted > 0 {
        writeln!(
            out,
            "Restarted {} that could not reload in place.",
            n(outcome.restarted, "sidebar"),
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
            "Deferred {} (no attached client); attach and re-run `rimz reload`.",
            n(outcome.deferred, "sidebar repair"),
        )?;
    }
    if outcome.failed > 0 {
        writeln!(
            out,
            "{} could not be repaired; attach and re-run `rimz reload`.",
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
