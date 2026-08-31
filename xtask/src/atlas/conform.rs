use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::facts::{Facets, Facts};
use super::modules::{crate_module_for_path, module_is_within, path_in_scope, scope_for_matching};
use super::sources::Source;
use super::syntax;
use super::target::{self, ModuleRule, TARGET_FILE, Target};
use super::{REPORT_VERSION, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";

const USAGE: &str = "cargo xtask atlas conform [--ratchet|--tighten|--init] [--file <path>] [--path <prefix>] [--layers <a,b,...>] [--verbose] [--json]

Compares the working tree with a refactor target (root refactor-target.toml by
default). `--ratchet` fails only when current values exceed budgets/baselines or
an import is outside its allow list. `--tighten` atomically lowers budgets and
baselines to current values and removes unused upward-import admissions; it never
raises them. A strangler counts whole-word
occurrences of its symbol in non-test Rust under its path (a file or directory).
A missing default target passes; a missing explicit --file is an error. `--init`
creates a clean current-tree baseline and never overwrites an existing target.
Import allow-lists cover resolved internal `use` declarations only.
Split Rust modules (`foo.rs` plus `foo/`) remain separate filesystem rules.

  --ratchet      fail on regressions (the checks/gate mode)
  --tighten      lower budgets/baselines and remove unused upward imports
  --init         seed module budgets, layers, and upward imports from the current tree
  --file <path>  target file (default root refactor-target.toml);
                 absolute as-is, relative from the repository root
  --path <path>  root-relative init subtree (default crates/rimz/src)
  --verbose      show every rule instead of folding rules exactly at budget
  --json         versioned JSON agent contract (v3)

Schema:
  version = 3
  layers = [\"store\", \"agents\", \"cli\"]
  [[module]]
  path = \"crates/rimz/src/cli\"
  upward-imports = [\"agents\"]
  surface-budget = 10
  [[strangler]]
  symbol = \"legacy_symbol\"
  path = \"crates/rimz/src/cli\"
  baseline = 2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Report,
    Ratchet,
    Tighten,
    Init,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
    file: Option<PathBuf>,
    path: Option<PathBuf>,
    layers: Option<Vec<String>>,
    verbose: bool,
    json: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ImportSite {
    module: String,
    path: PathBuf,
    line: usize,
}

#[derive(Clone, Debug, Serialize)]
struct RuleResult {
    kind: &'static str,
    path: PathBuf,
    symbol: Option<String>,
    status: &'static str,
    current: usize,
    budget: usize,
    delta: isize,
    unallowed_imports: Vec<String>,
    unallowed_import_sites: Vec<ImportSite>,
    #[serde(skip)]
    used_upward_imports: BTreeSet<String>,
    config_line: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    target: PathBuf,
    #[serde(skip)]
    default_target: bool,
    layers: Vec<String>,
    rules: Vec<RuleResult>,
    regressions: usize,
    parse_failures: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas conform output is a command stdout contract"
)]
pub(super) fn run(root: &Path, args: &[String]) -> Result<()> {
    let Some(args) = parse_args(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let default_target = args.file.is_none();
    let target_path = args
        .file
        .as_ref()
        .map_or_else(|| root.join(TARGET_FILE), |file| root.join(file));
    if args.mode == Mode::Init {
        if target_path.exists() {
            bail!(
                "atlas conform --init refuses to overwrite existing target `{}`",
                target_path.display()
            );
        }
        let scope = args
            .path
            .as_deref()
            .unwrap_or_else(|| Path::new(DEFAULT_PATH));
        let target = initialize(root, scope, args.layers.as_deref())?;
        let seeded = target.modules.len();
        target::write(&target_path, &target)?;
        println!(
            "initialized {} with {} module rules",
            target_path.display(),
            seeded
        );
        return Ok(());
    }
    let Some(mut target) = target::load(&target_path)? else {
        if args.file.is_some() {
            bail!(
                "atlas conform target file `{}` does not exist",
                target_path.display()
            );
        }
        if args.mode != Mode::Ratchet {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "version": REPORT_VERSION,
                        "verb": "conform",
                        "target": target_path,
                        "configured": false,
                    }))
                    .context("rendering unconfigured atlas conform JSON")?
                );
            } else {
                println!(
                    "Atlas conform — no {TARGET_FILE}; nothing to check (seed one with --init)"
                );
            }
        }
        return Ok(());
    };
    let report = evaluate(root, &target, &target_path, default_target)?;
    match args.mode {
        Mode::Report => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .context("rendering atlas conform JSON")?
                );
            } else {
                print_report(&report, args.verbose);
            }
            Ok(())
        }
        Mode::Ratchet => enforce(&report),
        Mode::Tighten => {
            tighten(&mut target, &report);
            target::write(&target_path, &target)?;
            if !args.json {
                println!("tightened {}", target_path.display());
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .context("rendering atlas conform JSON")?
                );
            }
            Ok(())
        }
        Mode::Init => unreachable!("init returns immediately after writing the new target"),
    }
}

