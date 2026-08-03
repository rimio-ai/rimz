//! The sidebar produce pipeline — what the elected producer runs per data tick.
//!
//! [`produce_snapshot`] resolves the base (the event-fresh store rollup folded
//! through the caller's [`RollupCursor`], plus the live pane frame shared
//! through the single-flight pane cache) and folds the producer enrichments:
//! group roots, context/activity sidecars, the pane overlay, and projection of
//! the cache refresher's published spending/account/git facts. The CLI
//! inspection path uses [`produce_snapshot_with_refresh`] to refresh heavy
//! lanes between two folds over one produced pane frame.
//!
//! The module is read-only on store truth: the rollup arrives through the
//! cursor fold, and every write is cache-class
//! (`snapshot.json`, `diff-stats.json`, `metrics-sample.json`,
//! shared provider/account/spending caches, or the persistent live roster) via
//! `write_temp_then_rename_cache` — rebuilt from truth on the next read, never
//! truth itself. `cargo xtask invariants` pins the boundary: no store-writer,
//! run-wake, or broker imports under `crates/rimz/src/sidebar/`.
//! The consumer-side read lives in [`super::consumer`]; performance model in
//! [docs/internals/performance.md](../../../../../docs/internals/performance.md).

pub(crate) mod git;
mod metrics;
mod panes;
pub(crate) mod tab_status;

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use crate::ids::{AgentKind, AgentSessionId, MuxName, PaneId};
use crate::sidebar::agent_projection::{AgentProjection, WiredAgentProjection};
use crate::sidebar::consumer::{RollupCursor, read_published_snapshot, rollup_snapshot};
use crate::sidebar::enrich::{
    FoldOpts, WorkspaceSnapshot, enrich, enrich_workspace, project_local,
};
use crate::sidebar::frame::{PaneFrame, assemble_frame};
use crate::sidebar::refresh::refresh_heavy_lanes;
use crate::sidebar::timing::unix_now_ms;
use crate::store::snapshot::{PaneAgent, RowCard, SidebarSnapshot, SnapshotErr};
use crate::{ResolvedWorkspace, RuntimePaths, StatePaths, Store};

#[derive(Debug, thiserror::Error)]
pub enum ProduceErr {
    /// The deterministic pane fixture (`RIMZ_TEST_PANE_LIST`) was requested
    /// but unreadable — a test-seam failure, never a production state.
    #[error("reading RIMZ_TEST_PANE_LIST {path}: {reason}")]
    Fixture { path: PathBuf, reason: String },
    /// Pane discovery failed: no live session to enumerate, or the mux errored.
    #[error(transparent)]
    PaneDiscovery(#[from] crate::mux::MuxErr),
    /// The mux returned an Ok-but-implausible pane frame and no prior frame was
    /// available to hold.
    #[error("pane frame rejected: {0:?}")]
    FrameRejected(crate::diag::record::FrameRejectReason),
    /// The store rollup could not be read or projected.
    #[error(transparent)]
    Rollup(#[from] SnapshotErr),
    /// State or runtime paths could not be prepared for the workspace.
    #[error(transparent)]
    Path(#[from] crate::store::paths::PathErr),
    /// The cached store snapshot fallback could not be read.
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
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
    state: &'a StatePaths,
    messages_dir: &'a Path,
    exclude: Option<&'a PaneId>,
    min_pane_cache_ms: Option<u64>,
    diag: &'a crate::diag::DiagSink,
}

#[cfg(feature = "testkit")]
pub(crate) fn publish_test_pane_frame(
    runtime: &RuntimePaths,
    frame: &PaneFrame,
) -> crate::store::atomic::Result<()> {
    crate::store::atomic::write_temp_then_rename_cache(&runtime.pane_frame_path(), frame)
}

/// Produce the full sidebar snapshot: rollup base + live pane frame + producer
/// enrichments. Inline `Refresh` publishes every shared cache consumers read;
/// live `Project` publishes pane/root truth and projects the cache refresher's
/// heavy lanes. `Err` on pane-discovery failure (or an unreadable store) —
/// the caller owns the fallback: the serve loop degrades to its held frame, and
/// CLI inspection can fall back to a frameless refreshed rollup.
pub fn produce_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    opts: &ProduceOptions,
) -> Result<SidebarSnapshot> {
    let produced = produce_workspace_snapshot(cursor, state, runtime, opts)?;
    Ok(project_local(
        produced.workspace,
        Some(&produced.frame),
        opts.exclude.as_ref(),
    ))
}

pub(crate) struct ProducedWorkspaceSnapshot {
    pub(crate) workspace: WorkspaceSnapshot,
    pub(crate) frame: PaneFrame,
}

pub(crate) fn produce_workspace_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    opts: &ProduceOptions,
) -> Result<ProducedWorkspaceSnapshot> {
    let frame = produce_pane_frame(runtime, opts)?;
    let snapshot = rollup_snapshot(state, cursor)?;
    let workspace = enrich_producing_workspace(
        snapshot,
        Some(&frame),
        ProducerEnrich {
            runtime,
            state,
            messages_dir: &state.messages_dir,
            exclude: opts.exclude.as_ref(),
            min_pane_cache_ms: opts.min_pane_cache_ms,
            diag: &opts.diag,
        },
    );
    Ok(ProducedWorkspaceSnapshot { workspace, frame })
}

pub fn live_roster_from_snapshot(
    snapshot: &SidebarSnapshot,
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    let rendered_agent_panes = snapshot
        .rows()
        .filter(|row| matches!(&row.card, RowCard::Agent(_)))
        .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.clone()))
        .collect::<HashSet<_>>();
    snapshot
        .agent_panes
        .iter()
        .filter(|pane_agent| {
            rendered_agent_panes.contains(&pane_agent.pane_id)
                || pane_agent.agent_id.as_ref().is_some_and(|agent_id| {
                    snapshot.agents.iter().any(|agent| {
                        agent.kind == pane_agent.kind
                            && &agent.agent_id == agent_id
                            && agent.is_launched_child()
                    })
                })
        })
        .filter_map(|agent| {
            agent
                .agent_id
                .clone()
                .map(|agent_id| (agent.kind.clone(), agent_id))
        })
        .filter(|(_, agent_id)| !agent_id.is_provisional())
        .filter(|(_, agent_id)| !agent_id.as_str().is_empty())
        .collect()
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
            state,
            messages_dir: &state.messages_dir,
            exclude: opts.exclude.as_ref(),
            min_pane_cache_ms: opts.min_pane_cache_ms,
            diag: &opts.diag,
        },
    ))
}

