use super::*;
use rimz::agents::{AgentStatus, TurnPhase};
use rimz::ids::{PaneId, WorkspaceId};

fn entry() -> AgentReportEntry {
    let state = rimz::testkit::agent_state("codex", "coder", jiff::Timestamp::UNIX_EPOCH);
    build_entry(
        &Default::default(),
        &state,
        None,
        None,
        "@coder".into(),
        None,
        jiff::Timestamp::UNIX_EPOCH,
        ReportOverrides::default(),
    )
}

fn run(status: RunStatus) -> RunRecord {
    let mut run = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/repo")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "ship the fix\nand run tests".into(),
        PathBuf::from("/repo"),
    );
    run.status = status;
    run.started_at = jiff::Timestamp::UNIX_EPOCH;
    run.updated_at = run.started_at + jiff::SignedDuration::from_secs(10);
    run.reader = Some("leader".into());
    run
}

#[test]
fn card_projection_show_live_run_duration_moves_without_exit() {
    let run = run(RunStatus::Running);
    for secs in [120, 180] {
        let text =
            strip(|w| render_run_section(w, &run, jiff::Timestamp::from_second(secs).unwrap()));
        assert!(!text.contains("exit"), "{text}");
        assert!(
            text.contains(&format!("running {}m · for @leader", secs / 60)),
            "{text}"
        );
        assert!(text.contains("prompt: ship the fix"), "{text}");
        assert!(!text.contains("and run tests"), "{text}");
    }
}

#[test]
fn card_projection_show_terminal_run_has_exit_and_fixed_duration() {
    let mut run = run(RunStatus::Completed);
    run.completed_at = Some(jiff::Timestamp::from_second(120).unwrap());
    let text = strip(|w| render_run_section(w, &run, jiff::Timestamp::from_second(600).unwrap()));
    assert!(text.contains("completed · exit 0 · 2m"), "{text}");
}

#[test]
fn card_projection_show_turn_elapsed_is_a_duration() {
    let mut entry = entry();
    entry.status = AgentStatus::Running;
    entry.phase = TurnPhase::Acting;
    entry.timeline.turn_started_at = Some(jiff::Timestamp::UNIX_EPOCH);
    entry.timeline.last_seen = jiff::Timestamp::from_second(119).unwrap();
    let text = strip(|w| {
        render_activity_section(
            w,
            &entry,
            None,
            None,
            true,
            jiff::Timestamp::from_second(120).unwrap(),
        )
    });
    assert!(
        text.contains("running (acting) · turn 2m · last seen 1s ago · stale"),
        "{text}"
    );
    assert!(!text.contains("2m ago"), "{text}");
    for status in [AgentStatus::Idle, AgentStatus::Success] {
        entry.status = status;
        entry.phase = TurnPhase::Idle;
        entry.timeline.last_seen = jiff::Timestamp::from_second(120).unwrap();
        let text = strip(|w| {
            render_activity_section(
                w,
                &entry,
                None,
                None,
                false,
                jiff::Timestamp::from_second(3600).unwrap(),
            )
        });
        assert!(!text.contains(" · turn "), "{text}");
        assert!(text.contains("last seen 58m ago"), "{text}");
    }
}

#[test]
fn paused_and_failed_show_their_reason_once() {
    for status in [AgentStatus::Paused, AgentStatus::Failed] {
        let mut entry = entry();
        entry.status = status;
        entry.description = Some("provider unavailable".into());
        let text = strip(|w| {
            render_activity_section(w, &entry, None, None, false, jiff::Timestamp::UNIX_EPOCH)
        });
        assert_eq!(text.matches("provider unavailable").count(), 1, "{text}");
        assert!(
            text.contains(&format!("  {}: provider unavailable", status.as_str())),
            "{text}"
        );
    }
}

#[test]
fn card_projection_show_context_distinguishes_occupancy_and_spend() {
    let mut entry = entry();
    entry.context.fill_pct = Some(31);
    entry.context.window = Some(272_000);
    entry.context.used_tokens = Some(84_320);
    entry.stats.total_tokens = Some(3_000_000);
    entry.stats.cache_read_tokens = Some(2_000_000);
    entry.stats.cost_usd = Some(0.87);
    entry.stats.active_secs = Some(720);
    let text = strip(|w| render_context_section(w, &entry, jiff::Timestamp::UNIX_EPOCH));
    assert!(
        text.contains(
            "31% of 272k · 84k in window · 3M spent (2M cache read) · $0.8700 · 12m active"
        ),
        "{text}"
    );
}

