use super::*;
use crate::ids::WorkspaceId;
use crate::mux::{LayoutColumn, PaneCmd};

#[test]
fn resume_in_the_lane_directory_stays_in_the_origin_pane() {
    let project = Path::new("/repo");
    let worktree = Path::new("/repo-worktrees/single-card");

    // The dropped-to-shell origin pane: launch dir is the cohort cwd, so
    // in-place placement stays available despite the lane channel.
    assert!(!resume_outside_launch_dir(
        Some("single-card"),
        worktree,
        worktree,
        project,
        worktree,
        Some(worktree),
    ));

    // From anywhere else the lane resume still opens its own tab.
    assert!(resume_outside_launch_dir(
        Some("single-card"),
        worktree,
        worktree,
        project,
        project,
        Some(project),
    ));
    assert!(resume_outside_launch_dir(
        Some("single-card"),
        worktree,
        worktree,
        project,
        worktree,
        None,
    ));

    let empty = Path::new("/rimz/empty/room/searcher");
    assert!(!resume_outside_launch_dir(
        None,
        empty,
        project,
        project,
        project,
        Some(project)
    ));
    assert!(!resume_outside_launch_dir(
        Some("single-card"),
        empty,
        worktree,
        project,
        project,
        Some(empty)
    ));
}

#[test]
fn resume_launch_dir_comparison_is_lexically_normalized() {
    let project = Path::new("/repo");
    let worktree = Path::new("/repo-worktrees/single-card");
    let launch_dir = Path::new("/repo-worktrees/../repo-worktrees/single-card");

    assert!(!resume_outside_launch_dir(
        Some("single-card"),
        worktree,
        worktree,
        project,
        worktree,
        Some(launch_dir),
    ));
}

fn request(placement: Placement) -> PlacementRequest {
    PlacementRequest {
        placement,
        mux: MuxName::Tmux,
        tab_anchor: None,
        cwd: PathBuf::from("/work"),
        title: "#lane".to_owned(),
        panes: LayoutPanes {
            columns: vec![LayoutColumn {
                panes: vec![PaneCmd {
                    argv: vec!["rimz".to_owned(), "agents".to_owned()],
                    name: Some("codex".to_owned()),
                }],
                stacked: false,
            }],
            focused_pane: 0,
        },
        sidebar: SidebarPaneOptions {
            runtime: crate::RuntimePaths::under(
                WorkspaceId::from_project_root(Path::new("/work")),
                tempfile::tempdir().expect("runtime root").path(),
            )
            .expect("runtime paths"),
            session_name: "room".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/work")),
            project_root: PathBuf::from("/work"),
            extra_env: BTreeMap::new(),
            cwd: PathBuf::from("/work"),
            target: crate::mux::SidebarTarget {
                share: crate::mux::WidthPermille::from_percent(25),
                max_cols: std::num::NonZeroU16::new(72).expect("nonzero test width"),
                pinned: false,
            },
            detected_view_size: None,
            rimz_bin: PathBuf::from("/bin/rimz"),
            pristine_birth: false,
            config: crate::config::MultiplexerConfig::default(),
            resume_tabs: Vec::new(),
            refresh_ms: None,
        },
        identity_env: BTreeMap::from([("RIMZ_PROJECT_MODE".to_owned(), "1".to_owned())]),
        focus: LaunchFocus::Keep,
        errors: PlacementErrors {
            new_tab: "tab context",
            new_pane: "pane context",
            same_pane: "same context",
        },
    }
}

#[test]
fn tab_anchor_requires_a_non_team_new_tab_in_the_callers_room() {
    use Placement::{NewPane, NewTab, SamePane};

    let pane = PaneId::from_parts(MuxName::Tmux, "%7");
    let project = Path::new("/room");
    let other_room = Path::new("/other-room");
    for (placement, has_team, has_pane, caller_room, anchored) in [
        (NewTab, false, true, Some(project), true),
        (NewTab, false, false, Some(project), false),
        (NewTab, true, true, Some(project), false),
        (NewPane, false, true, Some(project), false),
        (SamePane, false, true, Some(project), false),
        (NewTab, false, true, Some(other_room), false),
        (NewTab, false, true, None, false),
    ] {
        assert_eq!(
            resolve_tab_anchor(
                placement,
                has_team,
                has_pane.then(|| pane.clone()),
                caller_room,
                project,
            ),
            anchored.then(|| pane.clone()),
        );
    }
}

#[test]
fn prepares_new_tab_options() {
    for focus in [LaunchFocus::Keep, LaunchFocus::Take] {
        for anchor in [Some(PaneId::from_parts(MuxName::Tmux, "%7")), None] {
            let mut request = request(Placement::NewTab);
            request.focus = focus;
            request.tab_anchor = anchor.clone();
            let PreparedPlacement::NewTab(options) =
                prepare_resolved(request, Some((120, 40)), None).unwrap()
            else {
                panic!("new tab placement");
            };
            assert_eq!(options.after, anchor);
            assert_eq!(options.sidebar.session_name, "room");
            assert_eq!(options.title, "#lane");
            assert_eq!(options.env["RIMZ_PROJECT_MODE"], "1");
            assert_eq!(options.sidebar.cwd, Path::new("/work"));
            assert_eq!(options.panes.columns[0].panes[0].argv, ["rimz", "agents"]);
            assert_eq!(options.focus, focus == LaunchFocus::Take);
            assert!(options.dock_sidebar);
            assert_eq!(options.sidebar.session_name, "room");
        }
    }
}

