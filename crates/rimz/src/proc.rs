//! Minimal `/proc` reader for the `rimz reset` orphan sweep. Linux-only; other
//! platforms return an empty process list, so the sweep no-ops there rather than
//! guessing without `/proc`.

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
