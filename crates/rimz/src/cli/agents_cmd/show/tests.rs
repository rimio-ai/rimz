use super::*;
use rimz::agents::{AgentStatus, TurnPhase};
use rimz::harness::idle_stop::started::{RowKind, Started, StartedRow};
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

#[test]
fn raised_waiting_activity_shows_the_question_instead_of_the_old_task() {
    let mut state = rimz::testkit::agent_state("codex", "raised", jiff::Timestamp::UNIX_EPOCH);
    state.status = AgentStatus::Success;
    state.description = Some("old task".into());
    state.raised_ask = Some(rimz::agents::state::RaisedAsk {
        ask_id: rimz::ids::AskId::new(),
        state: rimz::agents::state::RaisedAskState::Open,
        raised_at: jiff::Timestamp::UNIX_EPOCH,
        basis_at: jiff::Timestamp::UNIX_EPOCH,
        opened_at: Some(jiff::Timestamp::UNIX_EPOCH),
        raised_by: None,
        text: Some("Which deployment?".into()),
        options: vec![],
    });
    let mut pane = rimz::pane::PaneRef::from_id(PaneId::from_parts(rimz::MuxName::Tmux, "%1"));
    pane.command = Some("codex".into());
    state.pane = Some(pane.clone());
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        vec![state.clone()],
        jiff::Timestamp::UNIX_EPOCH,
    )
    .with_live_panes(vec![pane], None);
    assert_eq!(
        row_for_agent(&snapshot, &state).unwrap().status(),
        Some(AgentStatus::Waiting)
    );
    let entry = build_entry(
        &Default::default(),
        &state,
        row_for_agent(&snapshot, &state),
        None,
        "@raised".into(),
        None,
        snapshot.now,
        ReportOverrides::default(),
    );
    let text = strip(|w| render_activity_section(w, &entry, None, None, None, false, snapshot.now));
    assert!(text.contains("  waiting: Which deployment?"), "{text}");
}

#[test]
fn delegated_wait_show_activity_names_subagent_and_focuses_answering_pane() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = rimz::ResolvedWorkspace {
        workspace_id: WorkspaceId::from_project_root(dir.path()),
        project_root: dir.path().to_owned(),
        cwd_project_root: None,
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: dir.path().to_owned(),
        worktree_branch: None,
        session_name: "room".to_owned(),
        mux_hint: None,
    };
    let paths =
        rimz::StatePaths::under(workspace.workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime =
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("runtime"))
            .unwrap();
    let store = rimz::Store::open(paths, runtime.clone()).unwrap();
    let now = jiff::Timestamp::from_second(1_000).unwrap();

    for (launched, expect_handle, expect_focus) in [
        (true, "@scout", "rimz agents focus @scout"),
        (false, "@Explore", "rimz agents focus @parent"),
    ] {
        let mut parent = rimz::testkit::agent_state("claude", "parent", now);
        parent.status = AgentStatus::Idle;
        parent.name = Some("parent".into());
        parent.name_explicit = true;
        let mut parent_pane =
            rimz::pane::PaneRef::from_id(PaneId::from_parts(rimz::MuxName::Tmux, "%1"));
        parent_pane.command = Some("claude".into());
        parent.pane = Some(parent_pane.clone());

        let mut child = rimz::testkit::agent_state("claude", "child-1", now);
        child.status = AgentStatus::Waiting;
        child.parent_agent_id = Some(parent.agent_id.clone());
        child.parent_agent_kind = Some(parent.kind.clone());
        child.waiting_since = Some(now);
        let mut panes = vec![parent_pane];
        if launched {
            child.launch_depth = Some(1);
            child.name = Some("scout".into());
            let mut child_pane =
                rimz::pane::PaneRef::from_id(PaneId::from_parts(rimz::MuxName::Tmux, "%2"));
            child_pane.command = Some("claude".into());
            child.pane = Some(child_pane.clone());
            panes.push(child_pane);
        } else {
            child.name = Some("Explore".into());
            child.name_explicit = true;
        }

        let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            workspace.workspace_id.clone(),
            vec![parent.clone(), child.clone()],
            now,
        )
        .with_live_panes(panes, None);
        let (report, error) =
            collect_show_report(&store, &workspace, &runtime, &snapshot, "@parent", None).unwrap();
        assert!(error.is_none());
        assert_eq!(
            report.ask_next_step.as_deref(),
            Some(expect_focus),
            "launched={launched}: collect_show_report must derive ask_next_step for the answering pane"
        );
        let entry = report.agent.as_ref().expect("parent report entry");
        let text = strip(|w| {
            render_activity_section(
                w,
                entry,
                report.started.as_ref(),
                report.ask.as_ref(),
                report.ask_next_step.as_deref(),
                false,
                now,
            )
        });
        assert!(
            text.contains(&format!("  waiting: {expect_handle}")),
            "launched={launched}: waiting line must name subagent: {text}"
        );
        assert!(
            text.contains(&format!("→ {expect_focus}")),
            "launched={launched}: focus step must target answering pane's agent: {text}"
        );
    }
}

