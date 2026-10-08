//! Account-scoped Codex model catalog and family alias selection.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::capabilities::{
    ModelAliasMove, ModelAliasRequest, ModelAliasResolution, ModelAliasRung, ModelCatalogEntry,
    ModelCatalogErr, ModelCatalogSource,
};
use crate::disk::{atomic, lock::WorkspaceLock};
use crate::ids::LoginKey;
use crate::utils::time::unix_now_ms;

const CACHE_VERSION: u32 = 1;
const FRESH_MS: u64 = 60 * 60 * 1000;
const RETRY_MS: u64 = 60 * 1000;
const STALE_WARN_MS: u64 = 24 * 60 * 60 * 1000;
const CATALOG_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Serialize, Deserialize)]
struct CatalogCache {
    version: u32,
    fetched_at: u64,
    #[serde(default)]
    failed_at: Option<u64>,
    #[serde(default)]
    failed_error: Option<String>,
    catalog: Vec<ModelCatalogEntry>,
    targets: BTreeMap<String, String>,
}

impl CatalogCache {
    fn read(path: &Path) -> Option<Self> {
        let cache: Self = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        (cache.version == CACHE_VERSION).then_some(cache)
    }

    fn fresh(&self, now: u64) -> bool {
        now.checked_sub(self.fetched_at)
            .is_some_and(|age| age < FRESH_MS)
    }

    fn write(&self, path: &Path) -> Result<(), atomic::AtomicErr> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| atomic::AtomicErr::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        atomic::write_temp_then_rename(path, self)
    }
}

struct AppServerSource;

impl ModelCatalogSource for AppServerSource {
    fn fetch(
        &mut self,
        paths: &RuntimePaths,
        login_env: &BTreeMap<String, String>,
    ) -> Result<Vec<ModelCatalogEntry>, ModelCatalogErr> {
        super::app_server::CodexAppServer::fetch_catalog(
            Some(&paths.codex_app_server_socket_path()),
            login_env,
            CATALOG_BUDGET,
        )
        .map_err(|err| ModelCatalogErr::Unavailable(err.to_string()))
    }
}

fn family(id: &str) -> Option<(Vec<&str>, &str)> {
    let (version, family) = id.strip_prefix("gpt-")?.split_once('-')?;
    let components: Vec<_> = version.split('.').collect();
    components
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|c| c.is_ascii_digit()))
        .then_some((components, family))
}

fn upgrade_end<'a>(
    entry: &'a ModelCatalogEntry,
    entries: &'a [ModelCatalogEntry],
) -> &'a ModelCatalogEntry {
    let mut current = entry;
    let mut seen = HashSet::from([current.id.as_str()]);
    while let Some(next) = current
        .upgrade
        .as_deref()
        .and_then(|id| entries.iter().find(|entry| entry.id == id && !entry.hidden))
    {
        if !seen.insert(next.id.as_str()) {
            break;
        }
        current = next;
    }
    current
}

fn select<'a>(
    entries: &'a [ModelCatalogEntry],
    baked: &str,
    effort: Option<&str>,
) -> Option<(&'a str, Vec<String>)> {
    let (_, target_family) = family(baked)?;
    let mut candidates: Vec<_> = entries
        .iter()
        .filter(|entry| !entry.hidden)
        .filter_map(|entry| {
            let (version, name) = family(&entry.id)?;
            (name == target_family).then_some((version, entry))
        })
        .collect();
    // Comparing normalized digit strings avoids an artificial integer-size limit.
    candidates.sort_by_cached_key(|(version, _)| {
        std::cmp::Reverse(
            version
                .iter()
                .map(|part| {
                    let digits = part.trim_start_matches('0');
                    (digits.len(), digits.to_owned())
                })
                .collect::<Vec<_>>(),
        )
    });
    let newest = upgrade_end(candidates.first()?.1, entries);
    let Some(effort) = effort else {
        return Some((&newest.id, Vec::new()));
    };
    for (_, candidate) in candidates {
        let target = upgrade_end(candidate, entries);
        if target.efforts.iter().any(|supported| supported == effort) {
            let warnings = if target.id == newest.id {
                Vec::new()
            } else {
                vec![format!(
                    "codex alias skipped {}: effort {effort} is unsupported; using {} (choose a supported effort to use the newest model)",
                    newest.id, target.id
                )]
            };
            return Some((&target.id, warnings));
        }
    }
    Some((
        &newest.id,
        vec![format!(
            "codex alias has no model supporting effort {effort}; using {} (choose a supported effort)",
            newest.id
        )],
    ))
}

