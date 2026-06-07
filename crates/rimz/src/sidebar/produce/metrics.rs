//! Per-pane `/proc` resource metrics on the sampling cadence's own clock: the
//! two-sample CPU/IO rates, the persisted pane→root-pid bindings with their
//! starttime pid-reuse guard, and the Zellij pid backfill walk.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ProcessState;
use crate::ledger::atomic;
use crate::sidebar::cache::unix_now_ms;
use crate::sidebar::frame::PaneFrame;
use crate::sidebar::timing::METRICS_SAMPLE_TTL;

/// Per-pane CPU and IO tick counters sampled by the producer on the previous
/// tick, plus the pane's root-pid binding. Two consecutive readings plus the
/// elapsed wall time give rates; the binding lets the next tick restore a
/// Zellij pane's root pid for one guarded stat read instead of the full
/// `/proc` table walk that re-deriving it costs.
#[derive(serde::Serialize, serde::Deserialize)]
struct MetricsSampleEntry {
    /// The PID the metrics were read from (shell or its single foreground child).
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
    /// Unix ms of the last full `/proc` sample, for the cadence gate.
    /// serde-default 0, so a pre-stamp cache reads as due and the first
    /// produce after an upgrade samples and re-stamps.
    #[serde(default)]
    sampled_at_ms: u64,
    entries: HashMap<String, MetricsSampleEntry>,
}

impl MetricsSampleCache {
    /// Whether the last sample is young enough that this produce skips `/proc`
    /// and carries the stored display values forward. Saturating, so a clock
    /// that ran backwards reads fresh rather than re-sampling every tick.
    fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.sampled_at_ms) <= METRICS_SAMPLE_TTL.as_millis() as u64
    }
}

fn read_metrics_sample_cache(path: &Path) -> MetricsSampleCache {
    let Ok(bytes) = std::fs::read(path) else {
        return MetricsSampleCache::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Enrich each pane with per-process resource metrics from `/proc`, on the
/// sampling cadence's own clock: within [`METRICS_SAMPLE_TTL`] of the last
/// sample the stored display values — and the pane→root-pid binding the
/// process-row name anchors on — carry forward with zero `/proc` IO; a due
/// produce reads the prior sample cache to compute two-sample rates (CPU%, IO
/// bytes/s) and writes a fresh stamped sample for the next window. Linux-only;
/// on other platforms every pane's metric fields stay `None`.
///
/// The steady-state due sample is O(panes) small `/proc` reads: each Zellij
/// pane's root pid restores from the prior window's guarded binding
/// ([`restore_cached_bindings`]) and each shell's foreground child comes from
/// its own `/proc/<pid>/task/<pid>/children` file. The full process-table walk
/// runs only while some pane's binding is unknown — pane churn or a foreground
/// change, exactly the moments a fresh `list-panes` was already paid for.
pub(super) fn enrich_pane_metrics(
    frame: &mut PaneFrame,
    session_name: &str,
    runtime: &crate::RuntimePaths,
) {
    let cache_path = runtime.root.join("metrics-sample.json");
    let prior = read_metrics_sample_cache(&cache_path);
    let now_ms = unix_now_ms();

    // Within the sampling window: carry the stored display values forward onto
    // the matching panes and return — zero `/proc` IO (no stat-validated
    // binding restore, no table walk, no stat/io reads) and no cache write.
    // The carry is keyed by pane id and guarded by the sample-time command:
    // a changed foreground means the values belong to the prior tenant, so
    // they stay off the fresh process. A pane absent from the cache keeps
    // `None`s — the same warmup it has today, one window wider.
    if prior.is_fresh(now_ms) {
        for pane in frame.pane_states_mut() {
            let Some(entry) = prior.entries.get(&pane.pane_id.to_string()) else {
                continue;
            };
            if entry.command != pane.current.command {
                continue;
            }
            // The root-pid binding rides with the values: the reducer anchors
            // an active process row's name on the root's comm, so a pidless
            // (Zellij) pane left unbound here would flip its label between
            // shell and program across windows.
            if pane.current.pid.is_none() {
                pane.current.pid = entry.pane_pid;
            }
            pane.metrics.cpu_pct = entry.cpu_pct;
            pane.metrics.io_bps = entry.io_bps;
            pane.metrics.rss_kb = entry.rss_kb;
            pane.metrics.process_state = entry.process_state;
        }
        return;
    }

    // Zellij's `list-panes` reports no per-pane pid (tmux fills `#{pane_pid}`
    // natively), so first restore each pidless pane's root pid from the prior
    // tick's binding — starttime-guarded, one stat read per pane instead of
    // the table walk below.
    let needs_walk = restore_cached_bindings(frame, &prior, &|pid| {
        crate::proc::stat_metrics(pid).map(|stat| stat.start_ticks)
    });

    // The walk's ppid→children map also serves the shell→single-child descent;
    // a walk-free tick reads each shell's direct children file instead.
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if needs_walk {
        let all_procs = crate::proc::list_processes();
        for p in &all_procs {
            children.entry(p.ppid).or_default().push(p.pid);
        }
        backfill_zellij_pane_pids(
            frame,
            &all_procs,
            &children,
            session_name,
            crate::proc::own_uid(),
            &|pid| crate::proc::cwd(pid),
        );
    }

    let clk_tck = crate::proc::clk_tck() as f64;
    let mut new_entries: HashMap<String, MetricsSampleEntry> = HashMap::new();

    for pane in frame.pane_states_mut() {
        let Some(shell_pid) = pane.current.pid else {
            continue;
        };
        // If the shell has exactly one child, its stats are more informative
        // than the shell's own (which idles while the child runs). Fall back to
        // the shell when there are zero or multiple children.
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

        // One `stat` read serves both CPU ticks and RSS (the separate `status`
        // read for VmRSS was a third file open per pane for one display figure).
        let stat_now = crate::proc::stat_metrics(stats_pid);
        pane.metrics.rss_kb = stat_now.map(|stat| stat.rss_kb);
        let cpu_now = stat_now.map(|stat| stat.cpu_ticks);
        let state_char = stat_now.map(|stat| stat.state);
        let io_now = crate::proc::io_bytes(stats_pid);

        let pane_key = pane.pane_id.to_string();
        let mut prior_state_char = None;
        if let Some(prior_entry) = prior.entries.get(&pane_key) {
            prior_state_char = prior_entry.state_char;
            // Only compute a rate when the stats PID hasn't changed across ticks
            // and the elapsed time is non-trivial (a very short gap yields noise).
            if prior_entry.stats_pid == stats_pid {
                let elapsed_ms = now_ms.saturating_sub(prior_entry.sampled_at_ms);
                let elapsed_secs = elapsed_ms as f64 / 1_000.0;
                if elapsed_secs >= 0.1 {
                    if let Some(ticks) = cpu_now {
                        let delta = ticks.saturating_sub(prior_entry.cpu_ticks);
                        let pct = (delta as f64 / elapsed_secs / clk_tck * 100.0).round();
                        pane.metrics.cpu_pct = Some(pct.clamp(0.0, u16::MAX as f64) as u16);
                    }
                    if let Some(bytes) = io_now {
                        let delta = bytes.saturating_sub(prior_entry.io_bytes);
                        pane.metrics.io_bps = Some((delta as f64 / elapsed_secs) as u64);
                    }
                }
            }
        }
        pane.metrics.process_state =
            process_state_from_stat(state_char, prior_state_char).filter(ProcessState::is_stuck);

        // The root binding recorded for the next tick's restore: the shell's
        // own stat read covers it when it is also the stats pid; an active
        // child costs one extra small read.
        let root_start_ticks = if stats_pid == shell_pid {
            stat_now.map(|stat| stat.start_ticks)
        } else {
            crate::proc::stat_metrics(shell_pid).map(|stat| stat.start_ticks)
        };
        new_entries.insert(
            pane_key,
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
                process_state: pane.metrics.process_state,
            },
        );
    }

    let new_cache = MetricsSampleCache {
        sampled_at_ms: now_ms,
        entries: new_entries,
    };
    if let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &new_cache) {
        tracing::warn!(error = %err, "metrics sample cache write failed");
    }
}