#[test]
fn prepares_new_pane_options() {
    for focus in [LaunchFocus::Keep, LaunchFocus::Take] {
        let mut request = request(Placement::NewPane);
        request.focus = focus;
        let target = PaneId::from_parts(MuxName::Tmux, "%7");
        let PreparedPlacement::NewPane(options) =
            prepare_resolved(request, Some((120, 40)), Some(target.clone())).unwrap()
        else {
            panic!("new pane placement");
        };
        assert_eq!(options.target, SplitTarget::Pane(target));
        assert_eq!(options.cwd.as_deref(), Some("/work"));
        assert_eq!(
            options.command.as_deref(),
            Some(&["rimz".to_owned(), "agents".to_owned()][..])
        );
        assert_eq!(options.env["RIMZ_PROJECT_MODE"], "1");
        assert_eq!(options.title.as_deref(), Some("codex"));
        assert_eq!(options.placement, SplitPlacement::default());
        assert_eq!(options.focus, focus == LaunchFocus::Take);
    }
}

#[test]
fn prepares_same_pane_exec() {
    let PreparedPlacement::SamePane { argv, env, cwd } =
        prepare_resolved(request(Placement::SamePane), Some((40, 120)), None).unwrap()
    else {
        panic!("same pane placement");
    };
    assert_eq!(argv, ["rimz", "agents"]);
    assert_eq!(env["RIMZ_PROJECT_MODE"], "1");
    assert_eq!(cwd, Path::new("/work"));
}

#[test]
fn single_pane_extraction_reports_empty_layout() {
    let mut request = request(Placement::NewPane);
    request.panes.columns.clear();
    let err = prepare_resolved(request, None, None)
        .err()
        .expect("empty layout must fail");
    assert_eq!(err.to_string(), "in-pane launch produced no pane command");
}

#[test]
fn failure_context_follows_placement() {
    for (placement, expected) in [
        (Placement::NewTab, "tab context"),
        (Placement::NewPane, "pane context"),
        (Placement::SamePane, "same context"),
    ] {
        let request = request(placement);
        let actual = match placement {
            Placement::NewTab => request.errors.new_tab,
            Placement::NewPane => request.errors.new_pane,
            Placement::SamePane => request.errors.same_pane,
        };
        assert_eq!(actual, expected);
    }
}

#[test]
fn resolves_launch_placement_matrix() {
    use Placement::{NewPane, NewTab, SamePane};

    for (policy, is_worktree, single_cell, has_pane, expected) in [
        (LaunchPlacement::Auto, false, true, true, SamePane),
        (LaunchPlacement::Auto, true, true, true, NewTab),
        (LaunchPlacement::Auto, false, false, true, NewTab),
        (LaunchPlacement::Auto, false, true, false, NewTab),
        (LaunchPlacement::Pane, false, true, true, NewPane),
        (LaunchPlacement::Pane, true, true, true, NewTab),
        (LaunchPlacement::Tab, false, true, true, NewTab),
    ] {
        assert_eq!(
            resolve_placement(false, false, policy, is_worktree, single_cell, has_pane).unwrap(),
            expected
        );
    }
    assert_eq!(
        resolve_placement(true, false, LaunchPlacement::Auto, false, true, true).unwrap(),
        NewTab
    );
    assert_eq!(
        resolve_placement(false, true, LaunchPlacement::Auto, true, true, true).unwrap(),
        NewPane
    );
    assert_eq!(
        apply_in_place_downgrade(SamePane, LaunchFocus::Keep, true),
        NewPane
    );
    assert_eq!(
        apply_in_place_downgrade(SamePane, LaunchFocus::Take, false),
        NewPane
    );
    assert_eq!(
        apply_in_place_downgrade(SamePane, LaunchFocus::Take, true),
        SamePane
    );

    let multi =
        resolve_placement(false, true, LaunchPlacement::Auto, false, false, true).unwrap_err();
    assert!(multi.to_string().contains("single agent cell"));
    let outside =
        resolve_placement(false, true, LaunchPlacement::Auto, false, true, false).unwrap_err();
    assert!(outside.to_string().contains("inside the room"));
}

#[test]
fn fork_defaults_to_launching_pane() {
    use Placement::{NewPane, NewTab, SamePane};

    for (new_tab, new_pane, bg, has_pane, expected) in [
        (false, false, false, true, SamePane),
        (false, true, false, true, NewPane),
        (true, false, false, true, NewTab),
        (false, false, true, true, NewPane),
        (false, false, false, false, NewTab),
    ] {
        assert_eq!(
            resolve_fork_placement(new_tab, new_pane, LaunchFocus::resolve(bg, None), has_pane)
                .unwrap(),
            expected
        );
    }
}

use crate::agents::{AgentState, AgentStatus, PermissionMode};
use crate::disk::paths::{RuntimePaths, StatePaths};
use crate::ids::{AgentKind, AgentSessionId};
use crate::pane::PaneRef;
use crate::store::run::{RunRecord, RunStatus};
use std::time::Duration;
#[test]
fn bind_timeout_records_last_read_failure() {
    assert_bind_timeout_last_read(
        Err("projection unreadable".to_owned()),
        serde_json::json!({"error": "projection unreadable"}),
    );
}

