//! Minimal process reader. Linux uses `/proc`; macOS uses `sysinfo`. The `rimz
//! reset` orphan sweep walks every process
//! ([`list_processes`]); the sidebar resolves a pane's owning shell from its root
//! pid ([`comm`]), dates an in-pane agent instance from its start
//! (`process_start`), and matches a process to a pane by working directory
//! ([`cwd`]). Unsupported platforms return an empty list / `None`, so callers
//! fall back rather than guessing. It also owns the hot-path subprocess spawn
//! seams the perf guards count and the user's login-shell selection.

mod bounded;
pub mod cgroup;
pub(crate) mod command;
#[cfg(target_os = "macos")]
mod macos;
pub(crate) mod memory;
mod pane_probe;

pub(crate) use bounded::{
    BoundedOutput, KillScope, pump_child, run_bounded_output, run_bounded_output_hosted,
};
pub(crate) use command::{
    command_program_basename, program_label, rimz_exec_worktree_path, sole_agent_child,
};

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[cfg(all(target_os = "linux", any(test, feature = "testkit")))]
use crate::testkit::host_pass::{Counter, count};

/// Native Linux boot identity, cached once per process, including failed reads.
/// Other platforms have no cheap native identity and return `None`.
pub fn boot_id() -> Option<String> {
    static BOOT_ID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    BOOT_ID
        .get_or_init(|| {
            if !cfg!(target_os = "linux") {
                return None;
            }
            let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
            let id = id.trim().to_owned();
            (!id.is_empty()).then_some(id)
        })
        .clone()
}

/// Stable per-device identity carried to the remote attach so a replacement
/// can retire an orphaned predecessor before entering the multiplexer.
pub const REMOTE_LINEAGE_ENV: &str = "RIMZ_REMOTE_LINEAGE";

pub use pane_probe::{
    HostedAgentProcess, InPaneAgentProcess, hosted_agent_process_for_root,
    in_pane_agent_process_for_root,
};
pub(crate) use pane_probe::{
    command_starts_with_elevation_wrapper, elevated_in_pane_agent, hosted_agent_absent_under_root,
    in_pane_agent_start_for_root, in_pane_agent_starts,
};

/// Return at most the final `cap` bytes, decoding an initial partial codepoint lossily.
#[doc(hidden)]
pub fn tail_output(bytes: &[u8], cap: usize) -> String {
    let start = bytes.len().saturating_sub(cap);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(target_os = "macos")]
pub use macos::{
    argv, children, clk_tck, comm, comm_and_ppid, cwd, env_var, list_processes, process_is_live,
    process_start_token, write_bytes,
};
#[cfg(target_os = "macos")]
pub(crate) use macos::{
    cmdline, environ, exe_path, io_bytes, process_start, real_uid, stat_metrics,
};

fn git_binary() -> &'static Path {
    static GIT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    GIT.get_or_init(|| which::which("git").unwrap_or_else(|_| "git".into()))
}

/// `git -C <worktree>` with the binary resolved once, so repeated hot-path
/// probes skip PATH lookup. Counts the spawn for the subprocess perf guards.
pub(crate) fn git_command(worktree: &Path) -> Command {
    testkit::count_spawn();
    git_command_uncounted(worktree)
}

fn git_command_uncounted(worktree: &Path) -> Command {
    let mut cmd = Command::new(git_binary());
    cmd.arg("-C").arg(worktree);
    cmd
}

/// Run git within a deadline, counting the spawn attempt only at the bounded runner.
pub(crate) fn run_bounded_git_output(
    worktree: &Path,
    args: &[&str],
    timeout: Duration,
) -> std::io::Result<BoundedOutput> {
    let mut command = git_command_uncounted(worktree);
    command.args(args);
    run_bounded_output(&mut command, timeout)
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
    crate::disk::paths::env_path("RIMZ_BIN")
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| resolve_existing_or_replacement(&exe))
        })
        .unwrap_or_else(|| PathBuf::from(bin_name("rimz")))
}

