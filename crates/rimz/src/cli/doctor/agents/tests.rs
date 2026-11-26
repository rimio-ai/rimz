use super::*;

fn compaction_agent(window: u64, count: u64, max_window: Option<u64>) -> rimz::agents::AgentState {
    let mut agent = rimz::testkit::agent_state("codex", "compaction", jiff::Timestamp::now());
    agent.record = Some(Box::new(rimz::agents::LaunchRecord {
        auto_compact: Some(count),
        ..Default::default()
    }));
    agent.context = Some(rimz::agents::AgentContext {
        model_id: Some("gpt-6-sol".into()),
        model_context_limits: Some(rimz::agents::context::ModelContextLimits {
            model: "gpt-6-sol".into(),
            max_window,
            usable_percent: Some(95),
            own_compact_limit: None,
        }),
        tokens: Some(rimz::agents::AgentTokenUsage {
            context_window_size: Some(window),
            ..Default::default()
        }),
        ..Default::default()
    });
    agent
}

#[test]
fn opencode_never_warns_about_an_unsupported_compaction_count() {
    let mut agent = compaction_agent(983_010, 2_000_000, None);
    agent.kind = rimz::ids::AgentKind::new_unchecked("opencode");
    agent.usage.context_capacity = Some(1_048_576);
    assert!(agent_compaction_lowered(&agent).is_none());
}

#[test]
fn compaction_lowered_fix_raises_a_smaller_or_unknown_window() {
    let env = rimz::agents::session_login_env(&rimz::ids::AgentKind::new_unchecked("codex"), None)
        .unwrap();
    let config = rimz::agents::find_definition("codex")
        .unwrap()
        .config_home(&env)
        .unwrap()
        .join("config.toml");
    for max in [Some(872_000), None] {
        let agent = compaction_agent(258_400, 372_000, max);
        assert_eq!(
            agent_compaction_lowered(&agent).map(|lowered| lowered.fix),
            Some(format!(
                "set `model_context_window` in {}",
                config.display()
            ))
        );
    }
}

#[test]
fn compaction_lowered_fix_reduces_the_count_at_model_capacity() {
    let agent = compaction_agent(828_400, 1_000_000, Some(872_000));
    assert_eq!(
        agent_compaction_lowered(&agent).map(|lowered| lowered.fix),
        Some(
            "lower `auto-compact` to 784800 or less where this launch sets it: its team \
             role, its profile, its kind base, else `[harness] auto_compact`"
                .into()
        )
    );
    let mut profiled = agent;
    profiled.profile = Some("big".into());
    assert_eq!(
        agent_compaction_lowered(&profiled).map(|lowered| lowered.fix),
        Some(
            "lower `auto-compact` to 784800 or less where this launch sets it: its team \
             role, the `big` profile, its kind base, else `[harness] auto_compact`"
                .into()
        )
    );
}

#[test]
fn compaction_lowered_keeps_warning_when_account_path_is_unknown() {
    let mut agent = compaction_agent(258_400, 372_000, Some(872_000));
    agent.login = Some("missing-compaction-account".parse().unwrap());
    assert_eq!(
        agent_compaction_lowered(&agent).map(|lowered| (lowered.effective, lowered.fix)),
        Some((
            244_800,
            "set `model_context_window` in the codex@missing-compaction-account account's config.toml"
                .into(),
        ))
    );
}

