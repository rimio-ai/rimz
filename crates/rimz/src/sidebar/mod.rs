//! Sidebar process liveness helpers.
//!
//! The sidebar heartbeat remains a latency hint. A stale, unreadable, or
//! protocol-mismatched heartbeat never blocks a fresh launch.
//!
//! The invariant is one *producer* per workspace, one *renderer* per tab. Every
//! tab runs its own renderer; the eldest live instance is elected the producer
//! (UUIDv7 ids sort by birth) and forks `list-panes`/git, while younger
//! renderers read its published cache read-only. So the mux/git round-trip is
//! paid once per workspace without any per-tab renderer going dark. A launch
//! lock keeps concurrent attaches from each spawning a daemon, and the orphan
//! sweep reaps a SIGKILLed instance's runtime files.

pub mod cache;
pub mod consumer;
pub mod enrich;
pub mod events;
pub mod focus_anchor;
pub mod frame;
pub mod fuse;
pub mod heartbeat;
pub mod meter;
pub mod notify;
pub mod observe;
pub mod produce;
pub mod read_marks;
pub mod refresh;
#[cfg(test)]
pub(crate) mod test_support;
pub mod timing;
pub mod unread;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing::debug;

use crate::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
use crate::ledger::RuntimePaths;
use crate::ledger::atomic;
use crate::ledger::single_flight::{self, Coalesced};
use crate::mux::{DaemonView, MuxBackend, SidebarLiveness, SidebarPaneOptions};
use crate::sidebar::heartbeat::{SIDEBAR_PROTOCOL_VERSION, SidebarHeartbeat};
use crate::sidebar::timing::SIDEBAR_HEARTBEAT_TTL;

/// Launch-lock poll cadence: the producer holds the election lock while the
/// daemon it spawned starts and publishes its first heartbeat, and a peer queued
/// behind it polls this long before giving up to the runtime election. Longer
/// than the diff-stats window because production here is an async process spawn,
/// not a synchronous git fork. `25ms × 60 ≈ 1.5s`.
const LAUNCH_WAIT_STEP: Duration = Duration::from_millis(25);
const LAUNCH_WAIT_STEPS: u32 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarLaunchOutcome {
    SkippedFresh,
    Opened,
    Failed,
}

#[derive(Debug, thiserror::Error)]
#[error("writing sidebar heartbeat {path}: {source}")]
pub struct HeartbeatWriteErr {
    pub path: PathBuf,
    #[source]
    pub source: atomic::AtomicErr,
}

/// Write this sidebar instance's liveness heartbeat in-process.
///
/// The heartbeat is a runtime liveness file, not ledger truth, so the renderer
/// owns it directly rather than forking `rimz sidebar heartbeat` once per tick.
/// The JSON shape and the atomic temp-then-rename are identical to the CLI path
/// they replace, so the ledger wakeup fanout and the launch freshness gate that
/// read it are unchanged. The renderer ensures the runtime dirs at startup, so
/// this only does the write.
pub fn write_heartbeat(
    runtime: &RuntimePaths,
    workspace_id: WorkspaceId,
    instance_id: &SidebarInstanceId,
    mux: MuxName,
    session_name: &str,
    wakeup_socket: &Path,
    pane_id: Option<PaneId>,
) -> Result<(), HeartbeatWriteErr> {
    let mut heartbeat = SidebarHeartbeat::new(
        workspace_id,
        instance_id.clone(),
        mux,
        session_name,
        wakeup_socket.to_path_buf(),
        pane_id,
    );
    heartbeat.build = crate::build_id::current_if_ready().map(str::to_owned);
    let path = runtime.sidebar_heartbeat_path(instance_id);
    // Cache-class: a heartbeat is disposable liveness, rewritten every beat
    // and gc-swept when stale — surviving a power cut buys nothing.
    atomic::write_temp_then_rename_cache(&path, &heartbeat)
        .map_err(|source| HeartbeatWriteErr { path, source })
}

