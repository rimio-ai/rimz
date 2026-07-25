//! Wall-clock budget for one `cargo xtask` run.
//!
//! Every task carries a budget, armed once at startup: [`overrun`] reports when
//! the run has spent it, and [`crate::runner`] terminates the running child and
//! fails with the overrun. A hung compile, a wedged test process, or a starved
//! multiplexer therefore costs a bounded wait instead of the caller's patience.
//!
//! `RIMZ_XTASK_TIMEOUT` sets the budget for one run: `15m`, `900s`, a bare
//! `900` (seconds), or `off` to lift the bound entirely.

use std::env;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// The pre-PR budget: `gate` and every fast task run inside this.
const DEFAULT_BUDGET: Duration = Duration::from_secs(15 * 60);
/// Whole-compile and whole-suite passes: `ci`, the unfiltered nextest suite,
/// instrumented coverage, and release-profile builds all pay a full build plus
/// every test tier, so they carry a wider bound than the pre-PR gate.
const LONG_BUDGET: Duration = Duration::from_secs(45 * 60);

const ENV_OVERRIDE: &str = "RIMZ_XTASK_TIMEOUT";

static RUN: OnceLock<Run> = OnceLock::new();

struct Run {
    task: String,
    limit: Option<Duration>,
    start: Instant,
}

/// A run that has spent its budget, rendered by the terminating caller.
pub(crate) struct Overrun {
    task: String,
    limit: Duration,
    elapsed: Duration,
}

impl std::fmt::Display for Overrun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "xtask `{}` exceeded its {} budget after {}",
            self.task,
            format_duration(self.limit),
            format_duration(self.elapsed),
        )
    }
}

impl Overrun {
    /// The one line that tells the operator how to get past this.
    pub(crate) fn next_step(&self) -> String {
        format!(
            "NEXT: rerun the slow step on its own, or widen the budget for one run with {ENV_OVERRIDE}={}",
            format_duration(self.limit.saturating_mul(3)),
        )
    }
}

/// Arm the budget for `task`. Called once, before the task runs.
pub(crate) fn arm(task: &str) -> Result<()> {
    let limit = resolve_limit(task, env::var(ENV_OVERRIDE).ok().as_deref())?;
    arm_with(task, limit);
    Ok(())
}

/// Arm an explicit budget, starting the clock now.
pub(crate) fn arm_with(task: &str, limit: Option<Duration>) {
    let _ = RUN.set(Run {
        task: task.to_owned(),
        limit,
        start: Instant::now(),
    });
}

/// The overrun for this run, once the wall clock passes the armed budget.
pub(crate) fn overrun() -> Option<Overrun> {
    let run = RUN.get()?;
    let limit = run.limit?;
    let elapsed = run.start.elapsed();
    (elapsed >= limit).then(|| Overrun {
        task: run.task.clone(),
        limit,
        elapsed,
    })
}

/// The budget for `task`, with the environment override applied.
fn resolve_limit(task: &str, raw_override: Option<&str>) -> Result<Option<Duration>> {
    match raw_override {
        Some(raw) => parse_budget(raw),
        None => Ok(task_budget(task)),
    }
}

/// `None` leaves a task unbounded: `sandbox` and `screenshot` hand the terminal
/// to a command the operator drives, so their wall clock is the operator's.
fn task_budget(task: &str) -> Option<Duration> {
    match task {
        "sandbox" | "screenshot" => None,
        "ci" | "checks" | "lint" | "test" | "test-archive" | "coverage" | "perf" | "semver"
        | "dist" | "install" | "install-dev" | "stage-install" | "profile-build" => {
            Some(LONG_BUDGET)
        }
        _ => Some(DEFAULT_BUDGET),
    }
}

