use super::*;
use crate::ids::{MuxClientId, PaneId};
use crate::sidebar_pane::app::fixtures::{publish_fold, snapshot_with_panes};
use crate::sidebar_pane::{CardReply, CardRequest, CardTarget};

fn request(room: &Room) -> CardReply {
    let stream = room.connect();
    stream.set_read_timeout(Some(SETTLE)).unwrap();
    attach::write_line(&stream, &CardRequest::CloseTarget).unwrap();
    let reply = attach::read_line::<CardReply>(&mut BufReader::new(stream)).unwrap();
    assert!(
        reply.is_some(),
        "a command gets a reply, not a pane admission or EOF"
    );
    reply.unwrap()
}

fn acting_view(clients: &[(u32, u32)], allowed: bool) {
    let room = Room::new();
    let id = |n| PaneId::from_parts(MuxName::Zellij, format!("terminal_{n}"));
    let mut pty = Pty::open(54, 30);
    let _attached = room.attach_as(&pty, Some(id(10)));
    painted(&mut pty);
    let mut second_pty = Pty::open(54, 30);
    let _second = room.attach_as(&second_pty, Some(id(20)));
    painted(&mut second_pty);
    let snapshot = snapshot_with_panes(
        &workspace(),
        vec![
            crate::sidebar_pane::app::fixtures::pane("terminal_1", "tab_1", false),
            crate::sidebar_pane::app::fixtures::pane("terminal_2", "tab_2", false),
        ],
    );
    let mut panes: Vec<_> = snapshot.rows().filter_map(|row| row.pane.clone()).collect();
    for (sidebar, tab) in [(10, "tab_1"), (20, "tab_2")] {
        let mut pane =
            crate::sidebar_pane::app::fixtures::pane(&format!("terminal_{sidebar}"), tab, false);
        pane.command = Some("rimz-sidebar".into());
        panes.push(pane);
    }
    panes.push(crate::sidebar_pane::app::fixtures::pane(
        "terminal_30",
        "tab_3",
        false,
    ));
    let mut frame = crate::sidebar::frame::assemble_frame(panes, 1, "rimz-test");
    frame.client_views = clients
        .iter()
        .map(|&(client, pane)| crate::pane::ClientPaneView {
            client_id: MuxClientId::Zellij(client),
            pane_id: id(pane),
        })
        .collect();
    frame.client_view_fresh = true;
    publish_fold(&room.host.plane, snapshot, frame);
    if allowed {
        let mut screen = vt100::Parser::new(30.try_into().unwrap(), 54.try_into().unwrap(), 0);
        eventually("the acting view has its highlighted row", || {
            screen.process(&pty.drain());
            screen.screen().contents().contains("zsh")
        });
        assert_eq!(
            request(&room),
            CardReply::CloseTarget {
                target: CardTarget::Pane(id(1))
            }
        );
    } else {
        assert!(matches!(request(&room), CardReply::Refused { .. }));
    }
    assert_eq!(
        lock(&room.host.attachments).len(),
        2,
        "commands are not attachments"
    );
}

#[test]
fn card_host_one_client_gets_its_highlight() {
    acting_view(&[(1, 1)], true);
}

#[test]
fn card_host_coviewers_get_their_shared_highlight() {
    acting_view(&[(1, 1), (2, 1)], true);
}

#[test]
fn card_host_clients_in_different_views_are_refused() {
    acting_view(&[(1, 1), (2, 2)], false);
}

#[test]
fn card_host_client_in_a_tab_without_sidebar_is_refused() {
    acting_view(&[(1, 30)], false);
}

fn unresponsive_view() -> (Room, Pane, Pty, Attachment) {
    let room = Room::new();
    let own = PaneId::from_parts(MuxName::Zellij, "terminal_10");
    let snapshot = snapshot_with_panes(
        &workspace(),
        vec![crate::sidebar_pane::app::fixtures::pane(
            "terminal_11",
            "tab_1",
            false,
        )],
    );
    let mut pty = Pty::open(54, 30);
    let attached = room.attach_as(&pty, Some(own.clone()));
    painted(&mut pty);
    let mut panes: Vec<_> = snapshot.rows().filter_map(|row| row.pane.clone()).collect();
    let mut sidebar = crate::sidebar_pane::app::fixtures::pane(own.raw(), "tab_1", false);
    sidebar.command = Some("rimz-sidebar".into());
    panes.push(sidebar);
    let mut frame = crate::sidebar::frame::assemble_frame(panes, 1, "rimz-test");
    frame.client_views = vec![crate::pane::ClientPaneView {
        client_id: MuxClientId::Zellij(1),
        pane_id: own.clone(),
    }];
    frame.client_view_fresh = true;
    publish_fold(&room.host.plane, snapshot, frame);
    eventually("the command view is ready", || {
        let (reply, _) = std::sync::mpsc::sync_channel(1);
        room.host
            .plane
            .card_input(CardRequest::CloseTarget, reply, Instant::now() + SETTLE)
            .is_ok()
    });
    let mut greeting = hello(&attached.instance_id);
    greeting.pane_id = own;
    let idle = Attachment::open(
        room.host.config_for(&greeting),
        &room.runtime,
        crate::diag::DiagSink::disabled(),
    );
    lock(&room.host.attachments)
        .get_mut(attached.instance_id.as_str())
        .unwrap()
        .close = idle.close_handle();
    (room, attached, pty, idle)
}

#[track_caller]
fn assert_host_refusal(room: &Room, reason: &str) {
    let (client, server) = UnixStream::pair().unwrap();
    let host = room.host.clone();
    let worker = std::thread::spawn(move || {
        host.serve_card(
            &server,
            CardRequest::CloseTarget,
            Duration::from_millis(100),
        );
    });
    client.set_read_timeout(Some(SETTLE)).unwrap();
    let mut replies = BufReader::new(client);
    let reply = attach::read_line::<CardReply>(&mut replies).unwrap();
    assert_eq!(
        reply,
        Some(CardRequest::CloseTarget.refused(reason)),
        "a transient host refusal must be a reply, not EOF"
    );
    assert_eq!(attach::read_line::<CardReply>(&mut replies).unwrap(), None);
    worker.join().unwrap();
}

#[test]
fn card_host_missing_destination_replies_with_a_refusal() {
    let (room, attached, _pty, _idle) = unresponsive_view();
    lock(&room.host.attachments).remove(attached.instance_id.as_str());
    assert_host_refusal(&room, "sidebar view is not ready; try again");
}

#[test]
fn card_host_full_input_lane_replies_with_a_refusal() {
    let (room, _attached, _pty, idle) = unresponsive_view();
    let input = idle.close_handle().input;
    while input.try_send(Wakeup::Tick).is_ok() {}
    assert_host_refusal(&room, "sidebar busy; try again");
}

#[test]
fn card_host_reply_deadline_replies_with_a_refusal() {
    let (room, _attached, _pty, idle) = unresponsive_view();
    assert_host_refusal(&room, "sidebar busy; try again");
    let Wakeup::Card(command) = idle.input.try_recv().unwrap() else {
        panic!("the card request is queued on the idle attachment");
    };
    assert_eq!(
        *command.answer.lock().unwrap(),
        Some(CardRequest::CloseTarget.refused("sidebar busy; try again")),
        "the deadline must answer the queued input before returning its refusal"
    );
}
