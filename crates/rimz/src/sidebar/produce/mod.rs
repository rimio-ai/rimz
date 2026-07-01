//! The sidebar produce pipeline — what the elected producer runs per data tick.
//!
//! [`produce_snapshot`] resolves the base (the event-fresh ledger rollup folded
//! through the caller's [`RollupCursor`], plus the live pane frame shared
//! through the single-flight pane cache) and folds the producer enrichments:
//! group roots, context/activity sidecars, the Codex daemon reap, the pane
//! overlay, and either inline heavy-lane refreshes or projection of the cache
//! refresher's published spending/account/git facts. Two callers drive it: the
//! elder renderer's fetch worker (in process, warm cursor, projected heavy
//! lanes) and the `rimz sidebar snapshot` CLI (one-shot, cold cursor, inline
//! refresh) — one implementation, two entry points.
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

mod git;
mod metrics;
mod panes;
mod spending;

use std::{collections::BTreeMap, path::PathBuf};

use crate::ids::{MuxName, PaneId};
use crate::sidebar::cache::unix_now_ms;
use crate::sidebar::consumer::{RollupCursor, read_published_snapshot, rollup_snapshot};
use crate::sidebar::enrich::{
    EnrichMode, HeavyLanes, enrich, wired_lazy_default_models, wired_lazy_kinds,
};
use crate::sidebar::frame::{PaneFrame, assemble_frame};
use crate::{RuntimePaths, SidebarSnapshot, StatePaths};

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
    FrameRejected(crate::schema::diag::FrameRejectReason),
    /// The ledger rollup could not be read or projected.
    #[error(transparent)]
    Rollup(#[from] crate::ledger::snapshot::SnapshotErr),
}

pub type Result<T> = std::result::Result<T, ProduceErr>;