pub(super) fn ratchet(root: &Path) -> Result<()> {
    let target_path = root.join(TARGET_FILE);
    let Some(target) = target::load(&target_path)? else {
        return Ok(());
    };
    enforce(&evaluate(root, &target, &target_path, true)?)
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut mode = Mode::Report;
    let mut file = None;
    let mut path = None;
    let mut layers = None;
    let mut verbose = false;
    let mut json = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--ratchet" if mode == Mode::Report => {
                mode = Mode::Ratchet;
                index += 1;
            }
            "--tighten" if mode == Mode::Report => {
                mode = Mode::Tighten;
                index += 1;
            }
            "--init" if mode == Mode::Report => {
                mode = Mode::Init;
                index += 1;
            }
            "--ratchet" | "--tighten" | "--init" => {
                bail!("atlas conform --ratchet, --tighten, and --init are mutually exclusive")
            }
            "--file" => {
                let raw = value(args, index, "conform", "--file")?;
                if raw.is_empty() {
                    bail!("atlas conform --file requires a non-empty path");
                }
                let parsed = PathBuf::from(raw);
                set_once(&mut file, parsed, "conform", "--file")?;
                index += 2;
            }
            "--path" => {
                let parsed = validate_scope(value(args, index, "conform", "--path")?, "--path")?;
                set_once(&mut path, parsed, "conform", "--path")?;
                index += 2;
            }
            "--layers" => {
                let parsed = value(args, index, "conform", "--layers")?
                    .split(',')
                    .map(str::trim)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if parsed.is_empty() || parsed.iter().any(String::is_empty) {
                    bail!("atlas conform --layers requires comma-separated module names");
                }
                set_once(&mut layers, parsed, "conform", "--layers")?;
                index += 2;
            }
            "--verbose" if !verbose => {
                verbose = true;
                index += 1;
            }
            "--verbose" => bail!("atlas conform --verbose may only be passed once"),
            "--json" if !json => {
                json = true;
                index += 1;
            }
            "--json" => bail!("atlas conform --json may only be passed once"),
            _ => bail!("unknown atlas conform argument `{arg}`"),
        }
    }
    if mode == Mode::Ratchet && json {
        bail!("atlas conform --ratchet does not combine with --json");
    }
    if mode == Mode::Init && json {
        bail!("atlas conform --init does not combine with --json");
    }
    if mode != Mode::Init && path.is_some() {
        bail!("atlas conform --path requires --init");
    }
    if mode != Mode::Init && layers.is_some() {
        bail!("atlas conform --layers requires --init");
    }
    if mode != Mode::Report && verbose {
        bail!("atlas conform --verbose is only valid for the default report mode");
    }
    if json && verbose {
        bail!("atlas conform --verbose does not combine with --json");
    }
    Ok(Some(Args {
        mode,
        file,
        path,
        layers,
        verbose,
        json,
    }))
}