fn process_state_from_stat(current: Option<char>, prior: Option<char>) -> Option<ProcessState> {
    match current {
        Some('Z') => Some(ProcessState::Stuck),
        Some('D') if prior == Some('D') => Some(ProcessState::Stuck),
        Some(_) => None,
        None => None,
    }
}

/// Restore the cached pane→root-pid bindings for pidless (Zellij) panes, and
/// report whether any pane still needs the full `/proc` table walk. A pane
/// hits when its cached entry carries a binding and the root pid is alive with
/// the same `starttime` ticks (the pid-reuse guard) — `read_start_ticks` is
/// injected so the guard unit-tests over fixtures. Steady state — stable panes
/// with live root pids — restores every binding and walks nothing.
fn restore_cached_bindings(
    frame: &mut PaneFrame,
    prior: &MetricsSampleCache,
    read_start_ticks: &dyn Fn(u32) -> Option<u64>,
) -> bool {
    let mut needs_walk = false;
    for pane in frame.pane_states_mut() {
        if pane.current.pid.is_some() {
            continue;
        }
        // The walk could not bind these either — no command to match, or the
        // sidebar's own chrome — so a miss on them never triggers it.
        let Some(command) = pane.current.command.as_deref() else {
            continue;
        };
        if command == crate::mux::zellij::SIDEBAR_PANE_NAME {
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

/// Backfill `pane_pid` for panes whose backend reported none (Zellij emits no
/// pid field; tmux fills `#{pane_pid}` natively), resolving each pane to its
/// root process — the direct child of the session's `zellij --server <socket>`
/// process — so the field carries tmux's semantics on both backends and the
/// shell→single-child descent above behaves identically.
///
/// Zellij reports a pane's *foreground* command as that process's `/proc`
/// cmdline (argv joined by spaces — the same form as
/// [`ProcInfo`](crate::proc::ProcInfo)`::cmdline`), so a pane matches the forest
/// process with that exact cmdline, then walks up to the direct server child.
/// The cwd narrow only breaks ties between same-cmdline candidates: a unique
/// match is taken as-is, since a foreground process may legitimately sit in
/// another directory than the pane reports (an agent that chdir'd into its
/// worktree). Pure over its inputs — the caller injects the process table and
/// the `/proc` cwd lookup — so the matcher unit-tests over fixtures.
///
/// Abstention is the failure mode: a pane stays pidless (no stats beats a
/// stranger's stats) when its command matches nothing or stays ambiguous after
/// the narrow — e.g. two idle `zsh` panes in one cwd. An *active* pane's
/// foreground cmdline is almost always unique, so real work still reads.
/// Sidebar chrome panes are skipped outright: every sidebar shares one
/// cmdline, and they are excluded from rows anyway.
fn backfill_zellij_pane_pids(
    frame: &mut PaneFrame,
    procs: &[crate::proc::ProcInfo],
    children: &HashMap<u32, Vec<u32>>,
    session_name: &str,
    own_uid: Option<u32>,
    proc_cwd: &dyn Fn(u32) -> Option<PathBuf>,
) {
    // Nothing to backfill (tmux, or an empty room): skip the server scan.
    if frame.pane_states().all(|pane| pane.current.pid.is_some()) {
        return;
    }
    let Some(server_pid) = zellij_server_pid(procs, session_name, own_uid) else {
        return;
    };
    let forest = descendants(children, server_pid);
    let parent_of: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
    for pane in frame.pane_states_mut() {
        if pane.current.pid.is_some() {
            continue;
        }
        let Some(command) = pane.current.command.as_deref() else {
            continue;
        };
        if command == crate::mux::zellij::SIDEBAR_PANE_NAME {
            continue;
        }
        let candidates: Vec<u32> = procs
            .iter()
            .filter(|p| forest.contains(&p.pid) && p.cmdline == command)
            .map(|p| p.pid)
            .collect();
        let matched = match candidates.as_slice() {
            &[only] => Some(only),
            &[] => None,
            many => {
                let narrowed: Vec<u32> = match pane.current.cwd.as_deref() {
                    Some(cwd) => many
                        .iter()
                        .copied()
                        .filter(|&pid| proc_cwd(pid).as_deref() == Some(Path::new(cwd)))
                        .collect(),
                    None => Vec::new(),
                };
                match narrowed.as_slice() {
                    &[only] => Some(only),
                    _ => None,
                }
            }
        };
        pane.current.pid =
            matched.and_then(|pid| walk_to_server_child(&parent_of, server_pid, pid));
    }
}

/// The pid of the session's Zellij server: the same-uid process whose cmdline
/// is `zellij --server <socket>` with the socket's file name equal to the
/// session name (Zellij names the server socket after the session). The uid
/// gate keeps a same-named session of another user from being walked.
fn zellij_server_pid(
    procs: &[crate::proc::ProcInfo],
    session_name: &str,
    own_uid: Option<u32>,
) -> Option<u32> {
    let own_uid = own_uid?;
    procs
        .iter()
        .find(|p| p.real_uid == own_uid && cmdline_is_session_server(&p.cmdline, session_name))
        .map(|p| p.pid)
}

/// Whether a cmdline runs the Zellij server for `session_name` — exactly
/// `<path>/zellij --server <socket>` with `basename(socket) == session_name`.
fn cmdline_is_session_server(cmdline: &str, session_name: &str) -> bool {
    let mut tokens = cmdline.split_whitespace();
    let file_name = |token: Option<&str>, name: &str| {
        token
            .map(Path::new)
            .and_then(Path::file_name)
            .is_some_and(|file| file == name)
    };
    file_name(tokens.next(), "zellij")
        && tokens.next() == Some("--server")
        && file_name(tokens.next(), session_name)
}

/// Every descendant of `root` in the ppid→children map — the session server's
/// process forest, one tree per pane.
fn descendants(children: &HashMap<u32, Vec<u32>>, root: u32) -> HashSet<u32> {
    let mut out = HashSet::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        for &child in children.get(&pid).map(Vec::as_slice).unwrap_or_default() {
            if out.insert(child) {
                stack.push(child);
            }
        }
    }
    out
}

/// Walk `pid` up its parent chain to the direct child of `server_pid` — the
/// pane root. Terminates by construction for a forest member (its membership
/// proves a parent chain to the server); the `None` arm covers a chain that
/// leaves the table mid-walk, e.g. a process that exited between reads.
fn walk_to_server_child(
    parent_of: &HashMap<u32, u32>,
    server_pid: u32,
    mut pid: u32,
) -> Option<u32> {
    loop {
        match parent_of.get(&pid) {
            Some(&ppid) if ppid == server_pid => return Some(pid),
            Some(&ppid) => pid = ppid,
            None => return None,
        }
    }
}

#[cfg(test)]
mod tests;
