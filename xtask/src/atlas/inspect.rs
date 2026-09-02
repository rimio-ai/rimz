use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Serialize;

use super::conform::{self, Direction};
use super::detect::{self, GuardFamily, IdentifierRole};
use super::facts::{Facets, Facts};
use super::history::{self, Commit};
use super::modules::{
    crate_module_for_path, module_is_within, path_in_scope, reference_module_label,
};
use super::output::{self, OutputArgs};
use super::references::{Edge, EdgeKind};
use super::shapes::{self, ShapeFamily};
use super::syntax::{FileSyntax, resolved_internal_import};
use super::target::{self, ModuleRule, TARGET_FILE, Target, Verdict, VerdictKind};
use super::{positive_usize, set_once, value};

mod calls;
mod flags;
mod selector;
mod surface;
#[cfg(test)]
mod testkit;
mod verdict;

use calls::{
    AssemblyGroup, CallShape, Caller, HeaviestSection, assembly_functions, call_shapes,
    callers_from_edges, quote_function, render_call_shapes, render_callers, render_heaviest,
    render_repeated, repeated_assembly,
};
use flags::{FlagSection, flag_section, render_flags};
use selector::ModuleSelector;
pub(super) use selector::resolve_module;
use surface::{SurfaceSection, render_surface, surface_section, vestigial_items};
use verdict::{InspectVerdict, render_verdict};

const USAGE: &str = "cargo xtask atlas inspect --module <module|path> [--from <module|path>] [--item <module::Name>] [--top N] [--all]

Builds a Markdown dossier for one Rust module from exact SCIP references.

  --module <value>  module or root-relative Rust file/directory
  --from <value>    caller module to quote (default: heaviest caller)
  --item <value>    public item key to investigate
  --top <n>         rows and names shown per section (default 20)
  --all             show shape and guard families below the finding gate";

const SECTIONS: &[&str] = &[
    "verdict",
    "record",
    "callers",
    "heaviest",
    "surface",
    "assembly",
    "calls",
    "flags",
    "shapes",
    "guards",
    "providers",
    "footer",
    "item",
];

