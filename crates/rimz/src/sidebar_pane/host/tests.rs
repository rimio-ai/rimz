use std::io::{BufReader, Read};
use std::os::fd::AsFd;

use super::*;
use crate::sidebar_pane::app::Pty;
use crate::sidebar_pane::app::fixtures::{serve_config, workspace};
use crate::wakeup::heartbeat::SidebarHeartbeat;
use crate::{MuxName, SidebarInstanceId, WorkspaceId};

const SETTLE: Duration = Duration::from_secs(10);

struct Room {
    _dir: tempfile::TempDir,
    runtime: RuntimePaths,
    host: Arc<Host>,
}

impl Room {
    fn new() -> Self {
        Self::with_timing(Timing::default())
    }

    fn with_timing(timing: Timing) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let workspace_id: WorkspaceId = workspace();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let template = serve_config(&workspace_id);
        let plane = DataPlane::detached(&template, &runtime);
        let socket_path = runtime.sidebar_host_socket_path(template.mux, &template.session_name);
        let host = Host::new(
            template,
            runtime.clone(),
            crate::diag::DiagSink::disabled(),
            plane,
            socket_path,
            Some("build-a".to_owned()),
            timing,
        );
        Self {
            _dir: dir,
            runtime,
            host,
        }
    }

    /// Serve one connection as the host would after accepting it.
    fn connect(&self) -> UnixStream {
        let (supervisor, served) = UnixStream::pair().unwrap();
        let host = self.host.clone();
        std::thread::spawn(move || host.serve_connection(served));
        supervisor
    }

    fn attach(&self, pty: &Pty) -> Pane {
        self.attach_as(pty, None)
    }

    fn attach_as(&self, pty: &Pty, pane_id: Option<crate::ids::PaneId>) -> Pane {
        let instance_id = SidebarInstanceId::new();
        let stream = self.connect();
        let mut hello = hello(&instance_id);
        if pane_id.is_some() {
            hello.refresh_ms = Some(500);
        }
        hello.pane_id = pane_id;
        attach::send_hello(&stream, &hello, pty.slave().as_fd()).unwrap();
        let mut replies = BufReader::new(stream);
        let reply = attach::read_line::<Reply>(&mut replies).unwrap();
        assert_eq!(
            reply,
            Some(Reply::Accept {
                build: Some("build-a".to_owned())
            })
        );
        Pane {
            instance_id,
            replies,
        }
    }

    fn heartbeat(&self, instance_id: &SidebarInstanceId) -> Option<SidebarHeartbeat> {
        SidebarHeartbeat::read_from(&self.runtime.sidebar_heartbeat_path(instance_id)).ok()
    }

    fn has_runtime_files(&self, instance_id: &SidebarInstanceId) -> bool {
        self.runtime.sidebar_heartbeat_path(instance_id).exists()
            || self.runtime.sidebar_socket_path(instance_id).exists()
    }

    fn wake(&self, instance_id: &SidebarInstanceId, word: &[u8]) {
        std::os::unix::net::UnixDatagram::unbound()
            .unwrap()
            .send_to(word, self.runtime.sidebar_socket_path(instance_id))
            .unwrap();
    }
}

struct Pane {
    instance_id: SidebarInstanceId,
    replies: BufReader<UnixStream>,
}

impl Pane {
    /// Every control line up to the end of the stream.
    fn controls(mut self) -> Vec<Control> {
        self.replies
            .get_ref()
            .set_read_timeout(Some(SETTLE))
            .unwrap();
        let mut controls = Vec::new();
        while let Some(line) = attach::read_line::<ControlLine>(&mut self.replies).unwrap() {
            controls.push(line.control);
        }
        controls
    }
}

fn hello(instance_id: &SidebarInstanceId) -> Hello {
    Hello {
        protocol: PROTOCOL.to_owned(),
        instance_id: instance_id.clone(),
        // No pane id: nothing in the attachment may ask a multiplexer.
        pane_id: None,
        supervisor_build: Some("build-a".to_owned()),
        tick_seconds: None,
        refresh_ms: None,
    }
}

