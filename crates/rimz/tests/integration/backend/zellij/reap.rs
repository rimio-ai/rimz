use std::time::{Duration, Instant};

use rimz::mux::{ClientFocusOptions, MuxBackend, ZellijBackend, zellij};

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
    let mut consecutive_matches = 0;
    let mut last_count = None;
    let mut last_error = String::new();
    loop {
        match backend
            .client_view(ClientFocusOptions {
                session_name: Some(session.to_owned()),
                command_timeout: Some(Duration::from_secs(2)),
            })
            .map(|view| view.presence.human_clients)
        {
            Ok(count) if count == want => {
                last_count = Some(count);
                last_error.clear();
                consecutive_matches += 1;
                if consecutive_matches == 2 {
                    return;
                }
            }
            Ok(count) => {
                last_count = Some(count);
                last_error.clear();
                consecutive_matches = 0;
            }
            Err(err) => {
                last_error = err.to_string();
                consecutive_matches = 0;
            }
        }
        assert!(
            Instant::now() < deadline,
            "client count for {session} did not stabilize at {want}; last count {last_count:?}; last error: {last_error}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