/// Every fresh, current-protocol sidebar heartbeat in the workspace runtime dir.
/// The shared scan behind the launch gate, the runtime election, and the reload
/// liveness set: a stale mtime, unreadable JSON, or mismatched protocol is
/// skipped (so an old-build sidebar drops out and reload replaces it).
pub(crate) fn fresh_sidebar_heartbeats(rt: &RuntimePaths) -> Vec<SidebarHeartbeat> {
    let entries = match fs::read_dir(&rt.heartbeat_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(err) => {
            debug!(path = %rt.heartbeat_dir.display(), error = %err, "sidebar heartbeat dir unreadable");
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !SidebarHeartbeat::is_heartbeat_file(&path) {
            continue;
        }
        if !mtime_within_ttl(&path) {
            continue;
        }
        let heartbeat = match SidebarHeartbeat::read_from(&path) {
            Ok(heartbeat) => heartbeat,
            Err(err) => {
                debug!(path = %path.display(), error = %err, "sidebar heartbeat unreadable");
                continue;
            }
        };
        if heartbeat.protocol_version == SIDEBAR_PROTOCOL_VERSION {
            out.push(heartbeat);
        }
    }
    out
}

fn fresh_sidebar_instances(rt: &RuntimePaths) -> Vec<SidebarInstanceId> {
    fresh_sidebar_heartbeats(rt)
        .into_iter()
        .map(|heartbeat| heartbeat.instance_id)
        .collect()
}

/// Grace for a just-spawned sidebar pane before reconcile may read "no
/// heartbeat yet" as "wedged": two heartbeat windows, so a just-added sidebar
/// is never closed before its first heartbeat lands — even by a reload run
/// seconds after the one that added it.
pub const FRESH_PANE_GRACE: Duration = SIDEBAR_HEARTBEAT_TTL.saturating_mul(2);

/// The live sidebars for one workspace runtime: every pane a fresh,
/// current-protocol heartbeat claims, plus whether any fresh heartbeat is
/// unlocated (no pane id). `rimz reload` folds this into the reconcile planner so
/// it keeps each view's live sidebar and replaces the wedged or duplicate ones.
pub fn sidebar_liveness(rt: &RuntimePaths) -> SidebarLiveness {
    let mut live = SidebarLiveness::default();
    for heartbeat in fresh_sidebar_heartbeats(rt) {
        match heartbeat.pane_id {
            Some(pane) => {
                live.claimed_panes.insert(pane);
            }
            None => live.has_unlocated = true,
        }
    }
    live
}

pub fn fresh_sidebar_present(rt: &RuntimePaths) -> bool {
    !fresh_sidebar_instances(rt).is_empty()
}

/// True when a live sidebar holds an older instance id than `own_id`. UUIDv7
/// ids sort by birth time, so the lowest id is the eldest. The eldest is the
/// sole producer (it finds no elder); every younger renderer reads its
/// published cache rather than forking its own `list-panes`/git (see
/// [`crate::sidebar`] module docs). The election trusts the same heartbeat TTL
/// as the launch gate, so a just-SIGKILLed elder is honoured for at most one
/// TTL before the next-eldest renderer takes over production.
pub fn elder_sidebar_instance(
    rt: &RuntimePaths,
    own_id: &SidebarInstanceId,
) -> Option<SidebarInstanceId> {
    fresh_sidebar_instances(rt)
        .into_iter()
        .filter(|id| id.as_str() < own_id.as_str())
        .min_by(|left, right| left.as_str().cmp(right.as_str()))
}

pub fn elder_sidebar_present(rt: &RuntimePaths, own_id: &SidebarInstanceId) -> bool {
    elder_sidebar_instance(rt, own_id).is_some()
}

/// Remove runtime files left by sidebars that exited without their RAII cleanup
/// (a SIGKILL skips it): heartbeats aged past the liveness TTL, sockets with no
/// live owner, and stale read-mark receipts from dead renderers. A live sidebar
/// re-stamps its heartbeat every tick, so a stale mtime is an honest "owner is
/// gone". A socket is kept while its owner is fresh (paired by short id) or
/// still starting up (bound before the first heartbeat — guarded by its own
/// fresh mtime). Best-effort: a removal race is ignored.
pub fn sweep_orphan_runtime(rt: &RuntimePaths) {
    let instances = fresh_sidebar_instances(rt);
    let live: HashSet<String> = instances.iter().map(|id| id.short().to_owned()).collect();
    let live_full: HashSet<String> = instances.iter().map(|id| id.as_str().to_owned()).collect();

    if let Ok(entries) = fs::read_dir(&rt.heartbeat_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if SidebarHeartbeat::is_heartbeat_file(&path) && !mtime_within_ttl(&path) {
                remove_orphan(&path);
            }
        }
    }
    if let Ok(entries) = fs::read_dir(&rt.sock_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(short) = sidebar_socket_short_id(&path) else {
                continue;
            };
            if !live.contains(&short) && !mtime_within_ttl(&path) {
                remove_orphan(&path);
            }
        }
    }
    if let Ok(entries) = fs::read_dir(&rt.read_marks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(instance_id) = read_marks::read_mark_file_instance_id(&path) else {
                continue;
            };
            if !live_full.contains(instance_id.as_str()) && !mtime_within_ttl(&path) {
                remove_orphan(&path);
            }
        }
    }
}

