//! Minimal `/proc` reader. The `rimz reset` orphan sweep walks every process
//! ([`list_processes`]); the sidebar resolves a pane's owning shell from its root
//! pid ([`comm`]), dates an in-pane agent instance from its start
//! ([`process_start`]), and matches a process to a pane by working directory
//! ([`cwd`]). Linux-only; other platforms return an empty list / `None`, so
//! callers fall back rather than guessing without `/proc`. It also owns the
//! hot-path subprocess spawn seams the perf guards count.

mod pane_probe;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub(crate) use pane_probe::command_starts_with_elevation_wrapper;
pub use pane_probe::{
    InPaneAgentProcess, elevated_in_pane_agent, hosted_agent_absent_under_root,
    in_pane_agent_process_for_root, in_pane_agent_start, in_pane_agent_start_for_root,
    in_pane_agent_starts,
};

fn git_binary() -> &'static Path {
    static GIT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    GIT.get_or_init(|| which::which("git").unwrap_or_else(|_| "git".into()))
}

/// `git -C <worktree>` with the binary resolved once, so repeated hot-path
/// probes skip PATH lookup. Counts the spawn for the subprocess perf guards.
pub(crate) fn git_command(worktree: &Path) -> Command {
    testkit::count_spawn();
    let mut cmd = Command::new(git_binary());
    cmd.arg("-C").arg(worktree);
    cmd
}

fn bin_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

/// The `rimz` binary to execute for self-spawns.
///
/// Linux reports `/proc/self/exe` with a trailing ` (deleted)` after an atomic
/// reinstall replaces the running inode. Long-lived helpers must execute the
/// replacement at the stripped path instead of failing until the process
/// restarts.
pub fn rimz_exe() -> PathBuf {
    crate::store::paths::env_path("RIMZ_BIN")
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| resolve_existing_or_replacement(&exe))
        })
        .unwrap_or_else(|| PathBuf::from(bin_name("rimz")))
}

/// Resolve an executable path reported by the OS to a real file on disk.
pub fn resolve_existing_or_replacement(path: &Path) -> Option<PathBuf> {
    resolve_existing_or_replacement_with(path, Path::is_file)
}

fn resolve_existing_or_replacement_with(
    path: &Path,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if exists(path) {
        return Some(path.to_path_buf());
    }
    strip_deleted_suffix(path).filter(|candidate| exists(candidate))
}

#[cfg(unix)]
fn strip_deleted_suffix(path: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    const DELETED_SUFFIX: &[u8] = b" (deleted)";
    let stripped = path.as_os_str().as_bytes().strip_suffix(DELETED_SUFFIX)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(stripped)))
}

#[cfg(not(unix))]
fn strip_deleted_suffix(path: &Path) -> Option<PathBuf> {
    path.as_os_str()
        .to_str()
        .and_then(|raw| raw.strip_suffix(" (deleted)"))
        .map(PathBuf::from)
}

#[derive(Debug)]
pub(crate) struct BoundedOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) timed_out: bool,
}

/// Run a subprocess with captured stdout and a wall-clock timeout.
///
/// Timeout kills and reaps the child, then drains any bytes already written.
/// This mirrors the mux bounded-command pattern without importing backend code
/// into domain modules.
pub(crate) fn run_bounded_output(
    command: &mut Command,
    timeout: Duration,
) -> std::io::Result<BoundedOutput> {
    testkit::count_spawn();
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().map(read_to_end_thread);
    let stderr = child.stderr.take().map(read_to_end_thread);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            let _ = join_reader(stderr);
            return Ok(BoundedOutput {
                status,
                stdout: join_reader(stdout),
                timed_out: false,
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let status = child.wait()?;
            drop(join_reader(stderr));
            return Ok(BoundedOutput {
                status,
                stdout: join_reader(stdout),
                timed_out: true,
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn read_to_end_thread(mut reader: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        bytes
    })
}

fn join_reader(handle: Option<std::thread::JoinHandle<Vec<u8>>>) -> Vec<u8> {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}

/// One process as the reset sweep needs to see it: its pid, its parent, the real
/// uid that owns it, and its full command line (argv joined by spaces).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: u32,
    pub ppid: u32,
    pub real_uid: u32,
    pub cmdline: String,
}

/// Every process visible to this user, best-effort. An entry that vanishes
/// mid-scan, or one whose `/proc` files this user cannot read, is skipped.
#[cfg(target_os = "linux")]
pub fn list_processes() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if let Some(info) = read_proc(pid) {
            out.push(info);
        }
    }
    out
}

