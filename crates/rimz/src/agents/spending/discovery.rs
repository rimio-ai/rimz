//! Process-local, frontier-bounded discovery of historical spend stores.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use glob::{MatchOptions, Pattern};

use crate::agents::AgentAdapter;

use super::aggregate::cold_parse_out_of_window;
use super::cache::SpendingDiskCache;

const COMPLETE_RECONCILE_INTERVAL: Duration = Duration::from_secs(15 * 60);
type RelativeFilter = (&'static str, fn(&Path) -> bool);

/// One adapter-owned historical-spend location.
#[derive(Clone, Debug)]
pub enum SpendingSource {
    Exact(PathBuf),
    Group(SpendingSourceGroup),
}

impl SpendingSource {
    pub fn exact(path: impl Into<PathBuf>) -> Self {
        Self::Exact(path.into())
    }

    pub fn group(trees: Vec<SpendingSourceTree>) -> Self {
        Self::Group(SpendingSourceGroup::all(trees))
    }

    /// Select the first matching path from the first tree that has a match.
    /// This models stores such as OpenCode's preferred primary database and
    /// sorted per-channel fallback without teaching discovery provider names.
    pub fn first(trees: Vec<SpendingSourceTree>) -> Self {
        Self::Group(SpendingSourceGroup::first(trees))
    }

    fn key(&self) -> String {
        match self {
            Self::Exact(path) => format!("exact\0{}", path.to_string_lossy()),
            Self::Group(group) => group.key(),
        }
    }
}

/// Ordered roots participating in one selection/precedence rule.
#[derive(Clone, Debug)]
pub struct SpendingSourceGroup {
    trees: Vec<SpendingSourceTree>,
    selection: GroupSelection,
}

#[derive(Clone, Copy, Debug)]
enum GroupSelection {
    AllRelativeFirst,
    FirstPath,
}

impl SpendingSourceGroup {
    fn all(trees: Vec<SpendingSourceTree>) -> Self {
        Self {
            trees,
            selection: GroupSelection::AllRelativeFirst,
        }
    }

    fn first(trees: Vec<SpendingSourceTree>) -> Self {
        Self {
            trees,
            selection: GroupSelection::FirstPath,
        }
    }

    fn key(&self) -> String {
        let mut key = match self.selection {
            GroupSelection::AllRelativeFirst => "group:all".to_owned(),
            GroupSelection::FirstPath => "group:first".to_owned(),
        };
        for tree in &self.trees {
            key.push('\0');
            key.push_str(&tree.key());
        }
        key
    }
}

/// A rooted relative glob. Matching never escapes `root`.
#[derive(Clone, Debug)]
pub struct SpendingSourceTree {
    root: PathBuf,
    pattern: String,
    matcher: Vec<RelativePatternComponent>,
    codex_date_partitions: bool,
    filter: Option<RelativeFilter>,
    descend_filter: Option<RelativeFilter>,
}

#[derive(Clone, Debug)]
enum RelativePatternComponent {
    Recursive,
    Pattern(Pattern),
}

impl SpendingSourceTree {
    pub fn new(root: impl Into<PathBuf>, pattern: impl Into<String>) -> Option<Self> {
        let pattern = pattern.into();
        let matcher = pattern
            .split('/')
            .filter(|component| !component.is_empty())
            .map(|component| {
                if component == "**" {
                    Some(RelativePatternComponent::Recursive)
                } else {
                    Pattern::new(component)
                        .ok()
                        .map(RelativePatternComponent::Pattern)
                }
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            root: root.into(),
            pattern,
            matcher,
            codex_date_partitions: false,
            filter: None,
            descend_filter: None,
        })
    }

    pub fn codex_dates(mut self) -> Self {
        self.codex_date_partitions = true;
        self
    }

    /// Add a provider-owned relative-path predicate for rules a glob cannot
    /// express, while keeping the rule identity in declaration invalidation.
    pub fn filtered(mut self, name: &'static str, filter: fn(&Path) -> bool) -> Self {
        self.filter = Some((name, filter));
        self
    }

    /// Add a provider-owned directory predicate that prunes unrelated trees.
    pub fn descend_filtered(mut self, name: &'static str, filter: fn(&Path) -> bool) -> Self {
        self.descend_filter = Some((name, filter));
        self
    }

    fn key(&self) -> String {
        format!(
            "{}\0{}\0{}\0{}\0{}",
            self.root.to_string_lossy(),
            self.pattern,
            self.codex_date_partitions,
            self.filter.map_or("", |(name, _)| name),
            self.descend_filter.map_or("", |(name, _)| name),
        )
    }

    fn matches(&self, relative: &Path) -> bool {
        let components = relative
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(value) => value.to_str(),
                _ => None,
            })
            .collect::<Vec<_>>();
        relative_components_match(&self.matcher, &components)
            && self.filter.is_none_or(|(_, filter)| filter(relative))
    }
}

