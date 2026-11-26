use super::*;
use crate::agents::SessionOrigin;
use crate::agents::account::AccountsCache;
use crate::agents::{AgentState, AgentStatus};
use crate::disk::atomic;
use crate::forge::pr_state::PrLink;
use crate::ids::LinkTier;
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use crate::remote::link::{LinkStats, LinkStatsFile};
use crate::sidebar::refresh::daemon_reap::{CodexDaemonReap, codex_daemon_reap_path};
use crate::sidebar::refresh::git_stats::{DiffStatsCache, DiffStatsCacheEntry};
use crate::sidebar::test_support::{activity_row, pane, root_agent, worktree_group};
use crate::utils::time::unix_now_ms;
use jiff::SignedDuration;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

mod agents;
mod capacity;
mod cohort;
mod frame;
mod git;
mod held;
mod labels;
mod spend;
mod typing;
mod workflows;

#[test]
fn hook_drainer_outages_are_projected_on_producer_and_consumer_folds() {
    let (_dir, runtime, snapshot) = runtime();
    let dir = runtime.hook_drainer_outages_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let mut held = Vec::new();
    for (pid, body, hold) in [
        (
            1,
            serde_json::json!({
                "v": 1, "agent_kind": "claude", "agent_name": "coder", "since_ms": 200
            }),
            true,
        ),
        (
            2,
            serde_json::json!({
                "v": 1, "agent_kind": "codex", "agent_name": null, "since_ms": 100
            }),
            true,
        ),
        (
            3,
            serde_json::json!({
                "v": 1, "agent_kind": "claude", "agent_name": "dead", "since_ms": 50
            }),
            false,
        ),
        (4, serde_json::json!({"broken": true}), true),
        (
            5,
            serde_json::json!({
                "v": 2, "agent_kind": "claude", "agent_name": "future", "since_ms": 25
            }),
            true,
        ),
    ] {
        let file = std::fs::File::create(dir.join(pid.to_string())).unwrap();
        serde_json::to_writer(&file, &body).unwrap();
        if hold {
            file.try_lock().unwrap();
            held.push(file);
        }
    }
    let expected = serde_json::json!({
        "since": Timestamp::from_millisecond(100).unwrap(),
        "agents": [
            {"agent_kind": "codex", "agent_name": null},
            {"agent_kind": "claude", "agent_name": "coder"}
        ]
    });
    for opts in [cached_opts(), producing_opts()] {
        let projected = fold(snapshot.clone(), None, &runtime, opts);
        assert_eq!(
            serde_json::to_value(&projected).unwrap()["hook_drainer_outage"],
            expected,
            "both fold paths must project only held, parseable outage records in start order"
        );
    }
    drop(held);
    let mut stale = serde_json::to_value(&snapshot).unwrap();
    stale["hook_drainer_outage"] = expected;
    let cleared = fold_cached(serde_json::from_value(stale).unwrap(), None, &runtime);
    assert!(
        serde_json::to_value(cleared)
            .unwrap()
            .get("hook_drainer_outage")
            .is_none(),
        "free leases must clear a previously published outage"
    );
}

