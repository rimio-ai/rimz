use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use super::super::facts::Facts;
use super::super::history::{self, BlameCommit};
use super::super::modules::{
    EXTERNAL_REACH, EscapingItem, ReExport, bounded_names, crate_path_for_source,
    escaping_items_for_boundary, is_declaration_only, module_is_within, reference_module_label,
    resolve_reexport,
};
use super::super::references::{Edge, EdgeKind};
use super::super::syntax::{FileSyntax, PubItem};
use super::ModuleSelector;

/// Share of outside production sites the head of the surface table must
/// cover before the summary stops counting items.
const SURFACE_HEAD_SHARE: f64 = 0.8;

#[cfg(test)]
#[path = "surface/tests.rs"]
mod tests;

/// One escaping item measured from outside the boundary: how many
/// production sites and files reach it, and from which modules. A
/// re-export is measured at the definition it names (`path`, `line`) and
/// keyed by the module that exports it.
#[derive(Clone, Debug, Serialize)]
pub(super) struct SurfaceRow {
    pub(super) module: String,
    pub(super) name: String,
    pub(super) kind: String,
    pub(super) reach: String,
    pub(super) narrow_to: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    #[serde(skip)]
    pub(super) end_line: usize,
    /// Lines the definition spans, for a plan's line arithmetic.
    pub(super) sloc: usize,
    pub(super) outside_sites: usize,
    pub(super) outside_files: usize,
    pub(super) callers: Vec<String>,
    pub(super) internal_sites: usize,
    pub(super) test_sites: usize,
    /// The defining module when the escaping declaration is a `pub use`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reexport_of: Option<String>,
    /// The definition as edges identify it.
    #[serde(skip)]
    pub(super) definition: EdgeTarget,
}

/// An escaping item with no production referrers.
#[derive(Clone, Debug, Serialize)]
pub(super) struct VestigialItem {
    pub(super) module: String,
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) sloc: usize,
    pub(super) test_referrers: usize,
    pub(super) introduced: Option<BlameCommit>,
    /// The introducing commit reads as a fix: the item pins a behaviour
    /// someone already paid for.
    pub(super) pins_fix: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reexport_of: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct UnresolvedItem {
    pub(super) module: String,
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
}

/// An item whose tests reach it from outside the visibility it can narrow
/// to: narrowing moves these tests.
#[derive(Clone, Debug, Serialize)]
pub(super) struct PinRow {
    pub(super) module: String,
    pub(super) name: String,
    pub(super) narrow_to: String,
    pub(super) lost_sites: usize,
    pub(super) tests: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct SurfaceSection {
    pub(super) items: Vec<SurfaceRow>,
    pub(super) outside_sites: usize,
    pub(super) head_items: usize,
    pub(super) single_site: usize,
    pub(super) internal_only: usize,
    /// Every escaping declaration as `survey`, `diff`, and `conform` count
    /// `esc`: `mod` lines and each `pub use` included, before this section
    /// folds re-exports onto their definitions.
    pub(super) declarations: usize,
    /// Rows measured through a `pub use` at their definition.
    pub(super) reexports: usize,
    /// Further `pub use` declarations of an item already measured.
    pub(super) aliases: usize,
    /// `pub use` declarations whose definition lies outside the boundary.
    pub(super) foreign_reexports: usize,
    pub(super) vestigial: Vec<VestigialItem>,
    pub(super) unresolved: Vec<UnresolvedItem>,
    pub(super) pins: Vec<PinRow>,
}

/// Identity of a referenced definition as edges carry it.
type EdgeTarget = (PathBuf, String, String, usize);

/// One test reference to a definition in the boundary.
struct TestSite {
    module: String,
    path: PathBuf,
    line: usize,
    function: Option<String>,
}

fn edge_target(edge: &Edge) -> EdgeTarget {
    (
        edge.to_path.clone(),
        edge.to.clone(),
        edge.item.clone(),
        edge.to_line,
    )
}

fn common_ancestor<'a>(modules: impl IntoIterator<Item = &'a str>) -> String {
    let mut modules = modules.into_iter();
    let Some(first) = modules.next() else {
        return String::new();
    };
    let mut common = first.split("::").collect::<Vec<_>>();
    for module in modules {
        let parts = module.split("::").collect::<Vec<_>>();
        common.truncate(
            common
                .iter()
                .zip(parts)
                .take_while(|(left, right)| *left == right)
                .count(),
        );
    }
    common.join("::")
}

