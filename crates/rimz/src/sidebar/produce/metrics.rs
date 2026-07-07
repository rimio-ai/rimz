//! Per-pane `/proc` resource metrics on the sampling cadence's own clock: the
//! two-sample pane-tree CPU/IO rates, the persisted pane→root-pid bindings with
//! their starttime pid-reuse guard, and the Zellij pid backfill walk.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::ProcessState;
use crate::ids::PaneId;
use crate::sidebar::frame::{PaneFrame, PaneMetrics, PaneState};
use crate::sidebar::timing::unix_now_ms;
use crate::sidebar::timing::{METRICS_BACKGROUND_SAMPLE_TTL, METRICS_FOCUSED_SAMPLE_TTL};
use crate::store::atomic;

mod zellij;

#[cfg(test)]
use zellij::backfill_zellij_pane_pids;
pub(super) use zellij::backfill_zellij_pane_pids_from_proc;

const METRICS_SAMPLE_VERSION: u8 = 2;

/// Per-pane CPU and IO tick counters sampled by the producer on the previous
/// tick, plus the pane's root-pid binding. Two consecutive readings plus the
/// elapsed wall time give rates; the binding lets the next tick restore a
/// Zellij pane's root pid for one guarded stat read instead of the full
/// `/proc` table walk that re-deriving it costs.
#[derive(serde::Serialize, serde::Deserialize, Clone, Default)]
struct MetricsSampleEntry {
    /// Cache shape for the sampled counters. Version 1 sampled one process
    /// (`stats_pid`) and cannot seed pane-tree rates, so version mismatches
    /// force one warmup sample while still allowing guarded root-pid restore.
    #[serde(default)]
    sample_version: u8,
    /// The pane root pid whose tree was sampled. `0` records an unbound sample
    /// attempt, so pidless panes that cannot be matched still respect the
    /// hot/idle retry cadence.
    stats_pid: u32,
    /// Aggregated pane-tree CPU ticks at sample time. Each live process
    /// contributes self CPU plus waited-child CPU.
    cpu_ticks: u64,
    /// Aggregated pane-tree rchar + wchar bytes at sample time.
    io_bytes: u64,
    /// Whether `io_bytes` came from a complete tree I/O sample. A missing
    /// `/proc/<pid>/io` read keeps the display hidden and cannot seed the next
    /// rate calculation from zero.
    #[serde(default)]
    io_bytes_valid: bool,
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
    /// Last stat-readable process states in the tree, keyed by pid and
    /// starttime so repeated `D` detection never aliases a reused pid.
    #[serde(default)]
    state_samples: Vec<ProcessStateSample>,
    /// Last stuck verdict, carried across fresh windows without re-reading
    /// `/proc`.
    #[serde(default)]
    process_state: Option<ProcessState>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessStateSample {
    pid: u32,
    start_ticks: u64,
    state: char,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PaneTreeSample {
    direct_children: Vec<u32>,
    process_count: u32,
    cpu_ticks: u64,
    io_bytes: Option<u64>,
    rss_kb: u64,
    root_start_ticks: u64,
    state_samples: Vec<ProcessStateSample>,
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
/// paying another mux roster read.
pub(super) fn pane_metrics_due(frame: &PaneFrame, runtime: &crate::RuntimePaths) -> bool {
    let prior = read_metrics_sample_cache(&runtime.root.join("metrics-sample.json"));
    let now_ms = unix_now_ms();
    let viewed: HashSet<&PaneId> = frame.viewed_panes.iter().collect();
    frame.pane_states().any(|pane| {
        pane_sampleable(pane)
            && metric_entry_due(
                prior.entries.get(&pane.pane_id.to_string()),
                &pane.current.command,
                now_ms,
                viewed.contains(&pane.pane_id),
            )
    })
}

/// Enrich each pane with process-tree resource metrics from `/proc`, on the
/// sampling cadence's own clock: viewed panes sample on
/// [`METRICS_FOCUSED_SAMPLE_TTL`], background panes on
/// [`METRICS_BACKGROUND_SAMPLE_TTL`]. Fresh
/// entries carry their stored display values — and the pane→root-pid binding
/// the process-row name anchors on — forward with zero `/proc` IO. Due entries
/// read `/proc` to compute two-sample rates (CPU%, IO bytes/s) and write a
/// fresh stamped sample for the next window. Linux-only; on other platforms
/// every pane's metric fields stay `None`.
///
/// The steady-state due sample is O(due pane trees) small `/proc` reads: each
/// Zellij pane's root pid restores from the prior window's guarded binding
/// ([`restore_cached_bindings`]), then descendants come from the full
/// process-table child map when it was already paid for, or from each sampled
/// process's per-task `/proc/<pid>/task/<tid>/children` files. The full process-table
/// walk runs only while some due pane's binding is unknown — pane churn or a
/// foreground change, exactly the moments a fresh roster was already paid for.
pub(super) fn enrich_pane_metrics(
    frame: &mut PaneFrame,
    session_name: &str,
    runtime: &crate::RuntimePaths,
) -> bool {
    let cache_path = runtime.root.join("metrics-sample.json");
    let prior = read_metrics_sample_cache(&cache_path);
    let now_ms = unix_now_ms();
    let viewed: HashSet<PaneId> = frame.viewed_panes.iter().cloned().collect();

    let mut due = HashSet::new();
    for pane in frame.pane_states_mut() {
        let pane_key = pane.pane_id.to_string();
        let prior_entry = prior.entries.get(&pane_key);
        if !pane_sampleable(pane) {
            continue;
        }
        if metric_entry_due(
            prior_entry,
            &pane.current.command,
            now_ms,
            viewed.contains(&pane.pane_id),
        ) {
            due.insert(pane_key);
        } else if let Some(entry) = prior_entry {
            apply_cached_entry(pane, entry);
        }
    }

    if due.is_empty() {
        return false;
    }

    // Zellij topology reports no per-pane pid (tmux fills `#{pane_pid}`
    // natively), so first restore each due pidless pane's root pid from the
    // prior tick's binding — starttime-guarded, one stat read per pane instead
    // of the table walk below.
    let needs_walk = restore_cached_bindings(frame, &prior, &due, &|pid| {
        crate::proc::stat_metrics(pid).map(|stat| stat.start_ticks)
    });

    // The walk's ppid→children map also serves the shell→tree descent;
    // a walk-free tick reads each process's per-task children files instead.
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
    let Some(sample) = sample_pane_tree(
        shell_pid,
        children,
        needs_walk,
        &crate::proc::stat_metrics,
        &crate::proc::io_bytes,
        &crate::proc::children,
    ) else {
        return MetricsSampleEntry {
            sample_version: METRICS_SAMPLE_VERSION,
            stats_pid: shell_pid,
            sampled_at_ms: now_ms,
            pane_pid: Some(shell_pid),
            command: pane.current.command.clone(),
            ..MetricsSampleEntry::default()
        };
    };
    pane.children = sample.direct_children.clone();
    let (cpu_pct, io_bps) =
        rate_metrics(prior.entries.get(pane_key), pane, &sample, clk_tck, now_ms);
    let prior_states = prior
        .entries
        .get(pane_key)
        .filter(|entry| entry.sample_version == METRICS_SAMPLE_VERSION)
        .map(|entry| entry.state_samples.as_slice())
        .unwrap_or_default();
    let process_state = process_state_from_tree(&sample.state_samples, prior_states);
    if display_metrics_ready(cpu_pct, io_bps, Some(sample.rss_kb)) {
        pane.metrics.rss_kb = Some(sample.rss_kb);
        pane.metrics.cpu_pct = cpu_pct;
        pane.metrics.io_bps = io_bps;
    }
    pane.metrics.process_state = process_state;

    MetricsSampleEntry {
        sample_version: METRICS_SAMPLE_VERSION,
        stats_pid: shell_pid,
        cpu_ticks: sample.cpu_ticks,
        io_bytes: sample.io_bytes.unwrap_or(0),
        io_bytes_valid: sample.io_bytes.is_some(),
        sampled_at_ms: now_ms,
        pane_pid: Some(shell_pid),
        root_start_ticks: Some(sample.root_start_ticks),
        command: pane.current.command.clone(),
        cpu_pct: pane.metrics.cpu_pct,
        io_bps: pane.metrics.io_bps,
        rss_kb: pane.metrics.rss_kb,
        state_samples: sample.state_samples,
        process_state,
    }
}

fn sample_pane_tree(
    root_pid: u32,
    children: &HashMap<u32, Vec<u32>>,
    needs_walk: bool,
    stat_metrics: &dyn Fn(u32) -> Option<crate::proc::StatMetrics>,
    io_bytes: &dyn Fn(u32) -> Option<u64>,
    proc_children: &dyn Fn(u32) -> Vec<u32>,
) -> Option<PaneTreeSample> {
    let root_stat = stat_metrics(root_pid)?;
    let root_children = direct_children(root_pid, children, needs_walk, proc_children);
    let mut sample = PaneTreeSample {
        direct_children: root_children.clone(),
        process_count: 0,
        cpu_ticks: 0,
        io_bytes: Some(0),
        rss_kb: 0,
        root_start_ticks: root_stat.start_ticks,
        state_samples: Vec::new(),
    };
    add_process_to_sample(root_pid, root_stat, io_bytes, &mut sample);

    let mut seen = HashSet::from([root_pid]);
    let mut stack = root_children;
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        let Some(stat) = stat_metrics(pid) else {
            continue;
        };
        add_process_to_sample(pid, stat, io_bytes, &mut sample);
        stack.extend(direct_children(pid, children, needs_walk, proc_children));
    }
    Some(sample)
}

fn direct_children(
    pid: u32,
    children: &HashMap<u32, Vec<u32>>,
    needs_walk: bool,
    proc_children: &dyn Fn(u32) -> Vec<u32>,
) -> Vec<u32> {
    match children.get(&pid) {
        Some(kids) => kids.clone(),
        None if !needs_walk => proc_children(pid),
        None => Vec::new(),
    }
}

fn add_process_to_sample(
    pid: u32,
    stat: crate::proc::StatMetrics,
    io_bytes: &dyn Fn(u32) -> Option<u64>,
    sample: &mut PaneTreeSample,
) {
    sample.process_count = sample.process_count.saturating_add(1);
    sample.cpu_ticks = sample
        .cpu_ticks
        .saturating_add(stat.cpu_ticks)
        .saturating_add(stat.child_cpu_ticks);
    sample.rss_kb = sample.rss_kb.saturating_add(stat.rss_kb);
    sample.io_bytes = match (sample.io_bytes, io_bytes(pid)) {
        (Some(total), Some(bytes)) => Some(total.saturating_add(bytes)),
        _ => None,
    };
    sample.state_samples.push(ProcessStateSample {
        pid,
        start_ticks: stat.start_ticks,
        state: stat.state,
    });
}

fn process_state_from_tree(
    current: &[ProcessStateSample],
    prior: &[ProcessStateSample],
) -> Option<ProcessState> {
    if current.iter().any(|sample| sample.state == 'Z') {
        return Some(ProcessState::Stuck);
    }
    let prior_d: HashSet<(u32, u64)> = prior
        .iter()
        .filter(|sample| sample.state == 'D')
        .map(|sample| (sample.pid, sample.start_ticks))
        .collect();
    current
        .iter()
        .any(|sample| sample.state == 'D' && prior_d.contains(&(sample.pid, sample.start_ticks)))
        .then_some(ProcessState::Stuck)
}

fn rate_metrics(
    prior_entry: Option<&MetricsSampleEntry>,
    pane: &PaneState,
    sample: &PaneTreeSample,
    clk_tck: f64,
    now_ms: u64,
) -> (Option<u16>, Option<u64>) {
    let Some(prior_entry) = prior_entry else {
        return (None, None);
    };
    if prior_entry.sample_version != METRICS_SAMPLE_VERSION
        || prior_entry.command != pane.current.command
        || prior_entry.pane_pid != pane.current.pid
        || prior_entry.root_start_ticks != Some(sample.root_start_ticks)
    {
        return (None, None);
    }
    let elapsed_ms = now_ms.saturating_sub(prior_entry.sampled_at_ms);
    let elapsed_secs = elapsed_ms as f64 / 1_000.0;
    if elapsed_secs < 0.1 {
        return (None, None);
    }
    let delta = sample.cpu_ticks.saturating_sub(prior_entry.cpu_ticks);
    let pct = (delta as f64 / elapsed_secs / clk_tck * 100.0).round();
    let cpu_pct = Some(pct.clamp(0.0, u16::MAX as f64) as u16);
    let io_bps = if prior_entry.io_bytes_valid {
        sample.io_bytes.map(|bytes| {
            let delta = bytes.saturating_sub(prior_entry.io_bytes);
            (delta as f64 / elapsed_secs) as u64
        })
    } else {
        None
    };
    (cpu_pct, io_bps)
}

/// The entry recorded for a due pidless pane the walk could not match: no
/// binding and no counters (`stats_pid` 0), just the sample-time command and
/// stamp, so the retry rides the hot/idle cadence instead of re-walking the
/// process table every produce.
fn unbound_entry(command: Option<String>, sampled_at_ms: u64) -> MetricsSampleEntry {
    MetricsSampleEntry {
        sample_version: METRICS_SAMPLE_VERSION,
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
        Some(command) => !crate::store::snapshot::command_is_sidebar_chrome(command),
        None => pane.current.pid.is_some(),
    }
}

/// Whether a pane needs a fresh `/proc` sample this produce: immediately when
/// it has no entry or its foreground command changed (the warmup sample for a
/// new tenant), otherwise on the viewed or background cadence.
/// Saturating, so a clock that ran backwards reads fresh rather than
/// re-sampling every tick.
fn metric_entry_due(
    entry: Option<&MetricsSampleEntry>,
    command: &Option<String>,
    now_ms: u64,
    is_viewed: bool,
) -> bool {
    let Some(entry) = entry else {
        return true;
    };
    if entry.sample_version != METRICS_SAMPLE_VERSION {
        return true;
    }
    if entry.command != *command {
        return true;
    }
    let ttl = if is_viewed {
        METRICS_FOCUSED_SAMPLE_TTL
    } else {
        METRICS_BACKGROUND_SAMPLE_TTL
    };
    now_ms.saturating_sub(entry.sampled_at_ms) > ttl.as_millis() as u64
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
