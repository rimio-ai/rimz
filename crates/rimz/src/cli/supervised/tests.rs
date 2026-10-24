use super::*;
use rimz::agents::PermissionMode;
use rimz::agents::{AgentState, AgentStatus, LaunchParams};
use rimz::disk::paths::{RuntimePaths, StatePaths};
use rimz::harness::run::{RunCancellation, SupervisedRunRequest};
use rimz::harness::run_wake::{self, ExpectedRunFrame};
use rimz::ids::{AgentKind, AgentSessionId, MuxName, PaneId, WorkspaceId};
use rimz::pane::PaneRef;
use rimz::store::run::{ReportTo, RunStatus, WakeupFrame};
use tokio::net::UnixDatagram;

fn assert_bind_timeout_last_read(
    read: Result<Vec<AgentState>, String>,
    expected: serde_json::Value,
) {
    let fixture = RunFixture::new(RunStatus::Running);
    let sink = rimz::diag::DiagSink::under(
        fixture.paths.root.clone(),
        fixture.workspace_id.clone(),
        "room",
        None,
    );
    let kind = AgentKind::new_unchecked("codex");
    let launch_id = AgentSessionId::from("launch_child");
    assert!(
        !super::pane::PaneBindWait {
            run_id: &fixture.run_id(),
            launch_id: &launch_id,
            kind: &kind,
            caller: rimz::diag::record::SubagentPaneBindCaller::Resume,
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
fn stream_json_prompt_concatenates_user_message_text() {
    // String content and text-block content both contribute; non-user
    // envelopes (assistant, system) are ignored.
    let input = "\
{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"first line\"}}
{\"type\":\"assistant\",\"message\":{\"content\":\"ignored\"}}
{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"second line\"},{\"type\":\"image\"}]}}
";
    let prompt = read_stream_json_prompt(std::io::Cursor::new(input)).expect("parse stream-json");
    assert_eq!(prompt, "first line\nsecond line");
}

#[test]
fn stream_json_prompt_rejects_malformed_lines() {
    let err = read_stream_json_prompt(std::io::Cursor::new("not json\n"))
        .expect_err("malformed stream-json line fails");
    assert!(err.to_string().contains("stream-json line"), "{err:#}");
}

#[test]
fn supervised_run_placement_matrix() {
    use super::run::{RunPlacement, run_placement};

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
    use super::pane::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
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
            placement: rimz::mux::SplitPlacement::Directional(rimz::mux::SplitDirection::Right,),
        })
    );

    let mut planner = agent_state("claude", "planner", AgentStatus::Running);
    planner.team = Some("forge".to_owned());
    planner.channel = Some("design".to_owned());
    let glyph = rimz::theme::theme_glyphs(&theme)(rimz::config::GlyphRole::StatusWorking);
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
    use super::pane::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
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
    use super::pane::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
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
            placement: rimz::mux::SplitPlacement::Stacked,
        })
    );
}

#[test]
fn subagent_zone_strategy_uses_live_ended_child_and_skips_dead_newer_child() {
    use super::pane::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
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
            placement: rimz::mux::SplitPlacement::Stacked,
        })
    );
}

#[test]
fn subagent_zone_strategy_reuses_unbound_team_companion_view() {
    use super::pane::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
    let glyph = rimz::theme::theme_glyphs(&theme)(rimz::config::GlyphRole::StatusWorking);
    let mut planner = agent_state("claude", "planner", AgentStatus::Running);
    planner.team = Some("forge".to_owned());
    planner.pane = Some(pane_ref("%1", "design"));
    let mut unbound_child = pane_ref("%2", &format!("design subagents {glyph}"));
    unbound_child.session_name = "room".to_owned();
    let mut sidebar = pane_ref("%3", "design subagents");
    sidebar.session_name = "room".to_owned();
    sidebar.command = Some(rimz::pane::SIDEBAR_CHROME_TITLE.to_owned());

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
    use super::pane::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
    let mut parent = agent_state("codex", "parent", AgentStatus::Running);
    parent.team = Some("forge".to_owned());
    parent.pane = Some(pane_ref("%1", "design"));
    // The backend, not the family selector, decides whether a stack has room.
    let mut live = (2..=11)
        .map(|id| pane_ref(&format!("%{id}"), "design subagents"))
        .collect::<Vec<_>>();
    let mut sidebar = pane_ref("%14", "design subagents");
    sidebar.command = Some(rimz::pane::SIDEBAR_CHROME_TITLE.to_owned());
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
    assert!(super::pane::launch_has_bound_pane(
        std::slice::from_ref(&child),
        &kind,
        &launch_id
    ));
    child.ended_at = Some(jiff::Timestamp::now());
    assert!(!super::pane::launch_has_bound_pane(
        &[child],
        &kind,
        &launch_id
    ));
    let mut probes = 0;
    assert!(super::pane::wait_for_subagent_pane_bind_with(
        || {
            probes += 1;
            probes == 3
        },
        Duration::from_secs(1),
        Duration::ZERO,
    ));
    assert_eq!(probes, 3);
    assert!(!super::pane::wait_for_subagent_pane_bind_with(
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
        assert!(matches!(result, super::pane::SubagentZoneOpen::Opened));
    }
}

#[test]
fn subagent_zone_ambiguous_split_never_opens_a_second_pane() {
    for error in [
        rimz::mux::MuxErr::Timeout {
            program: "fake".to_owned(),
            args: "split".to_owned(),
            seconds: 1,
        },
        rimz::mux::MuxErr::Output {
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
            super::pane::SubagentZoneOpen::Failed(_)
        ));
        assert!(backend.tabs.lock().unwrap().is_empty());
    }
}

#[test]
fn subagent_zone_companion_append_uses_numbered_overflow_and_definitive_errors() {
    use rimz::mux::{CompanionAppendErr, CompanionPaneAppend, MuxErr};

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
            matches!(result, super::pane::SubagentZoneOpen::Opened),
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
        rimz::mux::MuxErr::Timeout {
            program: "fake".into(),
            args: "append".into(),
            seconds: 1,
        },
        rimz::mux::MuxErr::Output {
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
                    Err(rimz::mux::CompanionAppendErr::Spawn(error)),
                    Ok(rimz::mux::CompanionPaneAppend::Opened),
                ]
                .into(),
            ),
            ..Default::default()
        };
        assert!(matches!(
            zone_backend::open(&fixture, &backend),
            super::pane::SubagentZoneOpen::Failed(_)
        ));
        assert_eq!(backend.appends.lock().unwrap().len(), 1);
        assert!(backend.tabs.lock().unwrap().is_empty());
    }
}

#[test]
fn subagent_zone_before_spawn_timeout_opens_numbered_companion() {
    assert_zone_before_spawn_fallback(rimz::mux::MuxErr::Timeout {
        program: "fake".into(),
        args: "append lookup".into(),
        seconds: 1,
    });
}

