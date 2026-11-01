use std::time::Duration;

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{MuxBackend, TabOptions};

use super::support::*;
use crate::backend::shared_view::{
    SidebarView, agent_layout, seed_agents, sidebar_view, wait_frame, wait_screen,
};

fn reveal_selected_card(client: &mut AttachedClient, card: &str, label: &str) {
    let screen = wait_screen(
        || client.screen(),
        |s| {
            sidebar_view(s).selected_card() == Some(card)
                || s.lines()
                    .any(|line| line.trim_start().starts_with(['↑', '↓']))
        },
        "native focus baseline is visible or has an off-screen marker",
    );
    if let Some(row) = screen
        .lines()
        .position(|line| line.trim_start().starts_with(['↑', '↓']))
    {
        let row = row + 1;
        client.send(format!("\x1b[<0;2;{row}M\x1b[<0;2;{row}m").as_bytes());
    }
    wait_screen(
        || client.screen(),
        |s| sidebar_view(s).selected_card() == Some(card),
        label,
    );
}

fn bottom_view(client: &mut AttachedClient) -> SidebarView {
    let before = client.screen();
    let before_view = sidebar_view(&before);
    client.send(b"G");
    let last = wait_screen(
        || client.screen(),
        |s| {
            let view = sidebar_view(s);
            s != before
                && view.selection_visible
                && (view.unnamed_bottom_selected()
                    || (view.selected_card().is_some()
                        && view.selected_card() == view.last_card()
                        && view.selected_card() != before_view.selected_card()))
        },
        "client browses to the bottom",
    );
    if sidebar_view(&last).selected_card().is_none() {
        client.send(b"k");
    }
    sidebar_view(&wait_screen(
        || client.screen(),
        |s| {
            let view = sidebar_view(s);
            view.selected_card().is_some() && view.selected_card() == view.last_card()
        },
        "client selects a bottom card",
    ))
}