#[test]
fn context_cards_use_published_capacity_but_keep_usable_window_fill() {
    for (kind, window, capacity, used, label) in [
        ("opencode", 200_000, 400_000, 26_000, "400k"),
        ("opencode", 983_010, 1_048_576, 127_791, "1M"),
        ("codex", 828_400, 872_000, 107_692, "872k"),
    ] {
        let mut state = rimz::testkit::agent_state(kind, "capacity", jiff::Timestamp::UNIX_EPOCH);
        state.usage.context_window = Some(window);
        state.usage.context_capacity = (kind == "opencode").then_some(capacity);
        state.usage.total_tokens = Some(used);
        state.usage.fresh_input_tokens = Some(used);
        if kind == "codex" {
            state.context = Some(rimz::agents::AgentContext {
                model_id: Some("gpt-6.1-sol".into()),
                model_context_limits: Some(rimz::agents::context::ModelContextLimits {
                    model: "gpt-6.1-sol".into(),
                    max_window: Some(capacity),
                    usable_percent: Some(95),
                    own_compact_limit: None,
                }),
                ..Default::default()
            });
        }
        let entry = build_entry(
            &Default::default(),
            &state,
            None,
            None,
            "@capacity".into(),
            None,
            jiff::Timestamp::UNIX_EPOCH,
            ReportOverrides::default(),
        );
        let text = strip(|w| render_context_section(w, &entry, jiff::Timestamp::UNIX_EPOCH));
        assert!(text.contains(&format!("13% of {label}")), "{kind}: {text}");
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["context"]["window"], window);
        assert_eq!(json["context"]["capacity"], capacity);
        assert_eq!(json["context"]["fill_pct"], 13);
        assert_eq!(json["context"]["used_tokens"], used);
    }
}

#[test]
fn permission_ask_visibility_uses_the_show_snapshot_clock() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = rimz::ResolvedWorkspace {
        workspace_id: WorkspaceId::from_project_root(dir.path()),
        project_root: dir.path().to_owned(),
        cwd_project_root: None,
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: dir.path().to_owned(),
        worktree_branch: None,
        session_name: "room".to_owned(),
        mux_hint: None,
    };
    let paths =
        rimz::StatePaths::under(workspace.workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime =
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("runtime"))
            .unwrap();
    let store = rimz::Store::open(paths, runtime.clone()).unwrap();
    let since = jiff::Timestamp::from_second(1_000).unwrap();
    let mut state = rimz::testkit::agent_state("codex", "review", since);
    state.status = AgentStatus::Waiting;
    state.mode = Some(PermissionMode::Auto);
    state.waiting_since = Some(since);
    state.open_ask = Some(rimz::agents::OpenAsk {
        extra_questions: 0,
        id: rimz::ids::AskId::new(),
        kind: rimz::agents::AskKind::Permission,
        detail: Some("approve this?".to_owned()),
        native_key: None,
        since,
    });
    let mut pane = rimz::pane::PaneRef::from_id(PaneId::from_parts(rimz::MuxName::Tmux, "%1"));
    pane.command = Some("codex".to_owned());
    state.pane = Some(pane.clone());
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        workspace.workspace_id.clone(),
        vec![state.clone()],
        jiff::Timestamp::from_second(1_059).unwrap(),
    )
    .with_live_panes(vec![pane.clone()], None);
    let (report, error) =
        collect_show_report(&store, &workspace, &runtime, &snapshot, "@codex", None).unwrap();
    assert!(error.is_none());
    assert_eq!(report.agent.as_ref().unwrap().status, AgentStatus::Running);
    assert!(report.ask_next_step.is_none(), "{:?}", report.ask_next_step);
    assert!(report.ask.is_none());
    assert!(report.agent.as_ref().unwrap().ask.is_none());

    // A queued async question still gets its next step with no blocking ask.
    state.status = AgentStatus::Running;
    state.open_ask = None;
    state.waiting_since = None;
    state.queued_asks.push(rimz::agents::QueuedAsk {
        id: rimz::ids::AskId::new(),
        detail: "Which branch?".to_owned(),
        native_key: "question".to_owned(),
        since,
    });
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        workspace.workspace_id.clone(),
        vec![state],
        jiff::Timestamp::from_second(1_059).unwrap(),
    )
    .with_live_panes(vec![pane], None);
    let (report, _) =
        collect_show_report(&store, &workspace, &runtime, &snapshot, "@codex", None).unwrap();
    assert!(report.ask_next_step.is_some());
}