#[test]
fn hook_drain_lag_is_projected_on_both_folds_past_the_threshold() {
    use crate::store::ingress::{self, HookDrainCursor, HookIngress};

    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    state.ensure_dirs().unwrap();
    std::fs::create_dir_all(state.hook_drain_cursor.parent().unwrap()).unwrap();
    let fed = |ago: i64| HookIngress {
        soft_compact_claim: None,
        boot_id: None,
        schema_version: "1".into(),
        event_id: crate::ids::EventId::new(),
        ts: snapshot.now - SignedDuration::from_secs(ago),
        source: crate::ids::AgentKind::new_unchecked("claude"),
        event: None,
        payload: "{}".into(),
        cwd: "/tmp/project".into(),
        hook_pid: 1,
        env: BTreeMap::new(),
    };
    let append = |frame: &HookIngress| {
        let lock = crate::disk::lock::SharedAppendLock::acquire(&state.hook_ingress_lock).unwrap();
        ingress::append(&state, frame, &lock).unwrap()
    };
    let cursor = |end: u64, frame: &HookIngress| {
        let cursor = HookDrainCursor {
            applied: end,
            claimed: end,
            applied_event_id: Some(frame.event_id.clone()),
            claimed_event_id: Some(frame.event_id.clone()),
            applied_frames: Some(12),
            ..HookDrainCursor::default()
        };
        std::fs::write(
            &state.hook_drain_cursor,
            serde_json::to_vec(&cursor).unwrap(),
        )
        .unwrap();
    };
    let projected = |opts| {
        serde_json::to_value(fold(snapshot.clone(), None, &runtime, opts))
            .unwrap()
            .get("hook_drain_lag")
            .cloned()
    };
    for opts in [cached_opts(), producing_opts()] {
        assert_eq!(projected(opts), None, "an empty tail has no lag");
    }
    let applied = fed(600);
    let applied_end = append(&applied);
    let oldest = fed(60);
    let oldest_end = append(&oldest);
    cursor(applied_end, &applied);
    for opts in [cached_opts(), producing_opts()] {
        assert_eq!(
            projected(opts),
            Some(serde_json::json!({"since": oldest.ts, "applied_frames": 12})),
            "both fold paths must date the oldest unapplied frame"
        );
    }
    cursor(oldest_end, &oldest);
    for opts in [cached_opts(), producing_opts()] {
        assert_eq!(projected(opts), None, "a drained tail has no lag");
    }
    append(&fed(5));
    for opts in [cached_opts(), producing_opts()] {
        assert_eq!(projected(opts), None, "a fresh head is under the threshold");
    }
}

#[test]
fn workflow_parks_do_not_publish_attention_notices() {
    let (_dir, runtime, snapshot) = runtime();
    let state = StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap();
    for (suffix, status, entered) in [
        (1, "parked", "2026-01-01T00:01:00Z"),
        (2, "parked", "2026-01-01T00:00:00Z"),
        (3, "active", "2026-01-01T00:00:00Z"),
        (4, "done", "2026-01-01T00:00:00Z"),
    ] {
        let id = crate::ids::WorkflowRunId::parse(&format!(
            "wf_0190b8172c007000800000000000000{suffix}"
        ))
        .unwrap();
        let paths = state.workflow(&id);
        std::fs::create_dir_all(&paths.home).unwrap();
        std::fs::write(&paths.record, serde_json::to_vec(&serde_json::json!({
            "version":crate::store::workflow::VERSION,"id":id,"workflow":"review","spec_dir":"/definitions/review",
            "scope":{"kind":"worktree","path":"/repo"},"project_root":"/repo","created_at":entered,"state":status,
            "park":{"reason":"asked","text":"Which fix?","at":entered},"node":"triage","entered_at":entered,
            "visits":{},"deadline_fired":false,"next_seq":1
        })).unwrap()).unwrap();
    }
    let projected = serde_json::to_value(fold_cached(snapshot, None, &runtime)).unwrap();
    assert!(projected.get("parked_workflows").is_none(), "{projected}");
}

#[test]
fn provider_labels_include_only_non_default_logins() {
    let accounts = toml::from_str("[claude.work]\nhome = \"/srv/rimz-test-work\"\n").unwrap();
    let key: crate::ids::LoginKey = "claude@work".parse().unwrap();
    let work = crate::agents::RoomLoginSet::new(
        Some(crate::ids::RoomLogins::from([(key.kind.clone(), key.name.clone())]).into()),
        Some(crate::agents::LoginCatalog::from_config(&accounts).unwrap()),
        BTreeMap::new(),
    );
    let (_dir, runtime, _) = runtime();
    let pool = crate::ids::LoginKey::default_for(key.kind.clone());
    let mut pool_spending = crate::agents::spending::ProviderSpendingCache::default();
    pool_spending.spending.by_login.insert(
        pool.clone(),
        crate::agents::SpendTally {
            week: crate::agents::spending::SpendWindow {
                usd: 12.0,
                sessions: 1,
                ..Default::default()
            },
            ..Default::default()
        },
    );
    // `rimz accounts` folds once per account, each under a selection naming it.
    for (login, logins, expected) in [
        (
            crate::ids::LoginKey::default_for(key.kind.clone()),
            crate::agents::RoomLoginSet::native(),
            "Claude",
        ),
        (key, work, "Claude · work"),
    ] {
        let accounts = BTreeMap::from([(login, metered_account())]);
        let panels = provider_panels_from_caches(
            &runtime,
            &logins,
            &Default::default(),
            accounts,
            &pool_spending,
        );
        let names: Vec<_> = panels.iter().map(|panel| &panel.product_name).collect();
        assert_eq!(names, [expected]);
        assert_eq!(
            panels[0].spending.as_deref(),
            pool_spending.spending.by_login.get(&pool),
            "a shared account's panel reads its pool's spend"
        );
    }
}