#[test]
fn mod_missing_flags_live_claude_sessions_without_a_check_in() {
    let now = jiff::Timestamp::now();
    let state = |kind: &str, id: &str| {
        let mut agent = rimz::testkit::agent_state(kind, id, now);
        agent.name = Some(id.into());
        agent.name_explicit = true;
        agent
    };
    let silent = state("claude", "silent");
    let checked = state("claude", "checked");
    let codex = state("codex", "codex-session");
    let mut child = state("claude", "child");
    child.parent_agent_id = Some("silent".into());
    let mut ended = state("claude", "ended");
    ended.ended_at = Some(now);
    let mut launched = state("claude", "launched");
    launched.parent_agent_id = Some("silent".into());
    launched.launch_depth = Some(1);
    let starting = state("claude", "launch_starting");
    let mut failed = state("claude", "launch_failed");
    failed.status = rimz::agents::AgentStatus::Failed;
    let agents = [
        silent, checked, codex, child, ended, launched, starting, failed,
    ];
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
        agents.to_vec(),
        now,
    );
    let key = |agent: &rimz::agents::AgentState| (agent.kind.clone(), agent.agent_id.clone());
    let live = LiveSessions {
        keys: agents.iter().map(key).collect(),
        mod_checked_in: [&agents[1], &agents[4]]
            .map(|agent| (key(agent), check_in(agent, now)))
            .into(),
        ..Default::default()
    };
    for audit in [false, true] {
        let rollup = project_agent_rollup(
            &agents,
            &snapshot,
            &Default::default(),
            &rimz::address::AddressContext {
                channel: None,
                origin: rimz::address::ChannelOrigin::Root,
            },
            &mut Probe::Ready(Accounts { rows: vec![] }),
            audit,
            &live,
        );
        let wire = serde_json::to_value(rollup).unwrap();
        let flagged: Vec<_> = wire["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row.get("mod_missing") == Some(&serde_json::json!(true)))
            .map(|row| row["agent_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            flagged,
            ["launched", "silent"],
            "a launched child loads the mod; a provider-native child and a launch that never \
             registered have no session to load it (audit {audit})"
        );
    }
    let without_check_ins = LiveSessions {
        keys: live.keys.clone(),
        ..Default::default()
    };
    assert!(without_check_ins.mod_missing(&agents[1]));
    assert!(
        !without_check_ins.mod_missing(&agents[2]),
        "only Claude ships a mod"
    );
}

fn check_in(
    agent: &rimz::agents::AgentState,
    at: jiff::Timestamp,
) -> rimz::store::mod_checkin::ModCheckinRecord {
    rimz::store::mod_checkin::ModCheckinRecord {
        kind: agent.kind.clone(),
        agent_id: agent.agent_id.clone(),
        mod_version: "1.1.0".into(),
        provider_version: None,
        reported_at: at,
        first_reported_at: Some(at),
        readings_at: None,
        turn_report_at: None,
    }
}

#[test]
fn mod_signals_missing_flags_each_signal_a_finished_turn_lacked() {
    use super::super::model::ModSignal;
    let checked_in = jiff::Timestamp::now();
    let later = checked_in + jiff::SignedDuration::from_secs(5);
    let mut agent = rimz::testkit::agent_state("claude", "sess", checked_in);
    let key = (agent.kind.clone(), agent.agent_id.clone());
    let sessions = |record: rimz::store::mod_checkin::ModCheckinRecord| LiveSessions {
        keys: [key.clone()].into(),
        mod_checked_in: [(key.clone(), record)].into(),
        ..Default::default()
    };
    let bare = check_in(&agent, checked_in);
    let both = [ModSignal::Readings, ModSignal::TurnReport];
    assert!(
        sessions(bare.clone())
            .mod_signals_missing(&agent)
            .is_empty(),
        "no finished turn since the check-in owes nothing"
    );
    agent.turn_ended_at = Some(checked_in);
    assert!(
        sessions(bare.clone())
            .mod_signals_missing(&agent)
            .is_empty()
    );
    agent.turn_ended_at = Some(later);
    assert_eq!(sessions(bare.clone()).mod_signals_missing(&agent), both);
    let readings = rimz::store::mod_checkin::ModCheckinRecord {
        readings_at: Some(later),
        ..bare.clone()
    };
    assert_eq!(
        sessions(readings.clone()).mod_signals_missing(&agent),
        [ModSignal::TurnReport]
    );
    let turn = rimz::store::mod_checkin::ModCheckinRecord {
        turn_report_at: Some(later),
        ..bare.clone()
    };
    assert_eq!(
        sessions(turn).mod_signals_missing(&agent),
        [ModSignal::Readings]
    );
    let all = rimz::store::mod_checkin::ModCheckinRecord {
        turn_report_at: Some(later),
        ..readings
    };
    assert!(sessions(all).mod_signals_missing(&agent).is_empty());
    let unchecked = LiveSessions {
        keys: [key.clone()].into(),
        ..Default::default()
    };
    assert!(
        unchecked.mod_signals_missing(&agent).is_empty(),
        "a session that never checked in is flagged as mod_missing alone"
    );

    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
        vec![agent.clone()],
        later,
    );
    let rollup = project_agent_rollup(
        std::slice::from_ref(&agent),
        &snapshot,
        &Default::default(),
        &rimz::address::AddressContext {
            channel: None,
            origin: rimz::address::ChannelOrigin::Root,
        },
        &mut Probe::Ready(Accounts { rows: vec![] }),
        false,
        &sessions(bare),
    );
    let wire = serde_json::to_value(rollup).unwrap();
    assert_eq!(
        wire["rows"][0]["mod_signals_missing"],
        serde_json::json!(["readings", "turn_report"])
    );
}

