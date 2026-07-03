//! The sidebar produce pipeline — what the elected producer runs per data tick.
//!
//! [`produce_snapshot`] resolves the base (the event-fresh ledger rollup folded
//! through the caller's [`RollupCursor`], plus the live pane frame shared
//! through the single-flight pane cache) and folds the producer enrichments:
//! group roots, context/activity sidecars, the pane overlay, and projection of
//! the cache refresher's published spending/account/git facts. The CLI
//! inspection path uses [`produce_snapshot_with_refresh`] to refresh heavy
//! lanes between two folds over one produced pane frame.
//!
//! The module is read-only on ledger truth: the rollup arrives through the
//! cursor fold, and every write is a cache-class runtime file
//! (`snapshot.json`, `diff-stats.json`, `metrics-sample.json`,
//! shared provider/account/spending caches) via
//! `write_temp_then_rename_cache` — rebuilt from truth on the next read, never
//! truth itself. `cargo xtask invariants` pins the boundary: no ledger-writer,
//! feed-store, bridge, or broker imports under `crates/rimz/src/sidebar/`.
//! The consumer-side read lives in [`super::consumer`]; performance model in
//! [docs/internals/health/performance.md](../../../../../docs/internals/health/performance.md).

pub(crate) mod git;
mod metrics;
mod panes;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::ids::{MuxName, PaneId};
use crate::sidebar::consumer::{RollupCursor, read_published_snapshot, rollup_snapshot};
use crate::sidebar::enrich::{FoldOpts, enrich, wired_lazy_default_models, wired_lazy_kinds};
use crate::sidebar::frame::{PaneFrame, assemble_frame};
use crate::sidebar::refresh::{RefreshedLanes, refresh_heavy_lanes};
use crate::sidebar::timing::unix_now_ms;
use crate::{Ledger, ResolvedWorkspace, RuntimePaths, SidebarSnapshot, StatePaths};

#[derive(Debug, thiserror::Error)]
pub enum ProduceErr {
    /// The deterministic pane fixture (`RIMZ_TEST_PANE_LIST`) was requested
    /// but unreadable — a test-seam failure, never a production state.
    #[error("reading RIMZ_TEST_PANE_LIST {path}: {reason}")]
    Fixture { path: PathBuf, reason: String },
    /// `list-panes` failed: no live session to enumerate, or the mux errored.
    #[error(transparent)]
    PaneDiscovery(#[from] crate::mux::MuxErr),
    /// The mux returned an Ok-but-implausible pane frame and no prior frame was
    /// available to hold.
    #[error("pane frame rejected: {0:?}")]
    FrameRejected(crate::diag::record::FrameRejectReason),
    /// The ledger rollup could not be read or projected.
    #[error(transparent)]
    Rollup(#[from] crate::ledger::snapshot::SnapshotErr),
    /// State or runtime paths could not be prepared for the workspace.
    #[error(transparent)]
    Path(#[from] crate::ledger::paths::PathErr),
    /// The cached ledger snapshot fallback could not be read.
    #[error(transparent)]
    Ledger(#[from] crate::ledger::LedgerErr),
}

pub type Result<T> = std::result::Result<T, ProduceErr>;

/// What one produce targets: the session whose panes are read, the caller's
/// own-pane exclusion, and the pane-freshness floor a lifecycle/resize signal
/// carries (`min_pane_cache_ms` rejects any pane cache or root enumeration
/// older than the signal).
#[derive(Clone, Debug)]
pub struct ProduceOptions {
    pub mux: MuxName,
    pub session_name: String,
    pub exclude: Option<PaneId>,
    pub min_pane_cache_ms: Option<u64>,
    pub diag: crate::diag::DiagSink,
}

#[derive(Clone, Copy)]
struct ProducerEnrich<'a> {
    runtime: &'a RuntimePaths,
    messages_dir: &'a Path,
    exclude: Option<&'a PaneId>,
    min_pane_cache_ms: Option<u64>,
    diag: &'a crate::diag::DiagSink,
}

#[cfg(feature = "testkit")]
pub(crate) fn publish_test_pane_frame(
    runtime: &RuntimePaths,
    frame: &PaneFrame,
) -> crate::ledger::atomic::Result<()> {
    crate::ledger::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), frame)
}

/// Produce the full sidebar snapshot: rollup base + live pane frame + producer
/// enrichments. Inline `Refresh` publishes every shared cache consumers read;
/// live `Project` publishes pane/root truth and projects the cache refresher's
/// heavy lanes. `Err` on pane-discovery failure (or an unreadable ledger) —
/// the caller owns the fallback: the serve loop degrades to its held frame, and
/// CLI inspection can fall back to a frameless refreshed rollup.
pub fn produce_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    opts: &ProduceOptions,
) -> Result<SidebarSnapshot> {
    let frame = produce_pane_frame(runtime, opts)?;
    let snapshot = rollup_snapshot(state, cursor)?;
    Ok(enrich_producing_projecting(
        snapshot,
        Some(frame),
        ProducerEnrich {
            runtime,
            messages_dir: &state.messages_dir,
            exclude: opts.exclude.as_ref(),
            min_pane_cache_ms: opts.min_pane_cache_ms,
            diag: &opts.diag,
        },
    ))
}

