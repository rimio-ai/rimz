use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::super::references::{Edge, EdgeKind, FunctionId};
use super::super::sources::Source;
use super::super::syntax::FileSyntax;
use super::ModuleSelector;

#[cfg(test)]
#[path = "calls/tests.rs"]
mod tests;

#[derive(Clone, Debug, Serialize)]
pub(super) struct Caller {
    pub(super) module: String,
    pub(super) items: Vec<String>,
    pub(super) max_fn_items: usize,
    pub(super) top_fns: Vec<CallerFn>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct CallerFn {
    pub(super) function: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) items: usize,
    pub(super) items_unfolded: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct FunctionRow {
    pub(super) function: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) end_line: usize,
    pub(super) items: Vec<String>,
    pub(super) items_unfolded: usize,
    pub(super) sites: usize,
    pub(super) site_lines: Vec<usize>,
    pub(super) also: Vec<(String, usize)>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct Heaviest {
    pub(super) function: String,
    pub(super) path: PathBuf,
    pub(super) line: usize,
    pub(super) end_line: usize,
    pub(super) site_lines: Vec<usize>,
    pub(super) source: String,
}

#[derive(Debug, Serialize)]
pub(super) struct HeaviestSection {
    pub(super) functions: Vec<FunctionRow>,
    pub(super) quote: Option<Heaviest>,
}

const CALL_SHAPE_SHARED_ITEMS: usize = 3;
const CALL_SHAPE_SOLO_ITEMS: usize = 5;

/// One ordered sequence of target-item references shared by caller
/// functions: the call-site test written by the callers themselves.
#[derive(Clone, Debug, Serialize)]
pub(super) struct CallShape {
    pub(super) shape: Vec<String>,
    pub(super) items: usize,
    pub(super) items_unfolded: usize,
    pub(super) functions: Vec<AssemblyFunction>,
}
#[derive(Clone, Debug, Serialize)]
pub(super) struct AssemblyGroup {
    pub(super) items: Vec<String>,
    pub(super) functions: Vec<AssemblyFunction>,
    pub(super) children: Vec<AssemblyDepth>,
    pub(super) score: usize,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct AssemblyDepth {
    pub(super) extra_items: Vec<String>,
    pub(super) functions: Vec<AssemblyFunction>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct AssemblyFunction {
    pub(super) module: String,
    #[serde(serialize_with = "serialize_function_id")]
    pub(super) function: FunctionId,
    pub(super) items: usize,
    pub(super) items_unfolded: usize,
}

#[derive(Clone, Debug)]
struct ItemReference {
    line: usize,
    item: String,
    owner: Option<String>,
}

fn serialize_function_id<S>(function: &FunctionId, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    #[derive(Serialize)]
    struct SerializableFunction<'a> {
        path: &'a Path,
        label: &'a str,
        line: usize,
    }
    SerializableFunction {
        path: &function.path,
        label: &function.label,
        line: function.line,
    }
    .serialize(serializer)
}

fn owner_for_edge(edge: &Edge, syntax_files: &[FileSyntax]) -> Option<String> {
    let file = syntax_files.iter().find(|file| file.path == edge.to_path)?;
    let item = file
        .pub_items
        .iter()
        .find(|item| item.line == edge.to_line && item.name == edge.item)?;
    if matches!(
        item.kind.as_str(),
        "struct" | "enum" | "trait" | "type" | "union"
    ) {
        return Some(item.name.clone());
    }
    file.fns
        .iter()
        .find(|function| {
            function.name == edge.item
                && function.line <= edge.to_line
                && edge.to_line <= function.end_line
        })
        .and_then(|function| function.owner.clone())
}

fn is_type_alias_edge(edge: &Edge, syntax_files: &[FileSyntax]) -> bool {
    syntax_files
        .iter()
        .find(|file| file.path == edge.to_path)
        .and_then(|file| {
            file.pub_items
                .iter()
                .find(|item| item.line == edge.to_line && item.name == edge.item)
        })
        .is_some_and(|item| item.kind == "type")
}

fn folded_label(owner: &str, names: &[String]) -> String {
    let mut names = names.iter().filter(|name| name.as_str() != owner).fold(
        Vec::<String>::new(),
        |mut names, name| {
            if names.last() != Some(name) {
                names.push(name.clone());
            }
            names
        },
    );
    if names.is_empty() {
        return owner.to_owned();
    }
    if let [name] = names.as_slice() {
        return format!("{owner}::{name}");
    }
    let omitted = names.len().saturating_sub(3);
    names.truncate(3);
    if omitted > 0 {
        names.push(format!("+{omitted}"));
    }
    format!("{owner}::{{{}}}", names.join(", "))
}

fn folded_sequence(edges: &[&Edge], syntax_files: &[FileSyntax]) -> Vec<String> {
    let mut references = edges
        .iter()
        .map(|edge| ItemReference {
            line: edge.from_line,
            item: edge.item.clone(),
            owner: owner_for_edge(edge, syntax_files),
        })
        .collect::<Vec<_>>();
    references.sort_by(|left, right| {
        left.line
            .cmp(&right.line)
            .then_with(|| left.item.cmp(&right.item))
    });

    // Every reference to one owner type (the type, its variants, and its
    // associated functions) is one item, shown at its first reference.
    let mut owner_runs = BTreeMap::<String, Vec<(usize, String)>>::new();
    for (index, reference) in references.iter().enumerate() {
        let Some(owner) = &reference.owner else {
            continue;
        };
        owner_runs
            .entry(owner.clone())
            .or_default()
            .push((index, reference.item.clone()));
    }
    let mut folded_runs = BTreeMap::<usize, String>::new();
    let mut folded_sites = BTreeSet::new();
    for (owner, sites) in owner_runs {
        let names = sites
            .iter()
            .map(|(_, name)| name.clone())
            .collect::<Vec<_>>();
        if names.iter().collect::<BTreeSet<_>>().len() < 2 {
            continue;
        }
        let Some((first, _)) = sites.first() else {
            continue;
        };
        folded_runs.insert(*first, folded_label(&owner, &names));
        folded_sites.extend(sites.into_iter().map(|(index, _)| index));
    }
    let mut folded = Vec::new();
    for (index, reference) in references.into_iter().enumerate() {
        if let Some(label) = folded_runs.get(&index) {
            folded.push(label.clone());
            continue;
        }
        if folded_sites.contains(&index) {
            continue;
        }
        if folded.last() != Some(&reference.item) {
            folded.push(reference.item);
        }
    }
    folded
}

fn distinct_folded_items(edges: &[&Edge], syntax_files: &[FileSyntax]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    folded_sequence(edges, syntax_files)
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

/// Distinct target items one function references, every reference to one
/// owner type folded into one item: the `max/fn` the vocabulary defines,
/// shared by the dossier and `diff` so a contract ceiling read from one is
/// judged by the other.
pub(in crate::atlas) fn folded_item_count(edges: &[&Edge], syntax_files: &[FileSyntax]) -> usize {
    distinct_folded_items(edges, syntax_files).len()
}

fn unfolded_items(edges: &[&Edge]) -> usize {
    edges
        .iter()
        .map(|edge| edge.item.as_str())
        .collect::<BTreeSet<_>>()
        .len()
}

pub(super) fn callers_from_edges(
    edges: &[Edge],
    target: &ModuleSelector,
    syntax_files: &[FileSyntax],
) -> Vec<Caller> {
    #[derive(Default)]
    struct Assembly {
        edges: Vec<Edge>,
    }
    let mut rows = BTreeMap::<String, Assembly>::new();
    for edge in edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && !edge.test
            && target.matches(&edge.to, &edge.to_path)
            && !target.matches(&edge.from, &edge.from_path)
    }) {
        rows.entry(edge.from.clone())
            .or_default()
            .edges
            .push(edge.clone());
    }
    let mut rows = rows
        .into_iter()
        .map(|(module, assembly)| {
            let mut by_function = BTreeMap::<FunctionId, Vec<&Edge>>::new();
            for edge in &assembly.edges {
                if let Some(function) = &edge.from_fn {
                    by_function
                        .entry(FunctionId::new(&edge.from_path, function))
                        .or_default()
                        .push(edge);
                }
            }
            let mut items = BTreeSet::new();
            for function_edges in by_function.values() {
                items.extend(distinct_folded_items(function_edges, syntax_files));
            }
            items.extend(
                assembly
                    .edges
                    .iter()
                    .filter(|edge| edge.from_fn.is_none())
                    .map(|edge| edge.item.clone()),
            );
            let mut top_fns = assembly
                .edges
                .into_iter()
                .filter_map(|edge| {
                    let function = edge.from_fn.clone()?;
                    Some((FunctionId::new(&edge.from_path, &function), edge))
                })
                .fold(
                    BTreeMap::<FunctionId, Vec<Edge>>::new(),
                    |mut functions, (function, edge)| {
                        functions.entry(function).or_default().push(edge);
                        functions
                    },
                )
                .into_iter()
                .map(|(function, edges)| {
                    let edge_refs = edges.iter().collect::<Vec<_>>();
                    CallerFn {
                        function: function.label,
                        path: function.path,
                        line: function.line,
                        items: distinct_folded_items(&edge_refs, syntax_files).len(),
                        items_unfolded: unfolded_items(&edge_refs),
                    }
                })
                .collect::<Vec<_>>();
            top_fns.sort_by(|left, right| {
                right
                    .items
                    .cmp(&left.items)
                    .then_with(|| left.function.cmp(&right.function))
                    .then_with(|| left.path.cmp(&right.path))
                    .then_with(|| left.line.cmp(&right.line))
            });
            let max_fn_items = top_fns.first().map_or(0, |function| function.items);
            top_fns.truncate(3);
            Caller {
                module,
                items: items.into_iter().collect(),
                max_fn_items,
                top_fns,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .max_fn_items
            .cmp(&left.max_fn_items)
            .then_with(|| right.items.len().cmp(&left.items.len()))
            .then_with(|| left.module.cmp(&right.module))
    });
    rows
}

pub(super) fn assembly_functions(
    edges: &[Edge],
    syntax_files: &[FileSyntax],
    from: &ModuleSelector,
    to: &ModuleSelector,
) -> Vec<FunctionRow> {
    let target_module = to.module.split("::").next().unwrap_or(&to.module);
    let mut also_by_function =
        BTreeMap::<FunctionId, BTreeMap<String, BTreeSet<(String, usize)>>>::new();
    for edge in edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Reference && !edge.test)
    {
        let Some(function) = &edge.from_fn else {
            continue;
        };
        let provider = edge.to.split("::").next().unwrap_or(&edge.to);
        let caller = edge.from.split("::").next().unwrap_or(&edge.from);
        if provider == caller || provider == target_module {
            continue;
        }
        also_by_function
            .entry(FunctionId::new(&edge.from_path, function))
            .or_default()
            .entry(provider.to_owned())
            .or_default()
            .insert((edge.item.clone(), edge.to_line));
    }
    let mut by_function = BTreeMap::<FunctionId, (Vec<&Edge>, usize, BTreeSet<usize>)>::new();
    for edge in edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && !edge.test
            && from.matches(&edge.from, &edge.from_path)
            && to.matches(&edge.to, &edge.to_path)
            && !is_type_alias_edge(edge, syntax_files)
    }) {
        let Some(function) = &edge.from_fn else {
            continue;
        };
        let aggregate = by_function
            .entry(FunctionId::new(&edge.from_path, function))
            .or_default();
        aggregate.0.push(edge);
        aggregate.1 += 1;
        aggregate.2.insert(edge.from_line);
    }
    let mut rows = by_function
        .into_iter()
        .map(|(function, (edges, sites, site_lines))| {
            let items_unfolded = unfolded_items(&edges);
            let mut also = also_by_function
                .remove(&function)
                .unwrap_or_default()
                .into_iter()
                .map(|(module, items)| (module, items.len()))
                .collect::<Vec<_>>();
            also.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            FunctionRow {
                end_line: function_end_line(syntax_files, &function),
                function: function.label,
                path: function.path,
                line: function.line,
                items: distinct_folded_items(&edges, syntax_files),
                items_unfolded,
                sites,
                site_lines: site_lines.into_iter().collect(),
                also,
            }
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .items
            .len()
            .cmp(&left.items.len())
            .then_with(|| left.function.cmp(&right.function))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    rows
}

fn function_end_line(files: &[FileSyntax], key: &FunctionId) -> usize {
    files
        .iter()
        .find(|file| file.path == key.path)
        .and_then(|file| {
            file.fns
                .iter()
                .find(|function| function.line == key.line && function.label() == key.label)
        })
        .map_or(key.line, |function| function.end_line)
}

/// Lines a quote may hold before later sites are dropped.
const QUOTE_LINES: usize = 80;
/// A gap between two site windows shorter than this stays quoted, since
/// the wiring the call-site test judges sits between the references.
const QUOTE_GAP: usize = 10;

/// Quotes the heaviest caller: the whole function when it fits in
/// `QUOTE_LINES`, else its signature and every reference site with a line
/// of context, gaps under `QUOTE_GAP` lines kept, longer ones elided.
pub(super) fn quote_function(function: &FunctionRow, sources: &[Source]) -> Option<Heaviest> {
    let source = sources.iter().find(|source| source.path == function.path)?;
    let source_lines = source.text.lines().collect::<Vec<_>>();
    let start = function.line.max(1);
    let end = function.end_line.min(source_lines.len()).max(start);
    let signature_end = if end - start < QUOTE_LINES {
        end
    } else {
        (start..=end)
            .find(|line| source_lines[*line - 1].contains('{'))
            .unwrap_or_else(|| (start + 2).min(end))
    };

    #[derive(Debug)]
    struct Window {
        start: usize,
        end: usize,
        sites: BTreeSet<usize>,
    }

    let mut windows = vec![Window {
        start,
        end: signature_end,
        sites: BTreeSet::new(),
    }];
    for site in function
        .site_lines
        .iter()
        .copied()
        .filter(|site| (start..=end).contains(site))
    {
        let window = Window {
            start: site.saturating_sub(1).max(start),
            end: (site + 1).min(end),
            sites: BTreeSet::from([site]),
        };
        if let Some(previous) = windows.last_mut()
            && window.start <= previous.end + QUOTE_GAP
        {
            previous.end = previous.end.max(window.end);
            previous.sites.extend(window.sites);
        } else {
            windows.push(window);
        }
    }

    let mut rendered = Vec::new();
    let mut source_line_count = 0;
    let mut previous_end = None;
    let mut omitted_sites = 0;
    for (index, window) in windows.iter().enumerate() {
        let available = QUOTE_LINES.saturating_sub(source_line_count);
        let window_lines = window.end - window.start + 1;
        if available == 0 {
            omitted_sites += windows[index..]
                .iter()
                .map(|window| window.sites.len())
                .sum::<usize>();
            break;
        }
        let included_end = window.end.min(window.start + available - 1);
        if let Some(previous_end) = previous_end
            && window.start > previous_end + 1
        {
            rendered.push(format!("… {} lines", window.start - previous_end - 1));
        }
        rendered.extend(
            source_lines[window.start - 1..included_end]
                .iter()
                .map(|line| (*line).to_owned()),
        );
        source_line_count += included_end - window.start + 1;
        previous_end = Some(included_end);
        if included_end < window.end {
            omitted_sites += window
                .sites
                .iter()
                .filter(|site| **site > included_end)
                .count();
            omitted_sites += windows[index + 1..]
                .iter()
                .map(|window| window.sites.len())
                .sum::<usize>();
            break;
        }
        debug_assert!(source_line_count <= QUOTE_LINES);
        debug_assert_eq!(window_lines, included_end - window.start + 1);
    }
    if omitted_sites > 0 {
        rendered.push(format!("… {omitted_sites} more sites"));
    }
    Some(Heaviest {
        function: function.function.clone(),
        path: function.path.clone(),
        line: function.line,
        end_line: function.end_line,
        site_lines: function.site_lines.clone(),
        source: rendered.join("\n"),
    })
}

/// Orders each outside production function's target references by source
/// line, drops consecutive repeats, and groups functions that share the
/// same sequence.
pub(super) fn call_shapes(
    edges: &[Edge],
    target: &ModuleSelector,
    syntax_files: &[FileSyntax],
) -> Vec<CallShape> {
    let mut functions = BTreeMap::<FunctionId, (String, Vec<&Edge>)>::new();
    for edge in edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && !edge.test
            && target.matches(&edge.to, &edge.to_path)
            && !target.matches(&edge.from, &edge.from_path)
            && !is_type_alias_edge(edge, syntax_files)
    }) {
        let Some(function) = &edge.from_fn else {
            continue;
        };
        functions
            .entry(FunctionId::new(&edge.from_path, function))
            .or_insert_with(|| (edge.from.clone(), Vec::new()))
            .1
            .push(edge);
    }
    let mut shapes = BTreeMap::<Vec<String>, (usize, Vec<AssemblyFunction>)>::new();
    for (function, (module, edges)) in functions {
        let shape = folded_sequence(&edges, syntax_files);
        let items = shape.iter().collect::<BTreeSet<_>>().len();
        let items_unfolded = unfolded_items(&edges);
        shapes
            .entry(shape)
            .or_insert_with(|| (items_unfolded, Vec::new()))
            .1
            .push(AssemblyFunction {
                module,
                function,
                items,
                items_unfolded,
            });
    }
    let mut rows = shapes
        .into_iter()
        .map(|(shape, (items_unfolded, mut functions))| {
            functions.sort();
            CallShape {
                items: shape.iter().collect::<BTreeSet<_>>().len(),
                items_unfolded,
                shape,
                functions,
            }
        })
        .filter(|row| {
            row.items >= CALL_SHAPE_SOLO_ITEMS
                || (row.functions.len() >= 2 && row.items >= CALL_SHAPE_SHARED_ITEMS)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .functions
            .len()
            .cmp(&left.functions.len())
            .then_with(|| right.items.cmp(&left.items))
            .then_with(|| left.shape.cmp(&right.shape))
    });
    rows
}

