//! The fleet spending walk: the shared `SPENDING_TTL`-gated transcript-history
//! walk feeding the enrichment spine's global `value_tally`, per-workspace
//! `workspace_value_tally`, and per-provider dashboard folds.

use std::collections::{BTreeMap, HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::agents::spending::{
    HeadlineSpec, ProviderSpendingCache, SpendScope, SpendingCaches, WorkspaceSpendingCache,
    compute_scoped_spending, discover_spending_files, read_provider_spending_cache,
    read_spending_cache, unix_secs_now,
};
use crate::harness::schedule::run_log;
use crate::ledger::parse_cache::FileStamp;
use crate::sidebar::timing::SPENDING_STALE_GRACE;
use crate::sidebar::timing::unix_now_ms;
use crate::{RuntimePaths, SidebarSnapshot};

const SPENDING_WAIT_STEP: Duration = Duration::from_millis(20);
const SPENDING_WAIT_STEPS: u32 = 15;

/// Walk every provider's transcript history into a fleet-wide and per-provider
/// [`Spending`](crate::agents::spending::Spending), publishing the stamped
/// result to the persistent shared `provider-spending.json` — the cache consumer
/// tabs read instead of walking, and the producer's own global gate: a stamp
/// younger than [`SPENDING_TTL`](crate::agents::spending::SPENDING_TTL) serves
/// the published provider totals without a global transcript walk or price-book
/// load.
///
/// The stale walk is single-flighted across every workspace for this user. The
/// elected producer reads the persistent shared `spending.json` cache, refreshes
/// only files whose mtime changed, writes it back if anything was updated, and
/// loads the shared price book (a TTL-gated remote refresh) so Codex's token
/// counts become dollars. When only this room's workspace tally is missing or
/// stale, the producer derives it read-only from the persistent shared
/// `spending.json` entry cache instead of joining the global walk election. A
/// timeout fallback grace-serves the last published provider totals while they
/// are bounded-stale, then computes a seeded local result for its own frame and
/// leaves the shared files to the elected producer.
///
/// Every registered adapter is discovered fleet-wide
/// ([`transcript_files`](crate::agents::AgentAdapter::transcript_files)) so each
/// counts on the same footing, and the dashboard panel and fleet ledger read
/// one provider's spend the same way regardless of which project it ran in.
pub(crate) fn compute_fleet_spending_with_walker(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &crate::agents::spending::HeadlineSpec,
) -> crate::agents::spending::SpendingCaches {
    use crate::agents::spending::{SpendScope, read_provider_spending_cache};

    let now_ms = unix_now_ms();
    let scope = SpendScope::for_workspace(
        snapshot.project_root.as_deref(),
        &snapshot.worktree_roots,
        snapshot.worktree_home.as_deref(),
    );
    let scope_hash = (!scope.is_empty()).then(|| scope.hash());
    let live_costs = live_costs_from_snapshot(snapshot);
    let provider_path = runtime.shared_provider_spending_path();
    // Fresh stamp: the published walk is young enough — serve it back with the
    // same single small read a consumer tab pays.
    let published = read_provider_spending_cache(&provider_path);
    if published.is_fresh(now_ms) {
        if let Some(workspace) = fresh_workspace_cache(runtime, scope_hash.as_deref(), now_ms) {
            return crate::agents::spending::SpendingCaches {
                provider: published,
                workspace,
            };
        }
        let files = discover_spending_files();
        if let Some(workspace) = workspace_cache_from_shared_entries(
            runtime,
            &published,
            &scope,
            scope_hash.as_deref(),
            &files,
            spec,
            &live_costs,
        ) {
            return crate::agents::spending::SpendingCaches {
                provider: published,
                workspace,
            };
        }
    }

    let fresh = || {
        let now_ms = unix_now_ms();
        let provider = read_provider_spending_cache(&provider_path);
        if !provider.is_fresh(now_ms) {
            return None;
        }
        if let Some(workspace) = fresh_workspace_cache(runtime, scope_hash.as_deref(), now_ms) {
            return Some(crate::agents::spending::SpendingCaches {
                provider,
                workspace,
            });
        }
        let files = discover_spending_files();
        workspace_cache_from_shared_entries(
            runtime,
            &provider,
            &scope,
            scope_hash.as_deref(),
            &files,
            spec,
            &live_costs,
        )
        .map(|workspace| crate::agents::spending::SpendingCaches {
            provider,
            workspace,
        })
    };
    match crate::ledger::single_flight::coalesce(
        &runtime.shared_spending_lock(),
        SPENDING_WAIT_STEP,
        SPENDING_WAIT_STEPS,
        fresh,
    ) {
        crate::ledger::single_flight::Coalesced::Shared(cache) => cache,
        crate::ledger::single_flight::Coalesced::Produce(_guard) => {
            walk_fleet_spending(walker, runtime, snapshot, spec, true)
        }
        crate::ledger::single_flight::Coalesced::ProduceLocal => {
            served_within_grace(runtime, scope_hash.as_deref())
                .unwrap_or_else(|| walk_fleet_spending(walker, runtime, snapshot, spec, false))
        }
    }
}

/// Read the producer-published spending caches for a consumer fold, deriving a
/// workspace tally from the shared cursor cache on miss without writing any
/// per-workspace cache files.
pub(crate) fn consumer_spending_caches(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &HeadlineSpec,
) -> SpendingCaches {
    let provider = current_provider_spending_cache(runtime);
    let scope = SpendScope::for_workspace(
        snapshot.project_root.as_deref(),
        &snapshot.worktree_roots,
        snapshot.worktree_home.as_deref(),
    );
    let workspace = if scope.is_empty() {
        WorkspaceSpendingCache::default()
    } else {
        let scope_hash = scope.hash();
        let cache = matching_workspace_cache(runtime, Some(&scope_hash));
        if cache.version == crate::agents::spending::WORKSPACE_SPENDING_VERSION
            && cache.scope_hash == scope_hash
        {
            cache
        } else {
            derive_workspace_spending(
                runtime,
                &scope,
                scope_hash,
                provider.refreshed_at_ms,
                &discover_spending_files(),
                spec,
            )
        }
    };
    SpendingCaches {
        provider,
        workspace,
    }
}

fn current_provider_spending_cache(runtime: &RuntimePaths) -> ProviderSpendingCache {
    let cache = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    if cache.is_current_version() {
        cache
    } else {
        ProviderSpendingCache::default()
    }
}

fn derive_workspace_spending(
    runtime: &RuntimePaths,
    scope: &SpendScope,
    scope_hash: String,
    source_refreshed_at_ms: u64,
    files: &[(&'static dyn crate::agents::AgentAdapter, PathBuf)],
    spec: &HeadlineSpec,
) -> WorkspaceSpendingCache {
    let cursor_path = runtime.shared_spending_cursor_path();
    let automation_signature = run_log::automation_signature();
    let key = WorkspaceDeriveKey {
        cursor_path: cursor_path.clone(),
        cursor_stamp: FileStamp::of(&cursor_path),
        files_signature: discovered_files_signature(files),
        automation_signature,
        scope_hash: scope_hash.clone(),
        source_refreshed_at_ms,
        headline: spec.clone(),
    };
    if let Ok(memo) = workspace_derive_memo().lock()
        && let Some(cached) = memo.as_ref()
        && cached.key == key
    {
        return cached.workspace.clone();
    }

    let cursor = read_spending_cache(&cursor_path);
    let automation_files = run_log::automation_transcripts();
    let scoped = compute_scoped_spending(
        files,
        &cursor,
        &automation_files,
        scope,
        unix_secs_now(),
        spec,
    );
    let workspace = WorkspaceSpendingCache {
        version: crate::agents::spending::WORKSPACE_SPENDING_VERSION,
        refreshed_at_ms: source_refreshed_at_ms,
        scope_hash,
        tally: scoped.tally,
        headline_cutoff_secs: scoped.headline_cutoff_secs,
        carry_usd: 0.0,
        live_baselines: BTreeMap::new(),
    };
    if let Ok(mut memo) = workspace_derive_memo().lock() {
        *memo = Some(WorkspaceDeriveMemo {
            key,
            workspace: workspace.clone(),
        });
    }
    workspace
}

#[derive(Clone, Debug, PartialEq)]
struct WorkspaceDeriveMemo {
    key: WorkspaceDeriveKey,
    workspace: WorkspaceSpendingCache,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceDeriveKey {
    cursor_path: PathBuf,
    cursor_stamp: FileStamp,
    files_signature: u64,
    automation_signature: u64,
    scope_hash: String,
    source_refreshed_at_ms: u64,
    headline: HeadlineSpec,
}

fn workspace_derive_memo() -> &'static Mutex<Option<WorkspaceDeriveMemo>> {
    static MEMO: OnceLock<Mutex<Option<WorkspaceDeriveMemo>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(None))
}

fn discovered_files_signature(
    files: &[(&'static dyn crate::agents::AgentAdapter, PathBuf)],
) -> u64 {
    let mut hasher = DefaultHasher::new();
    for (adapter, file) in files {
        adapter.descriptor().kind.hash(&mut hasher);
        file.hash(&mut hasher);
    }
    hasher.finish()
}

fn walk_fleet_spending(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &crate::agents::spending::HeadlineSpec,
    publish: bool,
) -> crate::agents::spending::SpendingCaches {
    use crate::agents::pricing;
    use crate::agents::spending::{
        PROVIDER_SPENDING_VERSION, ProviderSpendingCache, SilentWalk, SpendScope, Spending,
        SpendingCaches, WORKSPACE_SPENDING_VERSION, WalkRequest, WorkspaceSpendingCache,
        read_provider_spending_cache, unix_secs_now, write_provider_spending_cache,
        write_provider_spending_cache_with_rollups, write_workspace_spending_cache,
    };

    let provider_path = runtime.shared_provider_spending_path();
    let scope = SpendScope::for_workspace(
        snapshot.project_root.as_deref(),
        &snapshot.worktree_roots,
        snapshot.worktree_home.as_deref(),
    );
    let scope_hash = (!scope.is_empty()).then(|| scope.hash());
    let live_costs = live_costs_from_snapshot(snapshot);
    // Tag each file with its adapter at discovery — the source knows the kind,
    // so pricing/bucketing never has to guess it from the path.
    let files = discover_spending_files();
    if files.is_empty() {
        // Empty discovery is not authoritative once a prior walk has found spend:
        // transcript homes can be transiently unreadable, and publishing a fresh
        // zero would blank the provider dashboard until the next successful walk.
        let published = read_provider_spending_cache(&provider_path);
        if !published.spending.total.is_zero() {
            return SpendingCaches {
                provider: published,
                workspace: matching_workspace_cache(runtime, scope_hash.as_deref()),
            };
        }
        let refreshed_at_ms = unix_now_ms();
        let spending = Spending::default();
        let workspace = WorkspaceSpendingCache {
            version: WORKSPACE_SPENDING_VERSION,
            refreshed_at_ms,
            scope_hash: scope_hash.clone().unwrap_or_default(),
            tally: Default::default(),
            headline_cutoff_secs: 0,
            carry_usd: 0.0,
            live_baselines: live_costs
                .iter()
                .map(|(id, usd, _)| (id.clone(), *usd))
                .collect(),
        };
        if publish {
            // Stamp the empty result too: an agentless machine must not re-run
            // the (empty) discovery readdirs every tick.
            write_provider_spending_cache(&provider_path, refreshed_at_ms, &spending);
            if let Some(scope_hash) = scope_hash.as_deref() {
                write_workspace_spending_cache(
                    &runtime.workspace_spending_path(scope_hash),
                    &workspace,
                );
                prune_workspace_spending_siblings(runtime, scope_hash);
            }
        }
        return SpendingCaches {
            provider: ProviderSpendingCache {
                version: PROVIDER_SPENDING_VERSION,
                refreshed_at_ms,
                days: Default::default(),
                models: Default::default(),
                spending,
            },
            workspace,
        };
    }

    let cache_path = runtime.shared_spending_cursor_path();
    // The price book exists only to price the walk, so its load (and TTL-gated
    // remote refresh, including the unknown-model chase) rides the stale arm
    // with it. A local fallback uses the embedded table so it never writes the
    // shared pricing cache without the spending lock.
    let now_secs = unix_secs_now();
    let prices = if publish {
        let unknowns = walker.recorded_unknown_models(&cache_path, &files, now_secs);
        pricing::load_for_spending(&runtime.shared_pricing_cache_path(), &unknowns)
    } else {
        pricing::PriceBook::embedded()
    };
    let origin_overrides = codex_origin_overrides(snapshot);
    let automation_files = run_log::automation_transcripts();
    let req = WalkRequest {
        files: &files,
        prices: &prices,
        now_secs,
        origin_overrides: &origin_overrides,
        automation_files: &automation_files,
        automation_signature: run_log::automation_signature(),
        scope: Some(&scope),
        spec,
    };
    let result = if publish {
        let mut observer = PublishingWalkObserver {
            runtime,
            provider_path: provider_path.clone(),
            files: &files,
            automation_files: &automation_files,
            now_secs,
            scope: Some(&scope),
            scope_hash: scope_hash.clone(),
            spec,
            live_costs: &live_costs,
        };
        walker.walk(&cache_path, &req, &mut observer)
    } else {
        let mut observer = SilentWalk;
        walker.walk_local(&cache_path, &req, &mut observer)
    };
    let refreshed_at_ms = unix_now_ms();
    let workspace = if let Some(scope_hash) = scope_hash.as_deref() {
        reconciled_workspace_cache(
            runtime,
            scope_hash,
            refreshed_at_ms,
            result.workspace_tally.clone(),
            result.workspace_headline_cutoff_secs,
            &live_costs,
        )
    } else {
        WorkspaceSpendingCache {
            version: WORKSPACE_SPENDING_VERSION,
            refreshed_at_ms,
            ..Default::default()
        }
    };
    if publish {
        write_provider_spending_cache_with_rollups(
            &provider_path,
            refreshed_at_ms,
            &result.spending,
            &result.days,
            &result.models,
        );
        if let Some(scope_hash) = scope_hash.as_deref() {
            write_workspace_spending_cache(
                &runtime.workspace_spending_path(scope_hash),
                &workspace,
            );
            prune_workspace_spending_siblings(runtime, scope_hash);
        }
    }
    SpendingCaches {
        provider: ProviderSpendingCache {
            version: PROVIDER_SPENDING_VERSION,
            refreshed_at_ms,
            days: result.days,
            models: result.models,
            spending: result.spending,
        },
        workspace,
    }
}

struct PublishingWalkObserver<'a> {
    runtime: &'a RuntimePaths,
    provider_path: PathBuf,
    files: &'a [(&'static dyn crate::agents::AgentAdapter, PathBuf)],
    automation_files: &'a std::collections::HashSet<PathBuf>,
    now_secs: u64,
    scope: Option<&'a crate::agents::spending::SpendScope>,
    scope_hash: Option<String>,
    spec: &'a crate::agents::spending::HeadlineSpec,
    live_costs: &'a [(String, f64, Option<u64>)],
}

impl crate::agents::spending::WalkObserver for PublishingWalkObserver<'_> {
    fn on_interval(&mut self, cache: &crate::agents::spending::SpendingDiskCache) {
        let result = crate::agents::spending::aggregate_walk_publish(
            self.files,
            cache,
            self.automation_files,
            self.now_secs,
            self.scope,
            self.spec,
        );
        let refreshed_at_ms = unix_now_ms();
        crate::agents::spending::write_provider_spending_cache_with_rollups(
            &self.provider_path,
            refreshed_at_ms,
            &result.spending,
            &result.days,
            &result.models,
        );
        if let Some(scope_hash) = self.scope_hash.as_deref() {
            let workspace = reconciled_workspace_cache(
                self.runtime,
                scope_hash,
                refreshed_at_ms,
                result.workspace_tally,
                result.workspace_headline_cutoff_secs,
                self.live_costs,
            );
            crate::agents::spending::write_workspace_spending_cache(
                &self.runtime.workspace_spending_path(scope_hash),
                &workspace,
            );
            prune_workspace_spending_siblings(self.runtime, scope_hash);
        }
    }
}