/// Produce a one-shot inspection snapshot with freshly refreshed heavy lanes:
/// pane frame once, fold once to derive lane inputs, refresh, then fold the
/// original rollup and same frame again with the returned lane values.
pub fn produce_snapshot_with_refresh(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    opts: &ProduceOptions,
) -> Result<SidebarSnapshot> {
    let frame = produce_pane_frame(runtime, opts)?;
    let snapshot = rollup_snapshot(state, cursor)?;
    Ok(enrich_with_refresh(
        snapshot,
        Some(frame),
        ProducerEnrich {
            runtime,
            messages_dir: &state.messages_dir,
            exclude: opts.exclude.as_ref(),
            min_pane_cache_ms: opts.min_pane_cache_ms,
            diag: &opts.diag,
        },
    ))
}

/// Produce the resolution snapshot: event-fresh rollup plus the live pane frame,
/// and no render spine. Talk/resolve commands need bound and lazy pane targets;
/// they do not read group roots, spending, accounts, provider dashboards, or git
/// facts, so this path pays one pane enumeration and stops there.
pub fn produce_resolution_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    opts: &ProduceOptions,
) -> Result<SidebarSnapshot> {
    let frame = produce_pane_frame(runtime, opts)?;
    let snapshot = rollup_snapshot(state, cursor)?;
    Ok(fold_resolution_frame(
        snapshot,
        frame,
        opts.exclude.as_ref(),
        wired_lazy_kinds(),
        wired_lazy_default_models(),
    ))
}

/// Produce the snapshot used by commands that resolve message recipients. It
/// folds a fresh pane frame into the event-fresh rollup without the render
/// spine, so just-started sessionless panes are addressable while the command
/// pays only one pane enumeration. When no mux is available, or pane discovery
/// fails, it falls back to the rollup's stamped panes.
pub fn resolution_snapshot(
    workspace: &ResolvedWorkspace,
    ledger: &Ledger,
    mux: Option<MuxName>,
) -> Result<SidebarSnapshot> {
    let mux = mux
        .or_else(|| crate::mux::auto_detect_backend(None).ok())
        // A deterministic pane fixture stands in for the mux in tests; produce
        // reads it without touching the real backend, so any mux value serves.
        .or_else(|| pane_fixture_active().then_some(MuxName::Zellij));
    let Some(mux) = mux else {
        return rollup_resolution_snapshot(ledger);
    };
    let state = StatePaths::for_workspace(workspace.workspace_id.clone())?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    let opts = ProduceOptions {
        mux,
        session_name: workspace.session_name.clone(),
        exclude: None,
        min_pane_cache_ms: Some(crate::sidebar::timing::unix_now_ms()),
        diag: crate::diag::DiagSink::disabled(),
    };
    match produce_resolution_snapshot(&mut RollupCursor::new(), &state, &runtime, &opts) {
        Ok(snapshot) => Ok(snapshot),
        // No live session / pane discovery failed: fall back to the rollup's own
        // stamped panes so a bound agent still resolves, exactly as before.
        Err(_) => rollup_resolution_snapshot(ledger),
    }
}

