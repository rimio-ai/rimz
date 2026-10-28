//! Sidebar renderer heartbeat wire format, publication, and freshness scans.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::disk::atomic;
use crate::disk::paths::RuntimePaths;
use crate::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};

/// Maximum age of a sidebar heartbeat before launch and wakeup
/// fanout treat the instance as dead and skip it.
pub const SIDEBAR_HEARTBEAT_TTL: Duration = Duration::from_secs(5);

// v6: unchanged claims renew by mtime; the record has no content-age field.
// Heartbeat readers accept only this protocol version.
pub const SIDEBAR_PROTOCOL_VERSION: &str = "rimz.plugin.v6";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarHeartbeat {
    pub protocol_version: String,
    pub workspace_id: WorkspaceId,
    pub instance_id: SidebarInstanceId,
    pub mux: MuxName,
    /// Multiplexer session this sidebar is pinned to. Reload and reconcile
    /// consumers use it to match the renderer to its live mux session; wakeup
    /// fanout is workspace-wide and does not inspect it.
    pub session_name: String,
    pub wakeup_socket: PathBuf,
    /// Normalized pane this renderer paints into (`<mux>:<raw>`). `rimz reload`
    /// links a live sidebar pane to its instance through this so it can tell a
    /// healthy sidebar from a duplicate or an unclaimed (wedged) pane. `None`
    /// when the renderer has no per-pane mux env var — treated as a wildcard
    /// that reconcile never closes a pane for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<PaneId>,
    /// Short digest of the renderer binary that wrote this heartbeat. Missing
    /// means an older renderer or an unreadable running image; reload treats it
    /// as live but not build-verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Semantic RimZ version of the writer, for human-facing build-drift
    /// notices. Missing means the renderer predates this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The renderer's own pane size in cells at this beat, so a frame drawn
    /// outside the pane can match it. Missing means an older renderer or an
    /// unreadable terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<SidebarSize>,
}

/// A sidebar pane's size in terminal cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarSize {
    pub cols: u16,
    pub rows: u16,
}

impl SidebarHeartbeat {
    pub fn new(
        workspace_id: WorkspaceId,
        instance_id: SidebarInstanceId,
        mux: MuxName,
        session_name: impl Into<String>,
        wakeup_socket: PathBuf,
        pane_id: Option<PaneId>,
    ) -> Self {
        Self {
            protocol_version: SIDEBAR_PROTOCOL_VERSION.to_owned(),
            workspace_id,
            instance_id,
            mux,
            session_name: session_name.into(),
            wakeup_socket,
            pane_id,
            build: None,
            version: None,
            size: None,
        }
    }

    /// Whether `path` is a sidebar heartbeat file (`sidebar.<id>.json`). The
    /// naming convention is owned here so the wakeup walk and the launch
    /// freshness gate agree on which files are heartbeats.
    pub fn is_heartbeat_file(path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("sidebar.") && name.ends_with(".json"))
    }

    /// Decode a heartbeat from its on-disk JSON. A parse failure maps to an IO
    /// error so callers handle read and decode uniformly.
    pub fn read_from(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(std::io::Error::other)
    }

    fn publish(&self, runtime: &RuntimePaths) -> Result<(), HeartbeatWriteErr> {
        let path = runtime.sidebar_heartbeat_path(&self.instance_id);
        let result = atomic::write_temp_then_rename_cache(&path, self)
            .map_err(|source| HeartbeatWriteErr { path, source });
        #[cfg(any(test, feature = "testkit"))]
        crate::testkit::host_pass::count(if result.is_ok() {
            crate::testkit::host_pass::Counter::HeartbeatWrite
        } else {
            crate::testkit::host_pass::Counter::HeartbeatFailure
        });
        result
    }
}

/// Walk a heartbeat directory, decode every sidebar heartbeat file, and keep
/// only current-protocol records. Freshness deliberately stays with callers:
/// all liveness readers check mtime TTL; session records compare mtimes.
pub fn read_current_heartbeats(dir: &Path) -> io::Result<Vec<(PathBuf, SidebarHeartbeat)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    let mut heartbeats = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                debug!(path = %dir.display(), error = %err, "sidebar heartbeat dir entry unreadable");
                continue;
            }
        };
        let path = entry.path();
        if !SidebarHeartbeat::is_heartbeat_file(&path) {
            continue;
        }
        let heartbeat = match SidebarHeartbeat::read_from(&path) {
            Ok(heartbeat) => heartbeat,
            Err(err) => {
                debug!(path = %path.display(), error = %err, "sidebar heartbeat unreadable");
                continue;
            }
        };
        if heartbeat.protocol_version != SIDEBAR_PROTOCOL_VERSION {
            debug!(
                path = %path.display(),
                protocol = heartbeat.protocol_version,
                expected = SIDEBAR_PROTOCOL_VERSION,
                "sidebar heartbeat unsupported protocol version"
            );
            continue;
        }
        heartbeats.push((path, heartbeat));
    }
    Ok(heartbeats)
}

