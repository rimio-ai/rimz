use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::conform::{self, Direction};
use super::detect::{self, GuardFamily};
use super::facts::{Facets, Facts};
use super::history::{self, Commit};
use super::index::IndexPolicy;
use super::modules::{
    EscapingItem, crate_module_for_path, escaping_items_for_boundary, module_is_within,
    path_in_scope, reference_module_label,
};
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

#[derive(Debug, PartialEq, Eq)]
struct Args {
    module: String,
    from: Option<String>,
    item: Option<String>,
    top: usize,
}

#[derive(Clone, Debug)]
struct Caller {
    module: String,
    items: Vec<String>,
    max_fn_items: usize,
    top_fns: Vec<CallerFn>,
}

#[derive(Clone, Debug)]
struct CallerFn {
    function: String,
    path: PathBuf,
    line: usize,
    items: usize,
}

#[derive(Clone, Debug)]
struct FunctionRow {
    function: String,
    path: PathBuf,
    line: usize,
    end_line: usize,
    items: Vec<String>,
    sites: usize,
}

#[derive(Clone, Debug)]
struct Heaviest {
    function: String,
    path: PathBuf,
    line: usize,
    end_line: usize,
    source: String,
}

#[derive(Clone, Debug)]
struct SurfaceItem {
    module: String,
    name: String,
    path: PathBuf,
    line: usize,
    test_referrers: usize,
    pass_through: bool,
}

#[derive(Clone, Debug)]
struct UnresolvedItem {
    module: String,
    name: String,
    path: PathBuf,
    line: usize,
}

