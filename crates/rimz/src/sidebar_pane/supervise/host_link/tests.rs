use std::os::fd::AsFd;
use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::sidebar_pane::app::fixtures::{serve_config, workspace};
use crate::sidebar_pane::attach::REJECT_PROTOCOL;
use crate::workspace::record::{self, WorkspaceRecord};

const BRIEF: Duration = Duration::from_millis(150);
const SETTLE: Duration = Duration::from_secs(10);

struct Room {
    _dir: tempfile::TempDir,
    runtime: RuntimePaths,
    config: ServeConfig,
}

impl Room {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(workspace(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        Self {
            _dir: dir,
            runtime,
            config: serve_config(&workspace()),
        }
    }

    fn listen(&self) -> UnixListener {
        let socket = self
            .runtime
            .sidebar_host_socket_path(self.config.mux, &self.config.session_name);
        UnixListener::bind(socket).unwrap()
    }

    fn hello(&self) -> Hello {
        hello_for(&self.config, Some("build-a"))
    }

    fn recorded() -> (Self, crate::StatePaths, std::path::PathBuf) {
        let mut room = Self::new();
        let root = room._dir.path().join("project");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let alias = room._dir.path().join("project-alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let id = crate::ids::WorkspaceId::from_project_root(&root);
        room.config = serve_config(&id);
        room.runtime = RuntimePaths::under(id.clone(), room._dir.path()).unwrap();
        room.runtime.ensure_dirs().unwrap();
        let state = crate::StatePaths::under(id.clone(), room._dir.path()).unwrap();
        record::write(
            &state,
            &WorkspaceRecord {
                layout: crate::disk::paths::WORKSPACE_LAYOUT,
                workspace_id: id,
                project_root: alias,
                worktree_root: None,
                session_name: room.config.session_name.clone(),
                root_class: crate::workspace::RootClass::Directory,
                rimz_bin: None,
                rimz_build: None,
                pins: Default::default(),
                updated_at: jiff::Timestamp::now(),
            },
        )
        .unwrap();
        (room, state, root)
    }
}

/// A host that reads one hello and answers it with `reply`, then says each
/// of `controls` and closes.
fn answer(listener: UnixListener, reply: Reply, controls: Vec<Control>) {
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let (hello, _output) = attach::recv_hello(&stream).unwrap().unwrap();
        assert_eq!(hello.protocol, PROTOCOL);
        attach::write_line(&stream, &reply).unwrap();
        for control in controls {
            attach::write_line(&stream, &ControlLine { control }).unwrap();
        }
    });
}

fn accept() -> Reply {
    Reply::Accept {
        build: Some("build-a".to_owned()),
    }
}

fn output() -> std::fs::File {
    std::fs::File::create("/dev/null").unwrap()
}

#[test]
fn a_pane_no_host_answers_finishes_once_the_wait_is_over() {
    let room = Room::new();
    let started = AtomicUsize::new(0);
    let began = Instant::now();

    let stream = connect(&room.config, &room.runtime, BRIEF, || {
        started.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });

    assert!(stream.unwrap().is_none());
    assert_eq!(
        started.load(Ordering::SeqCst),
        1,
        "it asked for a host once"
    );
    assert!(began.elapsed() >= BRIEF);
    assert!(began.elapsed() < SETTLE, "the wait bounds the first frame");
}

#[test]
fn a_host_that_cannot_start_returns_its_error_at_once() {
    let room = Room::new();
    let began = Instant::now();

    let stream = connect(&room.config, &room.runtime, SETTLE, || {
        Err(io::ErrorKind::NotFound.into())
    });

    assert_eq!(stream.unwrap_err().kind(), io::ErrorKind::NotFound);
    assert!(began.elapsed() < SETTLE);
}

#[test]
fn a_rejected_hello_preserves_the_hosts_reason() {
    let room = Room::new();
    answer(
        room.listen(),
        Reply::Reject {
            reason: REJECT_PROTOCOL.to_owned(),
        },
        Vec::new(),
    );
    let stream = connect(&room.config, &room.runtime, BRIEF, || {
        panic!("a host is already listening")
    })
    .unwrap()
    .unwrap();

    assert!(
        matches!(HostLink::open(stream, &room.hello(), output().as_fd(), SETTLE), Err(AttachFailure::Rejected(reason)) if reason == REJECT_PROTOCOL)
    );
}

#[test]
fn an_accepted_pane_hears_the_hosts_controls_then_its_end() {
    let room = Room::new();
    answer(room.listen(), accept(), vec![Control::SelfClose]);
    let stream = connect(&room.config, &room.runtime, BRIEF, || Ok(()))
        .unwrap()
        .unwrap();

    let mut link = HostLink::open(stream, &room.hello(), output().as_fd(), SETTLE).unwrap();

    assert_eq!(link.build.as_deref(), Some("build-a"));
    assert_eq!(link.poll(SETTLE), HostEvent::Control(Control::SelfClose));
    assert_eq!(link.poll(SETTLE), HostEvent::Lost);
}