fn eventually(what: &str, mut holds: impl FnMut() -> bool) {
    let deadline = Instant::now() + SETTLE;
    while !holds() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn has_frame_content(bytes: &[u8], rows: u16, cols: u16) -> bool {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(bytes);
    !parser.screen().contents().trim().is_empty()
}

#[test]
fn frame_content_check_rejects_title_and_clear_only_output() {
    for bytes in [
        b"".as_slice(),
        b"\x1b]0;paint-proof\x07",
        b"\x1b[2J\x1b[H",
        b"\x1b]0;paint-proof\x07\x1b[2J\x1b[H\x1b[?25l",
    ] {
        assert!(
            !has_frame_content(bytes, 12, 40),
            "terminal controls are not a painted frame: {bytes:?}"
        );
    }
}

fn painted(pty: &mut Pty) -> Vec<u8> {
    let size = rustix::termios::tcgetwinsize(pty.slave()).unwrap();
    let mut bytes = Vec::new();
    eventually("the pane is painted", || {
        bytes.extend(pty.drain());
        has_frame_content(&bytes, size.ws_row, size.ws_col)
    });
    bytes
}

#[test]
fn an_accepted_pane_is_painted_through_its_fd_and_reports_its_size() {
    let room = Room::new();
    let mut pty = Pty::open(40, 12);
    let pane = room.attach(&pty);

    assert!(!painted(&mut pty).is_empty());
    let mut heartbeat = None;
    eventually("the pane's heartbeat is written", || {
        heartbeat = room.heartbeat(&pane.instance_id);
        heartbeat.is_some()
    });
    let heartbeat = heartbeat.unwrap();
    assert_eq!(
        heartbeat.size.map(|size| (size.cols, size.rows)),
        Some((40, 12)),
        "the size is the pane fd's, not any terminal of the host"
    );
    assert_eq!(
        heartbeat.wakeup_socket,
        room.runtime.sidebar_socket_path(&pane.instance_id)
    );

    pty.resize(40, 30);
    room.wake(&pane.instance_id, b"resize");
    let mut snapshot = crate::sidebar_pane::app::fixtures::agent_snapshot(&workspace());
    snapshot.worktree_groups[0].rows[0].name = "paint-proof".into();
    room.host
        .plane
        .publish_snapshot(&pane.instance_id, snapshot);
    let mut parser = vt100::Parser::new(30, 40, 0);
    eventually("the real rendered card reaches terminal cells", || {
        parser.process(&pty.drain());
        parser.screen().contents().contains("paint-proof")
    });
}

#[test]
fn a_pane_the_mux_has_not_sized_draws_nothing_until_its_resize_word() {
    let room = Room::new();
    let mut pty = Pty::open(0, 0);
    let pane = room.attach(&pty);

    eventually("the pane's heartbeat is written", || {
        room.heartbeat(&pane.instance_id).is_some()
    });
    assert_eq!(room.heartbeat(&pane.instance_id).unwrap().size, None);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(pty.drain(), b"", "an unsized pane is not drawn");

    pty.resize(39, 20);
    room.wake(&pane.instance_id, b"resize");
    assert!(!painted(&mut pty).is_empty());
}

#[test]
fn a_hello_in_another_protocol_is_rejected_by_name() {
    let room = Room::new();
    let pty = Pty::open(40, 12);
    let instance_id = SidebarInstanceId::new();
    let stream = room.connect();
    let hello = Hello {
        protocol: "rimz.sidebar-attach.v0".to_owned(),
        ..hello(&instance_id)
    };
    attach::send_hello(&stream, &hello, pty.slave().as_fd()).unwrap();

    let mut replies = BufReader::new(stream);
    assert_eq!(
        attach::read_line::<Reply>(&mut replies).unwrap(),
        Some(Reply::Reject {
            reason: REJECT_PROTOCOL.to_owned()
        })
    );
    assert_eq!(attach::read_line::<Reply>(&mut replies).unwrap(), None);
    assert!(!room.has_runtime_files(&instance_id));
}

#[test]
fn a_host_leaving_for_another_build_rejects_a_new_pane() {
    let room = Room::new();
    room.host.begin_drain();
    let pty = Pty::open(40, 12);
    let instance_id = SidebarInstanceId::new();
    let stream = room.connect();
    attach::send_hello(&stream, &hello(&instance_id), pty.slave().as_fd()).unwrap();

    assert_eq!(
        attach::read_line::<Reply>(&mut BufReader::new(stream)).unwrap(),
        Some(Reply::Reject {
            reason: REJECT_BUILD.to_owned()
        })
    );
}

#[test]
fn an_admission_in_flight_cannot_insert_after_drain() {
    let room = Room::new();
    let _listener = listen(&room);
    let (checked_tx, checked_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    *lock(&room.host.before_admit) = Some(Box::new(move || {
        checked_tx.send(()).unwrap();
        resume_rx.recv_timeout(SETTLE).unwrap();
    }));
    let pty = Pty::open(40, 12);
    let id = SidebarInstanceId::new();
    let (stream, served) = UnixStream::pair().unwrap();
    let host = room.host.clone();
    host.connections.fetch_add(1, Ordering::SeqCst);
    let admission = std::thread::spawn(move || {
        host.serve_connection(served);
        host.connections.fetch_sub(1, Ordering::SeqCst);
    });
    attach::send_hello(&stream, &hello(&id), pty.slave().as_fd()).unwrap();
    checked_rx.recv_timeout(SETTLE).unwrap();
    room.host.begin_drain();
    let host = room.host.clone();
    let (left_tx, left_rx) = std::sync::mpsc::channel();
    let leaving = std::thread::spawn(move || {
        host.leave();
        left_tx.send(()).unwrap();
    });
    resume_tx.send(()).unwrap();
    let timely = left_rx.recv_timeout(room.host.timing.poll).is_ok();
    // Always release the connection, even on the old code's late accept.
    stream.set_read_timeout(Some(SETTLE)).unwrap();
    let mut replies = BufReader::new(stream);
    let reply = attach::read_line::<Reply>(&mut replies).unwrap();
    drop(replies);
    admission.join().unwrap();
    leaving.join().unwrap();

    assert_eq!(
        reply,
        Some(Reply::Reject {
            reason: REJECT_BUILD.to_owned()
        }),
        "drain must close admission before its locked insert"
    );
    assert!(
        timely,
        "leave must not wait the ten-second drain grace for a late pane"
    );
    assert!(lock(&room.host.attachments).is_empty());
    assert!(room.host.plane.subscriber_ids().is_empty());
    assert!(!room.has_runtime_files(&id));
    assert!(!room.host.socket_path.exists());
}

#[test]
fn attachment_diagnostics_name_the_renderer_and_share_the_limiter() {
    let mut room = Room::new();
    let diag = crate::diag::DiagSink::under(
        room._dir.path().to_path_buf(),
        workspace(),
        "rimz-test",
        None,
    );
    Arc::get_mut(&mut room.host).unwrap().diag = diag.clone();
    let mut pty = Pty::open(40, 12);
    let pane = room.attach(&pty);
    painted(&mut pty);
    let id = pane.instance_id.clone();
    eventually(
        "the attachment subscribes before the fixture publishes",
        || room.host.plane.subscriber_ids().contains(&id),
    );
    let mut snapshot = crate::sidebar_pane::app::fixtures::agent_snapshot(&workspace());
    snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: Vec::new(),
        own_view_is_daemon: false,
    });
    room.host.plane.publish_snapshot(&id, snapshot.clone());
    let notify = crate::wakeup::events::SidebarEventEnvelope::new(
        workspace(),
        Some("rimz-test".to_owned()),
        crate::utils::time::unix_now_ms(),
        crate::wakeup::events::SidebarEvent::Notify {
            title: String::new(),
            body: String::new(),
            panes: Vec::new(),
            recheck_unread: false,
            notification_kind: Some("baseline".to_owned()),
        },
    );
    room.wake(&id, &serde_json::to_vec(&notify).unwrap());
    let trace = crate::StatePaths::class_path(
        room._dir.path(),
        crate::disk::paths::Class::Audit,
        "notify.log.jsonl",
    );
    eventually("the renderer commits the frame-backed baseline", || {
        std::fs::read_to_string(&trace).unwrap_or_default().lines().any(|line| {
            let record: crate::diag::notify::NotifyTraceEnvelope = serde_json::from_str(line).unwrap();
            matches!(record.event, crate::diag::notify::NotifyTraceEvent::BellRing { suppressed: Some(ref reason), .. } if reason == "pane_not_in_view")
        })
    });
    snapshot.panes_produced_at_ms = None;
    room.host.plane.publish_snapshot(&id, snapshot);
    eventually("the attachment emits a gate hold", || {
        diag.log_path().unwrap().exists()
    });
    let records: Vec<crate::diag::record::DiagEnvelope> =
        std::fs::read_to_string(diag.log_path().unwrap())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let hold = records
        .iter()
        .find(|record| {
            matches!(
                record.event,
                crate::diag::record::DiagEvent::GateHold { .. }
            )
        })
        .expect("actual attachment gate-hold record");
    assert_eq!(hold.instance_id, Some(id));
    diag.emit(hold.event.clone());
    assert_eq!(
        std::fs::read_to_string(diag.log_path().unwrap())
            .unwrap()
            .lines()
            .count(),
        records.len(),
        "host and renderer must share the identity limiter"
    );
    drop(pane);
}

