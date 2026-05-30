use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};

use super::{GlobalFlags, open_ledger};
use rimz::ids::{MuxName, WorkspaceId};
use rimz::ledger::atomic;
use rimz::ledger::paths::env_path;
use rimz::ledger::single_flight::{self, Coalesced};
use rimz::ledger::workspace_record;
use rimz::mux::PaneListOptions;
use rimz::workspace::WorkspaceResolver;
use rimz::{Ledger, RuntimePaths, StatePaths};

#[derive(Debug, Args)]
pub struct SidebarArgs {
    #[command(subcommand)]
    command: SidebarSubcmd,
}

#[derive(Debug, Subcommand)]
enum SidebarSubcmd {
    /// Render the current snapshot. The sidebar process reads this.
    Snapshot {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long)]
        mux: Option<MuxName>,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long)]
        exclude_pane_id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Run the terminal sidebar renderer.
    Serve {
        #[arg(long)]
        workspace_id: Option<String>,
        #[arg(long)]
        mux: Option<MuxName>,
        #[arg(long)]
        session_name: Option<String>,
        #[arg(long, default_value_t = 2)]
        tick_seconds: u64,
    },
}

pub fn run(args: SidebarArgs, globals: &GlobalFlags) -> Result<()> {
    match args.command {
        SidebarSubcmd::Snapshot {
            workspace_id,
            mux,
            session_name,
            exclude_pane_id,
            json,
        } => {
            let mut resolved_session = None;
            let ledger = match workspace_id {
                Some(raw) => open_ledger_by_workspace_id(raw.parse()?),
                None => {
                    let workspace = WorkspaceResolver::resolve(".", globals.root.clone())?;
                    resolved_session = Some(workspace.session_name.clone());
                    open_ledger(&workspace)
                }
            }?;
            // The serve loop names its session explicitly; a bare CLI/inspection
            // call resolves it from the record. Only the former treats a
            // pane-discovery failure as fatal (see the match below).
            let explicit_session = session_name.is_some();
            let session_name = session_name
                .or(resolved_session)
                .or_else(|| session_name_from_record(&ledger));
            let exclude = exclude_pane_id
                .as_deref()
                .map(rimz::ids::PaneId::parse)
                .transpose()?;

            // Resolve the ledger rollup and the live pane list. The heavy work
            // (ledger projection + `list-panes`) is identical for every sidebar
            // pinned to one session, so it rides a short-lived single-flight
            // shared cache: the first sidebar to miss produces and writes it,
            // and the rest within the coalescing window read it back instead of
            // each spawning their own `list-panes` against the mux server. Only
            // the cheap per-sidebar projection (own-pane exclusion + own-view)
            // stays local.
            let (mut snapshot, panes): (rimz::SidebarSnapshot, Option<Vec<rimz::feed::PaneRef>>) =
                match (&session_name, pane_list_fixture()?) {
                    // A test fixture stands in for the mux; never touch the shared
                    // cache so deterministic tests can neither poison nor read it.
                    (Some(_), Some(fixture)) => (ledger.snapshot()?, Some(fixture)),
                    (Some(session), None) => {
                        let mux = mux
                            .or(globals.mux)
                            .or_else(|| rimz::mux::auto_detect_backend(None).ok());
                        match mux {
                            Some(mux) => match cached_base_or_produce(&ledger, mux, session) {
                                Ok((rollup, panes)) => (rollup, Some(panes)),
                                // The serve loop owns a live session, so a
                                // discovery failure there is real: fail hard and
                                // let the loop hold its last good frame via the
                                // degraded path, rather than flashing the raw
                                // ledger rollup (every agent the log ever saw).
                                Err(err) if explicit_session => {
                                    return Err(err).context("sidebar snapshot pane discovery");
                                }
                                // A bare inspection call has no live session to
                                // trust; fall back to the ledger rollup.
                                Err(err) => {
                                    tracing::warn!(error = %err, "sidebar snapshot pane discovery failed; showing ledger rollup");
                                    (ledger.snapshot()?, None)
                                }
                            },
                            None => (ledger.snapshot()?, None),
                        }
                    }
                    (None, _) => (ledger.snapshot()?, None),
                };

            // Enumerate the repo's worktrees so a checkout parked outside the
            // project root still earns its own pod instead of folding into
            // `external`. The git probe is cached under the diff-stats TTL and
            // runs on this fetch worker, never the render thread.
            if let Some(root) = snapshot.project_root.clone() {
                let roots = project_worktree_roots(&root, ledger.runtime_paths());
                snapshot = snapshot.with_worktree_roots(roots);
            }

            // Fold each session's rich statusline context onto its agent state
            // (read-only; the feed process is the writer). This enriches the
            // snapshot's `agents[]` for `--json` consumers without changing row
            // rendering. Both the context sidecar and the per-tool activity
            // heartbeats fold only onto existing agents, so an empty room skips
            // both directory scans entirely — the common idle case. Activity
            // lands before the pane overlay so age, ranking, the ask-fold guard,
            // and the stall window all see the truer per-tool value rather than
            // the turn-grained event-log timestamp.
            if !snapshot.agents.is_empty() {
                snapshot = snapshot.with_agent_context(rimz::ledger::agent_context::read_all(
                    ledger.runtime_paths(),
                ));
                let activity = rimz::agent_activity::read_all(ledger.runtime_paths());
                snapshot = snapshot.with_agent_activity(&activity);
            }

            if let Some(panes) = panes {
                if let Some(own) = exclude.as_ref() {
                    snapshot.own_view = rimz::SidebarOwnView::from_panes(own, &panes);
                }
                snapshot = snapshot.with_live_panes(panes, exclude.as_ref());
            }
            // Hook-install state is environment, not ledger, so the reducer
            // can't know it — fill it here so the renderer's first-run hint
            // can point at `rimz hooks install` until a supported agent is
            // wired. Unsupported adapters must not make the room look ready.
            snapshot.agent_hooks_ready = rimz::agents::KNOWN_AGENTS.iter().any(|name| {
                rimz::agents::integration_by_name(name)
                    .map(|agent| agent.supports_hook_install() && agent.hooks_installed())
                    .unwrap_or(false)
            });
            enrich_worktree_groups(&mut snapshot, ledger.runtime_paths());
            if json {
                let rendered = serde_json::to_string_pretty(&snapshot)?;
                #[expect(clippy::print_stdout, reason = "json emitter for sidebar")]
                {
                    println!("{rendered}");
                }
            } else {
                let tally = |status| {
                    snapshot
                        .worktree_groups
                        .iter()
                        .flat_map(|group| &group.status_counts)
                        .filter(|count| count.status == status)
                        .map(|count| count.count)
                        .sum::<usize>()
                };
                let waiting = tally(rimz::feed::AgentStatus::Waiting);
                let failed = tally(rimz::feed::AgentStatus::Failed);
                #[expect(clippy::print_stdout, reason = "human summary")]
                {
                    println!("Workspace:       {}", snapshot.display_name);
                    println!("Worktree groups: {}", snapshot.worktree_groups.len());
                    println!("Waiting:         {waiting}");
                    println!("Failed:          {failed}");
                }
            }
            Ok(())
        }
        SidebarSubcmd::Serve {
            workspace_id,
            mux,
            session_name,
            tick_seconds,
        } => {
            let needs_workspace_resolve = workspace_id.is_none() || session_name.is_none();
            let resolved = if needs_workspace_resolve {
                Some(WorkspaceResolver::resolve(".", globals.root.clone())?)
            } else {
                None
            };
            let workspace_id = match workspace_id {
                Some(raw) => raw.parse::<WorkspaceId>()?,
                None => resolved
                    .as_ref()
                    .ok_or_else(|| anyhow!("workspace_id missing but workspace was not resolved"))?
                    .workspace_id
                    .clone(),
            };
            let session_name = match session_name {
                Some(name) => name,
                None => resolved
                    .as_ref()
                    .ok_or_else(|| anyhow!("session_name missing but workspace was not resolved"))?
                    .session_name
                    .clone(),
            };
            let mux = match mux {
                Some(mux) => mux,
                None => rimz::mux::auto_detect_backend(globals.mux)?,
            };
            let program = sidebar_renderer_program();
            let mut command = Command::new(&program);
            command
                .args([
                    "serve",
                    "--workspace-id",
                    workspace_id.as_str(),
                    "--mux",
                    mux.as_str(),
                    "--session-name",
                    &session_name,
                    "--tick-seconds",
                    &tick_seconds.to_string(),
                ])
                .env("RIMZ_BIN", rimz_cli_program());
            let status = command
                .status()
                .with_context(|| format!("running `{}` serve", program.to_string_lossy()))?;
            if !status.success() {
                bail!("rimz-sidebar serve exited with {status}");
            }
            Ok(())
        }
    }
}