fn usage() -> String {
    format!("{USAGE}\n\n{}", output::USAGE)
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    module: String,
    from: Option<String>,
    item: Option<String>,
    top: usize,
    all: bool,
    output: OutputArgs,
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

/// What the repository already says about the module: its root file's
/// `//!` header and the nearest `AGENTS.md` contract above it, so a dossier
/// carries the record beside the numbers.
#[derive(Debug, Serialize)]
struct Record {
    path: Option<PathBuf>,
    header: Option<String>,
    contract: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct Report {
    verdict: InspectVerdict,
    record: Record,
    callers: Vec<Caller>,
    heaviest: HeaviestSection,
    surface: SurfaceSection,
    assembly: Vec<AssemblyGroup>,
    calls: Vec<CallShape>,
    flags: FlagSection,
    shapes: Vec<ShapeFamily>,
    guards: Vec<GuardFamily>,
    providers: Vec<Provider>,
    footer: Footer,
    item: Option<ItemEvidence>,
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
    let callers = callers_from_edges(&references.edges, &module, &facts.syntax.files);
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
    let repeated = repeated_assembly(&references.edges, &module, &facts.syntax.files);
    let calls = call_shapes(&references.edges, &module, &facts.syntax.files);
    let flags = flag_section(&facts, &module, &surface);
    let shape_families = if args.all {
        shapes::families_all_with_dropped(&facts, Path::new(".")).0
    } else {
        shapes::families(&facts, Path::new("."))
    }
    .into_iter()
    .filter(|family| {
        family
            .members
            .iter()
            .any(|member| module.matches(&crate_module_for_path(&member.path), &member.path))
    })
    .collect::<Vec<_>>();
    let guard_families = module_item_guards(&facts, &module, args.all);
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
    let verdict = InspectVerdict::from_report_data(
        &surface,
        &repeated,
        &callers,
        &assembly,
        flags.one_caller_count(),
        flags.constant_count(),
    );
    let report = Report {
        verdict,
        record: record(root, &facts, &module),
        callers,
        heaviest: HeaviestSection {
            functions: assembly,
            quote: heaviest,
        },
        surface,
        assembly: repeated,
        calls,
        flags,
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
    let mut all = false;
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
            "--all" => {
                if all {
                    bail!("atlas inspect --all may only be passed once");
                }
                all = true;
                index += 1;
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
        all,
        output,
    }))
}

/// Guard families anywhere in the crate that name an item the target
/// defines; `include_all` keeps the ones below the finding gate. A path
/// chain counts only under one of the target's own modules or types, so
/// `event::poll` never matches a module that also defines a `poll`.
fn module_item_guards(
    facts: &Facts,
    target: &ModuleSelector,
    include_all: bool,
) -> Vec<GuardFamily> {
    let mut names = BTreeSet::new();
    let mut scopes = BTreeSet::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| target.matches(&file.module_path, &file.path))
    {
        names.extend(file.pub_items.iter().map(|item| item.name.as_str()));
        names.extend(file.fns.iter().map(|function| function.name.as_str()));
        scopes.extend(file.module_path.rsplit("::").next());
        scopes.extend(
            file.mod_decls
                .iter()
                .filter_map(|(module, _)| module.rsplit("::").next()),
        );
        scopes.extend(
            file.pub_items
                .iter()
                .filter(|item| {
                    matches!(
                        item.kind.as_str(),
                        "struct" | "enum" | "trait" | "type" | "union"
                    )
                })
                .map(|item| item.name.as_str()),
        );
    }
    let default_keys = detect::guard_families(facts, Path::new("."))
        .into_iter()
        .map(|family| family.key)
        .collect::<BTreeSet<_>>();
    detect::guard_families_all_with_dropped(facts, Path::new("."))
        .0
        .into_iter()
        .filter(|family| {
            let mut qualified = false;
            let targets_module =
                detect::named_identifier_roles(&family.key)
                    .into_iter()
                    .any(|identifier| {
                        let segments = identifier
                            .segments()
                            .filter(|segment| !matches!(*segment, "crate" | "self" | "super"))
                            .collect::<Vec<_>>();
                        let Some(last) = segments.last() else {
                            return false;
                        };
                        if detect::is_std_identifier(last) {
                            return false;
                        }
                        if segments.len() >= 2 {
                            qualified = true;
                            segments.first().is_some_and(|first| {
                                scopes.contains(first)
                                    && (names.contains(last) || names.contains(first))
                            })
                        } else {
                            names.contains(last)
                                && matches!(
                                    identifier.role,
                                    IdentifierRole::Method | IdentifierRole::Field
                                )
                        }
                    });
            targets_module
                && (include_all || qualified || default_keys.contains(family.key.as_str()))
        })
        .collect()
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
    let markers = commit_markers(&commits);
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

fn commit_markers(commits: &[Commit]) -> Vec<String> {
    let mut marker_commits = BTreeSet::new();
    commits
        .iter()
        .filter(|commit| {
            !history::fix_markers(&format!("{}\n{}", commit.subject, commit.body)).is_empty()
                && marker_commits.insert(commit.id.as_str())
        })
        .map(|commit| format!("{} {}", commit.short, commit.subject))
        .collect()
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

fn record(root: &Path, facts: &Facts, module: &ModuleSelector) -> Record {
    let root_file = facts
        .syntax
        .files
        .iter()
        .find(|file| file.module_path == module.module);
    // `store/mod.rs` and `store.rs` both start the walk at `store/`, then
    // climb to the crate root.
    let contract = root_file.and_then(|file| {
        file.path
            .with_extension("")
            .ancestors()
            .take_while(|ancestor| ancestor.starts_with(&file.crate_path))
            .map(|ancestor| ancestor.join("AGENTS.md"))
            .find(|candidate| root.join(candidate).is_file())
    });
    Record {
        path: root_file.map(|file| file.path.clone()),
        header: root_file.and_then(|file| file.doc_head.clone()),
        contract,
    }
}

fn render_record(out: &mut String, record: &Record) {
    out.push_str("\n# Record\n\n");
    match &record.path {
        Some(path) => writeln!(out, "- module: {}", path.display()),
        None => writeln!(out, "- module: no root file"),
    }
    .expect("writing to a String cannot fail");
    match &record.contract {
        Some(path) => writeln!(out, "- contract: {}", path.display()),
        None => writeln!(out, "- contract: none above the module"),
    }
    .expect("writing to a String cannot fail");
    match &record.header {
        Some(header) => writeln!(out, "- header: {header}\n"),
        None => writeln!(out, "- header: none\n"),
    }
    .expect("writing to a String cannot fail");
}

fn render_markdown(report: &Report, output: &OutputArgs, top: usize) -> String {
    let mut rendered = String::new();
    if output.wants("verdict") {
        render_verdict(&mut rendered, &report.verdict);
    }
    if output.wants("record") {
        render_record(&mut rendered, &report.record);
    }
    if output.wants("item")
        && let Some(item) = &report.item
    {
        render_item(&mut rendered, item, top);
    }
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
    if output.wants("flags") {
        render_flags(&mut rendered, &report.flags, top);
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
    rendered
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
            let provider = family
                .provider
                .as_ref()
                .map_or_else(String::new, |provider| format!(", provider {provider}"));
            writeln!(
                out,
                "- key: `{}` — {} files, mean {:.1} SLOC, score {:.1}{provider}",
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
