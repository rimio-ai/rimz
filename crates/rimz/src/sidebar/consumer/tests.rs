use super::*;
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::sidebar::enrich::{FoldOpts, WorkspaceSnapshot, enrich};
use crate::sidebar::frame::{CarriedPane, assemble_frame};
use crate::sidebar::refresh::PrStateCache;
use crate::sidebar::refresh::git_stats::{DiffStatsCache, DiffStatsCacheEntry};
use crate::sidebar::test_support::{child_agent, pane, pane_in_tab, root_agent};
use crate::sidebar::timing::unix_now_ms;
use crate::store::atomic;
use crate::store::snapshot::{SidebarSnapshot, SidebarWorktreeKind};
use crate::{RuntimePaths, StatePaths};
use jiff::Timestamp;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn cached_opts() -> FoldOpts<'static> {
    FoldOpts {
        producing: false,
        fresh_roots: None,
        config: None,
        lanes: None,
        agent_projection: Default::default(),
    }
}

struct StampFixture {
    _state_root: tempfile::TempDir,
    _runtime_root: tempfile::TempDir,
    state: StatePaths,
    runtime: RuntimePaths,
}

impl StampFixture {
    fn new() -> Self {
        let state_root = tempfile::tempdir().unwrap();
        let runtime_root = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(state_root.path());
        let state = StatePaths::under(workspace.clone(), state_root.path()).unwrap();
        let runtime = RuntimePaths::under(workspace, runtime_root.path()).unwrap();
        state.ensure_dirs().unwrap();
        runtime.ensure_dirs().unwrap();
        std::fs::create_dir_all(&state.messages_dir).unwrap();

        for (name, path) in file_stamp_inputs(&state, &runtime) {
            write_stamp_file(&path, name);
        }

        Self {
            _state_root: state_root,
            _runtime_root: runtime_root,
            state,
            runtime,
        }
    }

    fn reader(&self) -> PublishedSnapshotReader {
        PublishedSnapshotReader::new(self.runtime.clone(), "rimz-test", None)
    }
}

fn file_stamp_inputs(state: &StatePaths, runtime: &RuntimePaths) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("events_log", state.events_log.clone()),
        ("latest_snapshot", state.latest_snapshot.clone()),
        ("rollup_cache", state.rollup_cache.clone()),
        ("agents_carryover", state.agents_carryover.clone()),
        ("workspace_record", state.workspace_record.clone()),
        ("message_queue", state.messages_dir.join("queue.json")),
        ("pane_frame", runtime.pane_frame_path()),
        ("diff_stats", runtime.diff_stats_path()),
        ("pr_state", runtime.pr_state_path()),
        ("unread", runtime.unread_path()),
        ("link_stats", crate::remote::link::stats_path(runtime)),
        ("accounts", runtime.shared_accounts_path()),
        ("rate_limits", runtime.shared_rate_limits_path()),
        ("credits", runtime.shared_credits_path()),
        ("provider_spending", runtime.shared_provider_spending_path()),
        ("agent_projection", runtime.agent_projection_path()),
        ("metrics_sample", runtime.root.join("metrics-sample.json")),
        (
            "codex_daemon_reap",
            crate::sidebar::refresh::daemon_reap::codex_daemon_reap_path(runtime),
        ),
    ]
}

fn dir_stamp_inputs(state: &StatePaths, runtime: &RuntimePaths) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("messages_dir", state.messages_dir.clone()),
        ("agent_context_dir", runtime.agent_context_dir.clone()),
        ("subagent_context_dir", runtime.subagent_context_dir.clone()),
        ("agent_activity_dir", runtime.agent_activity_dir.clone()),
        ("active_time_dir", runtime.active_time_dir.clone()),
        ("read_marks_dir", runtime.read_marks_dir.clone()),
    ]
}

fn write_stamp_file(path: &Path, value: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, format!("{value}-baseline")).unwrap();
}

fn daemon_codex(
    id: &str,
    worktree: &Path,
    pane: Option<crate::pane::PaneRef>,
    owner_pid: u32,
) -> crate::agents::AgentState {
    let mut agent = crate::testkit::agent_state("codex", id, Timestamp::now());
    agent.name = Some(id.to_owned());
    agent.status = crate::agents::AgentStatus::Success;
    agent.worktree_path = Some(worktree.to_string_lossy().into_owned());
    agent.pane = pane;
    agent.runtime_owner = Some(crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Daemon,
        id,
        owner_pid,
        None,
    ));
    agent
}

fn local_observation(
    session: &str,
    workspace: &Path,
    now: Timestamp,
) -> crate::agents::LocalSessionObservation {
    crate::agents::LocalSessionObservation {
        kind: crate::ids::AgentKind::new_unchecked("kiro"),
        session_id: crate::ids::AgentSessionId::from(session),
        workspace: workspace.to_path_buf(),
        transcript_path: workspace.join(format!("{session}.json")),
        created_at: now,
        fresh_binding_at: Some(now),
        first_event_at: Some(now),
        last_activity: now,
        projection: crate::agents::LocalSessionProjection::IdentityOnly,
    }
}

