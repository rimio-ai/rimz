use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::ids::{MuxName, ViewId, ViewKind, WorkspaceId};
use crate::mux::tab_name::TabOwnerRecord;
use crate::pane::PaneRef;
use crate::sidebar::frame::{PaneProcess, PaneState, TabFrame};
use crate::sidebar::test_support::{activity_row, worktree_group};

fn pane(id: &str) -> PaneRef {
    PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, id))
}

fn frame(name: &str, panes: &[&str]) -> PaneFrame {
    PaneFrame {
        produced_at_ms: 1,
        observed_at_ms: 1,
        topology_stamp_ms: Some(1),
        metrics_stamp_ms: None,
        build: None,
        session_name: "room".to_owned(),
        tabs: vec![TabFrame {
            view_id: ViewId::new_unchecked("@1"),
            kind: ViewKind::Window,
            name: Some(name.to_owned()),
            naming: Default::default(),
            panes: panes
                .iter()
                .map(|id| PaneState {
                    pane_id: PaneId::from_parts(MuxName::Tmux, id),
                    title: None,
                    first_seen_at_ms: None,
                    hosted_carry_since_ms: None,
                    is_floating: false,
                    current: PaneProcess {
                        pid: None,
                        command: None,
                        foreground_cmdline: None,
                        spawn_command: None,
                        cwd: None,
                        started_at: None,
                        hosted_agent_kind: None,
                        hosted_agent_process_start: None,
                        hosted_agent_lineage: Vec::new(),
                        resumed_session_id: None,
                        elevated_agent: None,
                    },
                    previous: None,
                    children: Vec::new(),
                    metrics: Default::default(),
                })
                .collect(),
        }],
        carried_panes: Vec::new(),
        viewed_panes: Vec::new(),
        client_views: Vec::new(),
        focused_pane: None,
        presence: None,
    }
}

fn snapshot(rows: Vec<(AgentStatus, &str, Timestamp)>, now: Timestamp) -> SidebarSnapshot {
    let workspace =
        WorkspaceId::parse("ws_0123456789abcdef01234567").expect("workspace id fixture");
    let mut snapshot = SidebarSnapshot::build_with_agents(workspace, Vec::new(), now);
    let rows = rows
        .into_iter()
        .map(|(status, pane_id, at)| {
            let mut row = activity_row(true, Some(status), at, std::path::Path::new("/repo"));
            row.pane = Some(pane(pane_id));
            row
        })
        .collect();
    snapshot.worktree_groups = vec![worktree_group(std::path::Path::new("/repo"), rows)];
    snapshot
}

#[test]
fn worst_live_agent_status_wins_the_tab() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(
        vec![
            (AgentStatus::Running, "%1", now),
            (AgentStatus::Waiting, "%2", now),
            (AgentStatus::Failed, "%3", now),
        ],
        now,
    );

    let renames = desired_tab_renames(&snapshot, &frame("#feat", &["%1", "%2", "%3"]), "zsh");

    assert_eq!(renames[0].desired_name, "#feat !");
    assert_eq!(renames[0].anchor.raw(), "%1");
    assert_eq!(
        renames[0].intent,
        TabNameIntent::Status {
            observed: "#feat".to_owned()
        }
    );
}

#[test]
fn success_expires_to_the_bare_manual_name() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let old = now - SignedDuration::from_mins(6);
    let snapshot = snapshot(vec![(AgentStatus::Success, "%1", old)], now);

    let renames = desired_tab_renames(&snapshot, &frame("my tab ✓", &["%1"]), "zsh");

    assert_eq!(renames[0].desired_name, "my tab");
    assert_eq!(
        renames[0].intent,
        TabNameIntent::Rest {
            observed: "my tab ✓".to_owned()
        }
    );
}

