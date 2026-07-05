use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agents::{ExtraCredits, ResetCredits};
use crate::config::AccountsConfig;
use crate::sidebar::timing::unix_now_ms;
use crate::sidebar::timing::{CREDITS_DISPLAY_MAX_AGE, OAUTH_USAGE_TTL};
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
    /// Last OAuth account-usage attempt. App-server/realtime credit writes
    /// preserve this stamp and never advance the OAuth cadence.
    #[serde(default)]
    pub oauth_read_at_ms: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_credits: Option<ExtraCredits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<ResetCredits>,
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
            oauth_read_at_ms: 0,
            ok: extra_credits.is_some(),
            extra_credits,
            reset_credits: None,
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
    let prior = cache.entries.get(kind).cloned();
    if prior
        .as_ref()
        .is_some_and(|entry| oauth_read_is_fresh(entry, now_ms))
    {
        return None;
    }
    let mut entry = entry();
    if entry.oauth_read_at_ms == 0 {
        entry.oauth_read_at_ms = unix_now_ms();
    }
    if !entry.ok {
        if let Some(prior) = prior.as_ref() {
            entry.observed_at_ms = prior.observed_at_ms;
            entry.ok = prior.ok;
            entry.extra_credits = prior.extra_credits.clone();
            entry.reset_credits = prior.reset_credits.clone();
        }
    } else if entry.reset_credits.is_none() {
        // Reset credits are Codex OAuth-only; app-server, extra-credits-only,
        // and failed writes must not erase the last successful reset read.
        entry.reset_credits = prior.as_ref().and_then(|entry| entry.reset_credits.clone());
    }
    cache.refreshed_at_ms = unix_now_ms();
    cache.entries.insert(kind.to_owned(), entry.clone());
    write_credits_cache(&path, &cache);
    Some(entry)
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
    let mut entry = entry;
    let prior = cache.entries.get(kind);
    if entry.oauth_read_at_ms == 0 {
        entry.oauth_read_at_ms = prior.map_or(0, |entry| entry.oauth_read_at_ms);
    }
    if entry.reset_credits.is_none() {
        // Reset credits are Codex OAuth-only; app-server, extra-credits-only,
        // and failed writes must not erase the last successful reset read.
        entry.reset_credits = prior.and_then(|entry| entry.reset_credits.clone());
    }
    cache.refreshed_at_ms = unix_now_ms();
    cache.entries.insert(kind.to_owned(), entry);
    write_credits_cache(&path, &cache);
}

pub fn invalidate_oauth_read(runtime: &RuntimePaths, kind: &str) {
    let path = runtime.shared_credits_path();
    let Some(_guard) = try_credits_cache_lock(&runtime.shared_credits_lock()) else {
        return;
    };
    let mut cache = read_credits_cache(&path);
    let Some(entry) = cache.entries.get_mut(kind) else {
        return;
    };
    entry.oauth_read_at_ms = 0;
    cache.refreshed_at_ms = unix_now_ms();
    write_credits_cache(&path, &cache);
}