#[test]
fn a_connection_closed_before_any_hello_is_dropped_silently() {
    let room = Room::new();
    let (probe, served) = UnixStream::pair().unwrap();
    drop(probe);

    room.host.serve_connection(served);

    assert!(lock(&room.host.attachments).is_empty());
    assert!(room.host.plane.subscriber_ids().is_empty());
}

#[test]
fn a_supervisor_that_goes_away_takes_its_heartbeat_and_socket_with_it() {
    let room = Room::new();
    let mut pty = Pty::open(40, 12);
    let pane = room.attach(&pty);
    painted(&mut pty);
    eventually("the pane's heartbeat is written", || {
        room.heartbeat(&pane.instance_id).is_some()
    });

    assert!(
        room.host.plane.is_producer(),
        "the host stands in the election as its one pane"
    );

    let instance_id = pane.instance_id.clone();
    drop(pane);

    eventually("the pane's runtime files are removed", || {
        !room.has_runtime_files(&instance_id)
    });
    eventually("the pane left the host", || {
        lock(&room.host.attachments).is_empty() && room.host.plane.subscriber_ids().is_empty()
    });
    assert!(
        !room.host.plane.is_producer(),
        "a host with no pane has no heartbeat to stand on"
    );
}

#[test]
fn a_closed_pane_detaches_on_the_failed_write_without_a_control() {
    let room = Room::new();
    let mut pty = Pty::open(40, 12);
    let pane = room.attach(&pty);
    painted(&mut pty);
    let instance_id = pane.instance_id.clone();

    // The mux closing the pane: its side of the pty goes, and the host's
    // next write to the fd it was handed fails.
    drop(pty);
    room.wake(&instance_id, b"resize");

    assert_eq!(pane.controls(), Vec::new(), "a bare end of stream");
    assert!(!room.has_runtime_files(&instance_id));
}

