//! The fleet spending walk: the shared `SPENDING_TTL`-gated transcript-history
//! walk feeding the enrichment spine's global `value_tally`, per-workspace
//! `workspace_value_tally`, and per-provider dashboard folds.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::agents::spending::{
    ProviderSpendingCache, SESSION_GAP_SECS, SpendProgress, SpendScope, SpendingCaches,
    WorkspaceSpendingCache, compute_scoped_spending, discover_spending_files,
    read_provider_spending_cache, user_input,
};
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
/// counts on the same footing, and the dashboard panel and fleet store read
/// one provider's spend the same way regardless of which project it ran in.
#[cfg(test)]
pub(crate) fn compute_fleet_spending_with_walker(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &crate::agents::spending::HeadlineSpec,
) -> crate::agents::spending::SpendingCaches {
    let context = SpendingRequestContext {
        project_root: snapshot.project_root.as_deref(),
        worktree_roots: &snapshot.worktree_roots,
        worktree_home: snapshot.worktree_home.as_deref(),
        origin_overrides: codex_origin_overrides(snapshot),
        allow_local_fallback: true,
    };
    compute_fleet_spending(walker, runtime, &context, spec, &mut |_| {})
}

/// Refresh through the elected account-global walker. A sidebar failure serves
/// the latest compatible durable publications and retries on its next cache
/// tick; it never constructs a process-local fallback walker.
pub(crate) fn compute_fleet_spending_via_service(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &crate::agents::spending::HeadlineSpec,
    startup: crate::agents::spending::service::SpendingServiceStartup,
) -> SpendingCaches {
    let request = crate::agents::spending::service::SpendingServiceRequest::workspace(
        runtime,
        runtime.workspace_id.clone(),
        snapshot.project_root.clone(),
        snapshot.worktree_roots.clone(),
        snapshot.worktree_home.clone(),
        codex_origin_overrides(snapshot),
        spec.clone(),
    );
    match crate::agents::spending::service::request(runtime, request, startup) {
        Ok(caches) => caches,
        Err(error) => {
            tracing::debug!(error = %error, "spending service unavailable; serving publication");
            consumer_spending_caches(runtime, snapshot)
        }
    }
}

pub(crate) fn serve_spending_service_request(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    request: &crate::agents::spending::service::SpendingServiceRequest,
    progress: &mut dyn FnMut(SpendProgress),
) -> SpendingCaches {
    let context = SpendingRequestContext {
        project_root: request.project_root.as_deref(),
        worktree_roots: &request.worktree_roots,
        worktree_home: request.worktree_home.as_deref(),
        origin_overrides: request.origin_overrides.clone(),
        allow_local_fallback: false,
    };
    compute_fleet_spending(walker, runtime, &context, &request.headline, progress)
}

pub(crate) fn serve_spending_direct_request(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    request: &crate::agents::spending::service::SpendingServiceRequest,
) -> SpendingCaches {
    let context = SpendingRequestContext {
        project_root: request.project_root.as_deref(),
        worktree_roots: &request.worktree_roots,
        worktree_home: request.worktree_home.as_deref(),
        origin_overrides: request.origin_overrides.clone(),
        allow_local_fallback: true,
    };
    compute_fleet_spending(walker, runtime, &context, &request.headline, &mut |_| {})
}

/// Serve a fully fresh durable result without waiting for the warm walker. The
/// service calls this before its try-lock so concurrent workspace requests do
/// not queue behind an unrelated cold walk.
pub(crate) fn fresh_spending_service_publication(
    runtime: &RuntimePaths,
    request: &crate::agents::spending::service::SpendingServiceRequest,
) -> Option<SpendingCaches> {
    fresh_published_spending(
        runtime,
        request.project_root.as_deref(),
        &request.worktree_roots,
        request.worktree_home.as_deref(),
    )
}

struct SpendingRequestContext<'a> {
    project_root: Option<&'a std::path::Path>,
    worktree_roots: &'a [PathBuf],
    worktree_home: Option<&'a std::path::Path>,
    origin_overrides: HashMap<PathBuf, PathBuf>,
    allow_local_fallback: bool,
}