fn initialize(root: &Path, scope: &Path, requested_layers: Option<&[String]>) -> Result<Target> {
    let facts = Facts::load(root, scope, Facets::default())?;
    let scoped_files = facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, scope))
        .collect::<Vec<_>>();
    let mut files_by_rule = BTreeMap::new();
    for file in scoped_files {
        files_by_rule
            .entry(direct_rule_path(&file.path, scope))
            .or_insert_with(Vec::new)
            .push(file);
    }
    let layers = requested_layers.map_or_else(
        || greedy_layers(scope, files_by_rule.values().flatten().copied(), &facts),
        <[String]>::to_vec,
    );
    let layer_positions = layers
        .iter()
        .enumerate()
        .map(|(index, layer)| (layer.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    let modules = files_by_rule
        .into_iter()
        .map(|(path, files)| {
            let module_entry = if root.join(&path).is_dir() {
                path.join("mod.rs")
            } else {
                path.clone()
            };
            let target_module = crate_module_for_path(&module_entry);
            let from_layer = files
                .first()
                .and_then(|file| layer_positions.get(top_module(&file.module_path)).copied());
            let imports = files
                .iter()
                .flat_map(|file| &file.imports)
                .filter_map(|import| {
                    syntax::resolved_internal_import(
                        import,
                        &facts.known_modules,
                        &facts.crate_names,
                    )
                })
                .filter(|import| {
                    let to_layer = layer_positions.get(top_module(import)).copied();
                    from_layer.zip(to_layer).is_some_and(|(from, to)| to > from)
                })
                .collect::<BTreeSet<_>>();
            let mut upward_imports = Vec::<String>::new();
            for import in imports {
                if !upward_imports
                    .iter()
                    .any(|allowed| module_is_within(&import, allowed))
                {
                    upward_imports.push(import);
                }
            }
            ModuleRule {
                path,
                allowed_imports: None,
                upward_imports: (!upward_imports.is_empty()).then_some(upward_imports),
                surface_budget: escaping_surface(
                    files.iter().copied(),
                    &target_module,
                    &facts.mod_index,
                ),
                config_line: 0,
            }
        })
        .collect();
    Ok(Target {
        version: 3,
        layers,
        modules,
        strangler: Vec::new(),
    })
}

fn top_module(module: &str) -> &str {
    module
        .split("::")
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or("(crate)")
}

fn greedy_layers<'a>(
    _scope: &Path,
    files: impl Iterator<Item = &'a syntax::FileSyntax>,
    facts: &Facts,
) -> Vec<String> {
    let files = files.collect::<Vec<_>>();
    let mut nodes = files
        .iter()
        .map(|file| top_module(&file.module_path).to_owned())
        .collect::<BTreeSet<_>>();
    let mut edges = BTreeMap::<(String, String), usize>::new();
    for file in files {
        let from = top_module(&file.module_path);
        for import in &file.imports {
            let Some(resolved) =
                syntax::resolved_internal_import(import, &facts.known_modules, &facts.crate_names)
            else {
                continue;
            };
            let to = top_module(&resolved);
            if from != to && nodes.contains(to) {
                *edges.entry((from.to_owned(), to.to_owned())).or_default() += 1;
            }
        }
    }
    let mut layers = Vec::with_capacity(nodes.len());
    while !nodes.is_empty() {
        let next = nodes
            .iter()
            .max_by(|left, right| {
                let score = |node: &str| {
                    let incoming = edges
                        .iter()
                        .filter(|((_, to), _)| to == node)
                        .map(|(_, weight)| *weight as isize)
                        .sum::<isize>();
                    let outgoing = edges
                        .iter()
                        .filter(|((from, _), _)| from == node)
                        .map(|(_, weight)| *weight as isize)
                        .sum::<isize>();
                    incoming - outgoing
                };
                score(left).cmp(&score(right)).then_with(|| right.cmp(left))
            })
            .cloned()
            .expect("a non-empty layer graph has a candidate");
        nodes.remove(&next);
        edges.retain(|(edge, _), _| edge != &next);
        layers.push(next);
    }
    layers
}

fn direct_rule_path(path: &Path, scope: &Path) -> PathBuf {
    let scope = scope_for_matching(scope);
    let relative = path.strip_prefix(scope).unwrap_or(path);
    if relative.components().count() <= 1 {
        path.to_path_buf()
    } else {
        scope.join(
            relative
                .components()
                .next()
                .expect("a scoped source has a first path component"),
        )
    }
}