/// The no-frame fallback: the rollup, with `agent_panes` synthesized from each
/// registered session's stamped pane. Without a live frame there is nothing to
/// cwd-bind or verify, so launch placeholders stay pane-less and only registered
/// sessions that already carry a pane are reachable.
fn rollup_resolution_snapshot(ledger: &Ledger) -> Result<SidebarSnapshot> {
    let mut snapshot = ledger.snapshot_cached()?;
    snapshot.agent_panes = snapshot
        .root_agents()
        .filter(|agent| !agent.agent_id.is_provisional())
        .filter_map(|agent| {
            let pane = agent.pane.as_ref()?;
            Some(crate::PaneAgent {
                kind: agent.kind.clone(),
                kind_ordinal: agent.kind_ordinal,
                name: agent.name.clone(),
                profile: agent.profile.clone(),
                role: agent.role.clone(),
                team: agent.team.clone(),
                channel: agent.channel.clone(),
                agent_id: Some(agent.agent_id.clone()),
                pane_id: pane.pane_id.clone(),
                worktree_path: agent.worktree_path.clone(),
                worktree_branch: agent.worktree_branch.clone(),
            })
        })
        .collect();
    Ok(snapshot)
}

pub fn produce_rollup_snapshot_with_refresh(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    exclude: Option<&PaneId>,
    min_pane_cache_ms: Option<u64>,
) -> Result<SidebarSnapshot> {
    let snapshot = rollup_snapshot(state, cursor)?;
    Ok(enrich_with_refresh(
        snapshot,
        None,
        ProducerEnrich {
            runtime,
            messages_dir: &state.messages_dir,
            exclude,
            min_pane_cache_ms,
            diag: &crate::diag::DiagSink::disabled(),
        },
    ))
}

/// Refresh the producer-owned heavy caches from the last published pane frame.
/// The live fetch worker projects these caches; this entry owns their
/// time-driven refresh without re-running the full pane/enrich spine.
pub fn refresh_producer_caches(
    cursor: &mut RollupCursor,
    spending_walker: &mut crate::agents::spending::SpendingWalker,
    state: &StatePaths,
    runtime: &RuntimePaths,
    session: &str,
    exclude: Option<&PaneId>,
) -> Result<()> {
    let base = read_published_snapshot(cursor, state, runtime, session, exclude)?;
    let config = crate::config::MachineConfig::load_lenient();
    let _ = refresh_heavy_lanes(
        &base,
        &base.agents,
        &state.messages_dir,
        runtime,
        &config,
        spending_walker,
    );
    Ok(())
}

/// Whether the deterministic pane fixture is active. The `--no-produce`
/// consumer read defers to the produce path while a fixture stands in for the
/// mux, so deterministic tests neither poison nor read the shared pane cache.
pub fn pane_fixture_active() -> bool {
    std::env::var_os("RIMZ_TEST_PANE_LIST").is_some_and(|value| !value.is_empty())
}

pub fn repaired_pane_frame_for_binding(
    runtime: &RuntimePaths,
    mux: MuxName,
    session: &str,
    command_timeout: std::time::Duration,
) -> Result<PaneFrame> {
    panes::repaired_pane_frame_for_binding(runtime, mux, session, command_timeout)
}

fn produce_pane_frame(runtime: &RuntimePaths, opts: &ProduceOptions) -> Result<PaneFrame> {
    match pane_list_fixture()? {
        // A test fixture stands in for the mux; never touch the shared cache
        // so deterministic tests can neither poison nor read it.
        Some(fixture) => Ok(assemble_frame(
            fixture,
            unix_now_ms(),
            opts.session_name.clone(),
        )),
        None => Ok(panes::cached_panes_or_produce(
            runtime,
            opts.mux,
            &opts.session_name,
            opts.min_pane_cache_ms,
            opts.exclude.as_ref(),
            &opts.diag,
        )?),
    }
}

