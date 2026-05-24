//! Resolver heartbeat freshness walk and TOCTOU re-stat.
//!
//! A resolver writes `heartbeat/resolver.<id>.json` under the workspace
//! runtime dir on a tick. The hook bridge engages when at least one entry on
//! the per-machine [`Allowlist`] has a heartbeat younger than
//! [`RESOLVER_HEARTBEAT_TTL`].
//!
//! Single-resolver health is checked by [`check_health`]; [`is_resolver_fresh`]
//! and [`restat`] are thin adapters over it. The four-step check is
//! allowlist lookup → heartbeat read → TTL test → optional pinned-binary
//! verify; the verdict enum keeps every failure mode named.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use tracing::debug;

use crate::bridge::BridgeErr;
use crate::ids::{RequestId, ResolverId};
use crate::ledger::RuntimePaths;
use crate::resolver::Allowlist;
use crate::resolver::allowlist::AllowlistEntry;
use crate::schema::RESOLVER_PROTOCOL_VERSION;
use crate::schema::heartbeat::ResolverHeartbeat;

/// Maximum age of a resolver heartbeat. The doc-suggested cadence is 1s tick,
/// 3s TTL (`docs/internals/resolvers.md`).
pub const RESOLVER_HEARTBEAT_TTL: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum FreshnessErr {
    #[error("reading resolver heartbeat dir {path}: {source}")]
    ReadDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

pub type Result<T> = std::result::Result<T, FreshnessErr>;

/// Return every allowlisted resolver whose heartbeat is younger than
/// [`RESOLVER_HEARTBEAT_TTL`], sorted by chain `order`. Missing heartbeat
/// directory returns an empty vec — the bridge declines to engage and the
/// hook falls through to native UI.
pub fn fresh_enrolled(rt: &RuntimePaths, allowlist: &Allowlist) -> Result<Vec<AllowlistEntry>> {
    if allowlist.entries().is_empty() {
        return Ok(Vec::new());
    }
    let entries = match fs::read_dir(&rt.heartbeat_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(FreshnessErr::ReadDir {
                path: rt.heartbeat_dir.clone(),
                source,
            });
        }
    };
    let now = Timestamp::now();
    let mut alive: Vec<AllowlistEntry> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(id) = resolver_id_from_path(&path) else {
            continue;
        };
        let Some(entry) = allowlist.get(&id) else {
            // Non-allowlisted heartbeats are ignored by the bridge. `rimz
            // doctor` reports them as unauthorized diagnostics.
            continue;
        };
        match read_resolver_heartbeat(&path) {
            Ok(hb) if hb.protocol_version != RESOLVER_PROTOCOL_VERSION => {
                debug!(
                    ?path,
                    protocol = hb.protocol_version,
                    expected = RESOLVER_PROTOCOL_VERSION,
                    "freshness: skipping resolver heartbeat with unsupported protocol version"
                );
            }
            Ok(hb) if is_fresh(&hb, now) => alive.push(entry.clone()),
            Ok(_) => {
                debug!(?path, "freshness: skipping stale resolver heartbeat");
            }
            Err(e) => {
                debug!(?path, error = %e, "freshness: skipping unreadable resolver heartbeat");
            }
        }
    }
    alive.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });
    Ok(alive)
}

/// Verdict from [`check_health`]. Named so the failure mode survives the
/// boundary between freshness logic and its callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealthVerdict {
    Fresh,
    NotAllowlisted,
    HeartbeatUnreadable,
    ProtocolMismatch,
    HeartbeatStale,
    PinMismatch,
}

