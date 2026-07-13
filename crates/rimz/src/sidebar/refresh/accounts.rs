use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::AgentAccount;
use crate::agents::account::AccountProbe;
use crate::sidebar::timing::{ACCOUNTS_RETRY_TTL, ACCOUNTS_TTL, unix_now_ms};

use super::SidebarSnapshot;

/// Poll cadence and budget for the accounts single-flight: a loser waits up to
/// `STEP * STEPS` for the elected prober's publish before forking its own probe.
/// Matched to the diff-stats single-flight, leaning long enough to ride the
/// elder's `claude auth status` fork rather than racing it.
const ACCOUNTS_WAIT_STEP: Duration = Duration::from_millis(20);
const ACCOUNTS_WAIT_STEPS: u32 = 15;

/// The producer's published account probe state, keyed by provider so one
/// transient failure retries without expiring every provider's successful read.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountsCache {
    pub providers: BTreeMap<String, ProviderRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderRecord {
    pub probed_at_ms: u64,
    /// A failed probe retries on `ACCOUNTS_RETRY_TTL`; a confident result rides
    /// `ACCOUNTS_TTL`.
    pub ok: bool,
    /// Probed account facts; `None` is an authoritative logged-out result.
    pub account: Option<AgentAccount>,
}

/// Resolve provider accounts for the producer behind a process-wide
/// single-flight. Fresh providers ride their own timestamps; only due kinds
/// fork, and the winner merges those records into the shared cache.
pub(crate) fn produce_accounts(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
) -> BTreeMap<String, AgentAccount> {
    let path = runtime.shared_accounts_path();
    let context_versions = context_versions(snapshot);
    let cache = read_accounts_cache(&path);
    if due_provider_kinds(&cache, snapshot, unix_now_ms()).is_empty() {
        return accounts_with_context_versions(&cache, &context_versions);
    }

    let lock_path = runtime.shared_accounts_lock();
    let fresh = || {
        let cache = read_accounts_cache(&path);
        due_provider_kinds(&cache, snapshot, unix_now_ms())
            .is_empty()
            .then(|| accounts_with_context_versions(&cache, &context_versions))
    };
    match crate::store::single_flight::coalesce(
        &lock_path,
        ACCOUNTS_WAIT_STEP,
        ACCOUNTS_WAIT_STEPS,
        fresh,
    ) {
        crate::store::single_flight::Coalesced::Shared(accounts) => accounts,
        crate::store::single_flight::Coalesced::Produce(_guard) => {
            let cache = read_accounts_cache(&path);
            let due = due_provider_kinds(&cache, snapshot, unix_now_ms());
            if due.is_empty() {
                return accounts_with_context_versions(&cache, &context_versions);
            }
            let cache = probe_accounts(snapshot, &due, &cache);
            write_accounts_cache(&path, &cache);
            accounts_with_context_versions(&cache, &context_versions)
        }
        // A wedged elder cannot block this frame. Probe only the still-due
        // providers locally and leave publishing to the lock holder.
        crate::store::single_flight::Coalesced::ProduceLocal => {
            let cache = read_accounts_cache(&path);
            let due = due_provider_kinds(&cache, snapshot, unix_now_ms());
            let cache = probe_accounts(snapshot, &due, &cache);
            accounts_with_context_versions(&cache, &context_versions)
        }
    }
}

pub(crate) fn cached_accounts_for_snapshot(
    cache: AccountsCache,
    snapshot: &SidebarSnapshot,
) -> BTreeMap<String, AgentAccount> {
    accounts_with_context_versions(&cache, &context_versions(snapshot))
}

fn due_provider_kinds(
    cache: &AccountsCache,
    snapshot: &SidebarSnapshot,
    now_ms: u64,
) -> BTreeSet<String> {
    let active_version_kinds = active_version_probe_kinds(snapshot);
    let context_versions = context_versions(snapshot);
    provider_kinds(snapshot)
        .into_iter()
        .filter(|kind| {
            let Some(record) = cache.providers.get(kind) else {
                return true;
            };
            let age_ms = now_ms.saturating_sub(record.probed_at_ms);
            if !record.ok {
                return age_ms > ACCOUNTS_RETRY_TTL.as_millis() as u64;
            }
            if age_ms > ACCOUNTS_TTL.as_millis() as u64 {
                return true;
            }
            age_ms > ACCOUNTS_RETRY_TTL.as_millis() as u64
                && active_version_kinds.contains(kind)
                && context_versions
                    .get(kind)
                    .filter(|version| !version.is_empty())
                    .is_none()
                && account_version(record.account.as_ref()).is_none()
        })
        .collect()
}

