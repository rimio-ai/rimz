//! The fleet spending walk: the shared `SPENDING_TTL`-gated transcript-history
//! walk feeding the enrichment spine's global `value_tally`, per-workspace
//! `workspace_value_tally`, and per-provider dashboard folds.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::agents::spending::discover_spending_files;
use crate::sidebar::cache::unix_now_ms;
use crate::{RuntimePaths, SidebarSnapshot};

const SPENDING_WAIT_STEP: Duration = Duration::from_millis(20);
const SPENDING_WAIT_STEPS: u32 = 15;

/// Walk every provider's transcript history into a fleet-wide and per-provider
/// [`Spending`](crate::agents::spending::Spending), publishing the stamped
/// result to the shared `provider-spending.json` — the cache consumer tabs read
/// instead of walking, and the producer's own global gate: a stamp younger than
/// [`SPENDING_TTL`](crate::agents::spending::SPENDING_TTL) serves the published
/// provider totals without a global transcript walk or price-book load.
///
/// The stale walk is single-flighted across every workspace for this user. The
/// elected producer reads the shared `spending.json` cache, refreshes only files
/// whose mtime changed, writes it back if anything was updated, and loads the
/// shared price book (a TTL-gated remote refresh) so Codex's token counts become
/// dollars. When only this room's workspace tally is missing or stale, the
/// producer derives it read-only from the shared `spending.json` entry cache
/// instead of joining the global walk election. A timeout fallback computes an
/// uncached in-memory result for its own frame and leaves the shared files to
/// the elected producer.
///
/// Every registered adapter is discovered fleet-wide
/// ([`transcript_files`](crate::agents::AgentAdapter::transcript_files)) so each
/// counts on the same footing, and the dashboard panel and fleet ledger read
/// one provider's spend the same way regardless of which project it ran in.
pub(super) fn compute_fleet_spending(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
) -> crate::agents::spending::SpendingCaches {
    use crate::agents::spending::{SpendScope, read_provider_spending_cache};

    let now_ms = unix_now_ms();
    let scope = SpendScope::from_roots(snapshot.project_root.as_deref(), &snapshot.worktree_roots);
    let scope_hash = (!scope.is_empty()).then(|| scope.hash());
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
            true,
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
            true,
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
            walk_fleet_spending(runtime, snapshot, true)
        }
        crate::ledger::single_flight::Coalesced::ProduceLocal => {
            walk_fleet_spending(runtime, snapshot, false)
        }
    }
}