/// Single source of truth for "is this resolver currently serving?". Both
/// [`is_resolver_fresh`] and [`restat`] route through this so the four-step
/// check (allowlist → heartbeat → TTL → pin) cannot drift between them.
fn check_health(rt: &RuntimePaths, allowlist: &Allowlist, id: &ResolverId) -> HealthVerdict {
    let Some(entry) = allowlist.get(id) else {
        return HealthVerdict::NotAllowlisted;
    };
    let path = rt.heartbeat_dir.join(format!("resolver.{id}.json"));
    let hb = match read_resolver_heartbeat(&path) {
        Ok(hb) => hb,
        Err(err) => {
            debug!(?path, %err, "resolver heartbeat unreadable");
            return HealthVerdict::HeartbeatUnreadable;
        }
    };
    if hb.protocol_version != RESOLVER_PROTOCOL_VERSION {
        debug!(
            resolver = id.as_str(),
            protocol = hb.protocol_version,
            expected = RESOLVER_PROTOCOL_VERSION,
            "resolver heartbeat protocol mismatch"
        );
        return HealthVerdict::ProtocolMismatch;
    }
    if !is_fresh(&hb, Timestamp::now()) {
        return HealthVerdict::HeartbeatStale;
    }
    if let Some(pinned) = entry.binary.as_deref()
        && !verify_binary_pin(pinned, hb.pid)
    {
        debug!(resolver = id.as_str(), pid = ?hb.pid, "binary pin mismatch");
        return HealthVerdict::PinMismatch;
    }
    HealthVerdict::Fresh
}

/// Cheap freshness predicate for a single resolver id. Returns `true` when
/// the resolver is on the allowlist, has a heartbeat younger than the TTL,
/// and (if pinned) the heartbeating process's binary still matches the pin.
/// Returns `false` on any uncertainty — the hook bridge poll loop uses this
/// to detect mid-flight staleness without needing a `RequestId` in scope.
pub fn is_resolver_fresh(rt: &RuntimePaths, allowlist: &Allowlist, id: &ResolverId) -> bool {
    matches!(check_health(rt, allowlist, id), HealthVerdict::Fresh)
}

/// Re-confirm `expected_id` is still serving immediately after binding the
/// per-request socket. Between [`fresh_enrolled`] and [`crate::bridge::bind`],
/// the resolver process can exit or drop off the allowlist; call this once
/// the socket is bound to close that TOCTOU window. On any non-fresh verdict
/// the hook drops the socket and downgrades to `native_ui`.
///
/// Pinned-binary verification uses `/proc/<pid>/exe` on Linux; on other
/// platforms the pin check fails closed.
pub fn restat(
    rt: &RuntimePaths,
    allowlist: &Allowlist,
    expected_id: &ResolverId,
    request_id: &RequestId,
) -> std::result::Result<(), BridgeErr> {
    match check_health(rt, allowlist, expected_id) {
        HealthVerdict::Fresh => Ok(()),
        verdict => {
            debug!(
                resolver = expected_id.as_str(),
                ?verdict,
                "restat: downgrading"
            );
            Err(BridgeErr::HeartbeatStale(request_id.clone()))
        }
    }
}

/// Compare a pinned binary path against the heartbeat's published `pid`.
/// Returns `true` on match, `false` otherwise. Best-effort per the docs:
/// missing `pid`, unreadable `/proc/<pid>/exe`, non-Linux platforms, and any
/// canonicalisation failure on either side all fail closed — the pin is a
/// defence-in-depth check and any uncertainty downgrades to `native_ui`.
fn verify_binary_pin(pinned: &Path, pid: Option<u32>) -> bool {
    let Some(pid) = pid else {
        debug!(?pinned, "binary pin requested but heartbeat carries no pid");
        return false;
    };
    let live = match read_proc_exe(pid) {
        Some(path) => path,
        None => {
            debug!(
                pid,
                "binary pin: /proc/<pid>/exe unreadable on this platform"
            );
            return false;
        }
    };
    let pinned_canon = match pinned.canonicalize() {
        Ok(p) => p,
        Err(err) => {
            debug!(?pinned, %err, "binary pin: pinned path failed to canonicalise");
            return false;
        }
    };
    let live_canon = match live.canonicalize() {
        Ok(p) => p,
        Err(err) => {
            debug!(?live, %err, "binary pin: live exe path failed to canonicalise");
            return false;
        }
    };
    pinned_canon == live_canon
}

#[cfg(target_os = "linux")]
fn read_proc_exe(pid: u32) -> Option<PathBuf> {
    fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(not(target_os = "linux"))]
fn read_proc_exe(_pid: u32) -> Option<PathBuf> {
    None
}

fn resolver_id_from_path(path: &Path) -> Option<ResolverId> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_prefix("resolver.")?.strip_suffix(".json")?;
    // Filenames with characters outside the resolver id alphabet are foreign
    // artefacts; ignore rather than error.
    ResolverId::parse(stem).ok()
}