pub(super) fn repeated_assembly(
    edges: &[Edge],
    target: &ModuleSelector,
    syntax_files: &[FileSyntax],
) -> Vec<AssemblyGroup> {
    let mut raw_functions = BTreeMap::<FunctionId, (String, Vec<&Edge>)>::new();
    for edge in edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && !edge.test
            && target.matches(&edge.to, &edge.to_path)
            && !target.matches(&edge.from, &edge.from_path)
            && !is_type_alias_edge(edge, syntax_files)
    }) {
        let Some(function) = &edge.from_fn else {
            continue;
        };
        let aggregate = raw_functions
            .entry(FunctionId::new(&edge.from_path, function))
            .or_insert_with(|| (edge.from.clone(), Vec::new()));
        aggregate.1.push(edge);
    }
    let functions = raw_functions
        .into_iter()
        .map(|(function, (module, edges))| {
            let items_unfolded = unfolded_items(&edges);
            let items = distinct_folded_items(&edges, syntax_files)
                .into_iter()
                .collect::<BTreeSet<_>>();
            (function, (module, items, items_unfolded))
        })
        .collect::<Vec<_>>();
    let mut intersections = BTreeSet::<Vec<String>>::new();
    for left in 0..functions.len() {
        for right in left + 1..functions.len() {
            if functions[left].1.0 == functions[right].1.0 {
                continue;
            }
            let items = functions[left]
                .1
                .1
                .intersection(&functions[right].1.1)
                .cloned()
                .collect::<Vec<_>>();
            if items.len() < 3 {
                continue;
            }
            intersections.insert(items);
        }
    }
    let rows = intersections
        .into_iter()
        .filter_map(|items| {
            let assembly_functions = functions
                .iter()
                .filter(|(_, (_, function_items, _))| {
                    items.iter().all(|item| function_items.contains(item))
                })
                .map(
                    |(function, (module, function_items, items_unfolded))| AssemblyFunction {
                        module: module.clone(),
                        function: function.clone(),
                        items: function_items.len(),
                        items_unfolded: *items_unfolded,
                    },
                )
                .collect::<Vec<_>>();
            (assembly_functions.len() >= 2
                && assembly_functions
                    .iter()
                    .map(|function| &function.module)
                    .collect::<BTreeSet<_>>()
                    .len()
                    >= 2)
                .then_some((items, assembly_functions))
        })
        .map(|(items, functions)| AssemblyGroup {
            score: items.len() * functions.len(),
            items,
            functions,
            children: Vec::new(),
        })
        .collect::<Vec<_>>();
    let root_indexes = rows
        .iter()
        .enumerate()
        .filter(|(index, row)| {
            !rows.iter().enumerate().any(|(other_index, other)| {
                index != &other_index
                    && other.items.iter().all(|item| row.items.contains(item))
                    && row
                        .functions
                        .iter()
                        .all(|function| other.functions.contains(function))
                    && (row.items.len() > other.items.len()
                        || row.functions.len() < other.functions.len())
            })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut roots = root_indexes
        .iter()
        .map(|index| rows[*index].clone())
        .collect::<Vec<_>>();
    for (index, row) in rows.iter().enumerate() {
        if root_indexes.contains(&index) {
            continue;
        }
        let Some(root) = roots
            .iter_mut()
            .filter(|root| {
                root.items.iter().all(|item| row.items.contains(item))
                    && row
                        .functions
                        .iter()
                        .all(|function| root.functions.contains(function))
            })
            .min_by(|left, right| {
                left.items
                    .len()
                    .cmp(&right.items.len())
                    .then_with(|| right.functions.len().cmp(&left.functions.len()))
                    .then_with(|| left.items.cmp(&right.items))
            })
        else {
            continue;
        };
        root.children.push(AssemblyDepth {
            extra_items: row
                .items
                .iter()
                .filter(|item| !root.items.contains(item))
                .cloned()
                .collect(),
            functions: row.functions.clone(),
        });
    }
    for root in &mut roots {
        root.children.sort_by(|left, right| {
            right
                .functions
                .len()
                .cmp(&left.functions.len())
                .then_with(|| left.extra_items.cmp(&right.extra_items))
        });
    }
    roots.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.items.cmp(&right.items))
    });
    roots
}