#[test]
fn bind_timeout_records_no_matching_rows() {
    assert_bind_timeout_last_read(
        Ok(vec![agent_state("codex", "other", AgentStatus::Idle)]),
        serde_json::json!([]),
    );
}

#[test]
fn bind_timeout_records_ended_and_unbound_rows() {
    let mut ended = agent_state("codex", "ended", AgentStatus::Idle);
    ended.launch_id = Some("launch_child".into());
    ended.ended_at = Some("2026-01-01T00:00:00Z".parse().unwrap());
    ended.pane = Some(pane_ref("%2", "subagents"));
    let mut unbound = agent_state("claude", "unbound", AgentStatus::Idle);
    unbound.launch_id = Some("launch_child".into());
    assert_bind_timeout_last_read(
        Ok(vec![ended, unbound]),
        serde_json::json!([
            {"kind": "codex", "pane": "tmux:%2", "ended_at": "2026-01-01T00:00:00Z"},
            {"kind": "claude", "pane": null, "ended_at": null},
        ]),
    );
}

#[test]
fn supervised_run_placement_matrix() {
    use {RunPlacement, run_placement};

    for (force_new_tab, has_ambient_pane, subagent, expected) in [
        (false, true, false, RunPlacement::Split),
        (false, true, true, RunPlacement::SubagentZone),
        (false, false, false, RunPlacement::Tab),
        (false, false, true, RunPlacement::Tab),
        (true, true, false, RunPlacement::Tab),
        (true, true, true, RunPlacement::Tab),
    ] {
        assert_eq!(
            run_placement(force_new_tab, has_ambient_pane, subagent),
            expected,
            "force_new_tab={force_new_tab}, has_ambient_pane={has_ambient_pane}, subagent={subagent}"
        );
    }
}

#[test]
fn subagent_zone_strategy_uses_solo_column_and_team_companion_tab() {
    use zone::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = crate::config::ThemeConfig::default();
    let mut solo = agent_state("codex", "solo", AgentStatus::Running);
    solo.pane = Some(pane_ref("%1", "work"));
    let live = vec![solo.pane.clone().unwrap()];
    assert_eq!(
        select_subagent_zone_strategy(
            std::slice::from_ref(&solo),
            &live,
            &solo,
            "room",
            &theme,
            "work subagents"
        ),
        Some(SubagentZoneStrategy::Split {
            session_name: "room".to_owned(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
            placement: crate::mux::SplitPlacement::Directional(crate::mux::SplitDirection::Right,),
        })
    );

    let mut planner = agent_state("claude", "planner", AgentStatus::Running);
    planner.team = Some("forge".to_owned());
    planner.channel = Some("design".to_owned());
    let glyph = crate::theme::theme_glyphs(&theme)(crate::config::GlyphRole::StatusWorking);
    planner.pane = Some(pane_ref("%2", &format!("design {glyph}")));
    assert_eq!(
        select_subagent_zone_strategy(
            std::slice::from_ref(&planner),
            std::slice::from_ref(planner.pane.as_ref().unwrap()),
            &planner,
            "room",
            &theme,
            "design subagents",
        ),
        Some(SubagentZoneStrategy::CompanionTab {
            title: "design subagents".to_owned(),
        })
    );
}

#[test]
fn subagent_zone_strategy_shares_companion_across_team() {
    use zone::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = crate::config::ThemeConfig::default();
    let mut planner = agent_state("claude", "planner", AgentStatus::Running);
    planner.team = Some("forge".to_owned());
    planner.channel = Some("design".to_owned());
    planner.pane = Some(pane_ref("%1", "design"));
    let mut coder = agent_state("codex", "coder", AgentStatus::Running);
    coder.team = Some("forge".to_owned());
    coder.channel = Some("design".to_owned());
    coder.pane = Some(pane_ref("%2", "design"));
    let mut older = launched_child("child-old", &planner, "%3", 10);
    older.pane.as_mut().unwrap().session_name = "room".to_owned();
    older.pane.as_mut().unwrap().view_name = Some("design subagents".to_owned());
    let mut newer = launched_child("child-new", &coder, "%4", 20);
    newer.pane.as_mut().unwrap().view_name = Some("run codex".to_owned());
    let live = vec![
        planner.pane.clone().unwrap(),
        older.pane.clone().unwrap(),
        newer.pane.clone().unwrap(),
    ];
    let agents = vec![planner.clone(), coder, older, newer];

    assert_eq!(
        select_subagent_zone_strategy(&agents, &live, &planner, "room", &theme, "design subagents"),
        Some(SubagentZoneStrategy::CompanionStack {
            anchors: vec![("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%3"))],
            title: "design subagents 2".to_owned(),
        })
    );
}