#[test]
fn every_pane_of_a_host_shares_its_one_data_plane() {
    let room = Room::new();
    let mut ptys = [Pty::open(40, 12), Pty::open(31, 9), Pty::open(50, 20)];
    let panes: Vec<Pane> = ptys.iter().map(|pty| room.attach(pty)).collect();
    for pty in &mut ptys {
        painted(pty);
    }

    let mut ids: Vec<String> = panes
        .iter()
        .map(|pane| pane.instance_id.as_str().to_owned())
        .collect();
    ids.sort();
    eventually("every pane subscribed to the plane", || {
        let subscribed: Vec<String> = room
            .host
            .plane
            .subscriber_ids()
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        subscribed == ids
    });
    for (pane, size) in panes.iter().zip([(40, 12), (31, 9), (50, 20)]) {
        eventually("each pane reports its own size", || {
            room.heartbeat(&pane.instance_id)
                .and_then(|heartbeat| heartbeat.size)
                .map(|size| (size.cols, size.rows))
                == Some(size)
        });
    }
}

#[test]
fn hidden_attachment_keeps_lifecycle_and_paints_on_viewed_fold() {
    hidden_reveal(crate::store::snapshot::SidebarPresence::Active, false);
}

#[test]
fn detached_attachment_keeps_lifecycle_and_paints_on_resize() {
    hidden_reveal(crate::store::snapshot::SidebarPresence::Detached, true);
}

