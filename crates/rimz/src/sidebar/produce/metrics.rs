//! Per-pane `/proc` resource metrics on the sampling cadence's own clock: the
//! two-sample CPU/IO rates, the persisted pane→root-pid bindings with their
//! starttime pid-reuse guard, and the Zellij pid backfill walk.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::ProcessState;
use crate::ids::PaneId;
use crate::ledger::atomic;
use crate::sidebar::cache::unix_now_ms;
use crate::sidebar::frame::{PaneFrame, PaneMetrics, PaneState};
use crate::sidebar::timing::{METRICS_HOT_SAMPLE_TTL, METRICS_SAMPLE_TTL};

mod zellij;

pub(super) use zellij::backfill_zellij_pane_pids_from_proc;
use zellij::process_state_from_stat;
#[cfg(test)]
use zellij::{backfill_zellij_pane_pids, resolve_candidate_root};

/// Per-pane CPU and IO tick counters sampled by the producer on the previous
/// tick, plus the pane's root-pid binding. Two consecutive readings plus the
/// elapsed wall time give rates; the binding lets the next tick restore a
/// Zellij pane's root pid for one guarded stat read instead of the full
/// `/proc` table walk that re-deriving it costs.
#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
struct MetricsSampleEntry {
    /// The PID the metrics were read from (shell or its single foreground
    /// child). `0` records an unbound sample attempt, so pidless panes that
    /// cannot be matched still respect the hot/idle retry cadence.
    stats_pid: u32,
    /// utime + stime ticks from `/proc/<pid>/stat` at sample time.
    cpu_ticks: u64,
    /// rchar + wchar bytes from `/proc/<pid>/io` at sample time.
    io_bytes: u64,
    /// Unix milliseconds when this sample was taken.
    sampled_at_ms: u64,
    /// The pane's root pid (tmux semantics: the direct child of the mux
    /// server), recorded so the next tick restores the binding instead of
    /// re-matching the pane against the whole process table.
    #[serde(default)]
    pane_pid: Option<u32>,
    /// `starttime` ticks (stat field 22) of the root pid at record time — the
    /// exact pid-reuse guard: a recycled pid carries a different start time,
    /// so a stale binding can never latch onto a stranger's process.
    #[serde(default)]
    root_start_ticks: Option<u64>,
    /// The pane's foreground command at sample time — the re-tenancy guard for
    /// the within-TTL carry. A changed command means the values below belong to
    /// the prior tenant (on tmux the root pid is the shell and survives every
    /// foreground change, so pid identity alone cannot tell), and the carry
    /// skips rather than mislabel the fresh process for a sample window.
    #[serde(default)]
    command: Option<String>,
    /// Last computed display values, persisted so a within-TTL produce copies
    /// them onto the matching pane instead of re-reading `/proc`. `cpu_pct` /
    /// `io_bps` are `None` on an entry's first sample (no prior reading to
    /// rate); `rss_kb` is the last stat read.
    #[serde(default)]
    cpu_pct: Option<u16>,
    #[serde(default)]
    io_bps: Option<u64>,
    #[serde(default)]
    rss_kb: Option<u64>,
    /// Last `/proc/<pid>/stat` state character for the sampled process.
    #[serde(default)]
    state_char: Option<char>,
    /// Last stuck verdict, carried across fresh windows without re-reading
    /// `/proc`.
    #[serde(default)]
    process_state: Option<ProcessState>,
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct MetricsSampleCache {
    /// Unix ms of the last `/proc` sample written by this cache shape. The
    /// per-pane entry stamps own the cadence gate; this top-level stamp stays
    /// as a cheap diagnostic and a stable field for older cache readers.
    #[serde(default)]
    sampled_at_ms: u64,
    entries: HashMap<String, MetricsSampleEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PaneRootBinding {
    pub(super) pid: u32,
    pub(super) start_ticks: u64,
}

fn read_metrics_sample_cache(path: &Path) -> MetricsSampleCache {
    let Ok(bytes) = std::fs::read(path) else {
        return MetricsSampleCache::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// The cached pane-root bindings the pane producer can use as a pid-reuse
/// guard when the mux source drops a pane. Entries without a root pid or
/// recorded start ticks are omitted rather than guessed.
pub(super) fn pane_root_bindings(
    runtime: &crate::RuntimePaths,
) -> HashMap<PaneId, PaneRootBinding> {
    let prior = read_metrics_sample_cache(&runtime.root.join("metrics-sample.json"));
    prior
        .entries
        .into_iter()
        .filter_map(|(pane_id, entry)| {
            let pane_id = PaneId::parse(&pane_id).ok()?;
            Some((
                pane_id,
                PaneRootBinding {
                    pid: entry.pane_pid?,
                    start_ticks: entry.root_start_ticks?,
                },
            ))
        })
        .collect()
}

/// Whether any pane in `frame` needs a fresh `/proc` sample. Used by the pane
/// cache fast path so metrics can refresh from a topology-fresh frame without
/// paying another mux `list-panes`.
pub(super) fn pane_metrics_due(frame: &PaneFrame, runtime: &crate::RuntimePaths) -> bool {
    let prior = read_metrics_sample_cache(&runtime.root.join("metrics-sample.json"));
    let now_ms = unix_now_ms();
    frame.pane_states().any(|pane| {
        pane_sampleable(pane)
            && metric_entry_due(
                prior.entries.get(&pane.pane_id.to_string()),
                &pane.current.command,
                now_ms,
            )
    })
}

/// Enrich each pane with per-process resource metrics from `/proc`, on the
/// sampling cadence's own clock: active/recently-changed panes sample on
/// [`METRICS_HOT_SAMPLE_TTL`], idle panes on [`METRICS_SAMPLE_TTL`]. Fresh
/// entries carry their stored display values — and the pane→root-pid binding
/// the process-row name anchors on — forward with zero `/proc` IO. Due entries
/// read `/proc` to compute two-sample rates (CPU%, IO bytes/s) and write a
/// fresh stamped sample for the next window. Linux-only; on other platforms
/// every pane's metric fields stay `None`.
///
/// The steady-state due sample is O(due panes) small `/proc` reads: each Zellij
/// pane's root pid restores from the prior window's guarded binding
/// ([`restore_cached_bindings`]) and each shell's foreground child comes from
/// its own `/proc/<pid>/task/<pid>/children` file. The full process-table walk
/// runs only while some due pane's binding is unknown — pane churn or a
/// foreground change, exactly the moments a fresh `list-panes` was already
/// paid for.
pub(super) fn enrich_pane_metrics(
    frame: &mut PaneFrame,
    session_name: &str,
    runtime: &crate::RuntimePaths,
) -> bool {
    let cache_path = runtime.root.join("metrics-sample.json");
    let prior = read_metrics_sample_cache(&cache_path);
    let now_ms = unix_now_ms();

    let mut due = HashSet::new();
    for pane in frame.pane_states_mut() {
        let pane_key = pane.pane_id.to_string();
        let prior_entry = prior.entries.get(&pane_key);
        if !pane_sampleable(pane) {
            continue;
        }
        if metric_entry_due(prior_entry, &pane.current.command, now_ms) {
            due.insert(pane_key);
        } else if let Some(entry) = prior_entry {
            apply_cached_entry(pane, entry);
        }
    }

    if due.is_empty() {
        return false;
    }

    // Zellij's `list-panes` reports no per-pane pid (tmux fills `#{pane_pid}`
    // natively), so first restore each due pidless pane's root pid from the
    // prior tick's binding — starttime-guarded, one stat read per pane instead
    // of the table walk below.
    let needs_walk = restore_cached_bindings(frame, &prior, &due, &|pid| {
        crate::proc::stat_metrics(pid).map(|stat| stat.start_ticks)
    });

    // The walk's ppid→children map also serves the shell→single-child descent;
    // a walk-free tick reads each shell's direct children file instead.
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if needs_walk {
        children = backfill_zellij_pane_pids_from_proc(frame, session_name);
    }

    let clk_tck = crate::proc::clk_tck() as f64;
    let mut new_entries: HashMap<String, MetricsSampleEntry> = HashMap::new();

    for pane in frame.pane_states_mut() {
        let pane_key = pane.pane_id.to_string();
        if !due.contains(&pane_key) {
            if let Some(entry) = prior.entries.get(&pane_key) {
                new_entries.insert(pane_key, entry.clone());
            }
            continue;
        }
        let entry = sample_due_pane(
            pane, &prior, &pane_key, &children, needs_walk, clk_tck, now_ms,
        );
        new_entries.insert(pane_key, entry);
    }

    // Every due key names a frame pane, so a non-empty `due` always sampled
    // at least one entry: stamp the cache and report the change.
    let new_cache = MetricsSampleCache {
        sampled_at_ms: now_ms,
        entries: new_entries,
    };
    if let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &new_cache) {
        tracing::warn!(error = %err, "metrics sample cache write failed");
    }
    true
}

fn sample_due_pane(
    pane: &mut PaneState,
    prior: &MetricsSampleCache,
    pane_key: &str,
    children: &HashMap<u32, Vec<u32>>,
    needs_walk: bool,
    clk_tck: f64,
    now_ms: u64,
) -> MetricsSampleEntry {
    pane.metrics = PaneMetrics::default();
    let Some(shell_pid) = pane.current.pid else {
        return unbound_entry(pane.current.command.clone(), now_ms);
    };
    let kids = match children.get(&shell_pid) {
        Some(kids) => kids.clone(),
        None if !needs_walk => crate::proc::children(shell_pid),
        None => Vec::new(),
    };
    pane.children = kids.clone();
    let stats_pid = match kids.as_slice() {
        &[child] => child,
        _ => shell_pid,
    };

    let stat_now = crate::proc::stat_metrics(stats_pid);
    let rss_kb = stat_now.map(|stat| stat.rss_kb);
    let cpu_now = stat_now.map(|stat| stat.cpu_ticks);
    let state_char = stat_now.map(|stat| stat.state);
    let io_now = crate::proc::io_bytes(stats_pid);
    let (prior_state_char, cpu_pct, io_bps) = rate_metrics(
        prior.entries.get(pane_key),
        pane,
        stats_pid,
        cpu_now,
        io_now,
        clk_tck,
        now_ms,
    );
    let process_state =
        process_state_from_stat(state_char, prior_state_char).filter(ProcessState::is_stuck);
    if display_metrics_ready(cpu_pct, io_bps, rss_kb) {
        pane.metrics.rss_kb = rss_kb;
        pane.metrics.cpu_pct = cpu_pct;
        pane.metrics.io_bps = io_bps;
    }
    pane.metrics.process_state = process_state;

    let root_start_ticks = if stats_pid == shell_pid {
        stat_now.map(|stat| stat.start_ticks)
    } else {
        crate::proc::stat_metrics(shell_pid).map(|stat| stat.start_ticks)
    };
    MetricsSampleEntry {
        stats_pid,
        cpu_ticks: cpu_now.unwrap_or(0),
        io_bytes: io_now.unwrap_or(0),
        sampled_at_ms: now_ms,
        pane_pid: Some(shell_pid),
        root_start_ticks,
        command: pane.current.command.clone(),
        cpu_pct: pane.metrics.cpu_pct,
        io_bps: pane.metrics.io_bps,
        rss_kb: pane.metrics.rss_kb,
        state_char,
        process_state,
    }
}

fn rate_metrics(
    prior_entry: Option<&MetricsSampleEntry>,
    pane: &PaneState,
    stats_pid: u32,
    cpu_now: Option<u64>,
    io_now: Option<u64>,
    clk_tck: f64,
    now_ms: u64,
) -> (Option<char>, Option<u16>, Option<u64>) {
    let Some(prior_entry) = prior_entry else {
        return (None, None, None);
    };
    if prior_entry.command != pane.current.command || prior_entry.stats_pid != stats_pid {
        return (prior_entry.state_char, None, None);
    }
    let elapsed_ms = now_ms.saturating_sub(prior_entry.sampled_at_ms);
    let elapsed_secs = elapsed_ms as f64 / 1_000.0;
    if elapsed_secs < 0.1 {
        return (prior_entry.state_char, None, None);
    }
    let cpu_pct = cpu_now.map(|ticks| {
        let delta = ticks.saturating_sub(prior_entry.cpu_ticks);
        let pct = (delta as f64 / elapsed_secs / clk_tck * 100.0).round();
        pct.clamp(0.0, u16::MAX as f64) as u16
    });
    let io_bps = io_now.map(|bytes| {
        let delta = bytes.saturating_sub(prior_entry.io_bytes);
        (delta as f64 / elapsed_secs) as u64
    });
    (prior_entry.state_char, cpu_pct, io_bps)
}

/// The entry recorded for a due pidless pane the walk could not match: no
/// binding and no counters (`stats_pid` 0), just the sample-time command and
/// stamp, so the retry rides the hot/idle cadence instead of re-walking the
/// process table every produce.
fn unbound_entry(command: Option<String>, sampled_at_ms: u64) -> MetricsSampleEntry {
    MetricsSampleEntry {
        sampled_at_ms,
        command,
        ..MetricsSampleEntry::default()
    }
}

/// Whether the metrics cadence tracks this pane at all: the sidebar's own
/// chrome stays out (both backends name it through the shared chrome
/// predicate), and a pane with neither command nor pid offers nothing to
/// sample or match.
fn pane_sampleable(pane: &PaneState) -> bool {
    match pane.current.command.as_deref() {
        Some(command) => !crate::ledger::snapshot::command_is_sidebar_chrome(command),
        None => pane.current.pid.is_some(),
    }
}

/// Whether a pane needs a fresh `/proc` sample this produce: immediately when
/// it has no entry or its foreground command changed (the warmup sample for a
/// new tenant), otherwise on the hot or idle cadence the entry's shape picks.
/// Saturating, so a clock that ran backwards reads fresh rather than
/// re-sampling every tick.
fn metric_entry_due(
    entry: Option<&MetricsSampleEntry>,
    command: &Option<String>,
    now_ms: u64,
) -> bool {
    let Some(entry) = entry else {
        return true;
    };
    if entry.command != *command {
        return true;
    }
    let ttl = if metrics_entry_hot(entry) {
        METRICS_HOT_SAMPLE_TTL
    } else {
        METRICS_SAMPLE_TTL
    };
    now_ms.saturating_sub(entry.sampled_at_ms) > ttl.as_millis() as u64
}

/// Whether an entry rides the hot cadence: its sample-time command reads as
/// active work, or its stats rode a foreground child of the pane root (the
/// shell has a tenant). [`metric_entry_due`] consults this only behind its
/// command guard, so the entry's command is also the pane's current one.
fn metrics_entry_hot(entry: &MetricsSampleEntry) -> bool {
    entry
        .command
        .as_deref()
        .is_some_and(crate::ledger::snapshot::process_is_active)
        || entry
            .pane_pid
            .is_some_and(|pane_pid| entry.stats_pid != pane_pid)
}

/// The all-or-nothing display gate: CPU, memory, and IO reach the pane
/// together or not at all, so the rendered cluster reads whole from its first
/// appearance (rates need a second same-tenant sample; RSS alone never shows).
fn display_metrics_ready(cpu_pct: Option<u16>, io_bps: Option<u64>, rss_kb: Option<u64>) -> bool {
    cpu_pct.is_some() && io_bps.is_some() && rss_kb.is_some()
}

/// Carry a fresh entry's stored values onto its pane: the root-pid binding,
/// the display figures (complete sets only), and the stuck verdict — the
/// zero-`/proc`-IO arm of the cadence.
fn apply_cached_entry(pane: &mut PaneState, entry: &MetricsSampleEntry) {
    // The root-pid binding rides with the values: the reducer anchors an active
    // process row's name on the root's comm, so a pidless (Zellij) pane left
    // unbound here would flip its label between shell and program across windows.
    if pane.current.pid.is_none() {
        pane.current.pid = entry.pane_pid;
    }
    pane.metrics = PaneMetrics::default();
    if display_metrics_ready(entry.cpu_pct, entry.io_bps, entry.rss_kb) {
        pane.metrics.cpu_pct = entry.cpu_pct;
        pane.metrics.io_bps = entry.io_bps;
        pane.metrics.rss_kb = entry.rss_kb;
    }
    pane.metrics.process_state = entry.process_state;
}

/// Restore the cached pane→root-pid bindings for pidless (Zellij) panes due a
/// fresh sample, and report whether any of them still needs the full `/proc`
/// table walk. A pane hits when its cached entry carries a binding and the
/// root pid is alive with the same `starttime` ticks (the pid-reuse guard) —
/// `read_start_ticks` is injected so the guard unit-tests over fixtures.
/// Panes outside `due` never trigger the walk: a fresh entry's binding already
/// rode [`apply_cached_entry`], and an unbound pane between samples retries on
/// its own cadence ([`unbound_entry`]) instead of dragging the walk onto every
/// produce. `due` holds only sampleable panes, so the panes the walk could
/// never bind — no command to match, the sidebar's own chrome — sit outside it
/// by construction ([`pane_sampleable`]). Steady state — stable panes with
/// live root pids — restores every binding and walks nothing.
fn restore_cached_bindings(
    frame: &mut PaneFrame,
    prior: &MetricsSampleCache,
    due: &HashSet<String>,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
) -> bool {
    let mut needs_walk = false;
    for pane in frame.pane_states_mut() {
        if pane.current.pid.is_some() || !due.contains(&pane.pane_id.to_string()) {
            continue;
        }
        match prior
            .entries
            .get(&pane.pane_id.to_string())
            .and_then(|entry| cached_root_pid(entry, read_start_ticks))
        {
            Some(pid) => pane.current.pid = Some(pid),
            None => needs_walk = true,
        }
    }
    needs_walk
}

/// The still-valid root pid a cache entry binds, or `None` when the binding
/// must be re-derived through the table walk: no binding recorded (an old
/// cache shape), the pid is gone, or the pid was recycled (`starttime`
/// mismatch).
fn cached_root_pid(
    entry: &MetricsSampleEntry,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
) -> Option<u32> {
    let pid = entry.pane_pid?;
    let recorded = entry.root_start_ticks?;
    (read_start_ticks(pid) == Some(recorded)).then_some(pid)
}

#[cfg(test)]
mod tests;