/// What one produce targets: the session whose panes are read, the caller's
/// own-pane exclusion, the pane-freshness floor a lifecycle/resize signal
/// carries (`min_pane_cache_ms` rejects any pane cache or root enumeration
/// older than the signal), and whether heavy lanes refresh inline or project
/// the cache refresher's last publish.
#[derive(Clone, Debug)]
pub struct ProduceOptions {
    pub mux: MuxName,
    pub session_name: String,
    pub exclude: Option<PaneId>,
    pub min_pane_cache_ms: Option<u64>,
    pub diag: Option<crate::diag::DiagSink>,
    pub heavy_lanes: HeavyLaneMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeavyLaneMode {
    Refresh,
    Project,
}

#[cfg(feature = "testkit")]
pub(crate) fn publish_test_pane_frame(
    runtime: &RuntimePaths,
    frame: &PaneFrame,
) -> crate::ledger::atomic::Result<()> {
    crate::ledger::atomic::write_temp_then_rename_cache(&runtime.root.join("snapshot.json"), frame)
}

/// Produce the full sidebar snapshot: rollup base + live pane frame + producer
/// enrichments. Inline `Refresh` publishes every shared cache consumers read;
/// live `Project` publishes pane/root truth and projects the cache refresher's
/// heavy lanes. `Err` on pane-discovery failure (or an unreadable ledger) —
/// the caller owns the fallback: the serve loop degrades to its held frame, the
/// CLI inspection call warns and emits [`produce_rollup_snapshot`].
pub fn produce_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    opts: &ProduceOptions,
) -> Result<SidebarSnapshot> {
    let frame = produce_pane_frame(runtime, opts)?;
    let snapshot = rollup_snapshot(state, cursor)?;
    Ok(enrich_producing(
        snapshot,
        Some(frame),
        runtime,
        opts.exclude.as_ref(),
        opts.min_pane_cache_ms,
        opts.diag.as_ref(),
        opts.heavy_lanes,
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

/// The producer enrichments over the bare rollup, with no pane frame — the
/// inspection arm for a call with no live session, no detectable mux, or a
/// failed pane discovery. Sidecar folds, spending, and accounts still run;
/// rendered groups stay empty because no pane frame admitted cards.
pub fn produce_rollup_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    exclude: Option<&PaneId>,
    min_pane_cache_ms: Option<u64>,
) -> Result<SidebarSnapshot> {
    let snapshot = rollup_snapshot(state, cursor)?;
    Ok(enrich_producing(
        snapshot,
        None,
        runtime,
        exclude,
        min_pane_cache_ms,
        None,
        HeavyLaneMode::Refresh,
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
    let config = crate::config::MachineConfig::load().unwrap_or_default();
    let trunk = config.sidebar.trunk.clone();
    let headline_spec = config.headline_spec();
    let resume_messages =
        crate::sidebar::enrich::read_auto_continue_resume_messages(runtime, &config.resume);
    let spending = spending::compute_fleet_spending_with_walker(
        spending_walker,
        runtime,
        &base,
        &headline_spec,
    );
    let _ = crate::sidebar::enrich::fold_machine_config_producing(
        base.clone(),
        runtime,
        &spending.provider.spending.by_provider,
        config,
        &resume_messages,
    );
    let mut git_snapshot = base;
    git::enrich_worktree_groups(&mut git_snapshot, runtime, trunk.as_deref());
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
            opts.diag.as_ref(),
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
    snapshot.panes_observed_at_ms = Some(frame.observed_or_produced_at_ms());
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

/// Assemble the producer inputs and run the shared enrichment spine
/// ([`crate::sidebar::enrich::enrich`]) in [`EnrichMode::Producing`]. This owns
/// only what forks or walks; the spine owns the fold order.
///
/// - Group roots: a repo room's worktree checkouts — cached under
///   `WORKTREE_ROOTS_TTL`, refused below the session-boundary freshness floor
///   (`min_pane_cache_ms`) so a new checkout's first agent re-enumerates
///   immediately. Directory rooms get git roots from each git-backed row's
///   resolved worktree during the row fold.
/// - In `Refresh` mode, the fleet spending walk runs before the config fold so
///   the dashboard panels are built, ranked, and capped with each provider's
///   spend known; in `Project` mode, the published heavy caches are folded
///   read-only.
/// - The per-machine config loads once (best-effort — a read failure falls
///   back to defaults, so display preference is enrichment, never a
///   precondition): the spine's config fold consumes it, and the git refresh
///   closure takes the preferred trunk from it.
fn enrich_producing(
    snapshot: SidebarSnapshot,
    frame: Option<PaneFrame>,
    runtime: &RuntimePaths,
    exclude: Option<&PaneId>,
    min_pane_cache_ms: Option<u64>,
    diag: Option<&crate::diag::DiagSink>,
    heavy: HeavyLaneMode,
) -> SidebarSnapshot {
    let roots = snapshot.project_root.clone().map(|root| {
        git::project_group_roots(&root, snapshot.root_class, runtime, min_pane_cache_ms)
    });
    let config = crate::config::MachineConfig::load().unwrap_or_default();
    match heavy {
        HeavyLaneMode::Refresh => {
            let trunk = config.sidebar.trunk.clone();
            let headline_spec = config.headline_spec();
            let compute_spending = |snapshot: &SidebarSnapshot| {
                spending::compute_fleet_spending(runtime, snapshot, &headline_spec)
            };
            let refresh_git = |snapshot: &mut SidebarSnapshot| {
                git::enrich_worktree_groups(snapshot, runtime, trunk.as_deref());
            };
            enrich(
                snapshot,
                frame,
                runtime,
                exclude,
                EnrichMode::Producing {
                    roots,
                    heavy: HeavyLanes::Refresh {
                        compute_spending: &compute_spending,
                        refresh_git: &refresh_git,
                    },
                    config: Box::new(config),
                },
                diag,
            )
        }
        HeavyLaneMode::Project => enrich(
            snapshot,
            frame,
            runtime,
            exclude,
            EnrichMode::Producing {
                roots,
                heavy: HeavyLanes::Project,
                config: Box::new(config),
            },
            diag,
        ),
    }
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
            channel: None,
            status: AgentStatus::Running,
            phase: TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
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
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        }
    }
}
