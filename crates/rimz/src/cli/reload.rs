//! `rimz reload` — publish a freshly-installed build and let every running
//! sidebar converge onto it without changing panes.
//!
//! User-scoped and cwd-independent (it runs from anywhere, even outside a rimz
//! session): the orchestration lives in [`rimz::reload`]. For each workspace with
//! a live mux session it publishes durable build intent and nudges the live
//! supervisors; their record poll makes delivery self-healing. `--repair` then
//! invokes the independent `rimz sidebar repair` orchestration. Held
//! `rimz stats --refresh` dashboards re-exec in place
//! before room enumeration. Workspaces whose session is gone have their
//! leftovers swept. An online shared web daemon restarts onto the new build.
//! Every step is best-effort and run-once.

use std::io::Write as _;

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

pub fn run(args: ReloadArgs, globals: &GlobalFlags) -> Result<()> {
    let outcome = reload_user_sidebars()?;
    let web_restarted = match rimz::web::restart_if_online(&super::machine_config()) {
        Ok(Some(web)) => {
            render::web_warnings(&web.warnings);
            true
        }
        Ok(None) => false,
        Err(err) => {
            let _ = writeln!(std::io::stderr().lock(), "rimz: warning: {err}");
            false
        }
    };
    report(&outcome, web_restarted)?;
    if args.repair {
        super::sidebar::repair(globals)?;
    }
    Ok(())
}

fn report(outcome: &ReloadOutcome, web_restarted: bool) -> Result<()> {
    let mut out = render::out();
    // Each tally reads at a glance: the count carries the accent, the verb stays plain.
    let n = |count: usize, noun: &str| {
        render::paint(render::palette::accent(), &self::count(count, noun))
    };
    if outcome.sessions == 0 && outcome.dead_swept == 0 && outcome.stats_reloaded == 0 {
        writeln!(out, "No running sidebars to reload.")?;
        writeln!(out, "Launch one with `rimz start` or `rimz attach`.")?;
    } else if outcome.reexeced > 0 {
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
    if outcome.plugin_upgraded > 0 {
        writeln!(
            out,
            "Upgraded {}.",
            n(outcome.plugin_upgraded, "presence plugin")
        )?;
    }
    if outcome.plugin_reconciled > 0 {
        writeln!(
            out,
            "Reconciled {}.",
            n(outcome.plugin_reconciled, "presence plugin")
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
    if web_restarted {
        writeln!(out, "Restarted the shared web daemon.")?;
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
