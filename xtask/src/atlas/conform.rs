use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::facts::{Facets, Facts};
use super::modules::{crate_module_for_path, module_is_within, path_in_scope};
use super::sources::Source;
use super::syntax::{self, Spelling};
use super::target::{self, LayerRanks, TARGET_FILE, Target};

const USAGE: &str = "cargo xtask atlas conform [--ratchet|--tighten]

Compares the working tree with root refactor-target.toml. `--ratchet` fails when
current values exceed budgets or a dependency is outside its admission list.
`--tighten` atomically lowers budgets and baselines to current values and removes
unused dependency admissions; it never raises them. A missing target passes.

  --ratchet  fail on regressions (the checks/gate mode)
  --tighten  lower budgets/baselines and remove unused admissions";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Report,
    Ratchet,
    Tighten,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ImportSite {
    module: String,
    path: PathBuf,
    line: usize,
    spelling: Spelling,
}

#[derive(Clone, Debug)]
struct RuleResult {
    path: PathBuf,
    symbol: Option<String>,
    current: usize,
    budget: usize,
    unallowed_dependencies: Vec<String>,
    unallowed_dependency_sites: Vec<ImportSite>,
    used_dependencies: BTreeSet<String>,
    config_line: Option<usize>,
    fix: Option<String>,
}

impl RuleResult {
    fn regression(&self) -> bool {
        self.current > self.budget || !self.unallowed_dependencies.is_empty()
    }

    fn measure(&self) -> &'static str {
        if self.symbol.is_some() {
            "strangler"
        } else {
            "surface"
        }
    }
}

#[derive(Debug)]
struct Report {
    target: PathBuf,
    layers: Vec<Vec<String>>,
    rules: Vec<RuleResult>,
    parse_failure_paths: Vec<PathBuf>,
}

impl Report {
    fn regressions(&self) -> usize {
        self.rules.iter().filter(|rule| rule.regression()).count()
    }
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
    let target_path = root.join(TARGET_FILE);
    let Some(mut target) = target::load(&target_path)? else {
        if args.mode == Mode::Report {
            println!("Atlas conform — no {TARGET_FILE}; nothing to check");
        }
        return Ok(());
    };
    let report = evaluate(root, &target, &target_path)?;
    match args.mode {
        Mode::Report => {
            print_report(&report);
            Ok(())
        }
        Mode::Ratchet => enforce(&report),
        Mode::Tighten => {
            if !report.parse_failure_paths.is_empty() {
                bail!(
                    "atlas conform --tighten cannot measure configured rules while these files do not parse:\n  {}\n\nRepair the {}, then tighten.",
                    report
                        .parse_failure_paths
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n  "),
                    if report.parse_failure_paths.len() == 1 {
                        "file"
                    } else {
                        "files"
                    }
                );
            }
            tighten(&mut target, &report);
            target::write(&target_path, &target)?;
            println!("tightened {}", target_path.display());
            Ok(())
        }
    }
}

pub(super) fn ratchet(root: &Path) -> Result<()> {
    let target_path = root.join(TARGET_FILE);
    let Some(target) = target::load(&target_path)? else {
        return Ok(());
    };
    enforce(&evaluate(root, &target, &target_path)?)
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mode = match args {
        [] => Mode::Report,
        [arg] if arg == "--ratchet" => Mode::Ratchet,
        [arg] if arg == "--tighten" => Mode::Tighten,
        [arg] => bail!("unknown atlas conform argument `{arg}`"),
        _ => bail!("atlas conform --ratchet and --tighten are mutually exclusive"),
    };
    Ok(Some(Args { mode }))
}

pub(super) fn top_module(module: &str) -> &str {
    module
        .split("::")
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or("(crate)")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Upward,
    Same,
    Downward,
}

pub(super) fn layer_direction(
    ranks: &LayerRanks,
    from_module: &str,
    to_module: &str,
) -> Option<Direction> {
    let from = ranks.get(top_module(from_module))?;
    let to = ranks.get(top_module(to_module))?;
    Some(match to.cmp(&from) {
        std::cmp::Ordering::Greater => Direction::Upward,
        std::cmp::Ordering::Equal => Direction::Same,
        std::cmp::Ordering::Less => Direction::Downward,
    })
}

