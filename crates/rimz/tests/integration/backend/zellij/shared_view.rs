use std::time::Duration;

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{MuxBackend, TabOptions};

use super::support::*;
use crate::backend::shared_view::{
    SidebarView, agent_layout, seed_agents, sidebar_view, wait_frame, wait_screen,
};

fn bottom_view(client: &mut AttachedClient) -> SidebarView {
    client.send(b"G");
    let last = wait_screen(
        || client.screen(),
        |s| {
            !s.contains("view-card-00")
                && !sidebar_view(s).cards.is_empty()
                && sidebar_view(s).selection_visible
        },
        "client browses to the bottom",
    );
    if !sidebar_view(&last)
        .cards
        .iter()
        .any(|(_, selected)| *selected)
    {
        client.send(b"k");
    }
    sidebar_view(&wait_screen(
        || client.screen(),
        |s| {
            !s.contains("view-card-00")
                && sidebar_view(s).cards.iter().any(|(_, selected)| *selected)
        },
        "client selects a bottom card",
    ))
}

#[test]
fn two_clients_split_join_and_carry_the_sidebar_view() {
    require_zellij!();
    let room = LiveZellijSession::new("shared-view");
    std::fs::write(room.path().join(".zshrc"), "").unwrap();
    std::fs::write(
        room.path().join("config.toml"),
        "[sidebar]\nafk_after_secs = 20\n",
    )
    .unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let binary = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let mut opts = sidebar_opts(room.name(), cwd.path(), binary, 160);
    opts.workspace_id = WorkspaceId::from_project_root(cwd.path());
    publish_room_bin(room.path(), &opts);
    opts.runtime = rimz::RuntimePaths::under(opts.workspace_id.clone(), room.path()).unwrap();
    room.backend().open_sidebar(&opts, None).unwrap();
    wait_for_pane_count(room.path(), room.name(), 2);
    let mut first = AttachedClient::attach(&room, 160, 32);
    let before_tabs = PaneSnapshot::expect(room.path(), room.name()).tab_ids();
    room.backend()
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "agents".to_owned(),
            panes: agent_layout(),
            focus: false,
            dock_sidebar: true,
            after: None,
            sidebar: opts.clone(),
        })
        .unwrap();
    let snapshot = PaneSnapshot::expect(room.path(), room.name());
    let new_tabs: Vec<_> = snapshot
        .tab_ids()
        .into_iter()
        .filter(|id| !before_tabs.contains(id))
        .collect();
    assert_eq!(new_tabs.len(), 1, "one new tab: {new_tabs:?}");
    let agent_tab = new_tabs[0];
    let agents: Vec<_> = snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == agent_tab && pane.is_live_terminal() && !pane.is_sidebar())
        .map(|pane| pane.pane_ref(room.name()))
        .collect();
    assert_eq!(agents.len(), 12);
    let state = rimz::StatePaths::for_project_root_under(cwd.path(), room.path()).unwrap();
    seed_agents(&state, &agents);
    let sidebars: Vec<_> = snapshot
        .panes
        .iter()
        .filter(|pane| pane.is_sidebar())
        .collect();
    assert_eq!(sidebars.len(), 2);
    let home = sidebars
        .iter()
        .find(|pane| pane.tab_id != agent_tab)
        .unwrap();
    let away = sidebars
        .iter()
        .find(|pane| pane.tab_id == agent_tab)
        .unwrap();
    let home_id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", home.id));
    let away_id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", away.id));
    let home_work = snapshot
        .panes
        .iter()
        .find(|pane| pane.tab_id == home.tab_id && pane.is_live_terminal() && !pane.is_sidebar())
        .unwrap()
        .pane_ref(room.name());
    room.backend()
        .send_keys(
            &home_work.pane_id,
            room.name(),
            "printf '\\126IEW-HOME\\n'\n",
        )
        .unwrap();
    first.go_to_tab(1);
    first.press_alt_until('h', &home_id, "first client's home sidebar");
    wait_screen(
        || first.screen(),
        |s| s.contains("VIEW-HOME") && s.contains("⢿ 12"),
        "all twelve running cards reach the first client",
    );
    first.send(b"g");
    wait_screen(
        || first.screen(),
        |s| s.contains("VIEW-HOME") && s.contains("view-card-00"),
        "first client's home view",
    );
    let first_id = first.view().clients[0].client_id.clone();
    let mut second = AttachedClient::attach_beside(&room, 160, 32, 1);
    let second_id = second
        .view()
        .clients
        .iter()
        .find(|client| client.client_id != first_id)
        .unwrap()
        .client_id
        .clone();
    second.go_to_tab(2);
    let clients = poll_until(
        Duration::from_secs(15),
        || Ok::<_, String>(second.view().clients),
        |clients| {
            clients.iter().any(|client| {
                client.client_id == second_id
                    && (client.pane_id == away_id
                        || agents.iter().any(|pane| pane.pane_id == client.pane_id))
            })
        },
        "second client enters the agents tab",
    );
    wait_screen(
        || second.screen(),
        |s| s.contains("VIEW-AGENTS"),
        "second client's terminal renders the agents tab",
    );
    if clients
        .iter()
        .any(|client| client.client_id == second_id && client.pane_id != away_id)
    {
        second.press_alt('h');
    }
    poll_until(
        Duration::from_secs(15),
        || Ok::<_, String>(second.view().clients),
        |clients| {
            clients
                .iter()
                .any(|client| client.client_id == first_id && client.pane_id == home_id)
                && clients
                    .iter()
                    .any(|client| client.client_id == second_id && client.pane_id == away_id)
        },
        "clients on separate sidebar panes",
    );
    wait_frame(
        &opts.runtime,
        |frame| {
            frame
                .client_views
                .iter()
                .any(|client| client.client_id == second_id && client.pane_id == away_id)
        },
        "host resolves the split before independent input",
    );
    second.send(b"g");
    wait_screen(
        || second.screen(),
        |s| s.contains("VIEW-AGENTS") && s.contains("view-card-00"),
        "second client's fork starts at the top",
    );
    let bottom = bottom_view(&mut first);
    let before = sidebar_view(&second.screen());
    second.send(b"j");
    wait_screen(
        || second.screen(),
        |s| sidebar_view(s) != before && s.contains("view-card-00"),
        "other client's view stays at the top",
    );
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(sidebar_view(&first.screen()), bottom);
    second.go_to_tab(1);
    poll_until(
        Duration::from_secs(15),
        || Ok::<_, String>(second.view().clients),
        |clients| {
            clients.iter().any(|client| {
                client.client_id == second_id
                    && (client.pane_id == home_id || client.pane_id == home_work.pane_id)
            })
        },
        "second client joins the home sidebar",
    );
    wait_screen(
        || second.screen(),
        |s| s.contains("VIEW-HOME") && sidebar_view(s) == bottom,
        "arrival joins the incumbent's scrolled view",
    );
    wait_frame(
        &opts.runtime,
        |frame| {
            frame.client_views.iter().any(|client| {
                client.client_id == second_id
                    && (client.pane_id == home_id || client.pane_id == home_work.pane_id)
            })
        },
        "host samples the joined client before it leaves again",
    );
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(sidebar_view(&second.screen()), bottom);
    second.go_to_tab(2);
    poll_until(
        Duration::from_secs(15),
        || Ok::<_, String>(second.view().clients),
        |clients| {
            clients
                .iter()
                .any(|client| client.client_id == second_id && client.pane_id == away_id)
        },
        "second client returns to the away sidebar",
    );
    wait_screen(
        || second.screen(),
        |s| s.contains("VIEW-AGENTS") && sidebar_view(s) == bottom,
        "leaving carries the joined view",
    );
    second.send(b"g");
    wait_screen(
        || second.screen(),
        |s| s.contains("view-card-00"),
        "fork scrolls independently again",
    );
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(sidebar_view(&first.screen()), bottom);
    drop(first);
    wait_frame(
        &opts.runtime,
        |frame| frame.client_views.len() == 1 && frame.client_views[0].client_id == second_id,
        "host resolves the departed co-viewer",
    );
    std::thread::sleep(Duration::from_secs(23));
    room.backend()
        .focus_pane(&agents[0].pane_id, Some(room.name()))
        .unwrap();
    wait_frame(
        &opts.runtime,
        |frame| {
            frame
                .client_views
                .iter()
                .any(|client| client.client_id == second_id && client.pane_id == agents[0].pane_id)
        },
        "retained browse baseline matches the new client's initial working pane",
    );
    wait_screen(
        || second.screen(),
        |s| {
            sidebar_view(s)
                .cards
                .iter()
                .any(|(id, selected)| id == "view-card-00" && *selected)
        },
        "working-pane focus reaches the selection baseline before browsing",
    );
    second.press_alt('h');
    wait_frame(
        &opts.runtime,
        |frame| {
            frame
                .client_views
                .iter()
                .any(|client| client.client_id == second_id && client.pane_id == away_id)
        },
        "sidebar focus holds the working-pane baseline",
    );
    let held = bottom_view(&mut second);
    let selected = held
        .cards
        .iter()
        .position(|(_, selected)| *selected)
        .unwrap();
    let previous = &held.cards[selected.checked_sub(1).expect("preceding visible card")].0;
    drop(second);
    wait_for_human_client_count(room.backend(), room.name(), 0);
    wait_frame(
        &opts.runtime,
        |frame| {
            frame.client_views.is_empty() && frame.presence.is_some_and(|p| p.human_clients == 0)
        },
        "host resolves full detach before the hold starts",
    );
    std::thread::sleep(Duration::from_secs(1));
    let mut returning = AttachedClient::attach(&room, 160, 32);
    wait_screen(
        || returning.screen(),
        |s| sidebar_view(s) == held,
        "reattach within the hold keeps scroll and selection",
    );
    wait_frame(
        &opts.runtime,
        |frame| frame.client_views.len() == 1,
        "host resolves the returning client",
    );
    wait_screen(
        || returning.screen(),
        |s| sidebar_view(s) == held && !s.contains("zᶻ away"),
        "returning client receives its retained view after resolution",
    );
    returning.press_alt_until('h', &away_id, "returning client's sidebar");
    returning.send(b"k");
    wait_screen(
        || returning.screen(),
        |s| {
            sidebar_view(s)
                .cards
                .iter()
                .any(|(id, selected)| id == previous && *selected)
        },
        "returning input continues the retained cursor, not cached terminal bytes",
    );
    drop(returning);
    wait_for_human_client_count(room.backend(), room.name(), 0);
    wait_frame(
        &opts.runtime,
        |frame| {
            frame.client_views.is_empty() && frame.presence.is_some_and(|p| p.human_clients == 0)
        },
        "host resolves the second full detach",
    );
    std::thread::sleep(Duration::from_secs(23));
    let fresh = AttachedClient::attach(&room, 160, 32);
    let focused = &fresh.view().clients[0].pane_id;
    let baseline = agents
        .iter()
        .position(|pane| &pane.pane_id == focused)
        .map(|slot| format!("view-card-{slot:02}"));
    wait_frame(
        &opts.runtime,
        |frame| frame.client_views.len() == 1,
        "host resolves the fresh client after expiry",
    );
    wait_screen(
        || fresh.screen(),
        |s| {
            s.contains("view-card-00")
                && sidebar_view(s)
                    .cards
                    .iter()
                    .find(|(_, selected)| *selected)
                    .map(|(id, _)| id.as_str())
                    == baseline.as_deref()
        },
        "expired view starts at the top with native focus, not the retained browse pick",
    );
}
