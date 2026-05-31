use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};

use super::{GlobalFlags, open_ledger};
use rimz::ids::{MuxName, WorkspaceId};
use rimz::ledger::atomic;
use rimz::ledger::paths::env_path;
use rimz::ledger::single_flight::{self, Coalesced};
use rimz::ledger::workspace_record;
use rimz::mux::PaneListOptions;
use rimz::sidebar::snapshot::{
    DiffStats, DiffStatsCache, DiffStatsCacheEntry, SNAPSHOT_CACHE_TTL, SnapshotCache,
    WorktreeRootsCache, enrich_consumer, needed_worktree_paths, project_diff_stats,
    read_diff_stats_cache, read_published_snapshot, read_snapshot_cache, unix_now_ms,
};
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
        /// Render read-only from the producer's published cache: never fork
        /// `list-panes` or git. A non-producer renderer (one whose workspace
        /// already has an elder producer) passes this so the per-tab fleet
        /// pays the mux/git round-trip exactly once, on the elder.
        #[arg(long)]
        no_produce: bool,
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
            no_produce,
        } => {
            // A producer forks `list-panes`/git and publishes the shared cache;
            // a non-producer renders read-only from that cache. Default is to
            // produce, so bare CLI calls and the plugin rail are unchanged.
            let produce = !no_produce;
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

            let runtime = ledger.runtime_paths();
            let emit = |snapshot: &rimz::SidebarSnapshot| -> Result<()> {
                if json {
                    let rendered = serde_json::to_string_pretty(snapshot)?;
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
            };

            // Resolve the base and emit. The heavy work (ledger projection +
            // `list-panes`) is the producer's: one elected sidebar per workspace
            // forks it and publishes the shared cache. Every other per-tab
            // renderer is a consumer — it reads that published frame in process,
            // applying only its own-pane exclusion, never forking `list-panes`/git.
            let fixture = pane_list_fixture()?;
            if !produce
                && fixture.is_none()
                && let Some(session) = session_name.as_deref()
            {
                // Consumer: render the producer's published frame in process. A
                // cold cache (no publish yet) falls back to the bare rollup with
                // the same read-only enrichments until the next tick.
                let snapshot = match read_published_snapshot(runtime, session, exclude.as_ref()) {
                    Some(snapshot) => snapshot,
                    None => enrich_consumer(ledger.snapshot()?, None, runtime, exclude.as_ref()),
                };
                return emit(&snapshot);
            }

            // Producer (or a deterministic test fixture, or a bare inspection
            // call): resolve the base — ledger rollup plus live pane list,
            // single-flighted across the fleet — then fold the git enrichments
            // and publish the cache the consumers read.
            let (mut snapshot, panes): (rimz::SidebarSnapshot, Option<Vec<rimz::feed::PaneRef>>) =
                match (&session_name, fixture) {
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
                let roots = project_worktree_roots(&root, runtime);
                snapshot = snapshot.with_worktree_roots(roots);
            }

            // Fold each session's rich statusline context onto its agent state
            // (read-only; the feed process is the writer). Both the context
            // sidecar and the per-tool activity heartbeats fold only onto
            // existing agents, so an empty room skips both directory scans —
            // the common idle case. Activity lands before the pane overlay so
            // age, ranking, the ask-fold guard, and the stall window all see the
            // truer per-tool value rather than the turn-grained event timestamp.
            if !snapshot.agents.is_empty() {
                snapshot =
                    snapshot.with_agent_context(rimz::ledger::agent_context::read_all(runtime));
                let activity = rimz::agent_activity::read_all(runtime);
                snapshot = snapshot.with_agent_activity(&activity);
            }

            if let Some(panes) = panes {
                if let Some(own) = exclude.as_ref() {
                    snapshot.own_view = rimz::SidebarOwnView::from_panes(own, &panes);
                }
                snapshot = snapshot.with_live_panes(panes, exclude.as_ref());
            }
            snapshot.agent_hooks_ready = rimz::sidebar::snapshot::agent_hooks_ready();
            // Fold the per-machine config onto the snapshot: row density plus the
            // per-provider dashboard (account-scoped budgets, spend, emblem). The
            // producer owns the out-of-band account probe (a subprocess) and
            // publishes it to `accounts.json` for consumer tabs to read back.
            // Best-effort — a config read failure falls back to defaults, so
            // display preference is enrichment, never a precondition.
            snapshot = rimz::sidebar::snapshot::fold_machine_config_producing(snapshot, runtime);
            enrich_worktree_groups(&mut snapshot, runtime);
            emit(&snapshot)
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

/// How a non-producing sidebar waits for the single producer's cache write
/// before giving up and producing locally. ~200ms total (10 × 20ms).
const SNAPSHOT_CACHE_WAIT_STEP: Duration = Duration::from_millis(20);
const SNAPSHOT_CACHE_WAIT_STEPS: u32 = 10;

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
    // Serve the rollup from `latest.json` when it is current (O(1), lock-free),
    // re-projecting only when a write raced this fetch. The `list-panes`
    // round-trip is the irreducible cost here; the rollup no longer adds an
    // O(active-events) replay under the workspace lock on the common path.
    let rollup = ledger.snapshot_cached()?;
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

/// How a non-producing sidebar waits for the elected producer's diff-stats
/// write before refreshing locally. ~300ms total (15 × 20ms) — wider than the
/// snapshot's ~200ms because the git tail (up to four sequential forks per
/// worktree) runs longer, yet still well under the ~2s backstop tick.
const DIFF_STATS_WAIT_STEP: Duration = Duration::from_millis(20);
const DIFF_STATS_WAIT_STEPS: u32 = 15;

/// Refresh the producer's per-worktree git facts, then project them onto the
/// snapshot's worktree groups. The git forks are the producer's job — a
/// consumer reads the published frame in process via
/// [`rimz::sidebar::snapshot::read_published_snapshot`] and never reaches here.
fn enrich_worktree_groups(snapshot: &mut rimz::SidebarSnapshot, runtime: &rimz::RuntimePaths) {
    let cache_path = runtime.root.join("diff-stats.json");
    let now_ms = unix_now_ms();
    // The producer refreshes the live worktrees' diff stats (single-flighted,
    // git forks parallel across worktrees), then the shared projection folds the
    // resulting cache onto the groups — the same projection a consumer applies.
    let needed = needed_worktree_paths(snapshot);
    let cache = refresh_diff_stats(&cache_path, runtime, &needed, now_ms);
    project_diff_stats(snapshot, &cache);
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
        // lock), refresh only what is still stale against that read — git forks
        // run in parallel across worktrees — and write once.
        Coalesced::Produce(_guard) => {
            let mut cache = read_diff_stats_cache(cache_path);
            let refreshed = refresh_entries(&stale(&cache), now_ms);
            let changed = !refreshed.is_empty();
            for (path, entry) in refreshed {
                cache.entries.insert(path, entry);
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
            for (path, entry) in refresh_entries(&stale(&cache), now_ms) {
                cache.entries.insert(path, entry);
            }
            cache
        }
    }
}

/// Most worktrees probed concurrently. Each worktree's own chain stays
/// sequential (merge-base needs the trunk ref), but independent worktrees run in
/// parallel; the cap keeps a many-worktree fleet from bursting a fork storm.
const MAX_PARALLEL_GIT: usize = 8;

/// Refresh several worktrees' diff-stats concurrently, returning each path's
/// fresh entry. Independent worktrees run in parallel — bounded to
/// [`MAX_PARALLEL_GIT`] live `git` chains at a time — while each path's own
/// `trunk ref → merge-base → numstat + rev-list → branch` chain stays
/// sequential. Runs on the diff-stats producer (the fetch worker), never the
/// render thread.
fn refresh_entries(paths: &[String], now_ms: u64) -> Vec<(String, DiffStatsCacheEntry)> {
    let mut out = Vec::with_capacity(paths.len());
    for chunk in paths.chunks(MAX_PARALLEL_GIT) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|path| scope.spawn(move || (path.clone(), refresh_entry(path, now_ms))))
                .collect();
            for handle in handles {
                if let Ok(entry) = handle.join() {
                    out.push(entry);
                }
            }
        });
    }
    out
}

