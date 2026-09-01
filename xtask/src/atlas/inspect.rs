use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::conform::{self, Direction};
use super::facts::{Facets, Facts};
use super::index::IndexPolicy;
use super::modules::{bounded_names, crate_module_for_path, module_is_within, path_in_scope};
use super::references::{Edge, EdgeKind};
use super::sources::Source;
use super::syntax::FileSyntax;
use super::target::{self, ModuleRule, TARGET_FILE, Target};
use super::{REPORT_VERSION, positive_usize, set_once, validate_scope, value};

const USAGE: &str = "cargo xtask atlas inspect --from <module|path> --to <module|path> [--path <scope>] [--file <target.toml>] [--top N] [--md|--json]

Shows the functions in one module that assemble escaping items from another.
The exact reference index is required; use `seams --module` for a use-edge view.

  --from <value>  calling crate module or root-relative file/directory
  --to <value>    target crate module or root-relative file/directory
  --path <path>   root-relative analysis scope (default .)
  --file <path>   target file (default root refactor-target.toml)
  --top <n>       functions, tests, and names shown per list (default 10)
  --md            markdown headings and fenced report sections
  --json          versioned JSON agent contract (v4)";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    from: String,
    to: String,
    path: PathBuf,
    file: Option<PathBuf>,
    top: usize,
    markdown: bool,
    json: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    from: String,
    to: String,
    path: PathBuf,
    totals: Totals,
    functions: Vec<FunctionRow>,
    heaviest: Option<Heaviest>,
    tests: Vec<TestRow>,
    rules: Vec<RuleRow>,
    parse_failures: Vec<PathBuf>,
    #[serde(skip)]
    target_configured: bool,
}

#[derive(Debug, Serialize)]
struct Totals {
    functions: usize,
    items: usize,
    sites: usize,
}

#[derive(Clone, Debug, Serialize)]
struct FunctionRow {
    function: String,
    path: PathBuf,
    line: usize,
    end_line: usize,
    items: Vec<String>,
    sites: usize,
}

#[derive(Debug, Serialize)]
struct Heaviest {
    function: String,
    path: PathBuf,
    line: usize,
    end_line: usize,
    source: String,
}

