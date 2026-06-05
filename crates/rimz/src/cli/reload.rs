//! `rimz reload` — pick up a freshly-installed build and restore every running
//! sidebar to a healthy state, across all of this user's workspaces.
//!
//! User-scoped and cwd-independent (it runs from anywhere, even outside a rimz
//! session): the orchestration lives in [`rimz::reload`]. For each workspace with
//! a live mux session it re-execs the live sidebars onto the current binary,
//! reconciles to one live sidebar per working view — closing duplicates and
//! unresponsive panes, reaping orphaned processes — and adds one to any working
//! view left without. Workspaces whose session is gone have their leftovers
//! swept. Every step is best-effort and run-once.

use anyhow::Result;
use clap::Args;

use super::GlobalFlags;
use rimz::reload::{ReloadOutcome, reload_user_sidebars};

#[derive(Debug, Args)]
pub struct ReloadArgs {}

pub fn run(_args: ReloadArgs, _globals: &GlobalFlags) -> Result<()> {
    report(&reload_user_sidebars());
    Ok(())
}

#[expect(clippy::print_stdout, reason = "user-facing maintenance report")]
fn report(outcome: &ReloadOutcome) {
    if outcome.sessions == 0 && outcome.dead_swept == 0 {
        println!("No running sidebars to reload.");
        println!("Launch one with `rimz start` or `rimz attach`.");
        return;
    }
    if outcome.signaled > 0 {
        println!(
            "Reloaded {} across {}.",
            count(outcome.signaled, "sidebar"),
            count(outcome.sessions, "session"),
        );
    }
    if outcome.recovered > 0 {
        println!(
            "Recovered {} in place.",
            count(outcome.recovered, "sidebar")
        );
    }
    if outcome.closed > 0 {
        println!(
            "Closed {}.",
            count(outcome.closed, "duplicate or unresponsive sidebar"),
        );
    }
    if outcome.redocked > 0 {
        println!(
            "Re-docked {} to the left column.",
            count(outcome.redocked, "sidebar")
        );
    }
    if outcome.reaped > 0 {
        println!(
            "Reaped {}.",
            count(outcome.reaped, "orphaned sidebar process")
        );
    }
    if outcome.dead_swept > 0 {
        println!(
            "Swept {} from stopped sessions.",
            count(outcome.dead_swept, "leftover process"),
        );
    }
    if outcome.deferred > 0 {
        println!(
            "Deferred {} (no attached client); attach and re-run `rimz reload`.",
            count(outcome.deferred, "sidebar add"),
        );
    }
    if outcome.failed > 0 {
        println!(
            "{} could not be re-added; run `rimz attach` to rebirth the session.",
            count(outcome.failed, "sidebar"),
        );
    }
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