/// A login whose policy blocks the mod shows the policy on its live sessions'
/// rows in place of a missing check-in or signal; another login keeps both.
#[test]
fn a_blocked_login_shows_its_policy_in_place_of_missing_mod_flags() {
    let now = jiff::Timestamp::now();
    let mut blocked = rimz::testkit::agent_state("claude", "blocked", now);
    blocked.turn_ended_at = Some(now + jiff::SignedDuration::from_secs(5));
    let mut other = blocked.clone();
    other.agent_id = "other".into();
    other.login = Some("work".parse().unwrap());
    let key = |agent: &rimz::agents::AgentState| (agent.kind.clone(), agent.agent_id.clone());
    let blocker = rimz::agents::ModBlocker {
        agent: "claude",
        key: "allowManagedModsOnly",
        file: "/etc/claude-code/managed-settings.json".into(),
        disables_hooks: false,
    };
    let live = LiveSessions {
        keys: [key(&blocked), key(&other)].into(),
        mod_checked_in: [&blocked, &other]
            .map(|agent| (key(agent), check_in(agent, now)))
            .into(),
        mod_blockers: [(blocked.login_key(), blocker.clone())].into(),
    };
    assert_eq!(live.mod_blocked(&blocked), Some(&blocker));
    assert!(!live.mod_missing(&blocked));
    assert!(live.mod_signals_missing(&blocked).is_empty());
    assert_eq!(live.mod_blocked(&other), None);
    assert_eq!(live.mod_signals_missing(&other).len(), 2);

    let agents = [blocked, other];
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
        agents.to_vec(),
        now,
    );
    let rollup = project_agent_rollup(
        &agents,
        &snapshot,
        &Default::default(),
        &rimz::address::AddressContext {
            channel: None,
            origin: rimz::address::ChannelOrigin::Root,
        },
        &mut Probe::Ready(Accounts { rows: vec![] }),
        false,
        &live,
    );
    let wire = serde_json::to_value(rollup).unwrap();
    let row = |id: &str| {
        wire["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["agent_id"] == id)
            .cloned()
            .unwrap()
    };
    let blocked = row("blocked");
    assert_eq!(
        blocked["mod_blocked"],
        serde_json::json!({
            "key": "allowManagedModsOnly",
            "file": "/etc/claude-code/managed-settings.json",
            "disables_hooks": false,
        })
    );
    assert_eq!(blocked.get("mod_signals_missing"), None);
    assert_eq!(row("other").get("mod_blocked"), None);
}

#[test]
fn compaction_lowered_keeps_live_healthy_rows_and_uses_the_latest_sidecar() {
    let now = jiff::Timestamp::now();
    let mut small = rimz::testkit::agent_state("codex", "small-window", now);
    small.name = Some("small-window".into());
    small.name_explicit = true;
    small.record = Some(Box::new(rimz::agents::LaunchRecord {
        auto_compact: Some(372_000),
        ..Default::default()
    }));
    let mut unlowered = small.clone();
    unlowered.agent_id = "large-window".into();
    let mut historical = small.clone();
    historical.agent_id = "historical-window".into();
    historical.ended_at = Some(now);
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
        vec![small.clone(), unlowered.clone(), historical.clone()],
        now,
    );
    small.context = Some(rimz::agents::AgentContext {
        model_id: Some("gpt-6-sol".into()),
        model_context_limits: Some(rimz::agents::context::ModelContextLimits {
            model: "gpt-6-sol".into(),
            max_window: Some(872_000),
            usable_percent: Some(95),
            own_compact_limit: None,
        }),
        tokens: Some(rimz::agents::AgentTokenUsage {
            context_window_size: Some(258_400),
            ..Default::default()
        }),
        ..Default::default()
    });
    unlowered.context = small.context.clone();
    unlowered
        .context
        .as_mut()
        .unwrap()
        .tokens
        .as_mut()
        .unwrap()
        .context_window_size = Some(828_400);
    historical.context = small.context.clone();
    let live = LiveSessions {
        keys: [
            (small.kind.clone(), small.agent_id.clone()),
            (unlowered.kind.clone(), unlowered.agent_id.clone()),
        ]
        .into(),
        ..Default::default()
    };
    for audit in [false, true] {
        let rollup = project_agent_rollup(
            &[small.clone(), unlowered.clone(), historical.clone()],
            &snapshot,
            &Default::default(),
            &rimz::address::AddressContext {
                channel: None,
                origin: rimz::address::ChannelOrigin::Root,
            },
            &mut Probe::Ready(Accounts { rows: vec![] }),
            audit,
            &live,
        );
        let wire = serde_json::to_value(rollup).unwrap();
        let warnings: Vec<_> = wire["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row.get("compaction_lowered"))
            .cloned()
            .collect();
        assert_eq!(
            warnings,
            [serde_json::json!({"configured": 372_000,
            "effective": 244_800, "model": "gpt-6-sol", "raw_window": 272_000,
            "max_window": 872_000})]
        );
        assert_eq!(
            wire["rows"].as_array().unwrap().len(),
            if audit { 3 } else { 1 }
        );
    }
}

