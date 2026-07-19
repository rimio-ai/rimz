//! Process-local, frontier-bounded discovery of historical spend stores.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use glob::{MatchOptions, Pattern};

use crate::agents::AgentDefinition;

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

    /// The single-tree store: one root and one glob. Yields no source when the
    /// root does not resolve, so an adapter whose provider is not installed
    /// contributes nothing to discovery.
    pub fn tree(root: impl Into<PathBuf>, pattern: impl Into<String>) -> Vec<Self> {
        SpendingSourceTree::new(root, pattern)
            .map(|tree| Self::group(vec![tree]))
            .into_iter()
            .collect()
    }

    /// Select the first matching path from the first tree that has a match.
    /// This models stores such as OpenCode's preferred primary database and
    /// sorted per-channel fallback without teaching discovery provider names.
    pub fn first(trees: Vec<SpendingSourceTree>) -> Self {
        Self::Group(SpendingSourceGroup::first(trees))
    }

    pub(crate) fn fingerprint(&self) -> Vec<u8> {
        let mut fingerprint = Vec::new();
        match self {
            Self::Exact(path) => {
                fingerprint.push(0);
                encode_path(&mut fingerprint, path);
            }
            Self::Group(group) => {
                fingerprint.push(1);
                group.encode(&mut fingerprint);
            }
        }
        fingerprint
    }

    pub(crate) fn complete_files(&self) -> Vec<PathBuf> {
        match self {
            Self::Exact(path) => path.is_file().then(|| path.clone()).into_iter().collect(),
            Self::Group(group) => group.complete_files(),
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

    fn encode(&self, fingerprint: &mut Vec<u8>) {
        fingerprint.push(match self.selection {
            GroupSelection::AllRelativeFirst => 0,
            GroupSelection::FirstPath => 1,
        });
        encode_usize(fingerprint, self.trees.len());
        for tree in &self.trees {
            tree.encode(fingerprint);
        }
    }

    fn complete_files(&self) -> Vec<PathBuf> {
        let mut by_relative = BTreeMap::<PathBuf, PathBuf>::new();
        for tree in &self.trees {
            let tree_files = tree.complete_relative_files();
            if matches!(self.selection, GroupSelection::FirstPath)
                && let Some(relative) = tree_files.first()
            {
                return vec![tree.root.join(relative)];
            }
            for relative in tree_files {
                by_relative
                    .entry(relative.clone())
                    .or_insert_with(|| tree.root.join(relative));
            }
        }
        by_relative.into_values().collect()
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

    fn encode(&self, fingerprint: &mut Vec<u8>) {
        encode_path(fingerprint, &self.root);
        encode_bytes(fingerprint, self.pattern.as_bytes());
        fingerprint.push(u8::from(self.codex_date_partitions));
        encode_filter(fingerprint, self.filter);
        encode_filter(fingerprint, self.descend_filter);
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

    fn complete_relative_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        self.collect_complete(Path::new(""), &mut files);
        files.sort();
        files.dedup();
        files
    }

    fn collect_complete(&self, relative: &Path, files: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(self.root.join(relative)) else {
            return;
        };
        for entry in entries.filter_map(Result::ok) {
            let child_relative = relative.join(entry.file_name());
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() && self.should_descend(&child_relative) {
                self.collect_complete(&child_relative, files);
            } else if kind.is_file() && self.matches(&child_relative) {
                files.push(child_relative);
            }
        }
    }
}

fn encode_path(out: &mut Vec<u8>, path: &Path) {
    let normalized = crate::worktree::normalize_path_lexical(path);
    encode_bytes(out, normalized.as_os_str().as_encoded_bytes());
}

fn encode_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_le_bytes());
    out.extend_from_slice(value);
}

fn encode_usize(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u64).to_le_bytes());
}