fn compute_fleet_spending(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    context: &SpendingRequestContext<'_>,
    spec: &crate::agents::spending::HeadlineSpec,
    progress: &mut dyn FnMut(SpendProgress),
) -> SpendingCaches {
    use crate::agents::spending::{SpendScope, read_provider_spending_cache};

    let scope = SpendScope::for_workspace(
        context.project_root,
        context.worktree_roots,
        context.worktree_home,
    );
    let scope_hash = (!scope.is_empty()).then(|| scope.hash());
    let provider_path = runtime.shared_provider_spending_path();
    // Fresh stamp: the published walk is young enough — serve it back with the
    // same single small read a consumer tab pays.
    if let Some(caches) = fresh_published_spending(
        runtime,
        context.project_root,
        context.worktree_roots,
        context.worktree_home,
    ) {
        return caches;
    }

    let fresh = || {
        let now_ms = unix_now_ms();
        let provider = read_provider_spending_cache(&provider_path);
        if !provider.is_fresh(now_ms) {
            return None;
        }
        fresh_workspace_cache(runtime, scope_hash.as_deref(), now_ms).map(|workspace| {
            crate::agents::spending::SpendingCaches {
                provider,
                workspace,
            }
        })
    };
    match crate::store::single_flight::coalesce(
        &runtime.shared_spending_lock(),
        SPENDING_WAIT_STEP,
        SPENDING_WAIT_STEPS,
        fresh,
    ) {
        crate::store::single_flight::Coalesced::Shared(cache) => cache,
        crate::store::single_flight::Coalesced::Produce(_guard) => {
            let provider = read_provider_spending_cache(&provider_path);
            let files = discover_spending_files();
            if provider.is_fresh(unix_now_ms())
                && let Some(workspace) = workspace_cache_from_shared_entries_inner(
                    walker,
                    runtime,
                    &provider,
                    &scope,
                    scope_hash.as_deref(),
                    &files,
                    spec,
                    &context.origin_overrides,
                    true,
                )
            {
                crate::agents::spending::SpendingCaches {
                    provider,
                    workspace,
                }
            } else {
                walk_fleet_spending_context(walker, runtime, context, spec, true, progress)
            }
        }
        crate::store::single_flight::Coalesced::ProduceLocal => {
            let provider = read_provider_spending_cache(&provider_path);
            let files = discover_spending_files();
            if provider.is_fresh(unix_now_ms())
                && let Some(workspace) = workspace_cache_from_shared_entries_inner(
                    walker,
                    runtime,
                    &provider,
                    &scope,
                    scope_hash.as_deref(),
                    &files,
                    spec,
                    &context.origin_overrides,
                    false,
                )
            {
                crate::agents::spending::SpendingCaches {
                    provider,
                    workspace,
                }
            } else if !context.allow_local_fallback {
                served_within_grace(runtime, scope_hash.as_deref()).unwrap_or_else(|| {
                    SpendingCaches {
                        provider: current_provider_spending_cache(runtime),
                        workspace: matching_workspace_cache(runtime, scope_hash.as_deref()),
                    }
                })
            } else {
                served_within_grace(runtime, scope_hash.as_deref()).unwrap_or_else(|| {
                    walk_fleet_spending_context(walker, runtime, context, spec, false, progress)
                })
            }
        }
    }
}

fn fresh_published_spending(
    runtime: &RuntimePaths,
    project_root: Option<&std::path::Path>,
    worktree_roots: &[PathBuf],
    worktree_home: Option<&std::path::Path>,
) -> Option<SpendingCaches> {
    let now_ms = unix_now_ms();
    let scope = SpendScope::for_workspace(project_root, worktree_roots, worktree_home);
    let scope_hash = (!scope.is_empty()).then(|| scope.hash());
    let provider = read_provider_spending_cache(&runtime.shared_provider_spending_path());
    if !provider.is_fresh(now_ms) {
        return None;
    }
    fresh_workspace_cache(runtime, scope_hash.as_deref(), now_ms).map(|workspace| SpendingCaches {
        provider,
        workspace,
    })
}