#[test]
fn fast_show_header_marks_only_a_resolved_model() {
    let config = crate::cli::machine_config();
    let glyph = rimz::theme::theme_glyphs(&config.theme);
    let bolt = glyph(rimz::config::GlyphRole::CardFast);
    for fast in [true, false] {
        for model in [Some("gpt-6.1-sol"), None] {
            let mut state =
                rimz::testkit::agent_state("codex", "fast", jiff::Timestamp::UNIX_EPOCH);
            state.model = model.map(str::to_owned);
            state.context = Some(rimz::agents::AgentContext {
                fast: Some(fast),
                ..Default::default()
            });
            let mut report = entry();
            report.model = super::super::report::model_report(&state);
            let text = strip(|w| render_agent_section(w, &report));
            match (model, fast) {
                (Some(_), true) => {
                    assert!(text.ends_with(&format!("Sol 6.1 {bolt}\n")), "{text}")
                }
                (Some(_), false) => assert!(text.ends_with("Sol 6.1\n"), "{text}"),
                (None, _) => assert!(!text.contains(&bolt), "{text}"),
            }
        }
    }
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
fn show_model_names_the_tier_and_every_pass() {
    let mut state = rimz::testkit::agent_state("codex", "coder", jiff::Timestamp::UNIX_EPOCH);
    state.model = Some("astra".into());
    state.tier = Some(
        serde_json::from_value(serde_json::json!({
            "tier": "senior", "model": "astra",
            "unbuildable": [{"model": "opus", "error": "user-only skill; enable model invocation"}],
            "skipped": [{"model": "fable", "reason": "logged_out"}]
        }))
        .unwrap(),
    );
    let mut entry = entry();
    entry.model = super::super::report::model_report(&state);
    let text = strip(|w| render_agent_section(w, &entry));
    assert!(text.contains("Astra (senior; opus: not buildable: user-only skill; enable model invocation; fable: logged out)"), "{text}");
    state.tier.as_mut().unwrap().unbuildable.clear();
    state.tier.as_mut().unwrap().skipped.clear();
    entry.model = super::super::report::model_report(&state);
    let text = strip(|w| render_agent_section(w, &entry));
    assert!(text.contains("Astra (senior)"), "{text}");
    assert!(!text.contains("passed"));
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
    entry.timeline.last_seen = jiff::Timestamp::from_second(120).unwrap();
    entry.timeline.idle_since = jiff::Timestamp::from_second(119).unwrap();
    let text = strip(|w| {
        render_activity_section(
            w,
            &entry,
            None,
            None,
            None,
            true,
            jiff::Timestamp::from_second(120).unwrap(),
        )
    });
    assert!(
        text.contains("running (acting) · turn 2m · last request 1s ago · stale"),
        "{text}"
    );
    assert!(!text.contains("2m ago"), "{text}");
    for status in [AgentStatus::Idle, AgentStatus::Success] {
        entry.status = status;
        entry.phase = TurnPhase::Idle;
        entry.timeline.idle_since = jiff::Timestamp::from_second(120).unwrap();
        let text = strip(|w| {
            render_activity_section(
                w,
                &entry,
                None,
                None,
                None,
                false,
                jiff::Timestamp::from_second(3600).unwrap(),
            )
        });
        assert!(!text.contains(" · turn "), "{text}");
        assert!(text.contains("last request 58m ago"), "{text}");
    }
}

#[test]
fn paused_and_failed_show_their_reason_once() {
    for status in [AgentStatus::Paused, AgentStatus::Failed] {
        let mut entry = entry();
        entry.status = status;
        entry.description = Some("provider unavailable".into());
        let text = strip(|w| {
            render_activity_section(
                w,
                &entry,
                None,
                None,
                None,
                false,
                jiff::Timestamp::UNIX_EPOCH,
            )
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
    entry.context.capacity = Some(272_000);
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
            extra_questions: 0,
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
        started: Some(Started {
            outliving: vec![started_row(RowKind::Agent, "@first", "running", None)],
            ..Started::default()
        }),
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
        "Started\n",
        "Recent transcript\n",
        "Context\n",
        "Identity\n",
        "Capture\n",
    ]
    .map(|section| text.find(section).unwrap());
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
}

#[test]
fn agent_header_pairs_a_differing_name_with_the_handle() {
    let mut agent = entry();
    agent.handle = "@coder#auth".into();
    agent.name = Some("round-binder".into());
    let text = strip(|w| {
        render_show_sections(
            w,
            &ShowReport::default(),
            &agent,
            jiff::Timestamp::UNIX_EPOCH,
        )
    });
    assert!(text.starts_with("@coder (@round-binder)  codex"), "{text}");
    assert!(
        text.lines().next().unwrap().ends_with("  (@coder#auth)"),
        "{text}"
    );
}

fn started_row(kind: RowKind, name: &str, status: &str, command: Option<&str>) -> StartedRow {
    StartedRow {
        kind,
        name: name.to_owned(),
        status: status.to_owned(),
        tone: AgentStatus::Idle,
        phase: rimz::agents::TurnPhase::Idle,
        provider: None,
        since: Some(jiff::Timestamp::from_second(3_480).unwrap()),
        due: None,
        detail: Vec::new(),
        key: None,
        command: command.map(str::to_owned),
    }
}

fn all_groups() -> Started {
    let mut wait = started_row(
        RowKind::Wait,
        "wait-gate",
        "sleeping",
        Some("rimz wait cancel wait-gate"),
    );
    wait.detail = vec!["wakes after cargo xtask gate".to_owned()];
    Started {
        holding: vec![
            wait,
            started_row(
                RowKind::Message,
                "msg_1",
                "queued",
                Some("rimz message cancel msg_1"),
            ),
            started_row(RowKind::Run, "run_1", "running", None),
        ],
        closing: vec![started_row(RowKind::Subagent, "@child", "idle", None)],
        outliving: vec![started_row(
            RowKind::Agent,
            "@first › @second",
            "running",
            Some("rimz agents stop @second"),
        )],
    }
}

#[test]
fn started_section_heads_each_group_with_rows_and_drops_empty_ones() {
    let now = jiff::Timestamp::from_second(3_600).unwrap();
    let agent = entry();
    let report = ShowReport {
        started: Some(all_groups()),
        ..ShowReport::default()
    };
    let text = strip(|w| render_show_sections(w, &report, &agent, now));
    let section = &text[text.find("Started\n").expect("section")..text.find("Context\n").unwrap()];
    assert_eq!(
        section,
        "Started\n\
         \x20 would hold a stop\n\
         \x20   wait      wait-gate         sleeping  2m ago · wakes after cargo xtask gate  rimz wait cancel wait-gate\n\
         \x20   message   msg_1             queued    2m ago                                 rimz message cancel msg_1\n\
         \x20   run       run_1             running   2m ago\n\
         \x20 close with it\n\
         \x20   subagent  @child            idle      2m ago\n\
         \x20 outlive it\n\
         \x20   agent     @first › @second  running   2m ago                                 rimz agents stop @second\n\
         \n"
    );

    let only_outliving = ShowReport {
        started: Some(Started {
            outliving: all_groups().outliving,
            ..Started::default()
        }),
        ..ShowReport::default()
    };
    let text = strip(|w| render_show_sections(w, &only_outliving, &agent, now));
    assert!(text.contains("Started\n  outlive it\n"), "{text}");
    assert!(
        !text.contains("would hold a stop") && !text.contains("close with it"),
        "{text}"
    );

    for started in [None, Some(Started::default())] {
        let report = ShowReport {
            started,
            ..ShowReport::default()
        };
        let text = strip(|w| render_show_sections(w, &report, &agent, now));
        assert!(!text.contains("Started"), "no rows, no heading: {text}");
    }
}

#[test]
fn idle_stop_row_names_what_holds_it_and_counts_the_rest() {
    let now = jiff::Timestamp::from_second(3_600).unwrap();
    let mut agent = entry();
    agent.idle_stop = Some(rimz::agents::state::PendingIdleStop {
        stop: rimz::agents::state::IdleStop {
            after_secs: 180,
            requested_at: jiff::Timestamp::from_second(3_000).unwrap(),
            requested_by: None,
        },
        due_at: None,
    });
    let one = Started {
        holding: all_groups().holding[..1].to_vec(),
        ..Started::default()
    };
    let only_outliving = Started {
        outliving: all_groups().outliving,
        ..Started::default()
    };
    for (started, suffix) in [
        (
            Some(all_groups()),
            Some(", held by wait wait-gate and 2 more"),
        ),
        (Some(one), Some(", held by wait wait-gate")),
        (Some(only_outliving), None),
        (None, None),
    ] {
        let report = ShowReport {
            started,
            ..ShowReport::default()
        };
        let text = strip(|w| render_show_sections(w, &report, &agent, now));
        let row = text
            .lines()
            .find(|line| line.contains("idle_stop"))
            .expect("idle_stop row");
        match suffix {
            Some(suffix) => {
                assert!(row.contains(suffix), "{row}");
                assert_eq!(text.matches("held by").count(), 1, "{text}");
            }
            None => assert!(!row.contains("held by"), "{row}"),
        }
    }
}

#[test]
fn show_json_carries_started_only_when_it_has_rows() {
    let report = ShowReport {
        started: Some(all_groups()),
        ..ShowReport::default()
    };
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["started"]["holding"][0],
        serde_json::json!({
            "kind": "wait",
            "name": "wait-gate",
            "status": "sleeping",
            "since": "1970-01-01T00:58:00Z",
            "detail": ["wakes after cargo xtask gate"],
            "command": "rimz wait cancel wait-gate",
        })
    );
    assert_eq!(json["started"]["closing"][0]["kind"], "subagent");
    assert_eq!(json["started"]["outliving"][0]["name"], "@first › @second");
    let json = serde_json::to_value(ShowReport::default()).unwrap();
    assert!(json.get("started").is_none(), "{json}");
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
    state.usage.context_window = Some(272_000);
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
        started: None,
        started_unread: None,
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
    let text = strip(|w| render_activity_section(w, &entry, None, None, None, false, now));
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
        let text = strip(|out| render_identity_section(out, &entry, None, None));
        assert_eq!(text.contains("account"), state.login.is_some(), "{text}");
        if state.login.is_some() {
            assert!(text.contains("codex@work"), "{text}");
        }
    }
}

#[test]
fn identity_shows_requested_and_running_effort_only_when_they_differ() {
    for (requested, scalar, context, expected) in [
        (
            Some("xhigh"),
            Some("max"),
            None,
            Some("running max, requested xhigh"),
        ),
        (
            Some("xhigh"),
            Some("xhigh"),
            Some("max"),
            Some("running max, requested xhigh"),
        ),
        (Some("high"), Some("max"), Some("high"), None),
        (Some("high"), Some("high"), None, None),
        (None, Some("max"), None, None),
        (Some("xhigh"), None, None, None),
    ] {
        let mut state = rimz::testkit::agent_state("pi", "effort", jiff::Timestamp::UNIX_EPOCH);
        state.record = requested.map(|effort| {
            Box::new(rimz::agents::LaunchRecord {
                effort: Some(effort.into()),
                ..Default::default()
            })
        });
        state.effort = scalar.map(str::to_owned);
        state.context = context.map(|effort| rimz::agents::AgentContext {
            effort: Some(effort.into()),
            ..Default::default()
        });
        let entry = build_entry(
            &Default::default(),
            &state,
            None,
            None,
            "@effort".into(),
            None,
            jiff::Timestamp::UNIX_EPOCH,
            ReportOverrides::default(),
        );
        let report = ShowReport {
            agent_state: Some(state),
            ..Default::default()
        };
        let text =
            strip(|out| render_show_sections(out, &report, &entry, jiff::Timestamp::UNIX_EPOCH));
        if let Some(expected) = expected {
            let identity = text.split("Identity\n").nth(1).unwrap();
            assert!(identity.contains(expected), "{identity}");
        } else {
            assert!(!text.contains("requested "), "{text}");
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
        render_identity_section(&mut output, &entry, None, None).unwrap();
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

#[test]
fn a_failed_started_read_leaves_no_section_and_warns_with_its_error() {
    let read = std::io::Error::other("runs unreadable");
    let (started, unread) = split_started::<std::io::Error>(Some(Err(read)));
    assert!(started.is_none());
    let mut err = Vec::new();
    super::super::report::warn_started_unread(&mut err, "", &unread.expect("the read's error"))
        .unwrap();
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "warning: what it started could not be read: runs unreadable\n"
    );
    let (started, unread) = split_started::<std::io::Error>(Some(Ok(Started::default())));
    assert!(started.is_none() && unread.is_none());
    let (started, unread) = split_started::<std::io::Error>(Some(Ok(all_groups())));
    assert_eq!(started, Some(all_groups()));
    assert!(unread.is_none());
}

#[test]
fn show_parked_launched_child_prints_phase_in_text_and_json() {
    let now = jiff::Timestamp::from_second(200).unwrap();
    let mut parent = rimz::testkit::agent_state("claude", "parent", now);
    parent.status = AgentStatus::Idle;
    let mut parent_pane =
        rimz::pane::PaneRef::from_id(PaneId::from_parts(rimz::MuxName::Tmux, "%1"));
    parent_pane.command = Some("claude".into());
    parent.pane = Some(parent_pane.clone());

    let mut child = rimz::testkit::agent_state("claude", "child", now);
    child.parent_agent_id = Some(parent.agent_id.clone());
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.status = AgentStatus::Running;
    child.phase = TurnPhase::Parked;
    child.description = Some("background build".into());
    let mut child_pane =
        rimz::pane::PaneRef::from_id(PaneId::from_parts(rimz::MuxName::Tmux, "%2"));
    child_pane.command = Some("claude".into());
    child.pane = Some(child_pane.clone());

    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        WorkspaceId::from_project_root(std::path::Path::new("/repo")),
        vec![parent.clone(), child.clone()],
        now,
    )
    .with_live_panes(vec![parent_pane, child_pane], None);

    let child_entry = build_entry(
        &Default::default(),
        &child,
        row_for_agent(&snapshot, &child),
        None,
        "@child".into(),
        None,
        now,
        ReportOverrides {
            child: child_for_agent(&snapshot, &child),
            ..Default::default()
        },
    );
    let text = strip(|w| render_activity_section(w, &child_entry, None, None, None, false, now));
    assert!(
        text.contains("success (parked)"),
        "expected success (parked) in show text:\n{text}"
    );
    let child_json = serde_json::to_value(&child_entry).unwrap();
    assert_eq!(child_json["status"], "success");
    assert_eq!(child_json["phase"], "parked");

    let parent_entry = build_entry(
        &Default::default(),
        &parent,
        row_for_agent(&snapshot, &parent),
        None,
        "@parent".into(),
        None,
        now,
        ReportOverrides::default(),
    );
    let parent_json = serde_json::to_value(&parent_entry).unwrap();
    assert_eq!(parent_json["sub_agents"][0]["status"], "success");
    assert_eq!(parent_json["sub_agents"][0]["phase"], "parked");
}