#[test]
fn account_label_from_machine_config_reaches_cached_panels() {
    let key: crate::ids::LoginKey = "codex@team-1".parse().unwrap();
    let mut config = crate::config::MachineConfig::default();
    config.accounts.codex.insert(
        key.name.clone(),
        crate::config::NamedAccount {
            home: Some(PathBuf::from("/srv/rimz-test-team")),
            label: Some("work".to_owned()),
            ..Default::default()
        },
    );
    let logins = crate::agents::RoomLoginSet::new(
        Some(crate::ids::RoomLogins::from([(key.kind.clone(), key.name.clone())]).into()),
        Some(crate::agents::LoginCatalog::from_config(&config.accounts).unwrap()),
        BTreeMap::new(),
    );
    let (_dir, runtime, _) = runtime();
    let panels = provider_panels_from_caches(
        &runtime,
        &logins,
        &config,
        BTreeMap::from([(key.clone(), metered_account())]),
        &Default::default(),
    );
    assert_eq!(panels[0].product_name, "Codex · work");
    assert_eq!(panels[0].login_key(), key);
}

#[test]
fn dashboard_shows_only_the_rooms_current_account_per_provider() {
    let config: crate::config::MachineConfig =
        toml::from_str("[accounts.codex.team-1]\nhome = \"/srv/rimz-test-team\"\n").unwrap();
    let team: crate::ids::LoginKey = "codex@team-1".parse().unwrap();
    let old = crate::ids::LoginKey::default_for(team.kind.clone());
    let mut new = root_agent("codex", "new-1", None);
    new.login = Some(team.name.clone());
    let mut agents = vec![
        root_agent("claude", "c1", None),
        root_agent("codex", "old-1", None),
        root_agent("codex", "old-2", None),
        new,
    ];
    let panes: Vec<_> = agents
        .iter_mut()
        .enumerate()
        .map(|(idx, agent)| {
            let live = pane(
                &format!("terminal_{idx}"),
                agent.kind.as_str(),
                "/repo/main",
            );
            agent.pane = Some(live.clone());
            live
        })
        .collect();
    let (_dir, runtime, snapshot) = runtime();
    let snapshot =
        SidebarSnapshot::build_with_agents(snapshot.workspace_id.clone(), agents, snapshot.now)
            .with_live_panes(panes, None);
    let lanes = crate::sidebar::refresh::RefreshedLanes {
        spending: Default::default(),
        accounts: BTreeMap::from([
            (old.clone(), metered_account()),
            (team.clone(), metered_account()),
        ]),
        pr_states: BTreeMap::new(),
        branch_ci: BTreeMap::new(),
    };
    let room = |selection: Option<crate::ids::RoomLogins>| {
        crate::agents::RoomLoginSet::new(
            selection.map(Into::into),
            Some(crate::agents::LoginCatalog::from_config(&config.accounts).unwrap()),
            BTreeMap::new(),
        )
        .with_agents(&snapshot.agents)
    };
    let switched = room(Some(crate::ids::RoomLogins::from([(
        team.kind.clone(),
        team.name.clone(),
    )])));
    let panels = |logins: &crate::agents::RoomLoginSet| {
        fold_machine_config(
            snapshot.clone(),
            &runtime,
            &config,
            &config.remote_control,
            logins,
            Default::default(),
            Some(&lanes),
        )
        .0
        .providers
        .into_iter()
        .map(|panel| (panel.product_name, panel.active_sessions))
        .collect::<BTreeSet<_>>()
    };

    assert_eq!(
        panels(&switched),
        BTreeSet::from([("Claude".to_owned(), 1), ("Codex · team-1".to_owned(), 1)]),
        "the old account's live agents and probe earn no block and no count"
    );
    assert_eq!(
        panels(&room(Some(Default::default()))),
        BTreeSet::from([("Claude".to_owned(), 1), ("Codex".to_owned(), 2)])
    );
    assert!(
        panels(&room(None)).is_empty(),
        "an unreadable room record names no current account"
    );

    // The producer's scoped fold keeps every in-use login for its cache writes.
    let in_use = fold_machine_config_with(
        snapshot.clone(),
        &config,
        lanes.accounts.clone(),
        &BTreeMap::new(),
        Default::default(),
        &config.remote_control,
        &switched,
        PanelScope::InUse,
    );
    let names: BTreeSet<_> = in_use
        .providers
        .iter()
        .map(|panel| panel.product_name.as_str())
        .collect();
    assert_eq!(names, BTreeSet::from(["Claude", "Codex", "Codex · team-1"]));
}

