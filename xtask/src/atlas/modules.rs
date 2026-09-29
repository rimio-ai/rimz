use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use super::syntax::{FileSyntax, FnBody, ModIndex, PrivateItem, PubItem};

pub(super) const EXTERNAL_REACH: &str = "(extern)";

#[derive(Debug)]
pub(super) struct BinaryTargets {
    modules: BTreeMap<PathBuf, BTreeSet<String>>,
}

impl BinaryTargets {
    pub(super) fn new(files: &[FileSyntax]) -> Self {
        let declared = |path: &Path| {
            files
                .iter()
                .find(|file| file.path == path)
                .into_iter()
                .flat_map(|file| file.mod_decls.iter().map(|(module, _)| module.clone()))
                .collect::<BTreeSet<_>>()
        };
        let modules = files
            .iter()
            .filter(|file| file.path.ends_with("src/main.rs"))
            .map(|main| {
                let library = declared(&main.crate_path.join("src/lib.rs"));
                let binary = declared(&main.path).difference(&library).cloned().collect();
                (main.crate_path.clone(), binary)
            })
            .collect();
        Self { modules }
    }

    pub(super) fn contains(&self, path: &Path) -> bool {
        let path = scope_for_matching(path);
        let root = crate_path_for_source(path);
        if path == root.join("src/main.rs") || path.starts_with(root.join("src/bin")) {
            return true;
        }
        let module = crate_module_for_path(path);
        self.modules
            .get(&root)
            .is_some_and(|modules| modules.contains(module.split("::").next().unwrap_or(&module)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct ItemId {
    pub(super) module: String,
    pub(super) kind: String,
    pub(super) name: String,
}

#[derive(Clone, Debug)]
pub(super) struct EscapingItem {
    pub(super) id: ItemId,
    pub(super) path: PathBuf,
    pub(super) line: usize,
}

/// Keeps the matches defined in `module` itself when there are any, else
/// every match: a key names its own module first, then beneath it.
fn nearest<'a, T>(
    matches: Vec<(&'a FileSyntax, &'a T)>,
    in_module: impl Fn(&FileSyntax, &T) -> bool,
) -> Vec<(&'a FileSyntax, &'a T)> {
    let exact = matches
        .iter()
        .filter(|(file, item)| in_module(file, item))
        .copied()
        .collect::<Vec<_>>();
    if exact.is_empty() { matches } else { exact }
}

/// Every public item a `module::Name` key names: `Name` defined in `module`
/// or any module beneath it, so `message::queue_synthetic` finds
/// `message::deliver::queue_synthetic`. A free item wins over a method of
/// the same name (`prefer_free`), then definitions in the named module
/// itself win over deeper ones; a bare `Name` searches the whole crate.
pub(super) fn items_for_key<'a>(
    files: &'a [FileSyntax],
    key: &str,
) -> Vec<(&'a FileSyntax, &'a PubItem)> {
    let (module, name) = key.rsplit_once("::").unwrap_or(("", key));
    let matches = files
        .iter()
        .flat_map(|file| {
            file.pub_items
                .iter()
                .filter(|item| item.name == name && module_is_within(&item.module, module))
                .map(move |item| (file, item))
        })
        .collect::<Vec<_>>();
    nearest(prefer_free(matches, |item| item.member), |_, item| {
        item.module == module
    })
}

/// Rust path semantics for a name-only key: it names a free item (anything
/// outside an `impl` or trait) first, and a method only when no free item
/// matches.
fn prefer_free<'a, T>(
    matches: Vec<(&'a FileSyntax, &'a T)>,
    member: impl Fn(&T) -> bool,
) -> Vec<(&'a FileSyntax, &'a T)> {
    if matches.iter().any(|(_, item)| !member(item)) {
        matches
            .into_iter()
            .filter(|(_, item)| !member(item))
            .collect()
    } else {
        matches
    }
}

/// Rust path semantics for an owner-qualified key: `Owner::name` names an
/// inherent method first, and a method of a trait `impl` for `Owner` only when
/// no inherent method matches.
fn prefer_inherent<'a>(
    matches: Vec<(&'a FileSyntax, &'a FnBody)>,
) -> Vec<(&'a FileSyntax, &'a FnBody)> {
    if matches.iter().any(|(_, function)| !function.trait_impl) {
        matches
            .into_iter()
            .filter(|(_, function)| !function.trait_impl)
            .collect()
    } else {
        matches
    }
}

