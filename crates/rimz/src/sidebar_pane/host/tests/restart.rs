use super::*;
use crate::ids::{MuxClientId, PaneId};
use crate::pane::ClientPaneView;
use crate::sidebar_pane::app::fixtures::{agent_snapshot, pane, remember_fold};

fn sidebar(index: usize) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, format!("sidebar_{index}"))
}

fn working(index: usize) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, format!("terminal_{index}"))
}

impl Room {
    fn view_client(&self, index: usize) {
        let mut snapshot = agent_snapshot(&workspace());
        let first = snapshot.worktree_groups[0].rows[0].clone();
        let mut panes = Vec::new();
        let rows = (0..2)
            .map(|index| {
                let tab = format!("tab_{index}");
                let mut chrome = pane(sidebar(index).raw(), &tab, false);
                chrome.command = Some("rimz-sidebar".into());
                panes.push(chrome);
                let work = pane(working(index).raw(), &tab, false);
                panes.push(work.clone());
                let mut row = first.clone();
                row.id = format!("agent-{index}");
                row.name = format!("restart-card-{index}");
                row.pane = Some(work);
                row
            })
            .collect::<Vec<_>>();
        Arc::make_mut(&mut snapshot.worktree_groups)[0].rows = rows.into();
        snapshot.viewed_panes = vec![sidebar(index)];
        let mut frame = crate::sidebar::frame::assemble_frame(
            panes,
            crate::utils::time::unix_now_ms(),
            "rimz-test",
        );
        frame.client_views = vec![ClientPaneView {
            client_id: MuxClientId::Zellij(1),
            pane_id: sidebar(index),
        }];
        frame.client_view_fresh = true;
        remember_fold(&self.host.plane, snapshot, frame);
    }

    fn attach_carried(&self, pty: &Pty, index: usize, baseline: Option<PaneId>) -> Pane {
        let mut greeting = hello(&SidebarInstanceId::new());
        greeting.pane_id = Some(sidebar(index));
        greeting.baseline_pane = baseline;
        self.attach_hello(pty, greeting, self.connect())
    }
}

fn selected(pty: &mut Pty, index: usize) {
    let mut parser = vt100::Parser::new(30, 54, 0);
    eventually("the carried selection is painted", || {
        parser.process(&pty.drain());
        parser
            .screen()
            .contents()
            .lines()
            .any(|line| line.contains(&format!("restart-card-{index}")) && line.contains('▌'))
    });
}

#[test]
fn an_arriving_attachment_never_replaces_the_views_existing_baseline() {
    let room = Room::new();
    room.view_client(0);
    let mut first = Pty::open(54, 30);
    let _first = room.attach_carried(&first, 0, Some(working(0)));
    selected(&mut first, 0);

    room.view_client(1);
    let mut second = Pty::open(54, 30);
    let _second = room.attach_carried(&second, 1, Some(working(1)));
    selected(&mut second, 0);
}

#[test]
fn a_hidden_attachments_carry_cannot_seed_the_view_seen_elsewhere() {
    let room = Room::new();
    room.view_client(0);
    let mut hidden = Pty::open(54, 30);
    let hidden_pane = room.attach_carried(&hidden, 1, Some(working(1)));
    eventually("hidden attachment initialized", || {
        room.heartbeat(&hidden_pane.instance_id).is_some()
    });
    let mut viewed = Pty::open(54, 30);
    let _viewed = room.attach_carried(&viewed, 0, Some(working(0)));
    selected(&mut viewed, 0);
    assert!(!has_frame_content(&hidden.drain(), 30, 54));
}

#[test]
fn an_unsized_attachments_carry_waits_for_its_first_painted_view() {
    let room = Room::new();
    room.view_client(0);
    let mut pending = Pty::open(0, 0);
    let first = room.attach_carried(&pending, 0, Some(working(0)));
    eventually("unsized attachment initialized", || {
        room.heartbeat(&first.instance_id).is_some()
    });
    assert!(pending.drain().is_empty());

    room.view_client(1);
    let mut viewed = Pty::open(54, 30);
    let _viewed = room.attach_carried(&viewed, 1, Some(working(1)));
    selected(&mut viewed, 1);
}
