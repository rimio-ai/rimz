use std::io::{BufReader, Read, Write};
use std::os::fd::AsFd;
use std::sync::mpsc::TryRecvError::Empty;

use super::*;
use crate::sidebar_pane::app::Pty;
use crate::sidebar_pane::app::fixtures::{serve_config, workspace};
use crate::wakeup::heartbeat::SidebarHeartbeat;
use crate::{MuxName, SidebarInstanceId, WorkspaceId};

const SETTLE: Duration = Duration::from_secs(10);

mod restart;

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
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let workspace_id: WorkspaceId = workspace();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        let room = serve_config(&workspace_id).room;
        let plane = DataPlane::detached();
        let socket_path = runtime.sidebar_host_socket_path(room.mux, &room.session_name);
        let host = Host::new(
            room,
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

    fn replace_host(&mut self) {
        self.host.leave();
        eventually("old attachments released", || {
            lock(&self.host.attachments).is_empty()
        });
        self.host = Host::new(
            self.host.room.clone(),
            self.runtime.clone(),
            crate::diag::DiagSink::disabled(),
            DataPlane::detached(),
            self.host.socket_path.clone(),
            Some("build-a".to_owned()),
            Timing::default(),
        );
    }

    fn attach(&self, pty: &Pty) -> Pane {
        self.attach_as(pty, None)
    }

    fn attach_as(&self, pty: &Pty, pane_id: Option<crate::ids::PaneId>) -> Pane {
        let instance_id = SidebarInstanceId::new();
        let stream = self.connect();
        let mut hello = hello(&instance_id);
        if let Some(pane_id) = pane_id {
            hello.refresh_ms = Some(500);
            hello.pane_id = pane_id;
        }
        self.attach_hello(pty, hello, stream)
    }

    fn attach_hello(&self, pty: &Pty, hello: Hello, stream: UnixStream) -> Pane {
        let instance_id = hello.instance_id.clone();
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
    }

    fn wake(&self, pane: &Pane, word: &[u8]) {
        if word.first() == Some(&b'{') {
            let path = self
                .runtime
                .sidebar_host_wake_socket_path(self.host.room.mux, &self.host.room.session_name);
            if !path.exists() {
                self.host.receive_room(bind_wake_socket(&path).unwrap());
            }
            bind_wake_socket(&self._dir.path().join("sender.sock"))
                .unwrap()
                .send_to(word, path)
                .unwrap();
        } else {
            let mut stream = pane.replies.get_ref();
            stream.write_all(word).unwrap();
            stream.write_all(b"\n").unwrap();
        }
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
        // A synthetic, non-addressable pane: nothing in the attachment may ask a multiplexer.
        pane_id: crate::ids::PaneId::from_parts(
            MuxName::Zellij,
            format!("sidebar_{}", instance_id.as_str()),
        ),
        baseline_pane: None,
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
fn a_replacement_host_seats_only_the_carried_live_baseline() {
    use crate::ids::PaneId;
    use crate::sidebar_pane::app::fixtures::{agent_snapshot, remember_fold};

    let mut room = Room::new();
    let own = PaneId::from_parts(MuxName::Tmux, "%7");
    let selected = PaneId::from_parts(MuxName::Tmux, "%6");
    let mut snapshot = agent_snapshot(&workspace());
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .name = "selected-card".into();
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .pane
    .as_mut()
    .unwrap()
    .pane_id = selected.clone();
    let mut other = snapshot.worktree_groups[0].rows[0].clone();
    other.id = "agent-2".into();
    other.name = "other-card".into();
    other.pane.as_mut().unwrap().pane_id = PaneId::from_parts(MuxName::Tmux, "%5");
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)
        .push(other);
    snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
        sibling_count: 2,
        working_pane_ids: snapshot
            .rows()
            .map(|row| row.pane.as_ref().unwrap().pane_id.clone())
            .collect(),
        own_view_is_daemon: false,
    });
    snapshot.focused_pane = Some(selected.clone());
    snapshot.viewed_panes = vec![own.clone()];
    let mut sidebar = snapshot.rows().next().unwrap().pane.clone().unwrap();
    sidebar.pane_id = own.clone();
    sidebar.command = Some("rimz-sidebar".into());
    let mut frame = crate::sidebar::frame::assemble_frame(
        snapshot
            .rows()
            .filter_map(|row| row.pane.clone())
            .chain([sidebar])
            .collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    frame.client_views = vec![crate::pane::ClientPaneView {
        client_id: crate::ids::MuxClientId::Tmux("client-1".into()),
        pane_id: selected.clone(),
    }];
    frame.client_view_fresh = true;
    let mut pty = Pty::open(54, 30);
    let mut pane = room.attach_as(&pty, Some(own.clone()));
    painted(&mut pty);
    remember_fold(&room.host.plane, snapshot.clone(), frame.clone());
    room.host
        .plane
        .publish_snapshot(&pane.instance_id, snapshot.clone());
    let mut parser = vt100::Parser::new(30, 54, 0);
    eventually("focused card selected", || {
        parser.process(&pty.drain());
        parser
            .screen()
            .contents()
            .lines()
            .any(|line| line.contains("selected-card") && line.contains('▌'))
    });
    pane.replies
        .get_ref()
        .set_read_timeout(Some(SETTLE))
        .unwrap();
    let report = attach::read_line::<serde_json::Value>(&mut pane.replies);
    assert!(
        report.is_ok(),
        "a baseline change must reach the supervisor: {report:?}"
    );
    let report = report.unwrap();
    assert_eq!(
        report,
        Some(serde_json::json!({"control": {"baseline": "tmux:%6"}}))
    );
    let instance = pane.instance_id.clone();
    room.replace_host();
    drop(pane);
    snapshot.focused_pane = Some(own.clone());
    frame.client_views[0].pane_id = own.clone();

    for baseline in [Some("tmux:%6"), None, Some("tmux:%9")] {
        let mut greeting = hello(&instance);
        greeting.pane_id = own.clone();
        greeting.baseline_pane = baseline.map(|pane| PaneId::parse(pane).unwrap());
        let mut pty = Pty::open(54, 30);
        let mut pane = room.attach_hello(&pty, greeting, room.connect());
        painted(&mut pty);
        remember_fold(&room.host.plane, snapshot.clone(), frame.clone());
        room.host
            .plane
            .publish_snapshot(&instance, snapshot.clone());
        let mut parser = vt100::Parser::new(30, 54, 0);
        eventually("successor cards painted", || {
            parser.process(&pty.drain());
            parser.screen().contents().contains("selected-card")
        });
        let screen = parser.screen().contents();
        assert_eq!(
            screen
                .lines()
                .any(|line| line.contains("selected-card") && line.contains('▌')),
            baseline == Some("tmux:%6"),
            "{baseline:?}:\n{screen}"
        );
        assert!(
            !screen
                .lines()
                .any(|line| line.contains("other-card") && line.contains('▌')),
            "{screen}"
        );
        if baseline == Some("tmux:%9") {
            pane.replies
                .get_ref()
                .set_read_timeout(Some(SETTLE))
                .unwrap();
            assert_eq!(
                attach::read_line::<serde_json::Value>(&mut pane.replies).unwrap(),
                Some(serde_json::json!({"control": {"baseline": null}})),
                "a departed baseline is forgotten by the supervisor too"
            );
        }
        drop(pane);
        eventually("successor attachment released", || {
            lock(&room.host.attachments).is_empty()
        });
        room.replace_host();
    }
}