fn hidden_reveal(presence: crate::store::snapshot::SidebarPresence, reveal_by_resize: bool) {
    let room = Room::new();
    let visible_id = crate::ids::PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let hidden_id = crate::ids::PaneId::from_parts(MuxName::Zellij, "terminal_2");
    let mut visible = Pty::open(40, 20);
    let mut hidden = Pty::open(40, 20);
    let visible_pane = room.attach_as(&visible, Some(visible_id.clone()));
    let hidden_pane = room.attach_as(&hidden, Some(hidden_id.clone()));
    painted(&mut visible);
    painted(&mut hidden);

    let mut snapshot = crate::sidebar_pane::app::fixtures::agent_snapshot(&workspace());
    snapshot.theme.display.refresh_ms = 500;
    snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
        sibling_count: 1,
        working_pane_ids: Vec::new(),
        own_view_is_daemon: false,
    });
    snapshot.presence = Some(presence);
    snapshot.viewed_panes = vec![visible_id];
    let publish = |snapshot: &crate::store::snapshot::SidebarSnapshot| {
        for pane in [&visible_pane, &hidden_pane] {
            room.host
                .plane
                .publish_snapshot(&pane.instance_id, snapshot.clone());
        }
    };
    publish(&snapshot);
    std::thread::sleep(Duration::from_millis(600));
    visible.drain();
    hidden.drain();
    let beat = room.heartbeat(&hidden_pane.instance_id).unwrap().last_seen;

    for (name, status) in [
        ("hidden-one", crate::agents::AgentStatus::Running),
        ("hidden-two", crate::agents::AgentStatus::Waiting),
        ("hidden-latest", crate::agents::AgentStatus::Idle),
    ] {
        snapshot.worktree_groups[0].rows[0].name = name.to_owned();
        snapshot.worktree_groups[0].rows[0]
            .as_agent_mut()
            .unwrap()
            .status = status;
        publish(&snapshot);
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            !visible.drain().is_empty(),
            "the visible attachment paints {name}"
        );
        assert_eq!(
            hidden.drain(),
            b"",
            "hidden fold {name} emits no frame bytes"
        );
    }
    eventually("the hidden heartbeat renews", || {
        room.heartbeat(&hidden_pane.instance_id).unwrap().last_seen > beat
    });
    assert_eq!(hidden.drain(), b"", "heartbeat maintenance does not paint");
    for word in [b"press:-:char:?".as_slice(), b"press:-:esc".as_slice()] {
        room.wake(&hidden_pane.instance_id, word);
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(hidden.drain(), b"", "hidden input drains without painting");
    }

    let revealed = Instant::now();
    if reveal_by_resize {
        room.wake(&hidden_pane.instance_id, b"resize");
    } else {
        snapshot.viewed_panes.push(hidden_id.clone());
        publish(&snapshot);
    }
    let mut bytes = Vec::new();
    while !String::from_utf8_lossy(&bytes).contains("hidden-latest") {
        assert!(
            revealed.elapsed() < Duration::from_millis(500),
            "reveal paints within one frame"
        );
        bytes.extend(hidden.drain());
        std::thread::sleep(Duration::from_millis(2));
    }

    // Expire the resize's optimistic watch and hide both again before controls.
    std::thread::sleep(Duration::from_secs(3));
    snapshot.viewed_panes.clear();
    publish(&snapshot);
    std::thread::sleep(Duration::from_millis(600));
    hidden.drain();
    visible.drain();
    snapshot.own_view.as_mut().unwrap().sibling_count = 0;
    room.host
        .plane
        .publish_snapshot(&hidden_pane.instance_id, snapshot);
    let instance_id = hidden_pane.instance_id.clone();
    assert_eq!(hidden_pane.controls(), [Control::SelfClose]);
    assert!(!room.has_runtime_files(&instance_id));
    assert_eq!(
        hidden.drain(),
        b"\x1b[?25h",
        "hidden self-close only clears pixels and restores the cursor"
    );
    let instance_id = visible_pane.instance_id.clone();
    room.host.leave();
    assert_eq!(visible_pane.controls(), [Control::Reload]);
    assert!(!room.has_runtime_files(&instance_id));
    assert_eq!(
        visible.drain(),
        b"\x1b[?25h",
        "hidden reload only clears pixels and restores the cursor"
    );
}

