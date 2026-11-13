use super::super::report::model_report;
use super::*;
use clap::Parser as _;
use rimz::agents::AgentStatus;

fn entry(status: AgentStatus) -> AgentReportEntry {
    let state = test_agent(status.as_str());
    let mut entry = build_entry(
        &Default::default(),
        &state,
        None,
        None,
        format!("@{}", status.as_str()),
        None,
        jiff::Timestamp::UNIX_EPOCH,
        ReportOverrides::default(),
    );
    entry.status = status;
    entry.description = Some("provider overloaded".into());
    entry.needs_you = match status {
        AgentStatus::Waiting | AgentStatus::Failed | AgentStatus::Paused => {
            Some(rimz::store::snapshot::NeedsYou {
                status,
                reason: entry.description.clone(),
                since: jiff::Timestamp::UNIX_EPOCH,
            })
        }
        _ => None,
    };
    entry
}

fn live(entries: &[AgentReportEntry]) -> Vec<(&AgentReportEntry, bool)> {
    entries.iter().map(|entry| (entry, true)).collect()
}

#[test]
fn card_projection_list_summary_counts_and_attention() {
    let entries = [
        AgentStatus::Running,
        AgentStatus::Sleeping,
        AgentStatus::Waiting,
        AgentStatus::Paused,
        AgentStatus::Failed,
        AgentStatus::Success,
    ]
    .map(entry);
    let mut out = anstream::StripStream::new(Vec::new());
    render_list_summary(
        &mut out,
        &live(&entries),
        2,
        "whole room",
        jiff::Timestamp::UNIX_EPOCH,
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert_eq!(
        text.lines().next(),
        Some("6 agents · 2 lanes · 3 needs you · 1 running · 1 sleeping · 1 at rest · whole room")
    );
    assert!(text.contains("\nneeds you\n"), "{text}");
    for status in ["waiting", "paused", "failed"] {
        assert!(
            text.lines()
                .any(|line| line.contains(&format!("@{status}"))
                    && line.contains("provider overloaded")),
            "{text}"
        );
    }
    assert!(
        !text.lines().skip(1).any(|line| line.contains("@running")),
        "{text}"
    );
}

#[test]
fn card_projection_list_summary_omits_zero_counts_and_uses_ask() {
    let mut waiting = entry(AgentStatus::Waiting);
    waiting.ask = Some(super::super::report::AskReport {
        id: "ask_0123456789abcdef".parse().unwrap(),
        kind: rimz::agents::AskKind::Question,
        detail: Some("which strategy?".into()),
        native_key: None,
        since: jiff::Timestamp::UNIX_EPOCH,
    });
    waiting.needs_you.as_mut().unwrap().reason = Some("which strategy?".into());
    let mut out = anstream::StripStream::new(Vec::new());
    render_list_summary(
        &mut out,
        &live(&[waiting]),
        1,
        "#auth",
        jiff::Timestamp::UNIX_EPOCH,
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert_eq!(
        text.lines().next(),
        Some("1 agent · 1 lane · 1 needs you · #auth")
    );
    assert!(text.contains("which strategy?"), "{text}");
    assert!(
        text.lines().any(|line| line.split_whitespace().eq([
            "@waiting",
            "waiting",
            "0s",
            "which",
            "strategy?"
        ])),
        "{text}"
    );
    let mut out = Vec::new();
    render_list_summary(
        &mut out,
        &live(&[entry(AgentStatus::Success)]),
        1,
        "whole room",
        jiff::Timestamp::UNIX_EPOCH,
    )
    .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.trim(), "1 agent · 1 lane · 1 at rest · whole room");
}

#[test]
fn card_projection_list_tokens_are_window_occupancy_and_age_is_idle() {
    let mut report = entry(AgentStatus::Running);
    report.context.used_tokens = Some(81_234);
    report.stats.total_tokens = Some(3_000_000);
    report.timeline.idle_since = jiff::Timestamp::from_second(1880).unwrap();
    let mut table = render::Table::new([
        "HANDLE", "PROFILE", "AGENT", "STATUS", "MODEL", "CTX", "TOKENS", "AGE",
    ]);
    table.row(agent_row(
        &report,
        jiff::Timestamp::from_second(2000).unwrap(),
        &theme_glyphs(&ThemeConfig::default()),
    ));
    let mut out = anstream::StripStream::new(Vec::new());
    table.render(&mut out).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert!(text.contains("81k"), "{text}");
    assert!(!text.contains("3M"), "{text}");
    assert!(text.lines().last().unwrap().ends_with("2m"), "{text}");
}

#[test]
fn fast_model_cell_uses_the_resolved_glyph_without_marking_missing_models() {
    for theme in [
        ThemeConfig::default(),
        ThemeConfig {
            style: Some(rimz::config::ThemeStyle::Modern),
            ..Default::default()
        },
        toml::from_str("[glyphs.unicode.card]\nfast = '++'").unwrap(),
    ] {
        let glyph = theme_glyphs(&theme);
        let bolt = glyph(GlyphRole::CardFast);
        for fast in [true, false] {
            for model in [Some("gpt-6.1-sol"), None] {
                let mut state = test_agent("fast");
                state.model = model.map(str::to_owned);
                state.context = Some(rimz::agents::AgentContext {
                    fast: Some(fast),
                    ..Default::default()
                });
                let mut report = entry(AgentStatus::Idle);
                report.model = model_report(&state);
                let mut table = render::Table::new(["MODEL"]);
                table.row([agent_row(&report, jiff::Timestamp::UNIX_EPOCH, &glyph).remove(4)]);
                let mut out = anstream::StripStream::new(Vec::new());
                table.render(&mut out).unwrap();
                let text = String::from_utf8(out.into_inner()).unwrap();
                let expected = match (model, fast) {
                    (Some(_), true) => format!("GPT 6.1 Sol {bolt}"),
                    (Some(_), false) => "GPT 6.1 Sol".to_owned(),
                    (None, _) => "-".to_owned(),
                };
                assert_eq!(text.lines().last().unwrap().trim(), expected);
            }
        }
    }
}

#[test]
fn list_channel_filter_resolves_explicit_all_and_current_channel() {
    let mut root = test_agent("root");
    root.worktree_path = Some("/repo/checkout".into());
    root.root_lane = true;
    let filter = list_channel_filter_for_current(false, Some("#main"), None).unwrap();
    assert!(rimz::address::agent_in_lane(
        &root,
        &rimz::address::LaneSelector::parse(Some(&filter), None)
    ));
    assert_eq!(
        list_channel_filter_for_current(true, Some("manual"), Some("feature".to_owned()))
            .as_deref(),
        Some("manual")
    );
    assert_eq!(
        list_channel_filter_for_current(false, Some("#manual"), Some("feature".to_owned()))
            .as_deref(),
        Some("manual")
    );
    assert_eq!(
        list_channel_filter_for_current(true, None, Some("feature".to_owned())),
        None
    );
    assert_eq!(
        list_channel_filter_for_current(false, None, Some("feature".to_owned())).as_deref(),
        Some("feature")
    );
    assert_eq!(list_channel_filter_for_current(false, None, None), None);
}

#[test]
fn pane_bound_roots_keeps_only_live_roots() {
    let mut snapshot = SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::parse("ws_000000000000000000000000").expect("workspace id"),
        vec![
            test_agent("sess-one"),
            test_agent("sess-two"),
            test_agent("sess-paneless"),
        ],
        jiff::Timestamp::UNIX_EPOCH,
    );
    snapshot.agent_panes = vec![
        test_pane_agent("sess-one", "terminal_1"),
        test_pane_agent("sess-two", "terminal_2"),
        PaneAgent {
            checkout: None,
            root_lane: false,
            lane: None,
            kind: AgentKind::new_unchecked("codex"),
            kind_ordinal: None,
            name: None,
            name_explicit: false,
            profile: None,
            role: None,
            channel: None,
            agent_id: None,
            pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_lazy"),
            pane_pid: None,
            worktree_path: None,
            worktree_branch: None,
        },
    ]
    .into();

    let kept: Vec<&str> = snapshot
        .pane_bound_roots()
        .map(|agent| agent.agent_id.as_str())
        .collect();

    assert_eq!(kept, ["sess-one", "sess-two"]);
}