#[cfg(not(target_os = "linux"))]
pub fn list_processes() -> Vec<ProcInfo> {
    Vec::new()
}

/// The base command name of `pid` from `/proc/<pid>/comm`. A pane's root process
/// is the shell that owns it, so this names the shell anchor a process row shows
/// on its primary line while a command runs underneath. Non-Linux, or an
/// unreadable `/proc`, yields `None` and the caller falls back to the foreground
/// program.
#[cfg(target_os = "linux")]
pub fn comm(pid: u32) -> Option<String> {
    parse_comm(&std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?)
}

#[cfg(not(target_os = "linux"))]
pub fn comm(_pid: u32) -> Option<String> {
    None
}

/// The value of environment variable `key` for `pid`, read from
/// `/proc/<pid>/environ` (NUL-separated `key=value` pairs). A sidebar inherits
/// its pane's `ZELLIJ_PANE_ID` / `TMUX_PANE`, so `rimz reload` reads it back to
/// attribute a running renderer to the pane it paints. Non-Linux, or an
/// unreadable `/proc`, yields `None`, so callers fall back rather than guess.
#[cfg(target_os = "linux")]
pub fn env_var(pid: u32, key: &str) -> Option<String> {
    parse_environ(&std::fs::read(format!("/proc/{pid}/environ")).ok()?, key)
}

#[cfg(not(target_os = "linux"))]
pub fn env_var(_pid: u32, _key: &str) -> Option<String> {
    None
}

/// Find `key`'s value in a NUL-separated `key=value` environ blob.
#[cfg(target_os = "linux")]
fn parse_environ(raw: &[u8], key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    String::from_utf8_lossy(raw)
        .split('\0')
        .find_map(|pair| pair.strip_prefix(prefix.as_str()).map(str::to_owned))
}

/// Trim the trailing newline `/proc/<pid>/comm` always carries; an empty name is
/// no name.
#[cfg(target_os = "linux")]
fn parse_comm(raw: &str) -> Option<String> {
    let name = raw.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(target_os = "linux")]
fn read_proc(pid: u32) -> Option<ProcInfo> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let (ppid, real_uid) = parse_status_identity(&status)?;
    Some(ProcInfo {
        pid,
        ppid,
        real_uid,
        cmdline: cmdline(pid)?,
    })
}

#[cfg(target_os = "linux")]
fn parse_status_identity(status: &str) -> Option<(u32, u32)> {
    let mut ppid = None;
    let mut real_uid = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            ppid = rest.trim().parse::<u32>().ok();
        } else if let Some(rest) = line.strip_prefix("Uid:") {
            // "Uid:\t<real>\t<effective>\t<saved>\t<fs>" — the real uid is first.
            real_uid = rest
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u32>().ok());
        }
        if ppid.is_some() && real_uid.is_some() {
            break;
        }
    }
    Some((ppid?, real_uid?))
}