fn open_ledger_by_workspace_id(workspace_id: WorkspaceId) -> Result<Ledger> {
    let paths = StatePaths::for_workspace(workspace_id.clone()).context("preparing state paths")?;
    let runtime = RuntimePaths::for_workspace(workspace_id).context("preparing runtime paths")?;
    Ledger::open(paths, runtime).context("opening ledger")
}

fn session_name_from_record(ledger: &Ledger) -> Option<String> {
    workspace_record::read(&ledger.paths().workspace_record)
        .ok()
        .map(|record| record.session_name)
}

fn pane_list_fixture() -> Result<Option<Vec<rimz::feed::PaneRef>>> {
    let Some(path) = std::env::var_os("RIMZ_TEST_PANE_LIST").filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    let bytes = std::fs::read(&path)
        .with_context(|| format!("reading RIMZ_TEST_PANE_LIST {}", path.display()))?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

/// Coalescing window for the shared snapshot cache. Well under the default 2s
/// data tick: when one ledger-delta wakeup wakes every sidebar at once, the
/// first produces the heavy snapshot and the rest read it back within this
/// window instead of each spawning their own `list-panes`. Short enough that
/// live pane/git drift (which fires no ledger delta) still surfaces inside one
/// tick — the same staleness budget the diff-stats cache already accepts.
const SNAPSHOT_CACHE_TTL: Duration = Duration::from_millis(750);

/// How a non-producing sidebar waits for the single producer's cache write
/// before giving up and producing locally. ~200ms total (10 × 20ms).
const SNAPSHOT_CACHE_WAIT_STEP: Duration = Duration::from_millis(20);
const SNAPSHOT_CACHE_WAIT_STEPS: u32 = 10;

/// Shared, single-flight snapshot cache. Holds the ledger rollup plus the live
/// pane list keyed to one `(workspace, session)` — the per-workspace runtime
/// root scopes the workspace; `session_name` guards against serving one
/// session's panes (which the Zellij backend stamps from the requested session,
/// not the true owner) to a sidebar pinned to another during a detach or
/// session-rotation handoff. Per-sidebar exclusion and own-view are applied by
/// the reader, so the cached snapshot is pre-pane-fold.
#[derive(Serialize, Deserialize)]
struct SnapshotCache {
    produced_at_ms: u64,
    session_name: String,
    panes: Vec<rimz::feed::PaneRef>,
    snapshot: rimz::SidebarSnapshot,
}

/// Read a same-session cache entry regardless of coalescing freshness. `None`
/// when it is absent, for another session, or unreadable. Used as the
/// hold-last-good fallback for a degraded fresh read.
fn read_snapshot_cache(cache_path: &Path, session: &str) -> Option<SnapshotCache> {
    let bytes = std::fs::read(cache_path).ok()?;
    let cache: SnapshotCache = serde_json::from_slice(&bytes).ok()?;
    (cache.session_name == session).then_some(cache)
}

/// Return a same-session cache entry younger than [`SNAPSHOT_CACHE_TTL`], or
/// `None` when it is absent, stale, for another session, or unreadable.
fn fresh_snapshot_cache(cache_path: &Path, session: &str) -> Option<SnapshotCache> {
    let cache = read_snapshot_cache(cache_path, session)?;
    let fresh =
        unix_now_ms().saturating_sub(cache.produced_at_ms) <= SNAPSHOT_CACHE_TTL.as_millis() as u64;
    fresh.then_some(cache)
}

/// Do the heavy work: project the ledger rollup and enumerate the session's
/// live panes. The `(workspace, session)`-shared result of this is what the
/// cache amortizes across sidebars.
fn produce_snapshot_base(
    ledger: &Ledger,
    mux: MuxName,
    session: &str,
) -> Result<(rimz::SidebarSnapshot, Vec<rimz::feed::PaneRef>)> {
    let rollup = ledger.snapshot()?;
    let panes = rimz::mux::backend_for(mux).list_panes(PaneListOptions {
        session_name: Some(session.to_owned()),
    })?;
    Ok((rollup, panes))
}

/// Fill any field a fresh `list-panes` read dropped, from the last good read of
/// the same pane id. A mid-tick race occasionally returns a live pane with a
/// null `command`/`cwd`/`pane_process_start`; left as-is that relabels a known
/// pane as a bare `process` row or regroups it under `external` until the next
/// read. Carrying the missing fields forward by pane id keeps the row steady,
/// and is unbounded while the pane persists — where a whole-list hold would
/// also mask genuinely changed panes. Scoped to the exact pane id, so a reused
/// id (a relaunch reports its own fresh fields) is never backfilled from the
/// prior tenant.
fn carry_forward_pane_fields(fresh: &mut [rimz::feed::PaneRef], prev: &[rimz::feed::PaneRef]) {
    for pane in fresh.iter_mut() {
        let Some(prior) = prev.iter().find(|prior| prior.pane_id == pane.pane_id) else {
            continue;
        };
        if pane.command.is_none() {
            pane.command = prior.command.clone();
        }
        if pane.cwd.is_none() {
            pane.cwd = prior.cwd.clone();
        }
        if pane.pane_process_start.is_none() {
            pane.pane_process_start = prior.pane_process_start;
        }
    }
}

/// Return the ledger rollup + live pane list for `session`, sharing one heavy
/// production across every sidebar via a short-lived single-flight cache.
///
/// Fast path: a fresh same-session cache is read back with no ledger or mux
/// work. Slow path: a non-blocking `try_lock` elects one producer; losers poll
/// briefly for its write, then fall back to producing locally so a wedged
/// producer never strands them.
fn cached_base_or_produce(
    ledger: &Ledger,
    mux: MuxName,
    session: &str,
) -> Result<(rimz::SidebarSnapshot, Vec<rimz::feed::PaneRef>)> {
    let runtime = ledger.runtime_paths();
    let cache_path = runtime.root.join("snapshot.json");

    // Fast path: a fresh same-session entry needs no ledger or mux work.
    if let Some(cache) = fresh_snapshot_cache(&cache_path, session) {
        return Ok((cache.snapshot, cache.panes));
    }

    // Slow path: elect one producer for this `(workspace, session)` refresh.
    // Losers read its write back; if it wedges, they fall back to an uncached
    // local produce rather than block.
    let lock_path = runtime.root.join("snapshot.lock");
    let fresh =
        || fresh_snapshot_cache(&cache_path, session).map(|cache| (cache.snapshot, cache.panes));
    match single_flight::coalesce(
        &lock_path,
        SNAPSHOT_CACHE_WAIT_STEP,
        SNAPSHOT_CACHE_WAIT_STEPS,
        fresh,
    ) {
        Coalesced::Shared((snapshot, panes)) => Ok((snapshot, panes)),
        Coalesced::ProduceLocal => produce_snapshot_base(ledger, mux, session),
        // We won: produce the heavy snapshot and publish it. The guard holds
        // the lock until this arm returns.
        Coalesced::Produce(_guard) => {
            let (rollup, mut panes) = produce_snapshot_base(ledger, mux, session)?;
            // A mid-tick `list-panes` race can drop a live pane's command/cwd/
            // process-start; rather than fold an anonymous `external`/`process`
            // row that blinks out next tick, backfill the missing fields from
            // the last good read of the same pane id.
            if let Some(prev) = read_snapshot_cache(&cache_path, session) {
                carry_forward_pane_fields(&mut panes, &prev.panes);
            }
            let cache = SnapshotCache {
                produced_at_ms: unix_now_ms(),
                session_name: session.to_owned(),
                panes: panes.clone(),
                snapshot: rollup.clone(),
            };
            if let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &cache) {
                tracing::warn!(path = %cache_path.display(), error = %err, "sidebar snapshot cache write failed");
            }
            Ok((rollup, panes))
        }
    }
}