#[test]
fn subagent_zone_strategy_reuses_legacy_parent_launch_children() {
    use zone::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = crate::config::ThemeConfig::default();
    let mut old = agent_state("codex", "OLD", AgentStatus::Idle);
    old.launch_id = Some(AgentSessionId::from("L"));
    old.ended_at = Some(jiff::Timestamp::from_second(10).unwrap());
    let mut parent = agent_state("codex", "NEW", AgentStatus::Running);
    parent.launch_id = old.launch_id.clone();
    parent.pane = Some(pane_ref("%1", "work"));
    let mut child = launched_child("child", &old, "%2", 20);
    child.pane.as_mut().unwrap().view_name = Some("work".to_owned());
    let mut wrong = launched_child("wrong-kind", &old, "%3", 30);
    wrong.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
    wrong.pane.as_mut().unwrap().view_name = Some("work".to_owned());
    let live = vec![
        parent.pane.clone().unwrap(),
        child.pane.clone().unwrap(),
        wrong.pane.clone().unwrap(),
    ];
    let agents = vec![old, parent.clone(), child, wrong];

    assert_eq!(
        select_subagent_zone_strategy(&agents, &live, &parent, "room", &theme, "work subagents"),
        Some(SubagentZoneStrategy::Split {
            session_name: "room".to_owned(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%2"),
            placement: crate::mux::SplitPlacement::Stacked,
        })
    );
}

#[test]
fn subagent_zone_strategy_uses_live_ended_child_and_skips_dead_newer_child() {
    use zone::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = crate::config::ThemeConfig::default();
    let mut parent = agent_state("codex", "parent", AgentStatus::Running);
    parent.pane = Some(pane_ref("%1", "work"));
    let mut kept = launched_child("kept", &parent, "%2", 10);
    kept.pane.as_mut().unwrap().view_name = Some("work".to_owned());
    kept.ended_at = Some(jiff::Timestamp::from_second(30).unwrap());
    let dead = launched_child("dead", &parent, "%3", 20);
    let live = vec![parent.pane.clone().unwrap(), kept.pane.clone().unwrap()];
    let agents = vec![parent.clone(), kept, dead];

    assert_eq!(
        select_subagent_zone_strategy(&agents, &live, &parent, "room", &theme, "work subagents"),
        Some(SubagentZoneStrategy::Split {
            session_name: "room".to_owned(),
            pane_id: PaneId::from_parts(MuxName::Tmux, "%2"),
            placement: crate::mux::SplitPlacement::Stacked,
        })
    );
}

#[test]
fn subagent_zone_strategy_reuses_unbound_team_companion_view() {
    use zone::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = crate::config::ThemeConfig::default();
    let glyph = crate::theme::theme_glyphs(&theme)(crate::config::GlyphRole::StatusWorking);
    let mut planner = agent_state("claude", "planner", AgentStatus::Running);
    planner.team = Some("forge".to_owned());
    planner.pane = Some(pane_ref("%1", "design"));
    let mut unbound_child = pane_ref("%2", &format!("design subagents {glyph}"));
    unbound_child.session_name = "room".to_owned();
    let mut sidebar = pane_ref("%3", "design subagents");
    sidebar.session_name = "room".to_owned();
    sidebar.command = Some(crate::pane::SIDEBAR_CHROME_TITLE.to_owned());

    assert_eq!(
        select_subagent_zone_strategy(
            std::slice::from_ref(&planner),
            &[planner.pane.clone().unwrap(), unbound_child],
            &planner,
            "room",
            &theme,
            "design subagents",
        ),
        Some(SubagentZoneStrategy::CompanionStack {
            anchors: vec![("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%2"))],
            title: "design subagents 2".to_owned(),
        })
    );
    assert_eq!(
        select_subagent_zone_strategy(
            std::slice::from_ref(&planner),
            &[planner.pane.clone().unwrap(), sidebar],
            &planner,
            "room",
            &theme,
            "design subagents",
        ),
        Some(SubagentZoneStrategy::CompanionStack {
            anchors: vec![("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%3"))],
            title: "design subagents 2".to_owned(),
        })
    );
}

#[test]
fn subagent_zone_strategy_keeps_large_companions_and_reuses_overflow() {
    use zone::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = crate::config::ThemeConfig::default();
    let mut parent = agent_state("codex", "parent", AgentStatus::Running);
    parent.team = Some("forge".to_owned());
    parent.pane = Some(pane_ref("%1", "design"));
    // The backend, not the family selector, decides whether a stack has room.
    let mut live = (2..=11)
        .map(|id| pane_ref(&format!("%{id}"), "design subagents"))
        .collect::<Vec<_>>();
    let mut sidebar = pane_ref("%14", "design subagents");
    sidebar.command = Some(crate::pane::SIDEBAR_CHROME_TITLE.to_owned());
    live.push(sidebar);
    let choose = |live: &[PaneRef]| {
        select_subagent_zone_strategy(
            std::slice::from_ref(&parent),
            live,
            &parent,
            "room",
            &theme,
            "design subagents",
        )
    };
    assert_eq!(
        choose(&live),
        Some(SubagentZoneStrategy::CompanionStack {
            anchors: vec![("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%2"))],
            title: "design subagents 2".to_owned(),
        })
    );
    live.push(pane_ref("%15", "design subagents 2"));
    // Newest generic fallback children do not hijack companion placement.
    live.push(pane_ref("%16", "run codex"));
    assert_eq!(
        choose(&live),
        Some(SubagentZoneStrategy::CompanionStack {
            anchors: vec![
                ("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%2")),
                ("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%15")),
            ],
            title: "design subagents 3".to_owned(),
        })
    );
    live.remove(0);
    assert_eq!(
        choose(&live),
        Some(SubagentZoneStrategy::CompanionStack {
            anchors: vec![
                ("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%3")),
                ("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%15")),
            ],
            title: "design subagents 3".to_owned(),
        })
    );
}

