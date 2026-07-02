use std::collections::BTreeMap;
use std::path::Path;

use fs4::FileExt;
use serde::{Deserialize, Serialize};

use crate::agents::ExtraCredits;
use crate::config::AccountsConfig;
use crate::sidebar::timing::unix_now_ms;
use crate::sidebar::timing::{CREDITS_DISPLAY_MAX_AGE, CREDITS_RETRY_TTL, CREDITS_TTL};
use crate::{RuntimePaths, SidebarSnapshot};

/// Shared provider extra-credits cache, keyed by agent kind.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct CreditsCache {
    pub refreshed_at_ms: u64,
    pub entries: BTreeMap<String, ProviderCreditsEntry>,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCreditsEntry {
    pub observed_at_ms: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_credits: Option<ExtraCredits>,
}

pub(crate) fn read_credits_cache(path: &Path) -> CreditsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn write_credits_cache(path: &Path, cache: &CreditsCache) {
    if let Err(err) = crate::ledger::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(
            path = %path.display(),
            tags.operation = "cache.credits_write",
            error = &err as &dyn std::error::Error,
            "sidebar credits cache write failed",
        );
    }
}

/// Merge one provider's paid-usage reading into the shared cache. Best-effort:
/// another producer may win the lock first, and the next due helper converges.
pub fn merge_provider_credits(
    runtime: &RuntimePaths,
    kind: &str,
    extra_credits: Option<ExtraCredits>,
) {
    merge_provider_credits_entry(
        runtime,
        kind,
        ProviderCreditsEntry {
            observed_at_ms: unix_now_ms(),
            ok: extra_credits.is_some(),
            extra_credits,
        },
    );
}

/// Merge one provider's entry only when the existing success/failure entry is
/// past its TTL. The freshness check happens under the shared lock, so detached
/// helpers in multiple workspaces single-flight the network fetch.
pub fn merge_provider_credits_entry_if_due(
    runtime: &RuntimePaths,
    kind: &str,
    entry: impl FnOnce() -> ProviderCreditsEntry,
) -> Option<ProviderCreditsEntry> {
    let path = runtime.shared_credits_path();
    let _guard = try_credits_cache_lock(&runtime.shared_credits_lock())?;
    let mut cache = read_credits_cache(&path);
    let now_ms = unix_now_ms();
    if cache
        .entries
        .get(kind)
        .is_some_and(|entry| entry_is_fresh(entry, now_ms))
    {
        return None;
    }
    let entry = entry();
    cache.refreshed_at_ms = unix_now_ms();
    cache.entries.insert(kind.to_owned(), entry.clone());
    write_credits_cache(&path, &cache);
    Some(entry)
}

pub fn provider_credits_entry_fresh(runtime: &RuntimePaths, kind: &str) -> bool {
    let cache = read_credits_cache(&runtime.shared_credits_path());
    let now_ms = unix_now_ms();
    cache
        .entries
        .get(kind)
        .is_some_and(|entry| entry_is_fresh(entry, now_ms))
}

pub(crate) fn merge_provider_credits_entry(
    runtime: &RuntimePaths,
    kind: &str,
    entry: ProviderCreditsEntry,
) {
    let path = runtime.shared_credits_path();
    let Some(_guard) = try_credits_cache_lock(&runtime.shared_credits_lock()) else {
        return;
    };
    let mut cache = read_credits_cache(&path);
    cache.refreshed_at_ms = unix_now_ms();
    cache.entries.insert(kind.to_owned(), entry);
    write_credits_cache(&path, &cache);
}

pub(crate) fn entry_is_fresh(entry: &ProviderCreditsEntry, now_ms: u64) -> bool {
    let ttl = if entry.ok {
        CREDITS_TTL
    } else {
        CREDITS_RETRY_TTL
    };
    now_ms.saturating_sub(entry.observed_at_ms) <= ttl.as_millis() as u64
}

fn entry_is_displayable(entry: &ProviderCreditsEntry, now_ms: u64) -> bool {
    entry.ok
        && now_ms.saturating_sub(entry.observed_at_ms) <= CREDITS_DISPLAY_MAX_AGE.as_millis() as u64
}

fn try_credits_cache_lock(path: &Path) -> Option<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .ok()?;
    FileExt::try_lock(&file).ok()?;
    Some(file)
}

pub(crate) fn apply_credits_cache(
    snapshot: &mut SidebarSnapshot,
    runtime: &RuntimePaths,
    accounts: &AccountsConfig,
) {
    if snapshot.providers.is_empty() {
        return;
    }
    let cache = read_credits_cache(&runtime.shared_credits_path());
    apply_credits_cache_with(snapshot, &cache, accounts, unix_now_ms());
}