/// Read producer-published spending caches for a consumer fold. A missing or
/// mismatched workspace publication stays absent until the elected producer
/// publishes the current scope; consumers never open the account-global cursor.
pub(crate) fn consumer_spending_caches(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
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
        matching_workspace_cache(runtime, Some(&scope_hash))
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

fn serve_prev_on_young_regression(
    prev: WorkspaceSpendingCache,
    next: WorkspaceSpendingCache,
    now_ms: u64,
) -> WorkspaceSpendingCache {
    if prev.version == crate::agents::spending::WORKSPACE_SPENDING_VERSION
        && prev.scope_hash == next.scope_hash
        && next.tally.headline.usd < prev.tally.headline.usd
        && now_ms.saturating_sub(prev.refreshed_at_ms) < SESSION_GAP_SECS * 1_000
        && prev.headline_cutoff_secs == next.headline_cutoff_secs
        && prev.day_cutoff_secs == next.day_cutoff_secs
    {
        prev
    } else {
        next
    }
}

fn walk_fleet_spending_context(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    context: &SpendingRequestContext<'_>,
    spec: &crate::agents::spending::HeadlineSpec,
    publish: bool,
    progress: &mut dyn FnMut(SpendProgress),
) -> crate::agents::spending::SpendingCaches {
    use crate::agents::pricing;
    use crate::agents::spending::{
        PROVIDER_SPENDING_VERSION, ProviderSpendingCache, SilentWalk, SpendScope, Spending,
        SpendingCaches, WORKSPACE_SPENDING_VERSION, WalkRequest, WorkspaceSpendingCache,
        read_provider_spending_cache, unix_secs_now, write_provider_spending_cache,
        write_workspace_spending_cache,
    };

    let provider_path = runtime.shared_provider_spending_path();
    let scope = SpendScope::for_workspace(
        context.project_root,
        context.worktree_roots,
        context.worktree_home,
    );
    let scope_hash = (!scope.is_empty()).then(|| scope.hash());
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
        let user_inputs = user_input::load();
        let scoped = compute_scoped_spending(
            &files,
            &Default::default(),
            &user_inputs,
            &scope,
            unix_secs_now(),
            spec,
        );
        let workspace = WorkspaceSpendingCache {
            version: WORKSPACE_SPENDING_VERSION,
            refreshed_at_ms,
            scope_hash: scope_hash.clone().unwrap_or_default(),
            tally: scoped.tally,
            headline_cutoff_secs: scoped.headline_cutoff_secs,
            day: scoped.day,
            day_cutoff_secs: scoped.day_cutoff_secs,
            live_baselines: scoped.live_baselines,
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
                day_by_provider: Default::default(),
                day_cutoff_secs: 0,
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
    // with it. A local fallback reads the shared pricing cache without writing;
    // only the producer refreshes it while holding the spending lock.
    let now_secs = unix_secs_now();
    let prices = if publish {
        let unknowns = walker.recorded_unknown_models(&cache_path, &files, now_secs);
        Arc::new(pricing::load_for_spending(
            &runtime.shared_pricing_cache_path(),
            &unknowns,
        ))
    } else {
        pricing::cached_book(&runtime.shared_pricing_cache_path())
    };
    let user_inputs = user_input::load();
    let req = WalkRequest {
        files: &files,
        prices: &prices,
        now_secs,
        origin_overrides: &context.origin_overrides,
        user_inputs: &user_inputs,
        scope: Some(&scope),
        spec,
    };
    let result = if publish {
        let mut observer = PublishingWalkObserver {
            runtime,
            provider_path: provider_path.clone(),
            files: &files,
            user_inputs: &user_inputs,
            now_secs,
            scope: Some(&scope),
            scope_hash: scope_hash.clone(),
            spec,
            progress,
        };
        walker.walk(&cache_path, &req, &mut observer)
    } else {
        let mut observer = SilentWalk;
        walker.walk_local(&cache_path, &req, &mut observer)
    };
    let refreshed_at_ms = unix_now_ms();
    let workspace = if let Some(scope_hash) = scope_hash.as_deref() {
        reconciled_workspace_cache(
            scope_hash,
            refreshed_at_ms,
            result.workspace_tally.clone(),
            result.workspace_headline_cutoff_secs,
            result.workspace_day,
            result.day_cutoff_secs,
            result.workspace_live_baselines,
        )
    } else {
        WorkspaceSpendingCache {
            version: WORKSPACE_SPENDING_VERSION,
            refreshed_at_ms,
            ..Default::default()
        }
    };
    if publish {
        crate::agents::spending::write_provider_spending_cache_with_day(
            &provider_path,
            refreshed_at_ms,
            &result.spending,
            &result.days,
            &result.models,
            &result.provider_day,
            result.day_cutoff_secs,
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
            day_by_provider: result.provider_day,
            day_cutoff_secs: result.day_cutoff_secs,
            days: result.days,
            models: result.models,
            spending: result.spending,
        },
        workspace,
    }
}

#[cfg(test)]
fn walk_fleet_spending(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    spec: &crate::agents::spending::HeadlineSpec,
    publish: bool,
) -> SpendingCaches {
    let context = SpendingRequestContext {
        project_root: snapshot.project_root.as_deref(),
        worktree_roots: &snapshot.worktree_roots,
        worktree_home: snapshot.worktree_home.as_deref(),
        origin_overrides: codex_origin_overrides(snapshot),
        allow_local_fallback: true,
    };
    walk_fleet_spending_context(walker, runtime, &context, spec, publish, &mut |_| {})
}

struct PublishingWalkObserver<'a> {
    runtime: &'a RuntimePaths,
    provider_path: PathBuf,
    files: &'a [(&'static dyn crate::agents::AgentAdapter, PathBuf)],
    user_inputs: &'a [crate::agents::spending::user_input::UserInputRecord],
    now_secs: u64,
    scope: Option<&'a crate::agents::spending::SpendScope>,
    scope_hash: Option<String>,
    spec: &'a crate::agents::spending::HeadlineSpec,
    progress: &'a mut dyn FnMut(SpendProgress),
}