#[test]
fn subagent_launch_waits_for_wrapper_pane_bind_but_caps_the_wait() {
    let mut child = agent_state("claude", "child", AgentStatus::Success);
    child.pane = Some(pane_ref("%2", "subagents"));
    child.launch_id = Some("launch_child".into());
    let kind = child.kind.clone();
    let launch_id = child.launch_id.clone().unwrap();
    assert!(zone::launch_has_bound_pane(
        std::slice::from_ref(&child),
        &kind,
        &launch_id
    ));
    child.ended_at = Some(jiff::Timestamp::now());
    assert!(!zone::launch_has_bound_pane(&[child], &kind, &launch_id));
    let mut probes = 0;
    assert!(zone::wait_for_subagent_pane_bind_with(
        || {
            probes += 1;
            probes == 3
        },
        Duration::from_secs(1),
        Duration::ZERO,
    ));
    assert_eq!(probes, 3);
    assert!(!zone::wait_for_subagent_pane_bind_with(
        || false,
        Duration::ZERO,
        Duration::ZERO,
    ));
}

#[test]
fn subagent_zone_caller_failure_opens_channel_companion() {
    assert_zone_fallback("caller");
}

#[test]
fn subagent_zone_pane_listing_failure_opens_channel_companion() {
    assert_zone_fallback("pane_listing");
}

#[test]
fn subagent_zone_projection_failure_opens_channel_companion() {
    assert_zone_fallback("projection");
}

#[test]
fn subagent_zone_missing_anchor_and_split_failure_open_channel_companion() {
    for split in [false, true] {
        let fixture = RunFixture::new(RunStatus::Running);
        let parent = zone_backend::register_caller(&fixture);
        let backend = zone_backend::Backend {
            listed: Some(if split { vec![parent] } else { vec![] }),
            ..Default::default()
        };
        let result = zone_backend::open(&fixture, &backend);
        let tabs = backend.tabs.lock().unwrap();
        assert_eq!(tabs.len(), 1, "must open a companion: {result:?}");
        assert_eq!(tabs[0].title, "#ch subagents");
        assert!(matches!(result, zone::SubagentZoneOpen::Opened));
    }
}

#[test]
fn subagent_zone_ambiguous_split_never_opens_a_second_pane() {
    for error in [
        crate::mux::MuxErr::Timeout {
            program: "fake".to_owned(),
            args: "split".to_owned(),
            timeout: Duration::from_secs(1),
        },
        crate::mux::MuxErr::Output {
            program: "fake".to_owned(),
            reason: "unconfirmed pane".to_owned(),
        },
    ] {
        let fixture = RunFixture::new(RunStatus::Running);
        let parent = zone_backend::register_caller(&fixture);
        let backend = zone_backend::Backend {
            listed: Some(vec![parent]),
            split_error: std::sync::Mutex::new(Some(error)),
            ..Default::default()
        };
        assert!(matches!(
            zone_backend::open(&fixture, &backend),
            zone::SubagentZoneOpen::Failed(_)
        ));
        assert!(backend.tabs.lock().unwrap().is_empty());
    }
}

#[test]
fn subagent_zone_companion_append_uses_numbered_overflow_and_definitive_errors() {
    use crate::mux::{CompanionAppendErr, CompanionPaneAppend, MuxErr};

    for (outcomes, attempts, overflow) in [
        (vec![Ok(CompanionPaneAppend::Opened)], 1, false),
        (
            vec![Ok(CompanionPaneAppend::Full), Ok(CompanionPaneAppend::Full)],
            2,
            true,
        ),
        (
            vec![
                Err(CompanionAppendErr::Spawn(MuxErr::Command {
                    program: "fake".into(),
                    args: "append".into(),
                    stderr: "no room".into(),
                })),
                Ok(CompanionPaneAppend::Opened),
            ],
            2,
            false,
        ),
    ] {
        let fixture = RunFixture::new(RunStatus::Running);
        let parent = zone_backend::register_caller(&fixture);
        let backend = zone_backend::Backend {
            listed: Some(vec![
                parent,
                pane_ref("%20", "#ch subagents"),
                pane_ref("%21", "#ch subagents 2"),
            ]),
            append_outcomes: std::sync::Mutex::new(outcomes.into()),
            ..Default::default()
        };
        let result = zone_backend::open(&fixture, &backend);
        assert!(
            matches!(result, zone::SubagentZoneOpen::Opened),
            "{result:?}"
        );
        assert_eq!(backend.appends.lock().unwrap().len(), attempts);
        let tabs = backend.tabs.lock().unwrap();
        assert_eq!(tabs.len(), usize::from(overflow));
        if overflow {
            assert_eq!(tabs[0].title, "#ch subagents 3");
        }
    }
}

#[test]
fn subagent_zone_ambiguous_append_never_opens_a_second_pane() {
    for error in [
        crate::mux::MuxErr::Timeout {
            program: "fake".into(),
            args: "append".into(),
            timeout: Duration::from_secs(1),
        },
        crate::mux::MuxErr::Output {
            program: "fake".into(),
            reason: "unconfirmed pane".into(),
        },
    ] {
        let fixture = RunFixture::new(RunStatus::Running);
        let parent = zone_backend::register_caller(&fixture);
        let backend = zone_backend::Backend {
            listed: Some(vec![
                parent,
                pane_ref("%20", "#ch subagents"),
                pane_ref("%21", "#ch subagents 2"),
            ]),
            append_outcomes: std::sync::Mutex::new(
                [
                    Err(crate::mux::CompanionAppendErr::Spawn(error)),
                    Ok(crate::mux::CompanionPaneAppend::Opened),
                ]
                .into(),
            ),
            ..Default::default()
        };
        assert!(matches!(
            zone_backend::open(&fixture, &backend),
            zone::SubagentZoneOpen::Failed(_)
        ));
        assert_eq!(backend.appends.lock().unwrap().len(), 1);
        assert!(backend.tabs.lock().unwrap().is_empty());
    }
}

