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

/// How long a degraded `list-panes` read may reuse the last good pane list
/// before serving the degraded read anyway. ~2 data ticks: long enough to ride
/// out a mid-tick race that drops a live pane's command, short enough that a
/// genuinely closed pane's row clears promptly and a persistently degraded
/// backend self-heals rather than freezing on a stale list.
const PANE_HOLD_MAX: Duration = Duration::from_secs(4);

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
    /// When `panes` were captured. Distinct from `produced_at_ms` (the
    /// coalescing stamp): a held-last-good read reuses an earlier list, so this
    /// keeps the *original* capture time. Only trusted as a hold source while
    /// `panes` are themselves non-degraded (see [`choose_panes`]). Defaulted so
    /// a pre-field cache file still deserializes.
    #[serde(default)]
    panes_good_at_ms: u64,
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

/// Open the single-flight lock file. `None` (e.g. the runtime dir is missing)
/// means "cannot coordinate" — the caller produces directly without caching.
fn open_snapshot_cache_lock(path: &Path) -> Option<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()
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

/// A pane reading is degraded when a live terminal pane reports no command. The
/// backends list only live panes (the Zellij backend drops held/exited panes),
/// and a live pane always reports its command, so a missing one is a mid-tick
/// `list-panes` race rather than a real state — the signal to hold the last good
/// list instead of folding an anonymous process row.
fn panes_degraded(panes: &[rimz::feed::PaneRef]) -> bool {
    panes.iter().any(|pane| pane.command.is_none())
}