fn evaluate(
    root: &Path,
    target: &Target,
    target_path: &Path,
    default_target: bool,
) -> Result<Report> {
    let facts = Facts::load(root, Path::new("."), Facets::default())?;
    let mut rules = Vec::new();
    let mut parse_failures = 0;
    for module in &target.modules {
        let absolute = root.join(&module.path);
        if !absolute.exists() {
            bail!(
                "{}:{}: configured module path `{}` does not exist",
                target_path.display(),
                module.config_line,
                module.path.display()
            );
        }
        let module_files = facts
            .syntax
            .files
            .iter()
            .filter(|file| {
                if absolute.is_file() {
                    file.path == module.path
                } else {
                    path_in_scope(&file.path, &module.path)
                }
            })
            .collect::<Vec<_>>();
        parse_failures += facts
            .syntax
            .parse_failures
            .iter()
            .filter(|path| {
                if absolute.is_file() {
                    *path == &module.path
                } else {
                    path_in_scope(path, &module.path)
                }
            })
            .count();
        let module_entry = if absolute.is_dir() {
            module.path.join("mod.rs")
        } else {
            module.path.clone()
        };
        let target_module = crate_module_for_path(&module_entry);
        let current = escaping_surface(
            module_files.iter().copied(),
            &target_module,
            &facts.mod_index,
        );
        let mut unallowed = BTreeMap::<String, BTreeSet<(PathBuf, usize)>>::new();
        let mut used_upward_imports = BTreeSet::new();
        for file in module_files {
            for import in &file.imports {
                let Some(resolved) = syntax::resolved_internal_import(
                    import,
                    &facts.known_modules,
                    &facts.crate_names,
                ) else {
                    continue;
                };
                if module_is_within(&resolved, &target_module) {
                    continue;
                }
                let allowed = if let Some(allowed_imports) = &module.allowed_imports {
                    allowed_imports
                        .iter()
                        .any(|allowed| module_is_within(&resolved, allowed))
                } else {
                    let from = target
                        .layers
                        .iter()
                        .position(|layer| layer == top_module(&file.module_path));
                    let to = target
                        .layers
                        .iter()
                        .position(|layer| layer == top_module(&resolved));
                    match from.zip(to) {
                        Some((from, to)) if to > from => {
                            let matching = module
                                .upward_imports
                                .as_deref()
                                .unwrap_or_default()
                                .iter()
                                .filter(|allowed| module_is_within(&resolved, allowed))
                                .cloned()
                                .collect::<Vec<_>>();
                            used_upward_imports.extend(matching.iter().cloned());
                            !matching.is_empty()
                        }
                        _ => true,
                    }
                };
                if allowed {
                    continue;
                }
                unallowed
                    .entry(resolved)
                    .or_default()
                    .insert((file.path.clone(), import.line));
            }
        }
        let unallowed_imports = unallowed.keys().cloned().collect::<Vec<_>>();
        let unallowed_import_sites = unallowed
            .into_iter()
            .flat_map(|(module, sites)| {
                sites.into_iter().map(move |(path, line)| ImportSite {
                    module: module.clone(),
                    path,
                    line,
                })
            })
            .collect();
        let regression = current > module.surface_budget || !unallowed_imports.is_empty();
        rules.push(RuleResult {
            kind: if module.allowed_imports.is_some() {
                "module"
            } else {
                "upward-import"
            },
            path: module.path.clone(),
            symbol: None,
            status: if regression { "regression" } else { "ok" },
            current,
            budget: module.surface_budget,
            delta: current as isize - module.surface_budget as isize,
            unallowed_imports,
            unallowed_import_sites,
            used_upward_imports,
            config_line: module.config_line,
        });
    }
    for strangler in &target.strangler {
        let absolute = root.join(&strangler.path);
        if !absolute.exists() {
            bail!(
                "{}:{}: configured strangler path `{}` does not exist",
                target_path.display(),
                strangler.config_line,
                strangler.path.display()
            );
        }
        let scoped_sources = sources_for_path(&facts.sources, &strangler.path, absolute.is_file());
        let current = count_in_sources(&scoped_sources, &facts.syntax.files, &strangler.symbol);
        rules.push(RuleResult {
            kind: "strangler",
            path: strangler.path.clone(),
            symbol: Some(strangler.symbol.clone()),
            status: if current > strangler.baseline {
                "regression"
            } else {
                "ok"
            },
            current,
            budget: strangler.baseline,
            delta: current as isize - strangler.baseline as isize,
            unallowed_imports: Vec::new(),
            unallowed_import_sites: Vec::new(),
            used_upward_imports: BTreeSet::new(),
            config_line: strangler.config_line,
        });
    }
    Ok(Report {
        version: REPORT_VERSION,
        verb: "conform",
        target: target_path.to_path_buf(),
        default_target,
        layers: target.layers.clone(),
        regressions: rules
            .iter()
            .filter(|rule| rule.status == "regression")
            .count(),
        rules,
        parse_failures,
    })
}