/// The user's own shell, stamped on an agent launched under a configured
/// `[agents] shell` so a `rimz` that agent runs still opens the user's shell
/// panes rather than the agent's `$SHELL`.
pub(crate) const USER_SHELL_ENV: &str = "RIMZ_USER_SHELL";

/// Resolve the user's configured shell for launches that should match a
/// normal terminal. A set [`USER_SHELL_ENV`] wins over a set `$SHELL`; either
/// one, when unlaunchable, returns `None`. The passwd shell is consulted only
/// when both are unset or empty. Sentinels such as `false` and `nologin`, and
/// missing absolute paths, are rejected.
pub(crate) fn user_shell() -> Option<PathBuf> {
    user_shell_from(env_shell(USER_SHELL_ENV), env_shell("SHELL"))
}

fn user_shell_from(stamped: Option<PathBuf>, shell: Option<PathBuf>) -> Option<PathBuf> {
    match stamped.or(shell) {
        Some(shell) => launchable_shell(&shell).then_some(shell),
        None => passwd_shell().filter(|shell| launchable_shell(shell)),
    }
}

/// Resolve the user's shell program for an ordinary shell pane.
pub fn user_shell_program() -> String {
    user_shell()
        .unwrap_or_else(|| PathBuf::from("sh"))
        .to_string_lossy()
        .into_owned()
}

/// Short, stable identity for an ordinary shell pane.
pub fn shell_pane_name() -> String {
    let shell = user_shell_program();
    Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "sh".to_owned())
}

fn env_shell(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(unix)]
fn passwd_shell() -> Option<PathBuf> {
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .ok()
        .flatten()?;
    Some(user.shell)
}

#[cfg(not(unix))]
fn passwd_shell() -> Option<PathBuf> {
    None
}

pub(crate) fn launchable_shell(shell: &Path) -> bool {
    !is_login_disabled_shell(shell) && shell_exists(shell)
}

fn is_login_disabled_shell(shell: &Path) -> bool {
    shell
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| matches!(name, "false" | "nologin"))
        .unwrap_or(false)
}

fn shell_exists(shell: &Path) -> bool {
    if shell.is_absolute() {
        return shell.is_file();
    }
    which::which(shell).is_ok()
}

/// Distinct executable files on PATH in search order, optionally excluding one file.
/// Each pair keeps the found path and its canonical path.
pub fn path_copies(program: &std::ffi::OsStr, exclude: Option<&Path>) -> Vec<(PathBuf, PathBuf)> {
    let path = std::env::var_os("PATH");
    let cwd = std::env::current_dir().ok();
    path_copies_with(program, path.as_deref(), cwd.as_deref(), exclude)
}

fn path_copies_with(
    program: &std::ffi::OsStr,
    path: Option<&std::ffi::OsStr>,
    cwd: Option<&Path>,
    exclude: Option<&Path>,
) -> Vec<(PathBuf, PathBuf)> {
    let Some(path) = path else {
        return Vec::new();
    };
    let mut lookup = which::WhichConfig::new()
        .binary_name(program.to_owned())
        .custom_path_list(path.to_owned())
        .system_cwd(false);
    if let Some(cwd) = cwd {
        lookup = lookup.custom_cwd(cwd.to_path_buf());
    }
    let mut seen: HashSet<_> = exclude.and_then(file_identity).into_iter().collect();
    lookup
        .all_results()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|path| path.canonicalize().ok().map(|canonical| (path, canonical)))
        .filter(|(_, canonical)| file_identity(canonical).is_some_and(|key| seen.insert(key)))
        .collect()
}

#[cfg(unix)]
fn file_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn list_processes() -> Vec<ProcInfo> {
    Vec::new()
}