fn read_resolver_heartbeat(path: &Path) -> std::result::Result<ResolverHeartbeat, io::Error> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn is_fresh(hb: &ResolverHeartbeat, now: Timestamp) -> bool {
    let age = now.duration_since(hb.last_seen);
    !age.is_negative() && Duration::from_secs(age.as_secs() as u64) <= RESOLVER_HEARTBEAT_TTL
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::WorkspaceId;
    use crate::resolver::allowlist::AllowlistEntry;
    use std::path::Path;
    use tempfile::tempdir;

    fn allowlist_with(ids: &[(&str, u32)]) -> Allowlist {
        let mut a = Allowlist::default();
        for (id, order) in ids {
            a.add(AllowlistEntry {
                id: id.parse().unwrap(),
                order: *order,
                budget_seconds: 30,
                binary: None,
                display_name: None,
            })
            .unwrap();
        }
        a
    }

    fn write_heartbeat(rt: &RuntimePaths, id: &str, last_seen: Timestamp) {
        rt.ensure_dirs().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/freshness-test"));
        let resolver_id: ResolverId = id.parse().unwrap();
        let mut hb = ResolverHeartbeat::new(workspace_id, resolver_id);
        hb.last_seen = last_seen;
        let path = rt.heartbeat_dir.join(format!("resolver.{id}.json"));
        std::fs::write(&path, serde_json::to_vec(&hb).unwrap()).unwrap();
    }

    fn write_heartbeat_with_protocol(
        rt: &RuntimePaths,
        id: &str,
        last_seen: Timestamp,
        protocol_version: &str,
    ) {
        rt.ensure_dirs().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/freshness-test"));
        let resolver_id: ResolverId = id.parse().unwrap();
        let mut hb = ResolverHeartbeat::new(workspace_id, resolver_id);
        hb.protocol_version = protocol_version.to_owned();
        hb.last_seen = last_seen;
        let path = rt.heartbeat_dir.join(format!("resolver.{id}.json"));
        std::fs::write(&path, serde_json::to_vec(&hb).unwrap()).unwrap();
    }

    fn rt() -> (tempfile::TempDir, RuntimePaths) {
        let dir = tempdir().unwrap();
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let rt = RuntimePaths::under(workspace_id, dir.path()).unwrap();
        rt.ensure_dirs().unwrap();
        (dir, rt)
    }

    #[test]
    fn empty_allowlist_short_circuits_without_io() {
        let (_d, rt) = rt();
        let list = Allowlist::default();
        assert!(fresh_enrolled(&rt, &list).unwrap().is_empty());
    }

    #[test]
    fn allowlisted_with_fresh_heartbeat_returns_entry() {
        let (_d, rt) = rt();
        let list = allowlist_with(&[("opus-policy", 10)]);
        write_heartbeat(&rt, "opus-policy", Timestamp::now());
        let alive = fresh_enrolled(&rt, &list).unwrap();
        assert_eq!(alive.len(), 1);
        assert_eq!(alive[0].id.as_str(), "opus-policy");
    }

    #[test]
    fn allowlisted_with_stale_heartbeat_is_dropped() {
        let (_d, rt) = rt();
        let list = allowlist_with(&[("opus-policy", 10)]);
        write_heartbeat(
            &rt,
            "opus-policy",
            Timestamp::now() - Duration::from_secs(60),
        );
        assert!(fresh_enrolled(&rt, &list).unwrap().is_empty());
    }

    #[test]
    fn allowlisted_with_wrong_protocol_is_dropped() {
        let (_d, rt) = rt();
        let list = allowlist_with(&[("opus-policy", 10)]);
        write_heartbeat_with_protocol(&rt, "opus-policy", Timestamp::now(), "rimz.resolver.v0");
        assert!(fresh_enrolled(&rt, &list).unwrap().is_empty());

        let request_id = RequestId::new();
        let id: ResolverId = "opus-policy".parse().unwrap();
        let err = restat(&rt, &list, &id, &request_id).unwrap_err();
        assert!(matches!(err, BridgeErr::HeartbeatStale(_)));
    }

    #[test]
    fn non_allowlisted_fresh_heartbeat_is_dropped() {
        let (_d, rt) = rt();
        let list = allowlist_with(&[("opus-policy", 10)]);
        write_heartbeat(&rt, "rogue-policy", Timestamp::now());
        assert!(fresh_enrolled(&rt, &list).unwrap().is_empty());
    }

    #[test]
    fn fresh_enrolled_is_sorted_by_order() {
        let (_d, rt) = rt();
        let list = allowlist_with(&[("opus", 10), ("slack", 20), ("pager", 30)]);
        let now = Timestamp::now();
        write_heartbeat(&rt, "slack", now);
        write_heartbeat(&rt, "pager", now);
        write_heartbeat(&rt, "opus", now);
        let alive = fresh_enrolled(&rt, &list).unwrap();
        let ids: Vec<&str> = alive.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["opus", "slack", "pager"]);
    }

    #[test]
    fn restat_with_matching_binary_pin_passes() {
        let (_d, rt) = rt();
        let mut list = Allowlist::default();
        // Pin to the current process's executable so /proc/self/exe matches.
        let live_exe = std::fs::read_link("/proc/self/exe").unwrap();
        list.add(AllowlistEntry {
            id: "pinned".parse().unwrap(),
            order: 10,
            budget_seconds: 30,
            binary: Some(live_exe),
            display_name: None,
        })
        .unwrap();
        write_heartbeat_with_pid(&rt, "pinned", Timestamp::now(), Some(std::process::id()));
        let request_id = RequestId::new();
        let id: ResolverId = "pinned".parse().unwrap();
        restat(&rt, &list, &id, &request_id).expect("matching pin passes");
    }

    #[test]
    fn restat_with_mismatched_binary_pin_downgrades() {
        let (_d, rt) = rt();
        let mut list = Allowlist::default();
        list.add(AllowlistEntry {
            id: "pinned".parse().unwrap(),
            order: 10,
            budget_seconds: 30,
            binary: Some(std::path::PathBuf::from("/usr/bin/false")),
            display_name: None,
        })
        .unwrap();
        write_heartbeat_with_pid(&rt, "pinned", Timestamp::now(), Some(std::process::id()));
        let request_id = RequestId::new();
        let id: ResolverId = "pinned".parse().unwrap();
        let err = restat(&rt, &list, &id, &request_id).unwrap_err();
        assert!(matches!(err, BridgeErr::HeartbeatStale(_)));
    }

    #[test]
    fn restat_with_pin_but_no_pid_downgrades() {
        let (_d, rt) = rt();
        let mut list = Allowlist::default();
        list.add(AllowlistEntry {
            id: "pinned".parse().unwrap(),
            order: 10,
            budget_seconds: 30,
            binary: Some(std::path::PathBuf::from("/usr/bin/true")),
            display_name: None,
        })
        .unwrap();
        write_heartbeat_with_pid(&rt, "pinned", Timestamp::now(), None);
        let request_id = RequestId::new();
        let id: ResolverId = "pinned".parse().unwrap();
        let err = restat(&rt, &list, &id, &request_id).unwrap_err();
        assert!(matches!(err, BridgeErr::HeartbeatStale(_)));
    }

    fn write_heartbeat_with_pid(
        rt: &RuntimePaths,
        id: &str,
        last_seen: Timestamp,
        pid: Option<u32>,
    ) {
        rt.ensure_dirs().unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/freshness-test"));
        let resolver_id: ResolverId = id.parse().unwrap();
        let mut hb = ResolverHeartbeat::new(workspace_id, resolver_id);
        hb.last_seen = last_seen;
        hb.pid = pid;
        let path = rt.heartbeat_dir.join(format!("resolver.{id}.json"));
        std::fs::write(&path, serde_json::to_vec(&hb).unwrap()).unwrap();
    }

    #[test]
    fn restat_errors_when_heartbeat_unlinked() {
        let (_d, rt) = rt();
        let list = allowlist_with(&[("opus", 10)]);
        write_heartbeat(&rt, "opus", Timestamp::now());
        let request_id = RequestId::new();
        let id: ResolverId = "opus".parse().unwrap();
        restat(&rt, &list, &id, &request_id).unwrap();
        std::fs::remove_file(rt.heartbeat_dir.join("resolver.opus.json")).unwrap();
        let err = restat(&rt, &list, &id, &request_id).unwrap_err();
        assert!(matches!(err, BridgeErr::HeartbeatStale(_)));
    }
}
