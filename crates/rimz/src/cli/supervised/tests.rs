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
        select_subagent_zone_strategy(std::slice::from_ref(&solo), &live, &solo, "room", &theme),
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
        select_subagent_zone_strategy(&agents, &live, &planner, "room", &theme),
        Some(SubagentZoneStrategy::CompanionGrid {
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
        select_subagent_zone_strategy(&agents, &live, &parent, "room", &theme),
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
        select_subagent_zone_strategy(&agents, &live, &parent, "room", &theme),
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
        ),
        Some(SubagentZoneStrategy::CompanionGrid {
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
        ),
        Some(SubagentZoneStrategy::CompanionGrid {
            anchors: vec![("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%3"))],
            title: "design subagents 2".to_owned(),
        })
    );
}

#[test]
fn subagent_zone_strategy_caps_physical_companions_and_reuses_overflow() {
    use super::pane::{SubagentZoneStrategy, select_subagent_zone_strategy};

    let theme = rimz::config::ThemeConfig::default();
    let mut parent = agent_state("codex", "parent", AgentStatus::Running);
    parent.team = Some("forge".to_owned());
    parent.pane = Some(pane_ref("%1", "design"));
    // None of these children has bound a durable agent row yet. Physical
    // occupancy, not registration or completion status, owns the tab cap.
    let mut live = (2..=9)
        .map(|id| pane_ref(&format!("%{id}"), "design subagents"))
        .collect::<Vec<_>>();
    let mut sidebar = pane_ref("%14", "design subagents");
    sidebar.command = Some(rimz::pane::SIDEBAR_CHROME_TITLE.to_owned());
    live.push(sidebar);
    let choose = |live: &[PaneRef]| {
        select_subagent_zone_strategy(std::slice::from_ref(&parent), live, &parent, "room", &theme)
    };
    assert_eq!(
        choose(&live),
        Some(SubagentZoneStrategy::CompanionTab {
            title: "design subagents 2".to_owned(),
        })
    );
    live.push(pane_ref("%15", "design subagents 2"));
    // Newest generic fallback children do not hijack companion placement.
    live.push(pane_ref("%16", "run codex"));
    assert_eq!(
        choose(&live),
        Some(SubagentZoneStrategy::CompanionGrid {
            anchors: vec![("room".to_owned(), PaneId::from_parts(MuxName::Tmux, "%15"))],
            title: "design subagents 3".to_owned(),
        })
    );
    live.remove(0);
    assert_eq!(
        choose(&live),
        Some(SubagentZoneStrategy::CompanionGrid {
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
        loop_reminder: None,
        passthrough: Vec::new(),
        managed_launch: rimz::agents::ManagedLaunchState::PendingResolution,
        login: rimz::store::writer::LaunchLogin::RoomDefault,
    }
}

#[test]
fn subagent_launch_anchors_at_the_parent_checkout() {
    use clap::Parser;

    let checkout = tempfile::tempdir().expect("parent checkout");
    let scratch = tempfile::tempdir().expect("invoking cwd");
    let git = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(checkout.path())
        .output()
        .expect("git init");
    assert!(git.status.success());
    let original_cwd = std::env::current_dir().expect("original cwd");
    std::env::set_current_dir(scratch.path()).expect("enter scratch");
    let globals = crate::cli::Cli::parse_from(["rimz", "subagents"]).global;
    let shell = resolve_run_workspace(&globals).expect("shell workspace");
    let mut parent = AgentState::stub("claude", "parent", AgentStatus::Idle);
    parent.worktree_path = Some(checkout.path().display().to_string());
    let anchored = anchor_subagent_workspace(
        shell.clone(),
        &supervised_request("task", true),
        Some(&parent),
        &globals,
    )
    .expect("parent workspace");
    let expected = checkout.path().canonicalize().expect("canonical checkout");
    assert_eq!(anchored.worktree_root, expected);
    assert_eq!(anchored.cwd_project_root.as_ref(), Some(&expected));
    let launch = rimz::worktree::resolve_launch_checkout(
        &anchored,
        &rimz::config::WorktreeConfig::default(),
        None,
        None,
        None,
        None,
    )
    .expect("launch checkout");
    assert_eq!(launch.cwd, expected);

    let subdir = checkout.path().join("packages/web");
    std::fs::create_dir_all(&subdir).expect("parent subdirectory");
    parent.worktree_path = Some(subdir.display().to_string());
    let anchored = anchor_subagent_workspace(
        shell.clone(),
        &supervised_request("task", true),
        Some(&parent),
        &globals,
    )
    .expect("subdirectory parent workspace");
    let launch = rimz::worktree::resolve_launch_checkout(
        &anchored,
        &rimz::config::WorktreeConfig::default(),
        None,
        None,
        None,
        None,
    )
    .expect("subdirectory launch checkout");
    assert_eq!(launch.cwd, subdir.canonicalize().expect("canonical subdir"));
    assert_eq!(anchored.cwd_project_root.as_ref(), Some(&expected));

    let peer = anchor_subagent_workspace(
        shell.clone(),
        &supervised_request("task", false),
        Some(&parent),
        &globals,
    )
    .expect("peer workspace");
    assert_eq!(peer.worktree_root, shell.worktree_root);

    parent.worktree_path = None;
    let legacy = anchor_subagent_workspace(
        shell.clone(),
        &supervised_request("task", true),
        Some(&parent),
        &globals,
    )
    .expect("legacy parent workspace");
    assert_eq!(legacy.worktree_root, shell.worktree_root);

    let missing = checkout.path().join("gone");
    parent.worktree_path = Some(missing.display().to_string());
    let error = anchor_subagent_workspace(
        shell,
        &supervised_request("task", true),
        Some(&parent),
        &globals,
    )
    .expect_err("vanished parent checkout");
    assert!(error.to_string().contains(&missing.display().to_string()));
    std::env::set_current_dir(original_cwd).expect("restore cwd");
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
        resume_model_override: false,
        isolation_default: Some(rimz::config::Isolation::Sandbox),
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
            isolation_default: Some(rimz::config::Isolation::Sandbox),
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
            loop_reminder: None,
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