#[test]
fn hidden_sidebar_mirrors_a_new_card_before_native_switch() {
    require_zellij!();
    let room = LiveZellijSession::new("hidden-mirror");
    std::fs::write(room.path().join(".zshrc"), "").unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let binary = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let mut opts = sidebar_opts(room.name(), cwd.path(), binary, 160);
    opts.workspace_id = WorkspaceId::from_project_root(cwd.path());
    publish_room_bin(room.path(), &opts);
    opts.runtime = rimz::RuntimePaths::under(opts.workspace_id.clone(), room.path()).unwrap();
    room.backend().open_sidebar(&opts, None).unwrap();
    wait_for_pane_count(room.path(), room.name(), 2);
    let mut client = AttachedClient::attach(&room, 160, 32);
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
    let home = snapshot
        .panes
        .iter()
        .find(|pane| pane.is_sidebar() && before_tabs.contains(&pane.tab_id))
        .unwrap()
        .pane_ref(room.name())
        .pane_id;
    let hidden = snapshot
        .panes
        .iter()
        .find(|pane| pane.is_sidebar() && !before_tabs.contains(&pane.tab_id))
        .unwrap()
        .pane_ref(room.name())
        .pane_id;
    client.go_to_tab(1);
    client.press_alt_until('h', &home, "home sidebar before the card appears");
    wait_frame(
        &opts.runtime,
        |frame| frame.viewed_panes == [home.clone()],
        &format!("only the home sidebar is viewed before the card appears: {home}"),
    );
    std::thread::sleep(Duration::from_secs(3));
    let agents: Vec<_> = snapshot
        .panes
        .iter()
        .filter(|pane| pane.is_live_terminal() && !pane.is_sidebar())
        .take(1)
        .map(|pane| pane.pane_ref(room.name()))
        .collect();
    assert_eq!(agents.len(), 1);
    let state = rimz::StatePaths::for_project_root_under(cwd.path(), room.path()).unwrap();
    seed_agents(&state, &agents, cwd.path());
    wait_screen(
        || client.screen(),
        |screen| screen.contains("view-card-00"),
        "the watched sidebar shows the new card",
    );
    let captured = wait_screen(
        || {
            room.backend()
                .capture_pane(&hidden, room.name(), None, false)
                .unwrap()
                .raw_text
        },
        |screen| screen.contains("view-card-00"),
        "the hidden sidebar's native grid contains the new card before any switch",
    );
    assert!(captured.contains("view-card-00"));
    assert_eq!(client.view().clients.len(), 1);
    assert_eq!(client.view().clients[0].pane_id, home);
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
    seed_agents(&state, &agents, cwd.path());
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
        |s| {
            let view = sidebar_view(s);
            s.contains("VIEW-HOME") && view.top_selected()
        },
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
        &format!("host resolves the split before independent input: {second_id:?} on {away_id}"),
    );
    second.send(b"g");
    wait_screen(
        || second.screen(),
        |s| {
            let view = sidebar_view(s);
            s.contains("VIEW-AGENTS") && view.top_selected()
        },
        "second client's fork starts at the top",
    );
    let bottom = bottom_view(&mut first);
    let before = sidebar_view(&second.screen());
    assert!(
        before.top_selected(),
        "other client's view stays at the top while the first browses: {before:?}"
    );
    let next = before
        .cards
        .iter()
        .position(|(_, selected)| *selected)
        .map_or(0, |selected| selected + 1);
    let next = before.cards.get(next).map(|(id, _)| id.as_str());
    second.send(b"j");
    wait_screen(
        || second.screen(),
        |s| {
            let view = sidebar_view(s);
            if let Some(next) = next {
                return view.selected_card() == Some(next);
            }
            view.cards
                .windows(2)
                .any(|pair| Some(pair[0].0.as_str()) == before.selected_card() && pair[1].1)
        },
        &format!("other client's view stays at the top: before {before:?}"),
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
        &format!(
            "host samples the joined client before it leaves again: \
             {second_id:?} on {home_id} or {}",
            home_work.pane_id
        ),
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
        |s| {
            let view = sidebar_view(s);
            view.top_selected() && view.selected_card() != bottom.selected_card()
        },
        "fork scrolls independently again",
    );
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(sidebar_view(&first.screen()), bottom);
    let pre = bottom_view(&mut second);
    drop(first);
    wait_frame(
        &opts.runtime,
        |frame| frame.client_views.len() == 1 && frame.client_views[0].client_id == second_id,
        &format!("host resolves the departed co-viewer: only {second_id:?} remains"),
    );
    std::thread::sleep(Duration::from_secs(23));
    second.press_alt('l');
    let clients = poll_until(
        Duration::from_secs(15),
        || Ok::<_, String>(second.view().clients),
        |clients| {
            clients.iter().any(|client| {
                client.client_id == second_id
                    && agents.iter().any(|pane| pane.pane_id == client.pane_id)
            })
        },
        &format!(
            "Alt+l moves the second client into an agent pane in list-clients: \
             {second_id:?} leaving {away_id}"
        ),
    );
    let mut focused = clients
        .iter()
        .find(|client| client.client_id == second_id)
        .unwrap()
        .pane_id
        .clone();
    for _ in 0..3 {
        let mut slot = agents
            .iter()
            .position(|pane| pane.pane_id == focused)
            .unwrap();
        if Some(format!("view-card-{slot:02}").as_str()) == pre.selected_card() {
            let current = snapshot
                .panes
                .iter()
                .find(|pane| {
                    pane.is_live_terminal() && pane.pane_ref(room.name()).pane_id == focused
                })
                .unwrap();
            let neighbor = snapshot
                .panes
                .iter()
                .filter(|pane| {
                    pane.is_live_terminal()
                        && pane.tab_id == agent_tab
                        && pane.pane_x == current.pane_x
                        && pane.id != current.id
                })
                .min_by_key(|pane| pane.pane_y.abs_diff(current.pane_y))
                .unwrap();
            focused = neighbor.pane_ref(room.name()).pane_id;
            second.press_alt_once_until(
                if neighbor.pane_y < current.pane_y {
                    'k'
                } else {
                    'j'
                },
                &focused,
                "native focus leaves the pre-move browse pick before its replacement is checked",
            );
            slot = agents
                .iter()
                .position(|pane| pane.pane_id == focused)
                .unwrap();
        }
        assert_ne!(
            Some(format!("view-card-{slot:02}").as_str()),
            pre.selected_card(),
            "native focus must replace the pre-move browse pick: \
             {second_id:?} on {focused}, pre: {pre:?}"
        );
        wait_frame(
            &opts.runtime,
            |frame| {
                frame
                    .client_views
                    .iter()
                    .any(|client| client.client_id == second_id && client.pane_id == focused)
            },
            &format!(
                "host frame follows the move list-clients confirmed: {second_id:?} on {focused}"
            ),
        );
        second.press_alt('h');
        let clients = poll_until(
            Duration::from_secs(15),
            || Ok::<_, String>(second.view().clients),
            |clients| {
                clients
                    .iter()
                    .any(|client| client.client_id == second_id && client.pane_id != focused)
            },
            "Alt+h moves the second client toward its sidebar in list-clients",
        );
        let pane = &clients
            .iter()
            .find(|client| client.client_id == second_id)
            .unwrap()
            .pane_id;
        if pane == &away_id {
            break;
        }
        assert!(
            agents.iter().any(|agent| &agent.pane_id == pane),
            "Alt+h must stay in the agents tab until the sidebar: {clients:?}"
        );
        focused.clone_from(pane);
    }
    let slot = agents
        .iter()
        .position(|pane| pane.pane_id == focused)
        .unwrap();
    let baseline = format!("view-card-{slot:02}");
    wait_frame(
        &opts.runtime,
        |frame| {
            frame
                .client_views
                .iter()
                .any(|client| client.client_id == second_id && client.pane_id == away_id)
        },
        &format!("sidebar focus holds the working-pane baseline: {second_id:?} on {away_id}"),
    );
    // Native focus does not scroll; revealing its marker leaves the selection unchanged.
    reveal_selected_card(
        &mut second,
        &baseline,
        &format!(
            "working-pane focus reaches the selection baseline before browsing: \
             {second_id:?} on {focused}, card {baseline}"
        ),
    );
    // Reattach can choose the entry pane, so seat that baseline before browsing the held view.
    let retention_baseline = &agents[0].pane_id;
    second.press_alt_once_until('l', &focused, "return to the working pane before retention");
    second.press_alt_until(
        'k',
        retention_baseline,
        "retention uses the entry working pane",
    );
    wait_frame(
        &opts.runtime,
        |frame| {
            frame.client_views.iter().any(|client| {
                client.client_id == second_id && &client.pane_id == retention_baseline
            })
        },
        &format!("host resolves the retention baseline: {second_id:?} on {retention_baseline}"),
    );
    reveal_selected_card(
        &mut second,
        &format!("view-card-{slot:02}", slot = 0),
        "entry working-pane baseline reaches the selection before the hold",
    );
    second.press_alt_until('h', &away_id, "sidebar before the retention browse");
    second.send(b"g");
    wait_screen(
        || second.screen(),
        |s| {
            let view = sidebar_view(s);
            view.top_selected()
        },
        "retention browse starts at the first listed card",
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
    let returning_focus = &returning.view().clients[0].pane_id;
    assert!(
        returning_focus == retention_baseline || returning_focus == &away_id,
        "returning native focus must keep the retention baseline: \
         {returning_focus}, expected {retention_baseline} or {away_id}"
    );
    wait_frame(
        &opts.runtime,
        |frame| frame.client_views.len() == 1 && &frame.client_views[0].pane_id == returning_focus,
        "host resolves the returning client",
    );
    wait_screen(
        || returning.screen(),
        |s| sidebar_view(s) == held,
        "reattach within the hold keeps scroll and selection",
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
        |s| sidebar_view(s).selected_card() == Some(previous.as_str()),
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
    let mut fresh = AttachedClient::attach(&room, 160, 32);
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
    let expired = sidebar_view(&wait_screen(
        || fresh.screen(),
        |s| {
            let view = sidebar_view(s);
            view.first_card().is_some() && view.selected_card() == baseline.as_deref()
        },
        "expired view keeps native focus, not the retained browse pick",
    ));
    let sidebar = if focused == &home_id || focused == &home_work.pane_id {
        &home_id
    } else {
        &away_id
    };
    fresh.press_alt_until(
        'h',
        sidebar,
        "fresh client's sidebar for the browse-offset proof",
    );
    fresh.send(b"g");
    let homed = sidebar_view(&wait_screen(
        || fresh.screen(),
        |s| {
            let view = sidebar_view(s);
            view.top_selected()
        },
        "fresh client's home key selects the first listed card",
    ));
    assert_eq!(
        homed.first_card(),
        expired.first_card(),
        "expired view drops the retained browse offset",
    );
}