#[test]
fn status_change_replaces_both_catalog_variants() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(vec![(AgentStatus::Waiting, "%1", now)], now);
    let unicode = desired_tab_renames(&snapshot, &frame("manual ⏸\u{fe0e}", &["%1"]), "zsh");
    let nerd = desired_tab_renames(&snapshot, &frame("manual \u{f04c}", &["%1"]), "zsh");

    assert_eq!(unicode[0].desired_name, "manual ?");
    assert_eq!(nerd[0].desired_name, "manual ?");
}

#[test]
fn tab_status_always_uses_unicode_glyphs() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    for (status, expected) in [
        (AgentStatus::Failed, "#feat !"),
        (AgentStatus::Waiting, "#feat ?"),
        (AgentStatus::Paused, "#feat ⏸\u{FE0E}"),
        (AgentStatus::Running, "#feat ⢿"),
        (AgentStatus::Success, "#feat ✓"),
    ] {
        assert_eq!(
            TabStatus::from_row(status, true)
                .expect("tab status fixture")
                .glyph(),
            theme::unicode_glyph(theme::agent_status_glyph_role(status)),
        );
        let mut snapshot = snapshot(vec![(status, "%1", now)], now);
        snapshot.theme.glyphs = toml::from_str(
            "set = \"nerd_font\"\n\
             [nerd_font.status]\n\
             waiting = \"W\"\n\
             attention = \"A\"\n\
             paused = \"P\"\n\
             working = \"R\"\n\
             done = \"D\"\n",
        )
        .expect("glyph config");

        let renames = desired_tab_renames(&snapshot, &frame("#feat", &["%1"]), "zsh");

        assert_eq!(renames[0].desired_name, expected);
    }
}

#[test]
fn unchanged_name_emits_no_mux_work() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(vec![(AgentStatus::Running, "%1", now)], now);

    assert!(desired_tab_renames(&snapshot, &frame("#feat ⢿", &["%1"]), "zsh").is_empty());
}

fn named_frame(name: &str, names: &[&str]) -> PaneFrame {
    let ids = (1..=names.len())
        .map(|id| format!("%{id}"))
        .collect::<Vec<_>>();
    let mut frame = frame(name, &ids.iter().map(String::as_str).collect::<Vec<_>>());
    for (pane, name) in frame.tabs[0].panes.iter_mut().zip(names) {
        pane.title = Some((*name).to_owned());
    }
    frame
}

fn owned_frame(name: &str, names: &[&str], founders: &[&str]) -> PaneFrame {
    let mut frame = named_frame(name, names);
    frame.tabs[0].naming.owner = Some(TabOwnerRecord {
        base: name.to_owned(),
        founders: founders.iter().map(|id| pane(id).pane_id).collect(),
    });
    frame
}

#[test]
fn unrecorded_profile_name_is_user_owned() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    assert!(
        desired_tab_renames(
            &snapshot(vec![], now),
            &named_frame("opus", &["opus"]),
            "zsh"
        )
        .is_empty()
    );
}

#[test]
fn automatic_window_is_never_touched() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let mut frame = named_frame("btop", &["btop"]);
    frame.tabs[0].naming.automatic = true;
    for rows in [vec![], vec![(AgentStatus::Running, "%1", now)]] {
        let snapshot = snapshot(rows, now);
        for _ in 0..2 {
            assert!(desired_tab_renames(&snapshot, &frame, "zsh").is_empty());
        }
    }
}

#[test]
fn founder_holds_then_label_follows_then_releases() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let mut frame = owned_frame("debugger", &["debugger", "brainstormer"], &["%1"]);
    let both = snapshot(
        vec![
            (AgentStatus::Idle, "%1", now),
            (AgentStatus::Idle, "%2", now),
        ],
        now,
    );
    assert!(desired_tab_renames(&both, &frame, "zsh").is_empty());
    frame.tabs[0].panes.remove(0);
    let peer = snapshot(vec![(AgentStatus::Idle, "%2", now)], now);
    let renames = desired_tab_renames(&peer, &frame, "zsh");
    assert_eq!(renames.len(), 1);
    assert_eq!(renames[0].desired_name, "brainstormer");
    assert_eq!(renames[0].anchor.raw(), "%2");
    assert_eq!(
        renames[0].intent,
        TabNameIntent::Rebuild {
            observed: "debugger".to_owned(),
            base: "brainstormer".to_owned(),
        }
    );
    frame.tabs[0].name = Some("brainstormer".to_owned());
    frame.tabs[0].naming.owner.as_mut().unwrap().base = "brainstormer".to_owned();
    let renames = desired_tab_renames(&snapshot(vec![], now), &frame, "zsh");
    assert_eq!(renames[0].desired_name, "zsh");
    assert_eq!(
        renames[0].intent,
        TabNameIntent::Release {
            observed: "brainstormer".to_owned()
        }
    );
}