#[test]
fn subagent_zone_before_spawn_output_error_opens_numbered_companion() {
    assert_zone_before_spawn_fallback(rimz::mux::MuxErr::Output {
        program: "fake".into(),
        reason: "unreadable geometry".into(),
    });
}

#[test]
fn subagent_zone_before_spawn_command_error_opens_numbered_companion() {
    assert_zone_before_spawn_fallback(rimz::mux::MuxErr::Command {
        program: "fake".into(),
        args: "append lookup".into(),
        stderr: "pane disappeared".into(),
    });
}

fn assert_zone_before_spawn_fallback(error: rimz::mux::MuxErr) {
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
                Err(rimz::mux::CompanionAppendErr::BeforeSpawn(error)),
                Ok(rimz::mux::CompanionPaneAppend::Opened),
            ]
            .into(),
        ),
        ..Default::default()
    };
    let result = zone_backend::open(&fixture, &backend);
    assert!(
        matches!(result, super::pane::SubagentZoneOpen::Opened),
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
        tab_error: std::sync::Mutex::new(Some(rimz::mux::MuxErr::Output {
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
    assert!(matches!(result, super::pane::SubagentZoneOpen::Failed(_)));
}

#[test]
fn failure_tail_keeps_scrollback_for_collapsed_members() {
    let backend = zone_backend::Backend::default();
    let pane = PaneId::parse("tmux:%1").unwrap();
    assert_eq!(
        super::pane::capture_failure_tail(&backend, &pane, "room").as_deref(),
        Some("earlier-line\nlast-line"),
    );
    assert_eq!(
        backend.captures.lock().unwrap().as_slice(),
        &[(Some(60), false)]
    );
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
    assert!(matches!(result, super::pane::SubagentZoneOpen::Opened));
    assert_zone_lookup_record(&fixture, lookup);
}

fn assert_zone_lookup_record(fixture: &RunFixture, lookup: &str) {
    let sink = rimz::diag::DiagSink::under(
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

#[test]
fn subagent_companion_channel_title_needs_no_caller() {
    let fixture = RunFixture::new(RunStatus::Running);
    assert_eq!(
        super::pane::subagent_companion_title(&fixture.store, Some("ch")),
        "#ch subagents"
    );
    assert_eq!(
        super::pane::subagent_companion_title(&fixture.store, None),
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
            super::run::run_tab_title(&fixture.store, channel, subagent, &kind),
            expected
        );
    }
}

#[test]
fn subagent_zone_strategy_uses_channel_family_instead_of_parent_view() {
    use super::pane::{SubagentZoneStrategy, select_subagent_zone_strategy};
    let theme = rimz::config::ThemeConfig::default();
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

mod zone_backend {
    use super::*;
    use rimz::mux as m;
    use std::sync::Mutex;

    #[derive(Default)]
    pub(super) struct Backend {
        pub(super) tabs: Mutex<Vec<m::TabOptions>>,
        pub(super) listed: Option<Vec<PaneRef>>,
        pub(super) split_error: Mutex<Option<m::MuxErr>>,
        pub(super) tab_error: Mutex<Option<m::MuxErr>>,
        pub(super) appends: Mutex<Vec<m::SplitPaneOptions>>,
        pub(super) append_outcomes: Mutex<
            std::collections::VecDeque<Result<m::CompanionPaneAppend, m::CompanionAppendErr>>,
        >,
        pub(super) captures: Mutex<Vec<(Option<u16>, bool)>>,
    }

    impl m::MuxBackend for Backend {
        fn view_geometry(
            &self,
            _session: &str,
            _anchor: &PaneId,
            _timeout: Duration,
        ) -> m::Result<Option<m::ViewGeometry>> {
            panic!("unexpected view_geometry")
        }

        fn name(&self) -> MuxName {
            MuxName::Tmux
        }
        fn list_panes(&self, opts: m::PaneListOptions) -> m::Result<m::PaneListing> {
            assert_eq!(opts.command_timeout, Some(Duration::from_millis(500)));
            assert_eq!(
                opts.consistency,
                m::PaneReadConsistency::RequireAuthoritative
            );
            if let Some(panes) = &self.listed {
                return Ok(m::PaneListing {
                    authoritative: true,
                    panes: panes.clone(),
                    views: Vec::new(),
                    observed_at_ms: 0,
                    session_focus: None,
                    client_view: None,
                    client_sample_withheld: false,
                });
            }
            Err(m::MuxErr::Timeout {
                program: "fake".to_owned(),
                args: "list-panes".to_owned(),
                seconds: 1,
            })
        }
        fn split_pane(&self, _opts: m::SplitPaneOptions) -> m::Result<Option<PaneId>> {
            Err(self
                .split_error
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| m::MuxErr::Command {
                    program: "fake".to_owned(),
                    args: "split".to_owned(),
                    stderr: "no room".to_owned(),
                }))
        }
        fn open_tab(&self, opts: &m::TabOptions) -> m::Result<()> {
            self.tabs.lock().unwrap().push(opts.clone());
            if let Some(error) = self.tab_error.lock().unwrap().take() {
                return Err(error);
            }
            Ok(())
        }
        fn ensure_session(&self, _opts: &m::SessionOptions) -> m::Result<()> {
            panic!("unexpected ensure_session")
        }
        fn attach_command(
            &self,
            _name: &str,
            _config: &rimz::config::MultiplexerConfig,
        ) -> m::CommandSpec {
            panic!("unexpected attach_command")
        }
        fn attach_existing_command(&self, _name: &str) -> m::CommandSpec {
            panic!("unexpected attach_existing_command")
        }
        fn attach_readonly_command(&self, _name: &str) -> m::CommandSpec {
            panic!("unexpected attach_readonly_command")
        }
        fn detach(&self, _name: &str) -> m::Result<()> {
            panic!("unexpected detach")
        }
        fn kill_session(&self, _name: &str) -> m::Result<()> {
            panic!("unexpected kill_session")
        }
        fn list_sessions_within(&self, _timeout: Duration) -> m::Result<Vec<String>> {
            panic!("unexpected list_sessions_within")
        }
        fn pane_content_size(
            &self,
            _pane: &PaneId,
            _session: Option<&str>,
            _timeout: Duration,
        ) -> m::Result<Option<m::PaneContentSize>> {
            panic!("unexpected pane_content_size")
        }
        fn append_companion_pane(
            &self,
            opts: m::SplitPaneOptions,
        ) -> Result<m::CompanionPaneAppend, m::CompanionAppendErr> {
            self.appends.lock().unwrap().push(opts);
            self.append_outcomes
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted append")
        }
        fn focus_pane(&self, _pane: &PaneId, _session: Option<&str>) -> m::Result<()> {
            panic!("unexpected focus_pane")
        }
        fn toggle_fullscreen(&self, _pane: &PaneId, _session: Option<&str>) -> m::Result<()> {
            panic!("unexpected toggle_fullscreen")
        }
        fn sidebar_width_step(
            &self,
            _runtime: &RuntimePaths,
            _session: &str,
            _pane: &PaneId,
            _min_observed_at_ms: Option<u64>,
        ) -> m::Result<m::WidthStep> {
            panic!("unexpected sidebar_width_step")
        }
        fn nudge_sidebar_width(
            &self,
            _session: &str,
            _pane: &PaneId,
            _current_cols: u16,
            _target_cols: u16,
        ) -> m::Result<()> {
            panic!("unexpected nudge_sidebar_width")
        }
        fn record_sidebar_width_default(&self, _session: &str, _cols: u16) -> m::Result<()> {
            panic!("unexpected record_sidebar_width_default")
        }
        fn capture_pane(
            &self,
            pane: &PaneId,
            _session: &str,
            lines: Option<u16>,
            ansi: bool,
        ) -> m::Result<m::PaneCapture> {
            self.captures.lock().unwrap().push((lines, ansi));
            let raw_text = if lines.is_some() {
                "earlier-line\nlast-line\n"
            } else {
                "last-line\n"
            };
            Ok(m::PaneCapture {
                pane_id: pane.clone(),
                raw_text: raw_text.to_owned(),
                lines: raw_text.lines().map(str::to_owned).collect(),
            })
        }
        fn send_keys(&self, _pane: &PaneId, _session: &str, _text: &str) -> m::Result<()> {
            panic!("unexpected send_keys")
        }
        fn send_key(
            &self,
            _pane: &PaneId,
            _session: &str,
            _key: rimz::pane::keys::NamedKey,
        ) -> m::Result<()> {
            panic!("unexpected send_key")
        }
        fn paste_text(&self, _pane: &PaneId, _session: &str, _text: &str) -> m::Result<()> {
            panic!("unexpected paste_text")
        }
        fn open_sidebar(
            &self,
            _opts: &m::SidebarPaneOptions,
            _daemon: Option<&m::DaemonView>,
        ) -> m::Result<()> {
            panic!("unexpected open_sidebar")
        }
        fn reconcile_sidebars(
            &self,
            _opts: &m::SidebarPaneOptions,
            _live: &dyn m::SidebarLivenessSource,
        ) -> m::Result<m::SidebarRecovery> {
            panic!("unexpected reconcile_sidebars")
        }
        fn open_background_view(
            &self,
            _opts: &m::BackgroundViewOptions,
        ) -> m::Result<m::BackgroundViewLaunch> {
            panic!("unexpected open_background_view")
        }
        fn confirm_resume_tabs(
            &self,
            _session: &str,
            _tabs: &[m::ResumeTab],
        ) -> Vec<std::result::Result<(), m::ResumeTabUnconfirmed>> {
            panic!("unexpected confirm_resume_tabs")
        }
        fn rename_tab(
            &self,
            _session: &str,
            _anchor: &PaneId,
            _name: &str,
            _intent: m::tab_name::TabNameIntent,
        ) -> m::Result<()> {
            panic!("unexpected rename_tab")
        }
        fn close_pane(&self, _session: &str, _pane: &PaneId) -> m::Result<()> {
            panic!("unexpected close_pane")
        }
        fn version(&self) -> m::Result<String> {
            panic!("unexpected version")
        }
    }

    pub(super) fn register_caller(fixture: &RunFixture) -> PaneRef {
        let identity = rimz::harness::ancestry::CallerIdentity::from_env();
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
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                fixture.workspace_id.clone(),
                "room",
                kind.as_str(),
                "SessionStart",
                &rimz::agents::AgentLifecycleObservation::new(
                    Some(id.clone()),
                    rimz::agents::LifecycleSignal::Registered,
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
                &rimz::ids::LoginName::default(),
                "room",
                &pane,
                rimz::pane::RuntimeOwner::new(
                    rimz::pane::RuntimeOwnerKind::Agent,
                    "parent",
                    owner_pid,
                    rimz::proc::process_start_token(owner_pid),
                ),
                None,
                None,
                None,
            )
            .unwrap();
        let projection = fixture
            .store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .unwrap();
        rimz::harness::ancestry::resolve_calling_agent(&projection.agents)
            .unwrap()
            .pane
            .clone()
            .unwrap()
    }

    pub(super) fn open(
        fixture: &RunFixture,
        backend: &Backend,
    ) -> super::super::pane::SubagentZoneOpen {
        let cwd = fixture._dir.path();
        let workspace = rimz::ResolvedWorkspace {
            workspace_id: fixture.workspace_id.clone(),
            project_root: cwd.to_owned(),
            cwd_project_root: None,
            root_class: rimz::workspace::RootClass::Directory,
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
        super::super::pane::split_into_subagent_zone(
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

#[test]
fn resumed_registration_satisfies_the_bind_wait() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let store = rimz::Store::open(
        StatePaths::under(workspace.clone(), dir.path()).unwrap(),
        RuntimePaths::under(workspace.clone(), &dir.path().join("rt")).unwrap(),
    )
    .unwrap();
    let kind = AgentKind::new_unchecked("codex");
    let id = AgentSessionId::from("child");
    let launch_id = AgentSessionId::from("launch_child");
    let stamp = |name, signal| {
        store
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
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
            &rimz::ids::LoginName::default(),
            "room",
            &PaneId::parse("tmux:%2").unwrap(),
            rimz::store::runtime::current_process_owner(
                rimz::pane::RuntimeOwnerKind::Agent,
                "child",
            ),
            None,
            None,
            None,
        )
        .unwrap();
    let bound = || {
        super::pane::launch_has_bound_pane(
            &store
                .runtime_projection(rimz::RuntimeScope::Audit)
                .unwrap()
                .agents,
            &kind,
            &launch_id,
        )
    };
    assert!(!bound());
    stamp("rimz.agent-resumed", LifecycleSignal::Registered);
    assert!(super::pane::wait_for_subagent_pane_bind_with(
        bound,
        Duration::ZERO,
        Duration::ZERO
    ));
}

fn supervised_request(prompt: &str, subagent: bool) -> SupervisedRunRequest {
    SupervisedRunRequest {
        spec: "codex".to_owned(),
        throttle_turn: None,
        prompt: prompt.to_owned(),
        description: None,
        worktree: None,
        cwd: None,
        from_pr: None,
        channel: None,
        name: None,
        background: false,
        self_cleanup_on_completion: false,
        subagent,
        force_new_tab: false,
        permission_mode: None,
        isolation: None,
        agent: None,
        tier: None,
        model: None,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        effort: None,
        fast: None,
        budget: None,
        max_turns: None,
        timeout: None,
        warn: Vec::new(),
        grace: None,
        keep: false,
        report_to: ReportTo::Launcher,
        retries: 0,
        verify: None,
        max_attempts: None,
        loop_task: None,
        workflow: None,
        loop_reminder: None,
        passthrough: Vec::new(),
        managed_launch: rimz::agents::ManagedLaunchState::PendingResolution,
        login: rimz::store::writer::LaunchLogin::RoomDefault,
    }
}

#[test]
fn a_request_account_pin_outranks_a_subagents_parent() {
    use rimz::store::writer::LaunchLogin;
    let codex = AgentKind::new_unchecked("codex");
    let mut caller = AgentState::stub("codex", "parent", AgentStatus::Running);
    caller.login = Some("spare".parse().unwrap());
    let mut request = supervised_request("fix-it", true);
    assert_eq!(
        super::run::launch_login(&request, Some(&caller), &codex),
        LaunchLogin::Pinned("spare".parse().unwrap())
    );
    request.login = LaunchLogin::Pinned("work".parse().unwrap());
    assert_eq!(
        super::run::launch_login(&request, Some(&caller), &codex),
        request.login
    );
}

#[test]
fn supervised_selection_checks_subagent_allowlist() {
    let mut caller = AgentState::stub("claude", "parent", AgentStatus::Running);
    caller.profile = Some("planner".to_owned());
    let profiles: rimz::config::ProfilesConfig = toml::from_str(
        r#"
        [planner]
        agent = "claude"
        subagents = ["explorer"]
        "#,
    )
    .expect("profiles");
    let mut request = supervised_request("fix-it", true);
    request.spec = "explorer".to_owned();

    super::run::check_supervised_subagent_allowed(&request, Some(&caller), &profiles)
        .expect("allowed subagent");

    request.spec = "codex".to_owned();
    let error = super::run::check_supervised_subagent_allowed(&request, Some(&caller), &profiles)
        .expect_err("unlisted subagent");
    assert!(error.to_string().contains("profile `planner`"), "{error:#}");
}

#[test]
fn unsupported_adapter_keeps_subagent_reminder_in_user_prompt() {
    let request = supervised_request("amp", true);
    let adapter = rimz::agents::find_definition("amp").unwrap();
    let prompt = super::run::supervised_prompt(&request, adapter);
    assert_eq!(
        prompt,
        format!(
            "amp\n\n{}",
            rimz::harness::launch_reminders::subagent_reminder()
        )
    );

    let ordinary = supervised_request("amp", false);
    assert_eq!(super::run::supervised_prompt(&ordinary, adapter), "amp");
}

#[test]
fn native_system_text_adapters_keep_subagent_reminder_out_of_user_prompt() {
    for kind in ["claude", "codex"] {
        let request = supervised_request(kind, true);
        let adapter = rimz::agents::find_definition(kind).unwrap();

        assert_eq!(super::run::supervised_prompt(&request, adapter), kind);
    }
}

#[test]
fn pane_resolution_uses_snapshot_when_record_has_no_pane() {
    let mut record = run_record("claude");
    let workspace_id = record.workspace_id.clone();
    record.agent_id = Some(AgentSessionId::from("sess-1"));
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%9");
    let mut pane = PaneRef::from_id(pane_id.clone());
    pane.session_name = "live-session".to_owned();
    let mut agent = agent_state("claude", "sess-1", AgentStatus::Running);
    agent.pane = Some(pane);
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        workspace_id,
        vec![agent],
        jiff::Timestamp::UNIX_EPOCH,
    );

    let resolved = resolve_run_pane_in_snapshot(&snapshot, "fallback-session", &record).unwrap();
    assert_eq!(resolved.pane_id, pane_id);
    assert_eq!(resolved.session_name, "live-session");
}

#[test]
fn stop_backstop_does_not_close_a_pre_rebirth_pane() {
    let mut fixture = RunFixture::new(RunStatus::Canceled);
    fixture.record.workflow = Some(rimz::agents::WorkflowAttemptRef {
        run: rimz::ids::WorkflowRunId::new(),
        seq: 1,
        role: rimz::agents::WorkflowAttemptRole::Step,
    });
    rimz::harness::run::create(&fixture.paths, &fixture.record).unwrap();
    rimz::harness::run::record_pane(
        fixture.store.paths(),
        &fixture.record.run_id,
        PaneId::from_parts(MuxName::Zellij, "terminal_8"),
    )
    .unwrap();
    fixture
        .store
        .append_event(&rimz::EventEnvelope::session_rebirth(
            fixture.record.workspace_id.clone(),
            "rimz-test",
        ))
        .unwrap();
    super::pane::settle_stopped_run_pane(
        &fixture.store,
        "rimz-test",
        &fixture.record,
        std::time::Duration::ZERO,
        |_| Ok(true),
        |_| panic!("a reused pane in the new session must not be closed"),
    )
    .unwrap();
}

#[test]
fn stop_backstop_uses_late_recorded_pane_id() {
    let fixture = RunFixture::new(RunStatus::Canceled);
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%8");
    rimz::harness::run::record_pane(
        fixture.store.paths(),
        &fixture.record.run_id,
        pane_id.clone(),
    )
    .unwrap();

    let (latest, resolved) =
        latest_resolved_run_pane(&fixture.store, "rimz-test", &fixture.record).unwrap();
    assert_eq!(latest.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(resolved.pane_id, pane_id);
    assert_eq!(resolved.session_name, "rimz-test");
}

/// The backstop's verdict is a listing or a close: each scripted listing
/// answers in turn (the last answer repeats).
#[test]
fn stop_backstop_reports_a_pane_it_could_not_confirm_closed() {
    use super::pane::settle_stopped_run_pane;
    #[derive(Clone, Copy)]
    enum Listing {
        Present,
        Absent,
        Unreadable,
        SessionGone,
    }
    use Listing::{Absent, Present, SessionGone, Unreadable};
    let settle_within = |grace: Duration, listings: &[Listing], close_ok: bool, recorded: bool| {
        let fixture = RunFixture::new(RunStatus::Canceled);
        if recorded {
            rimz::harness::run::record_pane(
                fixture.store.paths(),
                &fixture.record.run_id,
                PaneId::from_parts(MuxName::Tmux, "%8"),
            )
            .unwrap();
        }
        let mut listings = listings.iter().copied();
        let mut last = None;
        let mut closes = 0;
        let mut listed = 0;
        let verdict = settle_stopped_run_pane(
            &fixture.store,
            "rimz-test",
            &fixture.record,
            grace,
            |pane| {
                listed += 1;
                last = listings.next().or(last);
                match last.expect("a scripted listing") {
                    Present => Ok(true),
                    Absent => Ok(false),
                    Unreadable => Err(rimz::mux::MuxErr::Output {
                        program: "tmux".to_owned(),
                        reason: "unreadable".to_owned(),
                    }),
                    SessionGone => Err(rimz::mux::MuxErr::SessionNotFound {
                        session: pane.session_name.clone(),
                    }),
                }
            },
            |_| {
                closes += 1;
                if close_ok {
                    Ok(())
                } else {
                    Err(rimz::mux::MuxErr::Output {
                        program: "tmux".to_owned(),
                        reason: "close refused".to_owned(),
                    })
                }
            },
        );
        let verdict = verdict.map_err(|open| StopRunErr::PaneOpen(open).to_string());
        (verdict, closes, listed)
    };
    let settle = |listings: &[Listing], close_ok: bool, recorded: bool| {
        let (verdict, closes, _) = settle_within(Duration::ZERO, listings, close_ok, recorded);
        (verdict, closes)
    };
    let open = Err(
        "pane tmux:%8 is still open: could not parse mux output from `tmux`: close refused; rerun the stop to close it"
            .to_owned(),
    );

    assert_eq!(settle(&[Absent], false, true), (Ok(()), 0));
    assert_eq!(settle(&[Present], false, true), (open.clone(), 1));
    assert_eq!(settle(&[Present, Absent], false, true), (Ok(()), 1));
    assert_eq!(settle(&[Unreadable], true, true), (Ok(()), 1));
    assert_eq!(settle(&[Unreadable], false, true), (open, 1));
    assert_eq!(
        settle(&[SessionGone], false, true),
        (Ok(()), 0),
        "a session that is gone holds no pane"
    );
    assert_eq!(
        settle(&[Present, SessionGone], false, true),
        (Ok(()), 1),
        "a session gone by the re-list took the pane with it"
    );
    assert_eq!(
        settle(&[], false, false),
        (Ok(()), 0),
        "a run with no pane rimz can name has nothing to close"
    );
    assert_eq!(
        settle_within(Duration::from_secs(1), &[Unreadable, Absent], false, true),
        (Ok(()), 0, 2),
        "a listing error before the deadline is retried, not returned"
    );

    let unresolved = super::pane::PaneOpen {
        pane: None,
        reason: "no multiplexer found".to_owned(),
    };
    assert_eq!(
        StopRunErr::PaneOpen(unresolved).to_string(),
        "the run's pane was not closed: no multiplexer found; rerun the stop to close it"
    );
}

#[test]
fn stream_event_shapes_are_ndjson_ready() {
    let value = serde_json::to_value(RunStreamEvent::End {
        status: RunStatus::Canceled,
        last_message: Some("bye".to_owned()),
    })
    .unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "event": "end",
            "status": "canceled",
            "last_message": "bye"
        })
    );
}

#[test]
fn subagent_run_closes_its_pane_after_terminal_completion() {
    let dir = tempfile::tempdir().expect("temp dir");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    assert_eq!(run_exit_policy(true), (true, true));
    assert_eq!(run_exit_policy(false), (false, false));

    let run_id = rimz::RunId::new();
    let launch = rimz::agents::LaunchParams {
        model: Some("gpt-5".to_owned()),
        ..Default::default()
    };
    let launch_id = rimz::ids::AgentSessionId::from("child-id");
    let team_prompt = rimz::harness::team_prompt::TeamPrompt {
        consensus: rimz::harness::team_prompt::Consensus::BuiltIn,
        files: vec!["/team/pipeline.md".into()],
    };
    let system_prompt = rimz::config::PromptSource::File("/agents/child.md".into());
    let appended = [rimz::config::PromptSource::File("/agents/extra.md".into())];
    let skills = ["review".parse().unwrap()];
    let allowed_tools = ["Read".parse().unwrap()];
    let permission_args = ["--full-auto".to_owned()];
    let binding = rimz::agents::ProviderAccountBinding::decode(
        r#"{"scope":{"kind":"kind_wide"},"account_key":"acct"}"#,
    )
    .expect("provider binding");
    let worktree = Path::new("/tmp/child-worktree");
    let cell = rimz::harness::spec::AgentCell {
        empty_dir: None,
        resume_model_override: false,
        isolation_default: Some(rimz::config::Isolation::Scoped),
        kind: rimz::agents::definition_by_kind("codex")
            .unwrap()
            .spec()
            .kind_id(),
        args: permission_args.to_vec(),
        auto_compact: None,
        system_prompt_file: Some(system_prompt.clone()),
        append_system_prompt_files: appended.to_vec(),
        team_prompt: Some(team_prompt.clone()),
        skills: Some(skills.to_vec()),
        allowed_tools: Some(allowed_tools.to_vec()),
        launch: LaunchParams::default(),
    };
    let (close_pane_on_exit, exit_on_run_completion) = run_exit_policy(true);
    let pane = run_pane_cmd(
        &runtime,
        &rimz::harness::launch::ExecRequest {
            kind: rimz::agents::definition_by_kind("codex")
                .unwrap()
                .spec()
                .kind_id(),
            action: rimz::harness::launch::ExecAction::Launch {
                prompt: Some("work".to_owned()),
                extra_args: cell.args.clone(),
            },
            provider_account: rimz::harness::launch::ProviderAccountState::Pending {
                binding: binding.clone(),
            },
            run_id: Some(run_id.clone()),
            exit_on_run_completion,
            subagent: true,
            ..rimz::harness::launch::ExecRequest::fresh(
                &cell,
                rimz::harness::launch::ExecIdentity {
                    resume_model_override: false,
                    name: Some("child".to_owned()),
                    name_explicit: true,
                    launch_id: Some(launch_id.to_string()),
                    params: launch.clone(),
                },
                Some(worktree.to_path_buf()),
                close_pane_on_exit,
            )
        },
    )
    .unwrap();
    assert_eq!(pane.name.as_deref(), Some("codex"));
    let request = rimz::harness::launch::decode_exec_request(
        "codex",
        Some(worktree),
        pane.argv.last().expect("exec payload"),
    )
    .unwrap();
    // Together `close_pane_on_exit`, `exit_on_run_completion`, `subagent` and
    // `team_prompt` select the wrapper's parent-receipt hold before it closes the pane.
    assert_eq!(
        request,
        rimz::harness::launch::ExecRequest {
            isolation_default: Some(rimz::config::Isolation::Scoped),
            kind: rimz::agents::definition_by_kind("codex")
                .unwrap()
                .spec()
                .kind_id(),
            action: rimz::harness::launch::ExecAction::Launch {
                prompt: Some("work".to_owned()),
                extra_args: permission_args.to_vec(),
            },
            system_prompt_file: Some(system_prompt),
            append_system_prompt_files: appended.to_vec(),
            team_prompt: Some(team_prompt),
            skills: Some(skills.to_vec()),
            allowed_tools: Some(allowed_tools.to_vec()),
            provider_account: rimz::harness::launch::ProviderAccountState::Pending { binding },
            run_id: Some(run_id),
            worktree_path: Some(worktree.to_path_buf()),
            close_pane_on_exit: true,
            exit_on_run_completion: true,
            subagent: true,
            follow_up_message: None,
            loop_reminder: None,
            headless: None,
            identity: rimz::harness::launch::ExecIdentity {
                resume_model_override: false,
                name: Some("child".to_owned()),
                name_explicit: true,
                launch_id: Some("child-id".to_owned()),
                params: launch,
            },
        }
    );
}

struct RunFixture {
    _dir: tempfile::TempDir,
    workspace_id: WorkspaceId,
    paths: StatePaths,
    runtime: RuntimePaths,
    store: rimz::Store,
    record: RunRecord,
}

#[test]
fn stopped_subagent_end_is_recorded_once() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};

    let mut fixture = RunFixture::new(RunStatus::Canceled);
    fixture.record.subagent = true;
    fixture.record.agent_id = Some("launch_child".into());
    fixture.record.agent_name = Some("otter".to_owned());
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%3");
    fixture
        .store
        .append_event(&rimz::EventEnvelope::agent_launched(
            fixture.workspace_id.clone(),
            "room",
            &fixture.record.kind,
            rimz::store::event::AgentLaunchPayload {
                system_prompt_digest: None,
                agent_id: "launch_child".into(),
                launch_id: None,
                agent_name: "otter".to_owned(),
                agent_name_explicit: false,
                launch: LaunchParams {
                    parent_agent_id: Some("parent".into()),
                    parent_agent_kind: Some(AgentKind::new_unchecked("codex")),
                    launch_depth: Some(1),
                    ..Default::default()
                },
                state: rimz::store::event::AgentLaunchState::Starting,
                run_id: Some(fixture.run_id()),
                pane_id: Some(pane_id.clone()),
                runtime_owner: None,
                worktree_path: None,
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .unwrap();
    let mut observation =
        AgentLifecycleObservation::new(Some("child".into()), LifecycleSignal::Registered);
    observation.pane_id = Some(pane_id);
    fixture
        .store
        .append_event(&rimz::EventEnvelope::agent_lifecycle(
            fixture.workspace_id.clone(),
            "room",
            "codex",
            "SessionStart",
            &observation,
        ))
        .unwrap();
    let registered = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert_eq!(registered.len(), 1, "{registered:?}");
    assert_eq!(registered[0].agent_id, "child");
    assert_eq!(registered[0].name.as_deref(), Some("otter"));
    assert!(fixture.record.matches_agent(&registered[0]));
    fixture.record.completed_at = registered[0].registered_at;

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();
    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();

    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert!(agents[0].ended_at.is_some(), "{agents:?}");
    let events = fixture.store.read_events().unwrap();
    let ends = events
        .iter()
        .filter_map(|event| match event.kind() {
            rimz::store::event::EventKind::AgentLifecycle(payload)
                if payload.observation.signal == LifecycleSignal::Ended =>
            {
                Some(payload)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ends.len(), 1, "{events:?}");
    assert_eq!(ends[0].event_name.as_deref(), Some("rimz.subagent-stopped"));
    assert_eq!(ends[0].observation.agent_id.as_ref().unwrap(), "child");
}

#[test]
fn stopped_subagent_end_skips_root_ended_and_unmatched_rows() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};

    for case in ["root", "ended", "unmatched"] {
        let mut fixture = RunFixture::new(RunStatus::Canceled);
        fixture.record.subagent = case != "root";
        fixture.record.agent_id = Some(
            if case == "unmatched" {
                "missing"
            } else {
                "child"
            }
            .into(),
        );
        fixture
            .store
            .append_event(&rimz::EventEnvelope::agent_lifecycle(
                fixture.workspace_id.clone(),
                "room",
                "codex",
                "SessionStart",
                &AgentLifecycleObservation::new(
                    Some("child".into()),
                    if case == "ended" {
                        LifecycleSignal::Ended
                    } else {
                        LifecycleSignal::Registered
                    },
                ),
            ))
            .unwrap();
        let before = fixture.store.read_events().unwrap();
        stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();
        assert_eq!(fixture.store.read_events().unwrap(), before, "{case}");
    }
}

#[test]
fn stopped_subagent_end_prefers_the_run_id_row_over_a_newer_namesake() {
    use rimz::agents::{AgentLifecycleObservation, LifecycleSignal};

    let mut fixture = RunFixture::new(RunStatus::Completed);
    fixture.record.subagent = true;
    fixture.record.agent_id = Some("older".into());
    fixture.record.agent_name = Some("otter".to_owned());
    let registered_at = jiff::Timestamp::now();
    for (id, name, age) in [("older", "previous", 2), ("newer", "otter", 1)] {
        let mut observation =
            AgentLifecycleObservation::new(Some(id.into()), LifecycleSignal::Registered);
        observation.agent_name = Some(name.to_owned());
        let mut event = rimz::EventEnvelope::agent_lifecycle(
            fixture.workspace_id.clone(),
            "room",
            "codex",
            "SessionStart",
            &observation,
        );
        event.timestamp = registered_at - jiff::SignedDuration::from_secs(age);
        fixture.store.append_event(&event).unwrap();
    }
    let before = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert!(
        before.iter().all(|agent| agent.ended_at.is_none()),
        "{before:?}"
    );

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();
    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert!(
        agents
            .iter()
            .find(|agent| agent.agent_id == "older")
            .unwrap()
            .ended_at
            .is_some(),
        "{agents:?}"
    );
    assert!(
        agents
            .iter()
            .find(|agent| agent.agent_id == "newer")
            .unwrap()
            .ended_at
            .is_none(),
        "{agents:?}"
    );
}

fn register_stopped_subagent_card(
    fixture: &RunFixture,
    id: &str,
    signal: rimz::agents::LifecycleSignal,
    timestamp: jiff::Timestamp,
) {
    let mut observation = rimz::agents::AgentLifecycleObservation::new(Some(id.into()), signal);
    observation.agent_name = Some("otter".to_owned());
    observation.parent_agent_id = Some("parent".into());
    observation.launch.launch_depth = Some(1);
    let mut event = rimz::EventEnvelope::agent_lifecycle(
        fixture.workspace_id.clone(),
        "room",
        "codex",
        "SessionStart",
        &observation,
    );
    event.timestamp = timestamp;
    fixture.store.append_event(&event).unwrap();
}

#[test]
fn stopped_subagent_end_skips_later_namesake_when_own_card_is_absent() {
    let mut fixture = RunFixture::new(RunStatus::Completed);
    fixture.record.subagent = true;
    fixture.record.agent_id = Some("old".into());
    fixture.record.agent_name = Some("otter".to_owned());
    register_stopped_subagent_card(
        &fixture,
        "new",
        rimz::agents::LifecycleSignal::Registered,
        fixture.record.completed_at.unwrap() + jiff::SignedDuration::from_secs(1),
    );
    let before = fixture.store.read_events().unwrap();

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();

    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    let namesake = agents.iter().find(|agent| agent.agent_id == "new").unwrap();
    assert!(
        namesake.ended_at.is_none(),
        "live namesake was ended: {agents:?}"
    );
    assert_eq!(fixture.store.read_events().unwrap(), before);
}

#[test]
fn stopped_subagent_end_skips_later_namesake_when_own_card_is_ended() {
    use rimz::agents::LifecycleSignal;

    let mut fixture = RunFixture::new(RunStatus::Completed);
    fixture.record.subagent = true;
    fixture.record.agent_id = Some("old".into());
    fixture.record.agent_name = Some("otter".to_owned());
    let completed_at = fixture.record.completed_at.unwrap();
    for (id, signal, offset) in [
        ("old", LifecycleSignal::Registered, -2),
        ("old", LifecycleSignal::Ended, -1),
        ("new", LifecycleSignal::Registered, 1),
    ] {
        register_stopped_subagent_card(
            &fixture,
            id,
            signal,
            completed_at + jiff::SignedDuration::from_secs(offset),
        );
    }
    let before = fixture.store.read_events().unwrap();
    let registered = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert_eq!(registered.len(), 2, "{registered:?}");
    let old = registered
        .iter()
        .find(|agent| agent.agent_id == "old")
        .unwrap();
    assert!(old.ended_at.is_some(), "{registered:?}");
    let new = registered
        .iter()
        .find(|agent| agent.agent_id == "new")
        .unwrap();
    assert_eq!(
        new.registered_at,
        Some(completed_at + jiff::SignedDuration::from_secs(1)),
        "{registered:?}"
    );

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();

    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    let namesake = agents.iter().find(|agent| agent.agent_id == "new").unwrap();
    assert!(
        namesake.ended_at.is_none(),
        "live namesake was ended: {agents:?}"
    );
    assert_eq!(fixture.store.read_events().unwrap(), before);
}

#[test]
fn stopped_subagent_end_prefers_the_newest_name_match_before_completion() {
    let mut fixture = RunFixture::new(RunStatus::Completed);
    fixture.record.subagent = true;
    fixture.record.agent_name = Some("otter".to_owned());
    for (id, age) in [("older", 2), ("newest", 0), ("middle", 1)] {
        register_stopped_subagent_card(
            &fixture,
            id,
            rimz::agents::LifecycleSignal::Registered,
            fixture.record.completed_at.unwrap() - jiff::SignedDuration::from_secs(age),
        );
    }

    stamp_stopped_subagent_end(&fixture.store, "room", &fixture.record).unwrap();

    let agents = fixture
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .unwrap()
        .agents;
    assert_eq!(agents.len(), 3, "{agents:?}");
    for agent in &agents {
        assert_eq!(
            agent.ended_at.is_some(),
            agent.agent_id == "newest",
            "{agents:?}"
        );
    }
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
        let store = rimz::Store::open(paths.clone(), runtime.clone()).unwrap();
        let mut record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "go".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        record.status = status;
        record.completed_at = status.is_terminal().then_some(record.started_at);
        rimz::harness::run::create(&paths, &record).unwrap();
        Self {
            _dir: dir,
            workspace_id,
            paths,
            runtime,
            store,
            record,
        }
    }

    fn run_id(&self) -> rimz::RunId {
        self.record.run_id.clone()
    }

    fn expected(&self) -> ExpectedRunFrame {
        ExpectedRunFrame {
            workspace_id: self.workspace_id.clone(),
            run_id: self.run_id(),
        }
    }

    fn waiter(&self, cancellation: RunCancellation) -> run_wake::RunWaiter {
        run_wake::RunWaiter::bind(&self.runtime, self.expected(), cancellation).unwrap()
    }

    fn complete(&self, message: &str) {
        let mut record = self.record.clone();
        record.status = RunStatus::Completed;
        record.last_message = Some(message.to_owned());
        rimz::harness::run::create(&self.paths, &record).unwrap();
    }
}

#[test]
fn background_receipt_names_the_report_or_the_wait_command() {
    let receipt_for = |names: &[&str], path: Option<&str>, subagent: bool, report_to| {
        let mut err = Vec::new();
        super::output::write_background_receipt(
            &mut err,
            names,
            path.map(Path::new),
            subagent,
            report_to,
        )
        .unwrap();
        String::from_utf8(err).unwrap()
    };
    let path = Some("/state/out/planner/calm-fox.output");
    let one = ["calm-fox"].as_slice();
    let several = ["calm-fox", "bold-owl", "shy-elk"].as_slice();

    for (subagent, report, settled, noun, wait) in [
        (
            true,
            "SUBAGENT_REPORT",
            "every subagent you launched",
            "subagent",
            "rimz subagents wait",
        ),
        (
            false,
            "AGENT_REPORT",
            "every agent you launched",
            "agent",
            "rimz agents wait",
        ),
    ] {
        assert_eq!(
            receipt_for(one, path, subagent, ReportTo::Launcher),
            format!(
                "Running in the background. Keep working or end your turn: one {report} reaches you once {settled} has settled.\nIts response lands at /state/out/planner/calm-fox.output when it settles. To block instead: {wait} calm-fox\n"
            )
        );
        assert_eq!(
            receipt_for(several, path, subagent, ReportTo::Launcher),
            format!(
                "Running in the background. Keep working or end your turn: one {report} reaches you once {settled} has settled.\nEach response lands at /state/out/planner/<name>.output when its {noun} settles. To block instead: {wait} calm-fox bold-owl shy-elk\n"
            )
        );
        assert_eq!(
            receipt_for(one, path, subagent, ReportTo::Nobody),
            format!(
                "Running detached: no {report} will reach you.\nIts response lands at /state/out/planner/calm-fox.output when it settles. To collect it: {wait} calm-fox\n"
            )
        );
        assert_eq!(
            receipt_for(several, path, subagent, ReportTo::Nobody),
            format!(
                "Running detached: no {report} will reach you.\nEach response lands at /state/out/planner/<name>.output when its {noun} settles. To collect them: {wait} calm-fox bold-owl shy-elk\n"
            )
        );
    }

    assert_eq!(
        receipt_for(one, None, false, ReportTo::Launcher),
        "Running in the background. To print its final response: rimz agents wait calm-fox\n"
    );
    assert_eq!(
        receipt_for(several, None, false, ReportTo::Launcher),
        "Running in the background. To print their final responses: rimz agents wait calm-fox bold-owl shy-elk\n"
    );
    for names in [one, several] {
        for (subagent, report_to) in [
            (true, ReportTo::Launcher),
            (false, ReportTo::Nobody),
            (true, ReportTo::Nobody),
        ] {
            assert_eq!(
                receipt_for(names, None, subagent, report_to),
                receipt_for(names, None, false, ReportTo::Launcher),
                "a shell is told the same thing in either flavour, attached or detached"
            );
        }
    }
}

#[test]
fn presented_blocking_attempt_is_joined_only_once_terminal() {
    let fixture = RunFixture::new(RunStatus::Running);
    super::run::join_presented_attempt(&fixture.store, "rimz-test", &fixture.record);
    let load = || rimz::harness::run::load(&fixture.paths, &fixture.run_id()).unwrap();
    assert_eq!(load().joined_at, None);

    fixture.complete("done");
    super::run::join_presented_attempt(&fixture.store, "rimz-test", &load());
    assert!(load().joined_at.is_some());
}

#[test]
fn subagent_zone_lock_serializes_workspace_launches() {
    let fixture = RunFixture::new(RunStatus::Running);
    let lock_path = fixture
        .store
        .runtime_paths()
        .lock_path("subagent-zone.lock");

    let held = super::pane::lock_subagent_zone(&fixture.store).unwrap();
    assert!(
        rimz::disk::lock::WorkspaceLock::try_acquire(&lock_path)
            .unwrap()
            .is_none()
    );
    drop(held);
    assert!(
        rimz::disk::lock::WorkspaceLock::try_acquire(&lock_path)
            .unwrap()
            .is_some()
    );
}

#[test]
fn blocking_stream_wakeup_reloads_terminal_record() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let waiter = fixture.waiter(RunCancellation::new());
    let sock_path = waiter.socket_path().to_path_buf();

    fixture.complete("done");
    send_run_frame(
        &sock_path,
        &WakeupFrame::RunCompleted {
            workspace_id: fixture.workspace_id.clone(),
            run_id: run_id.clone(),
            status: RunStatus::Completed,
        },
    );
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(true);
    let mut out = Vec::new();
    let mut sink = output::StreamSink::ndjson(&mut out);

    let loaded = stream_blocking_run(
        &waiter,
        &fixture.store,
        rimz::agents::definition_by_kind("codex").unwrap(),
        Some(Duration::from_secs(1)),
        (&mut cursor, &mut sink),
    )
    .unwrap();

    assert_eq!(loaded.status, RunStatus::Completed);
    assert_eq!(loaded.last_message.as_deref(), Some("done"));
}

#[test]
fn blocking_stream_timeout_marks_run_timed_out() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let waiter = fixture.waiter(RunCancellation::new());
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(true);
    let mut out = Vec::new();
    let mut sink = output::StreamSink::ndjson(&mut out);

    let timed_out = stream_blocking_run(
        &waiter,
        &fixture.store,
        rimz::agents::definition_by_kind("codex").unwrap(),
        Some(Duration::ZERO),
        (&mut cursor, &mut sink),
    )
    .unwrap();

    assert_eq!(timed_out.status, RunStatus::TimedOut);
    assert_eq!(
        rimz::harness::run::load(&fixture.paths, &run_id)
            .unwrap()
            .status,
        RunStatus::TimedOut
    );
}

#[test]
fn blocking_text_stream_leaves_forensics_to_its_caller() {
    let fixture = RunFixture::new(RunStatus::Failed);
    let waiter = fixture.waiter(RunCancellation::new());
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(true);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut sink = output::StreamSink::text(
        &mut out,
        &mut err,
        crate::cli::render::prose::Prose::Raw,
        100,
    );

    let failed = stream_blocking_run(
        &waiter,
        &fixture.store,
        rimz::agents::definition_by_kind("codex").unwrap(),
        Some(Duration::from_secs(1)),
        (&mut cursor, &mut sink),
    )
    .unwrap();

    assert_eq!(failed.status, RunStatus::Failed);
    assert!(err.is_empty());
}

#[test]
fn attached_stream_timeout_does_not_mark_run_timed_out() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut sink = output::StreamSink::text(
        &mut out,
        &mut err,
        crate::cli::render::prose::Prose::Raw,
        100,
    );

    let outcome = stream_attached_run(
        &fixture.store,
        &run_id,
        rimz::agents::definition_by_kind("codex").unwrap(),
        false,
        Some(Duration::ZERO),
        &mut sink,
        |_| Ok(()),
    )
    .unwrap();

    assert_eq!(outcome, None);
    assert_eq!(
        rimz::harness::run::load(&fixture.paths, &run_id)
            .unwrap()
            .status,
        RunStatus::Running
    );
    assert!(String::from_utf8(err).unwrap().contains("wait timed out"));
}

#[test]
fn blocking_stream_interrupt_marks_run_canceled() {
    let fixture = RunFixture::new(RunStatus::Running);
    let run_id = fixture.run_id();
    let cancellation = RunCancellation::new();
    cancellation.request();
    let waiter = fixture.waiter(cancellation);
    let mut cursor = rimz::agents::transcript::TranscriptCursor::new(true);
    let mut out = Vec::new();
    let mut sink = output::StreamSink::ndjson(&mut out);

    let canceled = stream_blocking_run(
        &waiter,
        &fixture.store,
        rimz::agents::definition_by_kind("codex").unwrap(),
        Some(Duration::from_secs(1)),
        (&mut cursor, &mut sink),
    )
    .unwrap();

    assert_eq!(canceled.status, RunStatus::Canceled);
    assert_eq!(
        rimz::harness::run::load(&fixture.paths, &run_id)
            .unwrap()
            .status,
        RunStatus::Canceled
    );
}

fn run_record(kind: &str) -> RunRecord {
    RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        AgentKind::new_unchecked(kind),
        PermissionMode::Auto,
        "go".to_owned(),
        Path::new("/tmp/rimz-run").to_path_buf(),
    )
}

fn send_run_frame(path: &Path, frame: &WakeupFrame) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .unwrap();
    runtime.block_on(async {
        let sender = UnixDatagram::unbound().unwrap();
        let bytes = serde_json::to_vec(frame).unwrap();
        sender.send_to(&bytes, path).await.unwrap();
    });
}

fn agent_state(kind: &str, id: &str, status: AgentStatus) -> AgentState {
    AgentState {
        status,
        ..rimz::testkit::agent_state(kind, id, jiff::Timestamp::UNIX_EPOCH)
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
