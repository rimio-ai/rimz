//! The server-cwd contract: a tmux server only honours a pane's requested
//! directory while it can read its own.
//!
//! tmux's `spawn.c` performs the pane `chdir` only when `getcwd()` on the
//! server succeeds. A server born in a directory that is later deleted — a
//! disposable worktree, a swept tempdir — therefore strands every later pane
//! in that deleted directory even when RimZ passes an absolute `-c`.

use super::support::*;

/// The regression: RimZ runs every tmux client from `/`, so the server it
/// births inherits a directory that cannot be deleted, and panes keep landing
/// where they were asked to even after the launch directory is gone.
#[test]
fn panes_honor_cwd_after_the_launch_directory_is_deleted() {
    require_tmux!();
    let tempdir = TempDir::new().expect("tempdir");
    let runtime_root = tempdir.path().join("runtime");
    std::fs::create_dir_all(&runtime_root).expect("create runtime root");
    let project = tempdir.path().join("project");
    std::fs::create_dir_all(&project).expect("create project");

    // Birth the server from a directory that is then unlinked. nextest runs
    // each test in its own process, so moving this process's cwd is contained.
    let launch = tempdir.path().join("launch");
    std::fs::create_dir_all(&launch).expect("create launch dir");
    std::env::set_current_dir(&launch).expect("enter launch dir");

    let server = TmuxServer::in_runtime_root(&runtime_root);
    let workspace = WorkspaceId::from_project_root(&project);
    server
        .backend
        .ensure_session(&session_opts(
            "cwd-contract",
            workspace,
            &project,
            &project,
            Some((120, 40)),
        ))
        .expect("ensure session");

    std::fs::remove_dir(&launch).expect("unlink launch dir");

    // A window created after the launch directory vanished still lands where
    // it was asked to; before the managed endpoint pinned the client cwd this
    // reported the deleted directory instead.
    server.output(&[
        "new-window",
        "-t",
        "cwd-contract",
        "-c",
        project.to_str().expect("utf8 project"),
    ]);
    let panes = server.stdout(&["list-panes", "-a", "-F", "#{pane_current_path}"]);

    let canonical = project.canonicalize().expect("canonical project");
    for path in panes.lines() {
        assert_eq!(
            std::path::Path::new(path.trim()),
            canonical,
            "every pane opens in the requested directory, got:\n{panes}",
        );
    }
    assert!(
        !panes.contains("(deleted)"),
        "no pane inherits the deleted launch directory:\n{panes}",
    );
}

/// A server RimZ did not birth can still be poisoned — one left over from an
/// older release, or started by hand. Birth proves the server can place a pane
/// and refuses with a socket-scoped fix rather than opening a broken room.
#[test]
fn birth_refuses_a_server_whose_own_directory_was_deleted() {
    require_tmux!();
    let tempdir = TempDir::new().expect("tempdir");
    let runtime_root = tempdir.path().join("runtime");
    let project = tempdir.path().join("project");
    std::fs::create_dir_all(&project).expect("create project");
    let socket = rimz::mux::tmux::managed_server_socket_path_under(&runtime_root);
    std::fs::create_dir_all(socket.parent().expect("socket parent")).expect("create socket dir");

    // Poison the endpoint: birth a server from a directory, then delete it.
    let launch = tempdir.path().join("launch");
    std::fs::create_dir_all(&launch).expect("create launch dir");
    let born = std::process::Command::new("tmux")
        .scrub_session_env()
        .current_dir(&launch)
        .arg("-S")
        .arg(&socket)
        .args(["new-session", "-d", "-s", "squatter", "sleep 120"])
        .bounded_status()
        .expect("spawn poisoned server");
    assert!(born.success(), "poisoned server should start");
    std::fs::remove_dir(&launch).expect("unlink launch dir");

    let server = TmuxServer::in_runtime_root(&runtime_root);
    let err = server
        .backend
        .ensure_session(&session_opts(
            "poisoned-room",
            WorkspaceId::from_project_root(&project),
            &project,
            &project,
            Some((120, 40)),
        ))
        .expect_err("birth must refuse a server that cannot place panes");

    let message = err.to_string();
    assert!(
        message.contains("kill-server"),
        "the fix restarts the server, got: {message}",
    );
    assert!(
        message.contains(socket.to_str().expect("utf8 socket")),
        "the fix is scoped to the RimZ socket so other tmux servers survive, got: {message}",
    );
}