fn brisk() -> Timing {
    Timing {
        unattached_grace: Duration::from_millis(200),
        drain_grace: Duration::from_secs(5),
        poll: Duration::from_millis(20),
    }
}

fn listen(room: &Room) -> UnixListener {
    bind_listener(&room.host.socket_path).unwrap()
}

#[test]
fn a_host_with_no_pane_for_the_grace_leaves_and_takes_its_socket() {
    let room = Room::with_timing(brisk());
    let listener = listen(&room);
    let started = Instant::now();

    room.host.serve(listener, || None);

    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(!room.host.socket_path.exists());
}

#[test]
fn a_host_outlives_the_grace_while_a_pane_is_attached() {
    let room = Room::with_timing(brisk());
    let listener = listen(&room);
    let host = room.host.clone();
    let serving = std::thread::spawn(move || host.serve(listener, || None));

    let mut pty = Pty::open(40, 12);
    let instance_id = SidebarInstanceId::new();
    let stream = UnixStream::connect(&room.host.socket_path).unwrap();
    attach::send_hello(&stream, &hello(&instance_id), pty.slave().as_fd()).unwrap();
    painted(&mut pty);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!serving.is_finished(), "an attached pane keeps the host");

    drop(stream);
    serving.join().unwrap();
}

#[test]
fn a_recorded_build_other_than_the_hosts_reloads_every_pane_then_ends_it() {
    let room = Room::with_timing(brisk());
    let listener = listen(&room);
    let host = room.host.clone();
    let recorded = Arc::new(Mutex::new(Some("build-a".to_owned())));
    let seen = recorded.clone();
    let serving = std::thread::spawn(move || host.serve(listener, move || lock(&seen).clone()));

    let mut ptys = [Pty::open(40, 12), Pty::open(31, 9)];
    let mut panes = Vec::new();
    for pty in &mut ptys {
        let instance_id = SidebarInstanceId::new();
        let stream = UnixStream::connect(&room.host.socket_path).unwrap();
        attach::send_hello(&stream, &hello(&instance_id), pty.slave().as_fd()).unwrap();
        let mut replies = BufReader::new(stream);
        assert!(matches!(
            attach::read_line::<Reply>(&mut replies).unwrap(),
            Some(Reply::Accept { .. })
        ));
        painted(pty);
        panes.push(Pane {
            instance_id,
            replies,
        });
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !serving.is_finished(),
        "its own build recorded: nothing to do"
    );

    *lock(&recorded) = Some("build-b".to_owned());

    for pane in panes {
        let instance_id = pane.instance_id.clone();
        assert_eq!(pane.controls(), [Control::Reload]);
        assert!(!room.has_runtime_files(&instance_id));
    }
    serving.join().unwrap();
    assert!(
        !room.host.socket_path.exists(),
        "the next host binds a path this one no longer answers on"
    );
}

#[test]
fn a_silent_connection_reads_nothing_back() {
    let room = Room::new();
    let mut probe = room.connect();
    probe
        .shutdown(std::net::Shutdown::Write)
        .expect("half-close");
    let mut reply = Vec::new();
    probe.read_to_end(&mut reply).unwrap();
    assert_eq!(reply, b"");
}

#[test]
fn a_pane_that_stopped_reading_does_not_hold_the_host_leaving() {
    let room = Room::new();
    let mut config = room.host.template.clone();
    config.instance_id = SidebarInstanceId::new();
    let attachment = Attachment::open(
        config.clone(),
        &room.runtime,
        crate::diag::DiagSink::disabled(),
    )
    .unwrap();
    crate::sidebar_pane::app::fixtures::fill_inbox(attachment.socket_path());
    lock(&room.host.attachments).insert(
        config.instance_id.as_str().to_owned(),
        Live {
            close: attachment.close_handle(),
            leaving: Arc::new(AtomicBool::new(false)),
        },
    );

    let host = room.host.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        host.leave();
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(SETTLE)
        .expect("leaving returns with a pane's inbox full");
    drop(attachment);
}