fn relative_components_match(pattern: &[RelativePatternComponent], path: &[&str]) -> bool {
    let Some((head, tail)) = pattern.split_first() else {
        return path.is_empty();
    };
    match head {
        RelativePatternComponent::Recursive => {
            relative_components_match(tail, path)
                || (!path.is_empty() && relative_components_match(pattern, &path[1..]))
        }
        RelativePatternComponent::Pattern(component) => {
            let Some((name, rest)) = path.split_first() else {
                return false;
            };
            component.matches_with(
                name,
                MatchOptions {
                    case_sensitive: true,
                    require_literal_separator: true,
                    require_literal_leading_dot: false,
                },
            ) && relative_components_match(tail, rest)
        }
    }
}

#[derive(Default)]
pub(crate) struct SpendingDiscoveryIndex {
    adapters: HashMap<&'static str, AdapterState>,
    last_complete: Option<Instant>,
    last_authoritative: bool,
    force_complete: bool,
    stats: DiscoveryStats,
}

struct AdapterState {
    key: String,
    sources: Vec<SourceState>,
}

enum SourceState {
    Exact(ExactState),
    Group(GroupState),
}

#[derive(Default)]
struct ExactState {
    path: PathBuf,
    known: Option<KnownFile>,
}

struct GroupState {
    selection: GroupSelection,
    trees: Vec<TreeState>,
}

struct TreeState {
    declaration: SpendingSourceTree,
    root: DirectoryNode,
}

#[derive(Clone, Debug, Default)]
struct DirectoryNode {
    stamp: Option<SystemTime>,
    due: bool,
    files: BTreeMap<PathBuf, KnownFile>,
    children: BTreeMap<PathBuf, DirectoryNode>,
    active_frontier: bool,
}

#[derive(Clone, Copy, Debug)]
struct KnownFile {
    active: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DiscoveryStats {
    pub(crate) directory_stats: u64,
    pub(crate) read_dirs: u64,
    pub(crate) candidate_stats: u64,
}

impl SpendingDiscoveryIndex {
    pub(crate) fn discover(
        &mut self,
        adapters: impl Iterator<Item = &'static dyn AgentAdapter>,
        now_secs: u64,
    ) -> Vec<(&'static dyn AgentAdapter, PathBuf)> {
        self.stats = DiscoveryStats::default();
        let force_complete = self.complete_due();
        let mut authoritative = true;
        let mut discovered = Vec::new();
        for adapter in adapters {
            let declarations = adapter.spending_sources();
            let key = source_set_key(&declarations);
            let kind = adapter.descriptor().kind;
            let changed = self.adapters.get(kind).is_none_or(|state| state.key != key);
            if changed {
                self.adapters.insert(
                    kind,
                    AdapterState {
                        key,
                        sources: declarations.into_iter().map(SourceState::from).collect(),
                    },
                );
            }
            let Some(state) = self.adapters.get_mut(kind) else {
                continue;
            };
            let full = force_complete && !changed;
            let (files, complete) = scan_adapter(state, now_secs, full, Some(&mut self.stats));
            authoritative &= complete;
            discovered.extend(files.into_iter().map(|path| (adapter, path)));
        }
        if (force_complete || self.last_complete.is_none()) && authoritative {
            self.last_complete = Some(Instant::now());
            self.force_complete = false;
        }
        self.last_authoritative = authoritative;

        let mut seen = BTreeSet::new();
        discovered.retain(|(adapter, path)| seen.insert((adapter.descriptor().kind, path.clone())));
        discovered
    }

    pub(crate) fn reconcile(&mut self, cache: &SpendingDiskCache, now_secs: u64) {
        for state in self.adapters.values_mut() {
            for source in &mut state.sources {
                source.reconcile(cache, now_secs);
            }
        }
    }

    pub(crate) fn last_scan_authoritative(&self) -> bool {
        self.last_authoritative
    }

    #[cfg(test)]
    pub(crate) fn mark_non_authoritative_for_test(&mut self) {
        self.last_authoritative = false;
    }

