//! Exact-process and process-owned loopback socket discovery for `agy`.

#[cfg(any(target_os = "linux", target_os = "macos", test))]
use std::collections::BTreeSet;
use std::ffi::OsStr;
#[cfg(target_os = "linux")]
use std::io::Read as _;
#[cfg(any(target_os = "macos", test))]
use std::net::SocketAddr;
#[cfg(any(target_os = "linux", test))]
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(target_os = "macos")]
use std::time::Duration;
use std::time::Instant;

use super::{Candidate, LocalApiError, LoopbackEndpoint};

const MAX_CANDIDATES: usize = 4;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const MAX_ENDPOINTS_PER_PROCESS: usize = 12;
#[cfg(target_os = "linux")]
const MAX_FDS: usize = 4096;
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_PROC_NET_BYTES: u64 = 1024 * 1024;

pub(super) fn discover(deadline: Instant) -> Result<Vec<Candidate>, LocalApiError> {
    let uid = crate::proc::own_uid().ok_or(LocalApiError::Discovery)?;
    let mut processes = crate::proc::list_processes()
        .into_iter()
        .filter(|process| process.real_uid == uid)
        .filter_map(|process| {
            let start = crate::proc::process_start(process.pid)?;
            exact_identity(process.pid, uid).map(|token| (start, process.pid, token))
        })
        .collect::<Vec<_>>();
    processes.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));

    let mut candidates = Vec::new();
    for (_, pid, start_token) in processes.into_iter().take(MAX_CANDIDATES) {
        if Instant::now() >= deadline {
            break;
        }
        let Ok(endpoints) = listening_endpoints(pid, deadline) else {
            continue;
        };
        if !endpoints.is_empty() {
            candidates.push(Candidate {
                pid,
                uid,
                start_token,
                endpoints,
            });
        }
    }
    if candidates.is_empty() {
        Err(LocalApiError::Unavailable)
    } else {
        Ok(candidates)
    }
}

pub(super) fn revalidate(candidate: &Candidate) -> Result<(), LocalApiError> {
    let exe = crate::proc::exe_path(candidate.pid)
        .map(|value| value.0)
        .ok_or(LocalApiError::ProcessChanged)?;
    let argv = crate::proc::argv(candidate.pid).ok_or(LocalApiError::ProcessChanged)?;
    candidate_identity_matches(
        candidate,
        crate::proc::real_uid(candidate.pid),
        &exe,
        argv.first()
            .map(std::ffi::OsString::as_os_str)
            .ok_or(LocalApiError::ProcessChanged)?,
        crate::proc::process_start_token(candidate.pid).as_deref(),
    )
    .then_some(())
    .ok_or(LocalApiError::ProcessChanged)
}

pub(in crate::agents::antigravity) fn candidate_identity_matches(
    candidate: &Candidate,
    process_uid: Option<u32>,
    executable: &Path,
    argv0: &OsStr,
    start_token: Option<&str>,
) -> bool {
    identity_matches(process_uid, candidate.uid, executable, argv0)
        && start_token == Some(&candidate.start_token)
}

fn exact_identity(pid: u32, uid: u32) -> Option<String> {
    let exe = crate::proc::exe_path(pid)?.0;
    let argv = crate::proc::argv(pid)?;
    identity_matches(
        crate::proc::real_uid(pid),
        uid,
        &exe,
        argv.first()?.as_os_str(),
    )
    .then(|| crate::proc::process_start_token(pid))?
}

pub(in crate::agents::antigravity) fn identity_matches(
    process_uid: Option<u32>,
    expected_uid: u32,
    executable: &Path,
    argv0: &OsStr,
) -> bool {
    process_uid == Some(expected_uid)
        && executable.file_name() == Some(OsStr::new("agy"))
        && Path::new(argv0).file_name() == Some(OsStr::new("agy"))
}

#[cfg(target_os = "linux")]
fn listening_endpoints(
    pid: u32,
    _deadline: Instant,
) -> Result<Vec<LoopbackEndpoint>, LocalApiError> {
    Ok(proc_listening_endpoints(Path::new("/proc"), pid))
}