fn walk_fleet_spending(
    runtime: &RuntimePaths,
    snapshot: &SidebarSnapshot,
    publish: bool,
) -> crate::agents::spending::SpendingCaches {
    use crate::agents::pricing;
    use crate::agents::spending::{
        PROVIDER_SPENDING_VERSION, ProviderSpendingCache, SpendScope, Spending, SpendingCaches,
        WORKSPACE_SPENDING_VERSION, WorkspaceSpendingCache,
        compute_spending_with_origins_and_scope, read_spending_cache, unix_secs_now,
        write_provider_spending_cache, write_spending_cache, write_workspace_spending_cache,
    };

    let provider_path = runtime.shared_provider_spending_path();
    let scope = SpendScope::from_roots(snapshot.project_root.as_deref(), &snapshot.worktree_roots);
    let scope_hash = (!scope.is_empty()).then(|| scope.hash());
    // Tag each file with its adapter at discovery — the source knows the kind,
    // so pricing/bucketing never has to guess it from the path.
    let files = discover_spending_files();
    if files.is_empty() {
        let refreshed_at_ms = unix_now_ms();
        let spending = Spending::default();
        let workspace = WorkspaceSpendingCache {
            version: WORKSPACE_SPENDING_VERSION,
            refreshed_at_ms,
            scope_hash: scope_hash.clone().unwrap_or_default(),
            tally: Default::default(),
        };
        if publish {
            // Stamp the empty result too: an agentless machine must not re-run
            // the (empty) discovery readdirs every tick.
            write_provider_spending_cache(&provider_path, refreshed_at_ms, &spending);
            if let Some(scope_hash) = scope_hash.as_deref() {
                write_workspace_spending_cache(
                    &runtime.workspace_spending_path(scope_hash),
                    refreshed_at_ms,
                    scope_hash,
                    &workspace.tally,
                );
                prune_workspace_spending_siblings(runtime, scope_hash);
            }
        }
        return SpendingCaches {
            provider: ProviderSpendingCache {
                version: PROVIDER_SPENDING_VERSION,
                refreshed_at_ms,
                spending,
            },
            workspace,
        };
    }

    let cache_path = runtime.shared_spending_cursor_path();
    let mut cache = if publish {
        read_spending_cache(&cache_path)
    } else {
        Default::default()
    };
    // The price book exists only to price the walk, so its load (and TTL-gated
    // remote refresh, including the unknown-model chase) rides the stale arm
    // with it. A local fallback uses the embedded table so it never writes the
    // shared pricing cache without the spending lock.
    let now_secs = unix_secs_now();
    let prices = if publish {
        let unknowns = crate::agents::spending::recorded_unknown_models(&files, &cache, now_secs);
        pricing::load_for_spending(&runtime.shared_pricing_cache_path(), &unknowns)
    } else {
        pricing::PriceBook::embedded()
    };
    let origin_overrides = codex_origin_overrides(snapshot);
    let (spending, workspace_tally) = compute_spending_with_origins_and_scope(
        &files,
        &mut cache,
        &prices,
        now_secs,
        &origin_overrides,
        Some(&scope),
    );
    let refreshed_at_ms = unix_now_ms();
    if publish && cache.dirty {
        write_spending_cache(&cache_path, &cache);
    }
    if publish {
        write_provider_spending_cache(&provider_path, refreshed_at_ms, &spending);
        if let Some(scope_hash) = scope_hash.as_deref() {
            write_workspace_spending_cache(
                &runtime.workspace_spending_path(scope_hash),
                refreshed_at_ms,
                scope_hash,
                &workspace_tally,
            );
            prune_workspace_spending_siblings(runtime, scope_hash);
        }
    }
    SpendingCaches {
        provider: ProviderSpendingCache {
            version: PROVIDER_SPENDING_VERSION,
            refreshed_at_ms,
            spending,
        },
        workspace: WorkspaceSpendingCache {
            version: WORKSPACE_SPENDING_VERSION,
            refreshed_at_ms,
            scope_hash: scope_hash.unwrap_or_default(),
            tally: workspace_tally,
        },
    }
}