#[derive(Debug, thiserror::Error)]
#[error("writing sidebar heartbeat {path}: {source}")]
pub struct HeartbeatWriteErr {
    pub path: PathBuf,
    #[source]
    pub source: atomic::AtomicErr,
}

/// One attachment's last published claim; renewals retain no file descriptor.
#[derive(Default)]
pub(crate) struct HeartbeatLease {
    published: Option<SidebarHeartbeat>,
}

impl HeartbeatLease {
    pub(crate) fn renew(
        &mut self,
        runtime: &RuntimePaths,
        heartbeat: &SidebarHeartbeat,
    ) -> Result<(), HeartbeatWriteErr> {
        if self.published.as_ref() != Some(heartbeat) {
            heartbeat.publish(runtime)?;
            self.published = Some(heartbeat.clone());
            return Ok(());
        }
        use rustix::fs::{AtFlags, CWD, Timestamps, UTIME_NOW, UTIME_OMIT, utimensat};
        use rustix::time::Timespec;

        let path = runtime.sidebar_heartbeat_path(&heartbeat.instance_id);
        let times = Timestamps {
            last_access: Timespec {
                tv_sec: 0,
                tv_nsec: UTIME_OMIT,
            },
            last_modification: Timespec {
                tv_sec: 0,
                tv_nsec: UTIME_NOW,
            },
        };
        match utimensat(CWD, &path, &times, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(()) => {
                #[cfg(any(test, feature = "testkit"))]
                crate::testkit::host_pass::count(
                    crate::testkit::host_pass::Counter::HeartbeatRenewal,
                );
                Ok(())
            }
            Err(rustix::io::Errno::NOENT) => heartbeat.publish(runtime),
            Err(source) => {
                #[cfg(any(test, feature = "testkit"))]
                crate::testkit::host_pass::count(
                    crate::testkit::host_pass::Counter::HeartbeatFailure,
                );
                Err(HeartbeatWriteErr {
                    source: atomic::AtomicErr::Io {
                        path: path.clone(),
                        source: source.into(),
                    },
                    path,
                })
            }
        }
    }
}

/// Write this sidebar instance's liveness heartbeat in-process.
///
/// The heartbeat is disposable runtime liveness, not store truth: publication
/// is cache-class temp-plus-rename without fsync. Attachments use a lease to
/// renew unchanged claims by mtime instead. The record carries this process's
/// build id when readable. The caller ensures the runtime dirs at startup.
#[allow(clippy::too_many_arguments)]
pub fn write_heartbeat(
    runtime: &RuntimePaths,
    workspace_id: WorkspaceId,
    instance_id: &SidebarInstanceId,
    mux: MuxName,
    session_name: &str,
    wakeup_socket: &Path,
    pane_id: Option<PaneId>,
    size: Option<SidebarSize>,
) -> Result<(), HeartbeatWriteErr> {
    let mut heartbeat = SidebarHeartbeat::new(
        workspace_id,
        instance_id.clone(),
        mux,
        session_name,
        wakeup_socket.to_path_buf(),
        pane_id,
    );
    heartbeat.build = crate::build_id::current().map(str::to_owned);
    heartbeat.version = Some(crate::build_id::VERSION.to_owned());
    heartbeat.size = size;
    heartbeat.publish(runtime)
}

/// Every fresh, current-protocol sidebar heartbeat in the workspace runtime dir.
/// Shared by launch, room admission, reload, and pane geometry reads; stale
/// mtimes, unreadable JSON, and mismatched protocols are skipped.
pub(crate) fn fresh_sidebar_heartbeats(rt: &RuntimePaths) -> Vec<SidebarHeartbeat> {
    let heartbeats = match read_current_heartbeats(&rt.heartbeat_dir) {
        Ok(heartbeats) => heartbeats,
        Err(err) => {
            debug!(path = %rt.heartbeat_dir.display(), error = %err, "sidebar heartbeat dir unreadable");
            return Vec::new();
        }
    };

    heartbeats
        .into_iter()
        .filter(|(path, _)| mtime_within_ttl(path))
        .map(|(_, heartbeat)| heartbeat)
        .collect()
}

pub(crate) fn mtime_within_ttl(path: &Path) -> bool {
    mtime_within_ttl_at(path, SystemTime::now)
}