#[derive(Clone, Debug)]
struct AssemblyGroup {
    items: Vec<String>,
    functions: Vec<AssemblyFunction>,
    score: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AssemblyFunction {
    module: String,
    function: FunctionId,
}

#[derive(Clone, Debug)]
struct Provider {
    module: String,
    sites: usize,
    items: Vec<String>,
}

#[derive(Clone, Debug)]
struct RuleRow {
    path: PathBuf,
    provider: String,
    kind: &'static str,
    direction: &'static str,
    admitted: Option<String>,
}

#[derive(Clone, Debug)]
struct ItemEvidence {
    key: String,
    path: PathBuf,
    line: usize,
    declared: String,
    effective_reach: String,
    production_referrers: Vec<String>,
    test_referrers: Vec<String>,
    commits: Vec<Commit>,
    markers: Vec<String>,
    verdict: Option<Verdict>,
}

#[derive(Debug, PartialEq, Eq)]
struct ModuleSelector {
    module: String,
    path: Option<PathBuf>,
    directory: bool,
}

impl ModuleSelector {
    fn matches(&self, module: &str, path: &Path) -> bool {
        if !module_is_within(module, &self.module) {
            return false;
        }
        match &self.path {
            Some(scope) if self.directory => path_in_scope(path, scope),
            Some(file) => path == file,
            None => true,
        }
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas inspect output is the command's stdout contract"
)]
pub(super) fn run(root: &Path, raw: &[String]) -> Result<()> {
    let Some(args) = parse_args(raw)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let facts = Facts::load(
        root,
        Path::new("."),
        Facets {
            references: Some(IndexPolicy::Required),
            ..Facets::default()
        },
    )?;
    let module = resolve_module(root, &facts.syntax.files, &args.module, "--module")?;
    let references = facts
        .references
        .as_ref()
        .expect("the required reference facet is loaded");
    let target = target::load(&root.join(TARGET_FILE))?;
    let callers = callers_from_edges(&references.edges, &module);
    let from = args
        .from
        .as_deref()
        .map(|raw| resolve_module(root, &facts.syntax.files, raw, "--from"))
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
    let (zero_surface, unresolved) = zero_production_surface(&facts, &module);
    let repeated = repeated_assembly(&references.edges, &module, args.top);
    let shape_families = shapes::families(&facts, Path::new("."))
        .into_iter()
        .filter(|family| {
            family
                .members
                .iter()
                .any(|member| module.matches(&crate_module_for_path(&member.path), &member.path))
        })
        .collect::<Vec<_>>();
    let guard_families = detect::guard_families(&facts, Path::new("."))
        .into_iter()
        .filter(|family| {
            family
                .locations
                .iter()
                .any(|site| module.matches(&crate_module_for_path(&site.path), &site.path))
        })
        .collect::<Vec<_>>();
    let providers = providers(&facts, &module);
    let rules = target.as_ref().map_or_else(Vec::new, |target| {
        target_rules(root, target, &facts, &module, &providers)
    });
    let stale = target.as_ref().map_or_else(Vec::new, |target| {
        stale_module_verdicts(target, &module.module, &facts)
    });
    let item = args
        .item
        .as_deref()
        .map(|key| item_evidence(root, &facts, &module, target.as_ref(), key))
        .transpose()?;

    print_callers(&callers, args.top);
    print_heaviest(&assembly, heaviest.as_ref(), args.top);
    print_zero_surface(&zero_surface, &unresolved, args.top);
    print_repeated(&repeated, args.top);
    print_duplicated(&shape_families, &guard_families, args.top);
    print_providers(&providers, args.top);
    print_footer(
        &facts,
        &module,
        target.is_some(),
        &rules,
        &unresolved,
        &stale,
    );
    if let Some(item) = &item {
        print_item(item, args.top);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut module = None;
    let mut from = None;
    let mut item = None;
    let mut top = None;
    let mut index = 0;
    while index < args.len() {
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
            flag => bail!("unknown atlas inspect flag `{flag}`\n\n{USAGE}"),
        }
    }
    Ok(Some(Args {
        module: module.ok_or_else(|| anyhow::anyhow!("atlas inspect requires --module"))?,
        from,
        item,
        top: top.unwrap_or(20),
    }))
}

fn resolve_module(
    root: &Path,
    syntax_files: &[FileSyntax],
    raw: &str,
    flag: &str,
) -> Result<ModuleSelector> {
    let path_like = raw.contains('/') || raw.ends_with(".rs") || root.join(raw).exists();
    let (module, path, directory) = if path_like {
        let path = super::validate_scope(raw, &format!("inspect {flag}"))?;
        let absolute = root.join(&path);
        if !absolute.exists() {
            bail!("atlas inspect {flag} path `{raw}` does not exist");
        }
        let directory = absolute.is_dir();
        if !directory && path.extension().and_then(|extension| extension.to_str()) != Some("rs") {
            bail!("atlas inspect {flag} `{raw}` does not match a Rust module");
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
                Some(scope) if directory => path_in_scope(&file.path, scope),
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
    bail!("atlas inspect {flag} `{raw}` does not match a Rust module")
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
    let mut by_function = BTreeMap::<FunctionId, (BTreeSet<String>, usize)>::new();
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
    }
    let mut rows = by_function
        .into_iter()
        .map(|(function, (items, sites))| FunctionRow {
            end_line: function_end_line(syntax_files, &function),
            function: function.label,
            path: function.path,
            line: function.line,
            items: items.into_iter().collect(),
            sites,
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
    let span_lines = function.end_line.saturating_sub(function.line) + 1;
    let mut lines = source
        .text
        .lines()
        .skip(function.line.saturating_sub(1))
        .take(span_lines.min(80))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if span_lines > 80 {
        lines.push(format!("… {} more lines", span_lines - 80));
    }
    Some(Heaviest {
        function: function.function.clone(),
        path: function.path.clone(),
        line: function.line,
        end_line: function.end_line,
        source: lines.join("\n"),
    })
}

fn zero_production_surface(
    facts: &Facts,
    target: &ModuleSelector,
) -> (Vec<SurfaceItem>, Vec<UnresolvedItem>) {
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
    let mut zero = Vec::new();
    let mut unresolved = Vec::new();
    for item in escaping {
        let Some((file, definition)) = definition_for_escaping(&files, &item) else {
            continue;
        };
        let Some(item_refs) = references.get(file, definition) else {
            unresolved.push(UnresolvedItem {
                module: item.id.module,
                name: item.id.name,
                path: item.path,
                line: item.line,
            });
            continue;
        };
        if item_refs.production_count != 0 {
            continue;
        }
        zero.push(SurfaceItem {
            module: item.id.module,
            name: item.id.name.clone(),
            path: item.path,
            line: item.line,
            test_referrers: item_refs.test_count,
            pass_through: file
                .fns
                .iter()
                .any(|function| function.name == item.id.name && function.forwards.is_some()),
        });
    }
    zero.sort_by(|left, right| {
        right
            .test_referrers
            .cmp(&left.test_referrers)
            .then_with(|| left.module.cmp(&right.module))
            .then_with(|| left.name.cmp(&right.name))
    });
    unresolved.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
    });
    (zero, unresolved)
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

fn repeated_assembly(edges: &[Edge], target: &ModuleSelector, top: usize) -> Vec<AssemblyGroup> {
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
    let mut intersections = BTreeMap::<Vec<String>, BTreeSet<AssemblyFunction>>::new();
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
            let group = intersections.entry(items).or_default();
            for (function, (module, _)) in [&functions[left], &functions[right]] {
                group.insert(AssemblyFunction {
                    module: module.clone(),
                    function: function.clone(),
                });
            }
        }
    }
    let mut rows = intersections
        .into_iter()
        .filter(|(_, functions)| {
            functions.len() >= 2
                && functions
                    .iter()
                    .map(|function| &function.module)
                    .collect::<BTreeSet<_>>()
                    .len()
                    >= 2
        })
        .map(|(items, functions)| AssemblyGroup {
            score: items.len() * functions.len(),
            items,
            functions: functions.into_iter().collect(),
        })
        .collect::<Vec<_>>();
    let retained = rows
        .iter()
        .enumerate()
        .filter(|(index, row)| {
            !rows.iter().enumerate().any(|(other_index, other)| {
                index != &other_index
                    && row.functions == other.functions
                    && row.items.len() < other.items.len()
                    && row.items.iter().all(|item| other.items.contains(item))
            })
        })
        .map(|(_, row)| row.clone())
        .collect::<Vec<_>>();
    rows = retained;
    rows.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.items.cmp(&right.items))
    });
    rows.truncate(top);
    rows
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
    providers: &[Provider],
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
            providers
                .iter()
                .map(|provider| rule_row(rule, &ranks, &module.module, &provider.module))
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