#[test]
fn cached_alive_snapshot_binds_safe_local_session_intersection() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let live_worktree = dir.path().join("live");
    let removed_worktree = dir.path().join("removed");
    std::fs::create_dir_all(&live_worktree).unwrap();
    std::fs::create_dir_all(&removed_worktree).unwrap();
    let now = Timestamp::now();
    let mut live_pane = pane(
        "terminal_kiro",
        "kiro-cli",
        &live_worktree.to_string_lossy(),
    );
    live_pane.pane_process_start = Some(now - std::time::Duration::from_secs(1));
    let removed_pane = pane(
        "terminal_removed",
        "kiro-cli",
        &removed_worktree.to_string_lossy(),
    );
    let frame = assemble_frame(vec![live_pane.clone()], unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame).unwrap();
    let published_inputs = crate::sidebar::agent_projection::LocalSessionInputs::from_panes(&[
        live_pane.clone(),
        removed_pane,
    ]);
    let live_observation = local_observation("kiro-live", &live_worktree, now);
    let removed_observation = local_observation("kiro-removed", &removed_worktree, now);
    atomic::write_temp_then_rename_cache(
        &runtime.agent_projection_path(),
        &crate::sidebar::agent_projection::AgentProjectionPublication {
            session_name: "rimz-test".to_owned(),
            wiring: Default::default(),
            inputs: published_inputs,
            observations: vec![live_observation.clone(), removed_observation.clone()],
        },
    )
    .unwrap();
    let mut durable = root_agent("kiro", "durable", None);
    durable.worktree_path = Some(live_worktree.to_string_lossy().into_owned());
    durable.pane = Some(live_pane);
    let base = SidebarSnapshot::build_with_agents(workspace, vec![durable], now);

    let snapshot = cached_alive_snapshot(base, &runtime, "rimz-test");

    assert!(
        snapshot
            .agents
            .iter()
            .any(|agent| agent.agent_id == live_observation.session_id),
    );
    assert!(
        snapshot
            .agents
            .iter()
            .all(|agent| agent.agent_id != removed_observation.session_id),
        "published observations bind only through current card-admitted panes",
    );
}

#[test]
fn cached_daemon_reap_drops_paneless_codex_ghost_before_worktree_pins() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let owner_pid = std::process::id();
    crate::sidebar::refresh::daemon_reap::write_codex_daemon_reap(
        &runtime,
        &crate::sidebar::refresh::CodexDaemonReap {
            produced_at_ms: 1,
            daemon_pids: BTreeSet::from([owner_pid]),
            loaded: Some(BTreeSet::new()),
        },
    )
    .unwrap();
    let worktree = dir.path().join("ghost");
    let ghost = daemon_codex("ghost", &worktree, None, owner_pid);
    let snapshot = SidebarSnapshot::build_with_agents(workspace, vec![ghost], Timestamp::now());
    assert!(
        crate::worktree::protection_set_from_runtime(
            &[],
            &snapshot.agents,
            None,
            crate::worktree::Occupancy::Unproven,
        )
        .protects(&worktree),
    );

    let snapshot = reap_cached_daemon_sessions(snapshot, &runtime, "rimz-test");

    assert!(snapshot.agents.is_empty());
    assert!(
        !crate::worktree::protection_set_from_runtime(
            &[],
            &snapshot.agents,
            None,
            crate::worktree::Occupancy::Unproven,
        )
        .protects(&worktree),
    );
}

#[test]
fn cached_daemon_reap_forwards_published_live_panes() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let pane_id = PaneId::from_parts(MuxName::Tmux, "%1");
    let pane = crate::pane::PaneRef::from_id(pane_id.clone());
    let codex = daemon_codex("live-pane", dir.path(), Some(pane.clone()), 77);
    crate::sidebar::refresh::daemon_reap::write_codex_daemon_reap(
        &runtime,
        &crate::sidebar::refresh::CodexDaemonReap {
            produced_at_ms: 1,
            daemon_pids: BTreeSet::from([77]),
            loaded: Some(BTreeSet::new()),
        },
    )
    .unwrap();
    let frame = assemble_frame(vec![pane], 1, "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame).unwrap();
    let snapshot = SidebarSnapshot::build_with_agents(workspace, vec![codex], Timestamp::now());

    let snapshot = reap_cached_daemon_sessions(snapshot, &runtime, "rimz-test");

    assert_eq!(snapshot.agents[0].agent_id.as_str(), "live-pane");
}

#[test]
fn consumer_fold_inputs_stamp_is_stable_for_unchanged_inputs() {
    let fixture = StampFixture::new();
    let reader = fixture.reader();

    assert_eq!(
        reader.inputs_stamp(&fixture.state),
        reader.inputs_stamp(&fixture.state)
    );
}