fn live_costs_from_snapshot(snapshot: &SidebarSnapshot) -> Vec<(String, f64, Option<u64>)> {
    crate::sidebar::refresh::live_spend::live_row_costs(snapshot)
        .map(|(id, usd, registered_at)| (id.to_owned(), usd, registered_at))
        .collect()
}

fn reconciled_workspace_cache(
    runtime: &RuntimePaths,
    scope_hash: &str,
    refreshed_at_ms: u64,
    tally: crate::agents::spending::SpendTally,
    headline_cutoff_secs: u64,
    live_costs: &[(String, f64, Option<u64>)],
) -> crate::agents::spending::WorkspaceSpendingCache {
    use crate::agents::spending::{
        WORKSPACE_SPENDING_VERSION, WorkspaceSpendingCache, reconcile_workspace_carry,
    };

    let prev = matching_workspace_cache(runtime, Some(scope_hash));
    let (carry_usd, live_baselines) = reconcile_workspace_carry(
        &prev,
        scope_hash,
        tally.headline.usd,
        headline_cutoff_secs,
        live_costs,
    );
    WorkspaceSpendingCache {
        version: WORKSPACE_SPENDING_VERSION,
        refreshed_at_ms,
        scope_hash: scope_hash.to_owned(),
        tally,
        headline_cutoff_secs,
        carry_usd,
        live_baselines,
    }
}