/// Launch the workspace sidebar daemon if no fresh one is present, coalescing
/// concurrent attaches through the single-flight election. `daemon` is forwarded
/// to a session (re)birth so `rimz start` can lead the session with the daemon
/// view (Zellij's only way to order it first); every other caller passes `None`.
pub fn launch_sidebar_if_needed(
    backend: &dyn MuxBackend,
    runtime: &RuntimePaths,
    opts: &SidebarPaneOptions,
    daemon: Option<&DaemonView>,
) -> SidebarLaunchOutcome {
    sweep_orphan_runtime(runtime);
    // Fast path before contending — `single_flight`'s contract is that the
    // caller has already missed a fresh read by the time it elects.
    if fresh_sidebar_present(runtime) {
        ensure_session_view(backend, runtime, opts);
        return SidebarLaunchOutcome::SkippedFresh;
    }
    // Serialize check-then-launch through the shared single-flight election so
    // two concurrent attaches to one shared session can't both spawn a daemon.
    // A peer that finds a fresh heartbeat while polling skips; the winner holds
    // the lock until its daemon publishes one. No lock dir or a wedged producer
    // falls to a local launch, and the runtime election reaps the loser.
    let lock_path = runtime.root.join("sidebar-launch.lock");
    let _guard =
        match single_flight::coalesce(&lock_path, LAUNCH_WAIT_STEP, LAUNCH_WAIT_STEPS, || {
            fresh_sidebar_present(runtime).then_some(())
        }) {
            Coalesced::Shared(()) => {
                ensure_session_view(backend, runtime, opts);
                return SidebarLaunchOutcome::SkippedFresh;
            }
            Coalesced::Produce(guard) => Some(guard),
            Coalesced::ProduceLocal => None,
        };
    let mut opts = opts.clone();
    opts.replace_existing = true;
    match backend.open_sidebar(&opts, daemon) {
        Ok(()) => {
            // Hold the election lock (`_guard`) until the new daemon publishes
            // its heartbeat, so an attach polling behind us reads it and skips.
            // The daemon writes the heartbeat just after start, well inside the
            // budget; a slow one falls to the election rather than stalling the
            // attach further.
            wait_for_fresh_sidebar(runtime);
            SidebarLaunchOutcome::Opened
        }
        Err(
            err @ (crate::mux::MuxErr::SocketPathTooLong { .. }
            | crate::mux::MuxErr::SocketPathReportedTooLong { .. }),
        ) => {
            tracing::debug!(
                session = %opts.session_name,
                mux = %backend.name(),
                error = %err,
                "sidebar pane launch hit zellij socket path limit; attach gate reports the fix",
            );
            SidebarLaunchOutcome::Failed
        }
        Err(err) => {
            tracing::warn!(
                session = %opts.session_name,
                mux = %backend.name(),
                error = %err,
                "sidebar pane launch failed; continuing without sidebar",
            );
            SidebarLaunchOutcome::Failed
        }
    }
}

fn ensure_session_view(
    backend: &dyn MuxBackend,
    runtime: &RuntimePaths,
    opts: &SidebarPaneOptions,
) {
    let live = sidebar_liveness(runtime);
    match backend.reconcile_sidebars(opts, &live) {
        Ok(_) => {}
        Err(crate::mux::MuxErr::SessionNotFound { session }) => tracing::debug!(
            session = %session,
            mux = %backend.name(),
            "sidebar reconcile skipped; session not addressable yet (pre-attach gate will rebirth it)",
        ),
        Err(err) => tracing::warn!(
            session = %opts.session_name,
            mux = %backend.name(),
            error = %err,
            "ensuring the session sidebar view failed; continuing",
        ),
    }
}

/// Poll for a fresh sidebar heartbeat, returning as soon as one appears, on the
/// same cadence the launch election polls. Held under the election lock so the
/// next launcher observes the daemon we just spawned instead of racing it.
fn wait_for_fresh_sidebar(rt: &RuntimePaths) {
    for _ in 0..LAUNCH_WAIT_STEPS {
        if fresh_sidebar_present(rt) {
            return;
        }
        std::thread::sleep(LAUNCH_WAIT_STEP);
    }
}

/// Short (12-hex) instance id embedded in a `sidebar.<short>.sock` path, or
/// `None` for any other file. Mirrors the socket naming in the renderer.
fn sidebar_socket_short_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    name.strip_prefix("sidebar.")?
        .strip_suffix(".sock")
        .map(str::to_owned)
}

fn remove_orphan(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => debug!(path = %path.display(), "swept orphaned sidebar runtime file"),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sweeping orphaned runtime file failed")
        }
    }
}

fn mtime_within_ttl(path: &Path) -> bool {
    let modified = match fs::metadata(path).and_then(|meta| meta.modified()) {
        Ok(modified) => modified,
        Err(err) => {
            debug!(path = %path.display(), error = %err, "sidebar runtime file metadata unreadable");
            return false;
        }
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => age <= SIDEBAR_HEARTBEAT_TTL,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;
    use crate::ids::{MuxName, SidebarInstanceId, WorkspaceId};

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
            let heartbeat = SidebarHeartbeat::new(
                self.workspace_id.clone(),
                id.clone(),
                MuxName::Tmux,
                "session",
                self.runtime
                    .sock_dir
                    .join(format!("sidebar.{}.sock", id.short())),
                pane_id,
            );
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
        let body = format!("{hex_tail:0>32}");
        SidebarInstanceId::parse(&format!("sb_{body}")).expect("valid instance id")
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

        let live = sidebar_liveness(&h.runtime);
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
        // same shape and freshness the ledger wakeup fanout and launch gate read.
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
    fn sweep_removes_orphan_socket_keeps_live_and_starting() {
        let h = Harness::new();
        let live = instance("0c");
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
        let live = instance("0d");
        h.write_sidebar_for(&live);
        let dead = instance("0e");
        let fresh = instance("0f");
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
}
