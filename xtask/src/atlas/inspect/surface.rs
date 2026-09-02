use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;

use super::super::facts::Facts;
use super::super::history::{self, BlameCommit};
use super::super::modules::{
    EXTERNAL_REACH, EscapingItem, bounded_names, escaping_items_for_boundary, is_declaration_only,
    reference_module_label,
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

#[derive(Clone, Debug, Serialize)]
pub(super) struct SurfaceItem {
    pub(super) module: String,
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) test_referrers: usize,
    pub(super) pass_through: bool,
}

/// One escaping item measured from outside the boundary: how many
/// production sites and files reach it, and from which modules.
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
    pub(super) outside_sites: usize,
    pub(super) outside_files: usize,
    pub(super) callers: Vec<String>,
    pub(super) internal_sites: usize,
    pub(super) test_sites: usize,
}

/// Where test code reaches the module: through its escaping interface or
/// past it, at boundary-visible items the interface does not expose.
#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct TestReach {
    pub(super) sites: usize,
    pub(super) through_interface: usize,
    pub(super) past_interface: usize,
    pub(super) past_items: Vec<PastItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct PastItem {
    pub(super) module: String,
    pub(super) name: String,
    pub(super) sites: usize,
}

/// An escaping item at most one production site reaches whose definition
/// no commit has touched since the one that introduced it.
#[derive(Clone, Debug, Serialize)]
pub(super) struct VestigialItem {
    pub(super) module: String,
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) production_sites: usize,
    pub(super) introduced: BlameCommit,
    /// The introducing commit reads as a fix: the item pins a behaviour
    /// someone already paid for.
    pub(super) pins_fix: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct UnresolvedItem {
    pub(super) module: String,
    pub(super) name: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
}
#[derive(Debug, Default, Serialize)]
pub(super) struct SurfaceSection {
    pub(super) items: Vec<SurfaceRow>,
    pub(super) outside_sites: usize,
    pub(super) head_items: usize,
    pub(super) single_site: usize,
    pub(super) internal_only: usize,
    pub(super) test_reach: TestReach,
    pub(super) vestigial: Vec<VestigialItem>,
    pub(super) zero_production: Vec<SurfaceItem>,
    pub(super) unresolved: Vec<UnresolvedItem>,
}

/// Identity of a referenced definition as edges carry it.
type EdgeTarget = (PathBuf, String, String, usize);

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
/// Returns the section beside the count of unmeasured declarations.
pub(super) fn surface_section(facts: &Facts, target: &ModuleSelector) -> (SurfaceSection, usize) {
    let files = facts
        .syntax
        .files
        .iter()
        .filter(|file| target.matches(&file.module_path, &file.path))
        .collect::<Vec<_>>();
    let escaping = escaping_items_for_boundary(&files, &target.module, &facts.mod_index);
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
    let mut test_sites = BTreeMap::<EdgeTarget, usize>::new();
    for edge in references
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Reference && target.matches(&edge.to, &edge.to_path))
    {
        if edge.test {
            *test_sites.entry(edge_target(edge)).or_default() += 1;
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

    let mut section = SurfaceSection::default();
    let mut declaration_only = 0;
    let mut escaping_targets = BTreeSet::new();
    for item in escaping {
        let Some((file, definition)) = definition_for_escaping(&files, &item) else {
            continue;
        };
        let Some(item_refs) = references.get(file, definition) else {
            if is_declaration_only(&item.id.kind) {
                declaration_only += 1;
                continue;
            }
            section.unresolved.push(UnresolvedItem {
                module: item.id.module,
                name: item.id.name,
                path: item.path,
                line: item.line,
            });
            continue;
        };
        let key = (
            item.path.clone(),
            item.id.module.clone(),
            item.id.name.clone(),
            item.line,
        );
        escaping_targets.insert(key.clone());
        if item_refs.production_count == 0 {
            section.zero_production.push(SurfaceItem {
                module: item.id.module.clone(),
                name: item.id.name.clone(),
                path: item.path.clone(),
                line: item.line,
                test_referrers: item_refs.test_count,
                pass_through: file
                    .fns
                    .iter()
                    .any(|function| function.name == item.id.name && function.forwards.is_some()),
            });
        }
        let reached = outside.remove(&key).unwrap_or_default();
        let effective_reach = facts.mod_index.effective_reach(file, definition);
        section.items.push(SurfaceRow {
            module: item.id.module,
            name: item.id.name,
            kind: item.id.kind,
            reach: reach_label(&effective_reach),
            narrow_to: narrow_to(
                &definition.module,
                &effective_reach,
                &item_refs.production,
                item_refs.production_count,
                &facts.bin_modules,
            ),
            path: item.path,
            line: item.line,
            end_line: definition.end_line,
            outside_sites: reached.sites,
            outside_files: reached.files.len(),
            callers: reached.modules.into_iter().collect(),
            internal_sites: item_refs.production_count - reached.sites,
            test_sites: test_sites.get(&key).copied().unwrap_or_default(),
        });
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
    section.test_reach = test_reach(&test_sites, &escaping_targets);
    section.zero_production.sort_by(|left, right| {
        right
            .test_referrers
            .cmp(&left.test_referrers)
            .then_with(|| left.module.cmp(&right.module))
            .then_with(|| left.name.cmp(&right.name))
    });
    section.unresolved.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
    });
    (section, declaration_only)
}