/// Pick the pane list to serve, with the capture time of the chosen list. A good
/// fresh read always wins. A degraded fresh read falls back to a recently good
/// cached list — one that is itself non-degraded and captured within
/// [`PANE_HOLD_MAX`] — smoothing the race into a steady row. With no such
/// fallback the fresh (degraded) read is served so a persistently degraded
/// backend self-heals rather than freezing on a stale list.
fn choose_panes(
    fresh: Vec<rimz::feed::PaneRef>,
    prev_good: Option<(&[rimz::feed::PaneRef], u64)>,
    now_ms: u64,
) -> (Vec<rimz::feed::PaneRef>, u64) {
    if !panes_degraded(&fresh) {
        return (fresh, now_ms);
    }
    if let Some((prev_panes, prev_good_at)) = prev_good
        && !panes_degraded(prev_panes)
        && now_ms.saturating_sub(prev_good_at) <= PANE_HOLD_MAX.as_millis() as u64
    {
        return (prev_panes.to_vec(), prev_good_at);
    }
    (fresh, now_ms)
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

    if let Some(cache) = fresh_snapshot_cache(&cache_path, session) {
        return Ok((cache.snapshot, cache.panes));
    }

    let lock_path = runtime.root.join("snapshot.lock");
    let Some(lock_file) = open_snapshot_cache_lock(&lock_path) else {
        // No place to coordinate (runtime dir missing on a bare call): just do
        // the work without caching it.
        return produce_snapshot_base(ledger, mux, session);
    };

    match fs4::FileExt::try_lock(&lock_file) {
        // We are the single producer. The `lock_file` flock releases when it
        // drops at end of scope (its fd closes).
        Ok(()) => {
            // A peer may have written a fresh entry between our miss and the
            // lock — re-check before doing the heavy work.
            if let Some(cache) = fresh_snapshot_cache(&cache_path, session) {
                return Ok((cache.snapshot, cache.panes));
            }
            let (rollup, fresh) = produce_snapshot_base(ledger, mux, session)?;
            // A mid-tick `list-panes` race can drop a live pane's command;
            // rather than fold an anonymous `external`/`process` row that blinks
            // out next tick, hold the last good pane list (bounded by
            // PANE_HOLD_MAX) while still serving the fresh rollup.
            let now_ms = unix_now_ms();
            let prev = read_snapshot_cache(&cache_path, session);
            let prev_good = prev
                .as_ref()
                .map(|cache| (cache.panes.as_slice(), cache.panes_good_at_ms));
            let (panes, panes_good_at_ms) = choose_panes(fresh, prev_good, now_ms);
            let cache = SnapshotCache {
                produced_at_ms: now_ms,
                panes_good_at_ms,
                session_name: session.to_owned(),
                panes: panes.clone(),
                snapshot: rollup.clone(),
            };
            if let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &cache) {
                tracing::warn!(path = %cache_path.display(), error = %err, "sidebar snapshot cache write failed");
            }
            Ok((rollup, panes))
        }
        // Another sidebar is producing: wait briefly for its write, then fall
        // back to producing locally rather than blocking on a wedged producer.
        Err(_) => {
            for _ in 0..SNAPSHOT_CACHE_WAIT_STEPS {
                std::thread::sleep(SNAPSHOT_CACHE_WAIT_STEP);
                if let Some(cache) = fresh_snapshot_cache(&cache_path, session) {
                    return Ok((cache.snapshot, cache.panes));
                }
            }
            produce_snapshot_base(ledger, mux, session)
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct DiffStats {
    added: u32,
    removed: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct DiffStatsCache {
    entries: BTreeMap<String, DiffStatsCacheEntry>,
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
    let mut cache = read_diff_stats_cache(&cache_path);
    let now_ms = unix_now_ms();
    let mut changed = false;

    for group in &mut snapshot.worktree_groups {
        if group.kind != rimz::SidebarWorktreeKind::Worktree {
            continue;
        }
        // Git facts are per worktree *path*. The group key may carry a branch
        // suffix (a path that holds more than one), so read the path from the
        // rows — every row in a group shares it — and key the cache on it so
        // two branch-split groups for one dir share a single git read.
        let Some(path) = group
            .rows
            .iter()
            .find_map(|row| row.worktree_path.as_deref())
            .filter(|path| !path.is_empty())
        else {
            continue;
        };
        let worktree = Path::new(path);
        if !worktree.is_dir() {
            continue;
        }

        let entry = match cache.entries.get(path).filter(|e| e.is_fresh(now_ms)) {
            Some(entry) => entry.clone(),
            None => {
                let entry = DiffStatsCacheEntry::new(
                    now_ms,
                    worktree_diff_stats(worktree),
                    worktree_branch(worktree),
                );
                cache.entries.insert(path.to_owned(), entry.clone());
                changed = true;
                entry
            }
        };

        if let Some(stats) = entry.stats() {
            group.diff_added = Some(stats.added);
            group.diff_removed = Some(stats.removed);
        }
        if let Some(branch) = entry.branch.filter(|branch| !branch.is_empty()) {
            group.label = branch;
        }
    }

    if changed && let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &cache) {
        tracing::warn!(path = %cache_path.display(), error = %err, "sidebar diff-stats cache write failed");
    }
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

    /// A pane with the given (optional) command; other fields are irrelevant to
    /// the degraded/hold logic under test.
    fn pane(command: Option<&str>) -> rimz::feed::PaneRef {
        rimz::feed::PaneRef {
            pane_id: rimz::ids::PaneId::from_parts(MuxName::Zellij, "terminal_1"),
            session_name: "s".to_owned(),
            view_id: None,
            view_kind: None,
            view_name: None,
            is_focused: false,
            command: command.map(ToOwned::to_owned),
            cwd: Some("/tmp".to_owned()),
            pane_pid: None,
            pane_process_start: None,
        }
    }

    #[test]
    fn panes_degraded_flags_any_missing_command() {
        assert!(!panes_degraded(&[pane(Some("zsh")), pane(Some("claude"))]));
        assert!(!panes_degraded(&[]));
        assert!(panes_degraded(&[pane(Some("zsh")), pane(None)]));
    }

    #[test]
    fn choose_panes_keeps_a_good_fresh_read() {
        let fresh = vec![pane(Some("claude"))];
        let prev = vec![pane(Some("zsh"))];
        let (chosen, at) = choose_panes(fresh, Some((prev.as_slice(), 1_000)), 5_000);
        assert_eq!(chosen, vec![pane(Some("claude"))]);
        assert_eq!(at, 5_000, "a good fresh read stamps the current time");
    }

    #[test]
    fn choose_panes_holds_recent_good_over_degraded_fresh() {
        let fresh = vec![pane(None)];
        let prev = vec![pane(Some("claude"))];
        let hold_ms = PANE_HOLD_MAX.as_millis() as u64;
        // Within the hold window: serve the good list, keeping its capture time.
        let (chosen, at) = choose_panes(fresh, Some((prev.as_slice(), 1_000)), 1_000 + hold_ms);
        assert_eq!(chosen, vec![pane(Some("claude"))]);
        assert_eq!(at, 1_000);
    }

    #[test]
    fn choose_panes_drops_stale_or_degraded_fallback() {
        let hold_ms = PANE_HOLD_MAX.as_millis() as u64;
        // Stale good fallback (past the window) → serve the fresh degraded read.
        let good_but_stale = vec![pane(Some("claude"))];
        let (chosen, at) = choose_panes(
            vec![pane(None)],
            Some((good_but_stale.as_slice(), 1_000)),
            1_000 + hold_ms + 1,
        );
        assert!(panes_degraded(&chosen));
        assert_eq!(at, 1_000 + hold_ms + 1);
        // Degraded fallback → never held; serve the fresh degraded read.
        let degraded_prev = vec![pane(None)];
        let (chosen, _) = choose_panes(
            vec![pane(None)],
            Some((degraded_prev.as_slice(), 4_900)),
            5_000,
        );
        assert!(panes_degraded(&chosen));
        // No fallback at all → serve the fresh degraded read.
        let (chosen, _) = choose_panes(vec![pane(None)], None, 5_000);
        assert!(panes_degraded(&chosen));
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
            panes_good_at_ms: produced_at_ms,
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
    fn snapshot_cache_lock_is_exclusive_then_releases_on_drop() {
        // The single-flight lock elects exactly one producer; a second try while
        // it is held fails, and once the holder drops, the lock is free again.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.lock");
        {
            let held = open_snapshot_cache_lock(&path).expect("open lock");
            assert!(fs4::FileExt::try_lock(&held).is_ok(), "first producer wins");
            let contender = open_snapshot_cache_lock(&path).expect("open lock again");
            assert!(
                fs4::FileExt::try_lock(&contender).is_err(),
                "a second sidebar must not also become the producer"
            );
        }
        let after = open_snapshot_cache_lock(&path).expect("open lock after release");
        assert!(
            fs4::FileExt::try_lock(&after).is_ok(),
            "the lock frees once the producer drops it"
        );
    }
}