fn fold_resolution_frame(
    mut snapshot: SidebarSnapshot,
    frame: PaneFrame,
    exclude: Option<&PaneId>,
    lazy_kinds: Vec<String>,
    lazy_default_models: BTreeMap<String, String>,
) -> SidebarSnapshot {
    snapshot.wired_lazy_kinds = lazy_kinds;
    snapshot.lazy_agent_default_models = lazy_default_models;
    snapshot.panes_produced_at_ms = Some(frame.produced_at_ms);
    snapshot.panes_observed_at_ms = Some(frame.observed_at_ms);
    snapshot.with_live_panes(frame.to_pane_refs(), exclude)
}

/// The `RIMZ_TEST_PANE_LIST` fixture: a JSON pane list standing in for the
/// mux. Resolved here, inside the produce entry, so the CLI and the in-process
/// fetch worker honor it identically — one deterministic seam for the journey
/// and integration suites.
fn pane_list_fixture() -> Result<Option<Vec<crate::pane::PaneRef>>> {
    let Some(path) = std::env::var_os("RIMZ_TEST_PANE_LIST").filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    let bytes = std::fs::read(&path).map_err(|source| ProduceErr::Fixture {
        path: path.clone(),
        reason: source.to_string(),
    })?;
    let panes = serde_json::from_slice(&bytes).map_err(|source| ProduceErr::Fixture {
        path,
        reason: source.to_string(),
    })?;
    Ok(Some(panes))
}

/// Assemble the producer inputs and run the shared enrichment spine. This owns
/// the root enumeration; heavy refreshes live in [`crate::sidebar::refresh`].
///
/// - Group roots: a repo room's worktree checkouts — cached under
///   `WORKTREE_ROOTS_TTL`, refused below the session-boundary freshness floor
///   (`min_pane_cache_ms`) so a new checkout's first agent re-enumerates
///   immediately. Directory rooms get git roots from each git-backed row's
///   resolved worktree during the row fold.
/// - The per-machine config loads once (best-effort — a read failure falls
///   back to defaults, so display preference is enrichment, never a precondition).
fn enrich_producing_projecting(
    snapshot: SidebarSnapshot,
    frame: Option<PaneFrame>,
    opts: ProducerEnrich<'_>,
) -> SidebarSnapshot {
    let config = crate::config::MachineConfig::load_lenient();
    let roots = producer_roots(&snapshot, opts.runtime, opts.min_pane_cache_ms);
    enrich_producing_with(snapshot, frame, opts, config, roots, None, true)
}

fn enrich_with_refresh(
    snapshot: SidebarSnapshot,
    frame: Option<PaneFrame>,
    opts: ProducerEnrich<'_>,
) -> SidebarSnapshot {
    let config = crate::config::MachineConfig::load_lenient();
    let roots = producer_roots(&snapshot, opts.runtime, opts.min_pane_cache_ms);
    let folded = enrich_producing_with(
        snapshot.clone(),
        frame.clone(),
        opts,
        config.clone(),
        roots.clone(),
        None,
        false,
    );
    let mut walker = crate::agents::spending::SpendingWalker::new();
    // The intermediate fold applies the published daemon-reap cache. Probe from
    // the unreaped rollup so one-shot CLI refresh keeps the pre-split semantics:
    // a stale reap cache cannot hide the only daemon-mode Codex session that
    // should trigger a fresh `thread/loaded/list` read.
    let refreshed = refresh_heavy_lanes(
        &folded,
        &snapshot.agents,
        opts.messages_dir,
        opts.runtime,
        &config,
        &mut walker,
    );
    enrich_producing_with(snapshot, frame, opts, config, roots, Some(&refreshed), true)
}

fn producer_roots(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    min_pane_cache_ms: Option<u64>,
) -> Option<Vec<PathBuf>> {
    snapshot.project_root.clone().map(|root| {
        git::project_group_roots(&root, snapshot.root_class, runtime, min_pane_cache_ms)
    })
}