/// How long a worktree's git diff-stats stay cached before the four sequential
/// `git` forks behind them are re-run. A working-tree edit fires no ledger
/// delta, so this column is never push-refreshed — it rides this TTL plus the
/// sidebar's backstop poll. Held wide to keep the git-fork rate low across a
/// multi-worktree fleet; the cost the column buys is freshness lag, which the
/// renderer's latency-tolerant data layer is built to absorb.
const DIFF_STATS_TTL: Duration = Duration::from_secs(5);

/// How a non-producing sidebar waits for the elected producer's diff-stats
/// write before refreshing locally. ~300ms total (15 × 20ms) — wider than the
/// snapshot's ~200ms because the git tail (up to four sequential forks per
/// worktree) runs longer, yet still well under the ~2s backstop tick.
const DIFF_STATS_WAIT_STEP: Duration = Duration::from_millis(20);
const DIFF_STATS_WAIT_STEPS: u32 = 15;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DiffStats {
    added: u32,
    removed: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DiffStatsCache {
    entries: BTreeMap<String, DiffStatsCacheEntry>,
    /// The repo's worktree checkout roots, cached under the same TTL as the
    /// per-worktree diff stats. The set changes only on `git worktree
    /// add/remove`, so grouping reuses it across ticks instead of forking
    /// `git worktree list` every snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktrees: Option<WorktreeRootsCache>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WorktreeRootsCache {
    refreshed_at_ms: u64,
    roots: Vec<PathBuf>,
}

impl WorktreeRootsCache {
    fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= DIFF_STATS_TTL.as_millis() as u64
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct DiffStatsCacheEntry {
    refreshed_at_ms: u64,
    added: Option<u32>,
    removed: Option<u32>,
    /// Live branch resolved from the worktree path, cached under the same TTL
    /// as the diff stats so the group header tracks `git checkout` without a
    /// git call every tick.
    #[serde(default)]
    branch: Option<String>,
}

impl DiffStatsCacheEntry {
    fn new(refreshed_at_ms: u64, stats: Option<DiffStats>, branch: Option<String>) -> Self {
        Self {
            refreshed_at_ms,
            added: stats.map(|stats| stats.added),
            removed: stats.map(|stats| stats.removed),
            branch,
        }
    }

    fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.refreshed_at_ms) <= DIFF_STATS_TTL.as_millis() as u64
    }

    fn stats(&self) -> Option<DiffStats> {
        self.added
            .zip(self.removed)
            .map(|(added, removed)| DiffStats { added, removed })
    }
}