#[test]
fn integration_file_identity_resolves_dangling_directory_links() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let default = dir.path().join("default");
    let named = dir.path().join("named");
    std::fs::create_dir(&default).unwrap();
    std::fs::create_dir(&named).unwrap();
    symlink(default.join("rules"), named.join("absolute")).unwrap();
    symlink("../default/rules", named.join("relative")).unwrap();
    symlink("relative", named.join("chained")).unwrap();

    let expected = integration_file_identity(&default.join("rules/nested/rimz.rules"));
    let identities = ["absolute", "relative", "chained"]
        .map(|link| integration_file_identity(&named.join(link).join("nested/rimz.rules")));
    assert_eq!(identities, [expected.clone(), expected.clone(), expected]);

    symlink("cycle", named.join("cycle")).unwrap();
    let cyclic = named.join("cycle/rimz.rules");
    assert_eq!(integration_file_identity(&cyclic), cyclic);
}

#[test]
fn soft_compact_skips_list_only_detected_kinds_with_their_reasons() {
    let hooks = ["qoder", "codex", "amp", "muse", "kiro", "antigravity"].map(|kind| HookRow {
        kind: kind.into(),
        detected: kind != "kiro",
        status: HookStatus::Installed,
    });
    assert_eq!(
        serde_json::to_value(collect_soft_compact_skips(&hooks)).unwrap(),
        serde_json::json!([
            {"kind": "amp", "reason": "no_compact_command"},
            {"kind": "antigravity", "reason": "no_hook_context"},
            {"kind": "muse", "reason": "no_compaction_signal"},
            {"kind": "qoder", "reason": "no_live_context_reading"},
        ])
    );
    assert!(collect_soft_compact_skips(&[]).is_empty());
    assert!(collect_soft_compact_skips(&hooks[1..2]).is_empty());
}

#[test]
fn prompt_cache_fallback_lists_detected_kinds_using_any_sign_in_mode() {
    let hooks: Vec<_> = ["custom", "codex", "amp", "claude", "gemini"]
        .map(|kind| HookRow {
            kind: kind.into(),
            detected: kind != "gemini",
            status: HookStatus::Installed,
        })
        .into();
    let actual: Vec<_> = [
        ("", "1h", vec!["amp", "custom"]),
        ("[prompt_cache_ttl]\namp = 'off'", "1h", vec!["custom"]),
        (
            "[prompt_cache_ttl]\namp = {subscription = '2h', api-key = '3h'}",
            "1h",
            vec!["amp", "custom"],
        ),
        (
            "[prompt_cache_ttl]\nfallback = 'off'",
            "off",
            vec!["amp", "custom"],
        ),
        (
            "[prompt_cache_ttl]\nfallback = '2h'",
            "2h",
            vec!["amp", "custom"],
        ),
    ]
    .map(|(config, ttl, kinds)| {
        let harness = toml::from_str(config).unwrap();
        let summary = collect_prompt_cache_fallback(&hooks, &harness);
        let empty = collect_prompt_cache_fallback(&[], &harness);
        (
            serde_json::to_value(summary).unwrap()
                == serde_json::json!({"ttl": ttl, "kinds": kinds}),
            serde_json::to_value(empty).unwrap() == serde_json::json!({"ttl": ttl, "kinds": []}),
        )
    })
    .into();
    assert_eq!(actual, [(true, true); 5]);
}