#[test]
fn agent_list_address_accounts_for_unprinted_addressable_roots() {
    let mut live = test_agent("sess-live");
    live.role = Some("coder".to_owned());
    let mut historical = test_agent("sess-historical");
    historical.role = Some("coder".to_owned());
    let mut snapshot = SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::parse("ws_000000000000000000000000").expect("workspace id"),
        vec![live, historical],
        jiff::Timestamp::UNIX_EPOCH,
    );
    snapshot.agent_panes = vec![test_pane_agent("sess-live", "terminal_1")].into();

    let peers: Vec<&AgentState> = snapshot.pane_bound_roots().collect();
    let rows = peers
        .iter()
        .map(|agent| {
            rimz::address::selector::AgentRow::of(
                agent,
                &snapshot,
                &rimz::agents::ParkDemotion::default(),
                &crate::cli::CurrentChannel::Unscoped.address_context(),
                jiff::Timestamp::UNIX_EPOCH,
            )
        })
        .collect::<Vec<_>>();
    let rows = rows.iter().collect::<Vec<_>>();
    let report = build_list_report(
        &rimz::agents::ParkDemotion::default(),
        &snapshot,
        &rows,
        jiff::Timestamp::UNIX_EPOCH,
        None,
        &[],
        &Default::default(),
    );
    assert_eq!(report.agents[0].handle, "@sess-live");
}