fn provider_kinds(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    let mut kinds: BTreeSet<String> = crate::agents::known_kinds().map(str::to_owned).collect();
    kinds.extend(
        snapshot
            .agents
            .iter()
            .filter(|agent| agent.parent_agent_id.is_none())
            .filter_map(|agent| {
                crate::agents::find_adapter(agent.kind.as_str()).map(|_| agent.kind.to_string())
            }),
    );
    kinds
}

/// Keep the adapter calls at the edge; `probe_accounts_with` owns the pure
/// per-kind record merge and is exercised without subprocesses in unit tests.
fn probe_accounts(
    snapshot: &SidebarSnapshot,
    due_kinds: &BTreeSet<String>,
    previous: &AccountsCache,
) -> AccountsCache {
    let active_version_kinds = active_version_probe_kinds(snapshot);
    probe_accounts_with(
        due_kinds,
        previous,
        &active_version_kinds,
        unix_now_ms(),
        |kind, active| {
            let adapter = crate::agents::find_adapter(kind)?;
            let outcome = adapter.probe_account();
            let version = match &outcome {
                AccountProbe::Found(account) if account_version(Some(account)).is_none() => {
                    adapter.probe_version()
                }
                AccountProbe::LoggedOut | AccountProbe::Unavailable if active => {
                    adapter.probe_version()
                }
                _ => None,
            };
            if matches!(&outcome, AccountProbe::Unavailable) {
                tracing::warn!(
                    kind,
                    tags.operation = "accounts.probe_unavailable",
                    "provider account probe unavailable",
                );
            }
            Some((outcome, version))
        },
    )
}

fn probe_accounts_with(
    due_kinds: &BTreeSet<String>,
    previous: &AccountsCache,
    active_version_kinds: &BTreeSet<String>,
    probed_at_ms: u64,
    mut probe: impl FnMut(&str, bool) -> Option<(AccountProbe, Option<String>)>,
) -> AccountsCache {
    let mut providers = previous.providers.clone();
    for kind in due_kinds {
        let active = active_version_kinds.contains(kind);
        let Some((outcome, probed_version)) = probe(kind, active) else {
            continue;
        };
        let ok = !matches!(&outcome, AccountProbe::Unavailable);
        let previous_record = previous.providers.get(kind);
        let account = match outcome {
            AccountProbe::Found(mut account) => {
                if account_version(Some(&account)).is_none() {
                    account.version = probed_version.or_else(|| {
                        previous_record.and_then(|record| account_version(record.account.as_ref()))
                    });
                }
                Some(account)
            }
            AccountProbe::LoggedOut => active
                .then(|| {
                    probed_version
                        .or_else(|| {
                            previous_record
                                .and_then(|record| account_version(record.account.as_ref()))
                        })
                        .map(|version| AgentAccount {
                            version: Some(version),
                            ..Default::default()
                        })
                })
                .flatten(),
            AccountProbe::Unavailable => {
                let mut account = previous_record.and_then(|record| record.account.clone());
                if let Some(version) = probed_version {
                    account.get_or_insert_default().version = Some(version);
                }
                account
            }
        };
        providers.insert(
            kind.clone(),
            ProviderRecord {
                probed_at_ms,
                ok,
                account,
            },
        );
    }
    AccountsCache { providers }
}

fn account_version(account: Option<&AgentAccount>) -> Option<String> {
    account?
        .version
        .as_ref()
        .filter(|version| !version.is_empty())
        .cloned()
}

fn accounts_with_context_versions(
    cache: &AccountsCache,
    context_versions: &BTreeMap<String, String>,
) -> BTreeMap<String, AgentAccount> {
    let accounts = cache
        .providers
        .iter()
        .filter_map(|(kind, record)| {
            record
                .account
                .as_ref()
                .map(|account| (kind.clone(), account.clone()))
        })
        .collect();
    merge_context_versions(accounts, context_versions)
}

fn merge_context_versions(
    mut accounts: BTreeMap<String, AgentAccount>,
    context_versions: &BTreeMap<String, String>,
) -> BTreeMap<String, AgentAccount> {
    for (kind, version) in context_versions {
        accounts.entry(kind.clone()).or_default().version = Some(version.clone());
    }
    accounts
}

fn context_versions(snapshot: &SidebarSnapshot) -> BTreeMap<String, String> {
    let mut versions = BTreeMap::<String, (jiff::Timestamp, String)>::new();
    for agent in &snapshot.agents {
        if agent.parent_agent_id.is_some() {
            continue;
        }
        let Some(context) = agent.context.as_ref() else {
            continue;
        };
        let Some(version) = context
            .agent_version
            .as_ref()
            .filter(|version| !version.is_empty())
        else {
            continue;
        };
        let entry = versions
            .entry(agent.kind.to_string())
            .or_insert((context.observed_at, version.clone()));
        if context.observed_at > entry.0 {
            *entry = (context.observed_at, version.clone());
        }
    }
    versions
        .into_iter()
        .map(|(kind, (_observed_at, version))| (kind, version))
        .collect()
}

