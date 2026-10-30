use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};

use anyhow::Result;

use crate::cli::{render, spinner::Spinner};
use rimz::RuntimePaths;
use rimz::agents::spending::{
    KeptDay, RebuildErr, RebuildReport, RebuildScope, rebuild_spending_history,
};
use rimz::config::MachineConfig;
use rimz::harness::assist_log::{self, Assist, AssistRecord};

use super::{SPINNER_MIN_AGE, hold::progress_line};

pub(super) fn run(unattended: bool) -> Result<()> {
    let runtime = RuntimePaths::shared();
    let spec = MachineConfig::load_lenient().headline_spec();
    let scope = if unattended {
        RebuildScope::Pending
    } else {
        RebuildScope::All
    };
    let result = {
        let spinner = (!unattended)
            .then(|| Spinner::delayed("Rebuilding saved spend history", SPINNER_MIN_AGE));
        let stderr_tty = std::io::stderr().is_terminal();
        rebuild_spending_history(&runtime, &spec, scope, &mut |progress| {
            let Some(spinner) = &spinner else {
                return;
            };
            let line = progress_line(progress);
            spinner.set(&line);
            if !stderr_tty
                && (progress.finished_files == 0 || progress.finished_files == progress.total_files)
            {
                let _ = writeln!(render::err(), "{line}");
            }
        })
    };
    if unattended {
        match &result {
            Ok(report) => record_rebuild(report),
            Err(RebuildErr::History { report, .. }) => record_rebuild(report),
            Err(_) => {}
        }
        return match result {
            Ok(report) if report.deferred_days.is_empty() => Ok(()),
            _ => Err(render::AlreadyReported.into()),
        };
    }
    let report = result?;
    let mut out = render::out();
    if report.days.is_empty() {
        writeln!(out, "Nothing to rebuild.")?;
        return Ok(());
    }
    let unchanged = report.unchanged;
    let unchanged_suffix = if unchanged == 1 { "" } else { "s" };
    if report.rebuilt.is_empty() {
        writeln!(
            out,
            "No saved day changed; {unchanged} account-day{unchanged_suffix} unchanged."
        )?;
    } else {
        let days = report
            .rebuilt
            .iter()
            .map(|day| day.day)
            .collect::<BTreeSet<_>>()
            .len();
        let before = report
            .rebuilt
            .iter()
            .filter_map(|day| day.before_usd)
            .fold(0.0, |total, usd| total + usd);
        let after = report
            .rebuilt
            .iter()
            .fold(0.0, |total, day| total + day.after_usd);
        let day_suffix = if days == 1 { "" } else { "s" };
        writeln!(
            out,
            "Rebuilt {days} UTC day{day_suffix}: ${before:.2} → ${after:.2}; \
             {unchanged} account-day{unchanged_suffix} unchanged."
        )?;
    }
    for warning in day_warnings(&report.kept, DayWarning::Kept) {
        writeln!(render::err(), "{warning}")?;
    }
    for warning in day_warnings(&report.deferred_days, DayWarning::Deferred) {
        writeln!(render::err(), "{warning}")?;
    }
    if !report.deferred_days.is_empty() {
        return Err(render::AlreadyReported.into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum DayWarning {
    Kept,
    Deferred,
}

fn day_warnings(account_days: &[KeptDay], warning: DayWarning) -> Vec<String> {
    let mut accounts = BTreeMap::new();
    for day in account_days {
        accounts
            .entry(&day.login)
            .or_insert_with(BTreeSet::new)
            .insert(day.day);
    }
    accounts
        .into_iter()
        .map(|(login, days)| {
            let days = days.into_iter().collect::<Vec<_>>();
            let ranges = days
                .chunk_by(|previous, day| {
                    previous.checked_add(jiff::Span::new().days(1)).ok() == Some(*day)
                })
                .map(|run| {
                    if run.len() == 1 {
                        run[0].to_string()
                    } else {
                        format!("{}..{}", run[0], run[run.len() - 1])
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            let count = days.len();
            let (suffix, saved) = if count == 1 {
                ("", "the saved figure was kept")
            } else {
                ("s", "the saved figures were kept")
            };
            match warning {
                DayWarning::Kept => format!(
                    "warning: {login} {ranges} ({count} UTC day{suffix}): \
                     no transcripts were found; {saved}."
                ),
                DayWarning::Deferred => {
                    let verb = if count == 1 { "stays" } else { "stay" };
                    format!(
                        "warning: could not read {login} transcripts; \
                         {ranges} ({count} UTC day{suffix}) {verb} pending."
                    )
                }
            }
        })
        .collect()
}

fn record_rebuild(report: &RebuildReport) {
    if report.rebuilt.is_empty() {
        return;
    }
    assist_log::append(&AssistRecord {
        at: jiff::Timestamp::now(),
        assist: Assist::SpendRebuild {
            days: report.days.clone(),
            files: report.files,
            rebuilt: report.rebuilt.clone(),
            kept: report.kept.clone(),
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_day_warnings_collapse_ranges_per_account() {
        for (days, expected) in [
            (
                vec!["2026-02-02", "2026-01-31", "2026-02-01"],
                "2026-01-31..2026-02-02 (3 UTC days)",
            ),
            (
                vec!["2026-01-06", "2026-01-01", "2026-01-05", "2026-01-02"],
                "2026-01-01..2026-01-02, 2026-01-05..2026-01-06 (4 UTC days)",
            ),
            (vec!["2026-01-01"], "2026-01-01 (1 UTC day)"),
        ] {
            let kept = std::iter::once(("codex@default", "2026-01-01"))
                .chain(days.iter().map(|day| ("claude@default", *day)))
                .map(|(login, day)| {
                    serde_json::from_value(serde_json::json!({"login": login, "day": day})).unwrap()
                })
                .collect::<Vec<_>>();
            let saved = if days.len() == 1 {
                "the saved figure was kept"
            } else {
                "the saved figures were kept"
            };
            assert_eq!(
                day_warnings(&kept, DayWarning::Kept),
                [
                    format!(
                        "warning: claude@default {expected}: no transcripts were found; {saved}."
                    ),
                    "warning: codex@default 2026-01-01 (1 UTC day): no transcripts were found; \
                     the saved figure was kept."
                        .to_owned(),
                ]
            );
        }
    }
}