fn parent_module(module: &str) -> &str {
    module.rsplit_once("::").map_or("", |(parent, _)| parent)
}

/// The narrowest visibility that still covers every production caller:
/// `private` when only the item's module and its descendants reach it, or
/// nothing does. A caller in a binary-only module sits outside the library
/// crate, so the item must stay as visible as it is.
fn narrow_to(
    item_module: &str,
    effective_reach: &str,
    production_callers: &BTreeSet<String>,
    production_sites: usize,
    bin_modules: &BTreeSet<String>,
) -> String {
    if production_sites == 0 {
        return "private".to_owned();
    }
    let in_binary =
        |caller: &String| bin_modules.contains(caller.split("::").next().unwrap_or(caller));
    if production_callers.iter().any(in_binary) {
        return "keep".to_owned();
    }
    let ancestor = common_ancestor(
        std::iter::once(item_module).chain(production_callers.iter().map(String::as_str)),
    );
    if ancestor == effective_reach {
        "keep".to_owned()
    } else if ancestor == item_module {
        // A private item is visible to its own module and every descendant.
        "private".to_owned()
    } else if ancestor.is_empty() {
        "pub(crate)".to_owned()
    } else if ancestor == parent_module(item_module) {
        "pub(super)".to_owned()
    } else {
        format!("pub(in crate::{ancestor})")
    }
}

fn reach_label(effective_reach: &str) -> String {
    match effective_reach {
        EXTERNAL_REACH => "extern".to_owned(),
        "" => "crate".to_owned(),
        reach => reach.to_owned(),
    }
}