/// Produce the resolution snapshot: event-fresh rollup plus the live pane frame,
/// and no render spine. Talk/resolve commands need bound and sessionless agent
/// pane targets; they do not read group roots, spending, accounts, provider
/// dashboards, or git facts, so this path pays one pane enumeration and stops
/// there.
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
        crate::sidebar::agent_projection::probe_current(),
    ))
}

/// Produce the snapshot used by commands that resolve message recipients. It
/// folds a fresh pane frame into the event-fresh rollup without the render
/// spine, so just-started sessionless panes are addressable while the command
/// pays only one pane enumeration. When no mux is available, or pane discovery
/// fails, it falls back to the rollup's stamped panes.
pub fn resolution_snapshot(
    workspace: &ResolvedWorkspace,
    store: &Store,
    mux: Option<MuxName>,
) -> Result<SidebarSnapshot> {
    let mux = mux
        .or_else(|| crate::mux::auto_detect_backend(None).ok())
        // A deterministic pane fixture stands in for the mux in tests; produce
        // reads it without touching the real backend, so any mux value serves.
        .or_else(|| pane_fixture_active().then_some(MuxName::Zellij));
    let Some(mux) = mux else {
        return rollup_resolution_snapshot(store);
    };
    let state = StatePaths::for_workspace(workspace.workspace_id.clone())?;
    let runtime = RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    let opts = ProduceOptions {
        mux,
        session_name: workspace.session_name.clone(),
        exclude: None,
        min_pane_cache_ms: Some(crate::sidebar::timing::unix_now_ms()),
        diag: crate::diag::DiagSink::for_workspace(
            workspace.workspace_id.clone(),
            workspace.session_name.clone(),
            None,
        ),
    };
    match produce_resolution_snapshot(&mut RollupCursor::new(), &state, &runtime, &opts) {
        Ok(snapshot) => Ok(snapshot),
        // No live session / pane discovery failed: fall back to the rollup's own
        // stamped panes so a bound agent still resolves, exactly as before.
        Err(err) => {
            opts.diag
                .emit(crate::diag::record::DiagEvent::ResolutionFallback {
                    reason: err.to_string(),
                });
            rollup_resolution_snapshot(store)
        }
    }
}

