use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::Ordering::Relaxed;

use nix::sys::resource::{UsageWho, getrusage};
use nix::sys::time::TimeValLike;
use rimz::testkit::{self, WORK_COUNTERS};
use serde_json::json;

static LOG: OnceLock<WorkLog> = OnceLock::new();

struct WorkLog {
    path: PathBuf,
    started_ms: i64,
    command: OnceLock<String>,
}

struct OnReturn(Option<&'static WorkLog>);

pub(super) fn start() -> impl Drop {
    OnReturn(std::env::var_os("RIMZ_TEST_WORK_LOG").map(|path| {
        LOG.get_or_init(|| WorkLog {
            path: path.into(),
            started_ms: jiff::Timestamp::now().as_millisecond(),
            command: OnceLock::new(),
        })
    }))
}

pub(super) fn record_command(mut matches: &clap::ArgMatches) {
    let Some(log) = LOG.get() else {
        return;
    };
    let mut command = Vec::new();
    while let Some((name, subcommand)) = matches.subcommand() {
        command.push(name);
        matches = subcommand;
    }
    let command = if command.is_empty() {
        "start".to_owned()
    } else {
        command.join(" ")
    };
    let _ = log.command.set(command);
}

/// Records the command of a process whose arguments took the narrow hot parse,
/// which yields no `ArgMatches` to walk.
pub(super) fn record_hot_command(cli: &super::Cli) {
    let Some(log) = LOG.get() else {
        return;
    };
    let command = match &cli.subcommand {
        Some(super::Subcmd::Hooks(args)) => args.scope().0,
        Some(super::Subcmd::Statusline(_)) => "statusline",
        _ => return,
    };
    let _ = log.command.set(command.to_owned());
}

impl Drop for OnReturn {
    fn drop(&mut self) {
        if let Some(log) = self.0
            && let Err(error) = log.append()
        {
            let _ = writeln!(
                io::stderr().lock(),
                "rimz: cannot append test work log: {error}"
            );
        }
    }
}

impl WorkLog {
    fn append(&self) -> anyhow::Result<()> {
        let usage = getrusage(UsageWho::RUSAGE_SELF)?;
        let max_rss_kb = usage.max_rss();
        #[cfg(target_vendor = "apple")]
        let max_rss_kb = max_rss_kb / 1024;
        let record = json!({
            "pid": std::process::id(),
            "ppid": nix::unistd::getppid().as_raw(),
            "command": self.command.get().map(String::as_str).unwrap_or(""),
            "started_ms": self.started_ms,
            "ended_ms": jiff::Timestamp::now().as_millisecond(),
            "user_us": usage.user_time().num_microseconds(),
            "sys_us": usage.system_time().num_microseconds(),
            "max_rss_kb": max_rss_kb,
            "minor_faults": usage.minor_page_faults(),
            "spawns": testkit::spawn_count(),
            "fsyncs": testkit::fsync_count(),
            "event_log_bytes_read": testkit::bytes_read(),
            "carryover_bytes_parsed": testkit::carryover_bytes_parsed(),
            "rollup_rows_materialized": testkit::rollup_rows_materialized(),
            "fold_row_copies": testkit::fold_row_copies(),
            "snapshot_assemblies": WORK_COUNTERS.snapshot_assemblies.load(Relaxed),
            "snapshot_publishes": WORK_COUNTERS.snapshot_publishes.load(Relaxed),
            "rollup_bytes_parsed": WORK_COUNTERS.rollup_bytes_parsed.load(Relaxed),
            "latest_bytes_parsed": WORK_COUNTERS.latest_bytes_parsed.load(Relaxed),
            "machine_config_loads": WORK_COUNTERS.machine_config_loads.load(Relaxed),
            "marker_reads": WORK_COUNTERS.marker_reads.load(Relaxed),
        });
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        anyhow::ensure!(file.write(&line)? == line.len(), "short work-log append");
        Ok(())
    }
}
