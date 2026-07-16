use std::time::{Duration, Instant};

use rimz::mux::{ClientFocusOptions, MuxBackend, ZellijBackend, zellij};

use crate::common::CommandTimeoutExt;

use super::support::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn remote_lineage_reap_kills_only_the_matching_attached_client() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("reap");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let created = scoped_zellij(xdg.path())
        .args(["attach", "--create-background", &name])
        .bounded_output()
        .expect("create background session");
    assert!(created.status.success(), "create background session failed");
    wait_until_session_ready(xdg.path(), &name);
    wait_for_client_count(xdg.path(), &name, 0);

    let _client =
        AttachedClient::attach_with_lineage(xdg.path(), &name, "0123456789abcdef", 120, 40);
    wait_for_client_count(xdg.path(), &name, 1);
    let backend = ZellijBackend::with_runtime_dir(xdg.path());

    let other = zellij::reap_lineage_clients(&backend, &name, "fedcba9876543210")
        .expect("other-lineage reap");
    assert!(other.killed_pids.is_empty(), "other lineage: {other:?}");
    assert!(other.settled, "other lineage is a settled no-op: {other:?}");
    wait_for_client_count(xdg.path(), &name, 1);

    let same = zellij::reap_lineage_clients(&backend, &name, "0123456789abcdef")
        .expect("same-lineage reap");
    assert_eq!(same.killed_pids.len(), 1, "same lineage: {same:?}");
    assert_eq!(same.pre_clients, Some(1), "same lineage: {same:?}");
    assert_eq!(same.post_clients, Some(0), "same lineage: {same:?}");
    assert!(same.settled, "same lineage settles before return: {same:?}");
    assert!(!same.timed_out, "same lineage: {same:?}");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_for_client_count(xdg: &std::path::Path, session: &str, want: usize) {
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        let count = backend
            .client_view(ClientFocusOptions {
                session_name: Some(session.to_owned()),
                command_timeout: Some(Duration::from_secs(2)),
            })
            .map(|view| view.presence.human_clients)
            .unwrap_or(usize::MAX);
        if count == want {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "client count for {session} did not reach {want}; last count {count}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