/// The flattened command line of `pid` — `/proc/<pid>/cmdline`'s NUL-separated
/// argv joined by spaces for substring matching. The sidebar runs it through
/// the agent-CLI classifiers to tell an in-pane agent from the shell hosting
/// it. `None` on a non-Linux target or an unreadable entry — another user's
/// process — so callers fall back rather than guess.
#[cfg(target_os = "linux")]
pub fn cmdline(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        String::from_utf8_lossy(&raw)
            .replace('\0', " ")
            .trim()
            .to_owned(),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn cmdline(_pid: u32) -> Option<String> {
    None
}

/// The real uid of `pid` from `/proc/<pid>/status`. `cmdline`, `comm`, and
/// `stat` are normally readable across uids; `cwd`/`environ` are not. This lets
/// the sidebar distinguish an elevated descendant without crossing into that
/// user's private environment or config.
#[cfg(target_os = "linux")]
pub fn real_uid(pid: u32) -> Option<u32> {
    parse_status_identity(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
        .map(|(_, uid)| uid)
}

#[cfg(not(target_os = "linux"))]
pub fn real_uid(_pid: u32) -> Option<u32> {
    None
}

/// Wall-clock start time of `pid`, anchoring `/proc/<pid>/stat` field 22
/// (`starttime`, in clock ticks since boot) to the boot epoch (`btime` in
/// `/proc/stat`). A start time dates the in-pane instance: the sidebar compares
/// it against a session's last activity so a freshly-launched agent process never
/// inherits the stale session that last ran in the same worktree (the
/// `pane_start_allows_bind` guard). `None` on a non-Linux target or an unreadable
/// `/proc` — another user's process — so callers fall back rather than guess.
#[cfg(target_os = "linux")]
pub fn process_start(pid: u32) -> Option<jiff::Timestamp> {
    let ticks = parse_starttime_ticks(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
    let btime = parse_btime(&std::fs::read_to_string("/proc/stat").ok()?)?;
    let seconds = btime.checked_add((ticks / clk_tck()) as i64)?;
    jiff::Timestamp::from_second(seconds).ok()
}

#[cfg(not(target_os = "linux"))]
pub fn process_start(_pid: u32) -> Option<jiff::Timestamp> {
    None
}

/// The working directory of `pid` from the `/proc/<pid>/cwd` symlink. The sidebar
/// matches this against a pane's reported cwd to find the in-pane agent process
/// that backs the pane. `None` on a non-Linux target or an unreadable link.
#[cfg(target_os = "linux")]
pub fn cwd(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The executable backing `pid` from `/proc/<pid>/exe`. Linux appends
/// ` (deleted)` when the inode has been unlinked; return the stripped path plus
/// that flag so callers can name a stale running binary without treating the
/// suffix as part of the filesystem path. Non-Linux, or an unreadable link,
/// yields `None`.
#[cfg(target_os = "linux")]
pub fn exe_path(pid: u32) -> Option<(std::path::PathBuf, bool)> {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    const DELETED: &[u8] = b" (deleted)";

    let path = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let bytes = path.as_os_str().as_bytes();
    let Some(stripped) = bytes.strip_suffix(DELETED) else {
        return Some((path, false));
    };
    Some((
        std::path::PathBuf::from(OsString::from_vec(stripped.to_vec())),
        true,
    ))
}

/// The real uid this process runs as, read from its own `/proc` status. The
/// sidebar's pane-pid backfill matches a session's Zellij server by uid so a
/// same-named session of another user is never walked. `None` on a non-Linux
/// target, so callers skip rather than guess.
#[cfg(target_os = "linux")]
pub fn own_uid() -> Option<u32> {
    read_proc(std::process::id()).map(|info| info.real_uid)
}

#[cfg(not(target_os = "linux"))]
pub fn own_uid() -> Option<u32> {
    None
}

/// Best-effort account name for a real uid. UI-only; a missing name falls back
/// to a numeric uid at the row projection layer.
#[cfg(unix)]
pub fn user_name(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|user| user.name)
}

#[cfg(not(unix))]
pub fn user_name(_uid: u32) -> Option<String> {
    None
}

#[cfg(not(target_os = "linux"))]
pub fn cwd(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(target_os = "linux"))]
pub fn exe_path(_pid: u32) -> Option<(std::path::PathBuf, bool)> {
    None
}

/// Always-on observability seam for subprocesses on hot paths. The counter is
/// per-process and relaxed, matching the store counters: benchmarks,
/// integration gates, and the sidebar tick meter care about exact call counts at
/// the fork funnels, not cross-process aggregation.
#[doc(hidden)]
pub mod testkit {
    use std::sync::atomic::{AtomicU64, Ordering};

    static SPAWNS: AtomicU64 = AtomicU64::new(0);

    /// Subprocess spawn attempts counted since process start.
    pub fn spawn_count() -> u64 {
        SPAWNS.load(Ordering::Relaxed)
    }

    pub(crate) fn count_spawn() {
        SPAWNS.fetch_add(1, Ordering::Relaxed);
        crate::lane::count_spawn();
    }
}

/// One process's resource metrics from a **single** `/proc/<pid>/stat` read:
/// self CPU ticks, waited-child CPU ticks, resident set size, and the raw start
/// time. One read serves the sidebar's CPU% delta, its `M` memory figure, and
/// the pid-reuse guard — where a separate `status` read used to pay a second
/// file open per pane for `VmRSS` alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatMetrics {
    /// Process state character from field 3 (`R`, `S`, `D`, `Z`, ...).
    pub state: char,
    /// utime + stime (fields 14 + 15), in clock ticks. Two readings diffed
    /// over a known interval give CPU%.
    pub cpu_ticks: u64,
    /// cutime + cstime (fields 16 + 17), in clock ticks. The kernel accounts
    /// CPU used by waited-for children here, so pane-tree sampling can keep
    /// short-lived rustc/linker work visible after the child process exits.
    pub child_cpu_ticks: u64,
    /// Resident set size in KiB: field 24 (`rss`, in pages) × the page size.
    /// `rss` is the kernel's resident-page counter and can run a few pages
    /// apart from `status`'s `VmRSS`; invisible at the display's MiB
    /// granularity, and the field is display-only by contract.
    pub rss_kb: u64,
    /// Raw `starttime` (field 22) in clock ticks since boot — an exact integer
    /// identity for pid-reuse detection, with no `btime` anchoring round-trip.
    pub start_ticks: u64,
}

/// Read [`StatMetrics`] for `pid`. `None` on a non-Linux target or an
/// unreadable/garbled stat line, so callers abstain rather than guess.
#[cfg(target_os = "linux")]
pub fn stat_metrics(pid: u32) -> Option<StatMetrics> {
    parse_stat_metrics(
        &std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?,
        page_size_kb(),
    )
}

#[cfg(not(target_os = "linux"))]
pub fn stat_metrics(_pid: u32) -> Option<StatMetrics> {
    None
}

/// Parse a `/proc/<pid>/stat` line into [`StatMetrics`]. The same
/// `rsplit_once(')')` anchor as [`parse_starttime_ticks`] — `comm` may carry
/// spaces and parens — then, indexed past the closing paren: state 0, utime 11,
/// stime 12, cutime 13, cstime 14, starttime 19, rss 21.
#[cfg(target_os = "linux")]
fn parse_stat_metrics(stat: &str, page_kb: u64) -> Option<StatMetrics> {
    let tail = stat.rsplit_once(')')?.1;
    let mut fields = tail.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let utime: u64 = fields.nth(10)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    let cutime: u64 = fields.next()?.parse().ok()?;
    let cstime: u64 = fields.next()?.parse().ok()?;
    let start_ticks: u64 = fields.nth(4)?.parse().ok()?;
    let rss_pages: u64 = fields.nth(1)?.parse().ok()?;
    Some(StatMetrics {
        state,
        cpu_ticks: utime.saturating_add(stime),
        child_cpu_ticks: cutime.saturating_add(cstime),
        rss_kb: rss_pages.saturating_mul(page_kb),
        start_ticks,
    })
}

/// The system page size in KiB, for scaling stat's `rss` page count. An
/// unavailable or nonsensical `sysconf` answer falls back to 4 KiB — the
/// near-universal Linux default.
#[cfg(target_os = "linux")]
fn page_size_kb() -> u64 {
    nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE)
        .ok()
        .flatten()
        .and_then(|bytes| u64::try_from(bytes).ok())
        .map(|bytes| bytes / 1024)
        .filter(|&kb| kb > 0)
        .unwrap_or(4)
}