pub(super) fn render_callers(out: &mut String, rows: &[Caller], top: usize) {
    out.push_str("# Callers by assembly\n\n");
    out.push_str("| caller | items | max/fn | heaviest functions |\n");
    out.push_str("|---|---:|---:|---|\n");
    for row in rows.iter().take(top) {
        let functions = row
            .top_fns
            .iter()
            .map(|function| {
                format!(
                    "{} ({}:{}; {})",
                    function.function,
                    function.path.display(),
                    function.line,
                    function.items
                )
            })
            .collect::<Vec<_>>()
            .join("<br>");
        writeln!(
            out,
            "| {} | {} | {} | {} |",
            row.module,
            row.items.len(),
            row.max_fn_items,
            functions
        )
        .expect("writing to a String cannot fail");
    }
}

pub(super) fn render_heaviest(
    out: &mut String,
    rows: &[FunctionRow],
    heaviest: Option<&Heaviest>,
    top: usize,
) {
    out.push_str("\n# Heaviest assembly\n\n");
    out.push_str("| function | items | sites | also wires | location |\n");
    out.push_str("|---|---:|---:|---|---|\n");
    for row in rows.iter().take(top) {
        let mut also = row
            .also
            .iter()
            .take(3)
            .map(|(module, items)| format!("{module} {items}"))
            .collect::<Vec<_>>();
        if row.also.len() > 3 {
            also.push(format!("+{}", row.also.len() - 3));
        }
        writeln!(
            out,
            "| {} | {} | {} | {} | {}:{} |",
            row.function,
            row.items.len(),
            row.sites,
            also.join(", "),
            row.path.display(),
            row.line
        )
        .expect("writing to a String cannot fail");
    }
    let Some(heaviest) = heaviest else {
        out.push_str("\nnone\n");
        return;
    };
    writeln!(
        out,
        "\n## Quote\n\n{}:{}-{} `{}`\n\n```rust\n{}\n```",
        heaviest.path.display(),
        heaviest.line,
        heaviest.end_line,
        heaviest.function,
        heaviest.source
    )
    .expect("writing to a String cannot fail");
}

