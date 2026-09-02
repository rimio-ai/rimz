use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::source_files;

use super::history::Log;
use super::index;
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
    pub(super) references: bool,
}

#[derive(Debug)]
pub(super) struct Facts {
    pub(super) root: PathBuf,
    pub(super) scope: PathBuf,
    pub(super) sources: Vec<Source>,
    pub(super) syntax: SyntaxReport,
    pub(super) mod_index: ModIndex,
    pub(super) known_modules: BTreeSet<String>,
    /// Every identifier the workspace defines in production code: boundary
    /// visible items, function names, impl owners, and module names. Family
    /// detectors use it to tell crate vocabulary from std vocabulary without
    /// a SCIP index.
    pub(super) defined_names: BTreeSet<String>,
    /// Modules that define each production identifier. Shape-family gates use
    /// it to distinguish duplicated knowledge from use of one module's API.
    pub(super) defining_modules: BTreeMap<String, BTreeSet<String>>,
    /// Struct fields exactly one struct in the workspace declares: a guard
    /// naming one reads one type's state rather than a common field name.
    pub(super) unique_fields: BTreeSet<String>,
    /// Top-level modules only a `main.rs` declares. Callers there sit outside
    /// the library crate, so nothing they reach can narrow below `pub`.
    pub(super) bin_modules: BTreeSet<String>,
    pub(super) crate_names: BTreeSet<String>,
    pub(super) sizes: BTreeMap<PathBuf, FileSize>,
    pub(super) history: Option<Log>,
    pub(super) metrics: Option<MetricsReport>,
    pub(super) references: Option<References>,
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
        Self::from_sources(root, scope, sources, facets, None)
    }

    pub(super) fn load_at(
        root: &Path,
        scope: &Path,
        reference: &str,
        references: bool,
    ) -> Result<Self> {
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
        Self::from_sources(
            root,
            scope,
            sources,
            Facets {
                references,
                ..Facets::default()
            },
            Some(reference),
        )
    }

    fn from_sources(
        root: &Path,
        scope: &Path,
        sources: Vec<Source>,
        facets: Facets,
        reference: Option<&str>,
    ) -> Result<Self> {
        let crate_names = workspace_crate_names(root)?;
        let syntax = syntax::analyze_sources(&sources, &crate_names);
        let mod_index = ModIndex::new(&syntax.files);
        let known_modules = syntax
            .files
            .iter()
            .map(|file| file.module_path.clone())
            .collect();
        let defined_names = defined_names(&syntax);
        let defining_modules = defining_modules(&syntax);
        let unique_fields = unique_fields(&syntax);
        let bin_modules = bin_modules(&syntax);
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
        let references = facets
            .references
            .then(|| {
                if let Some(reference) = reference {
                    index::ensure_revision(root, reference, &sources)
                } else {
                    index::ensure(root, &sources)
                }
            })
            .transpose()?
            .map(|path| References::load(&path, &syntax, &sources))
            .transpose()?;
        Ok(Self {
            root: root.to_path_buf(),
            scope: scope.to_path_buf(),
            sources,
            syntax,
            mod_index,
            known_modules,
            defined_names,
            defining_modules,
            unique_fields,
            bin_modules,
            crate_names,
            sizes,
            history,
            metrics,
            references,
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

pub(super) fn defined_names(syntax: &SyntaxReport) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for file in &syntax.files {
        names.extend(file.pub_items.iter().map(|item| item.name.clone()));
        for function in &file.fns {
            names.insert(function.name.clone());
            if let Some(owner) = &function.owner {
                names.insert(owner.clone());
            }
        }
        names.extend(
            file.mod_decls
                .iter()
                .filter_map(|(module, _)| module.rsplit("::").next().map(str::to_owned)),
        );
    }
    names
}

pub(super) fn defining_modules(syntax: &SyntaxReport) -> BTreeMap<String, BTreeSet<String>> {
    let mut modules = BTreeMap::<String, BTreeSet<String>>::new();
    for file in &syntax.files {
        for name in file
            .pub_items
            .iter()
            .map(|item| &item.name)
            .chain(file.fns.iter().map(|function| &function.name))
            .chain(
                file.fns
                    .iter()
                    .filter_map(|function| function.owner.as_ref()),
            )
        {
            modules
                .entry(name.clone())
                .or_default()
                .insert(file.module_path.clone());
        }
        for (module, _) in &file.mod_decls {
            if let Some(name) = module.rsplit("::").next() {
                modules
                    .entry(name.to_owned())
                    .or_default()
                    .insert(module.clone());
            }
        }
    }
    modules
}

pub(super) fn unique_fields(syntax: &SyntaxReport) -> BTreeSet<String> {
    let mut declaring_structs = BTreeMap::<&str, usize>::new();
    for field in syntax.files.iter().flat_map(|file| &file.struct_fields) {
        *declaring_structs.entry(field).or_default() += 1;
    }
    declaring_structs
        .into_iter()
        .filter(|(_, structs)| *structs == 1)
        .map(|(field, _)| field.to_owned())
        .collect()
}

pub(super) fn bin_modules(syntax: &SyntaxReport) -> BTreeSet<String> {
    let declared_by = |root: &str| {
        syntax
            .files
            .iter()
            .filter(|file| file.path.file_name().is_some_and(|name| name == root))
            .flat_map(|file| file.mod_decls.iter().map(|(module, _)| module.clone()))
            .collect::<BTreeSet<_>>()
    };
    declared_by("main.rs")
        .difference(&declared_by("lib.rs"))
        .cloned()
        .collect()
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