/// Project live git facts onto each worktree group: the diff stats shown on
/// the header and the live branch label. Both are properties of the worktree
/// *path*, not of any one agent, so they belong to the group — which also
/// settles the shared-worktree "whose branch?" ambiguity. The live branch
/// overrides the reducer's pinned label so the header tracks `git checkout` in
/// the linked pane.
fn enrich_worktree_groups(snapshot: &mut rimz::SidebarSnapshot, runtime: &rimz::RuntimePaths) {
    let cache_path = runtime.root.join("diff-stats.json");
    let now_ms = unix_now_ms();

    // The worktree paths this snapshot needs git facts for: a `Worktree`-kind
    // group whose recovered path is a live directory. De-duplicated so two
    // branch-split groups for one dir share a single git read — the cache key
    // is the bare path.
    let mut needed: Vec<String> = Vec::new();
    for group in &snapshot.worktree_groups {
        if group.kind != rimz::SidebarWorktreeKind::Worktree {
            continue;
        }
        let Some(path) = worktree_group_path(group) else {
            continue;
        };
        if Path::new(path).is_dir() && !needed.iter().any(|known| known == path) {
            needed.push(path.to_owned());
        }
    }

    // Refresh the diff stats those paths need — single-flighted across the
    // fleet — then project the resulting cache onto the groups.
    let cache = refresh_diff_stats(&cache_path, runtime, &needed, now_ms);
    for group in &mut snapshot.worktree_groups {
        if group.kind != rimz::SidebarWorktreeKind::Worktree {
            continue;
        }
        let Some(path) = worktree_group_path(group).map(ToOwned::to_owned) else {
            continue;
        };
        // Only paths resolved this tick (live dirs) carry stats, so a stale
        // leftover entry for a now-missing worktree never resurfaces.
        if !needed.iter().any(|known| known == &path) {
            continue;
        }
        let Some(entry) = cache.entries.get(&path).cloned() else {
            continue;
        };
        if let Some(stats) = entry.stats() {
            group.diff_added = Some(stats.added);
            group.diff_removed = Some(stats.removed);
        }
        if let Some(branch) = entry.branch.filter(|branch| !branch.is_empty()) {
            group.label = branch;
        }
    }
}