#[test]
fn consumer_fold_inputs_stamp_ignores_account_global_spending_cursor() {
    let fixture = StampFixture::new();
    let before = consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime);

    std::fs::write(
        fixture.runtime.shared_spending_cursor_path(),
        b"replaced account-global cursor with a longer body",
    )
    .unwrap();

    assert_eq!(
        consumer_fold_inputs_stamp(&fixture.state, &fixture.runtime),
        before,
    );
}

#[test]
fn consumer_fold_inputs_stamp_changes_for_each_file_input() {
    for name in [
        "events_log",
        "latest_snapshot",
        "rollup_cache",
        "agents_carryover",
        "workspace_record",
        "message_queue",
        "pane_frame",
        "diff_stats",
        "pr_state",
        "unread",
        "link_stats",
        "accounts",
        "rate_limits",
        "credits",
        "provider_spending",
        "agent_projection",
        "metrics_sample",
        "codex_daemon_reap",
    ] {
        let fixture = StampFixture::new();
        let reader = fixture.reader();
        let before = reader.inputs_stamp(&fixture.state);
        let path = file_stamp_inputs(&fixture.state, &fixture.runtime)
            .into_iter()
            .find(|(candidate, _)| *candidate == name)
            .expect("case path")
            .1;

        std::fs::write(&path, format!("{name}-changed-with-a-longer-body")).unwrap();

        assert_ne!(
            reader.inputs_stamp(&fixture.state),
            before,
            "{name} must participate in the consumer fold input stamp",
        );
    }
}

#[test]
fn consumer_fold_inputs_stamp_changes_for_each_dir_input() {
    for name in [
        "messages_dir",
        "agent_context_dir",
        "subagent_context_dir",
        "agent_activity_dir",
        "active_time_dir",
        "read_marks_dir",
    ] {
        let fixture = StampFixture::new();
        let reader = fixture.reader();
        let before = reader.inputs_stamp(&fixture.state);
        let path = dir_stamp_inputs(&fixture.state, &fixture.runtime)
            .into_iter()
            .find(|(candidate, _)| *candidate == name)
            .expect("case path")
            .1;

        std::fs::remove_dir_all(&path).unwrap();

        assert_ne!(
            reader.inputs_stamp(&fixture.state),
            before,
            "{name} must participate in the consumer fold input stamp",
        );
    }
}

#[test]
fn consumer_fold_inputs_stamp_ignores_unrelated_runtime_churn() {
    let fixture = StampFixture::new();
    let reader = fixture.reader();
    let baseline = reader.inputs_stamp(&fixture.state);
    let unrelated = [
        fixture.runtime.root.join("snapshot.lock"),
        fixture.runtime.root.join("presence.stamp"),
        fixture.runtime.root.join("client-presence-probe.stamp"),
        fixture.runtime.root.join("producer-cache.json"),
    ];
    for path in unrelated {
        std::fs::write(&path, b"churn").unwrap();
        assert_eq!(
            reader.inputs_stamp(&fixture.state),
            baseline,
            "{} is not a consumer fold input",
            path.display(),
        );
        std::fs::remove_file(path).unwrap();
    }

    let temp = fixture.runtime.root.join(".snapshot.tmp-123");
    let renamed = fixture.runtime.root.join("producer-only.cache");
    std::fs::write(&temp, b"temp").unwrap();
    std::fs::rename(&temp, &renamed).unwrap();
    std::fs::remove_file(renamed).unwrap();
    let heartbeat = fixture.runtime.heartbeat_dir.join("sidebar.unrelated.json");
    std::fs::write(heartbeat, b"{}").unwrap();
    assert_eq!(reader.inputs_stamp(&fixture.state), baseline,);
}

#[test]
fn consumer_fold_inputs_stamp_tracks_filtered_dynamic_files() {
    let fixture = StampFixture::new();
    let reader = fixture.reader();
    for path in [
        fixture
            .runtime
            .root
            .join("workspace-spending.0123456789abcdef.json"),
        fixture.runtime.root.join("budget.0123456789abcdef.json"),
        fixture.runtime.root.join("budget.fleet.json"),
        fixture.runtime.root.join("budget.scopes.json"),
        fixture
            .runtime
            .root
            .join("auto-continue.0123456789abcdef.json"),
        fixture
            .runtime
            .persistent_shared_root
            .join("budget.account.codex.json"),
    ] {
        let before_create = reader.inputs_stamp(&fixture.state);
        write_stamp_file(&path, "created");
        let after_create = reader.inputs_stamp(&fixture.state);
        assert_ne!(after_create, before_create, "create {}", path.display());

        std::fs::write(&path, b"replaced-with-a-longer-payload").unwrap();
        let after_replace = reader.inputs_stamp(&fixture.state);
        assert_ne!(after_replace, after_create, "replace {}", path.display());

        std::fs::remove_file(&path).unwrap();
        assert_ne!(
            reader.inputs_stamp(&fixture.state),
            after_replace,
            "remove {}",
            path.display(),
        );
    }
}