/// Measures the escaping interface from outside: per-item outside sites and
/// files, the head that carries most sites, the one-site tail, items only
/// the module itself reaches, test reach, and the unreferenced remainder.
/// A `pub use` is measured at the definition it re-exports, so a module
/// whose root re-exports private submodules reads at its real surface.
/// Returns the section beside the count of unmeasured declarations
/// (`mod` items and re-exports of things the index does not define).
pub(super) fn surface_section(facts: &Facts, target: &ModuleSelector) -> (SurfaceSection, usize) {
    let files = facts
        .syntax
        .files
        .iter()
        .filter(|file| target.matches(&file.module_path, &file.path))
        .collect::<Vec<_>>();
    let mut escaping = escaping_items_for_boundary(&files, &target.module, &facts.mod_index);
    // Definitions first, so a re-export of an item that escapes on its own
    // reads as an alias rather than claiming the row.
    escaping.sort_by_key(|item| item.id.kind == "use");
    let references = facts
        .references
        .as_ref()
        .expect("inspect loads exact references");

    #[derive(Default)]
    struct Outside {
        sites: usize,
        files: BTreeSet<PathBuf>,
        modules: BTreeSet<String>,
    }
    let mut outside = BTreeMap::<EdgeTarget, Outside>::new();
    let mut test_sites = BTreeMap::<EdgeTarget, Vec<TestSite>>::new();
    for edge in references
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Reference && target.matches(&edge.to, &edge.to_path))
    {
        if edge.test {
            test_sites
                .entry(edge_target(edge))
                .or_default()
                .push(TestSite {
                    module: edge.from.clone(),
                    path: edge.from_path.clone(),
                    line: edge.from_line,
                    function: edge.from_fn.as_ref().map(|function| function.label.clone()),
                });
            continue;
        }
        if target.matches(&edge.from, &edge.from_path) {
            continue;
        }
        let aggregate = outside.entry(edge_target(edge)).or_default();
        aggregate.sites += 1;
        aggregate.files.insert(edge.from_path.clone());
        aggregate
            .modules
            .insert(reference_module_label(&edge.from, &target.module));
    }

    let mut section = SurfaceSection {
        declarations: escaping.len(),
        ..SurfaceSection::default()
    };
    let mut declaration_only = 0;
    let mut measured = BTreeSet::<EdgeTarget>::new();
    for item in escaping {
        let Some((file, declared)) = definition_for_escaping(&files, &item) else {
            continue;
        };
        let unresolved = |item: &EscapingItem| UnresolvedItem {
            module: item.id.module.clone(),
            name: item.id.name.clone(),
            path: item.path.clone(),
            line: item.line,
        };
        // (name at the boundary, defining file, definition)
        let definitions: Vec<(String, &FileSyntax, &PubItem)> = if declared.kind == "use" {
            match resolve_reexport(&facts.syntax.files, declared) {
                ReExport::Definition(def_file, definition) => {
                    vec![(item.id.name.clone(), def_file, definition)]
                }
                ReExport::Glob(items) => items
                    .into_iter()
                    .map(|(def_file, definition)| (definition.name.clone(), def_file, definition))
                    .collect(),
                ReExport::Foreign => {
                    declaration_only += 1;
                    continue;
                }
                ReExport::Unresolved => {
                    section.unresolved.push(unresolved(&item));
                    continue;
                }
            }
        } else if is_declaration_only(&declared.kind) {
            declaration_only += 1;
            continue;
        } else {
            vec![(item.id.name.clone(), file, declared)]
        };
        let effective_reach = facts.mod_index.effective_reach(file, declared);
        for (name, def_file, definition) in definitions {
            let reexport_of = (declared.kind == "use").then(|| definition.module.clone());
            if reexport_of.is_some() && !target.matches(&def_file.module_path, &def_file.path) {
                section.foreign_reexports += 1;
                continue;
            }
            let Some(item_refs) = references.get(def_file, definition) else {
                section.unresolved.push(unresolved(&item));
                continue;
            };
            let key = (
                def_file.path.clone(),
                definition.module.clone(),
                definition.name.clone(),
                definition.line,
            );
            if !measured.insert(key.clone()) {
                section.aliases += 1;
                continue;
            }
            if reexport_of.is_some() {
                section.reexports += 1;
            }
            let reached = outside.remove(&key).unwrap_or_default();
            section.items.push(SurfaceRow {
                module: item.id.module.clone(),
                name,
                kind: definition.kind.clone(),
                reach: reach_label(&effective_reach),
                narrow_to: narrow_to(
                    &item.id.module,
                    &effective_reach,
                    &item_refs.production,
                    item_refs.production_count,
                    &facts.bin_modules,
                ),
                path: def_file.path.clone(),
                line: definition.line,
                end_line: definition.end_line,
                sloc: definition.end_line.max(definition.line) - definition.line + 1,
                outside_sites: reached.sites,
                outside_files: reached.files.len(),
                callers: reached.modules.into_iter().collect(),
                internal_sites: item_refs.production_count - reached.sites,
                test_sites: test_sites.get(&key).map_or(0, Vec::len),
                reexport_of,
                definition: key,
            });
        }
    }
    section.items.sort_by(|left, right| {
        right
            .outside_sites
            .cmp(&left.outside_sites)
            .then_with(|| right.internal_sites.cmp(&left.internal_sites))
            .then_with(|| left.module.cmp(&right.module))
            .then_with(|| left.name.cmp(&right.name))
    });
    section.outside_sites = section.items.iter().map(|row| row.outside_sites).sum();
    let head_target = (section.outside_sites as f64 * SURFACE_HEAD_SHARE).ceil() as usize;
    let mut covered = 0;
    section.head_items = section
        .items
        .iter()
        .take_while(|row| {
            let below = covered < head_target;
            covered += row.outside_sites;
            below
        })
        .count();
    section.single_site = section
        .items
        .iter()
        .filter(|row| row.outside_sites == 1)
        .count();
    section.internal_only = section
        .items
        .iter()
        .filter(|row| row.outside_sites == 0 && row.internal_sites > 0)
        .count();
    section.unresolved.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
    });
    section.pins = pins(&section.items, &test_sites);
    (section, declaration_only)
}

