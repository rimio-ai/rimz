use super::*;
use crate::agents::{AgentContext, AgentStatus, AgentTurnError, TurnErrorClass};
use crate::disk::atomic;
use crate::forge::pr_state::PrStateCache;
use crate::ids::{MuxName, PaneId, WorkspaceId};
use crate::sidebar::enrich::{FoldOpts, WorkspaceSnapshot, enrich};
use crate::sidebar::frame::{CarriedPane, assemble_frame};
use crate::sidebar::refresh::git_stats::{DiffStatsCache, DiffStatsCacheEntry};
use crate::sidebar::test_support::{child_agent, pane, pane_in_tab, root_agent};
use crate::sidebar::workspace_projection::workspace_projection_path;
use crate::store::snapshot::{SidebarSnapshot, SidebarWorktreeKind};
use crate::utils::time::unix_now_ms;
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
        login: None,
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
    let mut live_observation = local_observation("kiro-live", &live_worktree, now);
    live_observation.login = Some("work".parse().unwrap());
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

    let live = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == live_observation.session_id)
        .unwrap();
    assert_eq!(live.login, live_observation.login);
    assert!(
        snapshot
            .agents
            .iter()
            .all(|agent| agent.agent_id != removed_observation.session_id),
        "published observations bind only through current card-admitted panes",
    );
}

#[test]
fn cached_alive_snapshot_attaches_rest_certificates_for_team_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let older_at = Timestamp::from_second(1_750_000_000).unwrap();
    let error_at = Timestamp::from_second(1_750_000_001).unwrap();
    let newer_at = Timestamp::from_second(1_750_000_002).unwrap();
    let shared_pane = pane("terminal_1", "codex", "/repo/main");
    let frame = assemble_frame(vec![shared_pane.clone()], unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame).unwrap();
    let owner = crate::pane::RuntimeOwner::new(
        crate::pane::RuntimeOwnerKind::Agent,
        "conversation-a",
        42,
        Some("agent-start".to_owned()),
    );
    let mut dead_owner = root_agent("codex", "conversation-a", None);
    dead_owner.last_activity = older_at;
    dead_owner.pane = Some(shared_pane.clone());
    dead_owner.runtime_owner = Some(owner.clone());
    dead_owner.launch_id = Some("launch-coder".into());
    dead_owner.role = Some("coder".to_owned());
    dead_owner.team = Some("forge".to_owned());
    dead_owner.origin = Some(crate::agents::SessionOrigin::Fresh);
    let mut successor = root_agent("codex", "conversation-b", None);
    successor.last_activity = newer_at;
    successor.pane = Some(shared_pane.clone());
    successor.runtime_owner = Some(crate::pane::RuntimeOwner {
        subject_id: "conversation-b".into(),
        ..owner
    });
    successor.launch_id = Some("launch-coder".into());
    successor.role = Some("coder".to_owned());
    successor.team = Some("forge".to_owned());
    successor.origin = Some(crate::agents::SessionOrigin::Fresh);
    let context = AgentContext {
        turn_error: Some(AgentTurnError {
            class: TurnErrorClass::PausedOverloaded,
            at: error_at,
            label: Some("server_overloaded".to_owned()),
        }),
        ..AgentContext::new("codex", error_at)
    };
    crate::store::agent_context::write(&runtime, "codex", "conversation-a", &context).unwrap();
    let observation = crate::agents::LocalSessionObservation {
        login: None,
        kind: crate::ids::AgentKind::new_unchecked("codex"),
        session_id: crate::ids::AgentSessionId::from("conversation-b"),
        workspace: PathBuf::from("/repo/main"),
        transcript_path: PathBuf::from("/repo/main/conversation-b.jsonl"),
        created_at: newer_at,
        fresh_binding_at: Some(newer_at),
        first_event_at: Some(newer_at),
        last_activity: newer_at,
        projection: crate::agents::LocalSessionProjection::IdentityOnly,
    };
    atomic::write_temp_then_rename_cache(
        &runtime.agent_projection_path(),
        &crate::sidebar::agent_projection::AgentProjectionPublication {
            session_name: "rimz-test".to_owned(),
            wiring: Default::default(),
            inputs: crate::sidebar::agent_projection::LocalSessionInputs::from_panes(&[
                shared_pane,
            ]),
            observations: vec![observation],
        },
    )
    .unwrap();
    let base = SidebarSnapshot::build_with_agents(workspace, vec![dead_owner, successor], newer_at);

    let snapshot = cached_alive_snapshot(base, &runtime, "rimz-test");

    let dead_owner = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == "conversation-a")
        .unwrap();
    assert_eq!(dead_owner.status, AgentStatus::Running);
    assert!(!dead_owner.holds_open_turn());
    let successor = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == "conversation-b")
        .unwrap();
    assert_eq!(
        successor.transcript_path.as_deref(),
        Some("/repo/main/conversation-b.jsonl")
    );
    let cohorts = crate::address::team_cohorts(&snapshot.agents);
    assert_eq!(cohorts[0].members.len(), 1);
    assert_eq!(cohorts[0].members[0].agent_id.as_str(), "conversation-b");
}