fn workspace_cache_from_shared_entries(
    runtime: &RuntimePaths,
    provider: &crate::agents::spending::ProviderSpendingCache,
    scope: &crate::agents::spending::SpendScope,
    scope_hash: Option<&str>,
    files: &[(&'static dyn crate::agents::AgentAdapter, PathBuf)],
    publish: bool,
) -> Option<crate::agents::spending::WorkspaceSpendingCache> {
    use crate::agents::spending::{
        WORKSPACE_SPENDING_VERSION, WorkspaceSpendingCache, compute_scoped_tally,
        read_spending_cache, unix_secs_now, write_workspace_spending_cache,
    };
    let Some(scope_hash) = scope_hash else {
        return Some(Default::default());
    };
    let cache = read_spending_cache(&runtime.shared_spending_cursor_path());
    let has_discovered_cache = files.iter().any(|(_, file)| {
        cache
            .files
            .contains_key(&file.to_string_lossy().into_owned())
    });
    if !provider.spending.total.is_zero() && !has_discovered_cache {
        return None;
    }
    let tally = compute_scoped_tally(files, &cache, scope, unix_secs_now());
    let workspace = WorkspaceSpendingCache {
        version: WORKSPACE_SPENDING_VERSION,
        refreshed_at_ms: provider.refreshed_at_ms,
        scope_hash: scope_hash.to_owned(),
        tally,
    };
    if publish {
        write_workspace_spending_cache(
            &runtime.workspace_spending_path(scope_hash),
            workspace.refreshed_at_ms,
            scope_hash,
            &workspace.tally,
        );
        prune_workspace_spending_siblings(runtime, scope_hash);
    }
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
mod tests {
    use crate::RuntimePaths;
    use crate::SidebarSnapshot;
    use crate::agents::TurnPhase;
    use crate::agents::spending::{
        CachedEntry, FileCacheEntry, PROVIDER_SPENDING_VERSION, ProviderSpendingCache, SpendCursor,
        SpendScope, Spending, read_spending_cache, read_workspace_spending_cache, unix_secs_now,
        write_provider_spending_cache, write_spending_cache,
    };
    use crate::feed::{AgentState, AgentStatus};
    use crate::ids::AgentKind;
    use crate::ids::WorkspaceId;
    use crate::ledger::single_flight::{Coalesced, coalesce};
    use crate::sidebar::cache::unix_now_ms;

    use jiff::Timestamp;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use super::{
        codex_origin_overrides, compute_fleet_spending, workspace_cache_from_shared_entries,
    };

    #[test]
    fn fresh_shared_publish_returns_without_walking() {
        let dir = tempfile::tempdir().unwrap();
        let first = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime paths");
        let second = RuntimePaths::under(
            WorkspaceId::from_project_root(&dir.path().join("other")),
            dir.path(),
        )
        .expect("runtime paths");
        first.ensure_dirs().expect("runtime dirs");
        second.ensure_dirs().expect("runtime dirs");
        let published_at = unix_now_ms();
        let mut spending = Spending::default();
        spending.total.today.usd = 1.23;
        write_provider_spending_cache(
            &first.shared_provider_spending_path(),
            published_at,
            &spending,
        );
        let snapshot = SidebarSnapshot::build(
            second.workspace_id.clone(),
            Vec::new(),
            Vec::new(),
            Timestamp::now(),
        );

        let cache = compute_fleet_spending(&second, &snapshot);

        assert_eq!(cache.provider.refreshed_at_ms, published_at);
        assert_eq!(cache.provider.spending, spending);
    }

    #[test]
    fn shared_spending_lock_serves_the_elected_publish_to_a_contender() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime paths");
        runtime.ensure_dirs().expect("runtime dirs");

        let guard = match coalesce::<crate::agents::spending::ProviderSpendingCache>(
            &runtime.shared_spending_lock(),
            Duration::ZERO,
            1,
            || None,
        ) {
            Coalesced::Produce(guard) => guard,
            _ => panic!("first contender must hold the shared spending election"),
        };

        let published_at = unix_now_ms();
        let mut spending = Spending::default();
        spending.total.today.usd = 4.56;
        let polls = AtomicU32::new(0);
        let outcome = coalesce(&runtime.shared_spending_lock(), Duration::ZERO, 3, || {
            if polls.fetch_add(1, Ordering::SeqCst) == 1 {
                write_provider_spending_cache(
                    &runtime.shared_provider_spending_path(),
                    published_at,
                    &spending,
                );
            }
            let cache = crate::agents::spending::read_provider_spending_cache(
                &runtime.shared_provider_spending_path(),
            );
            cache.is_fresh(unix_now_ms()).then_some(cache)
        });

        drop(guard);
        let Coalesced::Shared(cache) = outcome else {
            panic!("a contender must consume the elected producer's publish");
        };
        assert_eq!(cache.refreshed_at_ms, published_at);
        assert_eq!(cache.spending, spending);
    }

    #[test]
    fn workspace_cache_derives_from_shared_entries_while_global_lock_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let other_project = dir.path().join("other");
        let runtime =
            RuntimePaths::under(WorkspaceId::from_project_root(&other_project), dir.path())
                .expect("runtime paths");
        runtime.ensure_dirs().expect("runtime dirs");

        let project = dir.path().join("repo");
        let scope = SpendScope::from_roots(Some(&project), &[]);
        let scope_hash = scope.hash();
        let transcript = dir.path().join("claude.jsonl");
        let now_secs = unix_secs_now();
        let mut raw = read_spending_cache(&runtime.shared_spending_cursor_path());
        raw.files.insert(
            transcript.to_string_lossy().into_owned(),
            FileCacheEntry {
                mtime_secs: 1,
                len: 1,
                cursor: SpendCursor::default(),
                origin_path: None,
                entries: vec![CachedEntry {
                    ts_secs: now_secs,
                    cost_usd: 1.25,
                    input: 10,
                    output: 5,
                    cache_write: 0,
                    cache_read: 0,
                    message_id: Some("msg-1".to_owned()),
                    request_id: Some("req-1".to_owned()),
                    is_sidechain: false,
                    origin_path: Some(project.clone()),
                }],
                unknown_models: BTreeMap::new(),
            },
        );
        write_spending_cache(&runtime.shared_spending_cursor_path(), &raw);

        let mut spending = Spending::default();
        spending.total.today.usd = 1.25;
        spending.total.year.usd = 1.25;
        let provider = ProviderSpendingCache {
            version: PROVIDER_SPENDING_VERSION,
            refreshed_at_ms: unix_now_ms(),
            spending,
        };

        let _held =
            match coalesce::<()>(&runtime.shared_spending_lock(), Duration::ZERO, 1, || None) {
                Coalesced::Produce(guard) => guard,
                _ => panic!("test must hold the global spending lock"),
            };
        let files = vec![(
            &crate::agents::ClaudeAdapter as &'static dyn crate::agents::AgentAdapter,
            transcript,
        )];
        let stale_hash = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        crate::agents::spending::write_workspace_spending_cache(
            &runtime.workspace_spending_path(stale_hash),
            unix_now_ms(),
            stale_hash,
            &Default::default(),
        );
        assert!(runtime.workspace_spending_path(stale_hash).exists());

        let workspace = workspace_cache_from_shared_entries(
            &runtime,
            &provider,
            &scope,
            Some(&scope_hash),
            &files,
            true,
        )
        .expect("workspace cache derives from the shared cursor cache");

        assert!((workspace.tally.today.usd - 1.25).abs() < 1e-9);
        assert_eq!(
            read_workspace_spending_cache(&runtime.workspace_spending_path(&scope_hash)).tally,
            workspace.tally
        );
        assert!(
            !runtime.workspace_spending_path(stale_hash).exists(),
            "producer publishing the current scope prunes old per-hash workspace caches"
        );
    }

    #[test]
    fn codex_origin_overrides_read_transcript_and_worktree_from_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("rollout.jsonl");
        let worktree = dir.path().join("repo");
        let now = Timestamp::now();
        let agent = AgentState {
            agent_id: "codex-1".into(),
            kind: AgentKind::new_unchecked("codex"),
            name: None,
            kind_ordinal: None,
            status: AgentStatus::Running,
            phase: TurnPhase::Idle,
            pane: None,
            agent_pid: None,
            agent_process_start: None,
            runtime_owner: None,
            parent_agent_id: None,
            worktree_path: Some(worktree.display().to_string()),
            worktree_branch: None,
            task: None,
            prompt: None,
            transcript_path: Some(transcript.display().to_string()),
            recent_prompts: Vec::new(),
            model: None,
            effort: None,
            context_pct: None,
            context_window: None,
            total_tokens: None,
            cache_read_input_tokens: None,
            fresh_input_tokens: None,
            output_tokens: None,
            todo_done: None,
            todo_total: None,
            context: None,
            subagent_description: None,
            subagent_started_at: None,
            turn_started_at: None,
            compacting_since: None,
            compaction_count: 0,
            last_seen: now,
            last_activity: now,
            registered_at: Some(now),
        };
        let snapshot = SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(&worktree),
            Vec::new(),
            vec![agent],
            now,
        );

        let overrides = codex_origin_overrides(&snapshot);

        assert_eq!(overrides.get(&transcript), Some(&worktree));
    }
}
