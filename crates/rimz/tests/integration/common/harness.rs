//! In-process ledger fixture (the library tier): opens a real [`Ledger`] over
//! a tempdir for tests that drive ledger APIs directly without spawning the
//! `rimz` binary.

use rimz::{Ledger, RuntimePaths, StatePaths, WorkspaceId};
use tempfile::TempDir;

/// In-process ledger fixture for tests that drive `Ledger` APIs directly.
pub struct Harness {
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
            workspace_id,
            runtime_paths,
            ledger,
            _tempdir: tempdir,
        }
    }

    /// Library-tier twin of [`crate::common::Env::skip_if_sandboxed`]: ensures
    /// the socket dir exists, then returns `true` (after a `warn!`) when the
    /// sandbox forbids binding AF_UNIX datagram sockets. Bridge tests call this
    /// at the top and return early.
    pub fn skip_if_sandboxed(&self) -> bool {
        std::fs::create_dir_all(&self.runtime_paths.sock_dir).expect("mkdir sock");
        if super::af_unix_bind_sandboxed(&self.runtime_paths.sock_dir) {
            tracing::warn!("skipping: AF_UNIX bind is forbidden in this sandbox");
            return true;
        }
        false
    }
}
