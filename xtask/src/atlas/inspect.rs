use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Serialize;

use super::conform::{self, Direction};
use super::detect::{self, GuardFamily};
use super::facts::{Facets, Facts};
use super::history::{self, BlameCommit, Commit};
use super::modules::{
    EscapingItem, bounded_names, crate_module_for_path, escaping_items_for_boundary,
    is_declaration_only, module_is_within, path_in_scope, reference_module_label,
};
use super::output::{self, OutputArgs};
use super::references::{Edge, EdgeKind, FunctionId};
use super::shapes::{self, ShapeFamily};
use super::sources::Source;
use super::syntax::{FileSyntax, PubItem, resolved_internal_import};
use super::target::{self, ModuleRule, TARGET_FILE, Target, Verdict, VerdictKind};
use super::{positive_usize, set_once, value};

const USAGE: &str = "cargo xtask atlas inspect --module <module|path> [--from <module|path>] [--item <module::Name>] [--top N]

Builds a Markdown dossier for one Rust module from exact SCIP references.

  --module <value>  module or root-relative Rust file/directory
  --from <value>    caller module to quote (default: heaviest caller)
  --item <value>    public item key to investigate
  --top <n>         rows and names shown per section (default 20)";

const SECTIONS: &[&str] = &[
    "callers",
    "heaviest",
    "surface",
    "assembly",
    "calls",
    "shapes",
    "guards",
    "providers",
    "footer",
    "item",
];

/// Share of outside production sites the head of the surface table must
/// cover before the summary stops counting items.
const SURFACE_HEAD_SHARE: f64 = 0.8;

fn usage() -> String {
    format!("{USAGE}\n\n{}", output::USAGE)
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    module: String,
    from: Option<String>,
    item: Option<String>,
    top: usize,
    output: OutputArgs,
}

#[derive(Clone, Debug, Serialize)]
struct Caller {
    module: String,
    items: Vec<String>,
    max_fn_items: usize,
    top_fns: Vec<CallerFn>,
}

#[derive(Clone, Debug, Serialize)]
struct CallerFn {
    function: String,
    path: PathBuf,
    line: usize,
    items: usize,
}

#[derive(Clone, Debug, Serialize)]
struct FunctionRow {
    function: String,
    path: PathBuf,
    line: usize,
    end_line: usize,
    items: Vec<String>,
    sites: usize,
    site_lines: Vec<usize>,
}

#[derive(Clone, Debug, Serialize)]
struct Heaviest {
    function: String,
    path: PathBuf,
    line: usize,
    end_line: usize,
    site_lines: Vec<usize>,
    source: String,
}

#[derive(Clone, Debug, Serialize)]
struct SurfaceItem {
    module: String,
    name: String,
    path: PathBuf,
    line: usize,
    test_referrers: usize,
    pass_through: bool,
}

/// One escaping item measured from outside the boundary: how many
/// production sites and files reach it, and from which modules.
#[derive(Clone, Debug, Serialize)]
struct SurfaceRow {
    module: String,
    name: String,
    kind: String,
    path: PathBuf,
    line: usize,
    #[serde(skip)]
    end_line: usize,
    outside_sites: usize,
    outside_files: usize,
    callers: Vec<String>,
    internal_sites: usize,
    test_sites: usize,
}