fn workspace_cache_from_shared_entries(
    runtime: &RuntimePaths,
    provider: &crate::agents::spending::ProviderSpendingCache,
    scope: &crate::agents::spending::SpendScope,
    scope_hash: Option<&str>,
    files: &[(&'static dyn crate::agents::AgentAdapter, PathBuf)],
    spec: &crate::agents::spending::HeadlineSpec,
    live_costs: &[(String, f64, Option<u64>)],
) -> Option<crate::agents::spending::WorkspaceSpendingCache> {
    use crate::agents::spending::{
        compute_scoped_spending, read_spending_cache, unix_secs_now, write_workspace_spending_cache,
    };
    let Some(scope_hash) = scope_hash else {
        return Some(Default::default());
    };
    let cache = read_spending_cache(&runtime.shared_spending_cursor_path());
    let automation_files = run_log::automation_transcripts();
    let has_discovered_cache = files.iter().any(|(_, file)| {
        cache
            .files
            .contains_key(&file.to_string_lossy().into_owned())
    });
    if !provider.spending.total.is_zero() && !has_discovered_cache {
        return None;
    }
    let scoped = compute_scoped_spending(
        files,
        &cache,
        &automation_files,
        scope,
        unix_secs_now(),
        spec,
    );
    let workspace = reconciled_workspace_cache(
        runtime,
        scope_hash,
        provider.refreshed_at_ms,
        scoped.tally,
        scoped.headline_cutoff_secs,
        live_costs,
    );
    write_workspace_spending_cache(&runtime.workspace_spending_path(scope_hash), &workspace);
    prune_workspace_spending_siblings(runtime, scope_hash);
    Some(workspace)
}

fn prune_workspace_spending_siblings(runtime: &RuntimePaths, current_scope_hash: &str) {
    let current_prefix = current_scope_hash.get(..32).unwrap_or(current_scope_hash);
    let Ok(entries) = std::fs::read_dir(&runtime.root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(prefix) = name
            .strip_prefix("workspace-spending.")
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if prefix != current_prefix {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn fresh_workspace_cache(
    runtime: &RuntimePaths,
    scope_hash: Option<&str>,
    now_ms: u64,
) -> Option<crate::agents::spending::WorkspaceSpendingCache> {
    let Some(scope_hash) = scope_hash else {
        return Some(Default::default());
    };
    let cache = crate::agents::spending::read_workspace_spending_cache(
        &runtime.workspace_spending_path(scope_hash),
    );
    cache.is_fresh(now_ms, scope_hash).then_some(cache)
}

fn matching_workspace_cache(
    runtime: &RuntimePaths,
    scope_hash: Option<&str>,
) -> crate::agents::spending::WorkspaceSpendingCache {
    let Some(scope_hash) = scope_hash else {
        return Default::default();
    };
    let cache = crate::agents::spending::read_workspace_spending_cache(
        &runtime.workspace_spending_path(scope_hash),
    );
    if cache.version == crate::agents::spending::WORKSPACE_SPENDING_VERSION
        && cache.scope_hash == scope_hash
    {
        cache
    } else {
        Default::default()
    }
}

fn served_within_grace(
    runtime: &RuntimePaths,
    scope_hash: Option<&str>,
) -> Option<crate::agents::spending::SpendingCaches> {
    let provider = crate::agents::spending::read_provider_spending_cache(
        &runtime.shared_provider_spending_path(),
    );
    if !provider.is_current_version()
        || unix_now_ms().saturating_sub(provider.refreshed_at_ms)
            > SPENDING_STALE_GRACE.as_millis() as u64
    {
        return None;
    }
    Some(crate::agents::spending::SpendingCaches {
        provider,
        workspace: matching_workspace_cache(runtime, scope_hash),
    })
}

fn codex_origin_overrides(snapshot: &SidebarSnapshot) -> HashMap<PathBuf, PathBuf> {
    let row_worktrees: HashMap<&str, &str> = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter(|row| row.name == "codex")
        .filter_map(|row| Some((row.id.as_str(), row.worktree_path.as_deref()?)))
        .collect();
    let mut origins = HashMap::new();
    for agent in &snapshot.agents {
        if agent.kind.as_str() != "codex" {
            continue;
        }
        let Some(transcript_path) = agent.transcript_path.as_deref() else {
            continue;
        };
        let transcript_path = PathBuf::from(transcript_path);
        if !transcript_path.is_absolute() {
            continue;
        }
        let worktree = agent
            .worktree_path
            .as_deref()
            .or_else(|| row_worktrees.get(agent.agent_id.as_str()).copied());
        let Some(origin) =
            worktree.and_then(|worktree| crate::agents::spending::origin_path(Some(worktree)))
        else {
            continue;
        };
        origins.insert(transcript_path, origin);
    }
    origins
}

#[cfg(test)]
mod tests;