/// The worktree path a group's rows share, if any. The group key may carry a
/// branch suffix (a path that holds more than one branch), so the bare path is
/// recovered from the rows — every row in a group shares it.
fn worktree_group_path(group: &rimz::SidebarWorktreeGroup) -> Option<&str> {
    group
        .rows
        .iter()
        .find_map(|row| row.worktree_path.as_deref())
        .filter(|path| !path.is_empty())
}

/// Refresh the diff stats for `needed` worktree paths and return the cache map
/// to project. Single-flighted across the fleet, mirroring the snapshot cache:
/// the common case — every needed entry already fresh — touches no lock and
/// forks no git. Otherwise one elected producer forks git for the stale entries
/// and writes the shared cache once; the rest read its write back, or (if it
/// wedges) refresh locally for their own frame without writing — never
/// clobbering the producer's fresher map.
fn refresh_diff_stats(
    cache_path: &Path,
    runtime: &rimz::RuntimePaths,
    needed: &[String],
    now_ms: u64,
) -> DiffStatsCache {
    let stale = |cache: &DiffStatsCache| -> Vec<String> {
        needed
            .iter()
            .filter(|path| {
                !cache
                    .entries
                    .get(path.as_str())
                    .is_some_and(|entry| entry.is_fresh(now_ms))
            })
            .cloned()
            .collect()
    };

    let cache = read_diff_stats_cache(cache_path);
    // Fast path: nothing stale — no lock, no git, as the all-fresh tick already
    // behaved before the single-flight.
    if stale(&cache).is_empty() {
        return cache;
    }

    let lock_path = runtime.root.join("diff-stats.lock");
    let fresh = || {
        let cache = read_diff_stats_cache(cache_path);
        stale(&cache).is_empty().then_some(cache)
    };
    match single_flight::coalesce(
        &lock_path,
        DIFF_STATS_WAIT_STEP,
        DIFF_STATS_WAIT_STEPS,
        fresh,
    ) {
        // A peer already refreshed every entry we need.
        Coalesced::Shared(cache) => cache,
        // We won: re-read (a peer may have written between our miss and the
        // lock), refresh only what is still stale against that read, write once.
        Coalesced::Produce(_guard) => {
            let mut cache = read_diff_stats_cache(cache_path);
            let mut changed = false;
            for path in stale(&cache) {
                cache
                    .entries
                    .insert(path.clone(), refresh_entry(&path, now_ms));
                changed = true;
            }
            if changed && let Err(err) = atomic::write_temp_then_rename_cache(cache_path, &cache) {
                tracing::warn!(path = %cache_path.display(), error = %err, "sidebar diff-stats cache write failed");
            }
            cache
        }
        // The producer wedged: refresh locally for our own frame, but do not
        // write — the producer's map will be fresher.
        Coalesced::ProduceLocal => {
            let mut cache = cache;
            for path in stale(&cache) {
                cache
                    .entries
                    .insert(path.clone(), refresh_entry(&path, now_ms));
            }
            cache
        }
    }
}

/// Produce a fresh diff-stats entry for one worktree path: the sequential `git`
/// forks behind the column (trunk ref → merge-base → numstat) plus the live
/// branch label.
fn refresh_entry(path: &str, now_ms: u64) -> DiffStatsCacheEntry {
    let worktree = Path::new(path);
    DiffStatsCacheEntry::new(
        now_ms,
        worktree_diff_stats(worktree),
        worktree_branch(worktree),
    )
}

