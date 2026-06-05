//! The off-thread fetch machinery: the two-speed fetch cycle (in-process fast
//! lane plus the forked produce), its single-flight request coalescing, and
//! the lightweight self-close probe worker. Everything here runs on a worker
//! thread so the render/input loop never blocks on a subprocess.

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use rimz::ids::PaneId;
use rimz::mux::PaneListOptions;
use rimz::{MuxName, RuntimePaths, SidebarSnapshot, WorkspaceId};
use tracing::debug;

use super::input::{SELF_CLOSE_WAKEUP, SNAPSHOT_WAKEUP};
use super::lifecycle::{PROBE_COMMAND_TIMEOUT, SelfCloseState, self_close_decision};
use super::reload::resolve_snapshot_bin;
use super::{Result, ServeConfig, SidebarAppErr, tick_for};

/// Fork `rimz sidebar snapshot` for the producer: it resolves the workspace,
/// runs `list-panes` and git, and publishes the shared cache the consumers read.
/// Off the render loop (fetch worker thread), so the round-trip never stalls
/// animation. Consumers do not call this — they read the published cache in
/// process via [`rimz::sidebar::snapshot::read_published_snapshot`].
fn fetch_snapshot_for(
    rimz_bin: &Path,
    workspace_id: &WorkspaceId,
    mux: Option<MuxName>,
    session_name: Option<&str>,
    exclude_pane_id: Option<PaneId>,
    min_pane_cache_ms: Option<u64>,
) -> Result<SidebarSnapshot> {
    let mut command = Command::new(rimz_bin);
    command
        .args(["sidebar", "snapshot", "--workspace-id"])
        .arg(workspace_id.as_str());
    if let Some(mux) = mux {
        command.args(["--mux", mux.as_str()]);
    }
    if let Some(session_name) = session_name {
        command.args(["--session-name", session_name]);
    }
    if let Some(pane_id) = exclude_pane_id {
        command.args(["--exclude-pane-id", pane_id.as_str()]);
    }
    if let Some(min_pane_cache_ms) = min_pane_cache_ms {
        command
            .arg("--min-pane-cache-ms")
            .arg(min_pane_cache_ms.to_string());
    }
    command.arg("--json");
    let output = command
        .output()
        .map_err(|source| SidebarAppErr::CommandIo {
            program: rimz_bin.display().to_string(),
            source,
        })?;
    if !output.status.success() {
        return Err(SidebarAppErr::SnapshotCommand {
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// One refresh cycle's result: the snapshot fetch outcome. A cycle can post
/// two — the in-process fast frame, then the forked produce that reconciles
/// it. `final_for_request` marks the cycle's last outcome: the loop clears
/// `in_flight` (and releases any deferred refetch) only on it, so the
/// single-flight discipline still counts whole cycles, not posts.
pub(super) struct FetchOutcome {
    pub(super) snapshot: std::result::Result<SidebarSnapshot, String>,
    pub(super) final_for_request: bool,
}

/// One fetch cycle, posting one or two outcomes. Runs on the fetch worker
/// thread, keeping the `list-panes` + git round-trip off the render/input loop
/// so animation never stalls on it.
///
/// **Fast lane (every cycle, producer and consumer alike):** fold the
/// event-fresh ledger rollup over the published pane frame entirely in process
/// ([`rimz::sidebar::snapshot::read_published_snapshot`]) — no fork, no
/// `list-panes`, no git. This is the paint that lands a status flip or a cost
/// update within one wakeup, in single-digit milliseconds — and it runs even
/// over an aged pane frame, so a dead producer stales only pane *presence*
/// while status keeps flowing. Skipped only when no usable frame exists (cold
/// start); the produce below recovers that.
///
/// **Produce lane (the fork):** `rimz sidebar snapshot` refreshes pane truth,
/// git, spending, and accounts, and publishes the shared caches. One producer
/// per workspace — the eldest live instance — and on it the fork is gated to
/// the data tick: a ledger-delta storm paints per delta but forks at most once
/// per tick. A consumer forks only when forced (reload, fresh panes): stale-
/// frame recovery belongs to the election, not the consumers — a dead elder's
/// heartbeat ages out within one TTL and the next-eldest *becomes* the
/// producer, so a wedged producer costs one handoff, never an every-consumer
/// produce storm (the old self-heal, whose single-flight loser wait was
/// shorter than a `list-panes`, so every loser timed out into its own
/// uncached produce). A lone renderer is its own next-eldest, so it still
/// self-heals through the producer branch.
fn run_fetch_cycle(
    config: &ServeConfig,
    runtime: &RuntimePaths,
    request: FetchRequest,
    post: &mut dyn FnMut(FetchOutcome),
) {
    let is_producer = !rimz::sidebar::elder_sidebar_present(runtime, &config.instance_id);
    let exclude = rimz::mux::own_pane_id(config.mux);
    let now_ms = rimz::sidebar::snapshot::unix_now_ms();
    let fast = rimz::ledger::paths::StatePaths::for_workspace(config.workspace_id.clone())
        .ok()
        .and_then(|state| {
            rimz::sidebar::snapshot::read_published_snapshot(
                &state,
                runtime,
                &config.session_name,
                exclude.as_ref(),
            )
        });
    // The published frame's age decides whether the producer pays the fork this
    // cycle: a frame younger than one data tick still carries the truth the
    // fork would re-derive (pane TTL, git TTL, spend caches all outlive it).
    // Read from the published pane-frame stamp, and only when the fast lane
    // actually folded that frame — an unreadable ledger must produce, never
    // coast on a young stamp it could not use.
    let frame_age_ms = fast.as_ref().and_then(|_| {
        rimz::sidebar::snapshot::published_frame_age_ms(runtime, &config.session_name, now_ms)
    });
    let produce = produce_this_cycle(
        is_producer,
        request.force_produce,
        frame_age_ms,
        tick_for(config.tick_seconds).as_millis() as u64,
    );
    let fast_posted = fast.is_some();
    if let Some(snapshot) = fast {
        post(FetchOutcome {
            snapshot: Ok(snapshot),
            final_for_request: !produce,
        });
    }
    if produce {
        post(FetchOutcome {
            snapshot: fetch_snapshot_for(
                &resolve_snapshot_bin(&config.rimz_bin),
                &config.workspace_id,
                Some(config.mux),
                Some(&config.session_name),
                exclude,
                request.min_pane_cache_ms,
            )
            .map_err(|err| err.to_string()),
            final_for_request: true,
        });
    } else if !fast_posted {
        // Consumer with no published frame yet (cold start): report the soft
        // miss so the gate holds its last good frame.
        post(FetchOutcome {
            snapshot: Err("waiting for the producer's first published snapshot".to_owned()),
            final_for_request: true,
        });
    }
}

/// Whether this cycle pays the fork, decided from cheap pre-reads. Pure, so
/// the fork-gating contract is unit-testable: the producer forks when forced
/// or when the published frame outlived one data tick (`None` age = no usable
/// frame — cold start); a consumer forks only when explicitly forced (reload,
/// fresh panes). A consumer never forks on a stale frame — staleness recovery
/// is delegated to the election: once the dead elder's heartbeat ages out
/// (≤ one TTL) the next-eldest renderer *is* the producer and recovers through
/// the branch above, while everyone else keeps folding the held panes with the
/// event-fresh rollup. Exactly one producer at any moment, never a per-
/// consumer produce storm; the lone renderer is its own next-eldest.
fn produce_this_cycle(
    is_producer: bool,
    force_produce: bool,
    frame_age_ms: Option<u64>,
    tick_ms: u64,
) -> bool {
    if is_producer {
        force_produce || frame_age_ms.is_none_or(|age| age >= tick_ms)
    } else {
        force_produce
    }
}

/// One request to the fetch worker. `force_produce` makes the run take the
/// producer path (real `list-panes`/git) regardless of election. When it is
/// paired with `min_pane_cache_ms`, the producer ignores a pane cache older
/// than the signal that asked for fresh topology.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct FetchRequest {
    force_produce: bool,
    min_pane_cache_ms: Option<u64>,
}

impl FetchRequest {
    pub(super) fn fresh_panes() -> Self {
        Self {
            force_produce: true,
            min_pane_cache_ms: Some(rimz::sidebar::snapshot::unix_now_ms()),
        }
    }

    fn merge(&mut self, other: Self) {
        self.force_produce |= other.force_produce;
        self.min_pane_cache_ms = match (self.min_pane_cache_ms, other.min_pane_cache_ms) {
            (Some(current), Some(next)) => Some(current.max(next)),
            (Some(current), None) => Some(current),
            (None, Some(next)) => Some(next),
            (None, None) => None,
        };
    }
}

/// Spawn the background fetch worker. It blocks for a request, coalesces any
/// that piled up (a ledger-delta storm collapses to one fetch), runs one
/// [`run_fetch_cycle`], hands the result back over `result_tx`, and pokes the
/// loop's wakeup socket so it folds the result without polling. The thread ends
/// when the loop drops `request_tx`.
pub(super) fn spawn_fetch_worker(
    config: ServeConfig,
    runtime: RuntimePaths,
    socket_path: PathBuf,
    request_rx: std::sync::mpsc::Receiver<FetchRequest>,
    result_tx: std::sync::mpsc::Sender<FetchOutcome>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let waker = UnixDatagram::unbound().ok();
        while let Ok(first) = request_rx.recv() {
            // Coalesce any requests that piled up into one run, keeping the
            // strongest intent and the newest pane-freshness floor.
            let mut request = first;
            while let Ok(extra) = request_rx.try_recv() {
                request.merge(extra);
            }
            // Post each outcome as it lands and poke the loop per post, so the
            // fast in-process frame paints while the fork (if any) still runs.
            let mut disconnected = false;
            run_fetch_cycle(&config, &runtime, request, &mut |outcome| {
                if result_tx.send(outcome).is_err() {
                    disconnected = true;
                    return;
                }
                if let Some(waker) = &waker {
                    let _ = waker.send_to(SNAPSHOT_WAKEUP, &socket_path);
                }
            });
            if disconnected {
                return;
            }
        }
    })
}

pub(super) fn spawn_self_close_probe_worker(
    config: ServeConfig,
    socket_path: PathBuf,
    request_rx: std::sync::mpsc::Receiver<SelfCloseProbeRequest>,
    result_tx: std::sync::mpsc::Sender<SelfCloseProbeOutcome>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let waker = UnixDatagram::unbound().ok();
        while let Ok(first) = request_rx.recv() {
            let mut delay = first.delay;
            while let Ok(extra) = request_rx.try_recv() {
                delay = delay.min(extra.delay);
            }
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            let outcome = run_self_close_probe(&config);
            if result_tx.send(outcome).is_err() {
                return;
            }
            if let Some(waker) = &waker {
                let _ = waker.send_to(SELF_CLOSE_WAKEUP, &socket_path);
            }
        }
    })
}