#[test]
fn cached_daemon_reap_drops_paneless_codex_ghost_before_worktree_pins() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let owner_pid = std::process::id();
    atomic::write_temp_then_rename_cache(
        &crate::sidebar::refresh::daemon_reap::codex_daemon_reap_path(&runtime),
        &crate::sidebar::refresh::daemon_reap::CodexDaemonReap {
            produced_at_ms: crate::utils::time::unix_now_ms(),
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
fn cached_daemon_reap_keeps_ghost_when_publication_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let owner_pid = std::process::id();
    atomic::write_temp_then_rename_cache(
        &crate::sidebar::refresh::daemon_reap::codex_daemon_reap_path(&runtime),
        &crate::sidebar::refresh::daemon_reap::CodexDaemonReap {
            produced_at_ms: crate::utils::time::unix_now_ms()
                - crate::sidebar::timing::CODEX_DAEMON_REAP_STALE.as_millis() as u64
                - 1,
            daemon_pids: BTreeSet::from([owner_pid]),
            loaded: Some(BTreeSet::new()),
        },
    )
    .unwrap();
    let ghost = daemon_codex("ghost", dir.path(), None, owner_pid);
    let snapshot = SidebarSnapshot::build_with_agents(workspace, vec![ghost], Timestamp::now());

    let snapshot = reap_cached_daemon_sessions(snapshot, &runtime, "rimz-test");

    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].agent_id.as_str(), "ghost");
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
    atomic::write_temp_then_rename_cache(
        &crate::sidebar::refresh::daemon_reap::codex_daemon_reap_path(&runtime),
        &crate::sidebar::refresh::daemon_reap::CodexDaemonReap {
            produced_at_ms: crate::utils::time::unix_now_ms(),
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
        crate::forge::pr_state::PrLink {
            open: None,
            stack: Default::default(),
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
        login: None,
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
        login: None,
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
    let context = crate::agents::context::SubagentContext {
        usage: Some(crate::agents::AgentUsageSummary {
            fresh_input_tokens: Some(12_400),
            ..Default::default()
        }),
        agent_type: Some("Explore".to_owned()),
        model: None,
        effort: None,
        description: Some("trace the sidebar rows".to_owned()),
        cost_usd: Some(0.42),
        started_at: Some(now),
        observed_at: now,
    };
    crate::store::subagent_context::update(&runtime, "claude", "child-1", |prior| {
        (
            context.clone(),
            prior.and_then(|record| record.usage_cursor.clone()),
        )
    })
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
    assert_eq!(
        snapshot
            .agents
            .iter()
            .find(|agent| agent.agent_id == "child-1")
            .unwrap()
            .context_used_tokens(),
        Some(12_400)
    );
    assert_eq!(parent.sub_agents()[0].cost_usd, Some(0.42));
    assert_eq!(
        parent.sub_agents()[0].tokens,
        Some(crate::store::snapshot::SubAgentTokens::Window(12_400))
    );
}

/// The producer tick's own snapshot must carry a launched child with its
/// context joined, or the park detector never sees the limit marker.
#[test]
fn read_published_snapshot_carries_a_launched_child_parked_on_a_limit() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let state = StatePaths::under(workspace.clone(), dir.path()).unwrap();
    state.ensure_dirs().unwrap();

    let worktree = dir.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let wt = worktree.to_string_lossy().into_owned();
    let parent_pane = pane("terminal_parent", "claude", &wt);
    let child_pane = pane("terminal_child", "codex", &wt);
    let mut parent = root_agent("claude", "parent-1", None);
    parent.worktree_path = Some(wt.clone());
    parent.pane = Some(parent_pane.clone());
    let mut child = child_agent("codex", "parent-1", "child-1");
    child.parent_agent_kind = Some(parent.kind.clone());
    child.launch_depth = Some(1);
    child.worktree_path = Some(wt);
    child.pane = Some(child_pane.clone());
    let error_at = child.last_activity + std::time::Duration::from_secs(1);
    let context = AgentContext {
        turn_error: Some(AgentTurnError {
            class: TurnErrorClass::PausedRateLimit,
            at: error_at,
            label: Some("Usage limit reached".to_owned()),
        }),
        ..AgentContext::new("codex", error_at)
    };
    crate::store::agent_context::write(&runtime, "codex", "child-1", &context).unwrap();
    let mut run = crate::store::run::RunRecord::new(
        workspace.clone(),
        child.kind.clone(),
        crate::agents::PermissionMode::Auto,
        "gate".to_owned(),
        worktree.clone(),
    );
    run.subagent = true;
    run.status = crate::store::run::RunStatus::Running;
    run.agent_id = Some(child.agent_id.clone());
    let mut rollup =
        SidebarSnapshot::build_with_agents(workspace, vec![parent, child], Timestamp::now());
    rollup = rollup.with_project_root(Some(worktree));
    rollup.reflects_log = Some(crate::store::event_log::LogExtent {
        generation: 0,
        offset: 0,
    });
    atomic::write_temp_then_rename(&state.latest_snapshot, &rollup).unwrap();
    let frame = assemble_frame(vec![parent_pane, child_pane], unix_now_ms(), "rimz-test");
    atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), &frame).unwrap();

    let snapshot = read_published_snapshot(
        &mut RollupCursor::new(),
        &state,
        &runtime,
        "rimz-test",
        None,
    )
    .expect("published base");

    let park = crate::harness::park_notice::unnoticed_park(&run, &snapshot.agents)
        .expect("the producer's snapshot shows the park");
    assert_eq!(park.child.agent_id, "child-1");
    assert_eq!(park.parent.agent_id, "parent-1");
    assert_eq!(
        park.child
            .displayed_turn_error()
            .and_then(|(_, error)| error.label.as_deref()),
        Some("Usage limit reached"),
    );
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
        &StatePaths::under(runtime.workspace_id.clone(), dir.path()).unwrap(),
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
        &StatePaths::under(runtime.workspace_id.clone(), dir.path()).unwrap(),
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