#[cfg(target_os = "macos")]
fn listening_endpoints(
    pid: u32,
    deadline: Instant,
) -> Result<Vec<LoopbackEndpoint>, LocalApiError> {
    let lsof = ["/usr/sbin/lsof", "/usr/bin/lsof"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or(LocalApiError::Discovery)?;
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or(LocalApiError::Unavailable)?;
    let output = crate::proc::run_bounded_output(
        std::process::Command::new(lsof).args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
            "-F",
            "n",
        ]),
        remaining.min(Duration::from_millis(400)),
    )
    .map_err(|_| LocalApiError::Discovery)?;
    if output.timed_out
        || !output.status.success()
        || output.stdout.len() > MAX_PROC_NET_BYTES as usize
    {
        return Ok(Vec::new());
    }
    Ok(parse_lsof(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn listening_endpoints(
    _pid: u32,
    _deadline: Instant,
) -> Result<Vec<LoopbackEndpoint>, LocalApiError> {
    Err(LocalApiError::Unavailable)
}

#[cfg(target_os = "linux")]
fn proc_listening_endpoints(proc_root: &Path, pid: u32) -> Vec<LoopbackEndpoint> {
    let process_root = proc_root.join(pid.to_string());
    let inodes = socket_inodes(&process_root.join("fd"));
    if inodes.is_empty() {
        return Vec::new();
    }
    let mut endpoints = BTreeSet::new();
    for (name, ipv6) in [("tcp", false), ("tcp6", true)] {
        let Some(table) = read_limited(&process_root.join("net").join(name)) else {
            continue;
        };
        endpoints.extend(parse_proc_net(&table, &inodes, ipv6));
    }
    endpoints
        .into_iter()
        .take(MAX_ENDPOINTS_PER_PROCESS)
        .collect()
}

#[cfg(target_os = "linux")]
fn socket_inodes(fd_dir: &Path) -> BTreeSet<String> {
    let Ok(entries) = std::fs::read_dir(fd_dir) else {
        return BTreeSet::new();
    };
    entries
        .flatten()
        .take(MAX_FDS)
        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
        .filter_map(|target| socket_inode(target.as_os_str().to_string_lossy().as_ref()))
        .collect()
}

#[cfg(target_os = "linux")]
fn read_limited(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_PROC_NET_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= MAX_PROC_NET_BYTES as usize)
        .then(|| String::from_utf8(bytes).ok())
        .flatten()
}

#[cfg(any(target_os = "linux", test))]
pub(in crate::agents::antigravity) fn socket_inode(target: &str) -> Option<String> {
    target
        .strip_prefix("socket:[")?
        .strip_suffix(']')
        .filter(|inode| !inode.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(any(target_os = "linux", test))]
pub(in crate::agents::antigravity) fn parse_proc_net(
    table: &str,
    socket_inodes: &BTreeSet<String>,
    ipv6: bool,
) -> Vec<LoopbackEndpoint> {
    let mut endpoints = BTreeSet::new();
    for line in table.lines() {
        let columns = line.split_whitespace().collect::<Vec<_>>();
        if columns.len() <= 9 || columns[3] != "0A" || !socket_inodes.contains(columns[9]) {
            continue;
        }
        let Some((address, port)) = columns[1].rsplit_once(':') else {
            continue;
        };
        let Ok(port) = u16::from_str_radix(port, 16) else {
            continue;
        };
        if port == 0 {
            continue;
        }
        let address = if ipv6 {
            (address.eq_ignore_ascii_case("00000000000000000000000001000000")
                || address.eq_ignore_ascii_case("00000000000000000000000000000001"))
            .then_some(IpAddr::V6(Ipv6Addr::LOCALHOST))
        } else {
            address
                .eq_ignore_ascii_case("0100007F")
                .then_some(IpAddr::V4(Ipv4Addr::LOCALHOST))
        };
        if let Some(address) = address {
            endpoints.insert(LoopbackEndpoint { address, port });
        }
    }
    endpoints.into_iter().collect()
}

#[cfg(any(target_os = "macos", test))]
pub(in crate::agents::antigravity) fn parse_lsof(output: &str) -> Vec<LoopbackEndpoint> {
    let mut endpoints = BTreeSet::new();
    for name in output.lines().filter_map(|line| line.strip_prefix('n')) {
        let name = name.split("->").next().unwrap_or(name);
        let Ok(address) = name.parse::<SocketAddr>() else {
            continue;
        };
        if address.ip().is_loopback() && address.port() != 0 {
            endpoints.insert(LoopbackEndpoint {
                address: address.ip(),
                port: address.port(),
            });
        }
    }
    endpoints
        .into_iter()
        .take(MAX_ENDPOINTS_PER_PROCESS)
        .collect()
}