#[test]
fn subagent_zone_before_spawn_timeout_opens_numbered_companion() {
    assert_zone_before_spawn_fallback(crate::mux::MuxErr::Timeout {
        program: "fake".into(),
        args: "append lookup".into(),
        timeout: Duration::from_secs(1),
    });
}

#[test]
fn subagent_zone_before_spawn_output_error_opens_numbered_companion() {
    assert_zone_before_spawn_fallback(crate::mux::MuxErr::Output {
        program: "fake".into(),
        reason: "unreadable geometry".into(),
    });
}

#[test]
fn subagent_zone_before_spawn_command_error_opens_numbered_companion() {
    assert_zone_before_spawn_fallback(crate::mux::MuxErr::Command {
        program: "fake".into(),
        args: "append lookup".into(),
        stderr: "pane disappeared".into(),
    });
}

fn assert_zone_before_spawn_fallback(error: crate::mux::MuxErr) {
    let fixture = RunFixture::new(RunStatus::Running);
    let parent = zone_backend::register_caller(&fixture);
    let after = parent.pane_id.clone();
    let backend = zone_backend::Backend {
        listed: Some(vec![
            parent,
            pane_ref("%20", "#ch subagents"),
            pane_ref("%21", "#ch subagents 2"),
        ]),
        append_outcomes: std::sync::Mutex::new(
            [
                Err(crate::mux::CompanionAppendErr::BeforeSpawn(error)),
                Ok(crate::mux::CompanionPaneAppend::Opened),
            ]
            .into(),
        ),
        ..Default::default()
    };
    let result = zone_backend::open(&fixture, &backend);
    assert!(
        matches!(result, zone::SubagentZoneOpen::Opened),
        "{result:?}"
    );
    assert_eq!(backend.appends.lock().unwrap().len(), 1);
    let tabs = backend.tabs.lock().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].title, "#ch subagents 3");
    assert_eq!(tabs[0].after, Some(after));
    assert!(!tabs[0].focus);
    assert_zone_lookup_record(&fixture, "companion_stack");
}

#[test]
fn subagent_zone_failed_tab_is_not_retried() {
    let fixture = RunFixture::new(RunStatus::Running);
    let backend = zone_backend::Backend {
        tab_error: std::sync::Mutex::new(Some(crate::mux::MuxErr::Output {
            program: "fake".to_owned(),
            reason: "unconfirmed tab".to_owned(),
        })),
        ..Default::default()
    };
    let result = zone_backend::open(&fixture, &backend);
    assert_eq!(
        backend.tabs.lock().unwrap().len(),
        1,
        "must attempt exactly one tab: {result:?}"
    );
    assert!(matches!(result, zone::SubagentZoneOpen::Failed(_)));
}

#[test]
fn subagent_companion_channel_title_needs_no_caller() {
    let fixture = RunFixture::new(RunStatus::Running);
    assert_eq!(
        zone::subagent_companion_title(&fixture.store, Some("ch")),
        "#ch subagents"
    );
    assert_eq!(
        zone::subagent_companion_title(&fixture.store, None),
        "subagents"
    );
}

#[test]
fn supervised_tab_titles_distinguish_children_from_plain_print_runs() {
    let fixture = RunFixture::new(RunStatus::Running);
    let kind = AgentKind::new_unchecked("codex");
    for (subagent, channel, expected) in [
        (false, None, "run codex"),
        (false, Some("ch"), "run codex"),
        (true, Some("ch"), "#ch subagents"),
        (true, None, "subagents"),
    ] {
        assert_eq!(
            run_tab_title(&fixture.store, channel, subagent, &kind),
            expected
        );
    }
}

#[test]
fn subagent_zone_strategy_uses_channel_family_instead_of_parent_view() {
    use zone::{SubagentZoneStrategy, select_subagent_zone_strategy};
    let theme = crate::config::ThemeConfig::default();
    let mut parent = agent_state("codex", "parent", AgentStatus::Running);
    parent.team = Some("forge".to_owned());
    parent.pane = Some(pane_ref("%1", "different-view"));
    let live = vec![
        parent.pane.clone().unwrap(),
        pane_ref("%2", "#ch subagents"),
    ];
    assert_eq!(
        select_subagent_zone_strategy(
            &[parent.clone()],
            &live,
            &parent,
            "room",
            &theme,
            "#ch subagents"
        ),
        Some(SubagentZoneStrategy::CompanionStack {
            anchors: vec![("room".to_owned(), PaneId::parse("tmux:%2").unwrap())],
            title: "#ch subagents 2".to_owned(),
        })
    );
}

