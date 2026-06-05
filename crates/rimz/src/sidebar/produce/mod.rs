//! The sidebar produce pipeline — what the elected producer runs per data tick.
//!
//! [`produce_snapshot`] resolves the base (the event-fresh ledger rollup folded
//! through the caller's [`RollupCursor`], plus the live pane frame shared
//! through the single-flight pane cache) and folds the producer enrichments:
//! group roots, context/activity sidecars, the Codex daemon reap, the pane
//! overlay, the fleet spending walk, the account probe, and the per-worktree
//! git facts. Two callers drive it: the elder renderer's fetch worker (in
//! process, warm cursor) and the `rimz sidebar snapshot` CLI (one-shot, cold
//! cursor) — one implementation, two entry points.
//!
//! The module is read-only on ledger truth: the rollup arrives through the
//! cursor fold, and every write is a cache-class runtime file
//! (`snapshot.json`, `diff-stats.json`, `metrics-sample.json`,
//! `provider-spending.json`, `spending.json`, `accounts.json`) via
//! `write_temp_then_rename_cache` — rebuilt from truth on the next read, never
//! truth itself. `cargo xtask invariants` pins the boundary: no ledger-writer,
//! feed-store, bridge, or broker imports under `crates/rimz/src/sidebar/`.
//! The consumer-side read lives in [`super::snapshot`]; performance model in
//! [docs/internals/performance.md](../../../../../docs/internals/performance.md).

mod git;
mod metrics;
mod panes;
mod spending;

use std::path::PathBuf;

use crate::ids::{MuxName, PaneId};
use crate::sidebar::snapshot::{RollupCursor, SnapshotCache, rollup_snapshot, unix_now_ms};
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
    /// The ledger rollup could not be read or projected.
    #[error(transparent)]
    Rollup(#[from] crate::ledger::snapshot::SnapshotErr),
}

pub type Result<T> = std::result::Result<T, ProduceErr>;

/// What one produce targets: the session whose panes are read and the caller's
/// own-pane exclusion, plus the pane-freshness floor a lifecycle/resize signal
/// carries (`min_pane_cache_ms` rejects any pane cache or root enumeration
/// older than the signal).
#[derive(Clone, Debug)]
pub struct ProduceOptions {
    pub mux: MuxName,
    pub session_name: String,
    pub exclude: Option<PaneId>,
    pub min_pane_cache_ms: Option<u64>,
}

/// Produce the full sidebar snapshot: rollup base + live pane frame + every
/// producer enrichment, publishing the shared caches consumers read. `Err` on
/// pane-discovery failure (or an unreadable ledger) — the caller owns the
/// fallback: the serve loop degrades to its held frame, the CLI inspection
/// call warns and emits [`produce_rollup_snapshot`].
pub fn produce_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    opts: &ProduceOptions,
) -> Result<SidebarSnapshot> {
    let frame = match pane_list_fixture()? {
        // A test fixture stands in for the mux; never touch the shared cache
        // so deterministic tests can neither poison nor read it.
        Some(fixture) => SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: opts.session_name.clone(),
            panes: fixture,
        },
        None => panes::cached_panes_or_produce(
            runtime,
            opts.mux,
            &opts.session_name,
            opts.min_pane_cache_ms,
        )?,
    };
    let snapshot = rollup_snapshot(state, cursor)?;
    Ok(enrich_producing(
        snapshot,
        Some(frame),
        runtime,
        opts.exclude.as_ref(),
        opts.min_pane_cache_ms,
    ))
}

/// The producer enrichments over the bare rollup, with no pane frame — the
/// inspection arm for a call with no live session, no detectable mux, or a
/// failed pane discovery. Group roots, sidecar folds, spending, accounts, and
/// git all still run; only the pane overlay (gated on a frame) is skipped.
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
    ))
}

/// Whether the deterministic pane fixture is active. The `--no-produce`
/// consumer read defers to the produce path while a fixture stands in for the
/// mux, so deterministic tests neither poison nor read the shared pane cache.
pub fn pane_fixture_active() -> bool {
    std::env::var_os("RIMZ_TEST_PANE_LIST").is_some_and(|value| !value.is_empty())
}