#[test]
fn a_quiet_host_is_neither_a_control_nor_lost() {
    let room = Room::new();
    let listener = room.listen();
    let (held, release) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _hello = attach::recv_hello(&stream).unwrap().unwrap();
        attach::write_line(&stream, &accept()).unwrap();
        let _ = release.recv();
    });
    let stream = connect(&room.config, &room.runtime, BRIEF, || Ok(()))
        .unwrap()
        .unwrap();
    let mut link = HostLink::open(stream, &room.hello(), output().as_fd(), SETTLE).unwrap();

    assert_eq!(link.poll(Duration::from_millis(20)), HostEvent::Quiet);
    drop(held);
    assert_eq!(link.poll(SETTLE), HostEvent::Lost);
}

#[test]
fn panes_that_lose_their_host_together_start_one_replacement() {
    let room = Arc::new(Room::new());
    let started = Arc::new(AtomicUsize::new(0));
    let panes: Vec<_> = (0..4)
        .map(|_| {
            let room = room.clone();
            let started = started.clone();
            std::thread::spawn(move || {
                connect(&room.config, &room.runtime, SETTLE, || {
                    started.fetch_add(1, Ordering::SeqCst);
                    // The replacement host: it binds a little after its
                    // spawn returns, as a real one does.
                    let listener_room = room.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(100));
                        let listener = listener_room.listen();
                        let held: Vec<_> = listener.incoming().take(4).collect();
                        std::thread::sleep(SETTLE);
                        drop(held);
                    });
                    Ok(())
                })
                .unwrap()
                .is_some()
            })
        })
        .collect();

    for pane in panes {
        assert!(pane.join().unwrap(), "every pane reaches the replacement");
    }
    assert_eq!(started.load(Ordering::SeqCst), 1);
}

#[test]
fn the_host_argv_names_the_session_and_reads_as_no_other_sidebar_process() {
    let room = Room::new();

    let argv: Vec<String> = host_args(&room.config)
        .into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect();

    assert_eq!(
        argv,
        [
            "sidebar",
            "host",
            "--mux",
            "zellij",
            "--workspace-id",
            workspace().as_str(),
            "--session-name",
            "rimz-test"
        ]
    );
}

#[test]
fn a_host_that_fails_to_start_says_why_in_its_log() {
    use std::os::unix::fs::PermissionsExt;

    let (room, state, _) = Room::recorded();
    let exe = room._dir.path().join("failing-host");
    std::fs::write(&exe, "#!/bin/sh\necho \"host refused: $*\" >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let log = room._dir.path().join("state/log/sidebar-host.log");

    spawn_host(&exe, &room.config, &room.runtime, &state, &log).unwrap();

    let deadline = Instant::now() + SETTLE;
    loop {
        let written = std::fs::read_to_string(&log).unwrap_or_default();
        if written.contains("host refused: sidebar host") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the host's stderr never reached {}: {written:?}",
            log.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_host_environment_pins_its_room_and_keeps_the_session_not_the_pane() {
    let (room, state, root) = Room::recorded();
    let command =
        host_command(Path::new("/bin/true"), &room.config, &room.runtime, &state).unwrap();
    let env: std::collections::BTreeMap<_, _> = command.get_envs().collect();
    for (name, value) in crate::workspace::pin_env(&room.config.workspace_id, &root) {
        assert_eq!(
            env.get(std::ffi::OsStr::new(&name)),
            Some(&Some(std::ffi::OsStr::new(&value))),
            "{name}"
        );
    }
    for name in [
        "TMUX_PANE",
        "ZELLIJ_PANE_ID",
        "RIMZ_CHANNEL",
        "RIMZ_WORKTREE_PATH",
        "RIMZ_SIDEBAR_INSTANCE_ID",
    ] {
        assert_eq!(env.get(std::ffi::OsStr::new(name)), Some(&None), "{name}");
    }
    for name in ["TMUX", "ZELLIJ", "ZELLIJ_SESSION_NAME"] {
        assert!(
            !env.contains_key(std::ffi::OsStr::new(name)),
            "inherit {name}"
        );
    }
    assert_eq!(env.len(), 7, "only the pin and pane removals are set");
}

/// The log holds one line: when the host was refused, then why.
fn assert_refusal_logged(log: &Path, error: &str) {
    let logged = std::fs::read_to_string(log).unwrap();
    let (at, reason) = logged.split_once(' ').unwrap();
    at.parse::<jiff::Timestamp>().unwrap();
    assert_eq!(reason, format!("{error}\n"));
}

#[test]
fn a_host_without_a_workspace_record_is_not_spawned() {
    let room = Room::new();
    let state =
        crate::StatePaths::under(room.config.workspace_id.clone(), room._dir.path()).unwrap();
    let log = room._dir.path().join("host.log");
    let error = spawn_host(
        Path::new("/bin/true"),
        &room.config,
        &room.runtime,
        &state,
        &log,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(room.config.workspace_id.as_str()), "{error}");
    assert!(error.contains("cannot access"), "{error}");
    assert_refusal_logged(&log, &error);
}

#[test]
fn a_host_with_a_workspace_record_for_another_root_is_not_spawned() {
    let (room, state, _) = Room::recorded();
    let mut record = record::read(&state.workspace_record).unwrap();
    record.project_root = room._dir.path().to_path_buf();
    record::write(&state, &record).unwrap();
    let log = room._dir.path().join("host.log");
    let error = spawn_host(
        Path::new("/bin/true"),
        &room.config,
        &room.runtime,
        &state,
        &log,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains(room.config.workspace_id.as_str()), "{error}");
    assert!(error.contains("does not verify"), "{error}");
    assert_refusal_logged(&log, &error);
}