fn test_reach(
    test_sites: &BTreeMap<EdgeTarget, usize>,
    escaping: &BTreeSet<EdgeTarget>,
) -> TestReach {
    let mut reach = TestReach::default();
    let mut past = BTreeMap::<(String, String), usize>::new();
    for (key, sites) in test_sites {
        reach.sites += sites;
        if escaping.contains(key) {
            reach.through_interface += sites;
        } else {
            reach.past_interface += sites;
            *past.entry((key.1.clone(), key.2.clone())).or_default() += sites;
        }
    }
    reach.past_items = past
        .into_iter()
        .map(|((module, name), sites)| PastItem {
            module,
            name,
            sites,
        })
        .collect();
    reach.past_items.sort_by(|left, right| {
        right
            .sites
            .cmp(&left.sites)
            .then_with(|| left.module.cmp(&right.module))
            .then_with(|| left.name.cmp(&right.name))
    });
    reach
}

/// Escaping items with at most one production site whose definition lines
/// all blame to one commit: introduced once and never revisited. One
/// `git blame` per file that holds a candidate.
pub(super) fn vestigial_items(root: &Path, rows: &[SurfaceRow]) -> Result<Vec<VestigialItem>> {
    let mut blames = BTreeMap::<PathBuf, Vec<BlameCommit>>::new();
    let mut vestigial = Vec::new();
    for row in rows
        .iter()
        .filter(|row| row.outside_sites + row.internal_sites <= 1)
    {
        if !blames.contains_key(&row.path) {
            blames.insert(row.path.clone(), history::blame_lines(root, &row.path)?);
        }
        let blame = &blames[&row.path];
        let Some(first) = blame.get(row.line.saturating_sub(1)) else {
            continue;
        };
        let end = row.end_line.max(row.line).min(blame.len());
        if blame[row.line - 1..end]
            .iter()
            .all(|commit| commit == first)
        {
            vestigial.push(VestigialItem {
                module: row.module.clone(),
                name: row.name.clone(),
                path: row.path.clone(),
                line: row.line,
                production_sites: row.outside_sites + row.internal_sites,
                pins_fix: !history::fix_markers(&first.summary).is_empty(),
                introduced: first.clone(),
            });
        }
    }
    vestigial.sort_by(|left, right| {
        left.introduced
            .time
            .cmp(&right.introduced.time)
            .then_with(|| left.production_sites.cmp(&right.production_sites))
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
        "| item | kind | reach | narrow to | outside sites | files | internal | tests | callers |\n",
    );
    out.push_str("|---|---|---|---|---:|---:|---:|---:|---|\n");
    for row in surface.items.iter().take(top) {
        writeln!(
            out,
            "| `{}::{}` | {} | {} | {} | {} | {} | {} | {} | {} |",
            row.module,
            row.name,
            row.kind,
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
        "\n{} escaping items, {} outside production sites; {} items carry {}% of sites; {} items have one outside site; {} items only the module itself reaches",
        surface.items.len(),
        surface.outside_sites,
        surface.head_items,
        (SURFACE_HEAD_SHARE * 100.0) as usize,
        surface.single_site,
        surface.internal_only
    )
    .expect("writing to a String cannot fail");

    let reach = &surface.test_reach;
    writeln!(
        out,
        "\n## Test reach\n\n{} test sites: {} through the escaping interface, {} past it",
        reach.sites, reach.through_interface, reach.past_interface
    )
    .expect("writing to a String cannot fail");
    for item in reach.past_items.iter().take(top) {
        writeln!(
            out,
            "- `{}::{}` — {} test sites",
            item.module, item.name, item.sites
        )
        .expect("writing to a String cannot fail");
    }

    out.push_str("\n## Vestigial candidates\n\n");
    for item in surface.vestigial.iter().take(top) {
        writeln!(
            out,
            "- `{}::{}` — {}:{}; production sites: {}; untouched since `{}` {}{}",
            item.module,
            item.name,
            item.path.display(),
            item.line,
            item.production_sites,
            item.introduced.short,
            item.introduced.summary,
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

    render_zero_surface(out, &surface.zero_production, &surface.unresolved, top);
}

fn render_zero_surface(
    out: &mut String,
    zero: &[SurfaceItem],
    unresolved: &[UnresolvedItem],
    top: usize,
) {
    out.push_str("\n## Zero-production surface\n\n");
    for item in zero.iter().take(top) {
        let pass_through = if item.pass_through {
            " (pass-through)"
        } else {
            ""
        };
        writeln!(
            out,
            "- `{}`::`{}` — {}:{}; test referrers: {}{}",
            item.module,
            item.name,
            item.path.display(),
            item.line,
            item.test_referrers,
            pass_through
        )
        .expect("writing to a String cannot fail");
    }
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