#[test]
fn card_projection_show_ask_next_step_uses_supported_or_pane_route() {
    let dir = tempfile::tempdir().unwrap();
    let paths =
        rimz::StatePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    for (provider, kind, expected) in [
        (
            "codex",
            rimz::agents::AskKind::PlanApproval,
            "rimz answer @coder",
        ),
        (
            "codex",
            rimz::agents::AskKind::Permission,
            "rimz agents focus @coder",
        ),
    ] {
        let mut state = rimz::testkit::agent_state(provider, "coder", jiff::Timestamp::UNIX_EPOCH);
        state.status = AgentStatus::Waiting;
        state.waiting_since = Some(state.last_activity);
        state.open_ask = Some(rimz::agents::OpenAsk {
            id: "ask_0123456789abcdef".parse().unwrap(),
            kind,
            detail: Some("approve this?".into()),
            native_key: None,
            since: state.last_activity,
        });
        let next_step = show_ask_next_step(&paths, &state, "@coder").unwrap();
        assert!(
            next_step
                .as_deref()
                .is_some_and(|step| step.starts_with(expected)),
            "{next_step:?}"
        );
        let agent = build_entry(
            &Default::default(),
            &state,
            None,
            None,
            "@coder".into(),
            None,
            jiff::Timestamp::UNIX_EPOCH,
            ReportOverrides::default(),
        );
        let report = ShowReport {
            ask_next_step: next_step,
            ..ShowReport::default()
        };
        let text = strip(|w| render_show_sections(w, &report, &agent, jiff::Timestamp::UNIX_EPOCH));
        assert!(
            text.contains(&format!("waiting: approve this? → {expected}")),
            "{text}"
        );
    }
    for description in [None, Some("approve the plan")] {
        let mut agent = entry();
        agent.status = AgentStatus::Waiting;
        agent.description = description.map(str::to_owned);
        let text = strip(|w| {
            render_show_sections(
                w,
                &ShowReport::default(),
                &agent,
                jiff::Timestamp::UNIX_EPOCH,
            )
        });
        assert!(
            text.contains(&format!(
                "  waiting: {} → rimz agents focus @coder",
                description.unwrap_or("in the pane")
            )),
            "{text}"
        );
    }
}

#[test]
fn card_projection_show_sections_follow_activity_before_stable_facts() {
    let agent = entry();
    let report = ShowReport {
        run: Some(run(RunStatus::Running)),
        messages: vec![ShowMessage {
            id: "msg_1".into(),
            status: rimz::store::message::MessageStatus::Queued,
            from: "@user".into(),
            age: "1m ago".into(),
            text: "check".into(),
        }],
        recent_transcript: Some(crate::cli::transcript::RenderedChat {
            lane_label: None,
            channel: None,
            focus: None,
            entries: vec![],
            archive_prefix: 0,
            archived_hidden: 0,
            newest_archived_at: None,
            empty_message: None,
            last: None,
            flat: false,
            older_omitted: false,
        }),
        capture: Some(rimz::mux::PaneCapture {
            pane_id: PaneId::from_parts(rimz::MuxName::Tmux, "%1"),
            raw_text: "working".into(),
            lines: vec!["working".into()],
        }),
        ..ShowReport::default()
    };
    let text = strip(|w| render_show_sections(w, &report, &agent, jiff::Timestamp::UNIX_EPOCH));
    assert!(text.starts_with("@coder  codex"), "{text}");
    let positions = [
        "Run\n",
        "Messages\n",
        "Recent transcript\n",
        "Context\n",
        "Identity\n",
        "Capture\n",
    ]
    .map(|section| text.find(section).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
}

fn strip<E: std::fmt::Debug>(
    render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> std::result::Result<(), E>,
) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    render_one(&mut stream).expect("render to in-memory buffer");
    String::from_utf8(stream.into_inner()).expect("utf-8")
}