fn metered_account() -> crate::agents::AgentAccount {
    crate::agents::AgentAccount {
        metered: Some(true),
        ..Default::default()
    }
}

#[test]
fn entitlement_projection_overrides_cached_max_and_fresh_windows_only_for_its_login() {
    let (_dir, runtime, _) = runtime();
    let key: crate::ids::LoginKey = "claude@default".parse().unwrap();
    let other: crate::ids::LoginKey = "claude@work".parse().unwrap();
    let account = crate::agents::AgentAccount {
        plan: Some("max".to_owned()),
        ..metered_account()
    };
    crate::sidebar::refresh::usage::publish_account_usage_snapshot(
        &runtime,
        &key,
        Default::default(),
        crate::agents::AccountUsageSnapshot {
            rate_limits: Some(crate::agents::AgentRateLimits {
                windows: vec![crate::agents::RateLimitWindow {
                    used_percentage: Some(7),
                    duration_mins: Some(300),
                    resets_at: Some(Timestamp::now() + SignedDuration::from_hours(1)),
                    ..Default::default()
                }],
            }),
            extra_credits: Some(crate::agents::ExtraCredits::Disabled),
            ..Default::default()
        },
        1,
    );
    let entry = serde_json::json!({
        "observed_at_ms": 0,
        "ok": false,
        "entitlement": {"lapsed":{"since_ms":100}},
    });
    for (lapsed_key, applies) in [(other, false), (key.clone(), true)] {
        atomic::write_temp_then_rename_cache(
            &runtime.shared_credits_path(),
            &serde_json::json!({"refreshed_at_ms":0,"logins":{lapsed_key.to_string():entry}}),
        )
        .unwrap();
        let panels = provider_panels_from_caches(
            &runtime,
            &crate::agents::RoomLoginSet::native(),
            &Default::default(),
            BTreeMap::from([(key.clone(), account.clone())]),
            &Default::default(),
        );
        let panel = &panels[0];
        if applies {
            assert_eq!(panel.plan, None);
            assert!(panel.windows.is_empty());
            assert_eq!(panel.extra_credits, None);
            assert_eq!(panel.reset_credits, None);
            assert_eq!(
                serde_json::to_value(panel).unwrap()["entitlement"],
                entry["entitlement"]
            );
        } else {
            assert_eq!(panel.plan_label().as_deref(), Some("Claude Max"));
            assert_eq!(panel.windows.len(), 1);
        }
    }
}

fn runtime() -> (tempfile::TempDir, RuntimePaths, SidebarSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let snapshot = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    (dir, runtime, snapshot)
}

fn cached_opts() -> FoldOpts<'static> {
    FoldOpts {
        producing: false,
        fresh_roots: None,
        config: None,
        lanes: None,
        agent_projection: Default::default(),
    }
}

fn producing_opts() -> FoldOpts<'static> {
    FoldOpts {
        producing: true,
        config: Some(std::sync::Arc::new(crate::config::MachineConfig::default())),
        ..cached_opts()
    }
}