pub(super) fn rule_covers_path(root: &Path, rule_path: &Path, path: &Path) -> bool {
    if root.join(rule_path).is_file() {
        return path == rule_path;
    }
    path_in_scope(path, rule_path) || path == rule_path.with_extension("rs")
}

fn evaluate(root: &Path, target: &Target, target_path: &Path) -> Result<Report> {
    let facts = Facts::load(root, Path::new("."), Facets::default())?;
    evaluate_with_facts(root, target, target_path, &facts)
}

fn evaluate_with_facts(
    root: &Path,
    target: &Target,
    target_path: &Path,
    facts: &Facts,
) -> Result<Report> {
    let layer_ranks = target.layer_ranks();
    let mut rules = Vec::new();
    let mut parse_failure_paths = BTreeSet::new();
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
            .filter(|file| rule_covers_path(root, &module.path, &file.path))
            .collect::<Vec<_>>();
        parse_failure_paths.extend(
            facts
                .syntax
                .parse_failures
                .iter()
                .filter(|path| rule_covers_path(root, &module.path, path))
                .cloned(),
        );
        let module_entry = if absolute.is_dir() {
            module.path.join("mod.rs")
        } else {
            module.path.clone()
        };
        let target_module = crate_module_for_path(&module_entry);
        let current = super::modules::escaping_items_for_boundary(
            &module_files,
            &target_module,
            &facts.mod_index,
        )
        .len();
        let mut unallowed = BTreeMap::<String, BTreeSet<(PathBuf, usize, Spelling)>>::new();
        let mut used_dependencies = BTreeSet::new();
        for file in module_files {
            for dependency in &file.dependencies {
                let Some(resolved) = syntax::resolved_internal_import(
                    dependency,
                    &facts.known_modules,
                    &facts.crate_names,
                ) else {
                    continue;
                };
                if module_is_within(&resolved, &target_module) {
                    continue;
                }
                let admissions = if let Some(allowed) = &module.allowed_dependencies {
                    Some(allowed.as_slice())
                } else if layer_direction(&layer_ranks, &file.module_path, &resolved)
                    == Some(Direction::Upward)
                {
                    Some(module.upward_dependencies.as_deref().unwrap_or_default())
                } else {
                    None
                };
                let Some(admissions) = admissions else {
                    continue;
                };
                let matched = admissions
                    .iter()
                    .filter(|allowed| module_is_within(&resolved, allowed))
                    .collect::<Vec<_>>();
                if !matched.is_empty() {
                    used_dependencies.extend(matched.into_iter().cloned());
                    continue;
                }
                unallowed.entry(resolved).or_default().insert((
                    file.path.clone(),
                    dependency.line,
                    dependency.spelling,
                ));
            }
        }
        let unallowed_dependencies = unallowed.keys().cloned().collect::<Vec<_>>();
        let unallowed_dependency_sites = dependency_sites(unallowed);
        let fix =
            (current > module.surface_budget || !unallowed_dependencies.is_empty()).then(|| {
                let mut rule = module.clone();
                rule.surface_budget = current.max(rule.surface_budget);
                if !unallowed_dependencies.is_empty() {
                    let dependencies = if let Some(dependencies) = &mut rule.allowed_dependencies {
                        dependencies
                    } else {
                        rule.upward_dependencies.get_or_insert_with(Vec::new)
                    };
                    dependencies.extend(unallowed_dependencies.iter().cloned());
                    dependencies.sort();
                    dependencies.dedup();
                }
                target::render_module_rule(&rule)
            });
        rules.push(RuleResult {
            path: module.path.clone(),
            symbol: None,
            current,
            budget: module.surface_budget,
            unallowed_dependencies,
            unallowed_dependency_sites,
            used_dependencies,
            config_line: Some(module.config_line),
            fix,
        });
    }
    for file in facts.syntax.files.iter().filter(|file| {
        !target
            .modules
            .iter()
            .any(|module| rule_covers_path(root, &module.path, &file.path))
    }) {
        if layer_ranks.get(top_module(&file.module_path)).is_none() {
            continue;
        }
        let mut unallowed = BTreeMap::<String, BTreeSet<(PathBuf, usize, Spelling)>>::new();
        for dependency in &file.dependencies {
            let Some(resolved) = syntax::resolved_internal_import(
                dependency,
                &facts.known_modules,
                &facts.crate_names,
            ) else {
                continue;
            };
            if layer_direction(&layer_ranks, &file.module_path, &resolved)
                != Some(Direction::Upward)
            {
                continue;
            }
            unallowed.entry(resolved).or_default().insert((
                file.path.clone(),
                dependency.line,
                dependency.spelling,
            ));
        }
        if unallowed.is_empty() {
            continue;
        }
        let unallowed_dependencies = unallowed.keys().cloned().collect::<Vec<_>>();
        let module = crate_module_for_path(&file.path);
        let surface_budget =
            super::modules::escaping_items_for_boundary(&[file], &module, &facts.mod_index).len();
        let fix = target::render_module_rule(&target::ModuleRule {
            path: file.path.clone(),
            allowed_dependencies: None,
            upward_dependencies: Some(unallowed_dependencies.clone()),
            surface_budget,
            config_line: 0,
        });
        rules.push(RuleResult {
            path: file.path.clone(),
            symbol: None,
            current: 0,
            budget: 0,
            unallowed_dependencies,
            unallowed_dependency_sites: dependency_sites(unallowed),
            used_dependencies: BTreeSet::new(),
            config_line: None,
            fix: Some(fix),
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
        parse_failure_paths.extend(
            facts
                .syntax
                .parse_failures
                .iter()
                .filter(|path| rule_covers_path(root, &strangler.path, path))
                .cloned(),
        );
        let current = count_in_sources(&scoped_sources, &facts.syntax.files, &strangler.symbol);
        let fix = (current > strangler.baseline).then(|| {
            let mut rule = strangler.clone();
            rule.baseline = current;
            target::render_strangler_rule(&rule)
        });
        rules.push(RuleResult {
            path: strangler.path.clone(),
            symbol: Some(strangler.symbol.clone()),
            current,
            budget: strangler.baseline,
            unallowed_dependencies: Vec::new(),
            unallowed_dependency_sites: Vec::new(),
            used_dependencies: BTreeSet::new(),
            config_line: Some(strangler.config_line),
            fix,
        });
    }
    Ok(Report {
        target: target_path.to_path_buf(),
        layers: target.layers.clone(),
        rules,
        parse_failure_paths: parse_failure_paths.into_iter().collect(),
    })
}

fn dependency_sites(
    unallowed: BTreeMap<String, BTreeSet<(PathBuf, usize, Spelling)>>,
) -> Vec<ImportSite> {
    unallowed
        .into_iter()
        .flat_map(|(module, sites)| {
            sites
                .into_iter()
                .map(move |(path, line, spelling)| ImportSite {
                    module: module.clone(),
                    path,
                    line,
                    spelling,
                })
        })
        .collect()
}

pub(super) fn sources_for_path(sources: &[Source], path: &Path, is_file: bool) -> Vec<Source> {
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

pub(super) fn count_in_sources(
    sources: &[Source],
    syntax_files: &[syntax::FileSyntax],
    symbol: &str,
) -> usize {
    sources
        .iter()
        .filter(|source| source.is_production())
        .map(|source| {
            let file_syntax = syntax_files.iter().find(|file| file.path == source.path);
            source
                .text
                .split_inclusive('\n')
                .enumerate()
                .filter(|(index, _)| {
                    file_syntax.is_none_or(|file| file.cfg_kind_at(index + 1).is_none())
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
    if report.regressions() == 0 {
        return Ok(());
    }
    let mut violations = Vec::new();
    for rule in report.rules.iter().filter(|rule| rule.regression()) {
        let location = rule.config_line.map_or_else(
            || report.target.display().to_string(),
            |line| format!("{}:{line}", report.target.display()),
        );
        if rule.current > rule.budget {
            violations.push(if let Some(symbol) = &rule.symbol {
                format!(
                    "{location}: strangler `{symbol}` in `{}` is {} above {}",
                    rule.path.display(),
                    rule.current,
                    rule.budget
                )
            } else {
                format!(
                    "{location}: surface of `{}` is {} above {}",
                    rule.path.display(),
                    rule.current,
                    rule.budget
                )
            });
        }
        if !rule.unallowed_dependencies.is_empty() {
            let sites = rule
                .unallowed_dependency_sites
                .iter()
                .take(20)
                .map(render_site)
                .collect::<Vec<_>>()
                .join(", ");
            violations.push(format!(
                "{location}: module `{}` has dependencies outside its admissions: {}\n  source: {}",
                rule.path.display(),
                rule.unallowed_dependencies.join(", "),
                sites,
            ));
        }
        if let Some(fix) = &rule.fix {
            let action = rule.config_line.map_or_else(
                || format!("  fix: add to {}", report.target.display()),
                |line| {
                    format!(
                        "  fix: replace the rule at {}:{line} with",
                        report.target.display()
                    )
                },
            );
            let block = fix
                .lines()
                .map(str::to_owned)
                .collect::<Vec<_>>()
                .join("\n");
            violations.push(format!("{action}\n{block}"));
        }
    }
    bail!(
        "atlas conform ratchet regressed:\n{}\n\nReduce the current values, run `cargo xtask atlas conform --tighten` after improvement, or paste a fix above to reopen its budget deliberately.",
        violations.join("\n")
    )
}

fn render_site(site: &ImportSite) -> String {
    format!(
        "{}:{}{} ({})",
        site.path.display(),
        site.line,
        if site.spelling == Spelling::Qualified {
            " (qualified)"
        } else {
            ""
        },
        site.module
    )
}

fn tighten(target: &mut Target, report: &Report) {
    for module in &mut target.modules {
        let Some(result) = report
            .rules
            .iter()
            .find(|result| result.symbol.is_none() && result.path == module.path)
        else {
            continue;
        };
        module.surface_budget = module.surface_budget.min(result.current);
        if let Some(dependencies) = &mut module.allowed_dependencies {
            dependencies.retain(|dependency| result.used_dependencies.contains(dependency));
        }
        if let Some(dependencies) = &mut module.upward_dependencies {
            dependencies.retain(|dependency| result.used_dependencies.contains(dependency));
            if dependencies.is_empty() {
                module.upward_dependencies = None;
            }
        }
    }
    for strangler in &mut target.strangler {
        if let Some(result) = report.rules.iter().find(|result| {
            result.symbol.as_deref() == Some(&strangler.symbol) && result.path == strangler.path
        }) {
            strangler.baseline = strangler.baseline.min(result.current);
        }
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas conform report is the command's stdout contract"
)]
fn print_report(report: &Report) {
    println!("Atlas conform — {}", report.target.display());
    if !report.layers.is_empty() {
        println!(
            "layers: {}",
            report
                .layers
                .iter()
                .map(|layer| layer.join(" + "))
                .collect::<Vec<_>>()
                .join(" < ")
        );
    }
    println!("\nViolations");
    let mut violations = Vec::new();
    for rule in report.rules.iter().filter(|rule| rule.regression()) {
        if rule.current > rule.budget {
            violations.push(format!(
                "{} {}: {} / {}",
                rule.measure(),
                rule.symbol
                    .as_deref()
                    .unwrap_or_else(|| rule.path.to_str().unwrap_or("?")),
                rule.current,
                rule.budget
            ));
        }
        violations.extend(rule.unallowed_dependency_sites.iter().map(render_site));
    }
    if violations.is_empty() {
        println!("none");
    } else {
        for site in violations.iter().take(20) {
            println!("{site}");
        }
        println!("{} violations total", violations.len());
    }
    println!("\nHeadroom");
    let mut headroom = report
        .rules
        .iter()
        .filter(|rule| !rule.regression())
        .collect::<Vec<_>>();
    headroom.sort_by_key(|rule| rule.budget - rule.current);
    for rule in headroom.into_iter().take(20) {
        println!(
            "{}: {} / {}",
            rule.symbol
                .as_deref()
                .unwrap_or_else(|| rule.path.to_str().unwrap_or("?")),
            rule.current,
            rule.budget
        );
    }
    println!(
        "\nsummary: {} rules, {} regressions, {} parse failures",
        report.rules.len(),
        report.regressions(),
        report.parse_failure_paths.len()
    );
}

#[cfg(test)]
#[path = "conform/tests.rs"]
mod tests;