/// Whether a live member of `pgid` carries the exact environment marker.
/// A surviving member reserves the group ID after its leader has been reaped.
pub(crate) fn process_group_has_env(pgid: u32, key: &str, value: &str) -> bool {
    use nix::unistd::{Pid, getpgid};

    let Ok(pgid) = i32::try_from(pgid) else {
        return false;
    };
    if pgid <= 0 {
        return false;
    }
    let group = Pid::from_raw(pgid);
    #[cfg(target_os = "linux")]
    let mut pids = {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return false;
        };
        entries.flatten().filter_map(|entry| {
            entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
        })
    };
    #[cfg(not(target_os = "linux"))]
    let mut pids = list_processes().into_iter().map(|process| process.pid);
    pids.any(|pid| {
        let Ok(raw) = i32::try_from(pid) else {
            return false;
        };
        let member = Pid::from_raw(raw);
        getpgid(Some(member)) == Ok(group)
            && env_var(pid, key).as_deref() == Some(value)
            && process_is_live(pid, None)
            // Membership may change while reading the environment and liveness.
            && getpgid(Some(member)) == Ok(group)
    })
}

/// The base command name of `pid` from `/proc/<pid>/comm`. A pane's root process
/// is the shell that owns it, so this names the shell anchor a process row shows
/// on its primary line while a command runs underneath. Non-Linux, or an
/// unreadable `/proc`, yields `None` and the caller falls back to the foreground
/// program.
#[cfg(target_os = "linux")]
pub fn comm(pid: u32) -> Option<String> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcComm);
    parse_comm(&std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn comm(_pid: u32) -> Option<String> {
    None
}

/// The base command name and parent pid for `pid`, read in one process-status
/// probe. Hook attribution walks parent links through this instead of carrying
/// a private platform reader.
#[cfg(target_os = "linux")]
pub fn comm_and_ppid(pid: u32) -> Option<(String, u32)> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcStatus);
    parse_status_name_ppid(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
}

/// The exact argument vector of `pid`. Hook ownership classifiers use this
/// instead of flattened command lines so argument text cannot impersonate a
/// process role. Unreadable and unsupported process tables abstain.
#[cfg(target_os = "linux")]
pub fn argv(pid: u32) -> Option<Vec<std::ffi::OsString>> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcCmdline);
    parse_argv(&std::fs::read(format!("/proc/{pid}/cmdline")).ok()?)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn argv(_pid: u32) -> Option<Vec<std::ffi::OsString>> {
    None
}

#[cfg(target_os = "linux")]
fn parse_argv(raw: &[u8]) -> Option<Vec<std::ffi::OsString>> {
    use std::os::unix::ffi::OsStringExt as _;

    let args = raw
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| std::ffi::OsString::from_vec(arg.to_vec()))
        .collect::<Vec<_>>();
    (!args.is_empty()).then_some(args)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn comm_and_ppid(_pid: u32) -> Option<(String, u32)> {
    None
}

/// Parent process ids from nearest to farthest, capped to avoid cycles in a
/// malformed process table. Unsupported platforms return an empty list.
#[cfg(unix)]
pub fn ancestor_pids() -> Vec<u32> {
    let mut ancestors = Vec::new();
    let mut pid = std::os::unix::process::parent_id();
    for _ in 0..32 {
        if pid <= 1 {
            break;
        }
        ancestors.push(pid);
        let Some((_, parent)) = comm_and_ppid(pid) else {
            break;
        };
        pid = parent;
    }
    ancestors
}