#[test]
fn a_warm_plane_populates_the_first_nonblank_screen() {
    let room = Room::new();
    let mut seed = crate::sidebar_pane::app::fixtures::agent_snapshot(&workspace());
    std::sync::Arc::make_mut(&mut std::sync::Arc::make_mut(&mut seed.worktree_groups)[0].rows)[0]
        .name = "seed-card".into();
    let frame = crate::sidebar::frame::assemble_frame(
        seed.rows().filter_map(|row| row.pane.clone()).collect(),
        crate::utils::time::unix_now_ms(),
        "rimz-test",
    );
    crate::sidebar_pane::app::fixtures::remember_fold(&room.host.plane, seed, frame);

    let mut pty = Pty::open(40, 30);
    let _pane = room.attach(&pty);
    let mut parser = vt100::Parser::new(30, 40, 0);
    eventually("the pane's first nonblank screen", || {
        parser.process(&pty.drain());
        !parser.screen().contents().trim().is_empty()
    });
    let first = parser.screen().contents();
    assert!(
        first.contains("seed-card"),
        "the first screen must contain the remembered card, not just chrome:\n{first}"
    );
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
        room.runtime
            .sidebar_host_wake_socket_path(room.host.room.mux, &room.host.room.session_name)
    );

    pty.resize(40, 30);
    room.wake(&pane, b"resize");
    let mut snapshot = crate::sidebar_pane::app::fixtures::agent_snapshot(&workspace());
    std::sync::Arc::make_mut(
        &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
    )[0]
    .name = "paint-proof".into();
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
    room.wake(&pane, b"resize");
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
    let own = crate::PaneId::from_parts(MuxName::Zellij, "terminal_sidebar");
    let pane = room.attach_as(&pty, Some(own.clone()));
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
    snapshot.viewed_panes = vec![own];
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
    room.wake(&pane, &serde_json::to_vec(&notify).unwrap());
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
    snapshot.display_name = "missing pane cache".into();
    room.host.plane.publish_snapshot(&id, snapshot);
    // Wait on the gate-hold record itself: the log file exists as soon as any
    // diagnostic lands, which under load is before the hold is appended.
    eventually("the attachment emits a gate hold", || {
        std::fs::read_to_string(diag.log_path().unwrap())
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<crate::diag::record::DiagEnvelope>(line).ok())
            .any(|record| {
                matches!(
                    record.event,
                    crate::diag::record::DiagEvent::GateHold { .. }
                )
            })
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

    assert_eq!(
        room.host.plane.subscriber_ids(),
        std::slice::from_ref(&pane.instance_id)
    );

    let instance_id = pane.instance_id.clone();
    drop(pane);

    eventually("the pane's runtime files are removed", || {
        !room.has_runtime_files(&instance_id)
    });
    eventually("the pane left the host", || {
        lock(&room.host.attachments).is_empty() && room.host.plane.subscriber_ids().is_empty()
    });
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
    room.wake(&pane, b"resize");

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
fn detached_attachment_keeps_lifecycle_and_hidden_resize_waits_for_visibility() {
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
    snapshot.viewed_panes = vec![visible_id.clone()];
    let publish = |snapshot: &crate::store::snapshot::SidebarSnapshot| {
        use crate::sidebar_pane::app::fixtures::pane;
        let mut panes: Vec<_> = snapshot.rows().filter_map(|row| row.pane.clone()).collect();
        for (id, tab) in [(&visible_id, "tab_visible"), (&hidden_id, "tab_hidden")] {
            let mut sidebar = pane(id.raw(), tab, false);
            sidebar.command = Some("rimz-sidebar".into());
            panes.push(sidebar);
            if id == &visible_id || snapshot.own_view.as_ref().unwrap().sibling_count != 0 {
                let mut sibling = pane(&format!("{}_peer", id.raw()), tab, false);
                sibling.command = Some("rimz-sidebar".into());
                panes.push(sibling);
            }
        }
        let mut frame = crate::sidebar::frame::assemble_frame(panes, 1, "rimz-test");
        frame.presence = Some(crate::store::snapshot::PresenceSample {
            human_clients: usize::from(
                presence != crate::store::snapshot::SidebarPresence::Detached,
            ),
            last_input_ms: None,
            sampled_at_ms: 1,
        });
        crate::sidebar_pane::app::fixtures::publish_fold(&room.host.plane, snapshot.clone(), frame);
    };
    publish(&snapshot);
    let mut parser = vt100::Parser::new(20, 40, 0);
    eventually("the viewed pane paints the initial viewport", || {
        parser.process(&visible.drain());
        parser
            .screen()
            .contents()
            .contains(&snapshot.worktree_groups[0].rows[0].name)
    });
    visible.drain();
    hidden.drain();
    let beat = room.heartbeat(&hidden_pane.instance_id).unwrap().last_seen;

    for (name, status) in [
        ("hidden-one", crate::agents::AgentStatus::Running),
        ("hidden-two", crate::agents::AgentStatus::Waiting),
        ("hidden-latest", crate::agents::AgentStatus::Idle),
    ] {
        std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0]
        .name = name.to_owned();
        std::sync::Arc::make_mut(
            &mut std::sync::Arc::make_mut(&mut snapshot.worktree_groups)[0].rows,
        )[0]
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
    if reveal_by_resize {
        room.wake(&hidden_pane, b"resize");
        std::thread::sleep(Duration::from_millis(600));
        assert!(
            hidden.drain().is_empty(),
            "uncorroborated hidden resize routes no view"
        );
    }
    let revealed = Instant::now();
    snapshot.viewed_panes.push(hidden_id.clone());
    publish(&snapshot);
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
    publish(&snapshot);
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
    let wake = bind_wake_socket(
        &room
            .runtime
            .sidebar_host_wake_socket_path(room.host.room.mux, &room.host.room.session_name),
    )
    .unwrap();
    let started = Instant::now();

    room.host.serve(listener, wake, || None);

    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(!room.host.socket_path.exists());
}

#[test]
fn a_host_outlives_the_grace_while_a_pane_is_attached() {
    let room = Room::with_timing(brisk());
    let listener = listen(&room);
    let wake = bind_wake_socket(
        &room
            .runtime
            .sidebar_host_wake_socket_path(room.host.room.mux, &room.host.room.session_name),
    )
    .unwrap();
    let host = room.host.clone();
    let serving = std::thread::spawn(move || host.serve(listener, wake, || None));

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
    reloads_every_pane_then_ends(false);
}

#[test]
fn a_room_reload_word_reloads_every_pane_then_ends_it() {
    reloads_every_pane_then_ends(true);
}

fn reloads_every_pane_then_ends(via_socket: bool) {
    let mut room = Room::with_timing(brisk());
    if via_socket {
        Arc::get_mut(&mut room.host).unwrap().reload_action =
            |_| reload::ReloadAction::Reexec(PathBuf::from("next-build"));
    }
    let listener = listen(&room);
    let wake = bind_wake_socket(
        &room
            .runtime
            .sidebar_host_wake_socket_path(room.host.room.mux, &room.host.room.session_name),
    )
    .unwrap();
    let host = room.host.clone();
    let recorded = Arc::new(Mutex::new(Some("build-a".to_owned())));
    let seen = recorded.clone();
    let serving =
        std::thread::spawn(move || host.serve(listener, wake, move || lock(&seen).clone()));

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

    if via_socket {
        assert_eq!(crate::wakeup::reload_all(&room.runtime).unwrap(), 2);
    } else {
        *lock(&recorded) = Some("build-b".to_owned());
    }

    for pane in panes {
        let instance_id = pane.instance_id.clone();
        assert_eq!(pane.controls(), [Control::Reload]);
        assert!(!room.has_runtime_files(&instance_id));
    }
    serving.join().unwrap();
    assert!(
        !room
            .runtime
            .sidebar_host_wake_socket_path(room.host.room.mux, &room.host.room.session_name)
            .exists()
    );
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
    let config = room.host.config_for(&hello(&SidebarInstanceId::new()));
    let attachment = Attachment::open(
        config.clone(),
        &room.runtime,
        crate::diag::DiagSink::disabled(),
    );
    let close = attachment.close_handle();
    while close.wake.try_send(Wakeup::Tick).is_ok() {}
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

#[test]
fn stream_input_ignores_unknown_lines_and_delivers_keys() {
    use std::io::Write;
    let room = Room::new();
    let (attachment, live) = room.host.admit(&hello(&SidebarInstanceId::new())).unwrap();
    let (mut supervisor, host) = UnixStream::pair().unwrap();
    read_input(&host, live.close);
    supervisor
        .write_all(b"unknown\nsnapshot\nreload\npress:-:char:q\nresize\n")
        .unwrap();
    assert_eq!(
        attachment.input.recv_timeout(SETTLE),
        Ok(decode_wakeup(b"press:-:char:q"))
    );
    assert_eq!(attachment.input.recv_timeout(SETTLE), Ok(Wakeup::Resize));
    assert_eq!(attachment.inbox.recv_timeout(SETTLE), Ok(Wakeup::Tick));
    assert_eq!(attachment.inbox.recv_timeout(SETTLE), Ok(Wakeup::Tick));
    drop(supervisor);
    assert_eq!(attachment.inbox.recv_timeout(SETTLE), Ok(Wakeup::Tick));
}

#[test]
fn room_events_are_scoped_once_before_reaching_each_attachment() {
    use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};
    let room = Room::new();
    let first_hello = hello(&SidebarInstanceId::new());
    let second_hello = hello(&SidebarInstanceId::new());
    let (first, first_live) = room.host.admit(&first_hello).unwrap();
    let (second, second_live) = room.host.admit(&second_hello).unwrap();
    let _first = room.host.plane.subscribe(
        &room.host.config_for(&first_hello),
        first_live.close.wake.clone(),
    );
    let _second = room.host.plane.subscribe(
        &room.host.config_for(&second_hello),
        second_live.close.wake.clone(),
    );
    let mut envelope = SidebarEventEnvelope::new(
        workspace(),
        Some("another-session".into()),
        1,
        SidebarEvent::FocusChanged {
            focused: vec![crate::ids::PaneId::from_parts(
                MuxName::Zellij,
                "terminal_1",
            )],
            unfocused: Vec::new(),
        },
    );
    room.host
        .room_wakeup(&serde_json::to_vec(&envelope).unwrap());
    for pane in [&first, &second] {
        assert_eq!(pane.inbox.try_recv(), Err(Empty));
    }
    assert_eq!(room.host.plane.event_store(), Default::default());
    envelope.session_name = None;
    let before = crate::utils::time::unix_now_ms();
    room.host
        .room_wakeup(&serde_json::to_vec(&envelope).unwrap());
    let after = crate::utils::time::unix_now_ms();
    let recorded = room.host.plane.event_store();
    let ttl_ms = 12_000;
    assert!(
        recorded.has_focus_since(1, before + ttl_ms),
        "record before either attachment consumes its inbox"
    );
    assert!(!recorded.has_focus_since(1, after + ttl_ms + 1));
    for pane in [&first, &second] {
        assert_eq!(pane.inbox.try_recv(), Ok(Wakeup::Event(envelope.clone())));
        assert_eq!(pane.inbox.try_recv(), Err(Empty));
    }
    envelope.workspace_id = WorkspaceId::from_project_root(Path::new("/another-workspace"));
    envelope.sent_at_ms = 2;
    room.host
        .room_wakeup(&serde_json::to_vec(&envelope).unwrap());
    for pane in [&first, &second] {
        assert_eq!(pane.inbox.try_recv(), Err(Empty));
    }
    assert_eq!(room.host.plane.event_store(), recorded);

    for _ in 0..64 {
        first_live.close.wake.try_send(Wakeup::Tick).unwrap();
    }
    envelope.workspace_id = workspace();
    envelope.sent_at_ms = 3;
    room.host
        .room_wakeup(&serde_json::to_vec(&envelope).unwrap());
    assert!(room.host.plane.event_store().has_focus_since(3, after));
    assert_eq!(second.inbox.try_recv(), Ok(Wakeup::Event(envelope)));
    assert_eq!(second.inbox.try_recv(), Err(Empty));
    for _ in 0..64 {
        assert_eq!(first.inbox.try_recv(), Ok(Wakeup::Tick));
    }
    assert_eq!(first.inbox.try_recv(), Err(Empty));
}

#[test]
fn a_room_click_reaches_only_the_named_attachment() {
    let mut room = Room::new();
    Arc::get_mut(&mut room.host).unwrap().diag = crate::diag::DiagSink::under(
        room._dir.path().to_owned(),
        workspace(),
        &room.host.room.session_name,
        None,
    );
    let id = SidebarInstanceId::new();
    let (first, _) = room.host.admit(&hello(&id)).unwrap();
    let (second, _) = room.host.admit(&hello(&SidebarInstanceId::new())).unwrap();
    let close = lock(&room.host.attachments)
        .get(id.as_str())
        .unwrap()
        .close
        .clone();
    while close.wake.try_send(Wakeup::Tick).is_ok() {}
    room.host
        .room_wakeup(format!("click:{}:4:7", SidebarInstanceId::new()).as_bytes());
    room.host.room_wakeup(format!("click:{id}:4:7").as_bytes());
    assert_eq!(
        first.input.try_recv(),
        Ok(Wakeup::MouseClick { column: 4, row: 7 })
    );
    assert_eq!(second.input.try_recv(), Err(Empty));
    let wake = lock(&room.host.attachments)
        .get(id.as_str())
        .unwrap()
        .close
        .input
        .clone();
    for _ in 0..64 {
        wake.try_send(Wakeup::MouseClick { column: 4, row: 7 })
            .unwrap();
    }
    room.host.room_wakeup(format!("click:{id}:4:7").as_bytes());
    drop(first);
    room.host.room_wakeup(format!("click:{id}:4:7").as_bytes());
    let rows: Vec<serde_json::Value> =
        std::fs::read_to_string(crate::diag::focus_trace::log_path(room._dir.path()))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|row: &serde_json::Value| row["instance_id"] == id.as_str())
            .collect();
    let deliveries: Vec<_> = rows
        .iter()
        .map(|row| row["event"]["delivery"].as_str().unwrap())
        .collect();
    assert_eq!(
        deliveries,
        ["sent", "inbox_full", "inbox_closed"],
        "click enqueue success must not hide a dropped click"
    );
    for row in rows {
        assert_eq!(row["event"]["kind"], "click_delivery");
        assert_eq!(row["event"]["column"], 4);
        assert_eq!(row["event"]["row"], 7);
        let event: crate::diag::focus_trace::FocusTraceEvent =
            serde_json::from_value(row["event"].clone()).unwrap();
        assert_eq!(serde_json::to_value(event).unwrap(), row["event"]);
    }
}