/// One fold with the arguments every test shares: no messages dir, no excluded
/// pane, diagnostics off.
fn fold(
    snapshot: SidebarSnapshot,
    frame: Option<&crate::sidebar::frame::PaneFrame>,
    runtime: &RuntimePaths,
    opts: FoldOpts<'_>,
) -> SidebarSnapshot {
    enrich(
        snapshot,
        frame,
        &StatePaths::under(runtime.workspace_id.clone(), &runtime.root).unwrap(),
        runtime,
        None,
        None,
        opts,
        &crate::diag::DiagSink::disabled(),
    )
}

fn fold_cached(
    snapshot: SidebarSnapshot,
    frame: Option<&crate::sidebar::frame::PaneFrame>,
    runtime: &RuntimePaths,
) -> SidebarSnapshot {
    fold(snapshot, frame, runtime, cached_opts())
}

fn fold_producing(
    snapshot: SidebarSnapshot,
    frame: Option<&crate::sidebar::frame::PaneFrame>,
    runtime: &RuntimePaths,
) -> SidebarSnapshot {
    fold(snapshot, frame, runtime, producing_opts())
}

#[test]
fn fold_stamps_the_machine_config_onto_the_rendered_snapshot() {
    let (_dir, runtime, snapshot) = runtime();
    let mut config = crate::config::MachineConfig::default();
    config.sidebar.trunk = Some("develop".into());
    config.theme.display.provider_list = vec!["codex".into()];
    config.agents.attention.stalled_after_secs = std::num::NonZeroU32::new(2700).unwrap();
    let opts = FoldOpts {
        config: Some(std::sync::Arc::new(config.clone())),
        ..producing_opts()
    };

    let folded = fold(snapshot, None, &runtime, opts);

    assert_eq!(folded.sidebar, config.sidebar);
    assert_eq!(folded.theme, config.theme);
    assert_eq!(folded.attention, config.agents.attention);
}

#[test]
fn claude_host_serving_needs_a_pane_the_provider_still_stands_behind() {
    use crate::agents::runtime_control::RuntimeControlLiveness::{Down, Unknown, Up};
    use crate::sidebar::enrich::claude_host_serving;

    let cases = [
        (true, true, Up, true, "pane up and the host confirms it"),
        (
            true,
            true,
            Down,
            false,
            "the pane outlived the server that answered for it",
        ),
        (
            true,
            true,
            Unknown,
            true,
            "no record yet is not evidence of failure",
        ),
        (false, true, Up, false, "no pane, no host"),
        (
            true,
            false,
            Down,
            true,
            "a disabled host is not judged on its record",
        ),
    ];

    for (pane_present, enabled, liveness, expected, label) in cases {
        assert_eq!(
            claude_host_serving(pane_present, enabled, liveness),
            expected,
            "{label}"
        );
    }
}

#[test]
fn remote_control_badge_follows_enablement_and_probe_health() {
    use crate::store::snapshot::RemoteControlBadge::{Down, Healthy, Hidden, OffForRun};

    let cases = [
        (true, false, Some(true), Healthy, "configured and up"),
        (true, false, Some(false), Down, "configured and down"),
        (true, false, None, Healthy, "configured before first probe"),
        (
            false,
            true,
            Some(true),
            Healthy,
            "pane auto with live probe",
        ),
        (
            false,
            true,
            Some(false),
            Healthy,
            "pane auto ignores server probe",
        ),
        (
            true,
            true,
            Some(false),
            Down,
            "configured host wins over pane auto",
        ),
        (false, false, Some(false), Hidden, "disabled"),
    ];

    for (config_toggle, pane_auto, server_alive, expected, label) in cases {
        assert_eq!(
            remote_control_badge(false, config_toggle, pane_auto, server_alive),
            expected,
            "{label}"
        );
    }

    // Machine config on, room waived: muted, whatever the stale probe says.
    for server_alive in [Some(true), Some(false), None] {
        assert_eq!(
            remote_control_badge(true, false, false, server_alive),
            OffForRun,
            "waived host, probe {server_alive:?}"
        );
    }
    assert_eq!(
        remote_control_badge(true, false, true, None),
        Healthy,
        "the provider's own pane auto-enable still shows"
    );
}

#[test]
fn provider_store_adapters_are_wired_for_identityless_idle_cards() {
    let wired = crate::sidebar::agent_projection::probe_current();
    assert!(wired.kinds.iter().any(|kind| kind == "antigravity"));
    assert!(wired.kinds.iter().any(|kind| kind == "kiro"));
}

