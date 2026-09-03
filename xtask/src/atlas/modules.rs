use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use super::syntax::{FileSyntax, ModIndex, PubItem};

pub(super) const EXTERNAL_REACH: &str = "(extern)";

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

/// Every public item a `module::Name` key names: `Name` defined in `module`
/// or any module beneath it, so `message::queue_synthetic` finds
/// `message::deliver::queue_synthetic`. Definitions in the named module
/// itself win over deeper ones, and a bare `Name` searches the whole crate.
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
    let exact = matches
        .iter()
        .filter(|(_, item)| item.module == module)
        .copied()
        .collect::<Vec<_>>();
    if exact.is_empty() { matches } else { exact }
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
mod tests {
    use super::*;
    use crate::atlas::{sources::Source, syntax};

    #[test]
    fn module_rollup_follows_the_requested_scope() {
        assert_eq!(
            module_for_path(
                Path::new("crates/rimz/src/cli/agents_cmd/show.rs"),
                Path::new("crates/rimz/src/cli")
            ),
            "agents_cmd"
        );
        assert_eq!(
            crate_module_for_row(Path::new("crates/rimz/src/cli"), "agents_cmd"),
            "cli::agents_cmd"
        );
        assert_eq!(
            crate_module_for_path(Path::new("crates/rimz/src/cli/agents_cmd/mod.rs")),
            "cli::agents_cmd"
        );
        assert_eq!(
            rust_module_for_path(
                Path::new("crates/rimz/src/cli/agents_cmd/show.rs"),
                Path::new("crates/rimz/src/cli")
            )
            .as_deref(),
            Some("agents_cmd")
        );
        assert_eq!(
            rust_module_for_path(
                Path::new("crates/rimz/src/cli/snapshots/surface.snap"),
                Path::new("crates/rimz/src/cli")
            ),
            None
        );
        assert_eq!(
            module_for_path(
                Path::new("crates/rimz/src/cli/surface_tests.rs"),
                Path::new("crates/rimz/src/cli")
            ),
            "(root)"
        );
    }

    #[test]
    fn crate_modules_fall_back_to_parent_and_stem_outside_src() {
        assert_eq!(
            crate_module_for_path(Path::new("crates/rimz/examples/seed_perf_workspace.rs")),
            "examples::seed_perf_workspace"
        );
        assert_eq!(
            crate_module_for_path(Path::new("crates/rimz/benches/hotpath.rs")),
            "benches::hotpath"
        );
    }

    #[test]
    fn declaration_only_kinds_are_mods_and_uses() {
        assert!(is_declaration_only("mod"));
        assert!(is_declaration_only("use"));
        assert!(!is_declaration_only("fn"));
    }

    #[test]
    fn escaping_items_preserve_duplicate_cross_revision_identities() {
        let sources = vec![Source::new(
            "src/agents.rs",
            "pub struct A;\npub struct B;\nimpl A { pub fn name() {} }\nimpl B { pub fn name() {} }\n",
        )];
        let syntax = syntax::analyze_sources(&sources, &BTreeSet::new());
        let index = syntax::ModIndex::new(&syntax.files);

        let files = syntax.files.iter().collect::<Vec<_>>();
        let items = escaping_items(&files, Path::new("src"), &index);

        assert_eq!(items["agents"].len(), 4);
        assert_eq!(
            items["agents"]
                .iter()
                .filter(|item| item.id.name == "name")
                .count(),
            2
        );
    }

    #[test]
    fn manifest_crate_names_follow_lib_override_and_rust_normalization() {
        assert_eq!(
            crate_name_from_manifest("[package]\nname = \"rimz-presence-zellij\"\n")
                .unwrap()
                .as_deref(),
            Some("rimz_presence_zellij")
        );
        assert_eq!(
            crate_name_from_manifest(
                "[package]\nname = \"package-name\"\n[lib]\nname = \"import_name\"\n"
            )
            .unwrap()
            .as_deref(),
            Some("import_name")
        );
    }

    #[test]
    fn module_containment_models_crate_external_reach() {
        assert!(module_is_within("store", EXTERNAL_REACH));
        assert!(module_is_within("", EXTERNAL_REACH));
        assert!(module_is_within(EXTERNAL_REACH, EXTERNAL_REACH));
        assert!(!module_is_within(EXTERNAL_REACH, ""));
        assert!(module_is_within("store::event_log", "store"));
        assert!(!module_is_within("storehouse", "store"));
    }