#[test]
fn capture_section_has_a_heading_and_pane_frame() {
    let capture = rimz::mux::PaneCapture {
        pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_3"),
        raw_text: "working".to_owned(),
        lines: vec!["working".to_owned()],
    };

    let rendered = strip(|w| render_capture_section(w, &capture));

    assert!(
        rendered.starts_with("Capture\n╭─ zellij:terminal_3 "),
        "{rendered}"
    );
    assert!(rendered.contains("\n│ working"), "{rendered}");
    assert!(rendered.ends_with("╯\n"), "{rendered}");
}

#[test]
fn show_message_status_serializes_as_its_existing_string() {
    let message = ShowMessage {
        id: "msg_1".to_owned(),
        status: rimz::store::message::MessageStatus::TimedOut,
        from: "@planner".to_owned(),
        age: "2m ago".to_owned(),
        text: "check".to_owned(),
    };

    assert_eq!(
        serde_json::to_string(&message).expect("serialize show message"),
        r#"{"id":"msg_1","status":"timed_out","from":"@planner","age":"2m ago","text":"check"}"#
    );
}

#[test]
fn show_json_wraps_the_projected_agent() {
    let mut state = rimz::testkit::agent_state("codex", "show", jiff::Timestamp::UNIX_EPOCH);
    state.login = Some("work".parse().unwrap());
    state.launch_warnings = vec!["tool rules unsupported".into()];
    let peers = [&state];
    let report = ShowReport {
        tmp_dir: None,
        agent: Some(build_entry(
            &rimz::agents::ParkDemotion::default(),
            &state,
            None,
            None,
            rimz::address::agent_handle(peers[0], &peers, false),
            None,
            jiff::Timestamp::UNIX_EPOCH,
            ReportOverrides {
                effort: Some(rimz::agents::spending::SlotEffort {
                    cost_usd: Some(0.42),
                    ..rimz::agents::spending::SlotEffort::default()
                }),
                ..ReportOverrides::default()
            },
        )),
        agent_state: Some(state),
        stale: true,
        run: None,
        ask: None,
        ask_next_step: None,
        messages: Vec::new(),
        system_messages_hidden: 0,
        capture: None,
        recent_transcript: None,
    };

    assert_eq!(
        serde_json::to_value(&report).unwrap()["agent"]["launch_warnings"],
        serde_json::json!(["tool rules unsupported"])
    );
    assert_eq!(
        serde_json::to_value(&report).unwrap()["agent"]["login"],
        "codex@work"
    );
    insta::with_settings!({snapshot_path => "../snapshots"}, {
        insta::assert_json_snapshot!("show_agent_report", report);
    });
}

#[test]
fn activity_shows_hibernation_time_and_reclaimed_bytes() {
    let now = jiff::Timestamp::UNIX_EPOCH;
    let mut state = rimz::testkit::agent_state("claude", "cold", now);
    state.status = rimz::agents::AgentStatus::Running;
    state.hibernation = serde_json::from_str(
        r#"{"frozen_at":"1970-01-01T00:00:00Z","step":0,"reclaimed_bytes":2048,"wake_error":"permission denied"}"#,
    ).unwrap();
    let entry = build_entry(
        &rimz::agents::ParkDemotion::default(),
        &state,
        None,
        None,
        rimz::address::agent_handle(&state, &[&state], false),
        None,
        now,
        ReportOverrides::default(),
    );
    let text = strip(|w| render_activity_section(w, &entry, None, None, false, now));
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["hibernation"]["reclaimed_bytes"], 2048);
    assert_eq!(json["hibernation"]["wake_error"], "permission denied");
    assert!(text.contains(", wake failed: permission denied"), "{text}");
    assert!(
        text.contains("hibernating since 1970-01-01T00:00:00Z, step 0 of 3, 2 KB reclaimed"),
        "{text}"
    );
}

