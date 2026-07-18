//! Shared filesystem epochs and dependency caches for provider-local discovery.

use std::collections::HashMap;
use std::fs;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const LOCAL_SESSION_DISCOVERY_BACKSTOP: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ProviderPathState {
    Missing,
    File,
    Directory,
    SymlinkOrOther,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProviderPathStamp {
    pub(super) state: ProviderPathState,
    pub(super) len: u64,
    pub(super) modified: Option<SystemTime>,
}

impl ProviderPathStamp {
    pub(super) fn read(path: &Path) -> Self {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                let file_type = metadata.file_type();
                let state = if file_type.is_file() {
                    ProviderPathState::File
                } else if file_type.is_dir() {
                    ProviderPathState::Directory
                } else {
                    ProviderPathState::SymlinkOrOther
                };
                Self {
                    state,
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self {
                state: ProviderPathState::Missing,
                len: 0,
                modified: None,
            },
            Err(_) => Self {
                state: ProviderPathState::Unavailable,
                len: 0,
                modified: None,
            },
        }
    }

    pub(super) fn is_file(&self) -> bool {
        self.state == ProviderPathState::File
    }

    pub(super) fn is_dir(&self) -> bool {
        self.state == ProviderPathState::Directory
    }

    pub(super) fn is_stable(&self) -> bool {
        self.state != ProviderPathState::Unavailable
    }

    fn kind_only(mut self) -> Self {
        self.len = 0;
        self.modified = None;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StampComparison {
    Exact,
    KindOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StampedPath {
    path: PathBuf,
    comparison: StampComparison,
    stamp: ProviderPathStamp,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct StampedPaths {
    entries: Vec<StampedPath>,
}

impl StampedPaths {
    pub(super) fn exact(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut stamped = Self::default();
        stamped.record_exact_many(paths);
        stamped
    }

    #[cfg(test)]
    fn kind_only(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let mut stamped = Self::default();
        stamped.record_kind_only_many(paths);
        stamped
    }

    pub(super) fn record_exact(&mut self, path: impl Into<PathBuf>) {
        self.record(path.into(), StampComparison::Exact);
    }

    pub(super) fn record_exact_many(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            self.record_exact(path);
        }
    }

    pub(super) fn record_kind_only(&mut self, path: impl Into<PathBuf>) {
        self.record(path.into(), StampComparison::KindOnly);
    }

    pub(super) fn record_kind_only_many(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            self.record_kind_only(path);
        }
    }

    fn record(&mut self, path: PathBuf, comparison: StampComparison) {
        if self.entries.iter().any(|entry| entry.path == path) {
            return;
        }
        let stamp = Self::read(&path, comparison);
        self.entries.push(StampedPath {
            path,
            comparison,
            stamp,
        });
    }

    fn read(path: &Path, comparison: StampComparison) -> ProviderPathStamp {
        let stamp = ProviderPathStamp::read(path);
        match comparison {
            StampComparison::Exact => stamp,
            StampComparison::KindOnly => stamp.kind_only(),
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&Path, &ProviderPathStamp)> {
        self.entries
            .iter()
            .map(|entry| (entry.path.as_path(), &entry.stamp))
    }

    pub(super) fn all_stable(&self) -> bool {
        self.entries.iter().all(|entry| entry.stamp.is_stable())
    }

    pub(super) fn unchanged(&self) -> bool {
        self.entries.iter().all(|entry| {
            entry.stamp.is_stable() && Self::read(&entry.path, entry.comparison) == entry.stamp
        })
    }

    pub(super) fn recapture(&self) -> Self {
        Self {
            entries: self
                .entries
                .iter()
                .map(|entry| StampedPath {
                    path: entry.path.clone(),
                    comparison: entry.comparison,
                    stamp: Self::read(&entry.path, entry.comparison),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CatalogRefresh {
    attempted: bool,
    stable: bool,
}

impl CatalogRefresh {
    pub(super) fn attempted(self) -> bool {
        self.attempted
    }

    #[cfg(test)]
    fn stable(self) -> bool {
        self.stable
    }
}

pub(super) struct IncrementalCatalog<K, C> {
    key: Option<K>,
    last_stable_scan: Option<Instant>,
    topology: StampedPaths,
    entries: Vec<C>,
    invalidated: bool,
}

impl<K, C> Default for IncrementalCatalog<K, C> {
    fn default() -> Self {
        Self {
            key: None,
            last_stable_scan: None,
            topology: StampedPaths::default(),
            entries: Vec::new(),
            invalidated: false,
        }
    }
}

impl<K: Eq, C> IncrementalCatalog<K, C> {
    pub(super) fn refresh(
        &mut self,
        key: K,
        now: Instant,
        enumerate: impl FnOnce(&mut StampedPaths) -> Vec<C>,
    ) -> CatalogRefresh {
        let due = self.invalidated
            || self.key.as_ref() != Some(&key)
            || !self.topology.unchanged()
            || full_scan_due(self.last_stable_scan, now);
        if !due {
            return CatalogRefresh {
                attempted: false,
                stable: true,
            };
        }

        let mut topology = StampedPaths::default();
        let entries = enumerate(&mut topology);
        let stable = topology.all_stable() && topology.unchanged();
        let topology = topology.recapture();
        self.key = Some(key);
        self.topology = topology;
        self.invalidated = false;
        if stable {
            self.last_stable_scan = Some(now);
            self.entries = entries;
        } else {
            self.last_stable_scan = None;
            self.entries.clear();
        }
        CatalogRefresh {
            attempted: true,
            stable,
        }
    }

    pub(super) fn entries(&self) -> &[C] {
        &self.entries
    }

    pub(super) fn invalidate(&mut self) {
        self.invalidated = true;
        self.last_stable_scan = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValueRefreshKind {
    Cached,
    Refreshed,
    Invalidated,
}

pub(super) struct ValueRefresh<V> {
    kind: ValueRefreshKind,
    prior: Option<V>,
    current: Option<V>,
}

impl<V> ValueRefresh<V> {
    pub(super) fn kind(&self) -> ValueRefreshKind {
        self.kind
    }

    pub(super) fn prior(&self) -> Option<&V> {
        self.prior.as_ref()
    }

    pub(super) fn current(&self) -> Option<&V> {
        self.current.as_ref()
    }

    pub(super) fn into_current(self) -> Option<V> {
        self.current
    }
}

struct StableValue<V> {
    dependencies: StampedPaths,
    value: V,
}

pub(super) struct StableValueCache<K, V> {
    values: HashMap<K, StableValue<V>>,
}

impl<K, V> Default for StableValueCache<K, V> {
    fn default() -> Self {
        Self {
            values: HashMap::new(),
        }
    }
}

impl<K, V> StableValueCache<K, V>
where
    K: Eq + Hash + Clone,
    V: Clone,
{
    pub(super) fn refresh(
        &mut self,
        key: K,
        dependencies: StampedPaths,
        forced: bool,
        loader: impl FnOnce(&StampedPaths) -> V,
    ) -> ValueRefresh<V> {
        let prior = self.values.get(&key).map(|cached| cached.value.clone());
        if !forced
            && dependencies.all_stable()
            && let Some(cached) = self.values.get(&key)
            && cached.dependencies == dependencies
        {
            return ValueRefresh {
                kind: ValueRefreshKind::Cached,
                prior: prior.clone(),
                current: prior,
            };
        }

        if !dependencies.all_stable() {
            self.values.remove(&key);
            return ValueRefresh {
                kind: ValueRefreshKind::Invalidated,
                prior,
                current: None,
            };
        }

        let value = loader(&dependencies);
        let after = dependencies.recapture();
        if dependencies != after || !after.all_stable() {
            self.values.remove(&key);
            return ValueRefresh {
                kind: ValueRefreshKind::Invalidated,
                prior,
                current: None,
            };
        }
        self.values.insert(
            key,
            StableValue {
                dependencies: after,
                value: value.clone(),
            },
        );
        ValueRefresh {
            kind: ValueRefreshKind::Refreshed,
            prior,
            current: Some(value),
        }
    }

    pub(super) fn retain(&mut self, mut keep: impl FnMut(&K) -> bool) {
        self.values.retain(|key, _| keep(key));
    }
}

pub(super) fn normalized_workspace_inputs(workspaces: &[&Path]) -> Vec<PathBuf> {
    let mut workspaces = workspaces
        .iter()
        .filter(|workspace| workspace.is_absolute())
        .map(|workspace| crate::worktree::normalize_path_lexical(workspace))
        .collect::<Vec<_>>();
    workspaces.sort();
    workspaces.dedup();
    workspaces
}

fn full_scan_due(last_scan: Option<Instant>, now: Instant) -> bool {
    last_scan.is_none_or(|last_scan| {
        now.checked_duration_since(last_scan)
            .is_none_or(|elapsed| elapsed >= LOCAL_SESSION_DISCOVERY_BACKSTOP)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_distinguish_absence_files_directories_and_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let missing = ProviderPathStamp::read(&temp.path().join("missing"));
        let file = temp.path().join("file");
        fs::write(&file, "body").unwrap();
        let dir = temp.path().join("dir");
        fs::create_dir(&dir).unwrap();

        assert_eq!(missing.state, ProviderPathState::Missing);
        assert!(ProviderPathStamp::read(&file).is_file());
        assert!(ProviderPathStamp::read(&dir).is_dir());

        #[cfg(unix)]
        {
            let link = temp.path().join("link");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert_eq!(
                ProviderPathStamp::read(&link).state,
                ProviderPathState::SymlinkOrOther
            );
        }
    }

    #[test]
    fn exact_and_kind_only_stamps_apply_their_own_stability_rules() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("value");
        fs::write(&path, "one").unwrap();
        let exact = StampedPaths::exact([path.clone()]);
        let kind = StampedPaths::kind_only([path.clone()]);

        fs::write(&path, "longer").unwrap();
        assert!(!exact.unchanged());
        assert!(kind.unchanged());

        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(!kind.unchanged());
    }

    #[test]
    fn value_cache_hits_positive_and_negative_values_and_reloads_one_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::write(&first, "one").unwrap();
        fs::write(&second, "two").unwrap();
        let mut cache = StableValueCache::<PathBuf, Option<String>>::default();

        let positive = cache.refresh(
            first.clone(),
            StampedPaths::exact([first.clone()]),
            false,
            |_| Some("one".to_owned()),
        );
        let negative = cache.refresh(
            second.clone(),
            StampedPaths::exact([second.clone()]),
            false,
            |_| None,
        );
        assert_eq!(positive.kind(), ValueRefreshKind::Refreshed);
        assert_eq!(negative.kind(), ValueRefreshKind::Refreshed);
        assert_eq!(
            cache
                .refresh(
                    first.clone(),
                    StampedPaths::exact([first.clone()]),
                    false,
                    |_| panic!("stable positive reparsed"),
                )
                .kind(),
            ValueRefreshKind::Cached
        );
        assert_eq!(
            cache
                .refresh(
                    second.clone(),
                    StampedPaths::exact([second.clone()]),
                    false,
                    |_| panic!("stable negative reparsed"),
                )
                .kind(),
            ValueRefreshKind::Cached
        );

        fs::write(&first, "changed length").unwrap();
        assert_eq!(
            cache
                .refresh(
                    first.clone(),
                    StampedPaths::exact([first]),
                    false,
                    |_| Some("changed".to_owned()),
                )
                .kind(),
            ValueRefreshKind::Refreshed
        );
        assert_eq!(
            cache
                .refresh(
                    second.clone(),
                    StampedPaths::exact([second]),
                    false,
                    |_| panic!("unrelated dependency reparsed"),
                )
                .kind(),
            ValueRefreshKind::Cached
        );
    }

    #[test]
    fn mutation_during_value_load_invalidates_prior_value() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("value");
        fs::write(&path, "one").unwrap();
        let mut cache = StableValueCache::<(), String>::default();
        let result = cache.refresh((), StampedPaths::exact([path.clone()]), false, |_| {
            fs::write(&path, "changed length").unwrap();
            "one".to_owned()
        });
        assert_eq!(result.kind(), ValueRefreshKind::Invalidated);
        assert!(result.current().is_none());
    }

    #[test]
    fn catalog_retries_unstable_scans_and_tracks_key_topology_and_backstop() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let start = Instant::now();
        let mut catalog = IncrementalCatalog::<u8, PathBuf>::default();
        let first = catalog.refresh(1, start, |topology| {
            topology.record_exact(root.clone());
            vec![root.clone()]
        });
        assert!(first.attempted() && first.stable());
        assert_eq!(catalog.entries(), std::slice::from_ref(&root));
        assert!(
            !catalog
                .refresh(1, start + Duration::from_secs(29), |_| unreachable!())
                .attempted()
        );

        fs::write(root.join("entry"), "body").unwrap();
        assert!(
            catalog
                .refresh(1, start + Duration::from_secs(1), |topology| {
                    topology.record_exact(root.clone());
                    Vec::new()
                })
                .attempted()
        );
        assert!(
            catalog
                .refresh(2, start + Duration::from_secs(2), |topology| {
                    topology.record_exact(root.clone());
                    Vec::new()
                })
                .attempted()
        );
        assert!(
            catalog
                .refresh(2, start + Duration::from_secs(32), |topology| {
                    topology.record_exact(root.clone());
                    Vec::new()
                })
                .attempted()
        );
    }

    #[test]
    fn mutation_during_enumeration_empties_catalog_until_retry() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let now = Instant::now();
        let mut catalog = IncrementalCatalog::<(), PathBuf>::default();
        let unstable = catalog.refresh((), now, |topology| {
            topology.record_exact(root.clone());
            fs::write(root.join("entry"), "body").unwrap();
            vec![root.join("entry")]
        });
        assert!(unstable.attempted() && !unstable.stable());
        assert!(catalog.entries().is_empty());
        assert!(
            catalog
                .refresh((), now, |topology| {
                    topology.record_exact(root.clone());
                    vec![root.join("entry")]
                })
                .attempted()
        );
        assert_eq!(catalog.entries(), [root.join("entry")]);
    }

    #[test]
    fn explicit_catalog_invalidation_forces_refresh() {
        let now = Instant::now();
        let mut catalog = IncrementalCatalog::<(), ()>::default();
        catalog.refresh((), now, |_| vec![()]);
        catalog.invalidate();
        assert!(catalog.refresh((), now, |_| vec![()]).attempted());
    }

    #[test]
    fn normalizes_workspace_inputs() {
        let inputs = normalized_workspace_inputs(&[
            Path::new("/work/one/./src/.."),
            Path::new("relative"),
            Path::new("/work/one"),
        ]);
        assert_eq!(inputs, [PathBuf::from("/work/one")]);
    }
}