#[test]
fn founder_that_exited_to_a_shell_has_left() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let frame = owned_frame("debugger", &["debugger", "brainstormer"], &["%1"]);
    let renames = desired_tab_renames(
        &snapshot(vec![(AgentStatus::Idle, "%2", now)], now),
        &frame,
        "zsh",
    );
    assert_eq!(renames.len(), 1);
    assert_eq!(renames[0].desired_name, "brainstormer");
    assert_eq!(renames[0].anchor.raw(), "%2");
}

#[test]
fn untitled_survivor_keeps_the_base_and_only_updates_status() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    for (name, status, expected) in [
        ("founder", AgentStatus::Idle, None),
        ("founder", AgentStatus::Running, Some("founder ⢿")),
        ("founder ?", AgentStatus::Idle, Some("founder")),
    ] {
        let mut frame = owned_frame("founder", &["survivor"], &["%9"]);
        frame.tabs[0].name = Some(name.to_owned());
        frame.tabs[0].panes[0].title = None;
        let renames = desired_tab_renames(&snapshot(vec![(status, "%1", now)], now), &frame, "zsh");
        assert_eq!(
            renames.first().map(|rename| rename.desired_name.as_str()),
            expected
        );
        assert!(renames.iter().all(|rename| matches!(
            rename.intent,
            TabNameIntent::Status { .. } | TabNameIntent::Rest { .. }
        )));
    }
}

#[test]
fn tmux_follow_label_converges_on_the_read_back_name() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(vec![(AgentStatus::Idle, "%1", now)], now);
    for (title, read_back) in [
        ("host.lan", "host-lan"),
        ("a:b,c", "a-b_c"),
        (" peer ", "peer"),
    ] {
        let mut frame = owned_frame("founder", &[title], &["%9"]);
        let renames = desired_tab_renames(&snapshot, &frame, "zsh");
        assert_eq!(renames.len(), 1);
        assert!(matches!(renames[0].intent, TabNameIntent::Rebuild { .. }));
        frame.tabs[0].name = Some(read_back.to_owned());
        frame.tabs[0].naming.owner.as_mut().unwrap().base = read_back.to_owned();
        assert!(
            desired_tab_renames(&snapshot, &frame, "zsh").is_empty(),
            "{title}"
        );
        assert_eq!(renames[0].desired_name, read_back);
    }
}

#[test]
fn zellij_follow_label_keeps_the_raw_title_and_converges() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(vec![], now);
    let mut frame = owned_frame("founder", &["a:b,c.lan"], &[]);
    frame.tabs[0].kind = ViewKind::Tab;
    frame.tabs[0].view_id = ViewId::new_unchecked("tab_0");
    let survivor = &mut frame.tabs[0].panes[0];
    survivor.pane_id = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    survivor.current.hosted_agent_kind = Some(crate::ids::AgentKind::new_unchecked("claude"));
    let renames = desired_tab_renames(&snapshot, &frame, "zsh");
    assert_eq!(renames[0].desired_name, "a:b,c.lan");
    assert!(matches!(renames[0].intent, TabNameIntent::Rebuild { .. }));
    frame.tabs[0].name = Some("a:b,c.lan".to_owned());
    frame.tabs[0].naming.owner.as_mut().unwrap().base = "a:b,c.lan".to_owned();
    assert!(desired_tab_renames(&snapshot, &frame, "zsh").is_empty());
}