#[test]
fn model_label_prefers_live_context_then_durable_launch_fields() {
    let mut agent = test_agent("model-precedence");
    agent.model = Some("launch-model".to_owned());
    agent.effort = Some("launch-effort".to_owned());
    let mut context = rimz::agents::AgentContext::new("copilot", jiff::Timestamp::UNIX_EPOCH);
    context.model_id = Some("context-id".to_owned());
    context.model_display_name = Some("Context Display".to_owned());
    context.effort = Some("high".to_owned());
    agent.context = Some(context);
    assert_eq!(
        model_report(&agent).label.as_deref(),
        Some("Context Id@high")
    );

    agent.context.as_mut().unwrap().model_display_name = None;
    assert_eq!(
        model_report(&agent).label.as_deref(),
        Some("Context Id@high")
    );

    agent.context.as_mut().unwrap().model_id = None;
    assert_eq!(
        model_report(&agent).label.as_deref(),
        Some("Launch Model@high")
    );

    agent.context.as_mut().unwrap().effort = None;
    assert_eq!(
        model_report(&agent).label.as_deref(),
        Some("Launch Model@launch-effort")
    );

    agent.context = None;
    assert_eq!(
        model_report(&agent).label.as_deref(),
        Some("Launch Model@launch-effort")
    );
}

#[test]
fn json_entries_add_pr_only_to_agents_in_the_linked_group() {
    let now = jiff::Timestamp::UNIX_EPOCH;
    let mut linked = test_agent("linked");
    linked.channel = Some("feature".to_owned());
    linked.worktree_path = Some("/repo/worktrees/feature".to_owned());
    linked.worktree_branch = Some("feature".to_owned());
    let mut plain = test_agent("plain");
    plain.channel = Some("docs".to_owned());
    plain.worktree_path = Some("/repo/main".to_owned());
    plain.worktree_branch = Some("main".to_owned());
    let mut snapshot = SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(Path::new("/repo/main")),
        vec![linked, plain],
        now,
    )
    .with_project_root(Some(PathBuf::from("/repo/main")));
    let refs = snapshot.agents.iter().collect::<Vec<_>>();
    let (linked_key, linked_label, linked_kind) = {
        let group = group_live_agents_by_worktree(&refs, &snapshot)
            .into_iter()
            .find(|group| group.label == "feature")
            .unwrap();
        (group.key, group.label, group.kind)
    };
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups).push(
        serde_json::from_value(serde_json::json!({
            "key": linked_key,
            "label": linked_label,
            "kind": linked_kind,
            "status_counts": [],
            "rows": [],
            "pr_number": 91,
            "pr_state": "open",
            "ci": "passing"
        }))
        .unwrap(),
    );

    let refs = snapshot.agents.iter().collect::<Vec<_>>();
    let rows = refs
        .iter()
        .map(|agent| {
            rimz::address::selector::AgentRow::of(
                agent,
                &snapshot,
                &rimz::agents::ParkDemotion::default(),
                &crate::cli::CurrentChannel::Unscoped.address_context(),
                now,
            )
        })
        .collect::<Vec<_>>();
    let rows = rows.iter().collect::<Vec<_>>();
    let entries = build_list_report(
        &rimz::agents::ParkDemotion::default(),
        &snapshot,
        &rows,
        now,
        None,
        &[],
        &Default::default(),
    );
    let linked = entries
        .agents
        .iter()
        .find(|entry| entry.id.as_str() == "linked")
        .unwrap();
    let plain = entries
        .agents
        .iter()
        .find(|entry| entry.id.as_str() == "plain")
        .unwrap();

    assert_eq!(
        serde_json::to_value(linked).unwrap()["placement"]["pr"],
        serde_json::json!({"number": 91, "state": "open", "ci": "passing"})
    );
    assert_eq!(
        serde_json::to_value(plain).unwrap()["placement"]["pr"],
        serde_json::Value::Null
    );
}

