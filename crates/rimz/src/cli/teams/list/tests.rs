use super::*;
use rimz::agents::AgentStatus;
use rimz::config::RoleBinding;

fn snapshot(agents: Vec<AgentState>) -> SidebarSnapshot {
    SidebarSnapshot::build_with_agents(
        rimz::WorkspaceId::parse("ws_000000000000000000000000").unwrap(),
        agents,
        jiff::Timestamp::UNIX_EPOCH,
    )
}

fn team() -> Team {
    Team {
        roles: vec![RoleBinding {
            auto_compact: None,
            role: "planner".to_owned(),
            profile: "claude".to_owned(),
            mode: None,
            model: Some("fable".to_owned()),
            effort: Some("high".to_owned()),
            budget: None,
            system_prompt_file: Some("planner.md".into()),
            append_system_prompt_files: vec!["consensus.md".into()],
            args: None,
        }],
        leader: Some("planner".to_owned()),
        layout: None,
        scratch_files: Vec::new(),
        stages: Vec::new(),
        signals: Vec::new(),
    }
}

#[test]
fn team_signal_report_matches_origin_and_session() {
    let agent = AgentState::stub("claude", "sess-planner", AgentStatus::Running);
    let entry = TaskEntry {
        team: Some("forge#feat-x".parse().unwrap()),
        wake: Some(rimz::config::TaskTarget {
            kind: rimz::ids::AgentKind::new_unchecked("claude"),
            session: "sess-planner".into(),
            handle: "@old-handle".to_owned(),
        }),
        signal: Some("ci.failed".to_owned()),
        matches: Some(BTreeMap::from([(
            "path".to_owned(),
            "/repo/feat-x".to_owned(),
        )])),
        ..TaskEntry::default()
    };
    let report = live_signal(
        "binding",
        &entry,
        TaskSource::Instance,
        "forge#feat-x",
        &agent,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(report).unwrap(),
        serde_json::json!({
            "name": "binding", "selector": "ci.failed", "matches": {"path": "/repo/feat-x"}
        })
    );
    for instance in ["forge#feat-y", "peer#feat-x"] {
        assert!(live_signal("binding", &entry, TaskSource::Instance, instance, &agent).is_none());
    }
    for source in [
        TaskSource::Config,
        TaskSource::Project {
            state: rimz::trust::TrustState::Trusted,
        },
    ] {
        assert!(live_signal("binding", &entry, source, "forge#feat-x", &agent).is_none());
    }
    let mut wrong_kind = entry.clone();
    wrong_kind.wake.as_mut().unwrap().kind = rimz::ids::AgentKind::new_unchecked("codex");
    let mut old_session = entry.clone();
    old_session.wake.as_mut().unwrap().session = "previous-planner".into();
    let mut manual = entry.clone();
    manual.team = None;
    let mut timer = entry.clone();
    timer.signal = None;
    let mut spawn = entry;
    spawn.wake = None;
    for entry in [wrong_kind, old_session, manual, timer, spawn] {
        assert!(
            live_signal(
                "binding",
                &entry,
                TaskSource::Instance,
                "forge#feat-x",
                &agent
            )
            .is_none()
        );
    }
}