/// Items whose tests sit outside the visibility they can narrow to. A test
/// in another crate (`tests/`) loses every narrowing; a test in another
/// module loses `private`, `pub(super)`, and `pub(in …)`. These are the
/// tests a narrowing pass rewrites or deletes.
fn pins(rows: &[SurfaceRow], test_sites: &BTreeMap<EdgeTarget, Vec<TestSite>>) -> Vec<PinRow> {
    let mut pins = Vec::new();
    for row in rows.iter().filter(|row| row.narrow_to != "keep") {
        let Some(sites) = test_sites.get(&row.definition) else {
            continue;
        };
        let crate_src = crate_path_for_source(&row.path).join("src");
        let reach = match row.narrow_to.as_str() {
            "private" => Some(row.module.clone()),
            "pub(super)" => Some(parent_module(&row.module).to_owned()),
            "pub(crate)" => None,
            other => other
                .strip_prefix("pub(in crate::")
                .and_then(|rest| rest.strip_suffix(')'))
                .map(str::to_owned),
        };
        let lost = sites
            .iter()
            .filter(|site| {
                !site.path.starts_with(&crate_src)
                    || reach
                        .as_deref()
                        .is_some_and(|reach| !module_is_within(&site.module, reach))
            })
            .map(|site| {
                site.function.as_deref().map_or_else(
                    || format!("{}:{}", site.path.display(), site.line),
                    |function| format!("{function} ({}:{})", site.path.display(), site.line),
                )
            })
            .collect::<BTreeSet<_>>();
        if lost.is_empty() {
            continue;
        }
        pins.push(PinRow {
            module: row.module.clone(),
            name: row.name.clone(),
            narrow_to: row.narrow_to.clone(),
            lost_sites: lost.len(),
            tests: lost.into_iter().collect(),
        });
    }
    pins.sort_by(|left, right| {
        right
            .lost_sites
            .cmp(&left.lost_sites)
            .then_with(|| left.module.cmp(&right.module))
            .then_with(|| left.name.cmp(&right.name))
    });
    pins
}