#[test]
fn read_published_snapshot_folds_caches_without_forking() {
    // A real on-disk worktree so the live-dir projection fires.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let worktree = dir.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let wt = worktree.to_string_lossy().into_owned();

    // Publish the rollup (project root = the worktree) to `latest.json`, where
    // the consumer reads it fresh, and the live panes to `snapshot.json`. `own`
    // is excluded; a sibling pane becomes a row.
    let mut rollup = SidebarSnapshot::build(workspace.clone(), Vec::new(), Timestamp::now());
    rollup = rollup.with_project_root(Some(worktree.clone()));
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();
    let panes = vec![
        pane("terminal_0", "zsh", &wt),
        pane("terminal_own", "rimz-sidebar", &wt),
    ];
    let base = assemble_frame(panes, unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &base).unwrap();

    // Publish diff stats for the worktree path: +7 / -2, 3 commits ahead and
    // 1 behind a remote-default trunk, on branch `feat`.
    let mut diff = DiffStatsCache::default();
    diff.entries.insert(
        wt.clone(),
        DiffStatsCacheEntry {
            refreshed_at_ms: unix_now_ms(),
            commit_refreshed_at_ms: Some(unix_now_ms()),
            added: Some(7),
            removed: Some(2),
            commits: Some(3),
            behind: Some(1),
            trunk: Some("origin/main".to_owned()),
            branch: Some("feat".to_owned()),
            clean: Some(false),
            landed: Some(false),
            did_work: Some(true),
            merge_in_progress: Some(false),
            ..DiffStatsCacheEntry::default()
        },
    );
    atomic::write_temp_then_rename_cache(&runtime.diff_stats_path(), &diff).unwrap();
    let mut pr = PrStateCache::default();
    pr.states.insert(
        wt.clone(),
        crate::sidebar::refresh::pr::PrLink {
            branch: Some("feature".to_owned()),
            incarnation: None,
            state: crate::store::snapshot::WorktreePrState::Open,
            number: Some(91),
            url: None,
            ci: None,
            merge_sha: None,
        },
    );
    atomic::write_temp_then_rename_cache(&runtime.pr_state_path(), &pr).unwrap();

    let own = PaneId::from_parts(MuxName::Zellij, "terminal_own");
    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        Some(&own),
    )
    .expect("published base");

    // The worktree group carries the cached +7/-2 and the live branch label,
    // projected from the cache with no git fork.
    let group = snapshot
        .worktree_groups
        .iter()
        .find(|group| group.kind == SidebarWorktreeKind::Worktree)
        .expect("a worktree group");
    assert_eq!(group.diff_added, Some(7));
    assert_eq!(group.diff_removed, Some(2));
    assert_eq!(group.commits_ahead, Some(3));
    assert_eq!(group.commits_behind, Some(1));
    assert_eq!(
        group.trunk.as_deref(),
        Some("main"),
        "the ≡/✓ markers name the branch, so origin/ strips for display",
    );
    assert_eq!(group.label, "feat");
    assert_eq!(group.clean, Some(false), "the status verdict projects too");
    assert_eq!(group.landed, Some(false), "the landed verdict projects too");
    assert_eq!(
        group.trunk_sync,
        Some(crate::store::snapshot::WorktreeTrunkSync::Diverged),
        "the trunk-sync classifier projects from cached git facts"
    );
    assert_eq!(
        group.pr_state,
        Some(crate::store::snapshot::WorktreePrState::Open),
        "the PR state projects from cache with no forge CLI fork"
    );
    assert_eq!(group.pr_number, Some(91));
    // The own (sidebar) pane is excluded; the sibling renders as a row.
    assert!(
        snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .all(|row| {
                row.pane
                    .as_ref()
                    .is_none_or(|pane| pane.pane_id.as_str() != own.as_str())
            }),
        "the renderer's own pane is never a row"
    );
}