fn mtime_within_ttl_at(path: &Path, now: impl FnOnce() -> SystemTime) -> bool {
    let modified = match fs::metadata(path).and_then(|meta| meta.modified()) {
        Ok(modified) => modified,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar runtime file metadata unreadable");
            return false;
        }
    };
    match now().duration_since(modified) {
        Ok(age) => age <= SIDEBAR_HEARTBEAT_TTL,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_counter_counts_full_publications_not_reads_or_failed_writes() {
        use crate::testkit::host_pass::{Counter, thread_counters};
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap();
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        let instance = SidebarInstanceId::new();
        let counts = thread_counters();
        counts.reset();
        runtime.ensure_dirs().unwrap();
        let path = runtime.sidebar_heartbeat_path(&instance);
        fs::create_dir(&path).unwrap();
        let write = || {
            write_heartbeat(
                &runtime,
                workspace.clone(),
                &instance,
                MuxName::Tmux,
                "rimz-test",
                &runtime.sidebar_host_wake_socket_path(MuxName::Tmux, "rimz-test"),
                None,
                None,
            )
        };
        write().expect_err("a directory cannot be replaced by a heartbeat file");
        assert_eq!(counts.get(Counter::HeartbeatWrite), 0);
        assert_eq!(counts.get(Counter::HeartbeatFailure), 1);
        fs::remove_dir(path).unwrap();
        write().unwrap();
        assert_eq!(counts.get(Counter::HeartbeatWrite), 1);
        assert_eq!(counts.get(Counter::HeartbeatFailure), 1);
        assert_eq!(fresh_sidebar_heartbeats(&runtime).len(), 1);
        assert_eq!(counts.get(Counter::HeartbeatWrite), 1);
    }

    #[test]
    fn sidebar_heartbeat_build_identity_is_backward_compatible() {
        let json = serde_json::json!({
            "protocol_version": SIDEBAR_PROTOCOL_VERSION,
            "workspace_id": WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            "instance_id": SidebarInstanceId::new(),
            "mux": "tmux",
            "session_name": "rimz-test",
            "wakeup_socket": "/tmp/sidebar.sock",
        });

        let heartbeat: SidebarHeartbeat =
            serde_json::from_value(json).expect("missing build identity defaults to None");
        assert_eq!(heartbeat.build, None);
        assert_eq!(heartbeat.version, None);
        assert_eq!(heartbeat.size, None);

        let encoded = serde_json::to_string(&heartbeat).expect("serialize heartbeat");
        assert!(
            !encoded.contains("\"build\""),
            "None build stays absent from heartbeat JSON",
        );
        assert!(
            !encoded.contains("\"version\""),
            "None version stays absent from heartbeat JSON",
        );
        assert!(
            !encoded.contains("\"size\""),
            "None size stays absent from heartbeat JSON",
        );
    }

    fn lease_fixture() -> (tempfile::TempDir, RuntimePaths, SidebarHeartbeat) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap();
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let heartbeat = SidebarHeartbeat::new(
            workspace,
            SidebarInstanceId::new(),
            MuxName::Tmux,
            "rimz-test",
            runtime.sidebar_host_wake_socket_path(MuxName::Tmux, "rimz-test"),
            Some(PaneId::from_parts(MuxName::Tmux, "%1")),
        );
        (dir, runtime, heartbeat)
    }

    #[test]
    fn heartbeat_v6_has_no_content_clock() {
        let (_dir, _runtime, heartbeat) = lease_fixture();
        let record = serde_json::to_value(heartbeat).unwrap();
        assert_eq!(record["protocol_version"], "rimz.plugin.v6");
        assert_eq!(
            record
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "protocol_version",
                "workspace_id",
                "instance_id",
                "mux",
                "session_name",
                "wakeup_socket",
                "pane_id",
            ]),
        );
    }

    #[test]
    fn unchanged_lease_renewals_keep_bytes_and_inode_across_ttls() {
        use crate::testkit::host_pass::{Counter, thread_counters};
        use std::os::unix::fs::MetadataExt;

        let (_dir, runtime, heartbeat) = lease_fixture();
        let path = runtime.sidebar_heartbeat_path(&heartbeat.instance_id);
        let mut lease = HeartbeatLease::default();
        let counts = thread_counters();
        counts.reset();
        lease.renew(&runtime, &heartbeat).unwrap();
        assert!(path.is_file(), "the initial claim must be published");
        let original = fs::File::open(&path).unwrap();
        let inode = original.metadata().unwrap().ino();
        let bytes = fs::read(&path).unwrap();
        for _ in 0..3 {
            let expired = SystemTime::now() - SIDEBAR_HEARTBEAT_TTL * 2;
            fs::File::open(&path)
                .unwrap()
                .set_modified(expired)
                .unwrap();
            assert!(!mtime_within_ttl(&path));
            lease.renew(&runtime, &heartbeat).unwrap();
            let metadata = fs::metadata(&path).unwrap();
            assert!(metadata.modified().unwrap() > expired);
            assert_eq!(metadata.ino(), inode);
            assert_eq!(fs::read(&path).unwrap(), bytes);
            assert_eq!(fresh_sidebar_heartbeats(&runtime).len(), 1);
            assert_eq!(crate::wakeup::reload_all(&runtime).unwrap(), 1);
        }
        assert_eq!(counts.get(Counter::HeartbeatWrite), 1);
        assert_eq!(counts.get(Counter::HeartbeatRenewal), 3);
        assert_eq!(counts.get(Counter::HeartbeatFailure), 0);
    }

    #[test]
    fn lease_claim_changes_publish_in_full_immediately() {
        use std::os::unix::fs::MetadataExt;

        let (_dir, runtime, heartbeat) = lease_fixture();
        let mut prior = runtime.sidebar_heartbeat_path(&heartbeat.instance_id);
        atomic::write_temp_then_rename_cache(&prior, &heartbeat).unwrap();
        let mut lease = HeartbeatLease::default();
        lease.renew(&runtime, &heartbeat).unwrap();
        let mut pane = heartbeat.clone();
        pane.pane_id = Some(PaneId::from_parts(MuxName::Tmux, "%2"));
        let mut build = pane.clone();
        build.build = Some("new-build".into());
        let mut size = build.clone();
        size.size = Some(SidebarSize { cols: 60, rows: 40 });
        let mut instance = size.clone();
        instance.instance_id = SidebarInstanceId::new();
        for claim in [pane, build, size, instance] {
            let original = fs::File::open(&prior).unwrap();
            lease.renew(&runtime, &claim).unwrap();
            let path = runtime.sidebar_heartbeat_path(&claim.instance_id);
            assert!(path.is_file(), "a changed claim must be published");
            let published = SidebarHeartbeat::read_from(&path).unwrap();
            assert_eq!(
                serde_json::to_value(published).unwrap(),
                serde_json::to_value(&claim).unwrap()
            );
            assert_ne!(
                fs::metadata(&path).unwrap().ino(),
                original.metadata().unwrap().ino()
            );
            prior = path;
        }
    }

    #[test]
    fn deleted_lease_record_is_republished_and_other_errors_retry() {
        use crate::testkit::host_pass::{Counter, thread_counters};

        let (dir, runtime, heartbeat) = lease_fixture();
        let path = runtime.sidebar_heartbeat_path(&heartbeat.instance_id);
        atomic::write_temp_then_rename_cache(&path, &heartbeat).unwrap();
        let mut lease = HeartbeatLease::default();
        lease.renew(&runtime, &heartbeat).unwrap();
        let counts = thread_counters();
        counts.reset();
        fs::remove_file(&path).unwrap();
        lease.renew(&runtime, &heartbeat).unwrap();
        assert!(path.is_file(), "a deleted record must be recreated");
        assert_eq!(counts.get(Counter::HeartbeatWrite), 1);
        assert_eq!(counts.get(Counter::HeartbeatRenewal), 0);

        let saved = dir.path().join("saved-heartbeats");
        fs::rename(&runtime.heartbeat_dir, &saved).unwrap();
        fs::write(&runtime.heartbeat_dir, b"not a directory").unwrap();
        assert!(lease.renew(&runtime, &heartbeat).is_err());
        assert_eq!(counts.get(Counter::HeartbeatFailure), 1);
        assert_eq!(counts.get(Counter::HeartbeatRenewal), 0);
        fs::remove_file(&runtime.heartbeat_dir).unwrap();
        fs::rename(saved, &runtime.heartbeat_dir).unwrap();
        lease.renew(&runtime, &heartbeat).unwrap();
        assert_eq!(counts.get(Counter::HeartbeatRenewal), 1);
        assert_eq!(counts.get(Counter::HeartbeatFailure), 1);
    }

    #[test]
    fn heartbeat_mtime_ttl_includes_equality_and_future_dates() {
        let (_dir, runtime, heartbeat) = lease_fixture();
        let path = runtime.sidebar_heartbeat_path(&heartbeat.instance_id);
        atomic::write_temp_then_rename_cache(&path, &heartbeat).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        assert!(mtime_within_ttl_at(&path, || modified + SIDEBAR_HEARTBEAT_TTL));
        assert!(!mtime_within_ttl_at(&path, || modified
            + SIDEBAR_HEARTBEAT_TTL
            + Duration::from_nanos(1)));
        assert!(mtime_within_ttl_at(&path, || modified - Duration::from_secs(1)));
    }
}
