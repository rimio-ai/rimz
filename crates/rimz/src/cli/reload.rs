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

use std::io::Write;

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
        Ok(web) => {
            if let Some(writable) = &web.writable {
                render::web_warnings(&writable.warnings);
            }
            if let Some(share) = &web.share {
                render::web_warnings(&share.warnings);
            }
            web.writable.is_some() || web.share.is_some()
        }
        Err(err) => {
            let _ = writeln!(std::io::stderr().lock(), "rimz: warning: {err}");
            false
        }
    };
    report(&mut render::out(), &outcome, args.repair, web_restarted)?;
    if args.repair {
        super::sidebar::repair(globals)?;
    }
    Ok(())
}

fn report(
    out: &mut impl Write,
    outcome: &ReloadOutcome,
    repair: bool,
    web_restarted: bool,
) -> Result<()> {
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
    // Reload only converges sidebars that report in; a room whose sidebar is
    // gone or unreachable needs the repair verb, which `--repair` runs next.
    if outcome.sidebar_missing > 0 && !repair {
        writeln!(
            out,
            "{} with no sidebar reporting in; mount or replace it with `rimz reload --repair`.",
            n(outcome.sidebar_missing, "room"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(outcome: &ReloadOutcome, repair: bool) -> String {
        let mut out = Vec::new();
        report(&mut out, outcome, repair, false).expect("render the reload report");
        String::from_utf8(out).expect("utf-8 report")
    }

    // The count carries the accent, so a styled report splits around it; each
    // assertion stays on one side.
    #[test]
    fn a_room_that_lost_its_sidebar_is_pointed_at_repair() {
        let outcome = ReloadOutcome {
            sessions: 1,
            sidebar_missing: 1,
            ..ReloadOutcome::default()
        };
        let report = rendered(&outcome, false);
        assert!(report.contains("1 room"), "{report}");
        assert!(
            report.contains(
                "with no sidebar reporting in; mount or replace it with `rimz reload --repair`."
            ),
            "{report}"
        );
        // `--repair` runs that verb next, so naming it again would be noise.
        assert!(!rendered(&outcome, true).contains("--repair"));
    }

    #[test]
    fn a_converged_room_says_nothing_about_repair() {
        let outcome = ReloadOutcome {
            sessions: 1,
            reexeced: 1,
            ..ReloadOutcome::default()
        };
        let report = rendered(&outcome, false);
        assert!(report.contains("Reloaded"), "{report}");
        assert!(!report.contains("--repair"), "{report}");
    }
}