/// Produce a fresh diff-stats entry for one worktree path: the sequential `git`
/// forks behind the columns (trunk ref → merge-base, then numstat and
/// commit-count off that one base) plus the live branch label. The merge-base
/// is resolved once and shared by the diff and the commit count, so the two
/// columns cost one extra fork, not two full chains.
fn refresh_entry(path: &str, now_ms: u64) -> DiffStatsCacheEntry {
    let worktree = Path::new(path);
    let base = diff_base(worktree);
    let stats = base
        .as_deref()
        .and_then(|base| worktree_diff_stats(worktree, base));
    let commits = base
        .as_deref()
        .and_then(|base| worktree_commits_ahead(worktree, base));
    DiffStatsCacheEntry::new(now_ms, stats, commits, worktree_branch(worktree))
}

fn worktree_branch(worktree: &Path) -> Option<String> {
    let branch = git_line(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    // A detached HEAD has no branch to track — keep the reducer's path-basename
    // label rather than printing the literal "HEAD".
    if branch == "HEAD" { None } else { Some(branch) }
}

/// The total diff the worktree carries relative to `main`: committed, staged,
/// and unstaged changes folded into one `+/-`. We diff the *working tree*
/// against the `base` merge-base with the trunk, so it counts what this branch
/// added on top of where it forked — never the trunk's own progress since the
/// fork — and `git diff <commit>` reads the tree on disk, so staged and unstaged
/// work land in the same number as committed work.
fn worktree_diff_stats(worktree: &Path, base: &str) -> Option<DiffStats> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["diff", "--no-ext-diff", "--numstat", base, "--"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(parse_numstat(&String::from_utf8_lossy(&output.stdout)))
}

/// The commits the worktree carries ahead of the trunk — `git rev-list --count
/// <base>..HEAD`, the committed work waiting to land. Measured off the same
/// merge-base as the diff, so it counts this branch's own commits since the
/// fork, never the trunk's. The diff's `+/-` also folds in staged/unstaged
/// change; this column is committed work alone.
fn worktree_commits_ahead(worktree: &Path, base: &str) -> Option<u32> {
    let range = format!("{base}..HEAD");
    let count = git_line(worktree, &["rev-list", "--count", &range])?;
    count
        .parse::<u64>()
        .ok()
        .map(|count| count.min(u64::from(u32::MAX)) as u32)
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
        let path = dir.path().to_str().unwrap();
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
            assert_eq!(refresh_entry(path, 0).stats(), None);
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

        let entry = refresh_entry(path, 0);
        assert_eq!(
            entry.stats(),
            Some(DiffStats {
                // +2 committed, +1 staged, +1 unstaged — all measured from the
                // fork point, none from main's post-fork commit.
                added: 4,
                removed: 0,
            }),
            "the header counts committed + staged + unstaged over the trunk merge-base"
        );
        // One commit on the branch since the fork point — staged/unstaged change
        // does not bump the commit count.
        assert_eq!(
            entry.commits,
            Some(1),
            "the commit count is the branch's commits ahead of the trunk merge-base"
        );

        // A non-repository path has nothing to diff or count.
        let plain = tempfile::tempdir().unwrap();
        let plain_entry = refresh_entry(plain.path().to_str().unwrap(), 0);
        assert_eq!(plain_entry.stats(), None);
        assert_eq!(plain_entry.commits, None);
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

    #[test]
    fn read_only_consumer_serves_a_stale_same_session_base() {
        // A `--no-produce` renderer holds the producer's last published base even
        // past the freshness TTL — it renders the last good frame rather than
        // forking its own `list-panes`. The fresh-only read (the producer's fast
        // path) misses the stale entry; the TTL-agnostic read still serves it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);
        write_snapshot_cache(&path, "rimz-query-engine", stale);
        assert!(
            fresh_snapshot_cache(&path, "rimz-query-engine").is_none(),
            "the producer's fresh-only fast path skips a stale entry"
        );
        assert!(
            read_snapshot_cache(&path, "rimz-query-engine").is_some(),
            "the consumer's read serves the stale entry as last-good"
        );
    }
}