#[test]
fn resumed_registration_satisfies_the_bind_wait() {
    use crate::agents::{AgentLifecycleObservation, LifecycleSignal};
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let store = crate::Store::open(
        StatePaths::under(workspace.clone(), dir.path()).unwrap(),
        RuntimePaths::under(workspace.clone(), &dir.path().join("rt")).unwrap(),
    )
    .unwrap();
    let kind = AgentKind::new_unchecked("codex");
    let id = AgentSessionId::from("child");
    let launch_id = AgentSessionId::from("launch_child");
    let stamp = |name, signal| {
        store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                workspace.clone(),
                "room",
                kind.as_str(),
                name,
                &AgentLifecycleObservation::new(Some(id.clone()), signal),
            ))
            .unwrap();
    };
    stamp("SessionStart", LifecycleSignal::Registered);
    stamp("SessionEnd", LifecycleSignal::Ended);
    store
        .attach_agent_pane(
            &kind,
            &id,
            Some(&launch_id),
            &crate::ids::LoginName::default(),
            "room",
            &PaneId::parse("tmux:%2").unwrap(),
            crate::store::runtime::current_process_owner(
                crate::pane::RuntimeOwnerKind::Agent,
                "child",
            ),
            None,
            None,
            None,
        )
        .unwrap();
    let bound = || {
        zone::launch_has_bound_pane(
            &store
                .runtime_projection(crate::RuntimeScope::Audit)
                .unwrap()
                .agents,
            &kind,
            &launch_id,
        )
    };
    assert!(!bound());
    stamp("rimz.agent-resumed", LifecycleSignal::Registered);
    assert!(zone::wait_for_subagent_pane_bind_with(
        bound,
        Duration::ZERO,
        Duration::ZERO
    ));
}

#[test]
fn subagent_zone_lock_serializes_workspace_launches() {
    let fixture = RunFixture::new(RunStatus::Running);
    let lock_path = fixture
        .store
        .runtime_paths()
        .lock_path("subagent-zone.lock");

    let held = zone::lock_subagent_zone(&fixture.store).unwrap();
    assert!(
        crate::disk::lock::WorkspaceLock::try_acquire(&lock_path)
            .unwrap()
            .is_none()
    );
    drop(held);
    assert!(
        crate::disk::lock::WorkspaceLock::try_acquire(&lock_path)
            .unwrap()
            .is_some()
    );
}

fn assert_bind_timeout_last_read(
    read: Result<Vec<AgentState>, String>,
    expected: serde_json::Value,
) {
    let fixture = RunFixture::new(RunStatus::Running);
    let sink = crate::diag::DiagSink::under(
        fixture.paths.root.clone(),
        fixture.workspace_id.clone(),
        "room",
        None,
    );
    let kind = AgentKind::new_unchecked("codex");
    let launch_id = AgentSessionId::from("launch_child");
    assert!(
        !zone::PaneBindWait {
            run_id: &fixture.run_id(),
            launch_id: &launch_id,
            kind: &kind,
            caller: crate::diag::record::SubagentPaneBindCaller::Resume,
            open: Duration::from_millis(17),
        }
        .wait_with(
            &sink,
            || read.clone(),
            Duration::from_millis(1),
            Duration::from_millis(1)
        )
    );
    let log = std::fs::read_to_string(sink.log_path().unwrap()).unwrap_or_default();
    assert!(!log.is_empty(), "a bind timeout must emit durable evidence");
    let record: serde_json::Value = serde_json::from_str(log.trim()).unwrap();
    assert_eq!(record["severity"], "warn");
    let record = &record["event"];
    assert_eq!(record["kind"], "subagent_pane_bind_timeout");
    assert_eq!(record["run_id"], fixture.run_id().to_string());
    assert_eq!(record["launch_id"], "launch_child");
    assert_eq!(record["agent_kind"], "codex");
    assert_eq!(record["caller"], "resume");
    assert_eq!(record["open_ms"], 17);
    assert!(record["waited_ms"].as_u64().unwrap() >= 1);
    assert!(record["polls"].as_u64().unwrap() >= 1);
    assert_eq!(record["last_read"], expected);
}

fn assert_zone_fallback(lookup: &str) {
    let fixture = RunFixture::new(RunStatus::Running);
    let after = if lookup == "pane_listing" {
        zone_backend::register_caller(&fixture).pane_id
    } else {
        PaneId::parse("tmux:%99").unwrap()
    };
    if lookup == "projection" {
        if fixture.paths.events_log.is_file() {
            std::fs::remove_file(&fixture.paths.events_log).unwrap();
        }
        std::fs::create_dir_all(&fixture.paths.events_log).unwrap();
    }
    let backend = zone_backend::Backend::default();
    let result = zone_backend::open(&fixture, &backend);
    let tabs = backend.tabs.lock().unwrap();
    assert_eq!(
        tabs.len(),
        1,
        "lookup {lookup} must open a companion: {result:?}"
    );
    assert_eq!(tabs[0].title, "#ch subagents");
    assert_eq!(tabs[0].after, Some(after));
    assert!(!tabs[0].focus);
    assert!(matches!(result, zone::SubagentZoneOpen::Opened));
    assert_zone_lookup_record(&fixture, lookup);
}

fn assert_zone_lookup_record(fixture: &RunFixture, lookup: &str) {
    let sink = crate::diag::DiagSink::under(
        fixture.paths.root.clone(),
        fixture.workspace_id.clone(),
        "room",
        None,
    );
    let records = std::fs::read_to_string(sink.log_path().unwrap()).unwrap();
    let record: serde_json::Value = serde_json::from_str(records.trim()).unwrap();
    assert_eq!(record["severity"], "warn");
    assert_eq!(record["event"]["kind"], "subagent_zone_lookup_failed");
    assert_eq!(record["event"]["lookup"], lookup);
    assert_eq!(record["event"]["run_id"], fixture.run_id().to_string());
    assert!(!record["event"]["error"].as_str().unwrap().is_empty());
}