fn oauth_read_is_fresh(entry: &ProviderCreditsEntry, now_ms: u64) -> bool {
    entry.oauth_read_at_ms != 0
        && now_ms.saturating_sub(entry.oauth_read_at_ms) <= OAUTH_USAGE_TTL.as_millis() as u64
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
    file.try_lock().ok()?;
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
            panel.reset_credits = cache
                .entries
                .get(&panel.kind)
                .filter(|entry| entry_is_displayable(entry, now_ms))
                .and_then(|entry| entry.reset_credits.clone());
            continue;
        }

        let used = panel.spending.as_ref().map(|spending| spending.month.usd);
        panel.extra_credits = Some(ExtraCredits::known(used, None, ceiling));
        panel.reset_credits = None;
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
            reset_credits: None,
            windows: Vec::new(),
        }
    }

    #[test]
    fn oauth_read_stamp_throttles_oauth_probe() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime");
        runtime.ensure_dirs().unwrap();
        let path = runtime.shared_credits_path();
        let now = unix_now_ms();
        write_credits_cache(
            &path,
            &CreditsCache {
                refreshed_at_ms: now,
                entries: BTreeMap::from([(
                    "codex".to_owned(),
                    ProviderCreditsEntry {
                        observed_at_ms: 10,
                        oauth_read_at_ms: now,
                        ok: true,
                        extra_credits: Some(ExtraCredits::known(None, Some(1.0), None)),
                        reset_credits: None,
                    },
                )]),
            },
        );

        let mut calls = 0;
        assert!(
            merge_provider_credits_entry_if_due(&runtime, "codex", || {
                calls += 1;
                ProviderCreditsEntry {
                    observed_at_ms: unix_now_ms(),
                    oauth_read_at_ms: unix_now_ms(),
                    ok: true,
                    extra_credits: Some(ExtraCredits::known(None, Some(2.0), None)),
                    reset_credits: None,
                }
            })
            .is_none()
        );
        assert_eq!(calls, 0, "fresh OAuth attempt stamp skips the fetch");

        let stale = unix_now_ms() - OAUTH_USAGE_TTL.as_millis() as u64 - 1;
        let mut cache = read_credits_cache(&path);
        cache.entries.get_mut("codex").unwrap().oauth_read_at_ms = stale;
        write_credits_cache(&path, &cache);

        let mut calls = 0;
        assert!(
            merge_provider_credits_entry_if_due(&runtime, "codex", || {
                calls += 1;
                ProviderCreditsEntry {
                    observed_at_ms: unix_now_ms(),
                    oauth_read_at_ms: unix_now_ms(),
                    ok: true,
                    extra_credits: Some(ExtraCredits::known(None, Some(2.0), None)),
                    reset_credits: None,
                }
            })
            .is_some()
        );
        assert_eq!(calls, 1, "stale OAuth attempt stamp allows the fetch");
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
                oauth_read_at_ms: 0,
                ok: true,
                extra_credits: Some(ExtraCredits::known(Some(7.0), None, None)),
                reset_credits: None,
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

    #[test]
    fn extra_credits_only_merge_preserves_prior_reset_credits() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime");
        runtime.ensure_dirs().unwrap();
        let reset_credits = ResetCredits {
            count: 2,
            soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
        };

        merge_provider_credits_entry(
            &runtime,
            "codex",
            ProviderCreditsEntry {
                observed_at_ms: 1,
                oauth_read_at_ms: 1234,
                ok: true,
                extra_credits: None,
                reset_credits: Some(reset_credits.clone()),
            },
        );
        merge_provider_credits(
            &runtime,
            "codex",
            Some(ExtraCredits::known(None, Some(5.0), None)),
        );

        let cache = read_credits_cache(&runtime.shared_credits_path());
        assert_eq!(
            cache
                .entries
                .get("codex")
                .and_then(|entry| entry.reset_credits.clone()),
            Some(reset_credits)
        );
        assert_eq!(
            cache
                .entries
                .get("codex")
                .map(|entry| entry.oauth_read_at_ms),
            Some(1234)
        );
    }

    #[test]
    fn failed_oauth_merge_preserves_prior_displayable_credits() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime");
        runtime.ensure_dirs().unwrap();
        let reset_credits = ResetCredits {
            count: 4,
            soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
        };
        merge_provider_credits_entry(
            &runtime,
            "codex",
            ProviderCreditsEntry {
                observed_at_ms: 42,
                oauth_read_at_ms: 1,
                ok: true,
                extra_credits: Some(ExtraCredits::known(None, Some(5.0), None)),
                reset_credits: Some(reset_credits.clone()),
            },
        );

        merge_provider_credits_entry_if_due(&runtime, "codex", || ProviderCreditsEntry {
            observed_at_ms: unix_now_ms(),
            oauth_read_at_ms: unix_now_ms(),
            ok: false,
            extra_credits: None,
            reset_credits: None,
        });

        let entry = read_credits_cache(&runtime.shared_credits_path())
            .entries
            .remove("codex")
            .expect("codex entry");
        assert!(entry.ok);
        assert_eq!(entry.observed_at_ms, 42);
        assert_eq!(
            entry.extra_credits,
            Some(ExtraCredits::known(None, Some(5.0), None))
        );
        assert_eq!(entry.reset_credits, Some(reset_credits));
        assert!(entry.oauth_read_at_ms > 1);
    }

    #[test]
    fn invalidate_oauth_read_zeroes_attempt_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime");
        runtime.ensure_dirs().unwrap();
        merge_provider_credits_entry(
            &runtime,
            "codex",
            ProviderCreditsEntry {
                observed_at_ms: 1,
                oauth_read_at_ms: 1234,
                ok: true,
                extra_credits: Some(ExtraCredits::known(None, Some(5.0), None)),
                reset_credits: None,
            },
        );

        invalidate_oauth_read(&runtime, "codex");

        assert_eq!(
            read_credits_cache(&runtime.shared_credits_path())
                .entries
                .get("codex")
                .map(|entry| entry.oauth_read_at_ms),
            Some(0)
        );
    }

    #[test]
    fn genuine_zero_reset_credits_replaces_prior_count() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
            .expect("runtime");
        runtime.ensure_dirs().unwrap();

        merge_provider_credits_entry(
            &runtime,
            "codex",
            ProviderCreditsEntry {
                observed_at_ms: 1,
                oauth_read_at_ms: 0,
                ok: true,
                extra_credits: None,
                reset_credits: Some(ResetCredits {
                    count: 2,
                    soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
                }),
            },
        );
        merge_provider_credits_entry(
            &runtime,
            "codex",
            ProviderCreditsEntry {
                observed_at_ms: 2,
                oauth_read_at_ms: 0,
                ok: true,
                extra_credits: None,
                reset_credits: Some(ResetCredits {
                    count: 0,
                    soonest_expiry: None,
                }),
            },
        );

        let cache = read_credits_cache(&runtime.shared_credits_path());
        assert_eq!(
            cache
                .entries
                .get("codex")
                .and_then(|entry| entry.reset_credits.as_ref())
                .map(|credits| credits.count),
            Some(0)
        );
    }

    #[test]
    fn fold_applies_displayable_reset_credits_to_metered_panel() {
        let mut snapshot = SidebarSnapshot::build(
            crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
            Vec::new(),
            Vec::new(),
            jiff::Timestamp::from_second(1_700_000_000).unwrap(),
        );
        snapshot.providers = vec![panel("codex", true), panel("claude", true)];
        let reset_credits = ResetCredits {
            count: 1,
            soonest_expiry: jiff::Timestamp::from_second(1_800_000_000).ok(),
        };
        let mut cache = CreditsCache::default();
        let now_ms = CREDITS_DISPLAY_MAX_AGE.as_millis() as u64 + 100;
        cache.entries.insert(
            "codex".to_owned(),
            ProviderCreditsEntry {
                observed_at_ms: now_ms,
                oauth_read_at_ms: 0,
                ok: true,
                extra_credits: None,
                reset_credits: Some(reset_credits.clone()),
            },
        );
        cache.entries.insert(
            "claude".to_owned(),
            ProviderCreditsEntry {
                observed_at_ms: 0,
                oauth_read_at_ms: 0,
                ok: true,
                extra_credits: None,
                reset_credits: Some(ResetCredits {
                    count: 3,
                    soonest_expiry: None,
                }),
            },
        );

        apply_credits_cache_with(&mut snapshot, &cache, &AccountsConfig::default(), now_ms);

        assert_eq!(snapshot.providers[0].reset_credits, Some(reset_credits));
        assert_eq!(snapshot.providers[1].reset_credits, None);
    }
}