#[test]
fn read_published_snapshot_binds_safe_local_session_intersection() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let worktree = dir.path().join("wt");
    let removed_worktree = dir.path().join("removed-wt");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::create_dir_all(&removed_worktree).unwrap();
    let wt = worktree.to_string_lossy().into_owned();
    let removed_wt = removed_worktree.to_string_lossy().into_owned();
    let now = Timestamp::now();
    let mut live_pane = pane("terminal_kiro", "kiro-cli", &wt);
    live_pane.pane_process_start = Some(now - std::time::Duration::from_secs(1));
    let panes = vec![live_pane];
    let frame = assemble_frame(panes.clone(), unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame).unwrap();

    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    let rollup = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now())
        .with_project_root(Some(worktree.clone()));
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();

    let published_panes = vec![
        panes[0].clone(),
        pane("terminal_removed", "kiro-cli", &removed_wt),
    ];
    let inputs = crate::sidebar::agent_projection::LocalSessionInputs::from_panes(&published_panes);
    let session_id = crate::ids::AgentSessionId::from("kiro-session");
    let observation = crate::agents::LocalSessionObservation {
        kind: crate::ids::AgentKind::new_unchecked("kiro"),
        session_id: session_id.clone(),
        workspace: worktree.clone(),
        transcript_path: worktree.join("kiro-session.json"),
        created_at: now,
        fresh_binding_at: Some(now),
        first_event_at: Some(now),
        last_activity: now,
        projection: crate::agents::LocalSessionProjection::IdentityOnly,
    };
    let removed_session_id = crate::ids::AgentSessionId::from("removed-kiro-session");
    let removed_observation = crate::agents::LocalSessionObservation {
        session_id: removed_session_id.clone(),
        workspace: removed_worktree.clone(),
        transcript_path: removed_worktree.join("kiro-session.json"),
        ..observation.clone()
    };
    atomic::write_temp_then_rename_cache(
        &runtime.agent_projection_path(),
        &crate::sidebar::agent_projection::AgentProjectionPublication {
            session_name: "rimz-test".to_owned(),
            wiring: Default::default(),
            inputs,
            observations: vec![observation, removed_observation],
        },
    )
    .unwrap();

    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("published snapshot");
    assert!(
        snapshot
            .agents
            .iter()
            .any(|agent| agent.kind == "kiro" && agent.agent_id == session_id),
    );
    assert!(
        snapshot
            .agents
            .iter()
            .all(|agent| agent.agent_id != removed_session_id),
        "an observation removed from the current pane inputs stays hidden",
    );
}

#[test]
fn published_wiring_admits_a_hook_only_idle_pane_without_provider_config() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    let worktree = dir.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let frame = assemble_frame(
        vec![pane("terminal_droid", "droid", &worktree.to_string_lossy())],
        unix_now_ms(),
        "rimz-test",
    );
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame).unwrap();
    let rollup = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now())
        .with_project_root(Some(worktree));
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();
    atomic::write_temp_then_rename_cache(
        &runtime.agent_projection_path(),
        &crate::sidebar::agent_projection::AgentProjectionPublication {
            session_name: "rimz-test".to_owned(),
            wiring: crate::sidebar::agent_projection::WiredAgentProjection {
                kinds: vec!["droid".to_owned()],
                default_models: std::collections::BTreeMap::from([(
                    "droid".to_owned(),
                    "fixture-model".to_owned(),
                )]),
            },
            inputs: Default::default(),
            observations: Vec::new(),
        },
    )
    .unwrap();

    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .unwrap();
    assert!(
        snapshot
            .agent_panes
            .iter()
            .any(|pane| pane.kind == "droid" && pane.agent_id.is_none())
    );
    assert_eq!(
        snapshot
            .wired_default_models
            .get("droid")
            .map(String::as_str),
        Some("fixture-model")
    );
}

#[test]
fn read_published_snapshot_folds_subagent_context() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();

    let worktree = dir.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let wt = worktree.to_string_lossy().into_owned();
    let live_pane = pane("terminal_parent", "claude", &wt);
    let mut parent = root_agent("claude", "parent-1", None);
    parent.worktree_path = Some(wt.clone());
    parent.pane = Some(live_pane.clone());
    let mut child = child_agent("claude", "parent-1", "child-1");
    child.worktree_path = Some(wt.clone());
    child.pane = Some(live_pane.clone());
    child.task = None;
    let mut rollup = SidebarSnapshot::build_with_agents(
        workspace.clone(),
        vec![parent, child],
        Timestamp::now(),
    );
    rollup = rollup.with_project_root(Some(worktree));
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();

    let base = assemble_frame(vec![live_pane], unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &base).unwrap();
    let now = Timestamp::now();
    crate::store::subagent_context::write(
        &runtime,
        "claude",
        "child-1",
        &crate::agents::context::SubagentContext {
            agent_type: Some("Explore".to_owned()),
            model: None,
            description: Some("trace the sidebar rows".to_owned()),
            token_count: Some(12_400),
            cost_usd: Some(0.42),
            started_at: Some(now),
            observed_at: now,
        },
    )
    .unwrap();

    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("published base");
    let parent = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .find(|row| row.id == "parent-1")
        .expect("parent row");

    assert_eq!(parent.sub_agents().len(), 1);
    assert_eq!(parent.sub_agents()[0].id, "child-1");
    assert_eq!(parent.sub_agents()[0].name, "Explore");
    assert_eq!(
        parent.sub_agents()[0].description.as_deref(),
        Some("trace the sidebar rows"),
    );
    assert_eq!(parent.sub_agents()[0].total_tokens, Some(12_400));
    assert_eq!(parent.sub_agents()[0].cost_usd, Some(0.42));
}