#[test]
fn agent_pr_uses_projected_group_membership() {
    let mut linked = test_agent("linked");
    linked.worktree_path = Some("/repo/worktree".to_owned());
    linked.worktree_branch = Some("feature".to_owned());
    let mut stale = test_agent("stale");
    stale.worktree_path = Some("/repo/worktree".to_owned());
    stale.worktree_branch = Some("other".to_owned());
    let mut snapshot = SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::from_project_root(Path::new("/repo/main")),
        vec![linked, stale],
        jiff::Timestamp::UNIX_EPOCH,
    );
    let mut group: SidebarWorktreeGroup = serde_json::from_value(serde_json::json!({
        "key": "/repo/worktree",
        "label": "feature",
        "kind": "worktree",
        "status_counts": [],
        "rows": [],
        "pr_number": 91,
        "pr_state": "open",
        "ci": "passing"
    }))
    .unwrap();
    std::sync::Arc::make_mut(&mut group.rows).push(SidebarRow {
        checkout: None,
        id: "linked".to_owned(),
        name: "codex".to_owned(),
        pane: None,
        worktree_path: Some("/repo/worktree".to_owned()),
        worktree_branch: Some("feature".to_owned()),
        channel: None,
        lane: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        attention_at_ms: None,
        last_activity: jiff::Timestamp::UNIX_EPOCH,
        card: RowCard::Agent(std::sync::Arc::new(AgentCard {
            status: rimz::agents::AgentStatus::Idle,
            ..AgentCard::default()
        })),
    });
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups).push(group);

    assert_eq!(
        agent_pr(&snapshot, &snapshot.agents[0]),
        Some(PrInfo {
            number: Some(91),
            state: WorktreePrState::Open,
            ci: Some(WorktreeCi::Passing),
        })
    );
    assert_eq!(agent_pr(&snapshot, &snapshot.agents[1]), None);
}

fn test_agent(id: &str) -> AgentState {
    rimz::testkit::agent_state("codex", id, jiff::Timestamp::UNIX_EPOCH)
}

fn test_pane_agent(agent_id: &str, pane: &str) -> PaneAgent {
    PaneAgent {
        checkout: None,
        root_lane: false,
        lane: None,
        kind: AgentKind::new_unchecked("codex"),
        kind_ordinal: None,
        name: None,
        name_explicit: false,
        profile: None,
        role: None,
        channel: None,
        agent_id: Some(AgentSessionId::from(agent_id)),
        pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, pane),
        pane_pid: None,
        worktree_path: None,
        worktree_branch: None,
    }
}

