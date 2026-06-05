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

    /// Publish every fork-bearing produce input fresh, so an in-process
    /// [`rimz::sidebar::produce::produce_snapshot`] call pays no mux, no
    /// subprocess, and no transcript walk: the pane frame (the single-flight
    /// cache's fast path serves it), the provider-spending stamp, and the
    /// accounts stamp. Re-call right before each produce under test — the
    /// pane frame rides the short poll-mode TTL.
    pub fn publish_fresh_produce_inputs(&self, session: &str, panes: Vec<rimz::feed::PaneRef>) {
        let now_ms = rimz::sidebar::snapshot::unix_now_ms();
        let frame = rimz::sidebar::snapshot::SnapshotCache {
            produced_at_ms: now_ms,
            session_name: session.to_owned(),
            panes,
        };
        std::fs::write(
            self.runtime_paths.root.join("snapshot.json"),
            serde_json::to_vec(&frame).expect("serialize pane frame"),
        )
        .expect("publish pane frame");
        rimz::agents::spending::write_provider_spending_cache(
            &self.runtime_paths.root.join("provider-spending.json"),
            now_ms,
            &rimz::agents::spending::Spending::default(),
        );
        let accounts = rimz::sidebar::snapshot::AccountsCache {
            refreshed_at_ms: now_ms,
            accounts: Default::default(),
            ok: true,
        };
        std::fs::write(
            self.runtime_paths.root.join("accounts.json"),
            serde_json::to_vec(&accounts).expect("serialize accounts"),
        )
        .expect("publish accounts");
    }
}