#[derive(Debug, Serialize)]
struct TestRow {
    path: PathBuf,
    sites: usize,
    items: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RuleRow {
    path: PathBuf,
    kind: &'static str,
    direction: &'static str,
    admitted: Option<String>,
    debt: Option<RuleDebt>,
}

#[derive(Debug, Serialize)]
struct RuleDebt {
    prefix: String,
    sites: usize,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct FunctionKey {
    path: PathBuf,
    function: String,
    line: usize,
}

#[derive(Default)]
struct FunctionAggregate {
    items: BTreeSet<String>,
    sites: usize,
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
pub(super) fn run(root: &Path, args: &[String]) -> Result<()> {
    let Some(args) = parse_args(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let mut report = build_report(root, &args)?;
    if args.json {
        compact_json(&mut report, args.top);
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("rendering atlas inspect JSON")?
        );
    } else {
        print_report(&report, args.top, args.markdown);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut from = None;
    let mut to = None;
    let mut path = None;
    let mut file = None;
    let mut top = None;
    let mut markdown = false;
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--from" => {
                let raw = value(args, index, "inspect", "--from")?;
                if raw.is_empty() {
                    bail!("atlas inspect --from requires a non-empty module or path");
                }
                set_once(&mut from, raw.to_owned(), "inspect", "--from")?;
                index += 2;
            }
            "--to" => {
                let raw = value(args, index, "inspect", "--to")?;
                if raw.is_empty() {
                    bail!("atlas inspect --to requires a non-empty module or path");
                }
                set_once(&mut to, raw.to_owned(), "inspect", "--to")?;
                index += 2;
            }
            "--path" => {
                let raw = value(args, index, "inspect", "--path")?;
                set_once(
                    &mut path,
                    validate_scope(raw, "inspect --path")?,
                    "inspect",
                    "--path",
                )?;
                index += 2;
            }
            "--file" => {
                let raw = value(args, index, "inspect", "--file")?;
                if raw.is_empty() {
                    bail!("atlas inspect --file requires a non-empty path");
                }
                set_once(&mut file, PathBuf::from(raw), "inspect", "--file")?;
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
            "--md" => {
                if markdown || json {
                    bail!("atlas inspect accepts only one of --md or --json");
                }
                markdown = true;
                index += 1;
            }
            "--json" => {
                if markdown || json {
                    bail!("atlas inspect accepts only one of --md or --json");
                }
                json = true;
                index += 1;
            }
            "--no-index" => {
                bail!(
                    "atlas inspect is a reference view; use `atlas seams --module` for the use-edge view"
                );
            }
            flag => bail!("unknown atlas inspect flag `{flag}`\n\n{USAGE}"),
        }
    }
    Ok(Some(Args {
        from: from.ok_or_else(|| anyhow::anyhow!("atlas inspect requires --from"))?,
        to: to.ok_or_else(|| anyhow::anyhow!("atlas inspect requires --to"))?,
        path: path.unwrap_or_else(|| PathBuf::from(".")),
        file,
        top: top.unwrap_or(10),
        markdown,
        json,
    }))
}

fn build_report(root: &Path, args: &Args) -> Result<Report> {
    let facts = Facts::load(
        root,
        &args.path,
        Facets {
            references: Some(IndexPolicy::Required),
            ..Facets::default()
        },
    )?;
    let from = resolve_module(root, &facts.syntax.files, &args.from, "--from")?;
    let to = resolve_module(root, &facts.syntax.files, &args.to, "--to")?;
    let references = facts
        .references
        .as_ref()
        .expect("the required reference facet is loaded");
    let (totals, functions, heaviest, tests) = assembly_report(
        &references.edges,
        &facts.sources,
        &facts.syntax.files,
        &from,
        &to,
        &args.path,
    );

    let target_path = args
        .file
        .as_ref()
        .map_or_else(|| root.join(TARGET_FILE), |path| root.join(path));
    let target = target::load(&target_path)?;
    if args.file.is_some() && target.is_none() {
        bail!(
            "atlas inspect target file `{}` does not exist",
            target_path.display()
        );
    }
    let rules = target.as_ref().map_or_else(
        || Ok(Vec::new()),
        |target| target_rules(root, target, &target_path, &facts.syntax.files, &from, &to),
    )?;
    let target_configured = target.is_some();
    Ok(Report {
        version: REPORT_VERSION,
        verb: "inspect",
        from: from.module,
        to: to.module,
        path: args.path.clone(),
        totals,
        functions,
        heaviest,
        tests,
        rules,
        parse_failures: facts
            .syntax
            .parse_failures
            .iter()
            .filter(|path| path_in_scope(path, &args.path))
            .cloned()
            .collect(),
        target_configured,
    })
}

fn resolve_module(
    root: &Path,
    syntax_files: &[FileSyntax],
    raw: &str,
    flag: &str,
) -> Result<ModuleSelector> {
    let path_like = raw.contains('/') || raw.ends_with(".rs") || root.join(raw).exists();
    let (module, path, directory) = if path_like {
        let path = validate_scope(raw, &format!("inspect {flag}"))?;
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
    let matches = syntax_files.iter().any(|file| {
        module_is_within(&file.module_path, &module)
            && match &path {
                Some(scope) if directory => path_in_scope(&file.path, scope),
                Some(source) => &file.path == source,
                None => true,
            }
    });
    if matches {
        return Ok(ModuleSelector {
            module,
            path,
            directory,
        });
    }
    bail!("atlas inspect {flag} `{raw}` does not match a Rust module")
}

fn assembly_report(
    edges: &[Edge],
    sources: &[Source],
    syntax_files: &[FileSyntax],
    from: &ModuleSelector,
    to: &ModuleSelector,
    scope: &Path,
) -> (Totals, Vec<FunctionRow>, Option<Heaviest>, Vec<TestRow>) {
    let production = edges
        .iter()
        .filter(|edge| {
            edge.kind == EdgeKind::Reference
                && !edge.test
                && path_in_scope(&edge.from_path, scope)
                && from.matches(&edge.from, &edge.from_path)
                && to.matches(&edge.to, &edge.to_path)
        })
        .collect::<Vec<_>>();
    let mut items = BTreeSet::new();
    let mut by_function = BTreeMap::<FunctionKey, FunctionAggregate>::new();
    let mut outside = FunctionAggregate::default();
    for edge in &production {
        items.insert(edge.item.clone());
        if let Some(function) = &edge.from_fn {
            let aggregate = by_function
                .entry(FunctionKey {
                    path: edge.from_path.clone(),
                    function: function.label.clone(),
                    line: function.line,
                })
                .or_default();
            aggregate.items.insert(edge.item.clone());
            aggregate.sites += 1;
        } else {
            outside.items.insert(edge.item.clone());
            outside.sites += 1;
        }
    }
    let mut functions = by_function
        .into_iter()
        .map(|(key, aggregate)| FunctionRow {
            end_line: function_end_line(syntax_files, &key),
            function: key.function,
            path: key.path,
            line: key.line,
            items: aggregate.items.into_iter().collect(),
            sites: aggregate.sites,
        })
        .collect::<Vec<_>>();
    functions.sort_by(|left, right| {
        right
            .items
            .len()
            .cmp(&left.items.len())
            .then_with(|| left.function.cmp(&right.function))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    let function_count = functions.len();
    let heaviest = functions
        .first()
        .and_then(|function| quote_function(function, sources));
    if outside.sites > 0 {
        functions.push(FunctionRow {
            function: "(outside any function)".to_owned(),
            path: PathBuf::from("-"),
            line: 0,
            end_line: 0,
            items: outside.items.into_iter().collect(),
            sites: outside.sites,
        });
    }

    let mut tests_by_path = BTreeMap::<PathBuf, FunctionAggregate>::new();
    for edge in edges.iter().filter(|edge| {
        edge.kind == EdgeKind::Reference
            && edge.test
            && path_in_scope(&edge.from_path, scope)
            && to.matches(&edge.to, &edge.to_path)
            && items.contains(&edge.item)
    }) {
        let aggregate = tests_by_path.entry(edge.from_path.clone()).or_default();
        aggregate.items.insert(edge.item.clone());
        aggregate.sites += 1;
    }
    let mut tests = tests_by_path
        .into_iter()
        .map(|(path, aggregate)| TestRow {
            path,
            sites: aggregate.sites,
            items: aggregate.items.into_iter().collect(),
        })
        .collect::<Vec<_>>();
    tests.sort_by(|left, right| {
        right
            .sites
            .cmp(&left.sites)
            .then_with(|| left.path.cmp(&right.path))
    });
    (
        Totals {
            functions: function_count,
            items: items.len(),
            sites: production.len(),
        },
        functions,
        heaviest,
        tests,
    )
}

fn function_end_line(files: &[FileSyntax], key: &FunctionKey) -> usize {
    files
        .iter()
        .find(|file| file.path == key.path)
        .and_then(|file| {
            file.fns
                .iter()
                .find(|function| function.line == key.line && function.label() == key.function)
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

fn target_rules(
    root: &Path,
    target: &Target,
    target_path: &Path,
    syntax_files: &[FileSyntax],
    from: &ModuleSelector,
    to: &ModuleSelector,
) -> Result<Vec<RuleRow>> {
    let debt_sites = conform::debt_sites_by_rule(root, target, target_path)?;
    let from_files = syntax_files
        .iter()
        .filter(|file| from.matches(&file.module_path, &file.path))
        .collect::<Vec<_>>();
    let ranks = target.layer_ranks();
    Ok(target
        .modules
        .iter()
        .filter(|rule| {
            from_files
                .iter()
                .any(|file| conform::rule_covers_path(root, &rule.path, &file.path))
        })
        .map(|rule| rule_row(rule, &debt_sites, &ranks, &from.module, &to.module))
        .collect())
}

fn rule_row(
    rule: &ModuleRule,
    debt_sites: &BTreeMap<(PathBuf, String), usize>,
    ranks: &super::target::LayerRanks,
    from: &str,
    to: &str,
) -> RuleRow {
    let direction = match conform::layer_direction(ranks, from, to) {
        Some(Direction::Upward) => "upward",
        Some(Direction::Same) => "same",
        Some(Direction::Downward) => "downward",
        None => "unranked",
    };
    let admissions = rule
        .allowed_imports
        .as_deref()
        .or(rule.upward_imports.as_deref())
        .unwrap_or_default();
    let admitted = admissions
        .iter()
        .find(|prefix| modules_overlap(to, prefix))
        .cloned();
    let debt = rule
        .upward_debt
        .iter()
        .flatten()
        .find(|prefix| modules_overlap(to, prefix))
        .map(|prefix| RuleDebt {
            prefix: prefix.clone(),
            sites: debt_sites
                .get(&(rule.path.clone(), prefix.clone()))
                .copied()
                .unwrap_or(0),
        });
    RuleRow {
        path: rule.path.clone(),
        kind: if rule.allowed_imports.is_some() {
            "module"
        } else {
            "upward-import"
        },
        direction,
        admitted,
        debt,
    }
}

fn modules_overlap(left: &str, right: &str) -> bool {
    module_is_within(left, right) || module_is_within(right, left)
}

fn compact_json(report: &mut Report, top: usize) {
    let outside = report
        .functions
        .iter()
        .position(|function| function.function == "(outside any function)")
        .map(|index| report.functions.remove(index));
    report.functions.truncate(top);
    if let Some(outside) = outside {
        report.functions.push(outside);
    }
    for function in &mut report.functions {
        function.items.truncate(top);
    }
    report.tests.truncate(top);
    for test in &mut report.tests {
        test.items.truncate(top);
    }
}

#[expect(clippy::print_stdout, reason = "atlas inspect report helper")]
fn print_report(report: &Report, top: usize, markdown: bool) {
    let fence = markdown_fence(
        report
            .heaviest
            .as_ref()
            .map_or("", |heaviest| &heaviest.source),
    );
    println!(
        "{} → {}: {} functions, {} distinct items, {} sites",
        report.from, report.to, report.totals.functions, report.totals.items, report.totals.sites
    );
    section(
        &format!("Functions ({})", report.totals.functions),
        markdown,
        &fence,
    );
    println!("function | path:line | items | sites");
    for function in displayed_functions(report, top) {
        println!(
            "{} | {}:{} | {} | {}",
            function.function,
            function.path.display(),
            function.line,
            function.items.len(),
            function.sites
        );
    }
    close(markdown, &fence);

    section("Items by function", markdown, &fence);
    for function in displayed_functions(report, top) {
        println!(
            "{}: {}",
            function.function,
            bounded_names(&function.items, top)
        );
    }
    close(markdown, &fence);

    section("Heaviest call site", markdown, &fence);
    if let Some(heaviest) = &report.heaviest {
        println!(
            "{}:{}-{}  {}",
            heaviest.path.display(),
            heaviest.line,
            heaviest.end_line,
            heaviest.function
        );
        println!("{}", heaviest.source);
    } else {
        println!("none");
    }
    close(markdown, &fence);

    section(
        &format!("Tests touching the same items ({})", report.tests.len()),
        markdown,
        &fence,
    );
    for test in report.tests.iter().take(top) {
        println!(
            "{}: {} sites — {}",
            test.path.display(),
            test.sites,
            bounded_names(&test.items, top)
        );
    }
    close(markdown, &fence);

    section("Target rules", markdown, &fence);
    if !report.target_configured {
        println!("no target configured");
    } else if report.rules.is_empty() {
        println!("no covering rules");
    } else {
        println!("rule path | kind | direction | admitted | debt");
        for rule in &report.rules {
            let debt = rule.debt.as_ref().map_or_else(
                || "none".to_owned(),
                |debt| format!("{} ({} sites)", debt.prefix, debt.sites),
            );
            println!(
                "{} | {} | {} | {} | {}",
                rule.path.display(),
                rule.kind,
                rule.direction,
                rule.admitted.as_deref().unwrap_or("none"),
                debt
            );
        }
    }
    close(markdown, &fence);

    if !report.parse_failures.is_empty() {
        section("Parse failures", markdown, &fence);
        for path in &report.parse_failures {
            println!("{}", path.display());
        }
        close(markdown, &fence);
    }
}

fn displayed_functions(report: &Report, top: usize) -> Vec<&FunctionRow> {
    let mut functions = report
        .functions
        .iter()
        .filter(|function| function.function != "(outside any function)")
        .take(top)
        .collect::<Vec<_>>();
    functions.extend(
        report
            .functions
            .iter()
            .find(|function| function.function == "(outside any function)"),
    );
    functions
}

#[expect(clippy::print_stdout, reason = "atlas inspect report helper")]
fn section(name: &str, markdown: bool, fence: &str) {
    if markdown {
        println!("\n## {name}\n{fence}text");
    } else {
        println!("\n{name}");
    }
}

#[expect(clippy::print_stdout, reason = "atlas inspect report helper")]
fn close(markdown: bool, fence: &str) {
    if markdown {
        println!("{fence}");
    }
}

fn markdown_fence(contents: &str) -> String {
    let longest = contents
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat(longest.saturating_add(1).max(3))
}

#[cfg(test)]
mod tests;