fn read_diff_stats_cache(path: &Path) -> DiffStatsCache {
    let Ok(bytes) = std::fs::read(path) else {
        return DiffStatsCache::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn worktree_branch(worktree: &Path) -> Option<String> {
    let branch = git_line(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    // A detached HEAD has no branch to track — keep the reducer's path-basename
    // label rather than printing the literal "HEAD".
    if branch == "HEAD" { None } else { Some(branch) }
}

/// The total diff the worktree carries relative to `main`: committed, staged,
/// and unstaged changes folded into one `+/-`. We diff the *working tree*
/// against the merge-base with the trunk, so it counts what this branch added
/// on top of where it forked — never the trunk's own progress since the fork —
/// and `git diff <commit>` reads the tree on disk, so staged and unstaged work
/// land in the same number as committed work.
fn worktree_diff_stats(worktree: &Path) -> Option<DiffStats> {
    let base = diff_base(worktree)?;
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--no-ext-diff", "--numstat", &base, "--"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_numstat(&String::from_utf8_lossy(&output.stdout)))
}

/// The commit a worktree's diff is measured against: the merge-base between its
/// HEAD and the repo's trunk — the fork point a PR diffs against. Returns
/// `None` (so the header simply omits stats) when there is no trunk or no
/// shared ancestor, e.g. an orphan branch or a repo without `main`/`master`.
fn diff_base(worktree: &Path) -> Option<String> {
    let trunk = trunk_ref(worktree)?;
    git_line(worktree, &["merge-base", "HEAD", &trunk])
}

/// The repo's trunk branch: the local `main`/`master` a worktree forks from and
/// merges back into, falling back to the remote's advertised default for a
/// non-standard name. Branch refs are shared across a repo's worktrees, so this
/// resolves from inside any of them.
fn trunk_ref(worktree: &Path) -> Option<String> {
    for name in ["main", "master"] {
        if git_line(worktree, &["rev-parse", "--verify", "--quiet", name]).is_some() {
            return Some(name.to_owned());
        }
    }
    git_line(
        worktree,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
}

/// The repo's worktree checkout roots, so a worktree parked outside the project
/// root still groups as project-related instead of folding into `external`.
/// Cached under the diff-stats TTL: the worktree set changes only on `git
/// worktree add/remove`, so re-forking `git worktree list` every tick would be
/// pure overhead. Empty on a non-git project or a git probe failure, which
/// leaves the reducer's `project_root` prefix test to stand alone.
fn project_worktree_roots(project_root: &Path, runtime: &RuntimePaths) -> Vec<PathBuf> {
    let cache_path = runtime.root.join("diff-stats.json");
    let mut cache = read_diff_stats_cache(&cache_path);
    let now_ms = unix_now_ms();
    if let Some(cached) = cache.worktrees.as_ref().filter(|w| w.is_fresh(now_ms)) {
        return cached.roots.clone();
    }
    let roots = list_worktree_roots(project_root);
    cache.worktrees = Some(WorktreeRootsCache {
        refreshed_at_ms: now_ms,
        roots: roots.clone(),
    });
    if let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &cache) {
        tracing::warn!(path = %cache_path.display(), error = %err, "sidebar worktree-roots cache write failed");
    }
    roots
}

/// Parse `git -C <root> worktree list --porcelain` into the absolute checkout
/// root of every worktree — main and linked alike — from its `worktree <path>`
/// lines. Linked worktrees report absolute paths, so a checkout outside the
/// project root is captured here exactly as the reducer needs it.
fn list_worktree_roots(project_root: &Path) -> Vec<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success());
    let Some(output) = output else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| PathBuf::from(path.trim()))
        .collect()
}

/// Run `git -C <worktree> <args>` and return its stdout's first non-empty line,
/// or `None` on a missing git binary, a non-zero exit, or empty output.
fn git_line(worktree: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if line.is_empty() { None } else { Some(line) }
}

fn parse_numstat(output: &str) -> DiffStats {
    let mut stats = DiffStats::default();
    for line in output.lines() {
        let mut columns = line.split('\t');
        stats.added = stats
            .added
            .saturating_add(parse_numstat_cell(columns.next()));
        stats.removed = stats
            .removed
            .saturating_add(parse_numstat_cell(columns.next()));
    }
    stats
}

fn parse_numstat_cell(cell: Option<&str>) -> u32 {
    cell.and_then(|value| value.parse::<u64>().ok())
        .map(|value| value.min(u64::from(u32::MAX)) as u32)
        .unwrap_or(0)
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

pub(crate) fn sidebar_renderer_program() -> PathBuf {
    if let Some(path) = env_path("RIMZ_SIDEBAR_BIN") {
        return path;
    }
    if let Some(path) = sibling_bin("rimz-sidebar").filter(|path| path.is_file()) {
        return path;
    }
    which::which(bin_name("rimz-sidebar"))
        .unwrap_or_else(|_| PathBuf::from(bin_name("rimz-sidebar")))
}

pub(crate) fn sidebar_renderer_present() -> bool {
    if let Some(path) = env_path("RIMZ_SIDEBAR_BIN") {
        return path.is_file();
    }
    sibling_bin("rimz-sidebar").is_some_and(|path| path.is_file())
        || which::which(bin_name("rimz-sidebar")).is_ok()
}

/// A sibling of the running executable, named `stem` with the platform suffix.
fn sibling_bin(stem: &str) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    Some(current.parent()?.join(bin_name(stem)))
}

