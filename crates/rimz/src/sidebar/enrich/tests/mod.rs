use super::*;
use crate::agents::SessionOrigin;
use crate::agents::{AgentState, AgentStatus};
use crate::pane::{RuntimeOwner, RuntimeOwnerKind};
use crate::remote::link::{LinkStats, LinkStatsFile, LinkTier};
use crate::sidebar::refresh::AccountsCache;
use crate::sidebar::refresh::PrLink;
use crate::sidebar::refresh::git_stats::{
    DiffStatsCache, DiffStatsCacheEntry, WorktreeRootsCache, focused_worktree_paths,
    hot_worktree_paths, needed_worktree_paths,
};
use crate::sidebar::refresh::{CodexDaemonReap, read_codex_daemon_reap, write_codex_daemon_reap};
use crate::sidebar::test_support::{activity_row, pane, root_agent, worktree_group};
use crate::sidebar::timing::GIT_ACTIVITY_WINDOW;
use crate::sidebar::timing::unix_now_ms;
use crate::store::atomic;
use jiff::SignedDuration;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

mod agents;
mod cohort;
mod frame;
mod git;
mod labels;
mod paths;
mod spend;

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
    use crate::store::snapshot::RemoteControlBadge::{Down, Healthy, Hidden};

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
            remote_control_badge(config_toggle, pane_auto, server_alive),
            expected,
            "{label}"
        );
    }
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
    snapshot.worktree_groups = vec![channel_group(name, &worktree)];
    (dir, worktree, snapshot)
}

fn diff_cache_with_marker(path: &Path, name: &str) -> DiffStatsCache {
    DiffStatsCache {
        worktrees: Some(WorktreeRootsCache {
            refreshed_at_ms: unix_now_ms(),
            roots: vec![path.to_path_buf()],
            marker_names: Some(BTreeMap::from([(path.to_path_buf(), name.to_owned())])),
        }),
        ..DiffStatsCache::default()
    }
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

fn binding_log_lines(runtime: &RuntimePaths) -> usize {
    let path = runtime.root.join("binding.log.jsonl");
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
    let mut groups = vec![worktree_group(
        path,
        vec![
            agent_row(Some(85)),
            agent_row(Some(5)),
            activity_row(false, None, Timestamp::now(), path),
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