#[test]
fn catalog_projects_cohort_observability_by_worktree() {
    use rimz::store::snapshot::{AgentCard, RowCard, SidebarRow, SidebarWorktreeGroup};

    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(
        first.join("blackboard.md"),
        "# Board\nStage: Implement (delta) (@coder)\n",
    )
    .unwrap();
    std::fs::write(first.join("plan-notes.md"), "one\ntwo\nthree\n").unwrap();
    let mut definition = team();
    definition.stages = vec!["Plan".into(), "Implement".into()];
    definition.scratch_files = vec!["/blackboard.md".into(), "/*-notes.md".into()];
    let teams = TeamsConfig(BTreeMap::from([("forge".into(), definition)]));
    let mut agents = Vec::new();
    let mut groups = Vec::new();
    for (lane, path, number) in [("first", &first, 41), ("second", &second, 42)] {
        let mut agent = AgentState::stub("codex", lane, AgentStatus::Running);
        agent.team = Some("forge".into());
        agent.role = Some("coder".into());
        agent.channel = Some(lane.into());
        agent.worktree_path = Some(path.join(".").to_string_lossy().into_owned());
        agent.worktree_branch = Some(format!("branch-{lane}"));
        agent.last_activity = jiff::Timestamp::UNIX_EPOCH;
        agent.last_seen = jiff::Timestamp::from_second(600).unwrap();
        let mut group: SidebarWorktreeGroup = serde_json::from_value(serde_json::json!({
            "key": lane, "label": lane, "kind": "worktree", "status_counts": [], "rows": [],
            "pr_number": number, "pr_ci": "passing"
        }))
        .unwrap();
        group.rows.push(SidebarRow {
            id: agent.agent_id.to_string(),
            name: lane.into(),
            pane: None,
            worktree_path: Some(path.to_string_lossy().into_owned()),
            worktree_branch: agent.worktree_branch.clone(),
            channel: Some(lane.into()),
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity: agent.last_activity,
            card: RowCard::Agent(Box::new(AgentCard {
                description: Some(format!("reading {lane}.rs")),
                ..AgentCard::default()
            })),
        });
        groups.push(group);
        agents.push(agent);
    }
    let mut snapshot = snapshot(agents);
    snapshot.worktree_groups = groups;
    let build = |snapshot: &SidebarSnapshot| {
        build_catalog(
            &teams,
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            LiveCatalog {
                tasks: &BTreeMap::new(),
                snapshot,
                audit_agents: &[],
                lifetimes: &rimz::worktree::lane_lifetimes([]),
                prices: &rimz::agents::PriceBook::default(),
                worktree: None,
            },
            |_| None,
        )
    };
    let reports = build(&snapshot);
    let first_report = &reports[0].instances[0];
    assert_eq!(first_report.worktree.as_deref(), Some(first.as_path()));
    assert_eq!(first_report.branch.as_deref(), Some("branch-first"));
    assert_eq!(
        first_report.stage.as_ref().unwrap().name,
        "Implement (delta)"
    );
    assert_eq!(
        first_report.stage.as_ref().unwrap().owner.as_deref(),
        Some("coder")
    );
    assert_eq!(first_report.pr.as_ref().unwrap().number, Some(41));
    assert_eq!(first_report.pr.as_ref().unwrap().state, None);
    assert_eq!(first_report.memory.len(), 2);
    assert_eq!(first_report.memory[1].lines, 3);
    assert!(
        first_report
            .memory
            .iter()
            .all(|file| file.path.is_absolute() && file.modified_at.is_some())
    );
    assert_eq!(
        first_report.members[0].activity.as_deref(),
        Some("reading first.rs")
    );
    assert_eq!(
        first_report.members[0].last_activity_at,
        jiff::Timestamp::UNIX_EPOCH
    );
    assert_eq!(
        reports[0].instances[1].pr.as_ref().unwrap().number,
        Some(42)
    );
    assert!(reports[0].instances[1].stage.is_none());
    assert!(reports[0].instances[1].memory.is_empty());
    let json = serde_json::to_value(&reports).unwrap();
    assert_eq!(
        json[0]["instances"][0]["members"][0]["last_activity_at"],
        "1970-01-01T00:00:00Z"
    );
    assert!(json[0]["instances"][0]["members"][0]["phase"].is_string());
    assert!(json[0]["instances"][1]["stage"].is_null());
    let mut rendered = anstream::StripStream::new(Vec::new());
    write_catalog(&mut rendered, &reports, &ThemeConfig::default()).unwrap();
    insta::assert_snapshot!(
        "human_catalog_has_one_row_per_cohort",
        String::from_utf8(rendered.into_inner()).unwrap()
    );

    let mut conflicting = snapshot.agents[0].clone();
    conflicting.agent_id = "conflicting".into();
    conflicting.worktree_path = Some(second.to_string_lossy().into_owned());
    conflicting.worktree_branch = Some("other".into());
    snapshot.agents.push(conflicting);
    let conflicting = build(&snapshot);
    let instance = &conflicting[0].instances[0];
    assert!(instance.worktree.is_none());
    assert!(instance.branch.is_none());
    assert!(instance.stage.is_none());
    assert!(instance.pr.is_none());
    assert!(instance.memory.is_empty());
}