fn escaping_surface<'a>(
    files: impl IntoIterator<Item = &'a syntax::FileSyntax>,
    target_module: &str,
    mod_index: &syntax::ModIndex,
) -> usize {
    files
        .into_iter()
        .map(|file| {
            file.pub_items
                .iter()
                .filter(|item| {
                    let reach = mod_index.effective_reach(file, item);
                    !module_is_within(&reach, target_module)
                })
                .count()
        })
        .sum::<usize>()
}

fn sources_for_path(sources: &[Source], path: &Path, is_file: bool) -> Vec<Source> {
    sources
        .iter()
        .filter(|source| {
            if is_file {
                source.path == path
            } else {
                path_in_scope(&source.path, path)
            }
        })
        .cloned()
        .collect()
}

fn count_in_sources(
    sources: &[Source],
    syntax_files: &[syntax::FileSyntax],
    symbol: &str,
) -> usize {
    sources
        .iter()
        .filter(|source| source.is_production())
        .map(|source| {
            let test_regions = syntax_files
                .iter()
                .find(|file| file.path == source.path)
                .map_or(&[][..], |file| file.test_regions.as_slice());
            source
                .text
                .split_inclusive('\n')
                .enumerate()
                .filter(|(index, _)| {
                    !test_regions
                        .iter()
                        .any(|region| region.contains(&(index + 1)))
                })
                .flat_map(|(_, line)| {
                    line.split(|character: char| character != '_' && !character.is_alphanumeric())
                })
                .filter(|identifier| *identifier == symbol)
                .count()
        })
        .sum()
}

fn enforce(report: &Report) -> Result<()> {
    if report.regressions == 0 {
        return Ok(());
    }
    let mut violations = Vec::new();
    for rule in report
        .rules
        .iter()
        .filter(|rule| rule.status == "regression")
    {
        if rule.current > rule.budget {
            violations.push(format!(
                "{}:{}: {} `{}` is {} above {}",
                report.target.display(),
                rule.config_line,
                rule.kind,
                rule.symbol
                    .as_deref()
                    .unwrap_or_else(|| rule.path.to_str().unwrap_or("?")),
                rule.current,
                rule.budget
            ));
        }
        if !rule.unallowed_imports.is_empty() {
            let sites = rule
                .unallowed_import_sites
                .iter()
                .take(5)
                .map(|site| format!("{}:{} ({})", site.path.display(), site.line, site.module))
                .collect::<Vec<_>>()
                .join(", ");
            violations.push(format!(
                "{}:{}: module `{}` imports outside its allow list: {}\n  source: {}",
                report.target.display(),
                rule.config_line,
                rule.path.display(),
                rule.unallowed_imports.join(", "),
                sites,
            ));
        }
    }
    let file_arg = if report.default_target {
        String::new()
    } else {
        format!(" --file {}", report.target.display())
    };
    bail!(
        "atlas conform ratchet regressed:\n{}\n\nReduce the current values, run `cargo xtask atlas conform --tighten{}` after improvement, or deliberately edit {} to reopen a budget.",
        violations.join("\n"),
        file_arg,
        report.target.display()
    )
}

fn tighten(target: &mut Target, report: &Report) {
    let mut results = report.rules.iter();
    for module in &mut target.modules {
        if let Some(result) = results.next() {
            module.surface_budget = module.surface_budget.min(result.current);
            if let Some(upward_imports) = &mut module.upward_imports {
                upward_imports.retain(|import| result.used_upward_imports.contains(import));
                if upward_imports.is_empty() {
                    module.upward_imports = None;
                }
            }
        }
    }
    for strangler in &mut target.strangler {
        if let Some(result) = results.next() {
            strangler.baseline = strangler.baseline.min(result.current);
        }
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas conform report is the command's stdout contract"
)]
fn print_report(report: &Report, verbose: bool) {
    println!("Atlas conform — {}", report.target.display());
    if !report.layers.is_empty() {
        println!("layers: {}", report.layers.join(" < "));
    }
    println!("status      kind        current  budget      Δ  rule");
    let (rules, folded) = displayed_rules(report, verbose);
    for rule in rules {
        let label = rule.symbol.as_ref().map_or_else(
            || rule.path.display().to_string(),
            |symbol| format!("{symbol} ({})", rule.path.display()),
        );
        println!(
            "{:<11} {:<10} {:>7} {:>7} {:+6}  {}",
            rule.status, rule.kind, rule.current, rule.budget, rule.delta, label
        );
        if !rule.unallowed_imports.is_empty() {
            println!("  unallowed imports: {}", rule.unallowed_imports.join(", "));
            for site in rule.unallowed_import_sites.iter().take(5) {
                println!(
                    "    {}:{} imports {}",
                    site.path.display(),
                    site.line,
                    site.module
                );
            }
            if rule.unallowed_import_sites.len() > 5 {
                println!(
                    "    … and {} more source locations",
                    rule.unallowed_import_sites.len() - 5
                );
            }
        }
    }
    if folded > 0 {
        println!("… {folded} rules ok at budget (use --verbose to show)");
    }
    println!(
        "summary: {} rules, {} regressions, {} parse failures",
        report.rules.len(),
        report.regressions,
        report.parse_failures
    );
}