#[test]
fn consumer_own_view_counts_siblings_in_its_own_tab() {
    // A consumer reads the producer's session-wide pane list (`list-panes
    // -a`) and folds its own-view from it. An orphan sidebar — alone in its
    // tab — must see `Some(0)` siblings so self-close can fire, even though
    // the producer lives in another tab with its own siblings.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let main_sb = pane_in_tab("main_sb", "@0");
    let main_term = pane_in_tab("main_term", "@0");
    let orphan_sb = pane_in_tab("orphan_sb", "@1");
    let base = assemble_frame(
        vec![main_sb, main_term, orphan_sb],
        unix_now_ms(),
        "rimz-test",
    );
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &base).unwrap();
    // The rollup the consumer folds the panes over: an empty room, published
    // to `latest.json` where the consumer reads it fresh.
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    let rollup = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();

    let orphan_own = PaneId::from_parts(MuxName::Zellij, "orphan_sb");
    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        Some(&orphan_own),
    )
    .expect("base");
    assert_eq!(
        snapshot.own_view.map(|view| view.sibling_count),
        Some(0),
        "an orphan sidebar sees zero siblings in its own tab so self-close can fire"
    );
}

#[test]
fn read_published_snapshot_is_frameless_until_the_producer_publishes() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    // No published pane set yet (the producer hasn't run), so the consumer
    // read folds the store rollup without pane-admitted cards rather than
    // reporting a failed snapshot.
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    let mut rollup = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    rollup.display_name = "cold-room".to_owned();
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();

    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("frameless rollup");

    assert_eq!(snapshot.display_name, "cold-room");
    assert_eq!(snapshot.panes_produced_at_ms, None);
    assert!(snapshot.worktree_groups.is_empty());
}

#[test]
fn read_published_snapshot_reports_why_the_store_was_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::under(workspace, dir.path()).unwrap();
    state.ensure_dirs().unwrap();
    // A directory where the event log should be: the row scan's read fails,
    // and with no `latest.json` the rollup read has no fallback.
    std::fs::create_dir_all(&state.events_log).unwrap();

    let err = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect_err("an unreadable store rollup is the one failed consumer read");
    assert!(
        err.to_string()
            .contains(&state.events_log.display().to_string()),
        "the error names the unreadable path, got: {err}"
    );
}

#[test]
fn no_frame_enrich_preserves_rollup_metadata_but_emits_no_groups() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let agent = root_agent("claude", "sess-1", None);

    let snapshot = enrich(
        SidebarSnapshot::build_with_agents(workspace, vec![agent], Timestamp::now()),
        None,
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(snapshot.panes_produced_at_ms, None);
    assert_eq!(snapshot.agents.len(), 1);
    assert!(snapshot.worktree_groups.is_empty());
}

#[test]
fn enrich_maps_carried_frame_to_truth_notice() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let carried_id = PaneId::from_parts(MuxName::Zellij, "terminal_1");
    let mut frame = assemble_frame(
        vec![pane("terminal_1", "zsh", "/repo/main")],
        1_234,
        "rimz-test",
    );
    frame.carried_panes = vec![CarriedPane {
        pane_id: carried_id.clone(),
        pid: Some(42),
        start_ticks: Some(9),
        carried_since_ms: 1_000,
    }];

    let snapshot = enrich(
        SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now()),
        Some(&frame),
        &runtime,
        None,
        None,
        cached_opts(),
        &crate::diag::DiagSink::disabled(),
    );

    assert_eq!(
        snapshot.truth_degraded,
        Some(crate::store::snapshot::TruthNotice {
            carried: 1,
            since_ms: 1_000,
            pane_ids: vec![carried_id],
        })
    );
}

#[test]
fn consumer_reflects_a_fresh_rollup_over_a_stale_pane_cache() {
    // The event-fresh split: the consumer reads the rollup from `latest.json`
    // each call, so a status change shows even when the producer's published
    // pane cache has not moved. Republishing `latest.json` alone changes the
    // rendered rollup.
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();

    // A published (and never re-published) pane cache.
    let panes = assemble_frame(Vec::new(), unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &panes).unwrap();

    // A served publish carries the extent stamp; the workspace has no
    // events, so the matching extent is the empty log's.
    let stamp = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    let mut alpha = SidebarSnapshot::build(workspace.clone(), Vec::new(), Timestamp::now());
    alpha.display_name = "alpha".to_owned();
    alpha.reflects_log = stamp;
    atomic::write_temp_then_rename(&state.latest_snapshot, &alpha).unwrap();
    let first = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("base");
    assert_eq!(first.display_name, "alpha");

    // Republish ONLY `latest.json` (a different length so the parse cache
    // cannot mask the change); the pane cache is untouched.
    let mut bravo = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
    bravo.display_name = "bravo-the-second-rollup".to_owned();
    bravo.reflects_log = stamp;
    atomic::write_temp_then_rename(&state.latest_snapshot, &bravo).unwrap();
    let second = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("base");
    assert_eq!(
        second.display_name, "bravo-the-second-rollup",
        "the consumer folds the fresh rollup, not a cached one"
    );
}