/// Every production function a `module::name` or `module::Owner::name` key
/// names, under the key's module. Read as `module::name`, a free function
/// wins over a method (`prefer_free`); read as `module::Owner::name`, an
/// inherent method wins over a trait-impl one (`prefer_inherent`); definitions in the key's module
/// itself win over deeper ones.
pub(super) fn functions_for_key<'a>(
    files: &'a [FileSyntax],
    key: &str,
) -> Vec<(&'a FileSyntax, &'a FnBody)> {
    let (module, name) = key.rsplit_once("::").unwrap_or(("", key));
    let owned = module.rsplit_once("::").map_or(("", module), |split| split);
    // (module the definition must sit under, required owner)
    let readings = [(module, None), (owned.0, Some(owned.1))];
    let owner_fits = |owner: Option<&str>, function: &FnBody| {
        owner.is_none_or(|owner| function.owner.as_deref() == Some(owner))
    };
    let named = files
        .iter()
        .flat_map(|file| file.fns.iter().map(move |function| (file, function)))
        .filter(|(_, function)| function.name == name)
        .collect::<Vec<_>>();
    // Both readings follow Rust path semantics: a name-only key names a free
    // function before a method (`prefer_free`), and `Owner::name` names an
    // inherent method before a trait-impl one (`prefer_inherent`).
    let name_only = prefer_free(
        named
            .iter()
            .copied()
            .filter(|(_, function)| module_is_within(&function.module, readings[0].0))
            .collect(),
        |function| function.member,
    );
    let owned = prefer_inherent(
        named
            .iter()
            .copied()
            .filter(|(_, function)| {
                module_is_within(&function.module, readings[1].0)
                    && owner_fits(readings[1].1, function)
            })
            .collect(),
    );
    let matches = named
        .into_iter()
        .filter(|(_, function)| {
            name_only
                .iter()
                .chain(&owned)
                .any(|(_, kept)| std::ptr::eq(*kept, *function))
        })
        .collect::<Vec<_>>();
    nearest(matches, |_, function| {
        readings
            .iter()
            .any(|(module, owner)| function.module == *module && owner_fits(*owner, function))
    })
}

/// Every private production item (type, trait, const, static, type alias) a
/// `module::Name` key names, under the key's module; definitions in that
/// module itself win.
fn private_items_for_key<'a>(
    files: &'a [FileSyntax],
    key: &str,
) -> Vec<(&'a FileSyntax, &'a PrivateItem)> {
    let (module, name) = key.rsplit_once("::").unwrap_or(("", key));
    let matches = files
        .iter()
        .flat_map(|file| {
            file.private_items
                .iter()
                .filter(|item| item.name == name && module_is_within(&item.module, module))
                .map(move |item| (file, item))
        })
        .collect::<Vec<_>>();
    nearest(matches, |_, item| item.module == module)
}

/// What a key resolved to, one variant per resolution tier.
#[derive(Clone, Copy, Debug)]
pub(super) enum Resolved<'a> {
    /// A boundary-visible item, `pub use` re-exports included.
    Visible(&'a PubItem),
    /// A production function or method no visible item covers.
    Function(&'a FnBody),
    /// A private non-function item.
    Private(&'a PrivateItem),
}

impl Resolved<'_> {
    /// What an ambiguity message calls several definitions of this tier.
    pub(super) fn noun(self) -> &'static str {
        match self {
            Self::Visible(_) => "visible items",
            Self::Function(_) => "production functions",
            Self::Private(_) => "private items",
        }
    }
}

