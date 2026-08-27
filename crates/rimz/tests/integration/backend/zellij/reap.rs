use std::time::{Duration, Instant};

use rimz::mux::{ClientFocusOptions, MuxBackend, ZellijBackend, zellij};

use super::support::*;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn remote_lineage_reap_kills_only_the_matching_attached_client() {
    require_zellij!();

    let room = LiveZellijSession::new("reap");
    let xdg = room.path();
    let cwd = tempfile::tempdir().expect("session cwd");
    let name = room.name().to_owned();
    // Birth the session in the background and prove it answers actions before
    // any client attaches. A PTY `attach --create` births and attaches in one
    // unchecked step, and a starved host lets that client sit forever against a
    // server that never came up — the session under test has to exist on a
    // checked command.
    room.create_plain_background(cwd.path(), "600");

    let mut client = AttachedClient::attach_with_lineage(&room, "0123456789abcdef", 120, 40);
    wait_for_client_count(xdg, &name, 1, &mut client);
    let client_pid = client.pid();
    wait_for_attached_lineage_client(client_pid, &name, "0123456789abcdef");
    let backend = ZellijBackend::with_runtime_dir(xdg);

    let other = zellij::reap_lineage_clients(&backend, &name, "fedcba9876543210")
        .expect("other-lineage reap");
    assert!(other.killed_pids.is_empty(), "other lineage: {other:?}");
    assert!(other.settled, "other lineage is a settled no-op: {other:?}");
    wait_for_client_count(xdg, &name, 1, &mut client);

    let same = zellij::reap_lineage_clients(&backend, &name, "0123456789abcdef")
        .expect("same-lineage reap");
    assert_eq!(same.killed_pids, vec![client_pid], "same lineage: {same:?}");
    assert_eq!(same.pre_clients, Some(1), "same lineage: {same:?}");
    assert_eq!(same.post_clients, Some(0), "same lineage: {same:?}");
    assert!(same.settled, "same lineage settles before return: {same:?}");
    assert!(!same.timed_out, "same lineage: {same:?}");
}

#[test]
fn supervised_remote_wrapper_distinguishes_session_loss_from_detach() {
    require_zellij!();

    let killed = LiveZellijSession::new("remote-loss");
    let cwd = tempfile::tempdir().expect("session cwd");
    killed.create_plain_background(cwd.path(), "600");
    let mut killed_client = AttachedClient::attach_remote_wrapper(&killed, 120, 40);
    wait_for_client_count(killed.path(), killed.name(), 1, &mut killed_client);

    killed
        .backend()
        .kill_session(killed.name())
        .expect("kill remote session");
    let killed_status = wait_for_client_exit(&mut killed_client, "killed remote session");
    assert_eq!(
        killed_status.exit_code(),
        rimz::remote::REMOTE_SESSION_LOST_EXIT as u32,
        "lost session exits through the reconnect sentinel; output: {:?}",
        String::from_utf8_lossy(&killed_client.output_bytes())
    );

    let detached = LiveZellijSession::new("remote-detach");
    detached.create_plain_background(cwd.path(), "600");
    let mut detached_client = AttachedClient::attach_remote_wrapper(&detached, 120, 40);
    wait_for_client_count(detached.path(), detached.name(), 1, &mut detached_client);

    detached_client.press_detach();
    let detached_status = wait_for_client_exit(&mut detached_client, "explicit detach");
    assert!(
        detached_status.success(),
        "detach preserves zero; output: {:?}",
        String::from_utf8_lossy(&detached_client.output_bytes())
    );
    assert_eq!(
        detached
            .backend()
            .session_liveness(detached.name())
            .expect("detached session liveness"),
        rimz::mux::SessionLiveness::Live
    );
}

fn wait_for_client_exit(client: &mut AttachedClient, label: &str) -> portable_pty::ExitStatus {
    poll_until(
        SPAWN_TIMEOUT,
        || Ok(client.exit_status()),
        Option::is_some,
        label,
    )
    .expect("client exit status")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_for_attached_lineage_client(pid: u32, session: &str, lineage: &str) {
    poll_until(
        SPAWN_TIMEOUT,
        || {
            Ok((
                rimz::proc::list_processes()
                    .iter()
                    .any(|process| process.pid == pid),
                rimz::proc::comm(pid),
                rimz::proc::argv(pid),
                rimz::proc::env_var(pid, rimz::remote::REMOTE_LINEAGE_ENV),
                rimz::proc::process_start_token(pid),
            ))
        },
        |(listed, comm, argv, observed_lineage, start_token)| {
            *listed
                && comm.as_deref() == Some("zellij")
                && argv.as_ref().is_some_and(|argv| {
                    argv.len() == 4
                        && argv[1].to_str() == Some("attach")
                        && argv[2].to_str() == Some("--create")
                        && argv[3].to_str() == Some(session)
                })
                && observed_lineage.as_deref() == Some(lineage)
                && start_token.is_some()
        },
        &format!("attached lineage client pid {pid} to enter the process snapshot"),
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_for_client_count(
    xdg: &std::path::Path,
    session: &str,
    want: usize,
    client: &mut AttachedClient,
) {
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
        // A client that exited takes the count with it, so name that cause
        // directly rather than spending the whole deadline on a count that can
        // no longer move.
        if let Some(status) = client.exit_status() {
            panic!(
                "attached client for {session} exited with {status} while waiting for {want} clients; last count {last_count:?}; last error: {last_error}"
            );
        }
        assert!(
            Instant::now() < deadline,
            "client count for {session} did not stabilize at {want}; last count {last_count:?}; last error: {last_error}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}