pub(super) fn resolve(
    request: ModelAliasRequest<'_>,
    source: Option<&mut dyn ModelCatalogSource>,
) -> Option<ModelAliasResolution> {
    let baked = super::DEFINITIONS
        .models
        .iter()
        .find(|entry| entry.name == request.alias && entry.name != entry.id)?
        .id;
    let key = request.login.key();
    let path = request.paths.shared_model_catalog_path(&key);
    let lock = WorkspaceLock::acquire(&request.paths.shared_model_catalog_lock(&key));
    let mut cache = CatalogCache::read(&path);
    let now = unix_now_ms();
    let mut reason = String::new();
    let mut refresh_failure = None;
    let mut cache_updated = false;
    if let Err(err) = &lock {
        reason = format!("{err}; check cache directory permissions");
    } else if cache
        .as_ref()
        .and_then(|cache| cache.failed_at)
        .and_then(|failed_at| now.checked_sub(failed_at))
        .is_some_and(|age| age < RETRY_MS)
    {
        reason = cache
            .as_ref()
            .and_then(|cache| cache.failed_error.clone())
            .unwrap_or_else(|| "recent catalog fetch failed".into());
    } else if !cache.as_ref().is_some_and(|cache| cache.fresh(now)) {
        match source
            .unwrap_or(&mut AppServerSource)
            .fetch(request.paths, request.login_env)
        {
            Ok(catalog) => {
                let usable = select(&catalog, baked, request.effort).is_some();
                if !usable {
                    reason = "fresh catalog has no matching family".into();
                }
                if usable
                    || cache
                        .as_ref()
                        .and_then(|cache| select(&cache.catalog, baked, request.effort))
                        .is_none()
                {
                    cache = Some(CatalogCache {
                        version: CACHE_VERSION,
                        fetched_at: now,
                        failed_at: None,
                        failed_error: None,
                        catalog,
                        targets: cache.map_or_else(BTreeMap::new, |cache| cache.targets),
                    });
                    cache_updated = true;
                }
            }
            Err(err) => reason = err.to_string(),
        }
        if !reason.is_empty() {
            refresh_failure = Some(reason.clone());
            let cache = cache.get_or_insert_with(|| CatalogCache {
                version: CACHE_VERSION,
                fetched_at: 0,
                failed_at: None,
                failed_error: None,
                catalog: Vec::new(),
                targets: BTreeMap::new(),
            });
            cache.failed_at = Some(unix_now_ms());
            cache.failed_error = Some(reason.clone());
            cache_updated = true;
        }
    }

    let selected = cache
        .as_ref()
        .and_then(|cache| select(&cache.catalog, baked, request.effort));
    let Some((id, mut warnings)) = selected else {
        if reason.is_empty() {
            reason = format!("catalog has no {} family", request.alias);
        }
        let mut warnings = vec![format!(
            "codex alias {} is using baked fallback {baked}: {reason}",
            request.alias
        )];
        if cache_updated
            && let Some(cache) = &cache
            && let Err(err) = cache.write(&path)
        {
            warnings.push(format!(
                "cannot save codex model catalog: {err}; check cache directory permissions"
            ));
        }
        return Some(ModelAliasResolution {
            id: baked.into(),
            rung: ModelAliasRung::Baked,
            warnings,
            movement: None,
            refresh_failure,
        });
    };
    let id = id.to_owned();
    let rung = if reason.is_empty() && cache.as_ref().is_some_and(|cache| cache.fresh(now)) {
        ModelAliasRung::FreshCatalog
    } else {
        if lock.is_err() {
            warnings.push(format!(
                "codex alias {} cannot refresh catalog: {reason}",
                request.alias
            ));
        } else if let Some(cache) = &cache
            && now.saturating_sub(cache.fetched_at) >= STALE_WARN_MS
        {
            let seconds =
                i64::try_from(now.saturating_sub(cache.fetched_at) / 1000).unwrap_or(i64::MAX);
            let age = crate::utils::time::format_duration_coarse(seconds);
            warnings.push(format!(
                "codex alias {} is using a catalog from {age} ago: {reason}",
                request.alias
            ));
        }
        ModelAliasRung::CachedCatalog
    };
    let mut movement = None;
    if lock.is_ok() {
        // A selected entry can only come from this cache.
        let cache = cache.as_mut().expect("selected entry has a catalog");
        // Selection guarantees a family head; effort changes affect launches, not release history.
        let head = select(&cache.catalog, baked, None)
            .expect("selected entry has a family head")
            .0
            .to_owned();
        let previous = cache
            .targets
            .get(request.alias)
            .map_or(baked, String::as_str);
        if previous != head {
            movement = Some(ModelAliasMove {
                alias: request.alias.into(),
                from: previous.into(),
                to: head.clone(),
            });
        }
        let changed = cache.targets.get(request.alias) != Some(&head);
        cache.targets.insert(request.alias.into(), head);
        if (cache_updated || changed)
            && let Err(err) = cache.write(&path)
        {
            movement = None;
            warnings.push(format!(
                "cannot save codex model catalog: {err}; check cache directory permissions"
            ));
        }
    }
    Some(ModelAliasResolution {
        id,
        rung,
        warnings,
        movement,
        refresh_failure,
    })
}

pub(super) fn known_model(paths: &RuntimePaths, login: &LoginKey, id: &str) -> Option<bool> {
    CatalogCache::read(&paths.shared_model_catalog_path(login))
        .filter(|cache| cache.fetched_at != 0)
        .map(|cache| cache.catalog.iter().any(|entry| entry.id == id))
}

#[cfg(test)]
mod tests;