/// The production function a visible `fn` item declares. A function's span
/// starts at its attributes and doc comments, the item's line at its name,
/// so the item sits inside the function's span rather than on its line.
fn visible_function<'a>(file: &'a FileSyntax, item: &PubItem) -> Option<&'a FnBody> {
    (item.kind == "fn")
        .then(|| {
            file.fns
                .iter()
                .filter(|function| {
                    function.name == item.name
                        && function.line <= item.line
                        && item.line <= function.end_line
                })
                .max_by_key(|function| function.line)
        })
        .flatten()
}

/// One definition a key resolves to.
#[derive(Clone, Copy, Debug)]
pub(super) struct KeyDefinition<'a> {
    pub(super) file: &'a FileSyntax,
    pub(super) line: usize,
    /// The `impl` owner when the key resolved to a method.
    pub(super) owner: Option<&'a str>,
    /// The build-configuration predicate gating the definition.
    pub(super) cfg: Option<&'a str>,
    pub(super) resolved: Resolved<'a>,
}

impl<'a> KeyDefinition<'a> {
    /// The production function this definition is, if it is one.
    pub(super) fn function(&self) -> Option<&'a FnBody> {
        match self.resolved {
            Resolved::Visible(item) => visible_function(self.file, item),
            Resolved::Function(function) => Some(function),
            Resolved::Private(_) => None,
        }
    }
}

/// Every definition an item key names, whatever its spelling: `Name`,
/// `module::Name`, or `module::Owner::name`. The tiers resolve in order and
/// the first with a match wins: the visibility-qualified items
/// `items_for_key` finds, then the production functions `functions_for_key`
/// finds, then private non-function items. Each tier searches the key's
/// module first, then beneath it. Build-configuration alternatives count as
/// one definition (`collapse_cfg_alternatives`); several definitions left
/// are an ambiguity the caller reports by its own rule.
pub(super) fn definitions_for_key<'a>(
    files: &'a [FileSyntax],
    key: &str,
) -> Vec<KeyDefinition<'a>> {
    definitions_for_key_where(files, key, |_| true)
}

/// `definitions_for_key` over the files `keep` admits, applied before a tier
/// is chosen, so a tier with matches only outside them falls through.
pub(super) fn definitions_for_key_where<'a>(
    files: &'a [FileSyntax],
    key: &str,
    keep: impl Fn(&FileSyntax) -> bool,
) -> Vec<KeyDefinition<'a>> {
    collapse_cfg_alternatives(tier_definitions(files, key, keep))
}

/// Several definitions that each carry a build-configuration predicate, all
/// pairwise different, are one definition compiled per configuration (a
/// `pub use` under `cfg(feature = "sentry")` and its `not(...)` twin, a
/// `target_os` pair): they collapse to the first in file order. Same-cfg or
/// ungated definitions stay several.
fn collapse_cfg_alternatives(mut definitions: Vec<KeyDefinition<'_>>) -> Vec<KeyDefinition<'_>> {
    if definitions.len() < 2 {
        return definitions;
    }
    let predicates = definitions
        .iter()
        .map(|definition| definition.cfg)
        .collect::<Option<Vec<_>>>();
    let alternatives = predicates.is_some_and(|predicates| {
        predicates.iter().collect::<BTreeSet<_>>().len() == predicates.len()
    });
    if alternatives {
        definitions.sort_by(|left, right| {
            (&left.file.path, left.line).cmp(&(&right.file.path, right.line))
        });
        definitions.truncate(1);
    }
    definitions
}

fn tier_definitions<'a>(
    files: &'a [FileSyntax],
    key: &str,
    keep: impl Fn(&FileSyntax) -> bool,
) -> Vec<KeyDefinition<'a>> {
    let items = items_for_key(files, key)
        .into_iter()
        .filter(|(file, _)| keep(file))
        .map(|(file, item)| KeyDefinition {
            file,
            line: item.line,
            owner: visible_function(file, item).and_then(|function| function.owner.as_deref()),
            cfg: item.cfg.as_deref(),
            resolved: Resolved::Visible(item),
        })
        .collect::<Vec<_>>();
    if !items.is_empty() {
        return items;
    }
    let functions = functions_for_key(files, key)
        .into_iter()
        .filter(|(file, _)| keep(file))
        .map(|(file, function)| KeyDefinition {
            file,
            line: function.line,
            owner: function.owner.as_deref(),
            cfg: function.cfg.as_deref(),
            resolved: Resolved::Function(function),
        })
        .collect::<Vec<_>>();
    if !functions.is_empty() {
        return functions;
    }
    private_items_for_key(files, key)
        .into_iter()
        .filter(|(file, _)| keep(file))
        .map(|(file, item)| KeyDefinition {
            file,
            line: item.line,
            owner: None,
            cfg: item.cfg.as_deref(),
            resolved: Resolved::Private(item),
        })
        .collect()
}

