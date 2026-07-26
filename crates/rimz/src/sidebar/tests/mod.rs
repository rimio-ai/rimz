use std::time::Duration;

use tempfile::TempDir;

use super::*;
use crate::ids::{MuxName, SidebarInstanceId, WorkspaceId};
use crate::sidebar::heartbeat::SIDEBAR_PROTOCOL_VERSION;

mod launch;

struct Harness {
    _dir: TempDir,
    runtime: RuntimePaths,
    workspace_id: WorkspaceId,
}

impl Harness {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let workspace_id = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).expect("runtime");
        Self {
            _dir: dir,
            runtime,
            workspace_id,
        }
    }

    fn ensure_runtime(&self) {
        self.runtime.ensure_dirs().expect("runtime dirs");
    }

    fn path(&self) -> &Path {
        self._dir.path()
    }

    fn write_sidebar(&self, filename: &str, protocol_version: &str) -> std::path::PathBuf {
        self.ensure_runtime();
        let mut heartbeat = SidebarHeartbeat::new(
            self.workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Tmux,
            "session",
            self.runtime.sock_dir.join("sidebar.sock"),
            None,
        );
        heartbeat.protocol_version = protocol_version.to_owned();
        let path = self.runtime.heartbeat_dir.join(filename);
        std::fs::write(&path, serde_json::to_vec(&heartbeat).expect("json"))
            .expect("write heartbeat");
        path
    }

    /// Write a fresh, current-protocol heartbeat carrying `id`, at the path
    /// the renderer would use (`sidebar.<id>.json`).
    fn write_sidebar_for(&self, id: &SidebarInstanceId) -> std::path::PathBuf {
        self.write_sidebar_with_pane(id, None)
    }

    /// As [`Self::write_sidebar_for`], but stamping the heartbeat's claimed
    /// pane — exercised by the reload liveness set.
    fn write_sidebar_with_pane(
        &self,
        id: &SidebarInstanceId,
        pane_id: Option<crate::ids::PaneId>,
    ) -> std::path::PathBuf {
        self.ensure_runtime();
        let mut heartbeat = SidebarHeartbeat::new(
            self.workspace_id.clone(),
            id.clone(),
            MuxName::Tmux,
            "session",
            self.runtime
                .sock_dir
                .join(format!("sidebar.{}.sock", id.short())),
            pane_id,
        );
        heartbeat.build = Some("current".to_owned());
        let path = self.runtime.sidebar_heartbeat_path(id);
        std::fs::write(&path, serde_json::to_vec(&heartbeat).expect("json"))
            .expect("write heartbeat");
        path
    }
}

fn make_stale(path: &Path) {
    let old = SystemTime::now() - SIDEBAR_HEARTBEAT_TTL - Duration::from_secs(1);
    std::fs::File::open(path)
        .expect("open runtime file")
        .set_modified(old)
        .expect("set mtime");
}

fn instance(hex_tail: &str) -> SidebarInstanceId {
    // UUIDv7 ids sort lexicographically by birth; craft deterministic ids so
    // a test controls who is the elder. 32 hex chars after the `sb_` prefix.
    SidebarInstanceId::parse(&format!("sb_{hex_tail:0>32}")).expect("valid instance id")
}

#[test]
fn session_build_drift_uses_only_foreign_builds() {
    let drift = session_build_drift_from(
        [
            (Some("bbb"), Some("0.4.0")),
            (Some("aaa"), Some("0.5.0")),
            (Some("ccc"), Some("0.3.0")),
            (Some("bbb"), Some("0.4.0")),
        ],
        "aaa",
    )
    .expect("foreign build drifts");
    assert_eq!(drift.versions, ["0.3.0", "0.4.0"]);
    assert!(
        session_build_drift_from([(Some("bbb"), None)], "aaa")
            .expect("foreign build drifts")
            .versions
            .is_empty()
    );
    assert!(
        session_build_drift_from([(Some("aaa"), Some("0.3.0")), (None, Some("0.2.0"))], "aaa")
            .is_none()
    );
}