fn agent_state(kind: &str, id: &str, status: AgentStatus) -> AgentState {
    AgentState {
        status,
        ..crate::testkit::agent_state(kind, id, jiff::Timestamp::UNIX_EPOCH)
    }
}

fn pane_ref(id: &str, view_name: &str) -> PaneRef {
    let mut pane = PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, id));
    pane.view_name = Some(view_name.to_owned());
    pane
}

fn launched_child(id: &str, parent: &AgentState, pane_id: &str, registered_at: i64) -> AgentState {
    let mut child = agent_state("codex", id, AgentStatus::Running);
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.pane = Some(pane_ref(pane_id, "subagents"));
    child.registered_at = Some(jiff::Timestamp::from_second(registered_at).unwrap());
    child
}

use crate::launcher::test_backend as fake_backend;

mod zone_backend {
    pub(super) use super::fake_backend::Backend;
    use super::*;
    use crate::mux as m;

    pub(super) fn register_caller(fixture: &RunFixture) -> PaneRef {
        let identity = crate::harness::ancestry::CallerIdentity::from_env();
        let kind = identity.as_ref().map_or_else(
            || AgentKind::new_unchecked("codex"),
            |caller| caller.kind.clone(),
        );
        let id = identity
            .as_ref()
            .and_then(|caller| caller.launch_id.clone())
            .unwrap_or_else(|| "parent".into());
        let pane = identity
            .and_then(|caller| caller.pane_id)
            .unwrap_or_else(|| PaneId::parse("tmux:%1").unwrap());
        fixture
            .store
            .append_event(&crate::EventEnvelope::agent_lifecycle(
                fixture.workspace_id.clone(),
                "room",
                kind.as_str(),
                "SessionStart",
                &crate::agents::AgentLifecycleObservation::new(
                    Some(id.clone()),
                    crate::agents::LifecycleSignal::Registered,
                ),
            ))
            .unwrap();
        let owner_pid = std::os::unix::process::parent_id();
        fixture
            .store
            .attach_agent_pane(
                &kind,
                &id,
                Some(&id),
                &crate::ids::LoginName::default(),
                "room",
                &pane,
                crate::pane::RuntimeOwner::new(
                    crate::pane::RuntimeOwnerKind::Agent,
                    "parent",
                    owner_pid,
                    crate::proc::process_start_token(owner_pid),
                ),
                None,
                None,
                None,
            )
            .unwrap();
        let projection = fixture
            .store
            .runtime_projection(crate::RuntimeScope::Audit)
            .unwrap();
        crate::harness::ancestry::resolve_calling_agent(&projection.agents)
            .unwrap()
            .pane
            .clone()
            .unwrap()
    }

    pub(super) fn open(fixture: &RunFixture, backend: &Backend) -> super::zone::SubagentZoneOpen {
        let cwd = fixture._dir.path();
        let workspace = crate::ResolvedWorkspace {
            workspace_id: fixture.workspace_id.clone(),
            project_root: cwd.to_owned(),
            cwd_project_root: None,
            root_class: crate::workspace::RootClass::Directory,
            worktree_root: cwd.to_owned(),
            worktree_branch: None,
            session_name: "room".to_owned(),
            mux_hint: Some(MuxName::Tmux),
        };
        let sidebar = m::SidebarPaneOptions {
            runtime: fixture.runtime.clone(),
            session_name: "room".to_owned(),
            workspace_id: fixture.workspace_id.clone(),
            project_root: cwd.to_owned(),
            extra_env: Default::default(),
            cwd: cwd.to_owned(),
            target: m::SidebarTarget {
                share: m::WidthPermille::try_from(200).unwrap(),
                max_cols: std::num::NonZeroU16::new(40).unwrap(),
                pinned: false,
            },
            detected_view_size: None,
            rimz_bin: "rimz".into(),
            pristine_birth: false,
            config: Default::default(),
            resume_tabs: vec![],
            refresh_ms: None,
        };
        super::zone::split_into_subagent_zone(
            backend,
            &fixture.store,
            &workspace,
            cwd,
            Default::default(),
            sidebar,
            &m::PaneCmd {
                argv: vec!["child".to_owned()],
                name: Some("child".to_owned()),
            },
            "child",
            Some("ch"),
            Some(PaneId::parse("tmux:%99").unwrap()),
            &fixture.run_id(),
        )
    }
}

struct RunFixture {
    _dir: tempfile::TempDir,
    workspace_id: WorkspaceId,
    paths: StatePaths,
    runtime: RuntimePaths,
    store: crate::Store,
    record: RunRecord,
}
impl RunFixture {
    fn new(status: RunStatus) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("rs")
            .tempdir_in("/tmp")
            .unwrap();
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
        let runtime = RuntimePaths::under(workspace_id.clone(), dir.path()).unwrap();
        paths.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        let store = crate::Store::open(paths.clone(), runtime.clone()).unwrap();
        let mut record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = status;
        record.completed_at = status.is_terminal().then_some(record.started_at);
        crate::harness::run::create(&paths, &record).unwrap();
        Self {
            _dir: dir,
            workspace_id,
            paths,
            runtime,
            store,
            record,
        }
    }

    fn run_id(&self) -> crate::RunId {
        self.record.run_id.clone()
    }
}