/// Where a `use` item's references live once the SCIP index resolves them.
#[derive(Clone, Debug)]
pub(super) enum ReExport<'a> {
    /// One definition, possibly through a chain of re-exports.
    Definition(&'a FileSyntax, &'a PubItem),
    /// A glob: every public definition of the named module.
    Glob(Vec<(&'a FileSyntax, &'a PubItem)>),
    /// A module, or an item of a crate the index does not cover.
    Foreign,
    /// A known module that defines no such name.
    Unresolved,
}

const REEXPORT_DEPTH: usize = 8;

/// Follows a `use` item to the definition it re-exports. A definition is
/// never a `use` or a `mod`; chains and globs resolve up to a bounded depth.
pub(super) fn resolve_reexport<'a>(files: &'a [FileSyntax], item: &PubItem) -> ReExport<'a> {
    resolve_reexport_depth(files, item, REEXPORT_DEPTH)
}

fn known_module(files: &[FileSyntax], module: &str) -> bool {
    files.iter().any(|file| {
        file.module_path == module || file.pub_items.iter().any(|item| item.module == module)
    })
}

fn resolve_reexport_depth<'a>(
    files: &'a [FileSyntax],
    item: &PubItem,
    depth: usize,
) -> ReExport<'a> {
    let Some(target) = &item.target else {
        return ReExport::Foreign;
    };
    let Some(module) = target
        .modules
        .iter()
        .find(|module| known_module(files, module))
    else {
        return ReExport::Foreign;
    };
    if target.name == "*" {
        return ReExport::Glob(glob_definitions(files, module, depth));
    }
    let mut current = (module.clone(), target.name.clone());
    for _ in 0..depth {
        let found = files.iter().find_map(|file| {
            file.pub_items
                .iter()
                .find(|item| item.module == current.0 && item.name == current.1)
                .map(|item| (file, item))
        });
        let Some((file, item)) = found else {
            return if known_module(files, &current.0) {
                ReExport::Unresolved
            } else {
                ReExport::Foreign
            };
        };
        match item.kind.as_str() {
            "mod" => return ReExport::Foreign,
            "use" => {
                let Some(next) = &item.target else {
                    return ReExport::Foreign;
                };
                let Some(module) = next
                    .modules
                    .iter()
                    .find(|module| known_module(files, module))
                else {
                    return ReExport::Foreign;
                };
                // A name passing through a glob keeps its own name there.
                let name = if next.name == "*" {
                    current.1
                } else {
                    next.name.clone()
                };
                current = (module.clone(), name);
            }
            _ => return ReExport::Definition(file, item),
        }
    }
    ReExport::Unresolved
}

/// Every definition a glob of `module` exports: its own definitions plus
/// what its own re-exports resolve to.
fn glob_definitions<'a>(
    files: &'a [FileSyntax],
    module: &str,
    depth: usize,
) -> Vec<(&'a FileSyntax, &'a PubItem)> {
    let mut definitions = Vec::new();
    for file in files {
        for item in file.pub_items.iter().filter(|item| item.module == module) {
            match item.kind.as_str() {
                "mod" => {}
                "use" => {
                    if depth == 0 {
                        continue;
                    }
                    match resolve_reexport_depth(files, item, depth - 1) {
                        ReExport::Definition(file, item) => definitions.push((file, item)),
                        ReExport::Glob(items) => definitions.extend(items),
                        ReExport::Foreign | ReExport::Unresolved => {}
                    }
                }
                _ => definitions.push((file, item)),
            }
        }
    }
    definitions
}