/// The no-frame fallback: the rollup, with `agent_panes` synthesized from each
/// registered session's stamped pane. Without a live frame there is nothing to
/// cwd-bind or verify, so launch placeholders stay pane-less and only registered
/// sessions that already carry a pane are reachable.
fn rollup_resolution_snapshot(store: &Store) -> Result<SidebarSnapshot> {
    let mut snapshot = store.snapshot_cached()?;
    snapshot.agent_panes = snapshot
        .agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent())
        .filter(|agent| !agent.agent_id.is_provisional())
        .filter_map(|agent| {
            let pane = agent.pane.as_ref()?;
            Some(PaneAgent {
                kind: agent.kind.clone(),
                kind_ordinal: agent.kind_ordinal,
                name: agent.name.clone(),
                name_explicit: agent.name_explicit,
                profile: agent.profile.clone(),
                role: agent.role.clone(),
                channel: agent.channel.clone(),
                agent_id: Some(agent.agent_id.clone()),
                pane_id: pane.pane_id.clone(),
                pane_pid: pane.pane_pid,
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
            state,
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
    state: &StatePaths,
    runtime: &RuntimePaths,
    session: &str,
    exclude: Option<&PaneId>,
) -> Result<()> {
    refresh_producer_caches_with_state(
        cursor,
        state,
        runtime,
        session,
        exclude,
        &mut Default::default(),
    )
}

pub(crate) fn refresh_producer_caches_with_state(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    session: &str,
    exclude: Option<&PaneId>,
    refresh_state: &mut crate::sidebar::refresh::ProducerRefreshState,
) -> Result<()> {
    let base = read_published_snapshot(cursor, state, runtime, session, exclude)?;
    let config = crate::config::MachineConfig::load_lenient();
    let _ = refresh_heavy_lanes(
        &base,
        &base.agents,
        state,
        runtime,
        &config,
        crate::agents::spending::service::SpendingServiceStartup::HostEligible,
        refresh_state,
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
    wired: WiredAgentProjection,
) -> SidebarSnapshot {
    snapshot.wired_kinds = wired.kinds;
    snapshot.wired_default_models = wired.default_models;
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
fn enrich_producing_workspace(
    snapshot: SidebarSnapshot,
    frame: Option<&PaneFrame>,
    opts: ProducerEnrich<'_>,
) -> WorkspaceSnapshot {
    let config = crate::config::MachineConfig::load_lenient();
    let roots = producer_roots(&snapshot, opts.runtime, opts.min_pane_cache_ms);
    let agent_projection = refresh_agent_projection(frame, &opts);
    enrich_workspace(
        snapshot,
        frame,
        opts.runtime,
        Some(opts.messages_dir),
        FoldOpts {
            producing: true,
            fresh_roots: roots,
            config: Some(config),
            lanes: None,
            agent_projection,
        },
        opts.diag,
    )
}

fn enrich_with_refresh(
    snapshot: SidebarSnapshot,
    frame: Option<PaneFrame>,
    opts: ProducerEnrich<'_>,
) -> SidebarSnapshot {
    let config = crate::config::MachineConfig::load_lenient();
    let roots = producer_roots(&snapshot, opts.runtime, opts.min_pane_cache_ms);
    let agent_projection = refresh_agent_projection(frame.as_ref(), &opts);
    let folded = enrich_producing_with(
        snapshot.clone(),
        frame.clone(),
        opts,
        FoldOpts {
            producing: false,
            fresh_roots: roots.clone(),
            config: Some(config.clone()),
            lanes: None,
            agent_projection: agent_projection.clone(),
        },
    );
    // The intermediate fold applies the published daemon-reap cache. Probe from
    // the unreaped rollup so one-shot CLI refresh keeps the pre-split semantics:
    // a stale reap cache cannot hide the only daemon-mode Codex session that
    // should trigger a fresh `thread/loaded/list` read.
    let refreshed = refresh_heavy_lanes(
        &folded,
        &snapshot.agents,
        opts.state,
        opts.runtime,
        &config,
        crate::agents::spending::service::SpendingServiceStartup::OneShot,
        &mut Default::default(),
    );
    enrich_producing_with(
        snapshot,
        frame,
        opts,
        FoldOpts {
            producing: true,
            fresh_roots: roots,
            config: Some(config),
            lanes: Some(&refreshed),
            agent_projection,
        },
    )
}

fn refresh_agent_projection(
    frame: Option<&PaneFrame>,
    opts: &ProducerEnrich<'_>,
) -> AgentProjection {
    let Some(frame) = frame else {
        return AgentProjection {
            wiring: crate::sidebar::agent_projection::probe_current(),
            local_sessions: Vec::new(),
        };
    };
    let panes = SidebarSnapshot::card_admitted_live_panes(frame.to_pane_refs(), None);
    crate::sidebar::agent_projection::refresh_published(opts.runtime, &frame.session_name, &panes)
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
    fold: FoldOpts<'_>,
) -> SidebarSnapshot {
    enrich(
        snapshot,
        frame.as_ref(),
        opts.runtime,
        Some(opts.messages_dir),
        opts.exclude,
        fold,
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
            title: None,
            is_floating: false,
            command: command.map(ToOwned::to_owned),
            foreground_cmdline: None,
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
    use crate::agents::{AgentState, AgentStatus};
    use crate::ids::WorkspaceId;
    use crate::pane::RuntimeOwnerKind;
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
            vec![bound_agent],
            now,
        );
        let frame = assemble_frame(vec![bound_pane, lazy_pane], 123, "rimz-test".to_owned());

        let snapshot = fold_resolution_frame(
            snapshot,
            frame,
            None,
            WiredAgentProjection {
                kinds: vec!["codex".to_owned()],
                default_models: std::collections::BTreeMap::new(),
            },
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
            .expect("sessionless pane");
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

    #[test]
    fn live_roster_keeps_pane_backed_full_sessions() {
        let now = Timestamp::now();
        let live_pane = test_support::pane("live", Some("claude"), Some("/repo/live"));
        let daemon_pane = test_support::pane("daemon", Some("codex"), Some("/repo/daemon"));
        let provisional_pane =
            test_support::pane("provisional", Some("codex"), Some("/repo/provisional"));
        let empty_pane = test_support::pane("empty", Some("codex"), Some("/repo/empty"));
        let wired_pane = test_support::pane("wired", Some("pi"), Some("/repo/wired"));
        let launched_pane = test_support::pane("launched", Some("codex"), Some("/repo/launched"));
        let mut live = agent("claude", "live", now);
        live.pane = Some(live_pane.clone());
        let unknown = agent("codex", "unknown", now);
        let mut daemon = agent("codex", "daemon", now);
        daemon.pane = Some(daemon_pane.clone());
        daemon.runtime_owner = Some(crate::store::runtime::current_process_owner(
            RuntimeOwnerKind::Daemon,
            "daemon",
        ));
        let mut paneless_daemon = agent("codex", "paneless-daemon", now);
        paneless_daemon.runtime_owner = Some(crate::store::runtime::current_process_owner(
            RuntimeOwnerKind::Daemon,
            "paneless-daemon",
        ));
        let mut child = agent("claude", "child", now);
        child.parent_agent_id = Some("live".into());
        let mut launched = agent("codex", "launched", now);
        launched.parent_agent_id = Some("live".into());
        launched.parent_agent_kind = Some(AgentKind::new_unchecked("claude"));
        launched.launch_depth = Some(1);
        launched.pane = Some(launched_pane.clone());
        let mut provisional = agent("codex", "launch_abc", now);
        provisional.pane = Some(provisional_pane.clone());
        let mut empty = agent("codex", "", now);
        empty.pane = Some(empty_pane.clone());
        let mut snapshot = SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(std::path::Path::new("/repo")),
            vec![
                live,
                unknown,
                daemon,
                paneless_daemon,
                child,
                launched,
                provisional,
                empty,
            ],
            now,
        );
        snapshot.wired_kinds.push("pi".to_owned());
        let snapshot = snapshot.with_live_panes(
            vec![
                live_pane,
                daemon_pane,
                provisional_pane,
                empty_pane,
                wired_pane,
                launched_pane,
            ],
            None,
        );

        let roster = live_roster_from_snapshot(&snapshot);

        assert_eq!(
            roster,
            [
                (AgentKind::new_unchecked("claude"), "live".into()),
                (AgentKind::new_unchecked("codex"), "daemon".into()),
                (AgentKind::new_unchecked("codex"), "launched".into()),
            ]
            .into_iter()
            .collect()
        );
    }

    fn agent(kind: &str, id: &str, now: Timestamp) -> AgentState {
        AgentState {
            status: AgentStatus::Running,
            ..crate::testkit::agent_state(kind, id, now)
        }
    }
}