pub(super) fn render_call_shapes(out: &mut String, rows: &[CallShape], top: usize) {
    out.push_str("\n# Call shapes\n\n");
    for row in rows.iter().take(top) {
        let mut shape = row
            .shape
            .iter()
            .take(16)
            .cloned()
            .collect::<Vec<_>>()
            .join(" → ");
        if row.shape.len() > 16 {
            write!(shape, " → … {} more", row.shape.len() - 16)
                .expect("writing to a String cannot fail");
        }
        writeln!(
            out,
            "- ×{} ({} items): `{}`",
            row.functions.len(),
            row.items,
            shape
        )
        .expect("writing to a String cannot fail");
        for function in row.functions.iter().take(5) {
            writeln!(
                out,
                "  - {}::{} ({}:{})",
                function.module,
                function.function.label,
                function.function.path.display(),
                function.function.line
            )
            .expect("writing to a String cannot fail");
        }
    }
}

pub(super) fn render_repeated(out: &mut String, rows: &[AssemblyGroup], top: usize) {
    out.push_str("\n# Repeated assembly\n\n");
    for row in rows.iter().take(top) {
        writeln!(
            out,
            "- {} items × {} functions",
            row.items.len(),
            row.functions.len()
        )
        .expect("writing to a String cannot fail");
        writeln!(out, "  - items: {}", row.items.join(", "))
            .expect("writing to a String cannot fail");
        for function in &row.functions {
            writeln!(
                out,
                "  - {}::{} ({}:{})",
                function.module,
                function.function.label,
                function.function.path.display(),
                function.function.line
            )
            .expect("writing to a String cannot fail");
        }
        for child in &row.children {
            writeln!(
                out,
                "  - + {}: {} of {} functions",
                child.extra_items.join(", "),
                child.functions.len(),
                row.functions.len()
            )
            .expect("writing to a String cannot fail");
        }
    }
}