/// `rchar` and `wchar` lines from `/proc/<pid>/io`, summed. Captures all VFS
/// I/O (including page-cache reads) so a build reading cached files still shows
/// I/O activity.
#[cfg(target_os = "linux")]
fn parse_io_bytes(io: &str) -> Option<u64> {
    let mut rchar: Option<u64> = None;
    let mut wchar: Option<u64> = None;
    for line in io.lines() {
        if let Some(rest) = line.strip_prefix("rchar:") {
            rchar = rest.trim().parse().ok();
        } else if let Some(rest) = line.strip_prefix("wchar:") {
            wchar = rest.trim().parse().ok();
        }
        if rchar.is_some() && wchar.is_some() {
            break;
        }
    }
    Some(rchar?.saturating_add(wchar?))
}

/// The `wchar` counter from `/proc/<pid>/io`. This counts bytes passed to
/// write-like syscalls, including terminal output and non-pty writes.
#[cfg(target_os = "linux")]
fn parse_write_bytes(io: &str) -> Option<u64> {
    io.lines()
        .find_map(|line| line.strip_prefix("wchar:"))?
        .trim()
        .parse()
        .ok()
}

/// Field 22 (`starttime`) of a `/proc/<pid>/stat` line. The second field (`comm`)
/// is parenthesized and may itself contain spaces and parens, so anchor on the
/// *last* `)` and count whitespace-separated fields after it: `starttime` is the
/// 20th field past `comm` (index 19), since `comm` is field 2 and `starttime` is
/// field 22.
#[cfg(target_os = "linux")]
fn parse_starttime_ticks(stat: &str) -> Option<u64> {
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// System boot time as a Unix epoch (seconds) from the `btime <secs>` line of
/// `/proc/stat`.
#[cfg(target_os = "linux")]
fn parse_btime(stat: &str) -> Option<i64> {
    stat.lines()
        .find_map(|line| line.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()
}

/// Direct children of `pid`, unioned from every
/// `/proc/<pid>/task/<tid>/children` file. Linux records the child under the
/// task that forked it, so a process spawned by a worker thread — cargo job
/// threads and tokio runtimes included — appears under that thread's tid rather
/// than the process leader. This is the O(threads) sibling of walking the whole
/// process table to build a ppid map, for the sidebar's shell→tree stats
/// descent on a walk-free tick. Needs `CONFIG_PROC_CHILDREN` (mainstream kernels
/// enable it); an unreadable task dir yields the empty list for that process,
/// and an unreadable task file skips that task's child edge for the sample.
#[cfg(target_os = "linux")]
pub fn children(pid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };

    let mut children = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .is_none()
        {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(entry.path().join("children")) {
            children.extend(parse_children(&raw));
        }
    }
    children
}

#[cfg(not(target_os = "linux"))]
pub fn children(_pid: u32) -> Vec<u32> {
    Vec::new()
}

/// Parse the space-separated pid list of a `/proc/<pid>/task/<tid>/children`
/// file (a trailing space and no newline, per procfs).
#[cfg(target_os = "linux")]
fn parse_children(raw: &str) -> Vec<u32> {
    raw.split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect()
}

/// Combined VFS I/O bytes (rchar + wchar) for `pid` from `/proc/<pid>/io`.
/// Captures all I/O through the filesystem interface, cached reads included.
/// Two readings diffed over a known interval give bytes/s. `None` on a
/// non-Linux target, an unreadable file (e.g. another user's process), or a
/// missing field.
#[cfg(target_os = "linux")]
pub fn io_bytes(pid: u32) -> Option<u64> {
    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    parse_io_bytes(&io)
}

#[cfg(not(target_os = "linux"))]
pub fn io_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Process write-like bytes (`wchar`) for `pid` from `/proc/<pid>/io`. Two
/// readings diffed over a known interval give process write-rate. `None` on a
/// non-Linux target, an unreadable file, or a missing field.
#[cfg(target_os = "linux")]
pub fn write_bytes(pid: u32) -> Option<u64> {
    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    parse_write_bytes(&io)
}

#[cfg(not(target_os = "linux"))]
pub fn write_bytes(_pid: u32) -> Option<u64> {
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeTotals {
    pub cpu_ticks: u64,
    pub rss_kb: u64,
    pub io_bytes: Option<u64>,
    pub process_count: u32,
}

/// Sum CPU, resident memory, and VFS I/O over a process tree rooted at
/// `root_pid`. `None` means the root cannot be sampled on this platform or it
/// vanished before its first stat read.
pub fn tree_totals(root_pid: u32) -> Option<TreeTotals> {
    tree_totals_with(root_pid, &stat_metrics, &children, &io_bytes)
}

fn tree_totals_with(
    root_pid: u32,
    stat: &dyn Fn(u32) -> Option<StatMetrics>,
    children: &dyn Fn(u32) -> Vec<u32>,
    io: &dyn Fn(u32) -> Option<u64>,
) -> Option<TreeTotals> {
    let mut stack = vec![root_pid];
    let mut seen = std::collections::BTreeSet::new();
    let mut totals = TreeTotals {
        cpu_ticks: 0,
        rss_kb: 0,
        io_bytes: Some(0),
        process_count: 0,
    };
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        let children = children(pid);
        stack.extend(children);
        let metrics = match stat(pid) {
            Some(metrics) => metrics,
            None if pid == root_pid => return None,
            None => continue,
        };
        totals.cpu_ticks = totals
            .cpu_ticks
            .saturating_add(metrics.cpu_ticks)
            .saturating_add(metrics.child_cpu_ticks);
        totals.rss_kb = totals.rss_kb.saturating_add(metrics.rss_kb);
        totals.process_count = totals.process_count.saturating_add(1);
        totals.io_bytes = match (totals.io_bytes, io(pid)) {
            (Some(total), Some(bytes)) => Some(total.saturating_add(bytes)),
            _ => None,
        };
    }
    (totals.process_count > 0).then_some(totals)
}

/// Clock ticks per second for `/proc` `starttime` (`SC_CLK_TCK`). Linux reports
/// `starttime` in USER_HZ; `sysconf` returns it, and an unavailable or nonsensical
/// answer falls back to 100 — the USER_HZ every mainstream Linux target fixes it
/// at, independent of the kernel's `CONFIG_HZ`.
#[cfg(target_os = "linux")]
pub fn clk_tck() -> u64 {
    nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
        .ok()
        .flatten()
        .and_then(|ticks| u64::try_from(ticks).ok())
        .filter(|&ticks| ticks > 0)
        .unwrap_or(100)
}

#[cfg(not(target_os = "linux"))]
pub fn clk_tck() -> u64 {
    100
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn parse_comm_trims_trailing_newline() {
        assert_eq!(parse_comm("zsh\n").as_deref(), Some("zsh"));
        assert_eq!(parse_comm("bash").as_deref(), Some("bash"));
    }

    #[test]
    fn parse_comm_rejects_blank() {
        assert_eq!(parse_comm("\n"), None);
        assert_eq!(parse_comm("   "), None);
    }

    #[test]
    fn parse_environ_reads_a_present_key() {
        let blob = b"PATH=/usr/bin\0ZELLIJ_PANE_ID=3\0TERM=xterm\0";
        assert_eq!(parse_environ(blob, "ZELLIJ_PANE_ID").as_deref(), Some("3"));
        assert_eq!(parse_environ(blob, "PATH").as_deref(), Some("/usr/bin"));
    }

    #[test]
    fn parse_environ_missing_key_is_none() {
        let blob = b"PATH=/usr/bin\0TERM=xterm\0";
        assert_eq!(parse_environ(blob, "ZELLIJ_PANE_ID"), None);
        // A bare key with no `=` never matches the `key=` prefix.
        assert_eq!(parse_environ(b"ZELLIJ_PANE_ID\0", "ZELLIJ_PANE_ID"), None);
    }

    #[test]
    fn rimz_exe_resolution_keeps_existing_path() {
        let path = Path::new("/opt/rimz/bin/rimz");
        let resolved = resolve_existing_or_replacement_with(path, |candidate| candidate == path);

        assert_eq!(resolved.as_deref(), Some(path));
    }

    #[test]
    fn rimz_exe_resolution_strips_deleted_suffix_when_replacement_exists() {
        let real = Path::new("/opt/rimz/bin/rimz");
        let deleted = PathBuf::from(format!("{} (deleted)", real.display()));
        let resolved =
            resolve_existing_or_replacement_with(&deleted, |candidate| candidate == real);

        assert_eq!(resolved.as_deref(), Some(real));
    }

    #[test]
    fn rimz_exe_resolution_returns_none_when_deleted_and_replacement_missing() {
        let deleted = Path::new("/opt/rimz/bin/rimz (deleted)");

        assert!(resolve_existing_or_replacement_with(deleted, |_| false).is_none());
    }

    #[test]
    fn bounded_output_captures_stdout() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf rimz"]);

        let output = run_bounded_output(&mut cmd, Duration::from_secs(1)).expect("bounded output");

        assert!(output.status.success());
        assert!(!output.timed_out);
        assert_eq!(output.stdout, b"rimz");
    }

    #[test]
    fn bounded_output_kills_and_reaps_on_timeout() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5"]);

        let output =
            run_bounded_output(&mut cmd, Duration::from_millis(20)).expect("bounded output");

        assert!(output.timed_out);
        assert!(!output.status.success());
    }

    #[test]
    fn parse_status_identity_reads_ppid_and_real_uid() {
        let status = "\
Name:\tclaude
State:\tS (sleeping)
PPid:\t42
Uid:\t0\t0\t0\t0
";
        assert_eq!(parse_status_identity(status), Some((42, 0)));
    }

    #[test]
    fn parse_status_identity_rejects_missing_fields() {
        assert_eq!(parse_status_identity("PPid:\t42\n"), None);
        assert_eq!(
            parse_status_identity("Uid:\t1000\t1000\t1000\t1000\n"),
            None
        );
    }

    #[test]
    fn parse_starttime_ticks_reads_field_22() {
        // pid (comm) state ppid … starttime(field 22) …
        let stat = "1234 (codex) S 1 1234 1234 0 -1 0 0 0 0 0 1 2 3 4 20 0 5 0 646245020 …";
        assert_eq!(parse_starttime_ticks(stat), Some(646_245_020));
    }

    #[test]
    fn parse_starttime_ticks_handles_comm_with_spaces_and_parens() {
        // `comm` can carry spaces and nested parens; anchoring on the last `)`
        // keeps field 22 correct.
        let stat = "1234 (codex (1) :)) S 1 1234 1234 0 -1 0 0 0 0 0 1 2 3 4 20 0 5 0 777 0";
        assert_eq!(parse_starttime_ticks(stat), Some(777));
    }

    #[test]
    fn parse_starttime_ticks_rejects_malformed() {
        assert_eq!(parse_starttime_ticks("no parens here"), None);
        // Too few fields after `comm` to reach field 22.
        assert_eq!(parse_starttime_ticks("1 (sh) S 1 2 3"), None);
    }

    #[test]
    fn parse_stat_metrics_reads_cpu_rss_and_start_from_one_line() {
        // Indexed past the closing `)`:
        // state ppid pgrp session ttyno tpgid flags minflt cminflt majflt cmajflt utime stime
        //   0    1    2    3       4     5     6     7      8       9      10      11    12
        // cutime cstime priority nice threads itreal starttime vsize rss
        //   13     14      15     16    17      18      19       20   21
        let stat =
            "1234 (codex) S 1 1234 1234 0 -1 0 0 0 0 0 42 17 0 0 20 0 1 0 646245020 9000000 2048 …";
        assert_eq!(
            parse_stat_metrics(stat, 4),
            Some(StatMetrics {
                state: 'S',
                cpu_ticks: 59, // 42 + 17
                child_cpu_ticks: 0,
                rss_kb: 8192, // 2048 pages × 4 KiB
                start_ticks: 646_245_020,
            })
        );
    }

    #[test]
    fn parse_stat_metrics_handles_comm_with_spaces_and_parens() {
        // `comm` can carry spaces and nested parens; anchoring on the last `)`
        // keeps every field index correct.
        let stat = "1 (rust (1) :)) S 1 1 1 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 100 500 3 0";
        assert_eq!(
            parse_stat_metrics(stat, 4),
            Some(StatMetrics {
                state: 'S',
                cpu_ticks: 15, // 10 + 5
                child_cpu_ticks: 0,
                rss_kb: 12, // 3 pages × 4 KiB
                start_ticks: 100,
            })
        );
    }

    #[test]
    fn parse_stat_metrics_reads_waited_child_cpu() {
        let stat = "1234 (cargo) S 1 1234 1234 0 -1 0 0 0 0 0 42 17 300 25 20 0 1 0 646245020 9000000 2048 …";

        assert_eq!(
            parse_stat_metrics(stat, 4).map(|metrics| metrics.child_cpu_ticks),
            Some(325)
        );
    }

    #[test]
    fn parse_stat_metrics_rejects_truncated_lines() {
        // Enough fields for CPU but not for rss: the whole read abstains
        // rather than reporting a partial metric set.
        let stat = "1 (sh) S 1 1 1 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 100";
        assert_eq!(parse_stat_metrics(stat, 4), None);
        assert_eq!(parse_stat_metrics("no parens here", 4), None);
    }

    #[test]
    fn parse_children_reads_the_space_separated_pid_list() {
        assert_eq!(parse_children("123 456 789 "), vec![123, 456, 789]);
        assert_eq!(parse_children(""), Vec::<u32>::new());
        // Garbage tokens are skipped rather than poisoning the list.
        assert_eq!(parse_children("12 x 34"), vec![12, 34]);
    }

    #[test]
    fn parse_io_bytes_sums_rchar_and_wchar() {
        let io = "rchar: 1000\nwchar: 500\nsyscr: 12\nsyscw: 8\nread_bytes: 0\nwrite_bytes: 512\n";
        assert_eq!(parse_io_bytes(io), Some(1500));
    }

    #[test]
    fn parse_write_bytes_reads_wchar_only() {
        let io =
            "rchar: 1000\nwchar: 500\nsyscr: 12\nsyscw: 9000\nread_bytes: 0\nwrite_bytes: 8192\n";
        assert_eq!(parse_write_bytes(io), Some(500));
    }

    #[test]
    fn parse_io_bytes_rejects_incomplete() {
        // Only rchar present: returns None (wchar is missing).
        let io = "rchar: 1000\n";
        assert_eq!(parse_io_bytes(io), None);
    }

    #[test]
    fn tree_totals_walks_descendants_and_dedupes_cycles() {
        let stat = |pid| {
            Some(StatMetrics {
                state: 'S',
                cpu_ticks: u64::from(pid),
                child_cpu_ticks: 10,
                rss_kb: u64::from(pid * 100),
                start_ticks: 1,
            })
        };
        let children = |pid| match pid {
            1 => vec![2, 3],
            2 => vec![3],
            _ => Vec::new(),
        };
        let io = |pid| Some(u64::from(pid * 1_000));

        assert_eq!(
            tree_totals_with(1, &stat, &children, &io),
            Some(TreeTotals {
                cpu_ticks: 36,
                rss_kb: 600,
                io_bytes: Some(6_000),
                process_count: 3,
            })
        );
    }

    #[test]
    fn tree_totals_returns_none_when_root_is_missing() {
        let stat = |_pid| None;
        let children = |_pid| Vec::new();
        let io = |_pid| Some(0);

        assert_eq!(tree_totals_with(1, &stat, &children, &io), None);
    }

    #[test]
    fn tree_totals_missing_child_io_makes_tree_io_unknown() {
        let stat = |pid| {
            Some(StatMetrics {
                state: 'S',
                cpu_ticks: u64::from(pid),
                child_cpu_ticks: 0,
                rss_kb: 1,
                start_ticks: 1,
            })
        };
        let children = |pid| if pid == 1 { vec![2] } else { Vec::new() };
        let io = |pid| (pid == 1).then_some(10);

        assert_eq!(
            tree_totals_with(1, &stat, &children, &io).map(|totals| totals.io_bytes),
            Some(None)
        );
    }

    #[test]
    fn parse_btime_reads_the_btime_line() {
        let stat = "cpu  1 2 3\nintr 99\nbtime 1773993132\nprocesses 42\n";
        assert_eq!(parse_btime(stat), Some(1_773_993_132));
        assert_eq!(parse_btime("cpu 1 2 3\nprocesses 42\n"), None);
    }
}