#[test]
fn account_config_remedies_are_separate_from_the_wire_problem() {
    use rimz::agents::LoginConfigErr;
    let kind = rimz::ids::AgentKind::new_unchecked("codex");
    for (error, remedy) in [
        (
            LoginConfigErr::ReservedName { kind: kind.clone() },
            "remove",
        ),
        (
            LoginConfigErr::AmbiguousHome {
                kind: kind.clone(),
                name: "work".parse().unwrap(),
                home: "/home/one,two".into(),
            },
            "choose",
        ),
        (
            LoginConfigErr::DuplicateHome {
                kind,
                name: "work".parse().unwrap(),
                first: "other".parse().unwrap(),
                home: "/home/work".into(),
            },
            "give",
        ),
    ] {
        let text = error.to_string();
        let problem = super::super::model::AccountProblem::from(error);
        assert!(!problem.summary.contains(remedy), "{problem:?}");
        assert!(problem.fix.contains(remedy), "{problem:?}");
        assert_eq!(serde_json::to_value(problem).unwrap(), text);
    }
}

#[test]
fn empty_dir_io_errors_do_not_prescribe_removing_files() {
    let error = rimz::agents::empty_dir::EmptyDirErr::Io {
        path: "/empty".into(),
        source: std::io::ErrorKind::PermissionDenied.into(),
    };
    assert!(empty_dir_fix(&error).contains("repair access"));
    assert!(
        empty_dir_fix(&rimz::agents::empty_dir::EmptyDirErr::Dirty(
            "/empty/file".into()
        ))
        .contains("remove unexpected files")
    );
}

#[test]
fn rowless_budget_park_uses_the_published_agent() {
    let now = jiff::Timestamp::now();
    let mut raw = rimz::testkit::agent_state("codex", "session", now);
    raw.status = AgentStatus::Running;
    let mut published = raw.clone();
    published.budget_park = Some(rimz::agents::BudgetPark {
        cap_usd: 1.0,
        spend_usd: 2.0,
        window: rimz::agents::BudgetWindow::Session,
        at: now,
        scope: Default::default(),
        account_kind: None,
        resets_at: None,
        reset_window: None,
    });
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
        vec![published],
        now,
    );
    let rollup = project_agent_rollup(
        &[raw.clone()],
        &snapshot,
        &Default::default(),
        &rimz::address::AddressContext {
            channel: None,
            origin: rimz::address::ChannelOrigin::Root,
        },
        &mut Probe::Ready(Accounts { rows: vec![] }),
        false,
        &LiveSessions {
            keys: [(raw.kind, raw.agent_id)].into(),
            ..Default::default()
        },
    );
    let AgentRollup::Observed { counts, rows } = rollup else {
        panic!("missing rollup")
    };
    assert_eq!(counts.paused, 1);
    assert_eq!(rows[0].status, AgentStatus::Paused);
}

#[test]
fn newer_published_activity_keeps_the_named_handle() {
    let now = jiff::Timestamp::now();
    let mut raw = rimz::testkit::agent_state("codex", "session", now);
    raw.name = Some("otter".into());
    raw.name_explicit = true;
    let mut published = raw.clone();
    published.last_activity = now + jiff::SignedDuration::from_secs(1);
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
        vec![published],
        now,
    );
    let rollup = project_agent_rollup(
        &[raw.clone()],
        &snapshot,
        &Default::default(),
        &rimz::address::AddressContext {
            channel: None,
            origin: rimz::address::ChannelOrigin::Root,
        },
        &mut Probe::Ready(Accounts { rows: vec![] }),
        true,
        &LiveSessions {
            keys: [(raw.kind, raw.agent_id)].into(),
            ..Default::default()
        },
    );
    let AgentRollup::Observed { rows, .. } = rollup else {
        panic!("missing rollup")
    };
    assert_eq!(rows[0].handle, "@otter");
}