    #[test]
    fn module_endpoints_distinguish_scope_root_from_crate_root() {
        assert_eq!(module_endpoint("", ""), "(root)");
        assert_eq!(module_endpoint("", "agents"), "(crate)");
        assert_eq!(module_endpoint("(crate)", ""), "(root)");
        assert_eq!(module_endpoint("agents", "agents"), "(root)");
        assert_eq!(module_endpoint("agents::adapters", "agents"), "adapters");
        assert_eq!(module_endpoint("(crate)", "agents"), "(crate)");
    }

    #[test]
    fn reference_labels_preserve_exact_non_root_modules() {
        assert_eq!(reference_module_label("", ""), "(root)");
        assert_eq!(reference_module_label("", "agents"), "(crate)");
        assert_eq!(
            reference_module_label("cli::agents_cmd", "agents"),
            "cli::agents_cmd"
        );
    }
}

#[cfg(test)]
mod key_tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::atlas::{sources::Source, syntax};

    fn files(sources: &[(&str, &str)]) -> Vec<FileSyntax> {
        let sources = sources
            .iter()
            .map(|(path, text)| Source::new(*path, *text))
            .collect::<Vec<_>>();
        syntax::analyze_sources(&sources, &BTreeSet::new()).files
    }

    #[test]
    fn items_for_key_prefers_the_named_module_then_searches_beneath() {
        let files = files(&[
            (
                "crates/demo/src/message.rs",
                "pub mod deliver;\npub fn send() {}\n",
            ),
            (
                "crates/demo/src/message/deliver.rs",
                "pub fn queue_synthetic() {}\npub fn send() {}\n",
            ),
        ]);
        let names = |key: &str| {
            items_for_key(&files, key)
                .into_iter()
                .map(|(_, item)| format!("{}::{}", item.module, item.name))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names("message::queue_synthetic"),
            ["message::deliver::queue_synthetic"]
        );
        assert_eq!(names("message::send"), ["message::send"]);
        assert_eq!(names("message::deliver::send"), ["message::deliver::send"]);
        assert_eq!(names("send"), ["message::send", "message::deliver::send"]);
        assert!(names("message::missing").is_empty());
        assert!(names("cli::send").is_empty());
    }

    #[test]
    fn reexports_resolve_to_definitions_through_chains_and_globs() {
        let files = files(&[
            (
                "crates/demo/src/store.rs",
                "mod snapshot;\npub use snapshot::Snapshot;\npub use snapshot::*;\npub use crate::ids::Id;\npub use anyhow::Result;\npub use snapshot::Missing;\n",
            ),
            (
                "crates/demo/src/store/snapshot.rs",
                "mod row;\npub use row::{Row as Snapshot, Extra};\n",
            ),
            (
                "crates/demo/src/store/snapshot/row.rs",
                "pub struct Row;\npub struct Extra;\n",
            ),
            ("crates/demo/src/ids.rs", "pub struct Id;\n"),
        ]);
        let store = files
            .iter()
            .find(|file| file.module_path == "store")
            .expect("store file");
        let item = |name: &str| {
            store
                .pub_items
                .iter()
                .find(|item| item.name == name)
                .expect("store re-exports the name")
        };
        let definition = |name: &str| match resolve_reexport(&files, item(name)) {
            ReExport::Definition(file, definition) => {
                format!("{}::{}", file.module_path, definition.name)
            }
            other => panic!("{name} resolved to {other:?}"),
        };
        assert_eq!(definition("Snapshot"), "store::snapshot::row::Row");
        assert_eq!(definition("Id"), "ids::Id");
        let ReExport::Glob(items) = resolve_reexport(&files, item("*")) else {
            panic!("a glob resolves to a glob");
        };
        assert_eq!(
            items
                .iter()
                .map(|(_, item)| item.name.as_str())
                .collect::<Vec<_>>(),
            ["Row", "Extra"]
        );
        assert!(matches!(
            resolve_reexport(&files, item("Result")),
            ReExport::Foreign
        ));
        assert!(matches!(
            resolve_reexport(&files, item("Missing")),
            ReExport::Unresolved
        ));
    }
}