fn parse_budget(raw: &str) -> Result<Option<Duration>> {
    let raw = raw.trim();
    if matches!(raw, "off" | "none" | "never" | "0") {
        return Ok(None);
    }
    let (digits, unit_secs) = match raw.as_bytes().last() {
        Some(b'h') => (&raw[..raw.len() - 1], 3600),
        Some(b'm') => (&raw[..raw.len() - 1], 60),
        Some(b's') => (&raw[..raw.len() - 1], 1),
        _ => (raw, 1),
    };
    let Ok(count) = digits.trim().parse::<u64>() else {
        bail!("{ENV_OVERRIDE} expects a duration like `15m`, `900s`, `900`, or `off`; got `{raw}`");
    };
    if count == 0 {
        return Ok(None);
    }
    Ok(Some(Duration::from_secs(count * unit_secs)))
}

/// Minutes and seconds, the scales a gate budget is read in; a sub-second
/// budget (an override, or a test) renders in milliseconds so it stays visible.
pub(crate) fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    if secs == 0 {
        return format!("{}ms", duration.as_millis());
    }
    match (secs / 60, secs % 60) {
        (0, secs) => format!("{secs}s"),
        (mins, 0) => format!("{mins}m"),
        (mins, secs) => format!("{mins}m{secs}s"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_carries_the_pre_pr_budget_and_full_passes_carry_the_wide_one() {
        assert_eq!(task_budget("gate"), Some(DEFAULT_BUDGET));
        assert_eq!(task_budget("docs-links"), Some(DEFAULT_BUDGET));
        assert_eq!(task_budget("ci"), Some(LONG_BUDGET));
        assert_eq!(task_budget("coverage"), Some(LONG_BUDGET));
    }

    #[test]
    fn operator_driven_tasks_stay_unbounded() {
        assert_eq!(task_budget("sandbox"), None);
        assert_eq!(task_budget("screenshot"), None);
    }

    #[test]
    fn budget_override_reads_every_documented_form() {
        assert_eq!(
            parse_budget("45m").unwrap(),
            Some(Duration::from_secs(2700))
        );
        assert_eq!(parse_budget("90s").unwrap(), Some(Duration::from_secs(90)));
        assert_eq!(
            parse_budget(" 900 ").unwrap(),
            Some(Duration::from_secs(900))
        );
        assert_eq!(parse_budget("2h").unwrap(), Some(Duration::from_secs(7200)));
        for lifted in ["off", "none", "never", "0", "0m"] {
            assert_eq!(parse_budget(lifted).unwrap(), None, "{lifted}");
        }
    }

    #[test]
    fn budget_override_rejects_unparseable_durations() {
        let err = parse_budget("soon").unwrap_err().to_string();
        assert!(
            err.contains("RIMZ_XTASK_TIMEOUT expects a duration"),
            "{err}"
        );
    }

    #[test]
    fn budget_override_replaces_the_task_budget() {
        assert_eq!(
            resolve_limit("sandbox", Some("5m")).unwrap(),
            Some(Duration::from_secs(300))
        );
        assert_eq!(resolve_limit("gate", Some("off")).unwrap(), None);
        assert_eq!(resolve_limit("gate", None).unwrap(), Some(DEFAULT_BUDGET));
    }

    #[test]
    fn durations_render_at_minute_and_second_scale() {
        assert_eq!(format_duration(Duration::from_secs(900)), "15m");
        assert_eq!(format_duration(Duration::from_secs(903)), "15m3s");
        assert_eq!(format_duration(Duration::from_secs(42)), "42s");
        assert_eq!(format_duration(Duration::from_millis(200)), "200ms");
    }

    #[test]
    fn overrun_points_at_a_wider_budget() {
        let overrun = Overrun {
            task: "gate".to_owned(),
            limit: DEFAULT_BUDGET,
            elapsed: DEFAULT_BUDGET + Duration::from_secs(3),
        };

        assert_eq!(
            overrun.to_string(),
            "xtask `gate` exceeded its 15m budget after 15m3s"
        );
        assert!(
            overrun.next_step().contains("RIMZ_XTASK_TIMEOUT=45m"),
            "{}",
            overrun.next_step()
        );
    }
}