fn diff_entry(
    clean: bool,
    landed: bool,
    did_work: Option<bool>,
    ahead: u32,
    behind: u32,
) -> DiffStatsCacheEntry {
    DiffStatsCacheEntry {
        refreshed_at_ms: 0,
        commit_refreshed_at_ms: Some(0),
        added: Some(0),
        removed: Some(0),
        commits: Some(ahead),
        behind: Some(behind),
        trunk: Some("main".to_owned()),
        branch: Some("feature".to_owned()),
        clean: Some(clean),
        landed: Some(landed),
        did_work,
        merge_in_progress: Some(false),
        ..DiffStatsCacheEntry::default()
    }
}

fn write_worktree_marker(path: &Path, name: &str) {
    let git_dir = path.join(".git");
    std::fs::create_dir_all(&git_dir).unwrap();
    let marker = crate::worktree::WorktreeMarker {
        loop_fire: None,
        version: 1,
        name: name.to_owned(),
        branch: name.to_owned(),
        base_branch: Some("main".to_owned()),
        from_pr: None,
        base_ref: "HEAD".to_owned(),
        repo_root: path.to_path_buf(),
        worktree_path: path.to_path_buf(),
        created_at: Timestamp::now(),
    };
    atomic::write_temp_then_rename(&git_dir.join("rimz-worktree.json"), &marker).unwrap();
}

fn channel_group(label: &str, path: &Path) -> crate::store::snapshot::SidebarWorktreeGroup {
    let mut group = worktree_group(
        path,
        vec![activity_row(false, None, Timestamp::now(), path)],
    );
    group.key = format!("channel:{label}");
    group.label = label.to_owned();
    group.kind = SidebarWorktreeKind::Channel;
    group.place = None;
    group
}

/// A one-channel snapshot rooted in a fresh tempdir, with the worktree
/// directory created and the `.git` marker written when `marked`.
fn channel_snapshot(name: &str, marked: bool) -> (tempfile::TempDir, PathBuf, SidebarSnapshot) {
    let dir = tempfile::tempdir().unwrap();
    let worktree = dir.path().join(name);
    std::fs::create_dir_all(&worktree).unwrap();
    if marked {
        write_worktree_marker(&worktree, name);
    }
    let mut snapshot = SidebarSnapshot::build(
        WorkspaceId::from_project_root(dir.path()),
        Vec::new(),
        Timestamp::now(),
    );
    snapshot.worktree_groups = vec![channel_group(name, &worktree)].into();
    if marked {
        Arc::make_mut(&mut snapshot.worktree_groups)[0].place =
            Some(crate::store::snapshot::SidebarPlace {
                path: worktree.clone(),
                foreign: false,
            });
    }
    (dir, worktree, snapshot)
}

#[test]
fn trunk_sync_classifier_uses_marker_and_local_git_state() {
    let mut reconciling = diff_entry(true, true, Some(true), 0, 0);
    reconciling.merge_in_progress = Some(true);

    let cases = [
        (
            "clean, no work: never diverged from trunk",
            diff_entry(true, true, Some(false), 0, 0),
            "feature",
            Some(WorktreeTrunkSync::Pristine),
        ),
        (
            "dirty worktree diverges",
            diff_entry(false, true, Some(false), 0, 0),
            "feature",
            Some(WorktreeTrunkSync::Diverged),
        ),
        (
            "landed work ahead of trunk is merged",
            diff_entry(true, true, Some(true), 2, 5),
            "feature",
            Some(WorktreeTrunkSync::Merged),
        ),
        (
            "an in-flight merge is reconciling",
            reconciling,
            "feature",
            Some(WorktreeTrunkSync::Reconciling),
        ),
        (
            "trunk checkout is exempt",
            diff_entry(true, true, Some(true), 0, 0),
            "main",
            None,
        ),
        (
            "unmarked worktrees stay conservative",
            diff_entry(true, true, None, 0, 0),
            "feature",
            Some(WorktreeTrunkSync::Diverged),
        ),
        (
            "fresh fork behind trunk is not pristine",
            diff_entry(true, true, Some(false), 0, 1),
            "feature",
            Some(WorktreeTrunkSync::Diverged),
        ),
    ];

    for (label, entry, branch, expected) in cases {
        assert_eq!(
            classify_trunk_sync(&entry, branch, "main"),
            expected,
            "{label}"
        );
    }
}