fn displayed_rules(report: &Report, verbose: bool) -> (Vec<&RuleResult>, usize) {
    if verbose {
        return (report.rules.iter().collect(), 0);
    }
    let mut displayed = report
        .rules
        .iter()
        .filter(|rule| rule.status == "regression")
        .collect::<Vec<_>>();
    displayed.extend(
        report
            .rules
            .iter()
            .filter(|rule| rule.status != "regression" && rule.current < rule.budget),
    );
    let folded = report.rules.len().saturating_sub(displayed.len());
    (displayed, folded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn conform_args_separate_report_ratchet_and_tighten() {
        assert_eq!(
            parse_args(&[]).unwrap(),
            Some(Args {
                mode: Mode::Report,
                file: None,
                path: None,
                layers: None,
                verbose: false,
                json: false,
            })
        );
        assert_eq!(
            parse_args(&["--ratchet".into()]).unwrap(),
            Some(Args {
                mode: Mode::Ratchet,
                file: None,
                path: None,
                layers: None,
                verbose: false,
                json: false,
            })
        );
        assert!(parse_args(&["--ratchet".into(), "--tighten".into()]).is_err());
        assert_eq!(
            parse_args(&["--file".into(), "targets/cli.toml".into()])
                .unwrap()
                .unwrap()
                .file,
            Some(PathBuf::from("targets/cli.toml"))
        );
        assert_eq!(
            parse_args(&["--file".into(), "/tmp/target.toml".into()])
                .unwrap()
                .unwrap()
                .file,
            Some(PathBuf::from("/tmp/target.toml"))
        );
        assert_eq!(
            parse_args(&["--file".into(), "../target.toml".into()])
                .unwrap()
                .unwrap()
                .file,
            Some(PathBuf::from("../target.toml"))
        );
        assert!(parse_args(&["--file".into(), String::new()]).is_err());
        assert!(parse_args(&["--path".into(), "src".into()]).is_err());
        assert!(parse_args(&["--ratchet".into(), "--verbose".into()]).is_err());
        assert!(parse_args(&["--json".into(), "--verbose".into()]).is_err());
        assert!(parse_args(&["--verbose".into()]).unwrap().unwrap().verbose);
        assert_eq!(
            parse_args(&["--init".into(), "--path".into(), "src".into()])
                .unwrap()
                .unwrap()
                .path,
            Some(PathBuf::from("src"))
        );
    }

    #[test]
    fn allow_list_matches_descendants_not_prefix_collisions() {
        assert!(module_is_within("cli::render::table", "cli::render"));
        assert!(!module_is_within("cli::renderer", "cli::render"));
    }

    #[test]
    fn surface_budget_counts_only_reach_outside_the_rule_module() {
        let sources = vec![
            Source::new("src/lib.rs", "mod feature;\n"),
            Source::new(
                "src/feature/mod.rs",
                "mod detail;\npub(in crate) fn crate_wide() {}\n",
            ),
            Source::new(
                "src/feature/detail.rs",
                "pub(super) fn sibling_only() {}\npub fn behind_private_link() {}\n",
            ),
        ];
        let syntax = syntax::analyze_sources(&sources);
        let index = syntax::ModIndex::new(&syntax.files);
        let feature_files = syntax
            .files
            .iter()
            .filter(|file| file.module_path.starts_with("feature"));
        assert_eq!(escaping_surface(feature_files, "feature", &index), 1);
    }

    #[test]
    fn crate_root_surface_counts_only_crate_external_reach() {
        let sources = vec![Source::new(
            "src/lib.rs",
            "pub fn external() {}\npub(crate) fn crate_only() {}\n",
        )];
        let syntax = syntax::analyze_sources(&sources);
        let index = syntax::ModIndex::new(&syntax.files);

        assert_eq!(escaping_surface(&syntax.files, "", &index), 1);
    }

    #[test]
    fn conform_default_report_prioritizes_regressions_and_headroom() {
        let rule = |path: &str, status, current, budget| RuleResult {
            kind: "module",
            path: PathBuf::from(path),
            symbol: None,
            status,
            current,
            budget,
            delta: current as isize - budget as isize,
            unallowed_imports: Vec::new(),
            unallowed_import_sites: Vec::new(),
            used_upward_imports: BTreeSet::new(),
            config_line: 1,
        };
        let report = Report {
            version: REPORT_VERSION,
            verb: "conform",
            target: PathBuf::from("target.toml"),
            default_target: false,
            layers: Vec::new(),
            rules: vec![
                rule("at-budget", "ok", 2, 2),
                rule("headroom", "ok", 1, 3),
                rule("regression", "regression", 4, 2),
            ],
            regressions: 1,
            parse_failures: 0,
        };

        let (displayed, folded) = displayed_rules(&report, false);
        assert_eq!(
            displayed
                .iter()
                .map(|rule| rule.path.as_path())
                .collect::<Vec<_>>(),
            [Path::new("regression"), Path::new("headroom")]
        );
        assert_eq!(folded, 1);
        assert_eq!(displayed_rules(&report, true).0.len(), 3);
    }

    #[test]
    fn tighten_removes_unused_upward_import_admissions() {
        let mut target = Target {
            version: 3,
            layers: vec!["store".to_owned(), "cli".to_owned()],
            modules: vec![ModuleRule {
                path: PathBuf::from("src/store"),
                allowed_imports: None,
                upward_imports: Some(vec!["cli".to_owned(), "agents".to_owned()]),
                surface_budget: 4,
                config_line: 2,
            }],
            strangler: Vec::new(),
        };
        let report = Report {
            version: REPORT_VERSION,
            verb: "conform",
            target: PathBuf::from("target.toml"),
            default_target: false,
            layers: target.layers.clone(),
            rules: vec![RuleResult {
                kind: "upward-import",
                path: PathBuf::from("src/store"),
                symbol: None,
                status: "ok",
                current: 3,
                budget: 4,
                delta: -1,
                unallowed_imports: Vec::new(),
                unallowed_import_sites: Vec::new(),
                used_upward_imports: BTreeSet::from(["cli".to_owned()]),
                config_line: 2,
            }],
            regressions: 0,
            parse_failures: 0,
        };

        tighten(&mut target, &report);

        assert_eq!(target.modules[0].surface_budget, 3);
        assert_eq!(
            target.modules[0].upward_imports.as_deref(),
            Some(&["cli".to_owned()][..])
        );
    }

    #[test]
    fn configured_target_ratchets_and_tightens_without_subprocesses() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        fs::create_dir(root.path().join("src/nested")).unwrap();
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = []\n[package]\nname = \"probe\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/lib.rs"),
            "use probe::other::Thing;\npub fn run() -> Thing { Thing }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/other.rs"),
            "pub struct Thing;\nfn caller() { let _ = crate::run(); }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/nested/mod.rs"),
            "use probe::other::Thing;\npub fn nested() -> Thing { Thing }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("src/tests.rs"),
            "fn characterization() { crate::run(); crate::run(); }\n",
        )
        .unwrap();
        fs::write(
            root.path().join(TARGET_FILE),
            r#"
version = 3
layers = []
[[module]]
path = "src/nested"
allowed-imports = ["other"]
surface-budget = 5
[[strangler]]
symbol = "run"
path = "src/lib.rs"
baseline = 5
[[strangler]]
symbol = "run"
path = "src"
baseline = 5
"#,
        )
        .unwrap();

        ratchet(root.path()).unwrap();
        let target_path = root.path().join(TARGET_FILE);
        let mut configured = target::load(&target_path).unwrap().unwrap();
        let mut forbidden = configured.clone();
        forbidden.modules[0]
            .allowed_imports
            .as_mut()
            .unwrap()
            .clear();
        let forbidden_report = evaluate(root.path(), &forbidden, &target_path, true).unwrap();
        assert_eq!(forbidden_report.regressions, 1);
        assert_eq!(forbidden_report.rules[0].unallowed_imports, ["other"]);
        assert_eq!(
            forbidden_report.rules[0].unallowed_import_sites,
            [ImportSite {
                module: "other".to_owned(),
                path: PathBuf::from("src/nested/mod.rs"),
                line: 1,
            }]
        );
        assert!(enforce(&forbidden_report).is_err());

        let report = evaluate(root.path(), &configured, &target_path, true).unwrap();
        tighten(&mut configured, &report);
        target::write(&target_path, &configured).unwrap();
        let tightened = target::load(&target_path).unwrap().unwrap();
        assert_eq!(tightened.modules[0].surface_budget, 1);
        assert_eq!(tightened.strangler[0].baseline, 1);
        assert_eq!(tightened.strangler[1].baseline, 2);

        let target_directory = tempfile::tempdir().unwrap();
        let initialized_path = target_directory.path().join("initialized.toml");
        let initialized_arg = initialized_path.display().to_string();
        run(
            root.path(),
            &[
                "--init".into(),
                "--path".into(),
                "src".into(),
                "--file".into(),
                initialized_arg.clone(),
            ],
        )
        .unwrap();
        let initialized = target::load(&initialized_path).unwrap().unwrap();
        let initialized_report =
            evaluate(root.path(), &initialized, &initialized_path, false).unwrap();
        assert_eq!(initialized_report.regressions, 0);
        assert_eq!(initialized.modules.len(), 3);
        assert!(initialized.modules[0].allowed_imports.is_none());
        assert_eq!(initialized.modules[1].path, Path::new("src/nested"));
        assert!(initialized.modules[1].upward_imports.is_none());
        let before_tighten = fs::read_to_string(&initialized_path).unwrap();
        run(
            root.path(),
            &["--tighten".into(), "--file".into(), initialized_arg.clone()],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&initialized_path).unwrap(),
            before_tighten
        );
        assert!(
            run(
                root.path(),
                &["--init".into(), "--file".into(), initialized_arg],
            )
            .is_err()
        );
    }

    #[test]
    fn strangler_paths_do_not_count_matching_modules_from_other_crates() {
        let root = tempfile::tempdir().unwrap();
        for path in ["app/src/legacy", "tool/src/legacy"] {
            fs::create_dir_all(root.path().join(path)).unwrap();
        }
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\", \"tool\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        for member in ["app", "tool"] {
            fs::write(
                root.path().join(member).join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{member}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n"
                ),
            )
            .unwrap();
        }
        fs::write(root.path().join("app/src/lib.rs"), "pub mod legacy;\n").unwrap();
        fs::write(
            root.path().join("app/src/legacy/mod.rs"),
            "fn doomed() {}\n",
        )
        .unwrap();
        fs::write(
            root.path().join("app/src/legacy/caller.rs"),
            "fn call() { doomed(); }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("tool/src/lib.rs"),
            "fn doomed() { doomed(); doomed(); }\n",
        )
        .unwrap();
        fs::write(
            root.path().join("tool/src/legacy/mod.rs"),
            "fn doomed() { doomed(); }\n",
        )
        .unwrap();

        let target = Target {
            version: 3,
            layers: Vec::new(),
            modules: Vec::new(),
            strangler: vec![
                target::StranglerRule {
                    symbol: "doomed".to_owned(),
                    path: PathBuf::from("app/src"),
                    baseline: 10,
                    config_line: 1,
                },
                target::StranglerRule {
                    symbol: "doomed".to_owned(),
                    path: PathBuf::from("app/src/legacy"),
                    baseline: 10,
                    config_line: 2,
                },
                target::StranglerRule {
                    symbol: "doomed".to_owned(),
                    path: PathBuf::from("app/src/lib.rs"),
                    baseline: 10,
                    config_line: 3,
                },
            ],
        };

        let report = evaluate(
            root.path(),
            &target,
            &root.path().join("custom-target.toml"),
            false,
        )
        .unwrap();
        assert_eq!(
            report
                .rules
                .iter()
                .map(|rule| rule.current)
                .collect::<Vec<_>>(),
            [2, 2, 0]
        );
    }

    #[test]
    fn missing_explicit_target_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let error = run(
            root.path(),
            &["--file".into(), "missing-target.toml".into()],
        )
        .unwrap_err();
        assert!(error.to_string().contains("missing-target.toml"));
    }
}