fn run_self_close_probe(config: &ServeConfig) -> SelfCloseProbeOutcome {
    let Some(own) = rimz::mux::own_pane_id(config.mux) else {
        return SelfCloseProbeOutcome {
            sibling_count: None,
            error: None,
        };
    };
    match rimz::mux::backend_for(config.mux).list_panes(PaneListOptions {
        session_name: Some(config.session_name.clone()),
        command_timeout: Some(PROBE_COMMAND_TIMEOUT),
    }) {
        Ok(panes) => SelfCloseProbeOutcome {
            // This probe reads only `sibling_count`.
            sibling_count: rimz::SidebarOwnView::from_panes(&own, &panes)
                .map(|view| view.sibling_count),
            error: None,
        },
        Err(err) => SelfCloseProbeOutcome {
            sibling_count: None,
            error: Some(err.to_string()),
        },
    }
}

/// Ask the fetch worker for a fresh snapshot. `in_flight` collapses redundant
/// requests while one is already running; `force_after` (set by a ledger delta,
/// i.e. new committed data) guarantees one more fetch once the in-flight one
/// returns, so a delta that races an in-flight fetch is never lost.
/// `request` carries the strongest freshness requirement currently known.
pub(super) fn request_fetch(
    request_tx: &std::sync::mpsc::Sender<FetchRequest>,
    in_flight: &mut bool,
    pending_refetch: &mut Option<FetchRequest>,
    request: FetchRequest,
    force_after: bool,
) {
    if !*in_flight {
        if request_tx.send(request).is_ok() {
            *in_flight = true;
        }
    } else if force_after {
        match pending_refetch {
            Some(pending) => pending.merge(request),
            None => *pending_refetch = Some(request),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SelfCloseProbeRequest {
    delay: Duration,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct SelfCloseProbeOutcome {
    sibling_count: Option<usize>,
    error: Option<String>,
}

/// Ask the lightweight self-close worker for a live sibling count. While a
/// probe is already running, keep the shortest pending delay so an immediate
/// resize probe wins over the startup grace recheck.
pub(super) fn request_self_close_probe(
    request_tx: &std::sync::mpsc::Sender<SelfCloseProbeRequest>,
    in_flight: &mut bool,
    pending_delay: &mut Option<Duration>,
    delay: Duration,
) {
    if !*in_flight {
        if request_tx.send(SelfCloseProbeRequest { delay }).is_ok() {
            *in_flight = true;
        }
        return;
    }
    *pending_delay = Some(pending_delay.map_or(delay, |pending| pending.min(delay)));
}

/// Fold a fast probe result into the same latch the snapshot path uses. The
/// probe is best-effort metadata: failures never degrade the rendered frame
/// because the normal snapshot backstop still owns recovery.
pub(super) fn apply_self_close_probe_outcome(
    config: &ServeConfig,
    outcome: SelfCloseProbeOutcome,
    self_close: &mut SelfCloseState,
) -> bool {
    if let Some(error) = outcome.error {
        debug!(
            session = %config.session_name,
            error = %error,
            "self-close pane probe failed",
        );
        return false;
    }
    if self_close_decision(self_close, outcome.sibling_count) {
        debug!(
            session = %config.session_name,
            "sidebar tab emptied; exiting after resize probe",
        );
        return true;
    }
    false
}

#[cfg(test)]
mod tests;