#[test]
fn liveness_collects_fresh_panes_flags_unlocated_and_skips_stale() {
    use crate::ids::{MuxName, PaneId};

    let h = Harness::new();
    let located = instance("a1");
    let unlocated = instance("b2");
    let stale = instance("c3");
    h.write_sidebar_with_pane(&located, Some(PaneId::from_parts(MuxName::Tmux, "%7")));
    h.write_sidebar_with_pane(&unlocated, None);
    let stale_path =
        h.write_sidebar_with_pane(&stale, Some(PaneId::from_parts(MuxName::Tmux, "%9")));
    make_stale(&stale_path);

    let live = sidebar_liveness(&h.runtime, "current", MuxName::Tmux, "session");
    assert!(
        live.claimed_panes
            .contains(&PaneId::from_parts(MuxName::Tmux, "%7")),
        "a fresh heartbeat's pane is claimed",
    );
    assert!(
        !live
            .claimed_panes
            .contains(&PaneId::from_parts(MuxName::Tmux, "%9")),
        "a stale sidebar claims no pane",
    );
    assert_eq!(
        live.claimed_panes.len(),
        1,
        "only the fresh, located heartbeat claims a pane",
    );
    assert!(
        live.has_unlocated,
        "a fresh heartbeat with no pane id flags the wildcard",
    );
}

#[test]
fn fresh_sidebar_present_accepts_only_current_fresh_readable_heartbeats() {
    let absent = Harness::new();
    assert!(!fresh_sidebar_present(&absent.runtime), "absent dir");

    let fresh = Harness::new();
    fresh.write_sidebar("sidebar.fresh.json", SIDEBAR_PROTOCOL_VERSION);
    assert!(
        fresh_sidebar_present(&fresh.runtime),
        "fresh current protocol"
    );

    let stale = Harness::new();
    make_stale(&stale.write_sidebar("sidebar.stale.json", SIDEBAR_PROTOCOL_VERSION));
    assert!(!fresh_sidebar_present(&stale.runtime), "stale heartbeat");

    let old_protocol = Harness::new();
    old_protocol.write_sidebar("sidebar.old.json", "rimz.plugin.v0");
    assert!(
        !fresh_sidebar_present(&old_protocol.runtime),
        "old protocol"
    );

    let unreadable = Harness::new();
    unreadable.ensure_runtime();
    std::fs::write(
        unreadable
            .runtime
            .heartbeat_dir
            .join("sidebar.invalid.json"),
        b"{ not json",
    )
    .expect("write invalid heartbeat");
    assert!(
        !fresh_sidebar_present(&unreadable.runtime),
        "unreadable json"
    );
}

#[test]
fn in_process_write_heartbeat_is_fresh_and_round_trips() {
    // The renderer writes its heartbeat in-process now; it must land in the
    // same shape and freshness the store wakeup fanout and launch gate read.
    let h = Harness::new();
    h.ensure_runtime();
    let instance = SidebarInstanceId::new();
    let socket = h.runtime.sock_dir.join("sidebar.test.sock");

    write_heartbeat(
        &h.runtime,
        h.workspace_id.clone(),
        &instance,
        MuxName::Zellij,
        "rimz-test",
        &socket,
        None,
    )
    .expect("write heartbeat");

    assert!(fresh_sidebar_present(&h.runtime));
    let path = h.runtime.sidebar_heartbeat_path(&instance);
    let hb = SidebarHeartbeat::read_from(&path).expect("read back");
    assert_eq!(hb.instance_id, instance);
    assert_eq!(hb.protocol_version, SIDEBAR_PROTOCOL_VERSION);
    assert_eq!(hb.mux, MuxName::Zellij);
    assert_eq!(hb.wakeup_socket, socket);
    assert!(hb.build.is_some(), "heartbeat carries the running build id");
}

#[test]
fn younger_yields_to_live_elder_eldest_survives() {
    let h = Harness::new();
    let elder = instance("01");
    let middle = instance("02");
    let younger = instance("03");
    h.write_sidebar_for(&elder);
    h.write_sidebar_for(&middle);
    h.write_sidebar_for(&younger);
    // The younger sees an older live instance and yields; the eldest finds no
    // elder and stays, so exactly one survives.
    assert!(elder_sidebar_present(&h.runtime, &younger));
    assert_eq!(
        elder_sidebar_instance(&h.runtime, &younger),
        Some(elder.clone())
    );
    assert!(!elder_sidebar_present(&h.runtime, &elder));
}