#[cfg(not(unix))]
pub fn ancestor_pids() -> Vec<u32> {
    Vec::new()
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn env_var(_pid: u32, _key: &str) -> Option<String> {
    None
}

/// The complete environment of `pid` as parsed `key=value` pairs. A readable
/// empty environment is `Some(Vec::new())`; an unreadable process environment
/// is `None`, so signal callers can spare a process whose domain is unknown.
#[cfg(target_os = "linux")]
pub(crate) fn environ(pid: u32) -> Option<Vec<(String, String)>> {
    Some(parse_environ_pairs(
        &std::fs::read(format!("/proc/{pid}/environ")).ok()?,
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn environ(_pid: u32) -> Option<Vec<(String, String)>> {
    None
}

/// Find `key`'s value in a NUL-separated `key=value` environ blob.
#[cfg(target_os = "linux")]
fn parse_environ(raw: &[u8], key: &str) -> Option<String> {
    String::from_utf8_lossy(raw)
        .split('\0')
        .filter_map(parse_environ_pair)
        .find_map(|(candidate, value)| (candidate == key).then(|| value.to_owned()))
}

/// Parse a NUL-separated environment snapshot, splitting each entry at its
/// first `=` so values may contain further equals signs.
#[cfg(target_os = "linux")]
fn parse_environ_pairs(raw: &[u8]) -> Vec<(String, String)> {
    String::from_utf8_lossy(raw)
        .split('\0')
        .filter_map(parse_environ_pair)
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

#[cfg(target_os = "linux")]
fn parse_environ_pair(pair: &str) -> Option<(&str, &str)> {
    pair.split_once('=')
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
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcStatus);
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

#[cfg(target_os = "linux")]
fn parse_status_name_ppid(status: &str) -> Option<(String, u32)> {
    let mut name = None;
    let mut ppid = None;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Name:") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                name = Some(trimmed.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("PPid:") {
            ppid = rest.trim().parse::<u32>().ok();
        }
        if name.is_some() && ppid.is_some() {
            break;
        }
    }
    Some((name?, ppid?))
}

/// The flattened command line of `pid` — `/proc/<pid>/cmdline`'s NUL-separated
/// argv joined by spaces for substring matching. The sidebar runs it through
/// the agent-CLI classifiers to tell an in-pane agent from the shell hosting
/// it. `None` on a non-Linux target or an unreadable entry — another user's
/// process — so callers fall back rather than guess.
#[cfg(target_os = "linux")]
pub(crate) fn cmdline(pid: u32) -> Option<String> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcCmdline);
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        String::from_utf8_lossy(&raw)
            .replace('\0', " ")
            .trim()
            .to_owned(),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn cmdline(_pid: u32) -> Option<String> {
    None
}

/// The real uid of `pid` from `/proc/<pid>/status`. `cmdline`, `comm`, and
/// `stat` are normally readable across uids; `cwd`/`environ` are not. This lets
/// the sidebar distinguish an elevated descendant without crossing into that
/// user's private environment or config.
#[cfg(target_os = "linux")]
pub(crate) fn real_uid(pid: u32) -> Option<u32> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcStatus);
    parse_status_identity(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
        .map(|(_, uid)| uid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn real_uid(_pid: u32) -> Option<u32> {
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
pub(crate) fn process_start(pid: u32) -> Option<jiff::Timestamp> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcStat);
    let ticks = parse_starttime_ticks(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)?;
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcBootStat);
    let btime = parse_btime(&std::fs::read_to_string("/proc/stat").ok()?)?;
    let seconds = btime.checked_add((ticks / clk_tck()) as i64)?;
    jiff::Timestamp::from_second(seconds).ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn process_start(_pid: u32) -> Option<jiff::Timestamp> {
    None
}

/// Opaque per-platform start token for pid-reuse checks. Linux returns the
/// persisted `/proc/<pid>/stat` field 22 string unchanged; macOS tags the
/// process start timestamp because tokens compare only on the same host.
#[cfg(target_os = "linux")]
pub fn process_start_token(pid: u32) -> Option<String> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcStat);
    parse_starttime_ticks(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
        .map(|ticks| ticks.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_start_token(_pid: u32) -> Option<String> {
    None
}

/// Whether `pid` still names the expected live process. The optional start
/// token comes from [`process_start_token`] and rejects pid reuse where the
/// platform exposes process identity.
#[cfg(target_os = "linux")]
pub fn process_is_live(pid: u32, expected_start: Option<&str>) -> bool {
    let Some(metrics) = stat_metrics(pid) else {
        return false;
    };
    stat_metrics_is_live(metrics, expected_start)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub fn process_is_live(pid: u32, _expected_start: Option<&str>) -> bool {
    unix_kill_probe(pid)
}

#[cfg(not(unix))]
pub fn process_is_live(_pid: u32, _expected_start: Option<&str>) -> bool {
    true
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_kill_probe(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    matches!(kill(Pid::from_raw(pid), None), Ok(()) | Err(Errno::EPERM))
}

/// The working directory of `pid` from the `/proc/<pid>/cwd` symlink. The sidebar
/// matches this against a pane's reported cwd to find the in-pane agent process
/// that backs the pane. `None` on a non-Linux target or an unreadable link.
#[cfg(target_os = "linux")]
pub fn cwd(pid: u32) -> Option<std::path::PathBuf> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcCwd);
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The `(dev, ino)` of what `pid` sees at the absolute `path` in its own root,
/// read through `/proc/<pid>/root`. A bind mount keeps its source directory's
/// identity, so this names the directory a mount view put there whatever host
/// path it has since been renamed to. `None` off Linux, or when the root or
/// the path cannot be read.
#[cfg(target_os = "linux")]
pub(crate) fn root_path_identity(pid: u32, path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    let seen = Path::new("/proc")
        .join(pid.to_string())
        .join("root")
        .join(path.strip_prefix("/").ok()?);
    let metadata = std::fs::metadata(seen).ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn root_path_identity(_pid: u32, _path: &Path) -> Option<(u64, u64)> {
    None
}

/// The executable backing `pid` from `/proc/<pid>/exe`. Linux appends
/// ` (deleted)` when the inode has been unlinked; return the stripped path plus
/// that flag so callers can name a stale running binary without treating the
/// suffix as part of the filesystem path. Non-Linux, or an unreadable link,
/// yields `None`.
#[cfg(target_os = "linux")]
pub(crate) fn exe_path(pid: u32) -> Option<(std::path::PathBuf, bool)> {
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

/// The real uid this process runs as. The sidebar's pane-pid backfill matches a
/// session's Zellij server by uid so a same-named session of another user is
/// never walked. `None` on hosts without a unix uid.
#[cfg(unix)]
pub fn own_uid() -> Option<u32> {
    Some(nix::unistd::getuid().as_raw())
}

#[cfg(not(unix))]
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn cwd(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn exe_path(_pid: u32) -> Option<(std::path::PathBuf, bool)> {
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
pub(crate) struct StatMetrics {
    /// Process state character from field 3 (`R`, `S`, `D`, `Z`, ...).
    pub(crate) state: char,
    /// utime + stime (fields 14 + 15), in clock ticks. Two readings diffed
    /// over a known interval give CPU%.
    pub(crate) cpu_ticks: u64,
    /// cutime + cstime (fields 16 + 17), in clock ticks. The kernel accounts
    /// CPU used by waited-for children here, so pane-tree sampling can keep
    /// short-lived rustc/linker work visible after the child process exits.
    pub(crate) child_cpu_ticks: u64,
    /// Resident set size in KiB: field 24 (`rss`, in pages) × the page size.
    /// `rss` is the kernel's resident-page counter and can run a few pages
    /// apart from `status`'s `VmRSS`; invisible at the display's MiB
    /// granularity, and the field is display-only by contract.
    pub(crate) rss_kb: u64,
    /// Raw `starttime` (field 22) in clock ticks since boot — an exact integer
    /// identity for pid-reuse detection, with no `btime` anchoring round-trip.
    pub(crate) start_ticks: u64,
}

/// Read `StatMetrics` for `pid`. `None` on a non-Linux target or an
/// unreadable/garbled stat line, so callers abstain rather than guess.
#[cfg(target_os = "linux")]
pub(crate) fn stat_metrics(pid: u32) -> Option<StatMetrics> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcStat);
    parse_stat_metrics(
        &std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?,
        page_size_kb(),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn stat_metrics(_pid: u32) -> Option<StatMetrics> {
    None
}

/// Open a provider's controlling pty without consuming input or acquiring a controlling terminal.
pub(crate) fn input_tty(provider_pid: u32) -> Option<std::fs::File> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(any(test, feature = "testkit"))]
        count(Counter::ProcStat);
        let stat = std::fs::read_to_string(format!("/proc/{provider_pid}/stat")).ok()?;
        let flags = nix::fcntl::OFlag::O_NOCTTY
            | nix::fcntl::OFlag::O_NONBLOCK
            | nix::fcntl::OFlag::O_CLOEXEC;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags.bits())
            .open(input_tty_path(&stat)?)
            .ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = provider_pid;
        None
    }
}

#[cfg(target_os = "linux")]
fn input_tty_path(stat: &str) -> Option<PathBuf> {
    let tty: i32 = stat
        .rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(4)?
        .parse()
        .ok()?;
    let device = u64::from(tty.cast_unsigned());
    let major = nix::sys::stat::major(device);
    let minor = nix::sys::stat::minor(device);
    (136..=143)
        .contains(&major)
        .then(|| PathBuf::from(format!("/dev/pts/{}", (major - 136) * 256 + minor)))
}

/// Parse a `/proc/<pid>/stat` line into `StatMetrics`. The same
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

#[cfg(target_os = "linux")]
fn stat_metrics_is_live(metrics: StatMetrics, expected_start: Option<&str>) -> bool {
    if matches!(metrics.state, 'Z' | 'X') {
        return false;
    }
    expected_start.is_none_or(|expected| metrics.start_ticks.to_string() == expected)
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
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcTaskDir);
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
        #[cfg(any(test, feature = "testkit"))]
        count(Counter::ProcChildren);
        if let Ok(raw) = std::fs::read_to_string(entry.path().join("children")) {
            children.extend(parse_children(&raw));
        }
    }
    children
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
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
pub(crate) fn io_bytes(pid: u32) -> Option<u64> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcIo);
    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    parse_io_bytes(&io)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn io_bytes(_pid: u32) -> Option<u64> {
    None
}

/// Process write-like bytes (`wchar`) for `pid` from `/proc/<pid>/io`. Two
/// readings diffed over a known interval give process write-rate. `None` on a
/// non-Linux target, an unreadable file, or a missing field.
#[cfg(target_os = "linux")]
pub fn write_bytes(pid: u32) -> Option<u64> {
    #[cfg(any(test, feature = "testkit"))]
    count(Counter::ProcIo);
    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    parse_write_bytes(&io)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TreeMemory {
    pub peak_rss_kb: u64,
    pub swap_kb: u64,
}

#[cfg(target_os = "linux")]
fn parse_tree_memory(status: &str) -> Option<TreeMemory> {
    let field = |name| {
        status.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (key == name)
                .then_some(value.trim())?
                .strip_suffix(" kB")?
                .parse()
                .ok()
        })
    };
    Some(TreeMemory {
        peak_rss_kb: field("VmHWM")?,
        swap_kb: field("VmSwap").unwrap_or(0),
    })
}

/// Sum kernel resident-memory high-water marks and current swap over the live process tree, in KiB.
#[cfg(target_os = "linux")]
pub fn tree_memory_kb(root_pid: u32) -> Option<TreeMemory> {
    tree_memory_kb_with(
        root_pid,
        &|pid| {
            #[cfg(any(test, feature = "testkit"))]
            count(Counter::ProcStatus);
            parse_tree_memory(&std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
        },
        &children,
    )
}

#[cfg(not(target_os = "linux"))]
pub fn tree_memory_kb(_root_pid: u32) -> Option<TreeMemory> {
    None
}

#[cfg(target_os = "linux")]
fn tree_memory_kb_with(
    root_pid: u32,
    memory: &dyn Fn(u32) -> Option<TreeMemory>,
    children: &dyn Fn(u32) -> Vec<u32>,
) -> Option<TreeMemory> {
    let mut stack = vec![root_pid];
    let mut seen = std::collections::BTreeSet::new();
    let mut total = TreeMemory::default();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        stack.extend(children(pid));
        match memory(pid) {
            Some(memory) => {
                total.peak_rss_kb = total.peak_rss_kb.saturating_add(memory.peak_rss_kb);
                total.swap_kb = total.swap_kb.saturating_add(memory.swap_kb);
            }
            None if pid == root_pid => return None,
            None => {}
        }
    }
    Some(total)
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

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn clk_tck() -> u64 {
    100
}

#[cfg(test)]
mod tests;
