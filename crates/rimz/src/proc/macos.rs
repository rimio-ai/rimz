use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

use super::{ProcInfo, StatMetrics};

pub fn list_processes() -> Vec<ProcInfo> {
    let system = refreshed_system(ProcessesToUpdate::All, process_refresh_list());
    system
        .processes()
        .values()
        .filter_map(|process| {
            Some(ProcInfo {
                pid: process.pid().as_u32(),
                ppid: process.parent()?.as_u32(),
                real_uid: uid_to_u32(process.user_id()?)?,
                cmdline: joined_os_strings(process.cmd())?,
            })
        })
        .collect()
}

pub fn comm(pid: u32) -> Option<String> {
    process_with(pid, process_refresh_identity(), |process| {
        os_to_string(process.name())
    })
}

pub fn comm_and_ppid(pid: u32) -> Option<(String, u32)> {
    process_with(pid, process_refresh_identity(), |process| {
        Some((os_to_string(process.name())?, process.parent()?.as_u32()))
    })
}

pub fn argv(pid: u32) -> Option<Vec<OsString>> {
    process_with(
        pid,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        |process| (!process.cmd().is_empty()).then(|| process.cmd().to_vec()),
    )
}

pub fn env_var(pid: u32, key: &str) -> Option<String> {
    process_with(
        pid,
        ProcessRefreshKind::nothing().with_environ(UpdateKind::Always),
        |process| env_value(process.environ(), key),
    )
}

pub fn cmdline(pid: u32) -> Option<String> {
    process_with(
        pid,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        |process| joined_os_strings(process.cmd()),
    )
}

pub fn real_uid(pid: u32) -> Option<u32> {
    process_with(
        pid,
        ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
        |process| uid_to_u32(process.user_id()?),
    )
}

pub fn process_start(pid: u32) -> Option<jiff::Timestamp> {
    process_with(pid, process_refresh_identity(), |process| {
        let seconds = i64::try_from(process.start_time()).ok()?;
        jiff::Timestamp::from_second(seconds).ok()
    })
}

pub fn process_start_token(pid: u32) -> Option<String> {
    process_with(pid, process_refresh_identity(), |process| {
        Some(format!("start:{}", process.start_time()))
    })
}

pub fn cwd(pid: u32) -> Option<PathBuf> {
    process_with(
        pid,
        ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always),
        |process| process.cwd().map(PathBuf::from),
    )
}

pub fn exe_path(pid: u32) -> Option<(PathBuf, bool)> {
    process_with(
        pid,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
        |process| process.exe().map(|path| (path.to_path_buf(), false)),
    )
}

pub fn stat_metrics(pid: u32) -> Option<StatMetrics> {
    process_with(pid, process_refresh_stat(), |process| {
        Some(StatMetrics {
            state: status_state(process.status()),
            cpu_ticks: process.accumulated_cpu_time() / 10,
            // macOS/sysinfo exposes no waited-child CPU counter; short-lived
            // children between samples are invisible there.
            child_cpu_ticks: 0,
            rss_kb: process.memory() / 1024,
            start_ticks: process.start_time(),
        })
    })
}

pub fn children(pid: u32) -> Vec<u32> {
    let parent = Pid::from_u32(pid);
    let system = refreshed_system(ProcessesToUpdate::All, process_refresh_identity());
    system
        .processes()
        .values()
        .filter(|process| process.parent() == Some(parent))
        .map(|process| process.pid().as_u32())
        .collect()
}

/// Combined disk I/O bytes for `pid`. macOS reports disk read/write counters;
/// tty, pipe, and cached-VFS traffic are outside this narrower source.
pub fn io_bytes(pid: u32) -> Option<u64> {
    process_with(
        pid,
        ProcessRefreshKind::nothing().with_disk_usage(),
        |process| {
            let usage = process.disk_usage();
            Some(
                usage
                    .total_read_bytes
                    .saturating_add(usage.total_written_bytes),
            )
        },
    )
}

pub fn write_bytes(_pid: u32) -> Option<u64> {
    None
}