/// The `RIMZ_TEST_PANE_LIST` fixture: a JSON pane list standing in for the
/// mux. Resolved here, inside the produce entry, so the CLI and the in-process
/// fetch worker honor it identically — one deterministic seam for the journey
/// and integration suites.
fn pane_list_fixture() -> Result<Option<Vec<crate::feed::PaneRef>>> {
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

/// Fold the producer enrichments onto a base snapshot, in the produce order
/// the consumer's [`super::snapshot::enrich_consumer`] mirrors read-only.
fn enrich_producing(
    mut snapshot: SidebarSnapshot,
    frame: Option<SnapshotCache>,
    runtime: &RuntimePaths,
    exclude: Option<&PaneId>,
    min_pane_cache_ms: Option<u64>,
) -> SidebarSnapshot {
    // Enumerate the room's group roots — a repo room's worktree checkouts (so
    // one parked outside the project root still earns its own pod instead of
    // folding into `external`), a directory room's depth-1 child repos. The
    // probe is cached under WORKTREE_ROOTS_TTL — refused below the
    // session-boundary freshness floor, so a new checkout's first agent
    // re-enumerates immediately — and runs on the fetch worker, never the
    // render thread.
    if let Some(root) = snapshot.project_root.clone() {
        let roots =
            git::project_group_roots(&root, snapshot.root_class, runtime, min_pane_cache_ms);
        snapshot = snapshot.with_worktree_roots(roots);
    }

    // Fold each session's rich statusline context onto its agent state
    // (read-only; the feed process is the writer). Both the context sidecar
    // and the per-tool activity heartbeats fold only onto existing agents, so
    // an empty room skips both directory scans — the common idle case.
    // Activity lands before the pane overlay so age, ranking, the ask-fold
    // guard, and the stall window all see the truer per-tool value rather than
    // the turn-grained event timestamp.
    if !snapshot.agents.is_empty() {
        snapshot = snapshot.with_agent_context(crate::ledger::agent_context::read_all(runtime));
        snapshot =
            snapshot.with_subagent_context(crate::ledger::subagent_context::read_all(runtime));
        let activity = crate::agent_activity::read_for_keys(
            runtime,
            snapshot
                .agents
                .iter()
                .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
        );
        snapshot = snapshot.with_agent_activity(&activity);
    }

    // Wiring state gates the live-pane fold (the idle-instance synthesis),
    // so set it before folding panes, not after.
    snapshot.wired_lazy_kinds = crate::sidebar::snapshot::wired_lazy_kinds();
    // Reap daemon-mode Codex ghosts the app-server no longer holds: a
    // remote-control conversation records the shared daemon's pid, which
    // outlives it, so process liveness can never reap it. Gated on a
    // pane-less root `codex` session actually being present, so the common
    // room pays no proc scan or daemon probe. Best-effort and fail-safe —
    // no daemon process or an untrusted loaded list keeps every session —
    // and run before the pane fold so a ghost can neither render nor bind
    // its stale stats to a live pane.
    if snapshot.agents.iter().any(|agent| {
        agent.kind == "codex" && agent.pane.is_none() && agent.parent_agent_id.is_none()
    }) {
        let daemon_pids = crate::remote_control::codex_daemon_pids();
        if !daemon_pids.is_empty() {
            let loaded = crate::agents::codex::loaded_daemon_threads();
            snapshot.drop_dead_daemon_sessions(&daemon_pids, loaded.as_ref());
        }
    }
    if let Some(frame) = frame {
        let panes = frame.panes;
        if let Some(own) = exclude {
            snapshot.own_view = crate::SidebarOwnView::from_panes(own, &panes);
        }
        // Computed from the full session pane list (pre-exclusion), before
        // `with_live_panes` consumes `panes`. The panes arrive with their
        // `/proc`-derived process starts already stamped at frame
        // production ([`panes::stamp_pane_process_starts`]), so the
        // cwd-fallback guard fires here and in the consumer in-process fold
        // alike.
        snapshot.only_daemon_view_remains = SidebarSnapshot::only_daemon_view(&panes);
        snapshot = snapshot.with_live_panes(panes, exclude);
    }
    snapshot.agent_hooks_ready = crate::sidebar::snapshot::agent_hooks_ready();
    // Walk transcript history for the fleet + per-provider spend before
    // the config fold, so the dashboard panels are built, ranked, and
    // capped with each provider's spend already known.
    let spending = spending::compute_fleet_spending(runtime);
    // Fold the per-machine config onto the snapshot: the per-provider
    // dashboard (account-scoped budgets, spend, emblem). The producer owns
    // the out-of-band account probe (a subprocess) and publishes it to
    // `accounts.json` for consumer tabs to read back. Best-effort — a config
    // read failure falls back to defaults, so display preference is
    // enrichment, never a precondition. Loaded once here: the fold consumes
    // it and the git probe reads the preferred trunk from it.
    let config = crate::config::MachineConfig::load().unwrap_or_default();
    let trunk = config.sidebar.trunk.clone();
    snapshot = crate::sidebar::snapshot::fold_machine_config_producing(
        snapshot,
        runtime,
        &spending.by_provider,
        config,
    );
    git::enrich_worktree_groups(&mut snapshot, runtime, trunk.as_deref());
    spending::apply_spending(&mut snapshot, &spending);
    snapshot
}

/// Test fixtures shared by the produce submodules' unit suites.
#[cfg(test)]
pub(crate) mod test_support {
    /// A pane with the given id, command, and cwd; other fields are irrelevant
    /// to the helpers under test.
    pub(crate) fn pane(id: &str, command: Option<&str>, cwd: Option<&str>) -> crate::feed::PaneRef {
        crate::feed::PaneRef {
            pane_id: crate::ids::PaneId::from_parts(crate::ids::MuxName::Zellij, id),
            session_name: "s".to_owned(),
            view_id: None,
            view_kind: None,
            view_name: None,
            is_focused: false,
            command: command.map(ToOwned::to_owned),
            cwd: cwd.map(ToOwned::to_owned),
            pane_pid: None,
            pane_process_start: None,
            rss_kb: None,
            cpu_pct: None,
            io_bps: None,
        }
    }
}