    fn complete_due(&self) -> bool {
        if self.force_complete {
            return true;
        }
        self.last_complete
            .is_some_and(|last| last.elapsed() >= COMPLETE_RECONCILE_INTERVAL)
    }

    #[cfg(test)]
    pub(crate) fn force_complete_for_test(&mut self) {
        self.force_complete = true;
    }

    #[cfg(test)]
    pub(crate) fn stats_for_test(&self) -> DiscoveryStats {
        self.stats
    }

    #[cfg(test)]
    pub(crate) fn discover_sources_for_test(
        &mut self,
        sources: Vec<SpendingSource>,
        now_secs: u64,
    ) -> Vec<PathBuf> {
        self.discover_declared_sources("test", sources, now_secs)
    }

    #[cfg(feature = "testkit")]
    pub(crate) fn discover_sources_for_testkit(
        &mut self,
        sources: Vec<SpendingSource>,
        now_secs: u64,
    ) -> Vec<PathBuf> {
        self.discover_declared_sources("testkit", sources, now_secs)
    }

    #[cfg(any(test, feature = "testkit"))]
    fn discover_declared_sources(
        &mut self,
        kind: &'static str,
        sources: Vec<SpendingSource>,
        now_secs: u64,
    ) -> Vec<PathBuf> {
        self.stats = DiscoveryStats::default();
        let key = source_set_key(&sources);
        let changed = self.adapters.get(kind).is_none_or(|state| state.key != key);
        if changed {
            self.adapters.insert(
                kind,
                AdapterState {
                    key,
                    sources: sources.into_iter().map(SourceState::from).collect(),
                },
            );
        }
        let full = self.complete_due() && !changed;
        let (files, authoritative) = scan_adapter(
            self.adapters
                .get_mut(kind)
                .expect("declared state inserted"),
            now_secs,
            full,
            Some(&mut self.stats),
        );
        if (full || self.last_complete.is_none()) && authoritative {
            self.last_complete = Some(Instant::now());
            self.force_complete = false;
        }
        self.last_authoritative = authoritative;
        files
    }
}

fn source_set_key(sources: &[SpendingSource]) -> String {
    sources
        .iter()
        .map(SpendingSource::key)
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

impl From<SpendingSource> for SourceState {
    fn from(source: SpendingSource) -> Self {
        match source {
            SpendingSource::Exact(path) => Self::Exact(ExactState { path, known: None }),
            SpendingSource::Group(group) => Self::Group(GroupState {
                selection: group.selection,
                trees: group
                    .trees
                    .into_iter()
                    .map(|declaration| TreeState {
                        declaration,
                        root: DirectoryNode::default(),
                    })
                    .collect(),
            }),
        }
    }
}

fn scan_adapter(
    state: &mut AdapterState,
    now_secs: u64,
    full: bool,
    mut stats: Option<&mut DiscoveryStats>,
) -> (Vec<PathBuf>, bool) {
    let mut files = Vec::new();
    let mut authoritative = true;
    for source in &mut state.sources {
        let (mut source_files, complete) = source.scan(now_secs, full, stats.as_deref_mut());
        authoritative &= complete;
        files.append(&mut source_files);
    }
    files.sort();
    files.dedup();
    (files, authoritative)
}

impl SourceState {
    fn scan(
        &mut self,
        now_secs: u64,
        full: bool,
        stats: Option<&mut DiscoveryStats>,
    ) -> (Vec<PathBuf>, bool) {
        match self {
            Self::Exact(state) => state.scan(now_secs, full, stats),
            Self::Group(state) => state.scan(now_secs, full, stats),
        }
    }

    fn reconcile(&mut self, cache: &SpendingDiskCache, now_secs: u64) {
        match self {
            Self::Exact(state) => {
                if let Some(known) = state.known.as_mut()
                    && let Some(entry) = cache.files.get(&state.path.to_string_lossy().into_owned())
                {
                    known.active =
                        !cold_parse_out_of_window(entry.stat.newest_mtime_secs(), now_secs);
                }
            }
            Self::Group(state) => {
                for tree in &mut state.trees {
                    reconcile_node(&tree.declaration.root, &mut tree.root, cache, now_secs);
                }
            }
        }
    }
}

impl ExactState {
    fn scan(
        &mut self,
        now_secs: u64,
        full: bool,
        stats: Option<&mut DiscoveryStats>,
    ) -> (Vec<PathBuf>, bool) {
        if self.known.is_none() || full {
            count_candidate_stat(stats);
            match std::fs::metadata(&self.path) {
                Ok(meta) if meta.is_file() => {
                    self.known = Some(KnownFile {
                        active: metadata_is_active(&meta, now_secs),
                    });
                }
                Ok(_) => self.known = None,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.known = None;
                    return (Vec::new(), true);
                }
                Err(_) => return (self.active_paths(), false),
            }
        }
        (self.active_paths(), true)
    }

    fn active_paths(&self) -> Vec<PathBuf> {
        self.known
            .is_some_and(|known| known.active)
            .then(|| self.path.clone())
            .into_iter()
            .collect()
    }
}

impl GroupState {
    fn scan(
        &mut self,
        now_secs: u64,
        full: bool,
        mut stats: Option<&mut DiscoveryStats>,
    ) -> (Vec<PathBuf>, bool) {
        let mut by_relative = BTreeMap::<PathBuf, PathBuf>::new();
        let mut authoritative = true;
        for tree in &mut self.trees {
            let complete = scan_directory(
                &tree.declaration,
                Path::new(""),
                &mut tree.root,
                now_secs,
                full,
                true,
                stats.as_deref_mut(),
            );
            authoritative &= complete;
            let mut tree_files = Vec::new();
            collect_active_files(Path::new(""), &tree.root, &mut tree_files);
            tree_files.sort();
            if matches!(self.selection, GroupSelection::FirstPath)
                && let Some(relative) = tree_files.first()
            {
                return (vec![tree.declaration.root.join(relative)], authoritative);
            }
            for relative in tree_files {
                by_relative
                    .entry(relative.clone())
                    .or_insert_with(|| tree.declaration.root.join(relative));
            }
        }
        (by_relative.into_values().collect(), authoritative)
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_directory(
    tree: &SpendingSourceTree,
    relative: &Path,
    node: &mut DirectoryNode,
    now_secs: u64,
    full: bool,
    is_root: bool,
    mut stats: Option<&mut DiscoveryStats>,
) -> bool {
    if !full && !is_root && !node.active_frontier && !node.due {
        return true;
    }
    let path = tree.root.join(relative);
    count_directory_stat(stats.as_deref_mut());
    let before_meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && node.files.is_empty()
                && node.children.is_empty() =>
        {
            node.due = true;
            return true;
        }
        Err(_) => {
            node.due = true;
            return false;
        }
    };
    let Ok(before_stamp) = before_meta.modified() else {
        node.due = true;
        return false;
    };
    if !before_meta.is_dir() {
        node.due = true;
        return false;
    }

    let must_enumerate = full || node.stamp != Some(before_stamp) || node.due;
    let mut authoritative = true;
    if must_enumerate {
        let prior = node.clone();
        count_read_dir(stats.as_deref_mut());
        let Ok(entries) = std::fs::read_dir(&path) else {
            node.due = true;
            return false;
        };
        let mut seen_files = BTreeSet::new();
        let mut seen_dirs = BTreeSet::new();
        let mut complete_entries = true;
        for entry in entries {
            let Ok(entry) = entry else {
                complete_entries = false;
                continue;
            };
            let name = PathBuf::from(entry.file_name());
            let child_relative = relative.join(&name);
            let Ok(kind) = entry.file_type() else {
                complete_entries = false;
                continue;
            };
            if kind.is_dir() {
                if !tree.should_descend(&child_relative) {
                    continue;
                }
                seen_dirs.insert(name.clone());
                let child = node.children.entry(name).or_insert_with(|| DirectoryNode {
                    due: true,
                    ..DirectoryNode::default()
                });
                if !full
                    && tree.codex_date_partitions
                    && codex_partition_is_old(&child_relative, now_secs)
                    && !child.active_frontier
                {
                    continue;
                }
                authoritative &= scan_directory(
                    tree,
                    &child_relative,
                    child,
                    now_secs,
                    full,
                    false,
                    stats.as_deref_mut(),
                );
            } else if kind.is_file() && tree.matches(&child_relative) {
                seen_files.insert(name.clone());
                let should_stat = full || !node.files.contains_key(&name);
                if should_stat {
                    count_candidate_stat(stats.as_deref_mut());
                    match entry.metadata() {
                        Ok(meta) => {
                            node.files.insert(
                                name,
                                KnownFile {
                                    active: metadata_is_active(&meta, now_secs),
                                },
                            );
                        }
                        Err(_) => complete_entries = false,
                    }
                }
            }
        }
        count_directory_stat(stats.as_deref_mut());
        let after_stamp = std::fs::metadata(&path)
            .ok()
            .and_then(|meta| meta.modified().ok());
        let Some(after_stamp) = after_stamp else {
            *node = prior;
            node.due = true;
            return false;
        };
        if complete_entries {
            node.files.retain(|name, _| seen_files.contains(name));
            node.children.retain(|name, _| seen_dirs.contains(name));
        } else {
            authoritative = false;
        }
        node.stamp = Some(after_stamp);
        node.due = before_stamp != after_stamp || !complete_entries;
    } else {
        let active_children = node
            .children
            .iter_mut()
            .filter(|(_, child)| child.active_frontier)
            .collect::<Vec<_>>();
        for (name, child) in active_children {
            authoritative &= scan_directory(
                tree,
                &relative.join(name),
                child,
                now_secs,
                full,
                false,
                stats.as_deref_mut(),
            );
        }
    }
    refresh_frontier(node);
    authoritative
}

impl SpendingSourceTree {
    fn should_descend(&self, relative: &Path) -> bool {
        self.descend_filter
            .is_none_or(|(_, filter)| filter(relative))
    }
}

fn refresh_frontier(node: &mut DirectoryNode) -> bool {
    for child in node.children.values_mut() {
        refresh_frontier(child);
    }
    node.active_frontier = node.files.values().any(|file| file.active)
        || node.children.values().any(|child| child.active_frontier);
    node.active_frontier
}

fn collect_active_files(relative: &Path, node: &DirectoryNode, out: &mut Vec<PathBuf>) {
    out.extend(
        node.files
            .iter()
            .filter(|(_, known)| known.active)
            .map(|(name, _)| relative.join(name)),
    );
    for (name, child) in &node.children {
        collect_active_files(&relative.join(name), child, out);
    }
}

fn reconcile_node(root: &Path, node: &mut DirectoryNode, cache: &SpendingDiskCache, now_secs: u64) {
    for (name, known) in &mut node.files {
        let path = root.join(name);
        if let Some(entry) = cache.files.get(&path.to_string_lossy().into_owned()) {
            known.active = !cold_parse_out_of_window(entry.stat.newest_mtime_secs(), now_secs);
        }
    }
    for (name, child) in &mut node.children {
        reconcile_node(&root.join(name), child, cache, now_secs);
    }
    refresh_frontier(node);
}

fn metadata_is_active(meta: &std::fs::Metadata, now_secs: u64) -> bool {
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs());
    !cold_parse_out_of_window(mtime_secs, now_secs)
}

fn codex_partition_is_old(relative: &Path, now_secs: u64) -> bool {
    let mut components = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        });
    let Some(year) = components
        .next()
        .and_then(|value| value.parse::<i16>().ok())
    else {
        return false;
    };
    let Some(month) = components.next().and_then(|value| value.parse::<i8>().ok()) else {
        return false;
    };
    let Some(day) = components.next().and_then(|value| value.parse::<i8>().ok()) else {
        return false;
    };
    let Ok(partition) = jiff::civil::Date::new(year, month, day) else {
        return false;
    };
    let horizon =
        super::aggregate::WIDEST_SPEND_WINDOW_SECS + super::aggregate::SKIP_PARSE_MARGIN_SECS;
    let cutoff_secs = now_secs.saturating_sub(horizon).min(i64::MAX as u64) as i64;
    let Ok(cutoff) = jiff::Timestamp::from_second(cutoff_secs) else {
        return false;
    };
    partition < cutoff.to_zoned(jiff::tz::TimeZone::UTC).date()
}

fn count_directory_stat(stats: Option<&mut DiscoveryStats>) {
    #[cfg(test)]
    if let Some(stats) = stats {
        stats.directory_stats += 1;
    }
    #[cfg(not(test))]
    let _ = stats;
}

fn count_read_dir(stats: Option<&mut DiscoveryStats>) {
    #[cfg(test)]
    if let Some(stats) = stats {
        stats.read_dirs += 1;
    }
    #[cfg(not(test))]
    let _ = stats;
}

fn count_candidate_stat(stats: Option<&mut DiscoveryStats>) {
    #[cfg(test)]
    if let Some(stats) = stats {
        stats.candidate_stats += 1;
    }
    #[cfg(not(test))]
    let _ = stats;
}
