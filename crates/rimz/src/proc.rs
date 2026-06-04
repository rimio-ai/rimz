//! Minimal `/proc` reader. The `rimz reset` orphan sweep walks every process
//! ([`list_processes`]); the sidebar resolves a pane's owning shell from its root
//! pid ([`comm`]), dates an in-pane agent instance from its start
//! ([`process_start`]), and matches a process to a pane by working directory
//! ([`cwd`]). Linux-only; other platforms return an empty list / `None`, so
//! callers fall back rather than guessing without `/proc`.

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
    // `cmdline` is NUL-separated argv; flatten to spaces for substring matching.
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let cmdline = String::from_utf8_lossy(&raw)
        .replace('\0', " ")
        .trim()
        .to_owned();
    Some(ProcInfo {
        pid,
        ppid: ppid?,
        real_uid: real_uid?,
        cmdline,
    })
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

#[cfg(not(target_os = "linux"))]
pub fn cwd(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

/// Fields 14 (utime) + 15 (stime) from `/proc/<pid>/stat`. The same
/// `rsplit_once(')')` anchor as `parse_starttime_ticks`; utime is index 11 and
/// stime is index 12 after the closing paren.
#[cfg(target_os = "linux")]
fn parse_cpu_ticks(stat: &str) -> Option<u64> {
    let tail = stat.rsplit_once(')')?.1;
    let mut fields = tail.split_whitespace();
    let utime: u64 = fields.nth(11)?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(utime.saturating_add(stime))
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

/// Resident set size of `pid` in kibibytes from `VmRSS` in
/// `/proc/<pid>/status`. A single-sample metric — no prior value needed. `None`
/// on a non-Linux target, an unreadable file, or a missing field.
#[cfg(target_os = "linux")]
pub fn rss_kb(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            // "VmRSS:\t  12345 kB"
            return rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok());
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn rss_kb(_pid: u32) -> Option<u64> {
    None
}

/// utime + stime CPU ticks for `pid` from fields 14 and 15 of
/// `/proc/<pid>/stat`. Two readings diffed over a known interval give CPU%.
/// `None` on a non-Linux target or an unreadable file.
#[cfg(target_os = "linux")]
pub fn cpu_ticks(pid: u32) -> Option<u64> {
    parse_cpu_ticks(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

#[cfg(not(target_os = "linux"))]
pub fn cpu_ticks(_pid: u32) -> Option<u64> {
    None
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
    fn parse_cpu_ticks_sums_utime_and_stime() {
        // Fields 14 and 15 after `comm`; indices 11 and 12 past the closing `)`.
        // state ppid pgrp session ttyno tpgid flags minflt cminflt majflt cmajflt utime stime …
        //   0    1    2    3       4     5     6     7      8       9      10      11    12
        let stat = "1234 (codex) S 1 1234 1234 0 -1 0 0 0 0 0 42 17 0 0 20 0 1 0 646245020 …";
        assert_eq!(parse_cpu_ticks(stat), Some(59)); // 42 + 17
    }

    #[test]
    fn parse_cpu_ticks_handles_comm_with_spaces() {
        let stat = "1 (rust (1)) S 1 1 1 0 -1 0 0 0 0 0 10 5 0 0 20 0 1 0 100 0";
        assert_eq!(parse_cpu_ticks(stat), Some(15)); // 10 + 5
    }

    #[test]
    fn parse_io_bytes_sums_rchar_and_wchar() {
        let io = "rchar: 1000\nwchar: 500\nsyscr: 12\nsyscw: 8\nread_bytes: 0\nwrite_bytes: 512\n";
        assert_eq!(parse_io_bytes(io), Some(1500));
    }

    #[test]
    fn parse_io_bytes_rejects_incomplete() {
        // Only rchar present: returns None (wchar is missing).
        let io = "rchar: 1000\n";
        assert_eq!(parse_io_bytes(io), None);
    }

    #[test]
    fn parse_btime_reads_the_btime_line() {
        let stat = "cpu  1 2 3\nintr 99\nbtime 1773993132\nprocesses 42\n";
        assert_eq!(parse_btime(stat), Some(1_773_993_132));
        assert_eq!(parse_btime("cpu 1 2 3\nprocesses 42\n"), None);
    }
}