#[test]
fn catalog_merges_definition_and_live_instance() {
    let mut definition = team();
    definition.signals.push(rimz::config::TeamSignalBinding {
        signal: "ci.failed".to_owned(),
        role: "planner".to_owned(),
        matches: BTreeMap::from([("branch".to_owned(), "feat-x".to_owned())]),
        prompt: None,
    });
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), definition)]));
    let mut agent = AgentState::stub("claude", "sess-planner", AgentStatus::Running);
    agent.team = Some("forge".to_owned());
    agent.role = Some("planner".to_owned());
    agent.channel = Some("feat-x".to_owned());
    let snapshot = snapshot(vec![agent]);
    let reports = build_catalog(
        &teams,
        &ProfilesConfig::default(),
        &CommandsConfig::default(),
        LiveCatalog {
            tasks: &BTreeMap::new(),
            snapshot: &snapshot,
            audit_agents: &[],
            lifetimes: &rimz::worktree::lane_lifetimes([]),
            prices: &rimz::agents::PriceBook::default(),
            worktree: None,
        },
        |_| Some("/tmp/team.toml".to_owned()),
    );

    assert_eq!(reports.len(), 1);
    assert!(reports[0].valid);
    assert_eq!(reports[0].roles[0].model.as_deref(), Some("fable"));
    assert_eq!(reports[0].instances[0].channel, "feat-x");
    assert_eq!(reports[0].instances[0].members.len(), 1);
    assert_eq!(reports[0].instances[0].state, "working");
    let json = serde_json::to_value(&reports).unwrap();
    assert_eq!(
        json[0]["roles"][0]["signals"],
        serde_json::json!([{
            "signal": "ci.failed", "match": {"branch": "feat-x"}, "prompt": null
        }])
    );
    assert_eq!(json[0]["instances"][0]["members"][0]["role"], "planner");
    assert_eq!(
        json[0]["instances"][0]["members"][0]["signals"],
        serde_json::json!([])
    );
    assert_eq!(json[0]["roles"][0]["system_prompt_file"], "planner.md");
    assert_eq!(
        json[0]["roles"][0]["append_system_prompt_files"],
        serde_json::json!(["consensus.md"])
    );
    assert_eq!(json[0]["instances"][0]["members"][0]["handle"], "@planner");
    assert_eq!(json[0]["instances"][0]["members"][0]["status"], "running");
    assert!(json[0]["instances"][0]["members"][0].get("phase").is_some());

    let removed = build_catalog(
        &TeamsConfig::default(),
        &ProfilesConfig::default(),
        &CommandsConfig::default(),
        LiveCatalog {
            tasks: &BTreeMap::new(),
            snapshot: &snapshot,
            audit_agents: &[],
            lifetimes: &rimz::worktree::lane_lifetimes([]),
            prices: &rimz::agents::PriceBook::default(),
            worktree: None,
        },
        |_| None,
    );
    assert!(!removed[0].defined);
    assert_eq!(removed[0].instances[0].channel, "feat-x");
    let mut rendered = anstream::StripStream::new(Vec::new());
    write_catalog(&mut rendered, &removed, &ThemeConfig::default()).unwrap();
    let rendered = String::from_utf8(rendered.into_inner()).unwrap();
    assert!(rendered.contains("working · not defined"));
}