fn active_version_probe_kinds(snapshot: &SidebarSnapshot) -> BTreeSet<String> {
    snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter_map(|agent| {
            crate::agents::find_adapter(agent.kind.as_str()).map(|_| agent.kind.to_string())
        })
        .collect()
}

/// Read the producer's published account cache, or an empty cache on a cold,
/// corrupt, or old-schema file. Read-only and fork-free.
pub(crate) fn read_accounts_cache(path: &Path) -> AccountsCache {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Publish the probed account cache atomically so readers never observe a
/// partially merged provider map. A write failure leaves the prior cache live.
pub(super) fn write_accounts_cache(path: &Path, cache: &AccountsCache) {
    if let Err(err) = crate::store::atomic::write_temp_then_rename_cache(path, cache) {
        tracing::warn!(
            path = %path.display(),
            tags.operation = "cache.accounts_write",
            error = &err as &dyn std::error::Error,
            "sidebar accounts cache write failed",
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use jiff::Timestamp;

    use super::*;
    use crate::SidebarSnapshot;
    use crate::ids::WorkspaceId;
    use crate::sidebar::test_support::root_agent;

    fn record(probed_at_ms: u64, ok: bool, account: Option<AgentAccount>) -> ProviderRecord {
        ProviderRecord {
            probed_at_ms,
            ok,
            account,
        }
    }

    fn fresh_cache(now_ms: u64) -> AccountsCache {
        AccountsCache {
            providers: crate::agents::known_kinds()
                .map(|kind| (kind.to_owned(), record(now_ms, true, None)))
                .collect(),
        }
    }

    fn snapshot_with(kind: &str) -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/provider-version")),
            vec![root_agent(kind, "active", None)],
            Timestamp::now(),
        )
    }

    fn empty_snapshot() -> SidebarSnapshot {
        SidebarSnapshot::build_with_agents(
            WorkspaceId::from_project_root(Path::new("/tmp/provider-accounts")),
            Vec::new(),
            Timestamp::now(),
        )
    }

    #[test]
    fn failed_provider_retries_alone_without_refreshing_successful_records() {
        let now_ms = unix_now_ms();
        let stale_ms = now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
        let mut cache = fresh_cache(now_ms);
        cache.providers.insert(
            "copilot".to_owned(),
            record(
                stale_ms,
                false,
                Some(AgentAccount {
                    account_id: Some("octocat".to_owned()),
                    ..Default::default()
                }),
            ),
        );
        cache.providers.insert(
            "claude".to_owned(),
            record(
                stale_ms,
                true,
                Some(AgentAccount {
                    plan: Some("max".to_owned()),
                    ..Default::default()
                }),
            ),
        );
        let snapshot = empty_snapshot();
        let due = due_provider_kinds(&cache, &snapshot, now_ms);
        assert_eq!(due, BTreeSet::from(["copilot".to_owned()]));

        let successful = cache.providers["claude"].clone();
        let mut probed = Vec::new();
        let merged =
            probe_accounts_with(&due, &cache, &BTreeSet::new(), now_ms, |kind, _active| {
                probed.push(kind.to_owned());
                Some((AccountProbe::Unavailable, None))
            });

        assert_eq!(probed, ["copilot"]);
        assert_eq!(merged.providers["claude"], successful);
        assert_eq!(merged.providers["copilot"].probed_at_ms, now_ms);
    }

    #[test]
    fn unavailable_probe_keeps_last_known_account() {
        let previous_account = AgentAccount {
            plan: Some("pro".to_owned()),
            account_id: Some("octocat".to_owned()),
            ..Default::default()
        };
        let previous = AccountsCache {
            providers: BTreeMap::from([(
                "copilot".to_owned(),
                record(10, true, Some(previous_account.clone())),
            )]),
        };

        let merged = probe_accounts_with(
            &BTreeSet::from(["copilot".to_owned()]),
            &previous,
            &BTreeSet::new(),
            20,
            |_kind, _active| Some((AccountProbe::Unavailable, None)),
        );

        assert_eq!(merged.providers["copilot"].account, Some(previous_account));
        assert!(!merged.providers["copilot"].ok);

        let active = probe_accounts_with(
            &BTreeSet::from(["copilot".to_owned()]),
            &previous,
            &BTreeSet::from(["copilot".to_owned()]),
            20,
            |_kind, _active| Some((AccountProbe::Unavailable, Some("1.0.0".to_owned()))),
        );
        let account = active.providers["copilot"].account.as_ref().unwrap();
        assert_eq!(account.account_id.as_deref(), Some("octocat"));
        assert_eq!(account.version.as_deref(), Some("1.0.0"));
    }

    #[test]
    fn refreshed_accounts_keep_versions_and_logged_out_active_kinds_keep_version_only_records() {
        let previous = AccountsCache {
            providers: BTreeMap::from([(
                "pi".to_owned(),
                record(
                    10,
                    true,
                    Some(AgentAccount {
                        version: Some("0.78.0".to_owned()),
                        ..Default::default()
                    }),
                ),
            )]),
        };
        let due = BTreeSet::from(["pi".to_owned()]);
        let active = BTreeSet::from(["pi".to_owned()]);
        let found = probe_accounts_with(&due, &previous, &active, 20, |_kind, _active| {
            Some((
                AccountProbe::Found(AgentAccount {
                    plan: Some("OpenAI OAuth".to_owned()),
                    ..Default::default()
                }),
                None,
            ))
        });
        assert_eq!(
            found.providers["pi"]
                .account
                .as_ref()
                .and_then(|account| account.version.as_deref()),
            Some("0.78.0")
        );

        let logged_out = probe_accounts_with(&due, &previous, &active, 20, |_kind, _active| {
            Some((AccountProbe::LoggedOut, None))
        });
        assert_eq!(
            logged_out.providers["pi"]
                .account
                .as_ref()
                .and_then(|account| account.version.as_deref()),
            Some("0.78.0")
        );

        let idle = probe_accounts_with(&due, &previous, &BTreeSet::new(), 20, |_kind, _active| {
            Some((AccountProbe::LoggedOut, None))
        });
        assert_eq!(idle.providers["pi"].account, None);
    }

    #[test]
    fn live_context_versions_merge_without_writing_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        write_accounts_cache(
            &path,
            &AccountsCache {
                providers: BTreeMap::from([(
                    "codex".to_owned(),
                    record(
                        42,
                        true,
                        Some(AgentAccount {
                            metered: Some(true),
                            ..Default::default()
                        }),
                    ),
                )]),
            },
        );
        let cache = read_accounts_cache(&path);
        let versions = BTreeMap::from([("codex".to_owned(), "0.135.0".to_owned())]);

        let merged = accounts_with_context_versions(&cache, &versions);
        let persisted = read_accounts_cache(&path);

        assert_eq!(persisted.providers["codex"].probed_at_ms, 42);
        assert_eq!(
            merged
                .get("codex")
                .and_then(|account| account.version.as_deref()),
            Some("0.135.0")
        );
        assert_eq!(
            persisted.providers["codex"]
                .account
                .as_ref()
                .and_then(|account| account.version.as_deref()),
            None,
            "context versions remain local to the frame"
        );
    }

    #[test]
    fn old_schema_cache_is_discarded_and_every_provider_is_due() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("accounts.json");
        std::fs::write(&path, br#"{"refreshed_at_ms":42,"accounts":{},"ok":true}"#).unwrap();

        let cache = read_accounts_cache(&path);

        assert!(cache.providers.is_empty());
        let snapshot = empty_snapshot();
        assert_eq!(
            due_provider_kinds(&cache, &snapshot, 100),
            provider_kinds(&snapshot)
        );
    }

    #[test]
    fn missing_probeable_versions_refresh_per_provider_on_retry_cadence() {
        for kind in ["claude", "codex", "amp", "pi", "opencode", "kiro", "kimi"] {
            let snapshot = snapshot_with(kind);
            let now_ms = unix_now_ms();
            let mut cache = fresh_cache(now_ms);
            cache.providers.insert(
                kind.to_owned(),
                record(
                    now_ms,
                    true,
                    Some(AgentAccount {
                        plan: Some("Pro".to_owned()),
                        ..Default::default()
                    }),
                ),
            );
            assert!(!due_provider_kinds(&cache, &snapshot, now_ms).contains(kind));

            cache.providers.get_mut(kind).unwrap().probed_at_ms =
                now_ms.saturating_sub(ACCOUNTS_RETRY_TTL.as_millis() as u64 + 1);
            assert!(
                due_provider_kinds(&cache, &snapshot, now_ms).contains(kind),
                "an active {kind} account without a version re-probes after the retry window"
            );

            cache.providers.get_mut(kind).unwrap().ok = false;
            cache.providers.get_mut(kind).unwrap().probed_at_ms = now_ms;
            assert!(
                !due_provider_kinds(&cache, &snapshot, now_ms).contains(kind),
                "a failed {kind} probe waits for its own failure TTL"
            );
        }
    }
}
