use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::source_files;

use super::history::{Blame, Log};
use super::index::{self, IndexPolicy};
use super::metrics::{self, MetricsReport};
use super::modules::{path_in_scope, workspace_crate_names};
use super::references::References;
use super::sources::{self, Source};
use super::syntax::{self, ModIndex, SyntaxReport};

#[derive(Clone, Debug, Default)]
pub(super) struct FileSize {
    pub(super) code: u64,
    pub(super) tests: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Facets {
    pub(super) history: bool,
    pub(super) metrics: bool,
    pub(super) references: Option<IndexPolicy>,
    pub(super) blame: bool,
}

#[derive(Debug)]
pub(super) struct Facts {
    pub(super) root: PathBuf,
    pub(super) scope: PathBuf,
    pub(super) sources: Vec<Source>,
    pub(super) syntax: SyntaxReport,
    pub(super) mod_index: ModIndex,
    pub(super) known_modules: BTreeSet<String>,
    pub(super) crate_names: BTreeSet<String>,
    pub(super) sizes: BTreeMap<PathBuf, FileSize>,
    pub(super) history: Option<Log>,
    pub(super) metrics: Option<MetricsReport>,
    pub(super) references: Option<References>,
    pub(super) blame: Option<Blame>,
}

impl Facts {
    pub(super) fn load(root: &Path, scope: &Path, facets: Facets) -> Result<Self> {
        let sources = sources::working_tree_rust_sources(root)?;
        if !sources
            .iter()
            .any(|source| path_in_scope(&source.path, scope))
        {
            bail!("no tracked Rust files under `{}`", scope.display());
        }
        Self::from_sources(root, scope, sources, facets)
    }

    pub(super) fn load_at(root: &Path, scope: &Path, reference: &str) -> Result<Self> {
        let sources = sources::revision_sources(root, reference)?;
        if !sources
            .iter()
            .any(|source| path_in_scope(&source.path, scope))
        {
            bail!(
                "no tracked Rust files under `{}` at `{reference}`",
                scope.display()
            );
        }
        Self::from_sources(root, scope, sources, Facets::default())
    }

    fn from_sources(
        root: &Path,
        scope: &Path,
        sources: Vec<Source>,
        facets: Facets,
    ) -> Result<Self> {
        let syntax = syntax::analyze_sources(&sources);
        let mod_index = ModIndex::new(&syntax.files);
        let known_modules = syntax
            .files
            .iter()
            .map(|file| file.module_path.clone())
            .collect();
        let crate_names = workspace_crate_names(root)?;
        let sizes = file_sizes(&sources, &syntax);
        let scoped_sources = sources
            .iter()
            .filter(|source| path_in_scope(&source.path, scope))
            .cloned()
            .collect::<Vec<_>>();
        let scoped_syntax = syntax
            .files
            .iter()
            .filter(|file| path_in_scope(&file.path, scope))
            .cloned()
            .collect::<Vec<_>>();
        let history = facets.history.then(|| Log::read(root, scope)).transpose()?;
        let metrics = facets
            .metrics
            .then(|| metrics::analyze(root, scope, &scoped_sources, &scoped_syntax))
            .transpose()?;
        let references = match facets.references {
            Some(IndexPolicy::Required) => {
                let path = index::ensure(root, &sources)?;
                Some(References::load(&path, &syntax, &sources)?)
            }
            Some(IndexPolicy::Skip) | None => None,
        };
        let blame = facets
            .blame
            .then(|| {
                let files = sources
                    .iter()
                    .filter(|source| source.is_production() && path_in_scope(&source.path, scope))
                    .map(|source| source.path.clone())
                    .collect::<Vec<_>>();
                Blame::read(
                    root,
                    &files,
                    std::thread::available_parallelism().map_or(1, usize::from),
                )
            })
            .transpose()?;
        Ok(Self {
            root: root.to_path_buf(),
            scope: scope.to_path_buf(),
            sources,
            syntax,
            mod_index,
            known_modules,
            crate_names,
            sizes,
            history,
            metrics,
            references,
            blame,
        })
    }

    pub(super) fn sources_in(&self, scope: &Path) -> Vec<Source> {
        self.sources
            .iter()
            .filter(|source| path_in_scope(&source.path, scope))
            .cloned()
            .collect()
    }
}

fn file_sizes(sources: &[Source], syntax: &SyntaxReport) -> BTreeMap<PathBuf, FileSize> {
    let mut sizes = BTreeMap::new();
    for source in sources {
        if !source.is_production() && !source.is_test() {
            continue;
        }
        let (code, tests) = if source.is_test() {
            (0, source_files::rust_sloc(&source.text))
        } else {
            let test_regions = syntax
                .files
                .iter()
                .find(|file| file.path == source.path)
                .map_or(&[][..], |file| file.test_regions.as_slice());
            source_files::split_rust_sloc(&source.text, test_regions)
        };
        sizes.insert(source.path.clone(), FileSize { code, tests });
    }
    sizes
}