#[test]
fn user_base_over_record_gets_suffix_only() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let mut frame = owned_frame("opus", &["opus"], &["%1"]);
    frame.tabs[0].name = Some("my tab".to_owned());
    let renames = desired_tab_renames(
        &snapshot(vec![(AgentStatus::Running, "%1", now)], now),
        &frame,
        "zsh",
    );
    assert_eq!(renames[0].desired_name, "my tab ⢿");
    assert!(matches!(renames[0].intent, TabNameIntent::Status { .. }));
}

#[test]
fn rebuild_groups_survivors_oldest_first() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let mut frame = owned_frame("founder", &["d", "c", "b", "a", "missing-time"], &["%9"]);
    for (pane, first_seen) in
        frame.tabs[0]
            .panes
            .iter_mut()
            .zip([Some(4), Some(2), Some(2), Some(1), None])
    {
        pane.first_seen_at_ms = first_seen;
    }
    let snapshot = snapshot(
        (1..=5)
            .map(|id| match id {
                1 => "%1",
                2 => "%2",
                3 => "%3",
                4 => "%4",
                _ => "%5",
            })
            .map(|id| (AgentStatus::Idle, id, now))
            .collect(),
        now,
    );
    let renames = desired_tab_renames(&snapshot, &frame, "zsh");
    assert_eq!(renames.len(), 1);
    assert_eq!(renames[0].desired_name, "a+c+b+…");
    assert_eq!(renames[0].anchor.raw(), "%4");
}

#[test]
fn scoped_bases_are_never_rebuilt_or_released() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(vec![], now);
    for base in ["#feat", "team:forge", "team-forge"] {
        let mut frame = owned_frame(base, &["opus"], &["%9"]);
        assert!(desired_tab_renames(&snapshot, &frame, "zsh").is_empty());
        frame.tabs[0].name = Some(format!("{base} ?"));
        let renames = desired_tab_renames(&snapshot, &frame, "zsh");
        assert_eq!(renames[0].desired_name, base);
        assert!(matches!(renames[0].intent, TabNameIntent::Rest { .. }));
    }
}

#[test]
fn status_precedence_over_rebuild() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let frame = owned_frame("founder", &["a", "b"], &["%9"]);
    let snapshot = snapshot(
        vec![
            (AgentStatus::Running, "%1", now),
            (AgentStatus::Failed, "%2", now),
        ],
        now,
    );
    let renames = desired_tab_renames(&snapshot, &frame, "zsh");
    assert_eq!(renames[0].desired_name, "a+b !");
    assert!(matches!(renames[0].intent, TabNameIntent::Rebuild { .. }));
}

#[test]
fn waiting_loop_tab_gets_a_glyph_but_daemon_view_does_not() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(vec![(AgentStatus::Waiting, "%2", now)], now);
    let renames = desired_tab_renames(
        &snapshot,
        &named_frame("loop rimzd", &["rimz-sidebar", "claude"]),
        "zsh",
    );
    assert_eq!(renames.len(), 1);
    assert_eq!(renames[0].desired_name, "loop rimzd ?");
    assert!(
        desired_tab_renames(
            &snapshot,
            &named_frame("rimzd", &["rimz-sidebar", "claude"]),
            "zsh",
        )
        .is_empty()
    );
}

#[test]
fn pane_named_tab_releases_only_after_the_last_agent_leaves() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let frame = owned_frame("opus+codex", &["codex", "opus"], &["%1", "%2"]);
    for remaining in [vec!["%1", "%2"], vec!["%2"]] {
        for status in [
            AgentStatus::Idle,
            AgentStatus::Sleeping,
            AgentStatus::Success,
        ] {
            let snapshot = snapshot(
                remaining
                    .iter()
                    .map(|id| (status, *id, now - SignedDuration::from_mins(6)))
                    .collect(),
                now,
            );
            assert!(desired_tab_renames(&snapshot, &frame, "zsh").is_empty());
        }
    }
    let snapshot = snapshot(Vec::new(), now);
    let renames = desired_tab_renames(&snapshot, &frame, "zsh");
    assert_eq!(renames[0].desired_name, "zsh");
    assert_eq!(
        renames[0].intent,
        TabNameIntent::Release {
            observed: "opus+codex".to_owned()
        }
    );
    let mut released = frame;
    released.tabs[0].name = Some("zsh".to_owned());
    assert!(desired_tab_renames(&snapshot, &released, "zsh").is_empty());
}