pub fn clk_tck() -> u64 {
    100
}

fn process_with<T>(
    pid: u32,
    refresh: ProcessRefreshKind,
    f: impl FnOnce(&sysinfo::Process) -> Option<T>,
) -> Option<T> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, refresh);
    f(system.process(pid)?)
}

fn refreshed_system(processes: ProcessesToUpdate<'_>, refresh: ProcessRefreshKind) -> System {
    let mut system = System::new();
    system.refresh_processes_specifics(processes, true, refresh);
    system
}

fn process_refresh_identity() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing().without_tasks()
}

fn process_refresh_list() -> ProcessRefreshKind {
    process_refresh_identity()
        .with_user(UpdateKind::Always)
        .with_cmd(UpdateKind::Always)
}

fn process_refresh_stat() -> ProcessRefreshKind {
    process_refresh_identity().with_cpu().with_memory()
}

fn joined_os_strings(values: &[OsString]) -> Option<String> {
    let joined = values
        .iter()
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    (!joined.trim().is_empty()).then_some(joined)
}

fn env_value(environ: &[OsString], key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    environ
        .iter()
        .filter_map(|entry| entry.to_str())
        .find_map(|entry| entry.strip_prefix(prefix.as_str()).map(str::to_owned))
}

fn os_to_string(value: &OsStr) -> Option<String> {
    let value = value.to_string_lossy().trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn uid_to_u32(uid: &sysinfo::Uid) -> Option<u32> {
    u32::try_from(**uid).ok()
}

fn status_state(status: ProcessStatus) -> char {
    match status {
        ProcessStatus::Run => 'R',
        ProcessStatus::Sleep => 'S',
        ProcessStatus::Idle => 'I',
        ProcessStatus::Stop => 'T',
        ProcessStatus::Zombie => 'Z',
        _ => 'S',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_os_strings_returns_command_line() {
        let args = [
            OsString::from("cargo"),
            OsString::from("xtask"),
            OsString::from("gate"),
        ];
        assert_eq!(
            joined_os_strings(&args).as_deref(),
            Some("cargo xtask gate")
        );
        assert_eq!(joined_os_strings(&[]), None);
    }

    #[test]
    fn env_value_reads_prefixed_key() {
        let env = [
            OsString::from("PATH=/usr/bin"),
            OsString::from("ZELLIJ_PANE_ID=3"),
            OsString::from("TERM=xterm"),
        ];
        assert_eq!(env_value(&env, "ZELLIJ_PANE_ID").as_deref(), Some("3"));
        assert_eq!(env_value(&env, "RIMZ"), None);
        assert_eq!(env_value(&[OsString::from("RIMZ")], "RIMZ"), None);
    }

    #[test]
    fn status_state_maps_sysinfo_process_states() {
        assert_eq!(status_state(ProcessStatus::Run), 'R');
        assert_eq!(status_state(ProcessStatus::Sleep), 'S');
        assert_eq!(status_state(ProcessStatus::Idle), 'I');
        assert_eq!(status_state(ProcessStatus::Stop), 'T');
        assert_eq!(status_state(ProcessStatus::Zombie), 'Z');
        assert_eq!(status_state(ProcessStatus::Unknown(99)), 'S');
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn self_cmdline_cwd_stat_and_env_are_visible() {
        let pid = std::process::id();
        let argv = cmdline(pid).expect("self cmdline");
        assert!(!argv.is_empty());
        assert_eq!(
            cwd(pid).as_deref(),
            Some(std::env::current_dir().unwrap().as_path())
        );
        let metrics = stat_metrics(pid).expect("self stat metrics");
        assert!(metrics.rss_kb > 0);
        assert!(!matches!(metrics.state, 'Z'));
        assert!(env_var(pid, "PATH").is_some());
        assert!(process_start(pid).is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn children_finds_a_live_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("1")
            .spawn()
            .expect("spawn child");
        let parent = std::process::id();
        let child_pid = child.id();
        assert!(children(parent).contains(&child_pid));
        let _ = child.kill();
        let _ = child.wait();
    }
}