impl crate::agents::spending::WalkObserver for PublishingWalkObserver<'_> {
    fn on_file(&mut self, progress: SpendProgress) {
        (self.progress)(progress);
    }

    fn on_interval(&mut self, cache: &crate::agents::spending::SpendingDiskCache) {
        let result = crate::agents::spending::aggregate_walk_publish(
            self.files,
            cache,
            self.user_inputs,
            self.now_secs,
            self.scope,
            self.spec,
        );
        let refreshed_at_ms = unix_now_ms();
        crate::agents::spending::write_provider_spending_cache_with_day(
            &self.provider_path,
            refreshed_at_ms,
            &result.spending,
            &result.days,
            &result.models,
            &result.provider_day,
            result.day_cutoff_secs,
        );
        if let Some(scope_hash) = self.scope_hash.as_deref() {
            let workspace = reconciled_workspace_cache(
                scope_hash,
                refreshed_at_ms,
                result.workspace_tally,
                result.workspace_headline_cutoff_secs,
                result.workspace_day,
                result.day_cutoff_secs,
                result.workspace_live_baselines,
            );
            crate::agents::spending::write_workspace_spending_cache(
                &self.runtime.workspace_spending_path(scope_hash),
                &workspace,
            );
            prune_workspace_spending_siblings(self.runtime, scope_hash);
        }
    }
}

fn reconciled_workspace_cache(
    scope_hash: &str,
    refreshed_at_ms: u64,
    tally: crate::agents::spending::SpendTally,
    headline_cutoff_secs: u64,
    day: crate::agents::spending::SpendWindow,
    day_cutoff_secs: u64,
    live_baselines: BTreeMap<String, f64>,
) -> crate::agents::spending::WorkspaceSpendingCache {
    use crate::agents::spending::{WORKSPACE_SPENDING_VERSION, WorkspaceSpendingCache};

    WorkspaceSpendingCache {
        version: WORKSPACE_SPENDING_VERSION,
        refreshed_at_ms,
        scope_hash: scope_hash.to_owned(),
        tally,
        headline_cutoff_secs,
        day,
        day_cutoff_secs,
        live_baselines,
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "scope derivation keeps caller-validated paths, publication policy, and warm owner explicit"
)]
fn workspace_cache_from_shared_entries_inner(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    provider: &crate::agents::spending::ProviderSpendingCache,
    scope: &crate::agents::spending::SpendScope,
    scope_hash: Option<&str>,
    files: &[(&'static dyn crate::agents::AgentAdapter, PathBuf)],
    spec: &crate::agents::spending::HeadlineSpec,
    origin_overrides: &HashMap<PathBuf, PathBuf>,
    publish: bool,
) -> Option<crate::agents::spending::WorkspaceSpendingCache> {
    use crate::agents::spending::{unix_secs_now, write_workspace_spending_cache};
    let Some(scope_hash) = scope_hash else {
        return Some(Default::default());
    };
    walker.apply_origin_overrides(
        &runtime.shared_spending_cursor_path(),
        origin_overrides,
        publish,
        unix_secs_now(),
    );
    let user_inputs = user_input::load();
    let cached = walker.scoped_from_cache(
        &runtime.shared_spending_cursor_path(),
        files,
        &user_inputs,
        scope,
        unix_secs_now(),
        spec,
    );
    if !provider.spending.total.is_zero() && !cached.has_discovered_file {
        return None;
    }
    let scoped = cached.scoped;
    let workspace = reconciled_workspace_cache(
        scope_hash,
        provider.refreshed_at_ms,
        scoped.tally,
        scoped.headline_cutoff_secs,
        scoped.day,
        scoped.day_cutoff_secs,
        scoped.live_baselines,
    );
    let workspace = serve_prev_on_young_regression(
        matching_workspace_cache(runtime, Some(scope_hash)),
        workspace,
        unix_now_ms(),
    );
    if workspace.refreshed_at_ms != provider.refreshed_at_ms {
        return Some(workspace);
    }
    if publish {
        write_workspace_spending_cache(&runtime.workspace_spending_path(scope_hash), &workspace);
        prune_workspace_spending_siblings(runtime, scope_hash);
    }
    Some(workspace)
}

#[cfg(test)]
fn workspace_cache_from_shared_entries(
    walker: &mut crate::agents::spending::SpendingWalker,
    runtime: &RuntimePaths,
    provider: &crate::agents::spending::ProviderSpendingCache,
    scope: &crate::agents::spending::SpendScope,
    scope_hash: Option<&str>,
    files: &[(&'static dyn crate::agents::AgentAdapter, PathBuf)],
    spec: &crate::agents::spending::HeadlineSpec,
) -> Option<crate::agents::spending::WorkspaceSpendingCache> {
    workspace_cache_from_shared_entries_inner(
        walker,
        runtime,
        provider,
        scope,
        scope_hash,
        files,
        spec,
        &HashMap::new(),
        true,
    )
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