#[test]
fn published_reader_sees_republished_rollup_and_incremental_event_with_one_pane_frame() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    state.ensure_dirs().unwrap();

    let pane_frame = assemble_frame(Vec::new(), 12_345, "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &pane_frame).unwrap();
    let empty_extent = crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    };
    let mut first_publish = SidebarSnapshot::build(workspace.clone(), Vec::new(), Timestamp::now());
    first_publish.display_name = "first".to_owned();
    first_publish.reflects_log = Some(empty_extent);
    atomic::write_temp_then_rename(&state.latest_snapshot, &first_publish).unwrap();

    let mut reader = PublishedSnapshotReader::new(runtime, "rimz-test", None);
    let first = reader.read(&state).expect("first publish");
    assert_eq!(first.display_name, "first");
    assert_eq!(first.panes_produced_at_ms, Some(12_345));

    let mut second_publish =
        SidebarSnapshot::build(workspace.clone(), Vec::new(), Timestamp::now());
    second_publish.display_name = "second-publish".to_owned();
    second_publish.reflects_log = Some(empty_extent);
    atomic::write_temp_then_rename(&state.latest_snapshot, &second_publish).unwrap();
    let second = reader.read(&state).expect("republished latest snapshot");
    assert_eq!(second.display_name, "second-publish");
    assert_eq!(second.panes_produced_at_ms, Some(12_345));

    crate::store::event_log::append(
        &state.events_log,
        &crate::store::event::EventEnvelope::session_rebirth(workspace, "rimz-test"),
    )
    .unwrap();
    let third = reader.read(&state).expect("incremental event fold");
    assert_eq!(third.panes_produced_at_ms, Some(12_345));
    assert_eq!(
        third.reflects_log.map(|extent| extent.offset),
        std::fs::metadata(&state.events_log)
            .ok()
            .map(|meta| meta.len()),
        "reader folds only the append past the warm published base",
    );
}

struct AdoptionFixture {
    _dir: tempfile::TempDir,
    runtime: RuntimePaths,
    state: StatePaths,
    frame: crate::sidebar::frame::PaneFrame,
}

impl AdoptionFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_project_root(dir.path());
        let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
        let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
        runtime.ensure_dirs().unwrap();
        state.ensure_dirs().unwrap();
        let extent = crate::store::event_log::LogExtent {
            generation: 0,
            offset: 0,
        };
        let mut durable = SidebarSnapshot::build(workspace, Vec::new(), Timestamp::now());
        durable.display_name = "durable".to_owned();
        durable.reflects_log = Some(extent);
        atomic::write_temp_then_rename_cache(&state.latest_snapshot, &durable).unwrap();

        let mut frame = assemble_frame(Vec::new(), 10, "rimz-test");
        frame.topology_stamp_ms = Some(11);
        frame.metrics_stamp_ms = Some(12);
        atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame).unwrap();

        let mut projected = durable;
        projected.display_name = "projected".to_owned();
        let workspace = WorkspaceSnapshot(projected);
        let mut publisher =
            crate::sidebar::workspace_projection::WorkspaceProjectionPublisher::default();
        publisher
            .publish(&runtime, "rimz-test", &workspace, &frame)
            .unwrap();
        Self {
            _dir: dir,
            runtime,
            state,
            frame,
        }
    }

    fn read(&self) -> ConsumerSnapshotRead {
        PublishedSnapshotReader::new(self.runtime.clone(), "rimz-test", None)
            .read_adopting(&self.state)
            .unwrap()
    }
}

#[test]
fn consumer_adopts_only_an_exact_workspace_projection() {
    let fixture = AdoptionFixture::new();
    let read = fixture.read();
    assert_eq!(read.source, ConsumerSnapshotSource::Adoption);
    assert_eq!(read.snapshot.display_name, "projected");

    for (field, value) in [
        ("schema_version", serde_json::json!(99)),
        ("session", serde_json::json!("other-session")),
    ] {
        let fixture = AdoptionFixture::new();
        let path =
            crate::sidebar::workspace_projection::workspace_projection_path(&fixture.runtime);
        let mut projection: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        projection[field] = value;
        atomic::write_temp_then_rename_cache(&path, &projection).unwrap();
        assert_eq!(
            fixture.read().source,
            ConsumerSnapshotSource::Fallback,
            "{field}"
        );
    }

    let fixture = AdoptionFixture::new();
    std::fs::write(
        crate::sidebar::workspace_projection::workspace_projection_path(&fixture.runtime),
        b"{broken projection",
    )
    .unwrap();
    assert_eq!(fixture.read().source, ConsumerSnapshotSource::Fallback);

    let fixture = AdoptionFixture::new();
    std::fs::remove_file(
        crate::sidebar::workspace_projection::workspace_projection_path(&fixture.runtime),
    )
    .unwrap();
    assert_eq!(fixture.read().source, ConsumerSnapshotSource::Fallback);
}

