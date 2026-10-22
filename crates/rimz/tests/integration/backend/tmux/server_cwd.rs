//! The server-cwd contract: panes land in the directory they ask for, and birth
//! refuses a server that cannot place them there.
//!
//! Since tmux 3.7, `spawn.c` performs the pane `chdir` only when `getcwd()` on
//! the server succeeds. A server born in a directory that is later deleted
//! therefore strands every later pane there, even with an absolute `-c`.
//! Earlier releases honour `-c` regardless of the server's cwd.

use super::support::*;

/// RimZ runs every tmux client from `/`, so its server inherits a directory
/// that cannot be deleted and panes keep landing where requested after the
/// launch directory is gone. This guards the client-cwd pin on tmux 3.7 and
/// later; earlier releases honour `-c` without it.
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

    // A later window still lands where requested; on tmux 3.7 and later,
    // without the client-cwd pin this reported the deleted directory instead.
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

/// A server RimZ did not birth can have a deleted cwd. Since tmux 3.7, this
/// strands panes and birth refuses with a socket-scoped fix. Before 3.7, such
/// a server still places `-c` panes and birth accepts it.
#[test]
fn birth_refuses_a_deleted_cwd_server_only_when_it_strands_panes() {
    require_tmux!();
    let tempdir = TempDir::new().expect("tempdir");
    let runtime_root = tempdir.path().join("runtime");
    let project = tempdir.path().join("project");
    std::fs::create_dir_all(&project).expect("create project");
    let project = project.canonicalize().expect("canonical project");
    let server = TmuxServer::in_runtime_root(&runtime_root);
    let socket = &server.socket;

    // Poison the endpoint: birth a server from a directory, then delete it.
    let launch = tempdir.path().join("launch");
    std::fs::create_dir_all(&launch).expect("create launch dir");
    let launch = launch.canonicalize().expect("canonical launch dir");
    let born = std::process::Command::new("tmux")
        .scrub_session_env()
        .current_dir(&launch)
        .arg("-S")
        .arg(socket)
        .args(["new-session", "-d", "-s", "squatter", "sleep 120"])
        .bounded_status()
        .expect("spawn poisoned server");
    assert!(born.success(), "poisoned server should start");
    std::fs::remove_dir(&launch).expect("unlink launch dir");

    let probe_cwd = probe_pane_cwd(&server, &project);
    let places_panes = std::path::Path::new(&probe_cwd) == project;
    assert!(
        places_panes || probe_cwd == format!("{} (deleted)", launch.display()),
        "probe pane must be in the project or the deleted launch directory, got: {probe_cwd:?}",
    );

    let birth = server.backend.ensure_session(&session_opts(
        "poisoned-room",
        WorkspaceId::from_project_root(&project),
        &project,
        &project,
        Some((120, 40)),
    ));
    if places_panes {
        birth.expect("birth must accept a server that can place panes");
        return;
    }
    let err = birth.expect_err("birth must refuse a server that cannot place panes");

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

fn probe_pane_cwd(server: &TmuxServer, project: &std::path::Path) -> String {
    server.output(&[
        "new-session",
        "-d",
        "-s",
        "cwd-probe",
        "-c",
        project.to_str().expect("utf8 project"),
        "sleep 120",
    ]);
    server.wait_for_pane_command("cwd-probe", "sleep");
    server.pane_current_path("cwd-probe")
}