fn stale_module_verdicts(target: &Target, module: &str, facts: &Facts) -> Vec<String> {
    let pass_throughs = detect::passthroughs(facts, Path::new("."))
        .into_iter()
        .map(|row| format!("{}::{}", row.module, row.name))
        .collect::<BTreeSet<_>>();
    let items = facts
        .syntax
        .files
        .iter()
        .flat_map(|file| &file.pub_items)
        .map(|item| format!("{}::{}", item.module, item.name))
        .collect::<BTreeSet<_>>();
    let mut stale = target
        .verdicts
        .iter()
        .filter(|verdict| {
            matches!(verdict.kind, VerdictKind::Item | VerdictKind::PassThrough)
                && (verdict.key == module || verdict.key.starts_with(&format!("{module}::")))
                && match verdict.kind {
                    VerdictKind::Item => !items.contains(&verdict.key),
                    VerdictKind::PassThrough => !pass_throughs.contains(&verdict.key),
                    _ => false,
                }
        })
        .map(|verdict| format!("{:?}:{}", verdict.kind, verdict.key))
        .collect::<Vec<_>>();
    stale.sort();
    stale
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
    let (file, item) = facts
        .syntax
        .files
        .iter()
        .filter(|file| target.matches(&file.module_path, &file.path))
        .find_map(|file| {
            file.pub_items
                .iter()
                .find(|item| item.module == item_module && item.name == name)
                .map(|item| (file, item))
        })
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
            && target.matches(&edge.to, &edge.to_path)
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

#[expect(clippy::print_stdout, reason = "atlas inspect Markdown section")]
fn print_callers(rows: &[Caller], top: usize) {
    println!("# Callers by assembly\n");
    println!("| caller | items | max/fn | heaviest functions |");
    println!("|---|---:|---:|---|");
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
        println!(
            "| {} | {} | {} | {} |",
            row.module,
            row.items.len(),
            row.max_fn_items,
            functions
        );
    }
}

#[expect(clippy::print_stdout, reason = "atlas inspect Markdown section")]
fn print_heaviest(rows: &[FunctionRow], heaviest: Option<&Heaviest>, top: usize) {
    println!("\n# Heaviest assembly\n");
    println!("| function | items | sites | location |");
    println!("|---|---:|---:|---|");
    for row in rows.iter().take(top) {
        println!(
            "| {} | {} | {} | {}:{} |",
            row.function,
            row.items.len(),
            row.sites,
            row.path.display(),
            row.line
        );
    }
    let Some(heaviest) = heaviest else {
        println!("\nnone");
        return;
    };
    println!(
        "\n## Quote\n\n{}:{}-{} `{}`\n\n```rust\n{}\n```",
        heaviest.path.display(),
        heaviest.line,
        heaviest.end_line,
        heaviest.function,
        heaviest.source
    );
}

#[expect(clippy::print_stdout, reason = "atlas inspect Markdown section")]
fn print_zero_surface(zero: &[SurfaceItem], unresolved: &[UnresolvedItem], top: usize) {
    println!("\n# Zero-production surface\n");
    for item in zero.iter().take(top) {
        let pass_through = if item.pass_through {
            " (pass-through)"
        } else {
            ""
        };
        println!(
            "- `{}`::`{}` — {}:{}; test referrers: {}{}",
            item.module,
            item.name,
            item.path.display(),
            item.line,
            item.test_referrers,
            pass_through
        );
    }
    println!("\n## Unresolved definitions\n");
    for item in unresolved.iter().take(top) {
        println!(
            "- `{}`::`{}` — {}:{}",
            item.module,
            item.name,
            item.path.display(),
            item.line
        );
    }
}