#[test]
fn live_member_cost_comes_from_its_audit_slot() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("opencode.db");
    let connection = rusqlite::Connection::open(&transcript).unwrap();
    connection
        .execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)")
        .unwrap();
    connection
        .execute(
            "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
            (
                "message",
                "sess-planner",
                r#"{"cost":0.25,"modelID":"gpt","providerID":"openai","time":{"created":1780394400000},"tokens":{"input":10,"output":2,"cache":{"read":3,"write":4}}}"#,
            ),
        )
        .unwrap();
    drop(connection);
    let mut agent = AgentState::stub("opencode", "sess-planner", AgentStatus::Running);
    agent.team = Some("forge".to_owned());
    agent.role = Some("planner".to_owned());
    agent.channel = Some("feat-x".to_owned());
    agent.transcript_path = Some(transcript.to_string_lossy().into_owned());
    let lifetimes = rimz::worktree::lane_lifetimes(std::iter::once(&agent));
    let reports = build_catalog(
        &TeamsConfig(BTreeMap::from([("forge".to_owned(), team())])),
        &ProfilesConfig::default(),
        &CommandsConfig::default(),
        LiveCatalog {
            tasks: &BTreeMap::new(),
            snapshot: &snapshot(vec![agent.clone()]),
            audit_agents: &[agent],
            lifetimes: &lifetimes,
            prices: &rimz::agents::PriceBook::default(),
            worktree: None,
        },
        |_| None,
    );

    assert_eq!(reports[0].instances[0].members[0].cost_usd, Some(0.25));
}

#[test]
fn live_member_cost_counts_only_the_current_lane_lifetime() {
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join("feat-x");
    let git_dir = worktree.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    let marker = rimz::worktree::WorktreeMarker {
        version: 4,
        name: "feat-x".to_owned(),
        branch: "feat-x".to_owned(),
        base_branch: Some("main".to_owned()),
        from_pr: None,
        base_ref: "main".to_owned(),
        repo_root: dir.path().to_path_buf(),
        worktree_path: worktree.clone(),
        created_at: "2026-06-02T10:00:00Z".parse().unwrap(),
    };
    rimz::disk::atomic::write_temp_then_rename(&git_dir.join("rimz-worktree.json"), &marker)
        .unwrap();
    let transcript = dir.path().join("opencode.db");
    let connection = rusqlite::Connection::open(&transcript).unwrap();
    connection
        .execute_batch("CREATE TABLE message (id TEXT, session_id TEXT, data TEXT)")
        .unwrap();
    for (session, cost) in [("old-planner", 10.0), ("current-planner", 0.25)] {
        let data = serde_json::json!({
            "cost": cost,
            "modelID": "gpt",
            "providerID": "openai",
            "time": { "created": 1780394400000_i64 },
            "tokens": { "input": 10, "output": 2, "cache": { "read": 3, "write": 4 } }
        });
        connection
            .execute(
                "INSERT INTO message (id, session_id, data) VALUES (?1, ?2, ?3)",
                (session, session, data.to_string()),
            )
            .unwrap();
    }
    drop(connection);
    let mut current = AgentState::stub("opencode", "current-planner", AgentStatus::Running);
    current.team = Some("forge".to_owned());
    current.role = Some("planner".to_owned());
    current.channel = Some("feat-x".to_owned());
    current.worktree_path = Some(worktree.to_string_lossy().into_owned());
    current.registered_at = Some("2026-06-02T10:00:01Z".parse().unwrap());
    current.transcript_path = Some(transcript.to_string_lossy().into_owned());
    let mut old = AgentState::stub("opencode", "old-planner", AgentStatus::Success);
    old.team = current.team.clone();
    old.role = current.role.clone();
    old.channel = current.channel.clone();
    old.worktree_path = current.worktree_path.clone();
    old.registered_at = Some("2026-06-02T09:59:59Z".parse().unwrap());
    old.transcript_path = current.transcript_path.clone();
    let audit_agents = [old, current.clone()];
    let build = || {
        let lifetimes = rimz::worktree::lane_lifetimes(audit_agents.iter());
        build_catalog(
            &TeamsConfig(BTreeMap::from([("forge".to_owned(), team())])),
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            LiveCatalog {
                tasks: &BTreeMap::new(),
                snapshot: &snapshot(vec![current.clone()]),
                audit_agents: &audit_agents,
                lifetimes: &lifetimes,
                prices: &rimz::agents::PriceBook::default(),
                worktree: None,
            },
            |_| None,
        )
    };
    let reports = build();
    let instance = &reports[0].instances[0];
    assert_eq!(instance.members.len(), 1);
    assert_eq!(instance.members[0].cost_usd, Some(0.25));

    std::fs::write(git_dir.join("rimz-worktree.json"), "invalid marker").unwrap();
    let unreadable = build();
    let unreadable_instance = &unreadable[0].instances[0];
    assert_eq!(unreadable_instance.members.len(), 1);
    assert_eq!(unreadable_instance.members[0].cost_usd, None);
    assert_eq!(
        unreadable_instance.members[0].handle,
        instance.members[0].handle
    );
    assert_eq!(
        unreadable_instance.members[0].status,
        instance.members[0].status
    );
    assert_eq!(unreadable_instance.channel, instance.channel);
    assert_eq!(unreadable_instance.state, instance.state);
    assert_eq!(unreadable_instance.status_counts, instance.status_counts);

    std::fs::remove_dir_all(&worktree).unwrap();
    let removed = build();
    let removed_instance = &removed[0].instances[0];
    assert_eq!(removed_instance.members.len(), 1);
    assert_eq!(removed_instance.members[0].cost_usd, None);
    assert_eq!(
        removed_instance.members[0].handle,
        instance.members[0].handle
    );
    assert_eq!(
        removed_instance.members[0].status,
        instance.members[0].status
    );
    assert_eq!(removed_instance.channel, instance.channel);
    assert_eq!(removed_instance.state, instance.state);
    assert_eq!(removed_instance.status_counts, instance.status_counts);
    assert!(transcript.exists());
}