/// Escaping items with no production sites. A definition whose lines all
/// blame to one commit carries that commit as evidence. One `git blame` per
/// file that holds a candidate.
pub(super) fn vestigial_items(root: &Path, rows: &[SurfaceRow]) -> Result<Vec<VestigialItem>> {
    let mut blames = BTreeMap::<PathBuf, Vec<BlameCommit>>::new();
    let mut vestigial = Vec::new();
    for row in rows
        .iter()
        .filter(|row| row.outside_sites + row.internal_sites == 0)
    {
        if !blames.contains_key(&row.path) {
            blames.insert(row.path.clone(), history::blame_lines(root, &row.path)?);
        }
        let blame = &blames[&row.path];
        let introduced = blame.get(row.line.saturating_sub(1)).and_then(|first| {
            let end = row.end_line.max(row.line).min(blame.len());
            blame
                .get(row.line.saturating_sub(1)..end)
                .filter(|commits| commits.iter().all(|commit| commit == first))
                .map(|_| first.clone())
        });
        let pins_fix = introduced
            .as_ref()
            .is_some_and(|commit| !history::fix_markers(&commit.summary).is_empty());
        vestigial.push(VestigialItem {
            module: row.module.clone(),
            name: row.name.clone(),
            path: row.path.clone(),
            line: row.line,
            sloc: row.sloc,
            test_referrers: row.test_sites,
            introduced,
            pins_fix,
            reexport_of: row.reexport_of.clone(),
        });
    }
    vestigial.sort_by(|left, right| {
        left.introduced
            .as_ref()
            .map(|commit| commit.time)
            .cmp(&right.introduced.as_ref().map(|commit| commit.time))
            .then_with(|| left.module.cmp(&right.module))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(vestigial)
}

/// Guard families anywhere in the crate that name an item the target module
/// defines: the decisions callers make about this module's state that the
/// module could make once.
fn definition_for_escaping<'a>(
    files: &'a [&FileSyntax],
    escaping: &EscapingItem,
) -> Option<(&'a FileSyntax, &'a PubItem)> {
    let file = files
        .iter()
        .copied()
        .find(|file| file.path == escaping.path)?;
    let item = file.pub_items.iter().find(|item| {
        item.line == escaping.line
            && item.module == escaping.id.module
            && item.name == escaping.id.name
    })?;
    Some((file, item))
}

pub(super) fn render_surface(out: &mut String, surface: &SurfaceSection, top: usize) {
    out.push_str("\n# Escaping surface\n\n");
    out.push_str(
        "| item | kind | sloc | reach | narrow to | outside sites | files | internal | tests | callers |\n",
    );
    out.push_str("|---|---|---:|---|---|---:|---:|---:|---:|---|\n");
    for row in surface.items.iter().take(top) {
        writeln!(
            out,
            "| `{}::{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            row.module,
            row.name,
            row.kind,
            row.sloc,
            row.reach,
            row.narrow_to,
            row.outside_sites,
            row.outside_files,
            row.internal_sites,
            row.test_sites,
            bounded_names(&row.callers, 4)
        )
        .expect("writing to a String cannot fail");
    }
    if surface.items.len() > top {
        writeln!(out, "\n_{} more items omitted._", surface.items.len() - top)
            .expect("writing to a String cannot fail");
    }
    writeln!(
        out,
        "\n{} escaping items, {} outside production sites; {} items carry {}% of sites; {} items have one outside site; {} items only the module itself reaches; {} measured through `pub use` ({} further aliases, {} re-exports defined outside the boundary)",
        surface.items.len(),
        surface.outside_sites,
        surface.head_items,
        (SURFACE_HEAD_SHARE * 100.0) as usize,
        surface.single_site,
        surface.internal_only,
        surface.reexports,
        surface.aliases,
        surface.foreign_reexports
    )
    .expect("writing to a String cannot fail");

    out.push_str("\n## Vestigial candidates\n\n");
    if surface.vestigial.is_empty() {
        out.push_str("none\n");
    }
    for item in surface.vestigial.iter().take(top) {
        let introduced = item.introduced.as_ref().map_or_else(
            || "definition spans multiple commits".to_owned(),
            |commit| {
                format!(
                    "untouched since `{}` {} {}",
                    commit.short,
                    history::civil_date(commit.time),
                    commit.summary
                )
            },
        );
        let reexport = item
            .reexport_of
            .as_ref()
            .map_or_else(String::new, |module| {
                format!(" (re-exported from {module})")
            });
        writeln!(
            out,
            "- `{}::{}` — {}:{} ({} sloc){reexport}; test referrers: {}; {}{}",
            item.module,
            item.name,
            item.path.display(),
            item.line,
            item.sloc,
            item.test_referrers,
            introduced,
            if item.pins_fix { " (pins a fix)" } else { "" }
        )
        .expect("writing to a String cannot fail");
    }
    if surface.vestigial.len() > top {
        writeln!(
            out,
            "\n_{} more candidates omitted._",
            surface.vestigial.len() - top
        )
        .expect("writing to a String cannot fail");
    }

    render_unresolved(out, &surface.unresolved, top);
}

pub(super) fn render_pins(out: &mut String, pins: &[PinRow], top: usize) {
    out.push_str("\n# Tests past the narrowed reach\n\n");
    if pins.is_empty() {
        out.push_str("none: every test reaches its item from inside the narrowed reach\n");
        return;
    }
    out.push_str("| item | narrow to | test sites lost | tests |\n");
    out.push_str("|---|---|---:|---|\n");
    for pin in pins.iter().take(top) {
        writeln!(
            out,
            "| `{}::{}` | {} | {} | {} |",
            pin.module,
            pin.name,
            pin.narrow_to,
            pin.lost_sites,
            bounded_names(&pin.tests, 3)
        )
        .expect("writing to a String cannot fail");
    }
    if pins.len() > top {
        writeln!(out, "\n_{} more items omitted._", pins.len() - top)
            .expect("writing to a String cannot fail");
    }
    writeln!(
        out,
        "\n{} items keep {} test sites past their narrowed reach; those tests move with the narrowing",
        pins.len(),
        pins.iter().map(|pin| pin.lost_sites).sum::<usize>()
    )
    .expect("writing to a String cannot fail");
}

fn render_unresolved(out: &mut String, unresolved: &[UnresolvedItem], top: usize) {
    out.push_str("\n## Unresolved definitions\n\n");
    for item in unresolved.iter().take(top) {
        writeln!(
            out,
            "- `{}`::`{}` — {}:{}",
            item.module,
            item.name,
            item.path.display(),
            item.line
        )
        .expect("writing to a String cannot fail");
    }
}
