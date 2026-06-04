use std::collections::{HashMap, HashSet};
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
        /// Require a pane cache produced at or after this Unix millisecond.
        #[arg(long, hide = true)]
        min_pane_cache_ms: Option<u64>,
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
        #[arg(long, default_value_t = 1)]
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
            min_pane_cache_ms,
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
                let snapshot = match read_published_snapshot(
                    ledger.paths(),
                    runtime,
                    session,
                    exclude.as_ref(),
                ) {
                    Some(snapshot) => snapshot,
                    // Cold start: no published panes yet, so own-view is not
                    // computed — the bare rollup stands until the next tick.
                    None => enrich_consumer(ledger.snapshot()?, None, runtime, exclude.as_ref()),
                };
                return emit(&snapshot);
            }

            // Producer (or a deterministic test fixture, or a bare inspection
            // call): resolve the base — ledger rollup plus live pane list,
            // single-flighted across the fleet — then fold the git enrichments
            // and publish the cache the consumers read.
            let (mut snapshot, frame): (rimz::SidebarSnapshot, Option<SnapshotCache>) = match (
                &session_name,
                fixture,
            ) {
                // A test fixture stands in for the mux; never touch the shared
                // cache so deterministic tests can neither poison nor read it.
                (Some(session), Some(fixture)) => (
                    ledger.snapshot()?,
                    Some(SnapshotCache {
                        produced_at_ms: unix_now_ms(),
                        session_name: session.clone(),
                        panes: fixture,
                    }),
                ),
                (Some(session), None) => {
                    let mux = mux
                        .or(globals.mux)
                        .or_else(|| rimz::mux::auto_detect_backend(None).ok());
                    match mux {
                        Some(mux) => {
                            match cached_base_or_produce(&ledger, mux, session, min_pane_cache_ms) {
                                Ok((rollup, frame)) => (rollup, Some(frame)),
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
                            }
                        }
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
                snapshot = snapshot
                    .with_subagent_context(rimz::ledger::subagent_context::read_all(runtime));
                let activity = rimz::agent_activity::read_for_keys(
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
            snapshot.wired_lazy_kinds = rimz::sidebar::snapshot::wired_lazy_kinds();
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
                let daemon_pids = rimz::remote_control::codex_daemon_pids();
                if !daemon_pids.is_empty() {
                    let loaded = rimz::agents::codex::loaded_daemon_threads();
                    snapshot.drop_dead_daemon_sessions(&daemon_pids, loaded.as_ref());
                }
            }
            if let Some(frame) = frame {
                let mut panes = frame.panes;
                if let Some(own) = exclude.as_ref() {
                    snapshot.own_view = rimz::SidebarOwnView::from_panes(own, &panes);
                }
                // Computed from the full session pane list (pre-exclusion), before
                // `with_live_panes` consumes `panes`.
                snapshot.only_daemon_view_remains = rimz::SidebarSnapshot::only_daemon_view(&panes);
                // Backends that report no per-pane process start (Zellij) leave the
                // cwd-fallback guard (`pane_start_allows_bind`) blind, so a stale
                // daemon-mode Codex session would latch onto a freshly-started pane
                // in the same cwd. Derive the in-pane agent CLI's start from `/proc`
                // and stamp it so the guard fires: a process newer than a session
                // can't inherit it. Cheap — `in_pane_agent_start` scans `/proc` only
                // for a Codex pane that lacks a native start (so tmux pays nothing).
                for pane in &mut panes {
                    if pane.pane_process_start.is_some() {
                        continue;
                    }
                    let Some(cwd) = pane.cwd.as_deref().filter(|cwd| !cwd.is_empty()) else {
                        continue;
                    };
                    if let Some(kind) = pane
                        .command
                        .as_deref()
                        .and_then(rimz::ledger::snapshot::command_agent_kind)
                    {
                        pane.pane_process_start =
                            rimz::remote_control::in_pane_agent_start(kind, cwd);
                    }
                }
                snapshot = snapshot.with_live_panes(panes, exclude.as_ref());
            }
            snapshot.agent_hooks_ready = rimz::sidebar::snapshot::agent_hooks_ready();
            // Walk transcript history for the fleet + per-provider spend before
            // the config fold, so the dashboard panels are built, ranked, and
            // capped with each provider's spend already known.
            let spending = compute_fleet_spending(runtime);
            // Publish the aggregated totals so consumer tabs can read them
            // without re-walking the JSONL history themselves.
            rimz::agents::spending::write_provider_spending_cache(
                &runtime.root.join("provider-spending.json"),
                &spending,
            );
            // Fold the per-machine config onto the snapshot: the per-provider
            // dashboard (account-scoped budgets, spend, emblem). The
            // producer owns the out-of-band account probe (a subprocess) and
            // publishes it to `accounts.json` for consumer tabs to read back.
            // Best-effort — a config read failure falls back to defaults, so
            // display preference is enrichment, never a precondition. Loaded
            // once here: the fold consumes it and the git probe reads the
            // preferred trunk from it.
            let config = rimz::config::MachineConfig::load().unwrap_or_default();
            let trunk = config.sidebar.trunk.clone();
            snapshot = rimz::sidebar::snapshot::fold_machine_config_producing(
                snapshot,
                runtime,
                &spending.by_provider,
                config,
            );
            enrich_worktree_groups(&mut snapshot, runtime, trunk.as_deref());
            apply_spending(&mut snapshot, &spending);
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
fn fresh_snapshot_cache(
    cache_path: &Path,
    session: &str,
    min_produced_at_ms: Option<u64>,
) -> Option<SnapshotCache> {
    let cache = read_snapshot_cache(cache_path, session)?;
    let fresh =
        unix_now_ms().saturating_sub(cache.produced_at_ms) <= SNAPSHOT_CACHE_TTL.as_millis() as u64;
    let new_enough = min_produced_at_ms.is_none_or(|min| cache.produced_at_ms >= min);
    (fresh && new_enough).then_some(cache)
}

/// The session's live panes from the mux — the `list-panes` round-trip the
/// snapshot cache amortizes across the fleet. The ledger rollup is read
/// separately (fresh from `latest.json`), so this enumerates only the pane set.
/// One round-trip is the whole cost: the per-view `is_focused` mark rides the
/// pane list itself, so the sidebar's selection baseline needs no second
/// per-client probe.
fn list_session_panes(mux: MuxName, session: &str) -> Result<Vec<rimz::feed::PaneRef>> {
    Ok(rimz::mux::backend_for(mux).list_panes(PaneListOptions {
        session_name: Some(session.to_owned()),
        ..Default::default()
    })?)
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

/// Backfill any field a fresh read dropped from the last good read of the same
/// pane id (see [`carry_forward_pane_fields`]). Shared by both produce arms —
/// the elected producer and a loser falling back to a local produce — so a
/// raced `list-panes` answer renders no anonymous row on either path. Read-only
/// on the cache; the winner-only metrics enrich and cache write stay in the
/// `Produce` arm.
fn carry_forward_from_cache(panes: &mut [rimz::feed::PaneRef], cache_path: &Path, session: &str) {
    if let Some(prev) = read_snapshot_cache(cache_path, session) {
        carry_forward_pane_fields(panes, &prev.panes);
    }
}

/// Return the event-fresh ledger rollup + live pane list for `session`.
///
/// The rollup is always read fresh from `latest.json` (`Ledger::snapshot_cached`,
/// lock-free on the common path), so a status change or a new agent in an
/// existing pane shows within one wakeup rather than waiting on the pane cadence.
/// Only the expensive `list-panes` round-trip is coalesced across the fleet (see
/// [`cached_panes_or_produce`]).
fn cached_base_or_produce(
    ledger: &Ledger,
    mux: MuxName,
    session: &str,
    min_pane_cache_ms: Option<u64>,
) -> Result<(rimz::SidebarSnapshot, SnapshotCache)> {
    let frame = cached_panes_or_produce(ledger, mux, session, min_pane_cache_ms)?;
    let rollup = ledger.snapshot_cached()?;
    Ok((rollup, frame))
}

/// Return the live pane frame for `session` — the pane list plus the
/// `produced_at_ms` read stamp the renderer's jump guard orders against —
/// sharing one `list-panes` round-trip across every sidebar via a short-lived
/// single-flight cache.
///
/// Fast path: a fresh same-session cache is read back with no mux work. Slow
/// path: a non-blocking `try_lock` elects one producer; losers poll briefly for
/// its write, then fall back to producing locally so a wedged producer never
/// strands them.
fn cached_panes_or_produce(
    ledger: &Ledger,
    mux: MuxName,
    session: &str,
    min_pane_cache_ms: Option<u64>,
) -> Result<SnapshotCache> {
    let runtime = ledger.runtime_paths();
    let cache_path = runtime.root.join("snapshot.json");

    // Fast path: a fresh same-session entry needs no mux work.
    if let Some(cache) = fresh_snapshot_cache(&cache_path, session, min_pane_cache_ms) {
        return Ok(cache);
    }

    // Slow path: elect one producer for this `(workspace, session)` refresh.
    // Losers read its write back; if it wedges, they fall back to an uncached
    // local produce rather than block.
    let lock_path = runtime.root.join("snapshot.lock");
    let fresh = || fresh_snapshot_cache(&cache_path, session, min_pane_cache_ms);
    let produce_local = || -> Result<SnapshotCache> {
        Ok(SnapshotCache {
            produced_at_ms: unix_now_ms(),
            session_name: session.to_owned(),
            panes: list_session_panes(mux, session)?,
        })
    };
    match single_flight::coalesce(
        &lock_path,
        SNAPSHOT_CACHE_WAIT_STEP,
        SNAPSHOT_CACHE_WAIT_STEPS,
        fresh,
    ) {
        Coalesced::Shared(cache) => Ok(cache),
        // The producer wedged past the wait: produce locally rather than block.
        // The raced-read repair still applies — without it a dropped command/cwd
        // on this one path folds the anonymous row the winner path guards against.
        Coalesced::ProduceLocal => {
            let mut cache = produce_local()?;
            carry_forward_from_cache(&mut cache.panes, &cache_path, session);
            Ok(cache)
        }
        // We won: fork `list-panes` and publish it. The guard holds the lock
        // until this arm returns.
        Coalesced::Produce(_guard) => {
            let mut cache = produce_local()?;
            // A mid-tick `list-panes` race can drop a live pane's command/cwd/
            // process-start; rather than fold an anonymous `external`/`process`
            // row that blinks out next tick, backfill the missing fields from
            // the last good read of the same pane id.
            carry_forward_from_cache(&mut cache.panes, &cache_path, session);
            // Enrich each pane with per-process resource metrics (best-effort,
            // Linux-only). Runs inside the produce lock so only one producer
            // reads `/proc` per tick; the result is in the published pane cache,
            // so consumer tabs never fork their own reads.
            enrich_pane_metrics(&mut cache.panes, session, ledger.runtime_paths());
            if let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &cache) {
                tracing::warn!(path = %cache_path.display(), error = %err, "sidebar snapshot cache write failed");
            }
            Ok(cache)
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
/// `configured_trunk` is the per-machine `[sidebar] trunk` preference the trunk
/// ladder tries first.
fn enrich_worktree_groups(
    snapshot: &mut rimz::SidebarSnapshot,
    runtime: &rimz::RuntimePaths,
    configured_trunk: Option<&str>,
) {
    let cache_path = runtime.root.join("diff-stats.json");
    let now_ms = unix_now_ms();
    // The producer refreshes the live worktrees' diff stats (single-flighted,
    // git forks parallel across worktrees), then the shared projection folds the
    // resulting cache onto the groups — the same projection a consumer applies.
    let needed = needed_worktree_paths(snapshot);
    let cache = refresh_diff_stats(&cache_path, runtime, &needed, now_ms, configured_trunk);
    project_diff_stats(snapshot, &cache);
}

/// Walk every provider's transcript history into a fleet-wide and per-provider
/// [`Spending`](rimz::agents::spending::Spending). Reads the per-workspace
/// `spending.json` cache, refreshes only files whose mtime changed, then writes
/// back if anything was updated, and loads the price book (a TTL-gated remote
/// refresh) so Codex's token counts become dollars. Best-effort: a read/write or
/// fetch failure degrades gracefully to the cached or embedded data.
///
/// Every registered adapter is discovered fleet-wide
/// ([`transcript_files`](rimz::agents::AgentAdapter::transcript_files)) so each
/// counts on the same footing, and the dashboard panel and fleet ledger read
/// one provider's spend the same way regardless of which project it ran in.
fn compute_fleet_spending(runtime: &rimz::RuntimePaths) -> rimz::agents::spending::Spending {
    use rimz::agents::pricing;
    use rimz::agents::spending::{
        Spending, compute_spending, read_spending_cache, write_spending_cache,
    };
    use rimz::agents::{ADAPTERS, AgentAdapter};

    // Tag each file with its adapter at discovery — the source knows the kind,
    // so pricing/bucketing never has to guess it from the path.
    let files: Vec<(&'static dyn AgentAdapter, PathBuf)> = ADAPTERS
        .iter()
        .flat_map(|adapter| {
            adapter
                .transcript_files()
                .into_iter()
                .map(move |file| (*adapter, file))
        })
        .collect();
    if files.is_empty() {
        return Spending::default();
    }

    let cache_path = runtime.root.join("spending.json");
    let mut cache = read_spending_cache(&cache_path);
    let prices = pricing::load_for_spending(&runtime.root.join("pricing-cache.json"));
    let spending = compute_spending(&files, &mut cache, &prices);
    if cache.dirty {
        write_spending_cache(&cache_path, &cache);
    }
    spending
}

/// Attach the fleet `value_tally` to the snapshot — the JSONL today / month /
/// all-time pile read by both the cockpit's today figure and the bottom value
/// corner; `None` when nothing has ever been recorded. The per-provider breakdown
/// is folded into the dashboard panels separately (see `with_provider_aggregates`).
fn apply_spending(
    snapshot: &mut rimz::SidebarSnapshot,
    spending: &rimz::agents::spending::Spending,
) {
    snapshot.value_tally = (!spending.total.is_zero()).then(|| spending.total.clone());
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
    configured_trunk: Option<&str>,
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
            let refreshed = refresh_entries(&stale(&cache), now_ms, configured_trunk);
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
            for (path, entry) in refresh_entries(&stale(&cache), now_ms, configured_trunk) {
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
/// `trunk ref → merge-base → numstat + rev-list ×2 → branch` chain stays
/// sequential. Runs on the diff-stats producer (the fetch worker), never the
/// render thread.
fn refresh_entries(
    paths: &[String],
    now_ms: u64,
    configured_trunk: Option<&str>,
) -> Vec<(String, DiffStatsCacheEntry)> {
    let mut out = Vec::with_capacity(paths.len());
    for chunk in paths.chunks(MAX_PARALLEL_GIT) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|path| {
                    scope.spawn(move || {
                        (path.clone(), refresh_entry(path, now_ms, configured_trunk))
                    })
                })
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
/// forks behind the columns (trunk ref → merge-base, then numstat and the two
/// commit counts off that one base) plus the live branch label. The trunk and
/// merge-base are resolved once and shared by the diff and both commit counts,
/// so each extra column costs one fork, not a full chain.
fn refresh_entry(path: &str, now_ms: u64, configured_trunk: Option<&str>) -> DiffStatsCacheEntry {
    let worktree = Path::new(path);
    let trunk = trunk_ref(worktree, configured_trunk);
    let base = trunk
        .as_deref()
        .and_then(|trunk| diff_base(worktree, trunk));
    let stats = base
        .as_deref()
        .and_then(|base| worktree_diff_stats(worktree, base));
    let commits = base
        .as_deref()
        .and_then(|base| worktree_commits_ahead(worktree, base));
    let behind = base
        .as_deref()
        .zip(trunk.as_deref())
        .and_then(|(base, trunk)| worktree_commits_behind(worktree, base, trunk));
    DiffStatsCacheEntry::new(
        now_ms,
        stats,
        commits,
        behind,
        trunk,
        worktree_branch(worktree),
    )
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
    rev_list_count(worktree, &format!("{base}..HEAD"))
}

/// The commits the trunk has advanced past the worktree's fork point — `git
/// rev-list --count <base>..<trunk>`, the work a rebase would pick up. The
/// mirror of [`worktree_commits_ahead`], off the same merge-base. Deliberately
/// no part of the header's `≡` landed test: a fully-landed worktree is safe to
/// remove however far the trunk has moved on.
fn worktree_commits_behind(worktree: &Path, base: &str, trunk: &str) -> Option<u32> {
    rev_list_count(worktree, &format!("{base}..{trunk}"))
}

/// `git rev-list --count <range>` as a capped `u32` — the shared tail of the
/// ahead/behind columns.
fn rev_list_count(worktree: &Path, range: &str) -> Option<u32> {
    let count = git_line(worktree, &["rev-list", "--count", range])?;
    count
        .parse::<u64>()
        .ok()
        .map(|count| count.min(u64::from(u32::MAX)) as u32)
}

/// The commit a worktree's diff is measured against: the merge-base between its
/// HEAD and the repo's trunk — the fork point a PR diffs against. Returns
/// `None` (so the header simply omits stats) when there is no shared ancestor,
/// e.g. an orphan branch.
fn diff_base(worktree: &Path, trunk: &str) -> Option<String> {
    git_line(worktree, &["merge-base", "HEAD", trunk])
}

/// The repo's trunk branch: the configured `[sidebar] trunk` when it resolves
/// in this repo, else the local `main`/`master` a worktree forks from and
/// merges back into, falling back to the remote's advertised default for a
/// non-standard name. The configured name is a machine-wide *preference* — a
/// repo without that branch falls through to detection rather than losing its
/// stats — and an option-shaped name (leading `-`) is never handed to git.
/// Branch refs are shared across a repo's worktrees, so this resolves from
/// inside any of them.
fn trunk_ref(worktree: &Path, configured: Option<&str>) -> Option<String> {
    let configured = configured.filter(|name| !name.is_empty() && !name.starts_with('-'));
    for name in configured.into_iter().chain(["main", "master"]) {
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

/// Per-pane CPU and IO tick counters sampled by the producer on the previous
/// tick. Two consecutive readings plus the elapsed wall time give rates.
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
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct MetricsSampleCache {
    entries: HashMap<String, MetricsSampleEntry>,
}

fn read_metrics_sample_cache(path: &Path) -> MetricsSampleCache {
    let Ok(bytes) = std::fs::read(path) else {
        return MetricsSampleCache::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

/// Enrich each pane with per-process resource metrics from `/proc`. Reads the
/// prior sample cache to compute two-sample rates (CPU%, IO bytes/s); writes a
/// fresh sample for the next tick. Linux-only; on other platforms every pane's
/// metric fields stay `None`.
fn enrich_pane_metrics(
    panes: &mut [rimz::feed::PaneRef],
    session_name: &str,
    runtime: &rimz::RuntimePaths,
) {
    // Build a ppid → [child pids] map from the current process list so we can
    // find the foreground command the shell is running (the interesting process).
    let all_procs = rimz::proc::list_processes();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in &all_procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }

    // Zellij's `list-panes` reports no per-pane pid, so on that backend every
    // pane arrives pidless and the loop below would skip the whole room.
    // Backfill the root pid from the session server's process forest first,
    // so the metrics (and the process row's shell anchor) work on both backends.
    backfill_zellij_pane_pids(
        panes,
        &all_procs,
        &children,
        session_name,
        rimz::proc::own_uid(),
        &|pid| rimz::proc::cwd(pid),
    );

    let cache_path = runtime.root.join("metrics-sample.json");
    let prior = read_metrics_sample_cache(&cache_path);
    let now_ms = unix_now_ms();
    let clk_tck = rimz::proc::clk_tck() as f64;
    let mut new_entries: HashMap<String, MetricsSampleEntry> = HashMap::new();

    for pane in panes.iter_mut() {
        let Some(shell_pid) = pane.pane_pid else {
            continue;
        };
        // If the shell has exactly one child, its stats are more informative
        // than the shell's own (which idles while the child runs). Fall back to
        // the shell when there are zero or multiple children.
        let stats_pid = match children.get(&shell_pid).map(Vec::as_slice) {
            Some(&[child]) => child,
            _ => shell_pid,
        };

        pane.rss_kb = rimz::proc::rss_kb(stats_pid);

        let cpu_now = rimz::proc::cpu_ticks(stats_pid);
        let io_now = rimz::proc::io_bytes(stats_pid);

        let pane_key = pane.pane_id.to_string();
        if let Some(prior_entry) = prior.entries.get(&pane_key) {
            // Only compute a rate when the stats PID hasn't changed across ticks
            // and the elapsed time is non-trivial (a very short gap yields noise).
            if prior_entry.stats_pid == stats_pid {
                let elapsed_ms = now_ms.saturating_sub(prior_entry.sampled_at_ms);
                let elapsed_secs = elapsed_ms as f64 / 1_000.0;
                if elapsed_secs >= 0.1 {
                    if let Some(ticks) = cpu_now {
                        let delta = ticks.saturating_sub(prior_entry.cpu_ticks);
                        let pct = (delta as f64 / elapsed_secs / clk_tck * 100.0).round();
                        pane.cpu_pct = Some(pct.clamp(0.0, u16::MAX as f64) as u16);
                    }
                    if let Some(bytes) = io_now {
                        let delta = bytes.saturating_sub(prior_entry.io_bytes);
                        pane.io_bps = Some((delta as f64 / elapsed_secs) as u64);
                    }
                }
            }
        }

        new_entries.insert(
            pane_key,
            MetricsSampleEntry {
                stats_pid,
                cpu_ticks: cpu_now.unwrap_or(0),
                io_bytes: io_now.unwrap_or(0),
                sampled_at_ms: now_ms,
            },
        );
    }

    let new_cache = MetricsSampleCache {
        entries: new_entries,
    };
    if let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &new_cache) {
        tracing::warn!(error = %err, "metrics sample cache write failed");
    }
}

/// Backfill `pane_pid` for panes whose backend reported none (Zellij emits no
/// pid field; tmux fills `#{pane_pid}` natively), resolving each pane to its
/// root process — the direct child of the session's `zellij --server <socket>`
/// process — so the field carries tmux's semantics on both backends and the
/// shell→single-child descent above behaves identically.
///
/// Zellij reports a pane's *foreground* command as that process's `/proc`
/// cmdline (argv joined by spaces — the same form as
/// [`ProcInfo`](rimz::proc::ProcInfo)`::cmdline`), so a pane matches the forest
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
    panes: &mut [rimz::feed::PaneRef],
    procs: &[rimz::proc::ProcInfo],
    children: &HashMap<u32, Vec<u32>>,
    session_name: &str,
    own_uid: Option<u32>,
    proc_cwd: &dyn Fn(u32) -> Option<PathBuf>,
) {
    // Nothing to backfill (tmux, or an empty room): skip the server scan.
    if panes.iter().all(|pane| pane.pane_pid.is_some()) {
        return;
    }
    let Some(server_pid) = zellij_server_pid(procs, session_name, own_uid) else {
        return;
    };
    let forest = descendants(children, server_pid);
    let parent_of: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
    for pane in panes.iter_mut() {
        if pane.pane_pid.is_some() {
            continue;
        }
        let Some(command) = pane.command.as_deref() else {
            continue;
        };
        if command == rimz::mux::zellij::SIDEBAR_PANE_NAME {
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
                let narrowed: Vec<u32> = match pane.cwd.as_deref() {
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
        pane.pane_pid = matched.and_then(|pid| walk_to_server_child(&parent_of, server_pid, pid));
    }
}

/// The pid of the session's Zellij server: the same-uid process whose cmdline
/// is `zellij --server <socket>` with the socket's file name equal to the
/// session name (Zellij names the server socket after the session). The uid
/// gate keeps a same-named session of another user from being walked.
fn zellij_server_pid(
    procs: &[rimz::proc::ProcInfo],
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
            rss_kb: None,
            cpu_pct: None,
            io_bps: None,
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
    fn carry_forward_from_cache_backfills_from_disk() {
        // The shared repair both produce arms run: a raced read's dropped
        // fields backfill from the on-disk pane cache, so the wedged-producer
        // fallback path renders no anonymous row either.
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("snapshot.json");
        let prior = SnapshotCache {
            produced_at_ms: 1,
            session_name: "s".to_owned(),
            panes: vec![pane("terminal_1", Some("claude"), Some("/repo"))],
        };
        atomic::write_temp_then_rename_cache(&cache_path, &prior).unwrap();
        let mut panes = vec![pane("terminal_1", None, None)];
        carry_forward_from_cache(&mut panes, &cache_path, "s");
        assert_eq!(panes[0].command.as_deref(), Some("claude"));
        assert_eq!(panes[0].cwd.as_deref(), Some("/repo"));
    }

    #[test]
    fn carry_forward_from_cache_is_noop_without_prior() {
        // No cache on disk (the first tick after session birth): the read
        // passes through untouched rather than erroring.
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("snapshot.json");
        let mut panes = vec![pane("terminal_1", None, None)];
        carry_forward_from_cache(&mut panes, &cache_path, "s");
        assert_eq!(panes[0].command, None);
        assert_eq!(panes[0].cwd, None);
    }

    /// A process-table entry for the pid-backfill matcher fixtures; everything
    /// runs as one uid (1000) unless a test says otherwise.
    fn proc_info(pid: u32, ppid: u32, cmdline: &str) -> rimz::proc::ProcInfo {
        rimz::proc::ProcInfo {
            pid,
            ppid,
            real_uid: 1000,
            cmdline: cmdline.to_owned(),
        }
    }

    /// The ppid→children map `enrich_pane_metrics` builds, over a fixture table.
    fn children_of(procs: &[rimz::proc::ProcInfo]) -> HashMap<u32, Vec<u32>> {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in procs {
            children.entry(p.ppid).or_default().push(p.pid);
        }
        children
    }

    /// The session's Zellij server process, socket named after the session.
    fn server(pid: u32, session: &str) -> rimz::proc::ProcInfo {
        proc_info(
            pid,
            1,
            &format!("/usr/bin/zellij --server /run/user/1000/zellij/contract_version_1/{session}"),
        )
    }

    const SESSION: &str = "rimz-query-engine";

    fn backfill(
        panes: &mut [rimz::feed::PaneRef],
        procs: &[rimz::proc::ProcInfo],
        cwds: &[(u32, &str)],
    ) {
        let cwds: HashMap<u32, PathBuf> = cwds
            .iter()
            .map(|(pid, cwd)| (*pid, PathBuf::from(cwd)))
            .collect();
        backfill_zellij_pane_pids(
            panes,
            procs,
            &children_of(procs),
            SESSION,
            Some(1000),
            &|pid| cwds.get(&pid).cloned(),
        );
    }

    #[test]
    fn unique_foreground_match_backfills_the_pane_root() {
        // The htop pane: Zellij reports the foreground cmdline; the matcher
        // finds the one forest process with it and binds the pane to its root
        // (the direct server child, the zsh) — tmux's `#{pane_pid}` semantics,
        // so the shell→single-child descent then reads htop's stats.
        let procs = vec![
            server(100, SESSION),
            proc_info(200, 100, "zsh"),
            proc_info(300, 200, "htop"),
        ];
        let mut panes = vec![pane("terminal_4", Some("htop"), Some("/repo"))];
        backfill(&mut panes, &procs, &[]);
        assert_eq!(panes[0].pane_pid, Some(200));
    }

    #[test]
    fn unique_match_skips_the_cwd_check() {
        // An agent that chdir'd into its worktree sits in another directory
        // than its pane reports (`claude --worktree`), so a unique cmdline
        // match must bind without a cwd comparison.
        let procs = vec![
            server(100, SESSION),
            proc_info(200, 100, "zsh"),
            proc_info(300, 200, "claude --worktree feature"),
        ];
        let mut panes = vec![pane(
            "terminal_8",
            Some("claude --worktree feature"),
            Some("/repo"),
        )];
        backfill(&mut panes, &procs, &[(300, "/repo/worktrees/feature")]);
        assert_eq!(panes[0].pane_pid, Some(200));
    }

    #[test]
    fn cwd_narrows_same_command_candidates() {
        // Two panes both run `htop`, one per worktree: the cmdline ties, the
        // foreground's `/proc` cwd breaks it, and each pane binds its own root.
        let procs = vec![
            server(100, SESSION),
            proc_info(200, 100, "zsh"),
            proc_info(300, 200, "htop"),
            proc_info(210, 100, "zsh"),
            proc_info(310, 210, "htop"),
        ];
        let mut panes = vec![
            pane("terminal_1", Some("htop"), Some("/wt1")),
            pane("terminal_2", Some("htop"), Some("/wt2")),
        ];
        backfill(&mut panes, &procs, &[(300, "/wt1"), (310, "/wt2")]);
        assert_eq!(panes[0].pane_pid, Some(200));
        assert_eq!(panes[1].pane_pid, Some(210));
    }

    #[test]
    fn ambiguous_candidates_abstain() {
        // Two idle `zsh` panes in one cwd are indistinguishable — by cmdline
        // and by cwd — so both stay pidless: no stats beats a stranger's stats.
        let procs = vec![
            server(100, SESSION),
            proc_info(200, 100, "zsh"),
            proc_info(210, 100, "zsh"),
        ];
        let mut panes = vec![
            pane("terminal_6", Some("zsh"), Some("/repo")),
            pane("terminal_14", Some("zsh"), Some("/repo")),
        ];
        backfill(&mut panes, &procs, &[(200, "/repo"), (210, "/repo")]);
        assert_eq!(panes[0].pane_pid, None);
        assert_eq!(panes[1].pane_pid, None);
    }

    #[test]
    fn deep_foreground_walks_up_to_the_server_child() {
        // A launcher chain (zsh → npm → node script): the foreground match is
        // levels deep, and the walk still lands on the direct server child. A
        // foreground that *is* the server child binds itself.
        let procs = vec![
            server(100, SESSION),
            proc_info(200, 100, "zsh"),
            proc_info(300, 200, "npm run build"),
            proc_info(400, 300, "node /repo/build.js"),
            proc_info(500, 100, "claude remote-control --spawn worktree"),
        ];
        let mut panes = vec![
            pane("terminal_3", Some("node /repo/build.js"), Some("/repo")),
            pane(
                "terminal_1",
                Some("claude remote-control --spawn worktree"),
                Some("/repo"),
            ),
        ];
        backfill(&mut panes, &procs, &[]);
        assert_eq!(panes[0].pane_pid, Some(200));
        assert_eq!(panes[1].pane_pid, Some(500));
    }

    #[test]
    fn no_matching_server_is_a_no_op() {
        // Another session's server, another uid's same-named server, or no uid
        // at all (non-Linux): the backfill leaves every pane untouched rather
        // than walking a stranger's forest.
        let mut other_uid = server(100, SESSION);
        other_uid.real_uid = 1001;
        let procs = vec![
            server(110, "rimz-other"),
            other_uid,
            proc_info(200, 100, "zsh"),
            proc_info(300, 200, "htop"),
        ];
        let mut panes = vec![pane("terminal_4", Some("htop"), Some("/repo"))];
        backfill(&mut panes, &procs, &[]);
        assert_eq!(panes[0].pane_pid, None);

        let procs_ok = vec![server(100, SESSION), proc_info(300, 100, "htop")];
        backfill_zellij_pane_pids(
            &mut panes,
            &procs_ok,
            &children_of(&procs_ok),
            SESSION,
            None, // unknown own uid: skip rather than guess
            &|_| None,
        );
        assert_eq!(panes[0].pane_pid, None);
    }

    #[test]
    fn chrome_and_already_pidded_panes_are_left_alone() {
        // Sidebar chrome shares one cmdline across panes and is excluded from
        // rows, so it is skipped outright; a pane the backend already pidded
        // (tmux) is never re-derived.
        let procs = vec![
            server(100, SESSION),
            proc_info(200, 100, "rimz-sidebar"),
            proc_info(210, 100, "zsh"),
            proc_info(300, 210, "htop"),
        ];
        let chrome = pane("terminal_0", Some("rimz-sidebar"), Some("/repo"));
        let mut pidded = pane("terminal_4", Some("htop"), Some("/repo"));
        pidded.pane_pid = Some(42);
        let mut panes = vec![chrome, pidded];
        backfill(&mut panes, &procs, &[]);
        assert_eq!(panes[0].pane_pid, None);
        assert_eq!(panes[1].pane_pid, Some(42));
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
            assert_eq!(refresh_entry(path, 0, None).stats(), None);
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

        let entry = refresh_entry(path, 0, None);
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
        // Main's one post-fork commit is the branch's behind count, and the
        // resolved trunk names the header's `≡` marker.
        assert_eq!(
            entry.behind,
            Some(1),
            "the behind count is the trunk's commits past the merge-base"
        );
        assert_eq!(entry.trunk.as_deref(), Some("main"));

        // A non-repository path has nothing to diff or count.
        let plain = tempfile::tempdir().unwrap();
        let plain_entry = refresh_entry(plain.path().to_str().unwrap(), 0, None);
        assert_eq!(plain_entry.stats(), None);
        assert_eq!(plain_entry.commits, None);
        assert_eq!(plain_entry.behind, None);
        assert_eq!(plain_entry.trunk, None);
    }

    #[test]
    fn trunk_ladder_prefers_a_configured_branch_that_resolves() {
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
        if !git(&["init", "-q", "-b", "main"]) {
            assert_eq!(trunk_ref(dir.path(), Some("develop")), None);
            return;
        }
        let _ = git(&["config", "user.email", "t@example.com"]);
        let _ = git(&["config", "user.name", "t"]);
        std::fs::write(dir.path().join("f"), "x").unwrap();
        let _ = git(&["add", "f"]);
        let _ = git(&["commit", "-q", "-m", "init"]);
        let _ = git(&["branch", "develop"]);

        // The configured branch exists here, so it wins over `main`.
        assert_eq!(
            trunk_ref(dir.path(), Some("develop")).as_deref(),
            Some("develop")
        );
        // A machine-wide preference this repo lacks falls through to detection
        // rather than losing the repo's stats.
        assert_eq!(
            trunk_ref(dir.path(), Some("absent")).as_deref(),
            Some("main")
        );
        // An option-shaped name is never handed to git; detection stands alone.
        assert_eq!(
            trunk_ref(dir.path(), Some("--help")).as_deref(),
            Some("main")
        );
        assert_eq!(trunk_ref(dir.path(), None).as_deref(), Some("main"));
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
        let cache = SnapshotCache {
            produced_at_ms,
            session_name: session.to_owned(),
            panes: Vec::new(),
        };
        atomic::write_temp_then_rename(path, &cache).expect("write snapshot cache");
    }

    #[test]
    fn snapshot_cache_serves_a_fresh_same_session_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        write_snapshot_cache(&path, "rimz-query-engine", unix_now_ms());
        assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None).is_some());
    }

    #[test]
    fn snapshot_cache_misses_a_different_session() {
        // One session's panes must never be served to a sidebar pinned to
        // another — the Zellij backend stamps PaneRef.session_name from the
        // requested session, so a cross-session hit would mislabel panes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        write_snapshot_cache(&path, "rimz-query-engine", unix_now_ms());
        assert!(fresh_snapshot_cache(&path, "rimz-other", None).is_none());
    }

    #[test]
    fn snapshot_cache_misses_a_stale_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let stale = unix_now_ms().saturating_sub(SNAPSHOT_CACHE_TTL.as_millis() as u64 + 1);
        write_snapshot_cache(&path, "rimz-query-engine", stale);
        assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None).is_none());
    }

    #[test]
    fn snapshot_cache_misses_before_requested_pane_freshness_floor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        let produced_at_ms = unix_now_ms();
        write_snapshot_cache(&path, "rimz-query-engine", produced_at_ms);

        assert!(
            fresh_snapshot_cache(&path, "rimz-query-engine", Some(produced_at_ms)).is_some(),
            "a cache produced at the requested floor is usable"
        );
        assert!(
            fresh_snapshot_cache(
                &path,
                "rimz-query-engine",
                Some(produced_at_ms.saturating_add(1)),
            )
            .is_none(),
            "a pane-sensitive wakeup rejects the pre-signal pane cache"
        );
    }

    #[test]
    fn snapshot_cache_misses_when_absent_or_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("snapshot.json");
        assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None).is_none());
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(fresh_snapshot_cache(&path, "rimz-query-engine", None).is_none());
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
            fresh_snapshot_cache(&path, "rimz-query-engine", None).is_none(),
            "the producer's fresh-only fast path skips a stale entry"
        );
        assert!(
            read_snapshot_cache(&path, "rimz-query-engine").is_some(),
            "the consumer's read serves the stale entry as last-good"
        );
    }
}