#[expect(clippy::print_stdout, reason = "atlas inspect Markdown section")]
fn print_repeated(rows: &[AssemblyGroup], top: usize) {
    println!("\n# Repeated assembly\n");
    for row in rows.iter().take(top) {
        println!(
            "- {} items × {} functions",
            row.items.len(),
            row.functions.len()
        );
        println!("  - items: {}", row.items.join(", "));
        for function in &row.functions {
            println!(
                "  - {}::{} ({}:{})",
                function.module,
                function.function.label,
                function.function.path.display(),
                function.function.line
            );
        }
    }
}

#[expect(clippy::print_stdout, reason = "atlas inspect Markdown section")]
fn print_duplicated(shapes: &[ShapeFamily], guards: &[GuardFamily], top: usize) {
    println!("\n# Duplicated knowledge\n");
    println!("## Shape families\n");
    for family in shapes.iter().take(top) {
        println!(
            "- key: `{}` — {} files, mean {:.1} SLOC, score {:.1}",
            family.name, family.files, family.mean_sloc, family.score
        );
        for member in family.members.iter().take(5) {
            println!(
                "  - {}:{} `{}`",
                member.path.display(),
                member.line,
                member.name
            );
        }
    }
    println!("\n## Guard families\n");
    for family in guards.iter().take(top) {
        println!(
            "- key: `{}` — {} files, {} sites",
            family.key, family.files, family.sites
        );
        for site in family.locations.iter().take(5) {
            println!("  - {}:{} ({})", site.path.display(), site.line, site.kind);
        }
    }
}

#[expect(clippy::print_stdout, reason = "atlas inspect Markdown section")]
fn print_providers(rows: &[Provider], top: usize) {
    println!("\n# Providers\n");
    println!("| module | sites | items |");
    println!("|---|---:|---|");
    for row in rows.iter().take(top) {
        let items = if rows.len() <= top {
            row.items.join(", ")
        } else {
            "—".to_owned()
        };
        println!("| {} | {} | {} |", row.module, row.sites, items);
    }
}

#[expect(clippy::print_stdout, reason = "atlas inspect Markdown section")]
fn print_footer(
    facts: &Facts,
    module: &ModuleSelector,
    configured: bool,
    rules: &[RuleRow],
    unresolved: &[UnresolvedItem],
    stale: &[String],
) {
    println!("\n# Footer\n");
    if !configured {
        println!("target rules: no target configured");
    } else if rules.is_empty() {
        println!("target rules: no covering rules");
    } else {
        println!("| rule | provider | kind | direction | admitted |");
        println!("|---|---|---|---|---|");
        for rule in rules {
            println!(
                "| {} | {} | {} | {} | {} |",
                rule.path.display(),
                rule.provider,
                rule.kind,
                rule.direction,
                rule.admitted.as_deref().unwrap_or("none")
            );
        }
    }
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
    println!("\nparse failures: {parse_failures}");
    println!("unresolved definitions: {}", unresolved.len());
    println!("stale item/pass-through verdicts: {}", stale.len());
    for key in stale {
        println!("- `{key}`");
    }
}

#[expect(clippy::print_stdout, reason = "atlas inspect Markdown section")]
fn print_item(item: &ItemEvidence, top: usize) {
    println!("\n# Item evidence — `{}`\n", item.key);
    println!("- definition: {}:{}", item.path.display(), item.line);
    println!("- declared reach: `{}`", item.declared);
    println!("- effective reach: `{}`", item.effective_reach);
    println!("- production referrers:");
    for referrer in item.production_referrers.iter().take(top) {
        println!("  - `{referrer}`");
    }
    println!("- test referrers:");
    for referrer in item.test_referrers.iter().take(top) {
        println!("  - `{referrer}`");
    }
    println!("- introducing commits:");
    for commit in &item.commits {
        println!("  - `{}` {} {}", commit.short, commit.time, commit.subject);
    }
    println!("- fix markers:");
    for marker in &item.markers {
        println!("  - {marker}");
    }
    if let Some(verdict) = &item.verdict {
        println!("- verdict: {}", verdict.reason);
    } else {
        println!("- verdict: none");
    }
}

#[cfg(test)]
#[path = "inspect/tests.rs"]
mod tests;