/// Where test code reaches the module: through its escaping interface or
/// past it, at boundary-visible items the interface does not expose.
#[derive(Clone, Debug, Default, Serialize)]
struct TestReach {
    sites: usize,
    through_interface: usize,
    past_interface: usize,
    past_items: Vec<PastItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct PastItem {
    module: String,
    name: String,
    sites: usize,
}

/// An escaping item at most one production site reaches whose definition
/// no commit has touched since the one that introduced it.
#[derive(Clone, Debug, Serialize)]
struct VestigialItem {
    module: String,
    name: String,
    path: PathBuf,
    line: usize,
    production_sites: usize,
    introduced: BlameCommit,
    /// The introducing commit reads as a fix: the item pins a behaviour
    /// someone already paid for.
    pins_fix: bool,
}

/// Call shapes below this many distinct items only earn a row when several
/// functions share them; longer ones are the assembly a caller pays and
/// stand alone.
const CALL_SHAPE_SHARED_ITEMS: usize = 3;
const CALL_SHAPE_SOLO_ITEMS: usize = 5;

/// One ordered sequence of target-item references shared by caller
/// functions: the call-site test written by the callers themselves.
#[derive(Clone, Debug, Serialize)]
struct CallShape {
    shape: Vec<String>,
    items: usize,
    functions: Vec<AssemblyFunction>,
}

#[derive(Clone, Debug, Serialize)]
struct UnresolvedItem {
    module: String,
    name: String,
    path: PathBuf,
    line: usize,
}

#[derive(Clone, Debug, Serialize)]
struct AssemblyGroup {
    items: Vec<String>,
    functions: Vec<AssemblyFunction>,
    children: Vec<AssemblyDepth>,
    score: usize,
}

#[derive(Clone, Debug, Serialize)]
struct AssemblyDepth {
    extra_items: Vec<String>,
    functions: Vec<AssemblyFunction>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct AssemblyFunction {
    module: String,
    #[serde(serialize_with = "serialize_function_id")]
    function: FunctionId,
}

#[derive(Clone, Debug, Serialize)]
struct Provider {
    module: String,
    sites: usize,
    items: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct RuleRow {
    path: PathBuf,
    provider: String,
    kind: &'static str,
    direction: &'static str,
    admitted: Option<String>,
}

#[derive(Clone, Debug)]
struct ItemCandidate {
    path: PathBuf,
    line: usize,
    owner: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
struct VerdictDiagnostics {
    stale: Vec<String>,
    ambiguous: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ItemEvidence {
    key: String,
    path: PathBuf,
    line: usize,
    declared: String,
    effective_reach: String,
    production_referrers: Vec<String>,
    test_referrers: Vec<String>,
    #[serde(serialize_with = "serialize_commits")]
    commits: Vec<Commit>,
    markers: Vec<String>,
    verdict: Option<Verdict>,
}

#[derive(Debug, Serialize)]
struct Report {
    callers: Vec<Caller>,
    heaviest: HeaviestSection,
    surface: SurfaceSection,
    assembly: Vec<AssemblyGroup>,
    calls: Vec<CallShape>,
    shapes: Vec<ShapeFamily>,
    guards: Vec<GuardFamily>,
    providers: Vec<Provider>,
    footer: Footer,
    item: Option<ItemEvidence>,
}

#[derive(Debug, Serialize)]
struct HeaviestSection {
    functions: Vec<FunctionRow>,
    quote: Option<Heaviest>,
}

#[derive(Debug, Default, Serialize)]
struct SurfaceSection {
    items: Vec<SurfaceRow>,
    outside_sites: usize,
    head_items: usize,
    single_site: usize,
    internal_only: usize,
    test_reach: TestReach,
    vestigial: Vec<VestigialItem>,
    zero_production: Vec<SurfaceItem>,
    unresolved: Vec<UnresolvedItem>,
}

#[derive(Debug, Serialize)]
struct Footer {
    configured: bool,
    rules: Vec<RuleRow>,
    parse_failures: usize,
    unresolved_definitions: usize,
    declaration_only: usize,
    verdicts: VerdictDiagnostics,
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

fn serialize_commits<S>(commits: &[Commit], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    #[derive(Serialize)]
    struct SerializableCommit<'a> {
        id: &'a str,
        short: &'a str,
        time: i64,
        subject: &'a str,
        body: &'a str,
    }
    commits
        .iter()
        .map(|commit| SerializableCommit {
            id: &commit.id,
            short: &commit.short,
            time: commit.time,
            subject: &commit.subject,
            body: &commit.body,
        })
        .collect::<Vec<_>>()
        .serialize(serializer)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ModuleSelector {
    module: String,
    path: Option<PathBuf>,
    directory: bool,
}

impl ModuleSelector {
    pub(super) fn matches(&self, module: &str, path: &Path) -> bool {
        if !module_is_within(module, &self.module) {
            return false;
        }
        match &self.path {
            Some(scope) if self.directory => {
                path_in_scope(path, scope) || path == scope.with_extension("rs")
            }
            Some(file) => path == file,
            None => true,
        }
    }
}

pub(super) fn run(root: &Path, raw: &[String]) -> Result<()> {
    let Some(args) = parse_args(raw)? else {
        return OutputArgs::default().emit(&format!("{}\n", usage()));
    };
    let facts = Facts::load(
        root,
        Path::new("."),
        Facets {
            references: true,
            ..Facets::default()
        },
    )?;
    let module = resolve_module(
        root,
        &facts.syntax.files,
        &args.module,
        "inspect",
        "--module",
    )?;
    let references = facts
        .references
        .as_ref()
        .expect("the required reference facet is loaded");
    let target = target::load(&root.join(TARGET_FILE))?;
    let callers = callers_from_edges(&references.edges, &module);
    let from = args
        .from
        .as_deref()
        .map(|raw| resolve_module(root, &facts.syntax.files, raw, "inspect", "--from"))
        .transpose()?
        .or_else(|| {
            callers.first().map(|caller| ModuleSelector {
                module: caller.module.clone(),
                path: None,
                directory: false,
            })
        });
    let assembly = from.as_ref().map_or_else(Vec::new, |from| {
        assembly_functions(&references.edges, &facts.syntax.files, from, &module)
    });
    let heaviest = assembly
        .first()
        .and_then(|function| quote_function(function, &facts.sources));
    let (mut surface, declaration_only) = surface_section(&facts, &module);
    surface.vestigial = vestigial_items(root, &surface.items)?;
    let repeated = repeated_assembly(&references.edges, &module);
    let calls = call_shapes(&references.edges, &module);
    let shape_families = shapes::families(&facts, Path::new("."))
        .into_iter()
        .filter(|family| {
            family
                .members
                .iter()
                .any(|member| module.matches(&crate_module_for_path(&member.path), &member.path))
        })
        .collect::<Vec<_>>();
    let guard_families = module_item_guards(&facts, &module);
    let providers = providers(&facts, &module);
    let rules = target.as_ref().map_or_else(Vec::new, |target| {
        target_rules(root, target, &facts, &module)
    });
    let verdicts = target
        .as_ref()
        .map_or_else(VerdictDiagnostics::default, |target| {
            stale_module_verdicts(target, &module.module, &facts)
        });
    let item = args
        .item
        .as_deref()
        .map(|key| item_evidence(root, &facts, &module, target.as_ref(), key))
        .transpose()?;

    let parse_failures = facts
        .syntax
        .parse_failures
        .iter()
        .filter(|path| {
            module
                .path
                .as_ref()
                .is_none_or(|scope| path_in_scope(path, scope))
        })
        .count();
    let unresolved_definitions = surface.unresolved.len();
    let report = Report {
        callers,
        heaviest: HeaviestSection {
            functions: assembly,
            quote: heaviest,
        },
        surface,
        assembly: repeated,
        calls,
        shapes: shape_families,
        guards: guard_families,
        providers,
        footer: Footer {
            configured: target.is_some(),
            rules,
            parse_failures,
            unresolved_definitions,
            declaration_only,
            verdicts,
        },
        item,
    };
    let rendered = if args.output.json {
        render_json(&report, &args.output)?
    } else {
        render_markdown(&report, &args.output, args.top)
    };
    args.output.emit(&rendered)
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut module = None;
    let mut from = None;
    let mut item = None;
    let mut top = None;
    let mut output = OutputArgs::default();
    let mut index = 0;
    while index < args.len() {
        if let Some(eaten) = output.parse_flag(args, index, "inspect")? {
            index += eaten;
            continue;
        }
        let flag = args[index].as_str();
        match flag {
            "--module" | "--from" | "--item" => {
                let raw = value(args, index, "inspect", flag)?;
                if raw.is_empty() {
                    bail!("atlas inspect {flag} requires a non-empty value");
                }
                match flag {
                    "--module" => set_once(&mut module, raw.to_owned(), "inspect", flag)?,
                    "--from" => set_once(&mut from, raw.to_owned(), "inspect", flag)?,
                    "--item" => set_once(&mut item, raw.to_owned(), "inspect", flag)?,
                    _ => unreachable!(),
                }
                index += 2;
            }
            "--top" => {
                let raw = value(args, index, "inspect", "--top")?;
                set_once(
                    &mut top,
                    positive_usize(raw, "inspect", "--top")?,
                    "inspect",
                    "--top",
                )?;
                index += 2;
            }
            "--no-index" => bail!("atlas inspect requires the exact SCIP reference index"),
            flag => bail!("unknown atlas inspect flag `{flag}`\n\n{}", usage()),
        }
    }
    output.validate_sections("inspect", SECTIONS)?;
    Ok(Some(Args {
        module: module.ok_or_else(|| anyhow::anyhow!("atlas inspect requires --module"))?,
        from,
        item,
        top: top.unwrap_or(20),
        output,
    }))
}

pub(super) fn resolve_module(
    root: &Path,
    syntax_files: &[FileSyntax],
    raw: &str,
    command: &str,
    flag: &str,
) -> Result<ModuleSelector> {
    let path_like = raw.contains('/') || raw.ends_with(".rs") || root.join(raw).exists();
    let (module, path, directory) = if path_like {
        let path = super::validate_scope(raw, &format!("{command} {flag}"))?;
        let absolute = root.join(&path);
        if !absolute.exists() {
            bail!("atlas {command} {flag} path `{raw}` does not exist");
        }
        let directory = absolute.is_dir();
        if !directory && path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            bail!("atlas {command} {flag} `{raw}` does not match a Rust module");
        }
        let entry = if directory {
            path.join("mod.rs")
        } else {
            path.clone()
        };
        (crate_module_for_path(&entry), Some(path), directory)
    } else {
        (
            raw.strip_prefix("crate::").unwrap_or(raw).to_owned(),
            None,
            false,
        )
    };
    if syntax_files.iter().any(|file| {
        module_is_within(&file.module_path, &module)
            && match &path {
                Some(scope) if directory => {
                    path_in_scope(&file.path, scope) || file.path == scope.with_extension("rs")
                }
                Some(source) => &file.path == source,
                None => true,
            }
    }) {
        return Ok(ModuleSelector {
            module,
            path,
            directory,
        });
    }
    bail!("atlas {command} {flag} `{raw}` does not match a Rust module")
}

fn callers_from_edges(edges: &[Edge], target: &ModuleSelector) -> Vec<Caller> {
    #[derive(Default)]
    struct Assembly {
        items: BTreeSet<String>,
        functions: BTreeMap<FunctionId, BTreeSet<String>>,
    }
    let mut rows = BTreeMap::<String, Assembly>::new();
    for edge in edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && !edge.test
            && target.matches(&edge.to, &edge.to_path)
            && !target.matches(&edge.from, &edge.from_path)
    }) {
        let assembly = rows.entry(edge.from.clone()).or_default();
        assembly.items.insert(edge.item.clone());
        if let Some(function) = &edge.from_fn {
            assembly
                .functions
                .entry(FunctionId::new(&edge.from_path, function))
                .or_default()
                .insert(edge.item.clone());
        }
    }
    let mut rows = rows
        .into_iter()
        .map(|(module, assembly)| {
            let mut top_fns = assembly
                .functions
                .into_iter()
                .map(|(function, items)| CallerFn {
                    function: function.label,
                    path: function.path,
                    line: function.line,
                    items: items.len(),
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
                items: assembly.items.into_iter().collect(),
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

fn assembly_functions(
    edges: &[Edge],
    syntax_files: &[FileSyntax],
    from: &ModuleSelector,
    to: &ModuleSelector,
) -> Vec<FunctionRow> {
    let mut by_function = BTreeMap::<FunctionId, (BTreeSet<String>, usize, BTreeSet<usize>)>::new();
    for edge in edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && !edge.test
            && from.matches(&edge.from, &edge.from_path)
            && to.matches(&edge.to, &edge.to_path)
    }) {
        let Some(function) = &edge.from_fn else {
            continue;
        };
        let aggregate = by_function
            .entry(FunctionId::new(&edge.from_path, function))
            .or_default();
        aggregate.0.insert(edge.item.clone());
        aggregate.1 += 1;
        aggregate.2.insert(edge.from_line);
    }
    let mut rows = by_function
        .into_iter()
        .map(|(function, (items, sites, site_lines))| FunctionRow {
            end_line: function_end_line(syntax_files, &function),
            function: function.label,
            path: function.path,
            line: function.line,
            items: items.into_iter().collect(),
            sites,
            site_lines: site_lines.into_iter().collect(),
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

fn quote_function(function: &FunctionRow, sources: &[Source]) -> Option<Heaviest> {
    let source = sources.iter().find(|source| source.path == function.path)?;
    let source_lines = source.text.lines().collect::<Vec<_>>();
    let start = function.line.max(1);
    let end = function.end_line.min(source_lines.len()).max(start);
    let signature_end = (start..=end)
        .find(|line| source_lines[*line - 1].contains('{'))
        .unwrap_or_else(|| (start + 2).min(end));

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
            && window.start <= previous.end + 1
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
        let available = 80_usize.saturating_sub(source_line_count);
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
        debug_assert!(source_line_count <= 80);
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

/// Measures the escaping interface from outside: per-item outside sites and
/// files, the head that carries most sites, the one-site tail, items only
/// the module itself reaches, test reach, and the unreferenced remainder.
/// Returns the section beside the count of unmeasured declarations.
fn surface_section(facts: &Facts, target: &ModuleSelector) -> (SurfaceSection, usize) {
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
        section.items.push(SurfaceRow {
            module: item.id.module,
            name: item.id.name,
            kind: item.id.kind,
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
fn vestigial_items(root: &Path, rows: &[SurfaceRow]) -> Result<Vec<VestigialItem>> {
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
fn module_item_guards(facts: &Facts, target: &ModuleSelector) -> Vec<GuardFamily> {
    let mut names = BTreeSet::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| target.matches(&file.module_path, &file.path))
    {
        names.extend(file.pub_items.iter().map(|item| item.name.as_str()));
        names.extend(file.fns.iter().map(|function| function.name.as_str()));
    }
    detect::guard_families(facts, Path::new("."))
        .into_iter()
        .filter(|family| {
            detect::named_identifiers(&family.key)
                .into_iter()
                .any(|name| !detect::is_std_identifier(name) && names.contains(name))
        })
        .collect()
}

/// Orders each outside production function's target references by source
/// line, drops consecutive repeats, and groups functions that share the
/// same sequence.
fn call_shapes(edges: &[Edge], target: &ModuleSelector) -> Vec<CallShape> {
    let mut functions = BTreeMap::<FunctionId, (String, Vec<(usize, String)>)>::new();
    for edge in edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && !edge.test
            && target.matches(&edge.to, &edge.to_path)
            && !target.matches(&edge.from, &edge.from_path)
    }) {
        let Some(function) = &edge.from_fn else {
            continue;
        };
        functions
            .entry(FunctionId::new(&edge.from_path, function))
            .or_insert_with(|| (edge.from.clone(), Vec::new()))
            .1
            .push((edge.from_line, edge.item.clone()));
    }
    let mut shapes = BTreeMap::<Vec<String>, Vec<AssemblyFunction>>::new();
    for (function, (module, mut sites)) in functions {
        sites.sort();
        let mut shape = Vec::<String>::new();
        for (_, item) in sites {
            if shape.last() != Some(&item) {
                shape.push(item);
            }
        }
        shapes
            .entry(shape)
            .or_default()
            .push(AssemblyFunction { module, function });
    }
    let mut rows = shapes
        .into_iter()
        .map(|(shape, mut functions)| {
            functions.sort();
            CallShape {
                items: shape.iter().collect::<BTreeSet<_>>().len(),
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

fn repeated_assembly(edges: &[Edge], target: &ModuleSelector) -> Vec<AssemblyGroup> {
    let mut functions = BTreeMap::<FunctionId, (String, BTreeSet<String>)>::new();
    for edge in edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && !edge.test
            && target.matches(&edge.to, &edge.to_path)
            && !target.matches(&edge.from, &edge.from_path)
    }) {
        let Some(function) = &edge.from_fn else {
            continue;
        };
        let aggregate = functions
            .entry(FunctionId::new(&edge.from_path, function))
            .or_insert_with(|| (edge.from.clone(), BTreeSet::new()));
        aggregate.1.insert(edge.item.clone());
    }
    let functions = functions.into_iter().collect::<Vec<_>>();
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
                .filter(|(_, (_, function_items))| {
                    items.iter().all(|item| function_items.contains(item))
                })
                .map(|(function, (module, _))| AssemblyFunction {
                    module: module.clone(),
                    function: function.clone(),
                })
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

fn providers(facts: &Facts, target: &ModuleSelector) -> Vec<Provider> {
    let mut rows = BTreeMap::<String, (usize, BTreeSet<String>)>::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| target.matches(&file.module_path, &file.path))
    {
        for dependency in &file.dependencies {
            let Some(resolved) =
                resolved_internal_import(dependency, &facts.known_modules, &facts.crate_names)
            else {
                continue;
            };
            if module_is_within(&resolved, &target.module) {
                continue;
            }
            let top = resolved.split("::").next().unwrap_or(&resolved).to_owned();
            let aggregate = rows.entry(top).or_default();
            aggregate.0 += 1;
            aggregate.1.insert(dependency.item.clone());
        }
    }
    let mut rows = rows
        .into_iter()
        .map(|(module, (sites, items))| Provider {
            module,
            sites,
            items: items.into_iter().collect(),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .sites
            .cmp(&left.sites)
            .then_with(|| left.module.cmp(&right.module))
    });
    rows
}

fn target_rules(
    root: &Path,
    target: &Target,
    facts: &Facts,
    module: &ModuleSelector,
) -> Vec<RuleRow> {
    let files = facts
        .syntax
        .files
        .iter()
        .filter(|file| module.matches(&file.module_path, &file.path))
        .collect::<Vec<_>>();
    let ranks = target.layer_ranks();
    target
        .modules
        .iter()
        .filter(|rule| {
            files
                .iter()
                .any(|file| conform::rule_covers_path(root, &rule.path, &file.path))
        })
        .flat_map(|rule| {
            files
                .iter()
                .filter(|file| conform::rule_covers_path(root, &rule.path, &file.path))
                .flat_map(|file| &file.dependencies)
                .filter_map(|dependency| {
                    resolved_internal_import(dependency, &facts.known_modules, &facts.crate_names)
                })
                .filter(|provider| !module_is_within(provider, &module.module))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|provider| rule_row(rule, &ranks, &module.module, &provider))
        })
        .collect()
}

fn rule_row(rule: &ModuleRule, ranks: &super::target::LayerRanks, from: &str, to: &str) -> RuleRow {
    let direction = match conform::layer_direction(ranks, from, to) {
        Some(Direction::Upward) => "upward",
        Some(Direction::Same) => "same",
        Some(Direction::Downward) => "downward",
        None => "unranked",
    };
    let admissions = rule
        .allowed_dependencies
        .as_deref()
        .or(rule.upward_dependencies.as_deref())
        .unwrap_or_default();
    RuleRow {
        path: rule.path.clone(),
        provider: to.to_owned(),
        kind: if rule.allowed_dependencies.is_some() {
            "module"
        } else {
            "upward-dependency"
        },
        direction,
        admitted: admissions
            .iter()
            .find(|prefix| module_is_within(to, prefix))
            .cloned(),
    }
}

fn stale_module_verdicts(target: &Target, module: &str, facts: &Facts) -> VerdictDiagnostics {
    let pass_throughs = detect::passthroughs(facts, Path::new("."))
        .into_iter()
        .fold(
            BTreeMap::<String, Vec<ItemCandidate>>::new(),
            |mut rows, row| {
                let owner = facts
                    .syntax
                    .files
                    .iter()
                    .find(|file| file.path == row.path)
                    .and_then(|file| function_owner(file, &row.name, row.line));
                rows.entry(format!("{}::{}", row.module, row.name))
                    .or_default()
                    .push(ItemCandidate {
                        path: row.path,
                        line: row.line,
                        owner,
                    });
                rows
            },
        );
    let items = public_item_candidates(facts);
    let mut diagnostics = VerdictDiagnostics::default();
    for verdict in target.verdicts.iter().filter(|verdict| {
        matches!(verdict.kind, VerdictKind::Item | VerdictKind::PassThrough)
            && (verdict.key == module || verdict.key.starts_with(&format!("{module}::")))
    }) {
        let candidates = match verdict.kind {
            VerdictKind::Item => items.get(&verdict.key),
            VerdictKind::PassThrough => pass_throughs.get(&verdict.key),
            _ => None,
        };
        let label = format!("{:?}:{}", verdict.kind, verdict.key);
        match candidates.map(Vec::as_slice).unwrap_or_default() {
            [] => diagnostics.stale.push(label),
            candidates if candidates.len() > 1 => diagnostics
                .ambiguous
                .push(format!("{label} — {}", ambiguity(&verdict.key, candidates))),
            _ => {}
        }
    }
    diagnostics.stale.sort();
    diagnostics.ambiguous.sort();
    diagnostics
}

fn public_item_candidates(facts: &Facts) -> BTreeMap<String, Vec<ItemCandidate>> {
    let mut candidates = BTreeMap::<String, Vec<ItemCandidate>>::new();
    for file in &facts.syntax.files {
        for item in &file.pub_items {
            candidates
                .entry(format!("{}::{}", item.module, item.name))
                .or_default()
                .push(ItemCandidate {
                    path: file.path.clone(),
                    line: item.line,
                    owner: function_owner(file, &item.name, item.line),
                });
        }
    }
    candidates
}

fn function_owner(file: &FileSyntax, name: &str, line: usize) -> Option<String> {
    file.fns
        .iter()
        .find(|function| function.name == name && function.line == line)
        .and_then(|function| function.owner.clone())
}

fn ambiguity(key: &str, candidates: &[ItemCandidate]) -> String {
    let name = key.rsplit_once("::").map_or(key, |(_, name)| name);
    let locations = candidates
        .iter()
        .map(|candidate| {
            candidate.owner.as_ref().map_or_else(
                || format!("{}:{}", candidate.path.display(), candidate.line),
                |owner| {
                    format!(
                        "{}:{} (owner {owner})",
                        candidate.path.display(),
                        candidate.line
                    )
                },
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "ambiguous: {} public items named {name}: {locations}",
        candidates.len()
    )
}

fn item_evidence(
    root: &Path,
    facts: &Facts,
    target: &ModuleSelector,
    configured: Option<&Target>,
    key: &str,
) -> Result<ItemEvidence> {
    let (item_module, name) = key
        .rsplit_once("::")
        .ok_or_else(|| anyhow::anyhow!("atlas inspect --item requires `module::Name`"))?;
    if !module_is_within(item_module, &target.module) {
        bail!(
            "atlas inspect --item `{key}` is outside --module `{}`",
            target.module
        );
    }
    let matches = facts
        .syntax
        .files
        .iter()
        .filter(|file| target.matches(&file.module_path, &file.path))
        .flat_map(|file| {
            file.pub_items
                .iter()
                .filter(move |item| item.module == item_module && item.name == name)
                .map(move |item| (file, item))
        })
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        let candidates = matches
            .iter()
            .map(|(file, item)| ItemCandidate {
                path: file.path.clone(),
                line: item.line,
                owner: function_owner(file, &item.name, item.line),
            })
            .collect::<Vec<_>>();
        bail!(
            "atlas inspect --item `{key}` is {}",
            ambiguity(key, &candidates)
        );
    }
    let (file, item) = matches
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("atlas inspect --item `{key}` is not a public item"))?;
    let references = facts
        .references
        .as_ref()
        .expect("inspect loads exact references");
    let referrer = |edge: &Edge| {
        let module = reference_module_label(&edge.from, &target.module);
        edge.from_fn.as_ref().map_or_else(
            || format!("{module}::(outside any function)"),
            |function| format!("{module}::{}", function.label),
        )
    };
    let matching = references.edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && edge.item == name
            && edge.to == item_module
            && edge.to_path == file.path
    });
    let production_referrers = matching
        .clone()
        .filter(|edge| !edge.test)
        .map(referrer)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let test_referrers = matching
        .filter(|edge| edge.test)
        .map(referrer)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let commits = history::introducing_commits(root, &file.path, name)?;
    let markers = commits
        .iter()
        .flat_map(|commit| history::fix_markers(&format!("{}\n{}", commit.subject, commit.body)))
        .collect();
    Ok(ItemEvidence {
        key: key.to_owned(),
        path: file.path.clone(),
        line: item.line,
        declared: item.declared.clone(),
        effective_reach: facts.mod_index.effective_reach(file, item),
        production_referrers,
        test_referrers,
        commits,
        markers,
        verdict: configured.and_then(|target| item_verdict(target, key)),
    })
}

fn item_verdict(target: &Target, key: &str) -> Option<Verdict> {
    target
        .verdicts
        .iter()
        .find(|verdict| verdict.kind == VerdictKind::Item && verdict.key == key)
        .cloned()
}

fn render_json(report: &Report, output: &OutputArgs) -> Result<String> {
    let mut value = serde_json::to_value(report)?;
    let object = value
        .as_object_mut()
        .expect("serializing Report always produces a JSON object");
    let mut selected = serde_json::Map::new();
    for section in SECTIONS {
        if output.wants(section)
            && let Some(value) = object.remove(*section)
        {
            selected.insert((*section).to_owned(), value);
        }
    }
    let mut rendered = serde_json::to_string_pretty(&selected)?;
    rendered.push('\n');
    Ok(rendered)
}

fn render_markdown(report: &Report, output: &OutputArgs, top: usize) -> String {
    let mut rendered = String::new();
    if output.wants("callers") {
        render_callers(&mut rendered, &report.callers, top);
    }
    if output.wants("heaviest") {
        render_heaviest(
            &mut rendered,
            &report.heaviest.functions,
            report.heaviest.quote.as_ref(),
            top,
        );
    }
    if output.wants("surface") {
        render_surface(&mut rendered, &report.surface, top);
    }
    if output.wants("assembly") {
        render_repeated(&mut rendered, &report.assembly, top);
    }
    if output.wants("calls") {
        render_call_shapes(&mut rendered, &report.calls, top);
    }
    if output.wants("shapes") || output.wants("guards") {
        render_duplicated(
            &mut rendered,
            output.wants("shapes").then_some(report.shapes.as_slice()),
            output.wants("guards").then_some(report.guards.as_slice()),
            top,
        );
    }
    if output.wants("providers") {
        render_providers(&mut rendered, &report.providers, top);
    }
    if output.wants("footer") {
        render_footer(&mut rendered, &report.footer, top);
    }
    if output.wants("item")
        && let Some(item) = &report.item
    {
        render_item(&mut rendered, item, top);
    }
    rendered
}

fn render_callers(out: &mut String, rows: &[Caller], top: usize) {
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

fn render_heaviest(
    out: &mut String,
    rows: &[FunctionRow],
    heaviest: Option<&Heaviest>,
    top: usize,
) {
    out.push_str("\n# Heaviest assembly\n\n");
    out.push_str("| function | items | sites | location |\n");
    out.push_str("|---|---:|---:|---|\n");
    for row in rows.iter().take(top) {
        writeln!(
            out,
            "| {} | {} | {} | {}:{} |",
            row.function,
            row.items.len(),
            row.sites,
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

fn render_surface(out: &mut String, surface: &SurfaceSection, top: usize) {
    out.push_str("\n# Escaping surface\n\n");
    out.push_str("| item | kind | outside sites | files | internal | tests | callers |\n");
    out.push_str("|---|---|---:|---:|---:|---:|---|\n");
    for row in surface.items.iter().take(top) {
        writeln!(
            out,
            "| `{}::{}` | {} | {} | {} | {} | {} | {} |",
            row.module,
            row.name,
            row.kind,
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

fn render_call_shapes(out: &mut String, rows: &[CallShape], top: usize) {
    out.push_str("\n# Call shapes\n\n");
    for row in rows.iter().take(top) {
        writeln!(
            out,
            "- ×{} ({} items): `{}`",
            row.functions.len(),
            row.items,
            bounded_names(&row.shape, 16).replace(", ", " → ")
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

fn render_repeated(out: &mut String, rows: &[AssemblyGroup], top: usize) {
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

fn render_duplicated(
    out: &mut String,
    shapes: Option<&[ShapeFamily]>,
    guards: Option<&[GuardFamily]>,
    top: usize,
) {
    out.push_str("\n# Duplicated knowledge\n\n");
    if let Some(shapes) = shapes {
        out.push_str("## Shape families\n\n");
        for family in shapes.iter().take(top) {
            writeln!(
                out,
                "- key: `{}` — {} files, mean {:.1} SLOC, score {:.1}",
                family.name, family.files, family.mean_sloc, family.score
            )
            .expect("writing to a String cannot fail");
            for member in family.members.iter().take(5) {
                writeln!(
                    out,
                    "  - {}:{} `{}`",
                    member.path.display(),
                    member.line,
                    member.name
                )
                .expect("writing to a String cannot fail");
            }
        }
    }
    if let Some(guards) = guards {
        out.push_str("\n## Guard families\n\n");
        for family in guards.iter().take(top) {
            writeln!(
                out,
                "- key: `{}` — {} files, {} sites",
                family.key, family.files, family.sites
            )
            .expect("writing to a String cannot fail");
            for site in family.locations.iter().take(5) {
                writeln!(
                    out,
                    "  - {}:{} ({})",
                    site.path.display(),
                    site.line,
                    site.kind
                )
                .expect("writing to a String cannot fail");
            }
        }
    }
}

fn render_providers(out: &mut String, rows: &[Provider], top: usize) {
    out.push_str("\n# Providers\n\n");
    out.push_str("| module | sites | items |\n");
    out.push_str("|---|---:|---|\n");
    for row in rows.iter().take(top) {
        let mut items = row.items.iter().take(top).cloned().collect::<Vec<_>>();
        if row.items.len() > top {
            items.push(format!("… {} more", row.items.len() - top));
        }
        let items = items.join(", ");
        writeln!(out, "| {} | {} | {} |", row.module, row.sites, items)
            .expect("writing to a String cannot fail");
    }
}

fn render_footer(out: &mut String, footer: &Footer, top: usize) {
    out.push_str("\n# Footer\n\n");
    if !footer.configured {
        out.push_str("target rules: no target configured\n");
    } else if footer.rules.is_empty() {
        out.push_str("target rules: no covering rules\n");
    } else {
        out.push_str("| rule | provider | kind | direction | admitted |\n");
        out.push_str("|---|---|---|---|---|\n");
        for rule in footer.rules.iter().take(top) {
            writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                rule.path.display(),
                rule.provider,
                rule.kind,
                rule.direction,
                rule.admitted.as_deref().unwrap_or("none")
            )
            .expect("writing to a String cannot fail");
        }
        if footer.rules.len() > top {
            writeln!(
                out,
                "\n_{} more target-rule rows omitted._",
                footer.rules.len() - top
            )
            .expect("writing to a String cannot fail");
        }
    }
    writeln!(out, "\nparse failures: {}", footer.parse_failures)
        .expect("writing to a String cannot fail");
    writeln!(
        out,
        "unresolved definitions: {}",
        footer.unresolved_definitions
    )
    .expect("writing to a String cannot fail");
    writeln!(
        out,
        "re-exports and mod declarations (unmeasured): {}",
        footer.declaration_only
    )
    .expect("writing to a String cannot fail");
    writeln!(
        out,
        "stale item/pass-through verdicts: {}",
        footer.verdicts.stale.len()
    )
    .expect("writing to a String cannot fail");
    for key in &footer.verdicts.stale {
        writeln!(out, "- `{key}`").expect("writing to a String cannot fail");
    }
    writeln!(
        out,
        "ambiguous item/pass-through verdicts: {}",
        footer.verdicts.ambiguous.len()
    )
    .expect("writing to a String cannot fail");
    for key in &footer.verdicts.ambiguous {
        writeln!(out, "- `{key}`").expect("writing to a String cannot fail");
    }
}

fn render_item(out: &mut String, item: &ItemEvidence, top: usize) {
    writeln!(out, "\n# Item evidence — `{}`\n", item.key).expect("writing to a String cannot fail");
    writeln!(out, "- definition: {}:{}", item.path.display(), item.line)
        .expect("writing to a String cannot fail");
    writeln!(out, "- declared reach: `{}`", item.declared)
        .expect("writing to a String cannot fail");
    writeln!(out, "- effective reach: `{}`", item.effective_reach)
        .expect("writing to a String cannot fail");
    out.push_str("- production referrers:\n");
    for referrer in item.production_referrers.iter().take(top) {
        writeln!(out, "  - `{referrer}`").expect("writing to a String cannot fail");
    }
    out.push_str("- test referrers:\n");
    for referrer in item.test_referrers.iter().take(top) {
        writeln!(out, "  - `{referrer}`").expect("writing to a String cannot fail");
    }
    out.push_str("- introducing commits:\n");
    for commit in &item.commits {
        writeln!(
            out,
            "  - `{}` {} {}",
            commit.short, commit.time, commit.subject
        )
        .expect("writing to a String cannot fail");
    }
    out.push_str("- fix markers:\n");
    for marker in &item.markers {
        writeln!(out, "  - {marker}").expect("writing to a String cannot fail");
    }
    if let Some(verdict) = &item.verdict {
        writeln!(out, "- verdict: {}", verdict.reason).expect("writing to a String cannot fail");
    } else {
        out.push_str("- verdict: none\n");
    }
}

#[cfg(test)]
#[path = "inspect/tests.rs"]
mod tests;