#[test]
fn consumer_falls_back_for_stale_truth_or_legacy_frame() {
    let fixture = AdoptionFixture::new();
    crate::store::event_log::append(
        &fixture.state.events_log,
        &crate::store::event::EventEnvelope::session_rebirth(
            fixture.runtime.workspace_id.clone(),
            "rimz-test",
        ),
    )
    .unwrap();
    assert_eq!(fixture.read().source, ConsumerSnapshotSource::Fallback);

    let mut fixture = AdoptionFixture::new();
    fixture.frame.metrics_stamp_ms = Some(99);
    atomic::write_temp_then_rename_cache(&fixture.runtime.pane_frame_path(), &fixture.frame)
        .unwrap();
    assert_eq!(fixture.read().source, ConsumerSnapshotSource::Fallback);

    let mut fixture = AdoptionFixture::new();
    fixture.frame.topology_stamp_ms = None;
    fixture.frame.metrics_stamp_ms = None;
    atomic::write_temp_then_rename_cache(&fixture.runtime.pane_frame_path(), &fixture.frame)
        .unwrap();
    assert_eq!(fixture.read().source, ConsumerSnapshotSource::Fallback);
}

#[test]
fn consumer_adopts_an_event_fresh_projection_while_latest_publish_trails() {
    let fixture = AdoptionFixture::new();
    crate::store::event_log::append(
        &fixture.state.events_log,
        &crate::store::event::EventEnvelope::session_rebirth(
            fixture.runtime.workspace_id.clone(),
            "rimz-test",
        ),
    )
    .unwrap();
    let mut projected: SidebarSnapshot =
        serde_json::from_slice(&std::fs::read(&fixture.state.latest_snapshot).unwrap()).unwrap();
    projected.display_name = "event-fresh projection".to_owned();
    projected.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: std::fs::metadata(&fixture.state.events_log).unwrap().len(),
    });
    crate::sidebar::workspace_projection::WorkspaceProjectionPublisher::default()
        .publish(
            &fixture.runtime,
            "rimz-test",
            &WorkspaceSnapshot(projected),
            &fixture.frame,
        )
        .unwrap();

    let read = fixture.read();
    assert_eq!(read.source, ConsumerSnapshotSource::Adoption);
    assert_eq!(read.snapshot.display_name, "event-fresh projection");
}

#[test]
fn presence_only_frame_publication_keeps_projection_match() {
    let mut fixture = AdoptionFixture::new();
    let source = (
        fixture.frame.topology_stamp_ms,
        fixture.frame.metrics_stamp_ms,
    );
    fixture.frame.presence = Some(crate::store::snapshot::PresenceSample {
        human_clients: 0,
        last_input_ms: None,
        sampled_at_ms: unix_now_ms(),
    });
    atomic::write_temp_then_rename_cache(&fixture.runtime.pane_frame_path(), &fixture.frame)
        .unwrap();

    let read = fixture.read();
    assert_eq!(read.source, ConsumerSnapshotSource::Adoption);
    assert_eq!(
        (
            fixture.frame.topology_stamp_ms,
            fixture.frame.metrics_stamp_ms
        ),
        source
    );
    assert_eq!(
        read.snapshot.presence,
        Some(crate::store::snapshot::SidebarPresence::Detached)
    );
}

#[test]
fn slim_projection_stamp_detects_store_delta_and_projection_republish() {
    let fixture = AdoptionFixture::new();
    let baseline = consumer_projection_inputs_stamp(&fixture.state, &fixture.runtime);
    crate::store::event_log::append(
        &fixture.state.events_log,
        &crate::store::event::EventEnvelope::session_rebirth(
            fixture.runtime.workspace_id.clone(),
            "rimz-test",
        ),
    )
    .unwrap();
    assert_ne!(
        consumer_projection_inputs_stamp(&fixture.state, &fixture.runtime),
        baseline,
        "a durable append invalidates the slim unchanged check"
    );

    let fixture = AdoptionFixture::new();
    let baseline = consumer_projection_inputs_stamp(&fixture.state, &fixture.runtime);
    let path = crate::sidebar::workspace_projection::workspace_projection_path(&fixture.runtime);
    let mut projection: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    projection["projection"]["display_name"] = serde_json::json!("time-transition");
    atomic::write_temp_then_rename_cache(&path, &projection).unwrap();
    assert_ne!(
        consumer_projection_inputs_stamp(&fixture.state, &fixture.runtime),
        baseline,
        "content republish invalidates the slim unchanged check"
    );
}