fn encode_filter(out: &mut Vec<u8>, filter: Option<RelativeFilter>) {
    match filter {
        Some((name, _)) => {
            out.push(1);
            encode_bytes(out, name.as_bytes());
        }
        None => out.push(0),
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
    key: Vec<u8>,
    sources: Vec<SourceState>,
    frontier_generation: u64,
    reconcile_generation: u64,
    materialized: Option<MaterializedPaths>,
}

struct MaterializedPaths {
    frontier_generation: u64,
    reconcile_generation: u64,
    paths: Vec<PathBuf>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KnownFile {
    active: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DiscoveryStats {
    pub(crate) directory_stats: u64,
    pub(crate) read_dirs: u64,
    pub(crate) candidate_stats: u64,
    pub(crate) materializations: u64,
}

impl SpendingDiscoveryIndex {
    pub(crate) fn discover(
        &mut self,
        adapters: impl Iterator<Item = &'static AgentDefinition>,
        now_secs: u64,
    ) -> Vec<(&'static AgentDefinition, PathBuf)> {
        self.stats = DiscoveryStats::default();
        let force_complete = self.complete_due();
        let mut authoritative = true;
        let mut discovered = Vec::new();
        let mut seen_kinds = HashSet::new();
        for adapter in adapters {
            let kind = adapter.spec().kind;
            if !seen_kinds.insert(kind) {
                continue;
            }
            let declarations = adapter.spending_sources();
            let key = source_set_key(&declarations);
            let changed = self.adapters.get(kind).is_none_or(|state| state.key != key);
            if changed {
                self.adapters
                    .insert(kind, AdapterState::new(key, declarations));
            }
            let Some(state) = self.adapters.get_mut(kind) else {
                continue;
            };
            let full = force_complete && !changed;
            let before = self.stats.frontier_work();
            let complete = scan_adapter(state, now_secs, full, Some(&mut self.stats));
            if self.stats.frontier_work() != before {
                state.frontier_generation = state.frontier_generation.saturating_add(1);
            }
            authoritative &= complete;
            discovered.extend(
                state
                    .materialized_paths(&mut self.stats)
                    .iter()
                    .cloned()
                    .map(|path| (adapter, path)),
            );
        }
        if (force_complete || self.last_complete.is_none()) && authoritative {
            self.last_complete = Some(Instant::now());
            self.force_complete = false;
        }
        self.last_authoritative = authoritative;
        discovered
    }

    pub(crate) fn reconcile(&mut self, cache: &SpendingDiskCache, now_secs: u64) {
        for state in self.adapters.values_mut() {
            let mut changed = false;
            for source in &mut state.sources {
                changed |= source.reconcile(cache, now_secs);
            }
            if changed {
                state.reconcile_generation = state.reconcile_generation.saturating_add(1);
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
            self.adapters.insert(kind, AdapterState::new(key, sources));
        }
        let full = self.complete_due() && !changed;
        let state = self
            .adapters
            .get_mut(kind)
            .expect("declared state inserted");
        let before = self.stats.frontier_work();
        let authoritative = scan_adapter(state, now_secs, full, Some(&mut self.stats));
        if self.stats.frontier_work() != before {
            state.frontier_generation = state.frontier_generation.saturating_add(1);
        }
        let files = state.materialized_paths(&mut self.stats).to_vec();
        if (full || self.last_complete.is_none()) && authoritative {
            self.last_complete = Some(Instant::now());
            self.force_complete = false;
        }
        self.last_authoritative = authoritative;
        files
    }
}

impl DiscoveryStats {
    fn frontier_work(self) -> (u64, u64) {
        (self.read_dirs, self.candidate_stats)
    }
}

impl AdapterState {
    fn new(key: Vec<u8>, sources: Vec<SpendingSource>) -> Self {
        Self {
            key,
            sources: sources.into_iter().map(SourceState::from).collect(),
            frontier_generation: 1,
            reconcile_generation: 0,
            materialized: None,
        }
    }

    fn materialized_paths(&mut self, stats: &mut DiscoveryStats) -> &[PathBuf] {
        let current = (self.frontier_generation, self.reconcile_generation);
        let fresh = self.materialized.as_ref().is_some_and(|materialized| {
            (
                materialized.frontier_generation,
                materialized.reconcile_generation,
            ) == current
        });
        if !fresh {
            let mut paths = Vec::new();
            for source in &self.sources {
                source.collect_active_paths(&mut paths);
            }
            paths.sort();
            paths.dedup();
            self.materialized = Some(MaterializedPaths {
                frontier_generation: current.0,
                reconcile_generation: current.1,
                paths,
            });
            stats.materializations = stats.materializations.saturating_add(1);
        }
        &self
            .materialized
            .as_ref()
            .expect("materialized paths installed")
            .paths
    }
}

fn source_set_key(sources: &[SpendingSource]) -> Vec<u8> {
    let mut key = Vec::new();
    encode_usize(&mut key, sources.len());
    for source in sources {
        let fingerprint = source.fingerprint();
        encode_bytes(&mut key, &fingerprint);
    }
    key
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
) -> bool {
    let mut authoritative = true;
    for source in &mut state.sources {
        authoritative &= source.scan(now_secs, full, stats.as_deref_mut());
    }
    authoritative
}

impl SourceState {
    fn scan(&mut self, now_secs: u64, full: bool, stats: Option<&mut DiscoveryStats>) -> bool {
        match self {
            Self::Exact(state) => state.scan(now_secs, full, stats),
            Self::Group(state) => state.scan(now_secs, full, stats),
        }
    }

    fn collect_active_paths(&self, paths: &mut Vec<PathBuf>) {
        match self {
            Self::Exact(state) => paths.extend(state.active_paths()),
            Self::Group(state) => paths.extend(state.active_paths()),
        }
    }

    fn reconcile(&mut self, cache: &SpendingDiskCache, now_secs: u64) -> bool {
        match self {
            Self::Exact(state) => {
                let before = state.known;
                if let Some(known) = state.known.as_mut()
                    && let Some(entry) = cache.files.get(&state.path.to_string_lossy().into_owned())
                {
                    known.active =
                        !cold_parse_out_of_window(entry.stat.newest_mtime_secs(), now_secs);
                }
                state.known != before
            }
            Self::Group(state) => {
                let mut changed = false;
                for tree in &mut state.trees {
                    changed |=
                        reconcile_node(&tree.declaration.root, &mut tree.root, cache, now_secs);
                }
                changed
            }
        }
    }
}

impl ExactState {
    fn scan(&mut self, now_secs: u64, full: bool, stats: Option<&mut DiscoveryStats>) -> bool {
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
                    return true;
                }
                Err(_) => return false,
            }
        }
        true
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
    fn scan(&mut self, now_secs: u64, full: bool, mut stats: Option<&mut DiscoveryStats>) -> bool {
        let mut authoritative = true;
        for tree in &mut self.trees {
            authoritative &= DirectoryScanner {
                tree: &tree.declaration,
                now_secs,
                full,
                stats: stats.as_deref_mut(),
            }
            .scan(Path::new(""), &mut tree.root, true);
        }
        authoritative
    }

    fn active_paths(&self) -> Vec<PathBuf> {
        let mut by_relative = BTreeMap::<PathBuf, PathBuf>::new();
        for tree in &self.trees {
            let mut tree_files = Vec::new();
            collect_active_files(Path::new(""), &tree.root, &mut tree_files);
            tree_files.sort();
            if matches!(self.selection, GroupSelection::FirstPath)
                && let Some(relative) = tree_files.first()
            {
                return vec![tree.declaration.root.join(relative)];
            }
            for relative in tree_files {
                by_relative
                    .entry(relative.clone())
                    .or_insert_with(|| tree.declaration.root.join(relative));
            }
        }
        by_relative.into_values().collect()
    }
}

struct DirectoryScanner<'a, 's> {
    tree: &'a SpendingSourceTree,
    now_secs: u64,
    full: bool,
    stats: Option<&'s mut DiscoveryStats>,
}

enum DirectoryProbe {
    Ready(SystemTime),
    MissingEmpty,
    Failed,
}

#[derive(Default)]
struct DirectorySnapshot {
    seen_files: BTreeSet<PathBuf>,
    seen_dirs: BTreeSet<PathBuf>,
    entries_complete: bool,
    children_authoritative: bool,
}

impl DirectorySnapshot {
    fn new() -> Self {
        Self {
            entries_complete: true,
            children_authoritative: true,
            ..Self::default()
        }
    }
}

impl DirectoryScanner<'_, '_> {
    fn scan(&mut self, relative: &Path, node: &mut DirectoryNode, is_root: bool) -> bool {
        if self.skip(node, is_root) {
            return true;
        }
        let path = self.tree.root.join(relative);
        let before_stamp = match self.probe(&path, node) {
            DirectoryProbe::Ready(stamp) => stamp,
            DirectoryProbe::MissingEmpty => return true,
            DirectoryProbe::Failed => return false,
        };
        let authoritative = if self.full || node.stamp != Some(before_stamp) || node.due {
            self.enumerate(relative, &path, node, before_stamp)
        } else {
            self.scan_active_children(relative, node)
        };
        refresh_frontier(node);
        authoritative
    }

    fn skip(&self, node: &DirectoryNode, is_root: bool) -> bool {
        !self.full && !is_root && !node.active_frontier && !node.due
    }

    fn probe(&mut self, path: &Path, node: &mut DirectoryNode) -> DirectoryProbe {
        count_directory_stat(self.stats.as_deref_mut());
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => match meta.modified() {
                Ok(stamp) => DirectoryProbe::Ready(stamp),
                Err(_) => {
                    node.due = true;
                    DirectoryProbe::Failed
                }
            },
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && node.files.is_empty()
                    && node.children.is_empty() =>
            {
                node.due = true;
                DirectoryProbe::MissingEmpty
            }
            Ok(_) | Err(_) => {
                node.due = true;
                DirectoryProbe::Failed
            }
        }
    }

    fn enumerate(
        &mut self,
        relative: &Path,
        path: &Path,
        node: &mut DirectoryNode,
        before_stamp: SystemTime,
    ) -> bool {
        let prior = node.clone();
        count_read_dir(self.stats.as_deref_mut());
        let Ok(entries) = std::fs::read_dir(path) else {
            node.due = true;
            return false;
        };
        let mut snapshot = DirectorySnapshot::new();
        for entry in entries {
            let Ok(entry) = entry else {
                snapshot.entries_complete = false;
                continue;
            };
            self.visit_entry(relative, node, entry, &mut snapshot);
        }
        count_directory_stat(self.stats.as_deref_mut());
        let after_stamp = std::fs::metadata(path)
            .ok()
            .and_then(|meta| meta.modified().ok());
        let Some(after_stamp) = after_stamp else {
            *node = prior;
            node.due = true;
            return false;
        };
        if snapshot.entries_complete {
            node.files
                .retain(|name, _| snapshot.seen_files.contains(name));
            node.children
                .retain(|name, _| snapshot.seen_dirs.contains(name));
        }
        node.stamp = Some(after_stamp);
        node.due = before_stamp != after_stamp || !snapshot.entries_complete;
        snapshot.entries_complete && snapshot.children_authoritative
    }

    fn visit_entry(
        &mut self,
        relative: &Path,
        node: &mut DirectoryNode,
        entry: std::fs::DirEntry,
        snapshot: &mut DirectorySnapshot,
    ) {
        let name = PathBuf::from(entry.file_name());
        let child_relative = relative.join(&name);
        let Ok(kind) = entry.file_type() else {
            snapshot.entries_complete = false;
            return;
        };
        if kind.is_dir() {
            self.visit_directory(node, name, &child_relative, snapshot);
        } else if kind.is_file() && self.tree.matches(&child_relative) {
            self.visit_file(node, name, entry, snapshot);
        }
    }

    fn visit_directory(
        &mut self,
        node: &mut DirectoryNode,
        name: PathBuf,
        child_relative: &Path,
        snapshot: &mut DirectorySnapshot,
    ) {
        if !self.tree.should_descend(child_relative) {
            return;
        }
        snapshot.seen_dirs.insert(name.clone());
        let child = node.children.entry(name).or_insert_with(|| DirectoryNode {
            due: true,
            ..DirectoryNode::default()
        });
        let old_inactive_partition = !self.full
            && self.tree.codex_date_partitions
            && codex_partition_is_old(child_relative, self.now_secs)
            && !child.active_frontier;
        if !old_inactive_partition {
            snapshot.children_authoritative &= self.scan(child_relative, child, false);
        }
    }

    fn visit_file(
        &mut self,
        node: &mut DirectoryNode,
        name: PathBuf,
        entry: std::fs::DirEntry,
        snapshot: &mut DirectorySnapshot,
    ) {
        snapshot.seen_files.insert(name.clone());
        if !self.full && node.files.contains_key(&name) {
            return;
        }
        count_candidate_stat(self.stats.as_deref_mut());
        match entry.metadata() {
            Ok(meta) => {
                node.files.insert(
                    name,
                    KnownFile {
                        active: metadata_is_active(&meta, self.now_secs),
                    },
                );
            }
            Err(_) => snapshot.entries_complete = false,
        }
    }

    fn scan_active_children(&mut self, relative: &Path, node: &mut DirectoryNode) -> bool {
        let mut authoritative = true;
        for (name, child) in &mut node.children {
            if child.active_frontier {
                authoritative &= self.scan(&relative.join(name), child, false);
            }
        }
        authoritative
    }
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

fn reconcile_node(
    root: &Path,
    node: &mut DirectoryNode,
    cache: &SpendingDiskCache,
    now_secs: u64,
) -> bool {
    let mut changed = false;
    for (name, known) in &mut node.files {
        let path = root.join(name);
        if let Some(entry) = cache.files.get(&path.to_string_lossy().into_owned()) {
            let active = !cold_parse_out_of_window(entry.stat.newest_mtime_secs(), now_secs);
            changed |= known.active != active;
            known.active = active;
        }
    }
    for (name, child) in &mut node.children {
        changed |= reconcile_node(&root.join(name), child, cache, now_secs);
    }
    refresh_frontier(node);
    changed
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
    if let Some(stats) = stats {
        stats.directory_stats += 1;
    }
}

fn count_read_dir(stats: Option<&mut DiscoveryStats>) {
    if let Some(stats) = stats {
        stats.read_dirs += 1;
    }
}

fn count_candidate_stat(stats: Option<&mut DiscoveryStats>) {
    if let Some(stats) = stats {
        stats.candidate_stats += 1;
    }
}