#[test]
fn invalid_team_stays_visible_with_its_error() {
    let mut broken = team();
    broken.roles[0].profile = "missing".to_owned();
    broken.signals.push(rimz::config::TeamSignalBinding {
        signal: "ci.failed".to_owned(),
        role: "planner".to_owned(),
        matches: BTreeMap::new(),
        prompt: Some("Fix CI".to_owned()),
    });
    let reports = build_catalog(
        &TeamsConfig(BTreeMap::from([("broken".to_owned(), broken)])),
        &ProfilesConfig::default(),
        &CommandsConfig::default(),
        LiveCatalog {
            tasks: &BTreeMap::new(),
            snapshot: &snapshot(Vec::new()),
            audit_agents: &[],
            lifetimes: &rimz::worktree::lane_lifetimes([]),
            prices: &rimz::agents::PriceBook::default(),
            worktree: None,
        },
        |_| None,
    );

    assert!(!reports[0].valid);
    assert_eq!(reports[0].roles[0].signals[0].signal, "ci.failed");
    assert_eq!(
        reports[0].roles[0].signals[0].prompt.as_deref(),
        Some("Fix CI")
    );
    assert!(
        reports[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("unknown profile"))
    );

    let mut invalid_signal = team();
    invalid_signal
        .signals
        .push(rimz::config::TeamSignalBinding {
            signal: "ci.failed".to_owned(),
            role: "builder".to_owned(),
            matches: BTreeMap::new(),
            prompt: None,
        });
    let report = definition_report(
        "forge",
        &invalid_signal,
        &ProfilesConfig::default(),
        &CommandsConfig::default(),
        None,
        Vec::new(),
    );
    assert!(!report.valid);
    assert_eq!(
        report.error.as_deref(),
        Some("team `forge` signal binding 1 targets undeclared role `builder`")
    );
}