struct PublicationFixture {
    _dir: tempfile::TempDir,
    runtime: RuntimePaths,
    state: StatePaths,
    frame: crate::sidebar::frame::PaneFrame,
}

impl PublicationFixture {
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

        let mut frame = assemble_frame(Vec::new(), unix_now_ms(), "rimz-test");
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

    /// The seed read one second after the fixture's projection stamps.
    fn seed_pair(
        &self,
    ) -> Option<(
        WorkspaceSnapshot,
        Option<std::sync::Arc<crate::sidebar::frame::PaneFrame>>,
    )> {
        read_published_pair(
            &self.runtime,
            "rimz-test",
            std::time::Duration::from_secs(10),
            1_012,
        )
    }
}

#[test]
fn published_pair_needs_no_store_or_current_source() {
    let mut fixture = PublicationFixture::new();
    std::fs::remove_file(&fixture.state.latest_snapshot).unwrap();

    let pair = fixture.seed_pair();
    assert!(
        pair.is_some(),
        "a same-session publication seeds without live store stamps"
    );
    let (workspace, frame) = pair.unwrap();
    assert_eq!(workspace.snapshot().display_name, "projected");
    let frame = frame.expect("the projection's own frame pairs without live store stamps");
    assert_eq!(frame.topology_stamp_ms, Some(11));
    assert_eq!(frame.metrics_stamp_ms, Some(12));
    assert_eq!(frame.session_name, "rimz-test");

    for (topology, metrics) in [(Some(99), Some(12)), (Some(11), Some(99)), (None, None)] {
        fixture.frame.topology_stamp_ms = topology;
        fixture.frame.metrics_stamp_ms = metrics;
        atomic::write_temp_then_rename_cache(&fixture.runtime.pane_frame_path(), &fixture.frame)
            .unwrap();
        let (workspace, frame) = fixture.seed_pair().expect("the published cards still seed");
        assert_eq!(workspace.snapshot().display_name, "projected");
        assert!(
            frame.is_none(),
            "a foreign or legacy frame must not pair: topology={topology:?}, metrics={metrics:?}"
        );
    }
}