pub(super) fn item_escapes(
    file: &FileSyntax,
    item: &PubItem,
    target_module: &str,
    mod_index: &ModIndex,
) -> bool {
    !module_is_within(&mod_index.effective_reach(file, item), target_module)
}

pub(super) fn escaping_items(
    files: &[&FileSyntax],
    scope: &Path,
    mod_index: &ModIndex,
) -> BTreeMap<String, Vec<EscapingItem>> {
    let mut files_by_row = BTreeMap::<String, Vec<&FileSyntax>>::new();
    for file in files {
        let row = module_for_path(&file.path, scope);
        files_by_row.entry(row).or_default().push(*file);
    }
    files_by_row
        .into_iter()
        .map(|(row, files)| {
            let target_module = crate_module_for_row(scope, &row);
            (
                row,
                escaping_items_for_boundary(&files, &target_module, mod_index),
            )
        })
        .collect()
}

pub(super) fn escaping_items_for_boundary(
    files: &[&FileSyntax],
    target_module: &str,
    mod_index: &ModIndex,
) -> Vec<EscapingItem> {
    files
        .iter()
        .flat_map(|file| file.pub_items.iter().map(move |item| (*file, item)))
        .filter(|(file, item)| item_escapes(file, item, target_module, mod_index))
        .map(|(file, item)| EscapingItem {
            id: ItemId {
                module: item.module.clone(),
                kind: item.kind.clone(),
                name: item.name.clone(),
            },
            path: file.path.clone(),
            line: item.line,
        })
        .collect()
}

pub(super) fn scope_for_matching(scope: &Path) -> &Path {
    match scope.strip_prefix(Path::new(".")) {
        Ok(scope) if scope.as_os_str().is_empty() => Path::new(""),
        Ok(scope) => scope,
        Err(_) => scope,
    }
}

pub(super) fn module_for_path(path: &Path, scope: &Path) -> String {
    let relative = path.strip_prefix(scope_for_matching(scope)).unwrap_or(path);
    let mut components = relative.components();
    let Some(first) = components.next() else {
        return file_module(path);
    };
    if components.next().is_some() {
        first.as_os_str().to_string_lossy().into_owned()
    } else if crate::source_files::is_test_file(relative) {
        "(root)".to_owned()
    } else {
        file_module(relative)
    }
}

pub(super) fn rust_module_for_path(path: &Path, scope: &Path) -> Option<String> {
    (path.extension().and_then(std::ffi::OsStr::to_str) == Some("rs"))
        .then(|| module_for_path(path, scope))
}

pub(super) fn file_module(path: &Path) -> String {
    let Some(name) = path.file_name() else {
        return "(root)".to_owned();
    };
    let name = name.to_string_lossy();
    match name.as_ref() {
        "lib.rs" | "main.rs" | "mod.rs" => "(root)".to_owned(),
        _ => name.strip_suffix(".rs").unwrap_or(&name).to_owned(),
    }
}

pub(super) fn crate_module_for_path(path: &Path) -> String {
    let mut after_src = false;
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        let component = component.to_string_lossy();
        if after_src {
            parts.push(component.into_owned());
        } else if component == "src" {
            after_src = true;
        }
    }
    if !after_src {
        let parent = path
            .parent()
            .and_then(Path::file_name)
            .map(|component| component.to_string_lossy());
        let stem = path.file_stem().map(|stem| stem.to_string_lossy());
        return match (parent, stem) {
            (Some(parent), Some(stem)) => format!("{parent}::{stem}"),
            (None, Some(stem)) => stem.into_owned(),
            _ => String::new(),
        };
    }
    if let Some(last) = parts.last_mut() {
        if matches!(last.as_str(), "lib.rs" | "main.rs" | "mod.rs") {
            parts.pop();
        } else if let Some(stem) = last.strip_suffix(".rs") {
            *last = stem.to_owned();
        }
    }
    parts.join("::")
}

pub(super) fn is_declaration_only(kind: &str) -> bool {
    matches!(kind, "mod" | "use")
}