fn apply_credits_cache_with(
    snapshot: &mut SidebarSnapshot,
    cache: &CreditsCache,
    accounts: &AccountsConfig,
    now_ms: u64,
) {
    for panel in &mut snapshot.providers {
        let ceiling = accounts.usage_limit(&panel.kind);
        if panel.metered {
            panel.extra_credits = cache
                .entries
                .get(&panel.kind)
                .filter(|entry| entry_is_displayable(entry, now_ms))
                .and_then(|entry| entry.extra_credits.clone())
                .map(|credits| credits.with_limit_if_missing(ceiling))
                .or_else(|| ceiling.map(|limit| ExtraCredits::known(None, None, Some(limit))));
            continue;
        }

        let used = panel.spending.as_ref().map(|spending| spending.month.usd);
        panel.extra_credits = Some(ExtraCredits::known(used, None, ceiling));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::SpendTally;
    use crate::ids::WorkspaceId;
    use crate::{SidebarProviderPanel, SpendWindow};

    fn panel(kind: &str, metered: bool) -> SidebarProviderPanel {
        SidebarProviderPanel {
            kind: kind.to_owned(),
            product_name: kind.to_owned(),
            art: Vec::new(),
            color: 1,
            color_rgb: None,
            color_role: None,
            version: None,
            plan: None,
            metered,
            remote_control: false,
            spending: Some(SpendTally {
                month: SpendWindow {
                    usd: 12.5,
                    ..Default::default()
                },
                ..Default::default()
            }),
            extra_credits: None,
            windows: Vec::new(),
        }
    }

    #[test]
    fn freshness_uses_success_and_failure_ttls() {
        let now = CREDITS_TTL.as_millis() as u64 + 10_000;
        assert!(entry_is_fresh(
            &ProviderCreditsEntry {
                observed_at_ms: now - 1_000,
                ok: true,
                extra_credits: None,
            },
            now
        ));
        assert!(!entry_is_fresh(
            &ProviderCreditsEntry {
                observed_at_ms: now - CREDITS_TTL.as_millis() as u64 - 1,
                ok: true,
                extra_credits: None,
            },
            now
        ));
        assert!(!entry_is_fresh(
            &ProviderCreditsEntry {
                observed_at_ms: now - CREDITS_RETRY_TTL.as_millis() as u64 - 1,
                ok: false,
                extra_credits: None,
            },
            now
        ));
    }

    #[test]
    fn fold_applies_cached_credits_and_api_spend_ceiling() {
        let mut snapshot = SidebarSnapshot::build(
            crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            Vec::new(),
            Vec::new(),
            jiff::Timestamp::from_second(1_700_000_000).unwrap(),
        );
        snapshot.providers = vec![panel("claude", true), panel("codex", false)];
        let mut cache = CreditsCache::default();
        cache.entries.insert(
            "claude".to_owned(),
            ProviderCreditsEntry {
                observed_at_ms: 100,
                ok: true,
                extra_credits: Some(ExtraCredits::known(Some(7.0), None, None)),
            },
        );
        let mut accounts = AccountsConfig::default();
        accounts.usage_limit_usd.insert(
            "claude".to_owned(),
            crate::config::UsageLimitUsd::from_usd(50.0),
        );
        accounts.usage_limit_usd.insert(
            "codex".to_owned(),
            crate::config::UsageLimitUsd::from_usd(25.0),
        );

        apply_credits_cache_with(&mut snapshot, &cache, &accounts, 100);
        assert_eq!(
            snapshot.providers[0].extra_credits,
            Some(ExtraCredits::known(Some(7.0), None, Some(50.0)))
        );
        assert_eq!(
            snapshot.providers[1].extra_credits,
            Some(ExtraCredits::known(Some(12.5), None, Some(25.0)))
        );
    }

    #[test]
    fn cache_merge_preserves_other_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime");
        runtime.ensure_dirs().unwrap();
        merge_provider_credits(
            &runtime,
            "claude",
            Some(ExtraCredits::known(None, None, None)),
        );
        merge_provider_credits(
            &runtime,
            "codex",
            Some(ExtraCredits::known(None, Some(5.0), None)),
        );
        let cache = read_credits_cache(&runtime.shared_credits_path());
        assert!(cache.entries.contains_key("claude"));
        assert!(cache.entries.contains_key("codex"));
    }
}