#[test]
fn invalid_account_names_do_not_read_default_login_usage() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = rimz::RuntimePaths::under(
        rimz::ids::WorkspaceId::from_project_root(dir.path()),
        dir.path(),
    )
    .unwrap();
    runtime.ensure_dirs().unwrap();
    let cache = rimz::agents::account::RateLimitsCache {
        entries: std::collections::BTreeMap::from([(
            rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked("codex")).into(),
            rimz::agents::account::RateLimitCacheEntry {
                limits: rimz::agents::AgentRateLimits {
                    windows: vec![rimz::agents::RateLimitWindow {
                        observed_at: Some(jiff::Timestamp::now()),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    std::fs::write(
        rimz::agents::RateLimitsCache::shared_path(&runtime),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
    let mut rows = vec![AccountRow {
        kind: "codex".into(),
        name: "not@a-login".into(),
        home: None,
        room: false,
        machine_default: false,
        default_for: Default::default(),
        problem: None,
        outdated: None,
        usage: None,
    }];
    collect_account_usage(&mut rows, &runtime, &rimz::config::MachineConfig::default());
    assert!(
        rows[0].usage.is_none(),
        "invalid login inherited default usage"
    );
}

#[test]
fn account_rows_mark_machine_defaults_and_report_dangling_selections() {
    let config: rimz::config::AccountsConfig =
        toml::from_str("[codex.work]\n[use]\ncodex = \"work\"\nclaude = \"missing\"\n").unwrap();
    let catalog = rimz::agents::LoginCatalog::from_config(&config).unwrap();
    let rows = account_rows(
        &catalog,
        None,
        &std::collections::BTreeMap::new(),
        &config.use_accounts,
    );
    let rows = serde_json::to_value(rows).unwrap();
    let rows = rows.as_array().unwrap();
    let work = rows.iter().find(|row| row["name"] == "work").unwrap();
    assert_eq!(work["machine_default"], true);
    let missing = rows
        .iter()
        .find(|row| row["name"] == "missing")
        .expect("dangling machine selection");
    assert_eq!(missing["machine_default"], true);
    assert!(
        missing["problem"]
            .as_str()
            .unwrap()
            .contains("rimz accounts use --global claude default")
    );
}

#[test]
fn account_rows_flag_an_exported_home_outside_the_room_born_on_it() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("rimio");
    std::fs::create_dir(&home).unwrap();
    let config: rimz::config::AccountsConfig = toml::from_str(&format!(
        "[codex.rimio]\nhome = {:?}\n",
        home.display().to_string()
    ))
    .unwrap();
    let catalog = rimz::agents::LoginCatalog::from_config(&config).unwrap();
    let ambient = std::collections::BTreeMap::from([
        ("HOME".to_owned(), "/home/u".to_owned()),
        ("CODEX_HOME".to_owned(), home.display().to_string()),
    ]);
    let problem = |room: Option<&rimz::agents::RoomAccounts>| {
        let rows = account_rows(&catalog, room, &ambient, &config.use_accounts);
        let rows = serde_json::to_value(rows).unwrap();
        rows[0]["problem"].as_str().map(str::to_owned)
    };

    let outside = problem(None).expect("an exported account home is a problem");
    assert!(outside.contains("unset `CODEX_HOME`"), "{outside}");
    let room: rimz::agents::RoomAccounts = rimz::ids::RoomLogins::from([(
        rimz::ids::AgentKind::new_unchecked("codex"),
        "rimio".parse().unwrap(),
    )])
    .into();
    let inside = problem(Some(&room)).unwrap_or_default();
    assert!(!inside.contains("CODEX_HOME"), "{inside}");
}

#[test]
fn collect_trust_surfaces_project_effective_load_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".rimz")).unwrap();
    std::fs::write(
        dir.path().join(".rimz/config.toml"),
        "[tiers]\nsenior = [\"claude\"]\n",
    )
    .unwrap();
    rimz::trust::grant(dir.path()).unwrap();
    let ws = rimz::WorkspaceResolver::resolve(dir.path(), None).unwrap();
    let Probe::Ready(trust) = collect_trust(&ws) else {
        panic!("expected Ready trust probe");
    };
    assert!(
        trust
            .definition_errors
            .iter()
            .any(|err| err.contains("tiers")),
        "trusted project load error must surface in doctor trust definition_errors: {:?}",
        trust.definition_errors
    );
}