pub(super) fn crate_path_for_source(path: &Path) -> PathBuf {
    let mut crate_path = PathBuf::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        if component == "src" {
            return crate_path;
        }
        crate_path.push(component);
    }
    path.parent().unwrap_or_else(|| Path::new("")).to_path_buf()
}

pub(super) fn module_is_within(module: &str, ancestor: &str) -> bool {
    if ancestor == EXTERNAL_REACH {
        return true;
    }
    if module == EXTERNAL_REACH {
        return false;
    }
    ancestor.is_empty() || module == ancestor || module.starts_with(&format!("{ancestor}::"))
}

pub(super) fn module_endpoint(module: &str, scope_module: &str) -> String {
    let module = if module.is_empty() { "(crate)" } else { module };
    let top = |module: &str| {
        module
            .split("::")
            .next()
            .filter(|module| !module.is_empty())
            .unwrap_or("(root)")
            .to_owned()
    };
    if scope_module.is_empty() {
        return if module == "(crate)" {
            "(root)".to_owned()
        } else {
            top(module)
        };
    }
    if module == scope_module {
        return "(root)".to_owned();
    }
    if let Some(relative) = module
        .strip_prefix(scope_module)
        .and_then(|relative| relative.strip_prefix("::"))
    {
        return top(relative);
    }
    top(module)
}

pub(super) fn reference_module_label(module: &str, scope_module: &str) -> String {
    if module.is_empty() || module == "(crate)" {
        module_endpoint(module, scope_module)
    } else {
        module.to_owned()
    }
}

pub(super) fn crate_module_for_row(scope: &Path, row: &str) -> String {
    let scope_entry = if scope.extension().is_some_and(|extension| extension == "rs") {
        scope.to_path_buf()
    } else {
        scope.join("mod.rs")
    };
    let scope_module = crate_module_for_path(&scope_entry);
    if row == "(root)" {
        scope_module
    } else if scope_module.is_empty() {
        row.to_owned()
    } else {
        format!("{scope_module}::{row}")
    }
}

pub(super) fn path_in_scope(path: &Path, scope: &Path) -> bool {
    path.starts_with(scope_for_matching(scope))
}

pub(super) fn bounded_names(items: &[String], top: usize) -> String {
    let mut rendered = items
        .iter()
        .take(top)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if items.len() > top {
        rendered.push_str(&format!(" … {} more", items.len() - top));
    }
    rendered
}

pub(super) fn workspace_crate_names(root: &Path) -> Result<BTreeSet<String>> {
    let manifest_path = root.join("Cargo.toml");
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: toml::Value =
        toml::from_str(&raw).with_context(|| format!("parsing {}", manifest_path.display()))?;
    let mut names = BTreeSet::new();
    if let Some(name) = crate_name_from_manifest(&raw)? {
        names.insert(name);
    }
    let members = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str);
    for member in members {
        for directory in workspace_member_directories(root, member)? {
            let path = directory.join("Cargo.toml");
            let raw =
                fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            if let Some(name) = crate_name_from_manifest(&raw)
                .with_context(|| format!("parsing {}", path.display()))?
            {
                names.insert(name);
            }
        }
    }
    Ok(names)
}

fn workspace_member_directories(root: &Path, member: &str) -> Result<Vec<PathBuf>> {
    let Some(parent) = member.strip_suffix("/*") else {
        return Ok(vec![root.join(member)]);
    };
    let directory = root.join(parent);
    let mut members = fs::read_dir(&directory)
        .with_context(|| format!("reading {}", directory.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.join("Cargo.toml").is_file())
        .collect::<Vec<_>>();
    members.sort();
    Ok(members)
}

fn crate_name_from_manifest(raw: &str) -> Result<Option<String>> {
    let manifest: toml::Value = toml::from_str(raw)?;
    let name = manifest
        .get("lib")
        .and_then(|lib| lib.get("name"))
        .and_then(toml::Value::as_str)
        .or_else(|| {
            manifest
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
        });
    Ok(name.map(|name| name.replace('-', "_")))
}

#[cfg(test)]
#[path = "modules/tests.rs"]
mod tests;