#[test]
fn producer_election_tracker_shares_warm_elder_without_rescanning() {
    let h = Harness::new();
    let elder = instance("01");
    let younger = instance("09");
    let path = h.write_sidebar_for(&elder);
    let modified = SystemTime::now() - Duration::from_secs(1);
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let tracker = ProducerElectionTracker::new(h.runtime.clone(), younger);

    assert_eq!(tracker.elder_instance_at(modified), Some(elder.clone()));
    assert_eq!(tracker.full_scan_count(), 1);
    std::fs::remove_file(path).unwrap();

    let clone = tracker.clone();
    assert_eq!(
        clone.elder_instance_at(modified + SIDEBAR_HEARTBEAT_TTL - Duration::from_millis(1)),
        Some(elder),
    );
    assert_eq!(clone.full_scan_count(), 1, "clones share the warm memo");
    assert_eq!(
        clone.elder_instance_at(modified + SIDEBAR_HEARTBEAT_TTL),
        None,
    );
    assert_eq!(
        clone.full_scan_count(),
        2,
        "missing elder forces one rescan"
    );
}

#[test]
fn producer_election_tracker_refreshes_one_cached_elder_at_expiry() {
    let h = Harness::new();
    let elder = instance("01");
    let younger = instance("09");
    let path = h.write_sidebar_for(&elder);
    let first_modified = SystemTime::now() - Duration::from_secs(1);
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(first_modified)
        .unwrap();
    let tracker = ProducerElectionTracker::new(h.runtime.clone(), younger);
    assert_eq!(
        tracker.elder_instance_at(first_modified),
        Some(elder.clone())
    );

    let refreshed_modified = first_modified + HEARTBEAT_WRITE_INTERVAL;
    h.write_sidebar_for(&elder);
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(refreshed_modified)
        .unwrap();

    assert_eq!(
        tracker.elder_instance_at(first_modified + SIDEBAR_HEARTBEAT_TTL),
        Some(elder),
    );
    assert_eq!(
        tracker.full_scan_count(),
        1,
        "expiry validates only the cached heartbeat"
    );
}

#[test]
fn producer_election_tracker_rechecks_producer_on_heartbeat_cadence() {
    let h = Harness::new();
    h.ensure_runtime();
    let older = instance("01");
    let own = instance("09");
    let tracker = ProducerElectionTracker::new(h.runtime.clone(), own);
    let now = SystemTime::now();

    assert_eq!(tracker.elder_instance_at(now), None);
    h.write_sidebar_for(&older);
    assert_eq!(
        tracker.elder_instance_at(now + HEARTBEAT_WRITE_INTERVAL - Duration::from_millis(1)),
        None,
    );
    assert_eq!(tracker.full_scan_count(), 1);
    assert_eq!(
        tracker.elder_instance_at(now + HEARTBEAT_WRITE_INTERVAL),
        Some(older),
    );
    assert_eq!(tracker.full_scan_count(), 2);
}

#[test]
fn producer_election_tracker_ignores_build_changes_but_liveness_exposes_them() {
    let h = Harness::new();
    let elder = instance("01");
    let own = instance("09");
    let path = h.write_sidebar_for(&elder);
    let modified = SystemTime::now() - Duration::from_secs(1);
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let tracker = ProducerElectionTracker::new(h.runtime.clone(), own);
    assert_eq!(tracker.elder_instance_at(modified), Some(elder.clone()));

    let mut heartbeat = SidebarHeartbeat::read_from(&path).unwrap();
    heartbeat.build = Some("new-build".to_owned());
    std::fs::write(&path, serde_json::to_vec(&heartbeat).unwrap()).unwrap();
    let refreshed = modified + HEARTBEAT_WRITE_INTERVAL;
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(refreshed)
        .unwrap();

    assert_eq!(
        tracker.elder_instance_at(modified + SIDEBAR_HEARTBEAT_TTL),
        Some(elder),
    );
    assert_eq!(tracker.full_scan_count(), 1);
    assert!(
        fresh_sidebar_heartbeats(&h.runtime)
            .iter()
            .any(|heartbeat| heartbeat.build.as_deref() == Some("new-build"))
    );
}

#[test]
fn no_elder_when_alone() {
    let h = Harness::new();
    let only = instance("05");
    h.write_sidebar_for(&only);
    assert!(!elder_sidebar_present(&h.runtime, &only));
}

#[test]
fn stale_or_wrong_protocol_elder_is_not_honored() {
    let h = Harness::new();
    let younger = instance("09");
    // A stale lower id is a dead elder — ignored, so the survivor does not
    // yield to a ghost (recovery is bounded by the heartbeat TTL).
    let stale_elder = instance("07");
    make_stale(&h.write_sidebar_for(&stale_elder));
    // A wrong-protocol lower id is not a peer we hand off to.
    h.write_sidebar("sidebar.0000000000000008.json", "rimz.plugin.v0");
    assert!(!elder_sidebar_present(&h.runtime, &younger));
}