#[test]
fn list_json_carries_each_row_flat_beside_its_detail() {
    let now = jiff::Timestamp::UNIX_EPOCH;
    let root = test_agent("sess-root");
    let mut child = test_agent("sess-child");
    child.parent_agent_id = Some("sess-root".into());
    child.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
    child.launch_depth = Some(1);
    let mut snapshot = SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::parse("ws_000000000000000000000000").expect("workspace id"),
        vec![root, child],
        now,
    );
    snapshot.agent_panes = vec![
        test_pane_agent("sess-root", "terminal_1"),
        test_pane_agent("sess-child", "terminal_2"),
    ]
    .into();
    let reader = crate::cli::CurrentChannel::Unscoped.address_context();
    let demotion = rimz::agents::ParkDemotion::default();
    let query = Query::parse(&["parent=*,none"]).unwrap();
    let rows = selector::live_rows(&snapshot, &demotion, &reader, now)
        .into_iter()
        .filter(|row| query.admits(row, &reader))
        .collect::<Vec<_>>();
    let rows = rows.iter().collect::<Vec<_>>();
    let report = build_list_report(
        &demotion,
        &snapshot,
        &rows,
        now,
        None,
        &[],
        &Default::default(),
    );
    let json = serde_json::to_value(AgentListJson::new(&report).unwrap()).unwrap();
    assert_eq!(json["schema"], 2);
    let entries = json["agents"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{json:#}");
    for entry in entries {
        for field in Field::ALL {
            assert!(
                entry.get(field.name()).is_some(),
                "{} in {entry:#}",
                field.name()
            );
        }
        assert!(entry.get("sub_agents").is_none(), "{entry:#}");
        assert!(entry["model_detail"].is_object(), "{entry:#}");
        assert!(!entry["model"].is_object(), "{entry:#}");
        assert!(entry["placement"].is_object() && entry["timeline"].is_object());
    }
    let child = entries
        .iter()
        .find(|entry| entry["id"] == "sess-child")
        .unwrap();
    let root = entries
        .iter()
        .find(|entry| entry["id"] == "sess-root")
        .unwrap();
    assert_eq!(child["parent"], root["handle"]);
    assert!(root["parent"].is_null());
    assert_eq!(child["live"], true);
    assert_eq!(child["account"], "default");
}

#[test]
fn filter_help_lists_every_field_with_its_type() {
    let mut out = anstream::StripStream::new(Vec::new());
    render::filter_help(&mut out, FILTER_DEFAULTS).unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    for field in Field::ALL {
        assert!(
            text.lines().any(|line| {
                line.split_whitespace()
                    .take(2)
                    .eq([field.name(), field.kind().label()])
            }),
            "{} missing from:\n{text}",
            field.name()
        );
    }
    assert!(text.contains("rate-limit, spend-limit"), "{text}");
}

#[test]
fn quiet_refuses_json_and_filter_repeats() {
    let quiet_json = crate::cli::Cli::try_parse_from(["rimz", "agents", "list", "-q", "--json"]);
    assert!(quiet_json.is_err());
    let repeated = crate::cli::Cli::try_parse_from([
        "rimz",
        "agents",
        "list",
        "--filter",
        "status=idle",
        "--filter",
        "parent=*",
        "-q",
    ]);
    assert!(repeated.is_ok(), "{repeated:?}");
}

#[test]
fn closed_entry_takes_its_row_not_the_live_card_for_the_same_session() {
    let now = jiff::Timestamp::UNIX_EPOCH;
    let workspace = rimz::WorkspaceId::parse("ws_000000000000000000000000").expect("workspace id");
    let mut live =
        SidebarSnapshot::build_with_agents(workspace.clone(), vec![test_agent("sess-old")], now);
    let mut group: SidebarWorktreeGroup = serde_json::from_value(serde_json::json!({
        "key": "/repo", "label": "repo", "kind": "worktree", "status_counts": [], "rows": []
    }))
    .unwrap();
    std::sync::Arc::make_mut(&mut group.rows).push(SidebarRow {
        checkout: None,
        id: "sess-old".to_owned(),
        name: "codex".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        channel: None,
        lane: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        attention_at_ms: None,
        last_activity: now,
        card: RowCard::Agent(std::sync::Arc::new(AgentCard {
            status: AgentStatus::Waiting,
            ..AgentCard::default()
        })),
    });
    std::sync::Arc::make_mut(&mut live.worktree_groups).push(group);
    let mut ended = test_agent("sess-old");
    ended.status = AgentStatus::Idle;
    ended.ended_at = Some(now);
    let audit = SidebarSnapshot::build_with_agents(workspace, vec![ended], now);
    let reader = crate::cli::CurrentChannel::Unscoped.address_context();
    let demotion = rimz::agents::ParkDemotion::default();
    let closed = selector::closed_rows(&audit, &live, &[], &demotion, &reader, now);
    let [row] = closed.as_slice() else {
        panic!("one closed row, got {}", closed.len());
    };
    let through_live = AgentRow::of(&live.agents[0], &live, &demotion, &reader, now);
    assert_ne!(row.card(), through_live.card(), "the fixture must disagree");

    let report = build_list_report(
        &demotion,
        &live,
        &[row],
        now,
        None,
        &[],
        &Default::default(),
    );
    let [entry] = report.agents.as_slice() else {
        panic!("one entry, got {}", report.agents.len());
    };
    assert_eq!((entry.status, entry.phase), row.card());
    assert_eq!(entry.handle, row.handle());
    assert_eq!(entry.needs_you, None);
}

#[test]
fn empty_line_under_a_filter_names_the_filter_and_no_resume() {
    let filters = vec!["status=idle".to_owned()];
    for lane in [Some("docs"), None] {
        let line = empty_list_line(lane, &filters);
        assert!(line.contains("matches the filter"), "{line}");
        assert!(!line.contains("resume"), "{line}");
    }
    assert!(empty_list_line(Some("docs"), &[]).contains("resume"));
}

#[test]
fn a_closed_row_counts_at_rest_whatever_its_status() {
    let running = entry(AgentStatus::Running);
    let mut out = anstream::StripStream::new(Vec::new());
    render_list_summary(
        &mut out,
        &[(&running, false)],
        1,
        "whole room",
        jiff::Timestamp::UNIX_EPOCH,
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    assert_eq!(text.trim(), "1 agent · 1 lane · 1 at rest · whole room");
}

fn launched_child(id: &str, parent: &str) -> AgentState {
    let mut child = test_agent(id);
    child.parent_agent_id = Some(parent.into());
    child.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
    child.launch_depth = Some(1);
    child
}

#[test]
fn a_child_needs_you_by_its_displayed_status_and_only_while_live() {
    let now = jiff::Timestamp::UNIX_EPOCH;
    let workspace = rimz::WorkspaceId::parse("ws_000000000000000000000000").expect("workspace id");
    let mut kid = launched_child("kid", "root");
    kid.status = AgentStatus::Running;
    let mut done = launched_child("done", "root");
    done.status = AgentStatus::Failed;
    done.ended_at = Some(now);
    let mut live =
        SidebarSnapshot::build_with_agents(workspace, vec![test_agent("root"), kid, done], now);
    live.agent_panes = ["root", "kid", "done"]
        .iter()
        .zip(["terminal_1", "terminal_2", "terminal_3"])
        .map(|(id, pane)| test_pane_agent(id, pane))
        .collect::<Vec<_>>()
        .into();
    let nested: rimz::store::snapshot::SidebarSubAgent =
        serde_json::from_value(serde_json::json!({
            "id": "kid", "kind": "codex", "name": "codex", "status": "failed", "last_activity": now,
        }))
        .unwrap();
    let mut group: SidebarWorktreeGroup = serde_json::from_value(serde_json::json!({
        "key": "/repo", "label": "repo", "kind": "worktree", "status_counts": [], "rows": []
    }))
    .unwrap();
    std::sync::Arc::make_mut(&mut group.rows).push(SidebarRow {
        checkout: None,
        id: "root".to_owned(),
        name: "codex".to_owned(),
        pane: None,
        worktree_path: None,
        worktree_branch: None,
        channel: None,
        lane: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        attention_at_ms: None,
        last_activity: now,
        card: RowCard::Agent(std::sync::Arc::new(AgentCard {
            status: AgentStatus::Idle,
            sub_agents: vec![nested],
            ..AgentCard::default()
        })),
    });
    std::sync::Arc::make_mut(&mut live.worktree_groups).push(group);
    let reader = crate::cli::CurrentChannel::Unscoped.address_context();
    let demotion = rimz::agents::ParkDemotion::default();
    let rows = selector::live_rows(&live, &demotion, &reader, now);
    let refs = rows.iter().collect::<Vec<_>>();
    let report = build_list_report(&demotion, &live, &refs, now, None, &[], &Default::default());
    let needs = |id: &str| {
        let entry = report
            .agents
            .iter()
            .find(|entry| entry.id.as_str() == id)
            .unwrap_or_else(|| panic!("no entry for {id}"));
        (
            entry.status,
            entry.needs_you.as_ref().map(|needs| needs.status),
        )
    };
    assert_eq!(
        needs("kid"),
        (AgentStatus::Failed, Some(AgentStatus::Failed)),
        "the nested card's status needs you"
    );
    assert_eq!(needs("done").1, None, "an ended child is at rest");
    assert_eq!(needs("root"), (AgentStatus::Idle, None));
}