fn stats(rtt_ms: Option<u32>, miss_pct: u16) -> LinkStats {
    LinkStats {
        rtt_ms,
        miss_pct,
        window: 30,
    }
}

fn codex_root(id: &str, worktree: &str, pane_id: &str) -> AgentState {
    let mut agent = root_agent("codex", id, None);
    agent.worktree_path = Some(worktree.to_owned());
    agent.pane = Some(pane(pane_id, "codex", worktree));
    agent
}

fn binding_log_lines(state: &crate::StatePaths) -> usize {
    let path = state.audit_path("binding.log.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count()
}

fn snapshot_now_ms(snapshot: &SidebarSnapshot) -> u64 {
    snapshot.now.as_millisecond().max(0) as u64
}

/// The config fold stamps every *agent* row's context-severity verdict from
/// the `[theme.display.context_meter]` bands — the one classification the renderer's color
/// ramp and any future signal emitter read — and leaves process rows `None`.
#[test]
fn config_fold_stamps_agent_context_severity() {
    let path = Path::new("/repo/main");
    let agent_row = |pct: Option<u8>| {
        let mut row = activity_row(true, Some(AgentStatus::Running), Timestamp::now(), path);
        row.as_agent_mut().unwrap().usage.context_pct = pct;
        row
    };
    let mut scaled_row = agent_row(Some(8));
    let card = scaled_row.as_agent_mut().unwrap();
    card.auto_compact = Some(100_000);
    card.usage.context_window = Some(1_000_000);
    let mut groups = vec![worktree_group(
        path,
        vec![
            agent_row(Some(85)),
            agent_row(Some(5)),
            activity_row(false, None, Timestamp::now(), path),
            scaled_row,
        ],
    )];

    stamp_context_severity(&mut groups, &crate::config::ContextMeterConfig::default());

    let rows = &groups[0].rows;
    assert_eq!(
        rows[0].as_agent().and_then(|agent| agent.context_severity),
        Some(crate::agents::ContextSeverity::Amber),
        "85% crosses the default amber band"
    );
    assert_eq!(
        rows[1].as_agent().and_then(|agent| agent.context_severity),
        Some(crate::agents::ContextSeverity::Calm)
    );
    assert_eq!(
        rows[2].as_agent().and_then(|agent| agent.context_severity),
        None,
        "a process row carries no context verdict"
    );
    let scaled = rows[3].as_agent().unwrap().context_severity;
    let bands = crate::config::ContextMeterConfig {
        scale: crate::config::ContextMeterScale::Window,
        ..Default::default()
    };
    stamp_context_severity(&mut groups, &bands);
    assert_eq!(
        (
            scaled,
            groups[0].rows[3].as_agent().unwrap().context_severity
        ),
        (
            Some(crate::agents::ContextSeverity::Amber),
            Some(crate::agents::ContextSeverity::Calm)
        ),
        "the same row is amber against compaction and calm against the window"
    );
}

/// The cockpit scope hash for a project root, derived the way cached enrich
/// derives it: project root plus the durable worktree home resolved from the
/// loaded machine config. Tests that pre-write a per-scope workspace cache key
/// it through here so the consumer reads back the same hash.
fn workspace_scope_hash(project: &Path) -> String {
    let config = crate::config::MachineConfig::load_lenient();
    let home = crate::worktree::worktree_parent(project, &config.agents.worktree).ok();
    crate::agents::spending::SpendScope::for_workspace(Some(project), &[], home.as_deref()).hash()
}

fn cost_row_at(
    id: &str,
    usd: Option<f64>,
    registered_at: Option<Timestamp>,
    worktree_path: &Path,
) -> crate::store::snapshot::SidebarRow {
    let mut row = activity_row(
        true,
        Some(AgentStatus::Running),
        Timestamp::now(),
        worktree_path,
    );
    row.id = id.to_owned();
    let agent = row.as_agent_mut().unwrap();
    agent.registered_at = registered_at;
    agent.context = usd.map(|usd| crate::agents::AgentContext {
        cost: Some(crate::agents::AgentCost {
            total_cost_usd: Some(usd),
            ..Default::default()
        }),
        ..crate::agents::AgentContext::new("claude", Timestamp::from_second(1_750_000_000).unwrap())
    });
    row
}

/// Stage one login's stored account record and rate-limit entry beside a live
/// session whose context reports another plan and other windows. Returns the
/// runtime and the snapshot holding that session.
fn stage_record_beside_a_disagreeing_session(
    kind: &str,
) -> (tempfile::TempDir, RuntimePaths, SidebarSnapshot) {
    let (dir, runtime, snapshot) = runtime();
    let now = snapshot.now;
    let key = crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked(kind));
    let window = |used: u8| crate::agents::RateLimitWindow {
        used_percentage: Some(used),
        duration_mins: Some(300),
        resets_at: Some(now + SignedDuration::from_hours(2)),
        observed_at: Some(now),
        ..Default::default()
    };
    let account = |plan: &str| crate::agents::AgentAccount {
        plan: Some(plan.to_owned()),
        metered: Some(true),
        ..Default::default()
    };
    atomic::write_temp_then_rename_cache(
        &runtime.shared_accounts_path(),
        &serde_json::json!({"logins": {key.to_string(): {
            "probed_at_ms": unix_now_ms(),
            "ok": true,
            "account": account("stored-plan"),
        }}}),
    )
    .unwrap();
    atomic::write_temp_then_rename_cache(
        &crate::agents::account::RateLimitsCache::shared_path(&runtime),
        &crate::agents::account::RateLimitsCache {
            entries: BTreeMap::from([(
                crate::agents::account::AccountUsageKey::from(key),
                crate::agents::account::RateLimitCacheEntry {
                    limits: crate::agents::AgentRateLimits {
                        windows: vec![window(33)],
                    },
                    ..Default::default()
                },
            )]),
            ..Default::default()
        },
    )
    .unwrap();
    let mut agent = root_agent(kind, &format!("{kind}-live"), None);
    let mut context = crate::agents::AgentContext::new(kind, now);
    context.account = Some(account("live-plan"));
    context.rate_limits = Some(crate::agents::AgentRateLimits {
        windows: vec![window(77)],
    });
    agent.context = Some(context);
    let snapshot =
        SidebarSnapshot::build_with_agents(runtime.workspace_id.clone(), vec![agent], now);
    (dir, runtime, snapshot)
}