#[test]
fn live_instance_state_follows_team_attention_priority() {
    for (other, expected) in [
        ("waiting", "blocked"),
        ("failed", "blocked"),
        ("paused", "paused"),
        ("running", "working"),
        ("success", "sleeping"),
        ("idle", "sleeping"),
    ] {
        assert_eq!(
            instance_state(&BTreeMap::from([
                ("sleeping".to_owned(), 1),
                (other.to_owned(), 1),
            ])),
            expected,
            "sleeping with {other}"
        );
    }
    assert_eq!(
        instance_state(&BTreeMap::from([
            ("running".to_owned(), 2),
            ("failed".to_owned(), 1),
        ])),
        "blocked"
    );
    assert_eq!(
        instance_state(&BTreeMap::from([
            ("success".to_owned(), 1),
            ("paused".to_owned(), 1),
        ])),
        "paused"
    );
    assert_eq!(
        instance_state(&BTreeMap::from([("success".to_owned(), 2)])),
        "done"
    );
}

#[test]
fn human_catalog_and_empty_state_teach_the_command() {
    let reports = build_catalog(
        &TeamsConfig(BTreeMap::from([("forge".to_owned(), team())])),
        &ProfilesConfig::default(),
        &CommandsConfig::default(),
        LiveCatalog {
            tasks: &BTreeMap::new(),
            snapshot: &snapshot(Vec::new()),
            audit_agents: &[],
            lifetimes: &rimz::worktree::lane_lifetimes([]),
            prices: &rimz::agents::PriceBook::default(),
            worktree: None,
        },
        |_| None,
    );
    let mut rendered = Vec::new();
    write_catalog(&mut rendered, &reports, &ThemeConfig::default()).unwrap();
    let rendered = String::from_utf8(rendered).unwrap();
    assert!(rendered.contains("forge"));
    assert!(rendered.contains("STAGE"));
    assert!(rendered.contains("ready"));

    let mut empty = Vec::new();
    write_catalog(&mut empty, &[], &ThemeConfig::default()).unwrap();
    let empty = String::from_utf8(empty).unwrap();
    assert!(empty.contains("rimz teams install forge"));
    assert!(empty.contains("docs/guide/teams.md"));

    let mut built_in_only = Vec::new();
    write_catalog(
        &mut built_in_only,
        &[TeamReport {
            name: "peer".to_owned(),
            defined: true,
            source: Some("built-in".to_owned()),
            layout: Some("claude,codex".to_owned()),
            leader: Some("claude".to_owned()),
            roles: Vec::new(),
            valid: true,
            error: None,
            instances: Vec::new(),
        }],
        &ThemeConfig::default(),
    )
    .unwrap();
    assert!(
        String::from_utf8(built_in_only)
            .unwrap()
            .contains("No installed teams")
    );
}

#[test]
fn catalog_filter_matches_an_exact_lane_or_member_worktree() {
    let teams = TeamsConfig(BTreeMap::from([("forge".to_owned(), team())]));
    let mut agent = AgentState::stub("claude", "sess-planner", AgentStatus::Running);
    agent.team = Some("forge".to_owned());
    agent.role = Some("planner".to_owned());
    agent.channel = None;
    agent.worktree_path = Some("/repo-worktrees/feat-x".to_owned());
    let build = |filter| {
        build_catalog(
            &teams,
            &ProfilesConfig::default(),
            &CommandsConfig::default(),
            LiveCatalog {
                tasks: &BTreeMap::new(),
                snapshot: &snapshot(vec![agent.clone()]),
                audit_agents: &[],
                lifetimes: &rimz::worktree::lane_lifetimes([]),
                prices: &rimz::agents::PriceBook::default(),
                worktree: filter,
            },
            |_| None,
        )
    };

    assert_eq!(build(Some("feat-x"))[0].instances[0].channel, "feat-x");
    assert_eq!(
        build(Some("/repo-worktrees/feat-x"))[0].instances[0].channel,
        "feat-x"
    );
    assert!(build(Some("other"))[0].instances.is_empty());
}