#[test]
fn a_room_click_pushes_only_cross_tab_targets_before_dispatch() {
    use crate::ids::PaneId;
    use crate::mux::focus_anchor::{FocusIntentState, load};
    use crate::sidebar_pane::app::fixtures::{pane, snapshot_with_panes};

    const CHILD: &str = "RIMZ_TEST_HOST_CLICK_PUSH";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let shim = dir.path().join("zellij");
        std::fs::write(
            &shim,
            r#"#!/bin/sh
click_dir=${0%/*}
case " $* " in
  *" action list-clients "*) printf 'CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n' ;;
  *" action focus-pane-id "*)
    if [ -e "$click_dir/pushed" ]; then
      printf 'after-push\n'
    else
      printf 'without-push\n'
    fi > "$click_dir/dispatched" ;;
esac
"#,
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sidebar_pane::host::tests::a_room_click_pushes_only_cross_tab_targets_before_dispatch",
                "--nocapture",
            ])
            .env(CHILD, dir.path())
            .env("RIMZ_ZELLIJ_BIN", shim)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }

    let dir = PathBuf::from(std::env::var_os(CHILD).unwrap());
    for cross_tab in [true, false] {
        let room = Room::new();
        let own = PaneId::from_parts(MuxName::Zellij, "terminal_10");
        let first = PaneId::from_parts(MuxName::Zellij, "terminal_11");
        let target = PaneId::from_parts(MuxName::Zellij, "terminal_12");
        let mut pty = Pty::open(54, 30);
        let source = room.attach_as(&pty, Some(own));
        painted(&mut pty);

        // Keep B's real inbox under test control so receipt and acknowledgement
        // are observed independently of datagram folds or destination paints.
        let mut greeting = hello(&SidebarInstanceId::new());
        greeting.pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_20");
        let (destination, live) = room.host.admit(&greeting).unwrap();
        let _subscription = room
            .host
            .plane
            .subscribe(&room.host.config_for(&greeting), live.close.wake);

        let mut snapshot = snapshot_with_panes(
            &workspace(),
            vec![
                pane("terminal_11", "tab_1", false),
                pane(
                    "terminal_12",
                    if cross_tab { "tab_2" } else { "tab_1" },
                    false,
                ),
            ],
        );
        Arc::make_mut(&mut Arc::make_mut(&mut snapshot.worktree_groups)[0].rows)[1].name =
            "click-target".into();
        snapshot.focused_pane = Some(first.clone());
        snapshot.viewed_panes = vec![first.clone()];
        snapshot.own_view = Some(crate::store::snapshot::SidebarOwnView {
            sibling_count: 2,
            working_pane_ids: if cross_tab {
                vec![first]
            } else {
                vec![first, target.clone()]
            },
            own_view_is_daemon: false,
        });
        room.host
            .plane
            .publish_snapshot(&source.instance_id, snapshot);
        let mut parser = vt100::Parser::new(30, 54, 0);
        let mut row = None;
        eventually("the click target is painted", || {
            parser.process(&pty.drain());
            row = parser
                .screen()
                .contents()
                .lines()
                .position(|line| line.contains("click-target"));
            row.is_some()
        });
        room.host
            .room_wakeup(format!("click:{}:4:{}", source.instance_id, row.unwrap()).as_bytes());

        if cross_tab {
            let instance_id = room.host.config_for(&greeting).instance_id;
            let mut received = None;
            eventually("a cross-tab room click pushes to B before dispatch", || {
                received = room.host.plane.take_focus_push(&instance_id);
                received.is_some()
            });
            let push = received.unwrap();
            assert_eq!(push.anchor.pane_id, target);
            assert_eq!(load(&room.runtime), Some(push.anchor.clone()));
            assert!(!dir.join("dispatched").exists(), "dispatch must wait for B");
            std::fs::write(dir.join("pushed"), b"").unwrap();
            push.finish(crate::diag::focus_trace::PushVerdict::Painted);
        }
        eventually("the click is dispatched", || {
            load(&room.runtime).is_some_and(|anchor| anchor.state == FocusIntentState::Applied)
        });
        assert_eq!(
            std::fs::read_to_string(dir.join("dispatched")).unwrap(),
            if cross_tab {
                "after-push\n"
            } else {
                "without-push\n"
            }
        );
        while let Ok(wakeup) = destination.inbox.try_recv() {
            assert_eq!(wakeup, Wakeup::Tick, "no further focus push is expected");
        }
        room.host.leave();
        drop(source);
        std::fs::remove_file(dir.join("dispatched")).unwrap();
        if cross_tab {
            std::fs::remove_file(dir.join("pushed")).unwrap();
        }
    }
}

#[test]
fn room_events_of_an_unknown_kind_reach_no_attachment() {
    use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};
    let room = Room::new();
    let first_hello = hello(&SidebarInstanceId::new());
    let second_hello = hello(&SidebarInstanceId::new());
    let (first, first_live) = room.host.admit(&first_hello).unwrap();
    let (second, second_live) = room.host.admit(&second_hello).unwrap();
    let _first = room
        .host
        .plane
        .subscribe(&room.host.config_for(&first_hello), first_live.close.wake);
    let _second = room
        .host
        .plane
        .subscribe(&room.host.config_for(&second_hello), second_live.close.wake);
    let envelope = SidebarEventEnvelope::new(workspace(), None, 1, SidebarEvent::PanesChanged);
    for kind in ["retired_event", "pane_frame_published"] {
        let mut value = serde_json::to_value(&envelope).unwrap();
        value["event"]["kind"] = kind.into();
        room.host.room_wakeup(&serde_json::to_vec(&value).unwrap());
        for pane in [&first, &second] {
            assert_eq!(pane.inbox.try_recv(), Err(Empty), "kind: {kind}");
        }
    }
}

#[test]
fn room_reload_words_and_envelopes_request_the_host_not_the_panes() {
    use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};
    let room = Room::new();
    let hello = hello(&SidebarInstanceId::new());
    let (pane, live) = room.host.admit(&hello).unwrap();
    let _subscription = room
        .host
        .plane
        .subscribe(&room.host.config_for(&hello), live.close.wake);
    let envelope = SidebarEventEnvelope::new(workspace(), None, 1, SidebarEvent::Reload);
    for bytes in [b"reload".to_vec(), serde_json::to_vec(&envelope).unwrap()] {
        room.host.room_wakeup(&bytes);
        assert!(matches!(
            lock(&room.host.inbox).as_ref().unwrap().try_recv(),
            Ok(Event::Reload)
        ));
        assert_eq!(pane.inbox.try_recv(), Err(Empty));
    }
}

#[test]
fn open_file_target_caps_at_hard_and_never_lowers() {
    assert_eq!(open_file_soft_target(1_024, 1_073_741_816, 65_536), 65_536);
    assert_eq!(open_file_soft_target(1_024, 4_096, 65_536), 4_096);
    assert_eq!(open_file_soft_target(1_024, 1_024, 65_536), 1_024);
    assert_eq!(
        open_file_soft_target(200_000, nix::libc::RLIM_INFINITY, 65_536),
        200_000
    );
}

// nextest runs each test in its own process, so lowering this process's
// limit reaches no other test.
#[test]
fn host_startup_raises_a_default_open_file_limit() {
    use nix::sys::resource::{Resource, getrlimit, setrlimit};
    let (_, hard) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
    if hard < 2_048 {
        return;
    }
    setrlimit(Resource::RLIMIT_NOFILE, 1_024, hard).unwrap();
    raise_open_file_limit();
    let (soft, _) = getrlimit(Resource::RLIMIT_NOFILE).unwrap();
    assert_eq!(soft, open_file_soft_target(1_024, hard, OPEN_FILE_TARGET));
}