#[test]
fn every_surface_shows_the_stored_record_over_a_live_context_for_every_adapter() {
    let config = crate::config::MachineConfig::default();
    let logins = crate::agents::RoomLoginSet::native();
    let shown = |panel: Option<&SidebarProviderPanel>| {
        panel.map(|panel| {
            (
                panel.plan.as_ref().map(|plan| plan.tier.clone()),
                panel
                    .windows
                    .iter()
                    .map(|window| window.used_percentage)
                    .collect::<Vec<_>>(),
            )
        })
    };
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for definition in crate::agents::all_definitions() {
        let kind = definition.spec().kind;
        let (_dir, runtime, snapshot) = stage_record_beside_a_disagreeing_session(kind);
        let sidebar = fold_machine_config(
            snapshot,
            &runtime,
            &config,
            &config.remote_control,
            &logins.clone().with_agents(&[]),
            Default::default(),
            None,
        )
        .0;
        let accounts = crate::agents::account::read_accounts_cache(&runtime.shared_accounts_path())
            .logins
            .into_iter()
            .filter_map(|(key, record)| Some((key, record.account?)))
            .collect();
        let cached =
            provider_panels_from_caches(&runtime, &logins, &config, accounts, &Default::default());
        let stored = Some((Some("stored-plan".to_owned()), vec![Some(33)]));
        actual.push((
            kind,
            shown(sidebar.providers.iter().find(|panel| panel.kind == kind)),
            shown(cached.iter().find(|panel| panel.kind == kind)),
        ));
        expected.push((kind, stored.clone(), stored));
    }
    assert_eq!(actual, expected);
}