#[test]
fn hosted_agent_without_a_row_blocks_release_even_when_floating() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(Vec::new(), now);
    let mut frame = owned_frame("opus", &["opus"], &["%1"]);
    frame.tabs[0].panes[0].current.hosted_agent_kind =
        Some(crate::ids::AgentKind::new_unchecked("claude"));
    frame.tabs[0].panes[0].is_floating = true;
    assert!(desired_tab_renames(&snapshot, &frame, "zsh").is_empty());
}

#[test]
fn scoped_manual_and_unpinned_names_are_kept_but_stale_status_clears() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(Vec::new(), now);
    for name in ["#feat", "team:forge", "my tab", "zsh"] {
        assert!(desired_tab_renames(&snapshot, &named_frame(name, &["opus"]), "zsh").is_empty());
    }
    for name in ["#feat", "team:forge"] {
        assert!(desired_tab_renames(&snapshot, &named_frame(name, &[name]), "zsh").is_empty());
    }
    assert!(desired_tab_renames(&snapshot, &frame("opus", &["%1"]), "zsh").is_empty());
    let renames = desired_tab_renames(&snapshot, &named_frame("#feat ?", &["opus"]), "zsh");
    assert_eq!(renames[0].desired_name, "#feat");
    assert_eq!(
        renames[0].intent,
        TabNameIntent::Rest {
            observed: "#feat ?".to_owned()
        }
    );
    let renames = desired_tab_renames(
        &snapshot,
        &owned_frame("opus+codex+pi+…", &["pi", "opus", "codex", "nvim"], &[]),
        "zsh",
    );
    assert_eq!(
        renames[0].intent,
        TabNameIntent::Release {
            observed: "opus+codex+pi+…".to_owned()
        }
    );
}

#[test]
fn chrome_and_daemon_panes_neither_claim_nor_hold_a_name() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(Vec::new(), now);
    for command in ["rimz-sidebar", "claude remote-control", "codex app-server"] {
        let mut frame = owned_frame("opus", &["opus", "opus"], &[]);
        frame.tabs[0].panes[0].current.command = Some(command.to_owned());
        frame.tabs[0].panes[0].current.hosted_agent_kind =
            Some(crate::ids::AgentKind::new_unchecked("claude"));
        let renames = desired_tab_renames(&snapshot, &frame, "zsh");
        assert_eq!(
            renames[0].intent,
            TabNameIntent::Release {
                observed: "opus".to_owned()
            }
        );
        assert_eq!(renames[0].anchor.raw(), "%2");
        frame.tabs[0].panes[1].title = None;
        frame.tabs[0].naming.owner = None;
        assert!(desired_tab_renames(&snapshot, &frame, "zsh").is_empty());
    }
}

#[test]
fn tab_with_only_chrome_or_daemon_panes_still_clears_stale_status() {
    let now = Timestamp::from_second(1_700_000_000).expect("time");
    let snapshot = snapshot(Vec::new(), now);
    for command in ["rimz-sidebar", "claude remote-control", "codex app-server"] {
        let mut frame = named_frame("opus !", &["opus"]);
        frame.tabs[0].panes[0].current.command = Some(command.to_owned());
        let renames = desired_tab_renames(&snapshot, &frame, "zsh");
        assert_eq!(renames[0].desired_name, "opus");
        assert_eq!(
            renames[0].intent,
            TabNameIntent::Rest {
                observed: "opus !".to_owned()
            }
        );
    }
}