#[test]
fn published_pair_requires_a_valid_same_session_projection_and_frame() {
    let fixture = PublicationFixture::new();
    assert!(fixture.seed_pair().is_some());

    for (field, value) in [
        ("schema_version", serde_json::json!(5)),
        ("schema_version", serde_json::json!(99)),
        ("session", serde_json::json!("other-session")),
    ] {
        let fixture = PublicationFixture::new();
        let path = workspace_projection_path(&fixture.runtime);
        let mut projection: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        projection[field] = value;
        atomic::write_temp_then_rename_cache(&path, &projection).unwrap();
        assert!(fixture.seed_pair().is_none(), "{field}");
    }

    let fixture = PublicationFixture::new();
    std::fs::write(
        workspace_projection_path(&fixture.runtime),
        b"{broken projection",
    )
    .unwrap();
    assert!(fixture.seed_pair().is_none());

    for path in [workspace_projection_path, RuntimePaths::pane_frame_path] {
        let fixture = PublicationFixture::new();
        std::fs::remove_file(path(&fixture.runtime)).unwrap();
        assert!(fixture.seed_pair().is_none());
    }

    let mut fixture = PublicationFixture::new();
    fixture.frame.session_name = "other-session".to_owned();
    atomic::write_temp_then_rename_cache(&fixture.runtime.pane_frame_path(), &fixture.frame)
        .unwrap();
    assert!(fixture.seed_pair().is_none());
}

#[test]
fn published_seed_age_is_the_projections() {
    let mut fixture = PublicationFixture::new();
    let max_age = std::time::Duration::from_secs(20);

    assert!(read_published_pair(&fixture.runtime, "rimz-test", max_age, 20_012).is_some());
    assert!(
        read_published_pair(&fixture.runtime, "rimz-test", max_age, 20_013).is_none(),
        "an expired projection must not seed"
    );

    // A command outside the sidebar republishes the frame; the projection
    // beside it is as old as before.
    fixture.frame.topology_stamp_ms = Some(20_013);
    fixture.frame.metrics_stamp_ms = Some(20_013);
    fixture.frame.produced_at_ms = 20_013;
    atomic::write_temp_then_rename_cache(&fixture.runtime.pane_frame_path(), &fixture.frame)
        .unwrap();
    assert!(
        read_published_pair(&fixture.runtime, "rimz-test", max_age, 20_013).is_none(),
        "a fresh frame must not carry an old projection"
    );
}

#[test]
fn presence_only_frame_publication_keeps_projection_match() {
    let mut fixture = PublicationFixture::new();
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

    let (_, frame) = fixture.seed_pair().unwrap();
    let frame = frame.expect("presence-only publication keeps the seed frame paired");
    assert_eq!(
        (
            fixture.frame.topology_stamp_ms,
            fixture.frame.metrics_stamp_ms
        ),
        source
    );
    assert_eq!(frame.presence, fixture.frame.presence);
}