fn enrich_producing_with(
    snapshot: SidebarSnapshot,
    frame: Option<PaneFrame>,
    opts: ProducerEnrich<'_>,
    config: Arc<crate::config::MachineConfig>,
    roots: Option<Vec<PathBuf>>,
    lanes: Option<&RefreshedLanes>,
    producing: bool,
) -> SidebarSnapshot {
    enrich(
        snapshot,
        frame,
        opts.runtime,
        Some(opts.messages_dir),
        opts.exclude,
        FoldOpts {
            producing,
            fresh_roots: roots,
            config: Some(config),
            lanes,
        },
        opts.diag,
    )
}

/// Test fixtures shared by the produce submodules' unit suites.
#[cfg(test)]
pub(crate) mod test_support {
    /// A pane with the given id, command, and cwd; other fields are irrelevant
    /// to the helpers under test.
    pub(crate) fn pane(id: &str, command: Option<&str>, cwd: Option<&str>) -> crate::pane::PaneRef {
        crate::pane::PaneRef {
            pane_id: crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, id),
            session_name: "s".to_owned(),
            view_id: None,
            view_kind: None,
            view_name: None,
            is_focused: false,
            is_floating: false,
            command: command.map(ToOwned::to_owned),
            spawn_command: None,
            cwd: cwd.map(ToOwned::to_owned),
            pane_pid: None,
            pane_process_start: None,
            hosted_agent_kind: None,
            hosted_agent_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::lifecycle::TurnPhase;
    use crate::agents::{AgentState, AgentStatus};
    use crate::ids::{AgentKind, WorkspaceId};
    use jiff::Timestamp;

    #[test]
    fn resolution_frame_folds_bound_and_lazy_panes_without_render_spine() {
        let now = Timestamp::from_second(1_750_000_000).expect("fixed timestamp");
        let bound_pane = test_support::pane("bound", Some("claude"), Some("/repo/main"));
        let lazy_pane = test_support::pane("lazy", Some("codex"), Some("/repo/lazy"));
        let mut bound_agent = agent("claude", "sess-bound", now);
        bound_agent.pane = Some(bound_pane.clone());
        bound_agent.worktree_path = Some("/repo/main".to_owned());
        let snapshot = SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(std::path::Path::new("/repo/main")),
            Vec::new(),
            vec![bound_agent],
            now,
        );
        let frame = assemble_frame(vec![bound_pane, lazy_pane], 123, "rimz-test".to_owned());

        let snapshot = fold_resolution_frame(
            snapshot,
            frame,
            None,
            vec!["codex".to_owned()],
            BTreeMap::new(),
        );

        let bound = snapshot
            .agent_panes
            .iter()
            .find(|pane| pane.agent_id.as_deref() == Some("sess-bound"))
            .expect("bound pane");
        assert_eq!(bound.kind.as_str(), "claude");
        assert_eq!(bound.pane_id.raw(), "bound");
        let lazy = snapshot
            .agent_panes
            .iter()
            .find(|pane| pane.agent_id.is_none())
            .expect("lazy pane");
        assert_eq!(lazy.kind.as_str(), "codex");
        assert_eq!(lazy.pane_id.raw(), "lazy");
        assert_eq!(snapshot.panes_produced_at_ms, Some(123));
        assert_eq!(snapshot.panes_observed_at_ms, Some(123));
        assert!(snapshot.providers.is_empty());
        assert!(snapshot.value_tally.is_none());
        assert!(snapshot.workspace_value_tally.is_none());
        assert!(snapshot.today_spend_live_usd.is_none());
        assert!(snapshot.worktree_roots.is_empty());
    }

    fn agent(kind: &str, id: &str, now: Timestamp) -> AgentState {
        AgentState {
            agent_id: id.into(),
            kind: AgentKind::new_unchecked(kind),
            name: None,
            kind_ordinal: None,
            profile: None,
            role: None,
            team: None,
            launch_group: None,
            launch_ordinal: None,
            channel: None,
            status: AgentStatus::Running,
            phase: TurnPhase::Idle,
            pane: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: None,
            worktree_branch: None,
            task: None,
            prompt: None,
            description: None,
            transcript_path: None,
            origin: None,
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            cache_write_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_compact_command_tokens: None,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