fn bin_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

fn rimz_cli_program() -> PathBuf {
    env_path("RIMZ_BIN")
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_else(|| PathBuf::from(bin_name("rimz")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane with the given id, command, and cwd; other fields are irrelevant
    /// to the carry-forward logic under test.
    fn pane(id: &str, command: Option<&str>, cwd: Option<&str>) -> rimz::feed::PaneRef {
        rimz::feed::PaneRef {
            pane_id: rimz::ids::PaneId::from_parts(MuxName::Zellij, id),
            session_name: "s".to_owned(),
            view_id: None,
            view_kind: None,
            view_name: None,
            is_focused: false,
            command: command.map(ToOwned::to_owned),
            cwd: cwd.map(ToOwned::to_owned),
            pane_pid: None,
            pane_process_start: None,
        }
    }

    #[test]
    fn pane_fields_carry_forward_by_pane_id() {
        // A degraded read drops command and cwd; the last good read of the same
        // pane id backfills them so the row keeps its agent label and worktree
        // group instead of flashing a bare `process` under `external`.
        let mut fresh = vec![pane("terminal_1", None, None)];
        let prev = vec![pane("terminal_1", Some("claude"), Some("/repo"))];
        carry_forward_pane_fields(&mut fresh, &prev);
        assert_eq!(fresh[0].command.as_deref(), Some("claude"));
        assert_eq!(fresh[0].cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn carry_forward_does_not_cross_pane_id() {
        // A different (e.g. reused) pane id reports its own fresh fields and is
        // never backfilled from a stranger's last-good read.
        let mut fresh = vec![pane("terminal_2", None, None)];
        let prev = vec![pane("terminal_1", Some("claude"), Some("/repo"))];
        carry_forward_pane_fields(&mut fresh, &prev);
        assert_eq!(fresh[0].command, None);
        assert_eq!(fresh[0].cwd, None);
    }

    #[test]
    fn fresh_pane_field_wins_when_present() {
        // A genuine handoff (claude → zsh) is a real fresh value, not a dropped
        // field, so it is never overwritten by the prior tenant's command.
        let mut fresh = vec![pane("terminal_1", Some("zsh"), Some("/now"))];
        let prev = vec![pane("terminal_1", Some("claude"), Some("/repo"))];
        carry_forward_pane_fields(&mut fresh, &prev);
        assert_eq!(fresh[0].command.as_deref(), Some("zsh"));
        assert_eq!(fresh[0].cwd.as_deref(), Some("/now"));
    }

    #[test]
    fn parse_numstat_sums_text_diff_and_ignores_binary_rows() {
        let stats = parse_numstat("12\t4\tsrc/lib.rs\n-\t-\tassets/logo.png\n3\t0\tREADME.md\n");

        assert_eq!(
            stats,
            DiffStats {
                added: 15,
                removed: 4,
            }
        );
    }

    #[test]
    fn worktree_branch_reads_live_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status();
            match status {
                Ok(status) => status.success(),
                Err(_) => false,
            }
        };
        if !git(&["init", "-q"]) {
            // No git on PATH (or init failed); the helper degrades to None,
            // which is the documented fallback. Nothing to assert.
            assert_eq!(worktree_branch(dir.path()), None);
            return;
        }
        let _ = git(&["config", "user.email", "t@example.com"]);
        let _ = git(&["config", "user.name", "t"]);
        let _ = git(&["checkout", "-q", "-b", "feature-migration"]);
        std::fs::write(dir.path().join("f"), "x").unwrap();
        let _ = git(&["add", "f"]);
        let _ = git(&["commit", "-q", "-m", "init"]);

        assert_eq!(
            worktree_branch(dir.path()).as_deref(),
            Some("feature-migration"),
            "the live branch is read from the worktree, overriding any pinned label"
        );
        // A non-repository path has no branch to track.
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(worktree_branch(plain.path()), None);
    }

    #[test]
    fn worktree_diff_stats_total_committed_staged_and_unstaged_over_trunk() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        // `-b main` needs Git >= 2.28; an older git fails init and the helper
        // degrades to None, which is the documented fallback.
        if !git(&["init", "-q", "-b", "main"]) {
            assert_eq!(worktree_diff_stats(dir.path()), None);
            return;
        }
        let _ = git(&["config", "user.email", "t@example.com"]);
        let _ = git(&["config", "user.name", "t"]);
        let write = |name: &str, body: &str| std::fs::write(dir.path().join(name), body).unwrap();

        // Fork point on `main`: a three-line tracked file.
        write("base.txt", "a\nb\nc\n");
        let _ = git(&["add", "base.txt"]);
        let _ = git(&["commit", "-q", "-m", "base"]);
        let _ = git(&["branch", "feature-migration"]);

        // `main` advances *after* the fork — a merge-base diff must ignore this,
        // so it never shows up as the worktree's own churn.
        write("base.txt", "a\nB\nc\n");
        let _ = git(&["commit", "-aqm", "trunk moves on"]);

        let _ = git(&["checkout", "-q", "feature-migration"]);
        // Committed on the branch: a new two-line file.
        write("feat.txt", "x\ny\n");
        let _ = git(&["add", "feat.txt"]);
        let _ = git(&["commit", "-q", "-m", "feature work"]);
        // Staged but uncommitted: a new one-line file.
        write("staged.txt", "s\n");
        let _ = git(&["add", "staged.txt"]);
        // Unstaged: one more line appended to a tracked file.
        write("base.txt", "a\nb\nc\nd\n");

        assert_eq!(
            worktree_diff_stats(dir.path()),
            Some(DiffStats {
                // +2 committed, +1 staged, +1 unstaged — all measured from the
                // fork point, none from main's post-fork commit.
                added: 4,
                removed: 0,
            }),
            "the header counts committed + staged + unstaged over the trunk merge-base"
        );

        // A non-repository path has nothing to diff.
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(worktree_diff_stats(plain.path()), None);
    }

    #[test]
    fn list_worktree_roots_includes_a_checkout_outside_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        };
        if !git(&main, &["init", "-q"]) {
            // No git on PATH; the helper degrades to an empty list, which leaves
            // the reducer's project_root prefix test to stand alone.
            assert!(list_worktree_roots(&main).is_empty());
            return;
        }
        let _ = git(&main, &["config", "user.email", "t@example.com"]);
        let _ = git(&main, &["config", "user.name", "t"]);
        std::fs::write(main.join("f"), "x").unwrap();
        let _ = git(&main, &["add", "f"]);
        let _ = git(&main, &["commit", "-q", "-m", "init"]);

        // A worktree parked OUTSIDE the project root (a sibling of `main`).
        let external = tmp.path().join("external-wt");
        let _ = git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                external.to_str().unwrap(),
                "-b",
                "feature",
            ],
        );

        let roots = list_worktree_roots(&main);
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let roots: Vec<PathBuf> = roots.iter().map(|r| canon(r)).collect();
        assert!(
            roots.contains(&canon(&main)),
            "the main checkout is one of the worktree roots"
        );
        assert!(
            roots.contains(&canon(&external)),
            "a worktree outside the project root is enumerated, so it groups as project-related"
        );

        // A non-repository path has no worktrees to list.
        let plain = tempfile::tempdir().unwrap();
        assert!(list_worktree_roots(plain.path()).is_empty());
    }

    #[test]
    fn diff_stats_cache_entry_expires_after_ttl() {
        let entry = DiffStatsCacheEntry::new(
            1_000,
            Some(DiffStats {
                added: 2,
                removed: 1,
            }),
            Some("feature-migration".to_owned()),
        );

        assert!(entry.is_fresh(1_000 + DIFF_STATS_TTL.as_millis() as u64));
        assert!(!entry.is_fresh(1_001 + DIFF_STATS_TTL.as_millis() as u64));
        assert_eq!(
            entry.stats(),
            Some(DiffStats {
                added: 2,
                removed: 1,
            })
        );
        assert_eq!(entry.branch.as_deref(), Some("feature-migration"));
    }

    fn write_snapshot_cache(path: &Path, session: &str, produced_at_ms: u64) {
        let ws = WorkspaceId::from_project_root(Path::new("/tmp/x"));
        let cache = SnapshotCache {
            produced_at_ms,
            session_name: session.to_owned(),
            panes: Vec::new(),
            snapshot: rimz::SidebarSnapshot::build(ws, Vec::new(), Vec::new()),
        };
        atomic::write_temp_then_rename(path, &cache).expect("write snapshot cache");
    }

    #[test]
    fn snapshot_cache_serves_a_fresh_same_session_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        write_snapshot_cache(&path, "rimz-query-engine", unix_now_ms());
        assert!(fresh_snapshot_cache(&path, "rimz-query-engine").is_some());
    }

    #[test]
    fn snapshot_cache_misses_a_different_session() {
        // One session's panes must never be served to a sidebar pinned to
        // another — the Zellij backend stamps PaneRef.session_name from the
        // requested session, so a cross-session hit would mislabel panes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        write_snapshot_cache(&path, "rimz-query-engine", unix_now_ms());
        assert!(fresh_snapshot_cache(&path, "rimz-other").is_none());
    }

    #[test]
    fn snapshot_cache_misses_a_stale_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);
        write_snapshot_cache(&path, "rimz-query-engine", stale);
        assert!(fresh_snapshot_cache(&path, "rimz-query-engine").is_none());
    }

    #[test]
    fn snapshot_cache_misses_when_absent_or_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        assert!(fresh_snapshot_cache(&path, "rimz-query-engine").is_none());
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(fresh_snapshot_cache(&path, "rimz-query-engine").is_none());
    }
}