#[test]
fn sweep_removes_stale_heartbeat_keeps_fresh() {
    let h = Harness::new();
    let live = instance("0a");
    let dead = instance("0b");
    h.write_sidebar_for(&live);
    let dead_path = h.write_sidebar_for(&dead);
    make_stale(&dead_path);

    sweep_orphan_runtime(&h.runtime);

    assert!(h.runtime.sidebar_heartbeat_path(&live).exists());
    assert!(!dead_path.exists());
    assert!(fresh_sidebar_present(&h.runtime));
}

#[test]
fn purge_rebirth_heartbeats_removes_all_heartbeats_only() {
    let h = Harness::new();
    let live = instance("0c");
    let stale = instance("0d");
    let live_path = h.write_sidebar_for(&live);
    let stale_path = h.write_sidebar_for(&stale);
    make_stale(&stale_path);
    let socket = h.runtime.sock_dir.join("sidebar.keep.sock");
    let read_marks = h.runtime.read_marks_dir.join("sidebar.keep.json");
    let other = h.runtime.heartbeat_dir.join("producer.json");
    std::fs::write(&socket, b"").expect("write socket");
    std::fs::write(&read_marks, b"{}").expect("write read marks");
    std::fs::write(&other, b"{}").expect("write non-heartbeat");

    purge_rebirth_heartbeats(&h.runtime);

    assert!(!live_path.exists(), "fresh heartbeat removed at rebirth");
    assert!(!stale_path.exists(), "stale heartbeat removed at rebirth");
    assert!(socket.exists(), "sockets are not part of the rebirth purge");
    assert!(
        read_marks.exists(),
        "read marks are not part of the rebirth purge"
    );
    assert!(other.exists(), "non-heartbeat files are kept");
    assert!(!fresh_sidebar_present(&h.runtime));
}

#[test]
fn purge_rebirth_heartbeats_missing_dir_is_noop() {
    let h = Harness::new();

    purge_rebirth_heartbeats(&h.runtime);

    assert!(!h.runtime.heartbeat_dir.exists());
}

#[test]
fn sweep_removes_orphan_socket_keeps_live_and_starting() {
    let h = Harness::new();
    let live = instance("0e");
    h.write_sidebar_for(&live);

    let live_sock = h
        .runtime
        .sock_dir
        .join(format!("sidebar.{}.sock", live.short()));
    let orphan_sock = h.runtime.sock_dir.join("sidebar.ffffffffffff.sock");
    let starting_sock = h.runtime.sock_dir.join("sidebar.eeeeeeeeeeee.sock");
    for sock in [&live_sock, &orphan_sock, &starting_sock] {
        std::fs::write(sock, b"").expect("write socket file");
    }
    // The orphan's owner is long gone (no heartbeat, stale socket); the
    // starting socket is bound before its first heartbeat, so its fresh
    // mtime protects it.
    make_stale(&orphan_sock);

    sweep_orphan_runtime(&h.runtime);

    assert!(live_sock.exists(), "live owner's socket kept");
    assert!(starting_sock.exists(), "startup-window socket kept");
    assert!(!orphan_sock.exists(), "dead owner's socket swept");
}

#[test]
fn sweep_removes_orphan_read_marks_keeps_live_and_fresh() {
    let h = Harness::new();
    h.ensure_runtime();
    let live = instance("0f");
    h.write_sidebar_for(&live);
    let dead = instance("10");
    let fresh = instance("11");
    let live_marks = h.runtime.sidebar_read_marks_path(&live);
    let dead_marks = h.runtime.sidebar_read_marks_path(&dead);
    let fresh_marks = h.runtime.sidebar_read_marks_path(&fresh);
    let manual_marks = h.runtime.read_marks_dir.join("manual.json");
    for path in [&live_marks, &dead_marks, &fresh_marks, &manual_marks] {
        std::fs::write(path, br#"{"marks":{"row-a":1000}}"#).expect("write read marks");
    }
    make_stale(&live_marks);
    make_stale(&dead_marks);
    make_stale(&manual_marks);

    sweep_orphan_runtime(&h.runtime);

    assert!(live_marks.exists(), "live owner's read marks kept");
    assert!(fresh_marks.exists(), "fresh startup-window read marks kept");
    assert!(manual_marks.exists(), "manual API read marks kept");
    assert!(!dead_marks.exists(), "dead owner's read marks swept");
}
