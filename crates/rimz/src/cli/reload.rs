//! `rimz reload` — publish the invoking build and let every running
//! sidebar, rimzd process and hook drainer converge onto it without changing panes.
//!
//! User-scoped and cwd-independent (it runs from anywhere, even outside a rimz
//! session): the orchestration lives in [`rimz::reload`]. For each workspace with
//! a live mux session it publishes durable build intent and nudges the live
//! supervisors; their record poll makes delivery self-healing. `--repair` then
//! invokes independent sidebar structural repair and reopens wholly closed
//! rimzd tabs. Standalone `rimz stats --refresh` dashboards and stats under
//! non-following supervisors
//! re-exec in place before room enumeration,
//! alongside machine-wide retirement of outdated language-server brokers.
//! Brokers compare against the PATH build and keep serving until their server
//! stops; their leases survive for lazy revival. Workspaces whose session is
//! gone have their leftovers swept. An online shared web daemon restarts onto
//! the new build.
//! Rimzd processes poll the committed record instead: their children restart
//! only after the build moves, and a Claude host restart drops phone sessions.
//! Every step is best-effort and run-once.

use std::io::Write;

use anyhow::Result;
use clap::Args;

use super::GlobalFlags;
use crate::cli::render;
use rimz::reload::{ReloadOutcome, reload_user_sidebars};

#[derive(Debug, Args)]
pub struct ReloadArgs {
    /// Repair missing, duplicate, or wedged sidebar panes, and reopen closed
    /// rimzd tabs, after the upgrade.
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
    if outcome.sessions == 0
        && outcome.dead_swept == 0
        && outcome.stats_reloaded == 0
        && outcome.brokers_retired == 0
        && outcome.brokers_marked == 0
        && outcome.brokers_kept == 0
        && outcome.brokers_unchecked == 0
    {
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
    if outcome.brokers_retired > 0 {
        writeln!(
            out,
            "Retired {}. The next query in each checkout starts a fresh one.",
            n(outcome.brokers_retired, "outdated language-server broker"),
        )?;
    }
    if outcome.brokers_marked > 0 {
        writeln!(
            out,
            "{} still serving; each retires when its server next stops.",
            n(outcome.brokers_marked, "outdated language-server broker"),
        )?;
    }
    if outcome.brokers_kept > 0 {
        writeln!(
            out,
            "Left {} running: too old to be told to retire. Run `rimz reload` again once idle.",
            n(outcome.brokers_kept, "language-server broker"),
        )?;
    }
    if outcome.brokers_unchecked > 0 {
        writeln!(
            out,
            "Language-server brokers were not checked: no readable installed `rimz` resolves on PATH. No broker stops servers under memory pressure until `rimz` is on PATH.",
        )?;
    }
    if outcome.drainers_restarted > 0 {
        writeln!(
            out,
            "Restarted {} onto the current build.",
            n(outcome.drainers_restarted, "hook drainer"),
        )?;
    }
    if outcome.drainers_stop_failed > 0 {
        writeln!(
            out,
            "{} did not stop in time; they keep their old build until they go idle or the next reload.",
            n(outcome.drainers_stop_failed, "hook drainer"),
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
    fn drainer_counts_are_reported_only_when_nonzero() {
        let quiet = rendered(&ReloadOutcome::default(), false);
        assert!(!quiet.contains("hook drainer"), "{quiet}");
        let outcome = ReloadOutcome {
            sessions: 1,
            drainers_restarted: 2,
            drainers_stop_failed: 1,
            ..ReloadOutcome::default()
        };
        let report = rendered(&outcome, false);
        assert!(report.contains("2 hook drainers"), "{report}");
        assert!(report.contains("onto the current build"), "{report}");
        assert!(report.contains("1 hook drainer"), "{report}");
        assert!(report.contains("did not stop in time"), "{report}");
        assert!(
            report.contains("until they go idle or the next reload"),
            "{report}"
        );
    }

    #[test]
    fn broker_counts_are_reported_only_when_nonzero_and_suppress_no_sidebars() {
        let quiet = rendered(&ReloadOutcome::default(), false);
        assert!(!quiet.contains("broker"), "{quiet}");
        let cases = [
            (
                ReloadOutcome {
                    brokers_retired: 2,
                    ..ReloadOutcome::default()
                },
                "2 outdated language-server brokers",
                "Retired",
                "The next query in each checkout starts a fresh one.",
            ),
            (
                ReloadOutcome {
                    brokers_marked: 2,
                    ..ReloadOutcome::default()
                },
                "2 outdated language-server brokers",
                "still serving",
                "each retires when its server next stops.",
            ),
            (
                ReloadOutcome {
                    brokers_kept: 2,
                    ..ReloadOutcome::default()
                },
                "2 language-server brokers",
                "running: too old to be told to retire.",
                "Run `rimz reload` again once idle.",
            ),
            (
                ReloadOutcome {
                    brokers_unchecked: 2,
                    ..ReloadOutcome::default()
                },
                "Language-server brokers were not checked",
                "no readable installed `rimz` resolves on PATH",
                "No broker stops servers under memory pressure until `rimz` is on PATH.",
            ),
        ];
        for (outcome, count, meaning, next) in cases {
            let report = rendered(&outcome, false);
            for expected in [count, meaning, next] {
                assert!(report.contains(expected), "missing {expected:?}: {report}");
            }
            assert!(!report.contains("No running sidebars"), "{report}");
            assert!(!report.contains("Launch one with"), "{report}");
        }
    }

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
