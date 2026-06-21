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

use std::path::PathBuf;

use crate::ids::{MuxName, PaneId};
use crate::sidebar::cache::unix_now_ms;
use crate::sidebar::consumer::{RollupCursor, rollup_snapshot};
use crate::sidebar::enrich::{EnrichMode, enrich};
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
    pub diag: Option<crate::diag::DiagSink>,
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
        Some(fixture) => assemble_frame(fixture, unix_now_ms(), opts.session_name.clone()),
        None => panes::cached_panes_or_produce(
            runtime,
            opts.mux,
            &opts.session_name,
            opts.min_pane_cache_ms,
            opts.exclude.as_ref(),
            opts.diag.as_ref(),
        )?,
    };
    let snapshot = rollup_snapshot(state, cursor)?;
    Ok(enrich_producing(
        snapshot,
        Some(frame),
        runtime,
        opts.exclude.as_ref(),
        opts.min_pane_cache_ms,
        opts.diag.as_ref(),
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
    ))
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

/// Assemble the producer inputs and run the shared enrichment spine
/// ([`crate::sidebar::enrich::enrich`]) in [`EnrichMode::Producing`]. This owns
/// only what forks or walks; the spine owns the fold order.
///
/// - Group roots: a repo room's worktree checkouts, a directory room's
///   depth-1 child repos — cached under `WORKTREE_ROOTS_TTL`, refused below
///   the session-boundary freshness floor (`min_pane_cache_ms`) so a new
///   checkout's first agent re-enumerates immediately.
/// - The fleet spending walk runs before the config fold so the dashboard
///   panels are built, ranked, and capped with each provider's spend known.
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
) -> SidebarSnapshot {
    let roots = snapshot.project_root.clone().map(|root| {
        git::project_group_roots(&root, snapshot.root_class, runtime, min_pane_cache_ms)
    });
    let config = crate::config::MachineConfig::load().unwrap_or_default();
    let trunk = config.sidebar.trunk.clone();
    let headline_spec = config.sidebar.headline_spec();
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
            compute_spending: &compute_spending,
            config: Box::new(config),
            refresh_git: &refresh_git,
        },
        diag,
    )
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
            is_floating: false,
            command: command.map(ToOwned::to_owned),
            spawn_command: None,
            cwd: cwd.map(ToOwned::to_owned),
            pane_pid: None,
            pane_process_start: None,
            resumed_session_id: None,
            elevated_agent: None,
            first_seen_at_ms: None,
        }
    }
}