#[test]
fn agent_section_shows_only_named_accounts() {
    let mut state = rimz::testkit::agent_state("codex", "show", jiff::Timestamp::UNIX_EPOCH);
    for login in [None, Some("work".parse().unwrap())] {
        state.login = login;
        let entry = build_entry(
            &rimz::agents::ParkDemotion::default(),
            &state,
            None,
            None,
            rimz::address::agent_handle(&state, &[&state], false),
            None,
            jiff::Timestamp::UNIX_EPOCH,
            ReportOverrides::default(),
        );
        let text = strip(|out| render_identity_section(out, &entry, None));
        assert_eq!(text.contains("account"), state.login.is_some(), "{text}");
        if state.login.is_some() {
            assert!(text.contains("codex@work"), "{text}");
        }
    }
}

#[test]
fn agent_section_shows_launch_warnings_only_when_present() {
    let mut state = rimz::testkit::agent_state("codex", "show", jiff::Timestamp::UNIX_EPOCH);
    for warnings in [
        vec!["tool rules unsupported".into(), "skill shadowed".into()],
        vec![],
    ] {
        state.launch_warnings = warnings;
        let entry = build_entry(
            &rimz::agents::ParkDemotion::default(),
            &state,
            None,
            None,
            rimz::address::agent_handle(&state, &[&state], false),
            None,
            jiff::Timestamp::UNIX_EPOCH,
            ReportOverrides::default(),
        );
        let mut output = Vec::new();
        render_identity_section(&mut output, &entry, None).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(
            output.contains("launch_warnings"),
            !state.launch_warnings.is_empty()
        );
        if !state.launch_warnings.is_empty() {
            let first = output
                .lines()
                .position(|line| line.contains("tool rules unsupported"))
                .unwrap();
            let second = output
                .lines()
                .position(|line| line.contains("skill shadowed"))
                .unwrap();
            assert_eq!(second, first + 1, "{output}");
        }
    }
}

#[test]
fn show_excluded_lane_falls_back_to_the_addressed_record() {
    let dir = tempfile::tempdir().unwrap();
    let checkout = dir.path().join("removed");
    let mut first = rimz::testkit::agent_state("claude", "first", jiff::Timestamp::UNIX_EPOCH);
    first.worktree_path = Some(checkout.display().to_string());
    first.team = Some("forge".to_owned());
    first.role = Some("coder".to_owned());
    let mut second = first.clone();
    second.agent_id = "second".into();
    let records = [&first, &second];
    let slot = slot_lifetime_records(&records, &second);

    assert_eq!(slot.len(), 1);
    assert_eq!(slot[0].agent_id, second.agent_id);

    std::fs::create_dir_all(checkout.join(".git")).unwrap();
    std::fs::write(checkout.join(".git/rimz-worktree.json"), "{").unwrap();
    let unreadable_slot = slot_lifetime_records(&records, &second);
    assert_eq!(unreadable_slot.len(), 1);
    assert_eq!(unreadable_slot[0].agent_id, second.agent_id);
}

#[test]
fn launched_child_show_uses_its_own_continuation_slot() {
    let now = jiff::Timestamp::UNIX_EPOCH;
    let parent = rimz::testkit::agent_state("claude", "parent", now);
    let mut first = rimz::testkit::agent_state("codex", "child-one", now);
    first.parent_agent_id = Some(parent.agent_id.clone());
    first.parent_agent_kind = Some(parent.kind.clone());
    first.launch_depth = Some(1);
    first.pane = Some(rimz::pane::PaneRef::from_id(rimz::PaneId::from_parts(
        rimz::MuxName::Tmux,
        "%3",
    )));
    let mut second = first.clone();
    second.agent_id = "child-two".into();
    let records = [&parent, &first, &second];

    let child_slot = slot_lifetime_records(&records, &second);

    assert_eq!(
        child_slot
            .iter()
            .map(|record| record.agent_id.as_str())
            .collect::<Vec<_>>(),
        ["child-one", "child-two"]
    );
}

#[test]
fn tool_calls_render_total_then_top_three() {
    let calls = std::collections::BTreeMap::from([
        ("Edit".to_owned(), 9),
        ("Read".to_owned(), 7),
        ("Bash".to_owned(), 18),
        ("Write".to_owned(), 2),
    ]);

    assert_eq!(format_tool_calls(&calls), "36 · Bash 18, Edit 9, Read 7, …");
}
