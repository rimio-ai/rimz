//! Shared harness for integration tests. Real tempdir, real ledger files —
//! no in-memory stubs per `docs/contributing/testing.md`.

#![allow(dead_code)]

pub mod redact;

use std::io;
use std::os::unix::net::UnixDatagram;

use std::path::PathBuf;

use rimz::{Ledger, RuntimePaths, StatePaths, WorkspaceId};
use tempfile::TempDir;

pub struct Harness {
    pub state_root: PathBuf,
    pub runtime_root: PathBuf,
    pub workspace_id: WorkspaceId,
    pub runtime_paths: RuntimePaths,
    pub ledger: Ledger,
    _tempdir: TempDir,
}

impl Harness {
    pub fn new() -> Self {
        let tempdir = TempDir::new().expect("tempdir");
        let state_root = tempdir.path().join("state");
        let runtime_root = tempdir.path().join("runtime");
        let workspace_id = WorkspaceId::from_project_root(tempdir.path());
        let paths = StatePaths::under(workspace_id.clone(), &state_root).expect("state paths");
        let runtime_paths =
            RuntimePaths::under(workspace_id.clone(), &runtime_root).expect("runtime paths");
        let ledger = Ledger::open(paths, runtime_paths.clone()).expect("open ledger");

        Self {
            state_root,
            runtime_root,
            workspace_id,
            runtime_paths,
            ledger,
            _tempdir: tempdir,
        }
    }
}

/// Probe whether the current sandbox forbids binding AF_UNIX datagram
/// sockets. Returns `true` when a bind under `dir` fails with `EPERM` /
/// `EACCES` (`io::ErrorKind::PermissionDenied`) — the shape we see in
/// hermetic CI sandboxes that block `bind(2)` on Unix sockets. Tests that
/// would otherwise hard-fail should call this at the top, emit a
/// `tracing::warn!`, and return early — mirroring the "skip if mux binary
/// missing" idiom used by the zellij/tmux backend tests.
pub fn af_unix_bind_sandboxed(dir: &std::path::Path) -> bool {
    let probe = dir.join("rimz-af-unix-probe.sock");
    let _ = std::fs::remove_file(&probe);
    match UnixDatagram::bind(&probe) {
        Ok(sock) => {
            drop(sock);
            let _ = std::fs::remove_file(&probe);
            false
        }
        Err(e) if matches!(e.kind(), io::ErrorKind::PermissionDenied) => true,
        Err(_) => false,
    }
}
