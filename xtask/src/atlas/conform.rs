use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::facts::{Facets, Facts};
use super::modules::Boundary;
use super::syntax::Spelling;
use super::target::{self, TARGET_FILE, Target};

const USAGE: &str = "cargo xtask atlas conform [--ratchet|--tighten] [--only <path>]...

Compares the working tree with root refactor-target.toml. `--ratchet` fails when
current values exceed budgets or a dependency is outside its admission list.
`--tighten` lowers budgets and baselines in place and removes unused dependency
admissions; it never raises them. Comments and untouched rules are preserved.
A missing target passes; --only with no target fails.

  --ratchet  fail on regressions (the checks/gate mode)
  --tighten  lower budgets/baselines and remove unused admissions
  --only <path>  with --tighten: only the rules at this path (repeatable)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Report,
    Ratchet,
    Tighten,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
    only: BTreeSet<PathBuf>,
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
        if !args.only.is_empty() {
            bail!("atlas conform --only requires {TARGET_FILE}; restore it, then tighten");
        }
        if args.mode == Mode::Report {
            println!("Atlas conform — no {TARGET_FILE}; nothing to check");
        }
        return Ok(());
    };
    let unknown: Vec<_> = args
        .only
        .iter()
        .filter(|path| {
            !target.modules.iter().any(|rule| rule.path == **path)
                && !target.strangler.iter().any(|rule| rule.path == **path)
        })
        .map(|path| path.display().to_string())
        .collect();
    if !unknown.is_empty() {
        bail!(
            "atlas conform --only names no rule in {TARGET_FILE}:\n  {}",
            unknown.join("\n  ")
        );
    }
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
            tighten(&mut target, &report, &args.only);
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
    let Some(mut cursor) = super::ArgCursor::new(args, "conform") else {
        return Ok(None);
    };
    let mut mode = Mode::Report;
    let mut only = BTreeSet::new();
    while let Some(flag) = cursor.next() {
        match flag {
            "--ratchet" | "--tighten" => {
                if mode != Mode::Report {
                    bail!("atlas conform --ratchet and --tighten are mutually exclusive");
                }
                mode = if flag == "--ratchet" {
                    Mode::Ratchet
                } else {
                    Mode::Tighten
                };
            }
            "--only" => {
                only.insert(super::validate_scope(cursor.value_of(flag)?, flag)?);
            }
            arg => bail!("unknown atlas conform argument `{arg}`"),
        }
    }
    if !only.is_empty() && mode != Mode::Tighten {
        bail!("atlas conform --only requires --tighten");
    }
    Ok(Some(Args { mode, only }))
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
    for (path, line, kind) in target
        .modules
        .iter()
        .map(|rule| (&rule.path, rule.config_line, "module"))
        .chain(
            target
                .strangler
                .iter()
                .map(|rule| (&rule.path, rule.config_line, "strangler")),
        )
    {
        if !root.join(path).exists() {
            bail!(
                "{}:{line}: configured {kind} path `{}` does not exist",
                target_path.display(),
                path.display()
            );
        }
    }
    let measurement = target::measure(target, facts);
    let mut rules = Vec::new();
    let mut parse_failure_paths = BTreeSet::new();
    for (module, measured) in target
        .modules
        .iter()
        .map(Some)
        .zip(&measurement.rules)
        .chain(
            measurement
                .uncovered
                .iter()
                .map(|measured| (None, measured)),
        )
    {
        parse_failure_paths.extend(measured.parse_failures.iter().cloned());
        let current = module.map_or(0, |_| measured.esc);
        let budget = module.map_or(0, |module| module.surface_budget);
        let mut unallowed = BTreeMap::<String, BTreeSet<(PathBuf, usize, Spelling)>>::new();
        let mut used_dependencies = BTreeSet::new();
        for site in &measured.sites {
            match &site.admission {
                target::Admission::Admitted(prefixes) => {
                    used_dependencies.extend(prefixes.iter().cloned())
                }
                target::Admission::Unadmitted => {
                    unallowed.entry(site.to.to_owned()).or_default().insert((
                        site.file.path.clone(),
                        site.dependency.line,
                        site.dependency.spelling,
                    ));
                }
                target::Admission::Ungoverned => {}
            }
        }
        let unallowed_dependencies = unallowed.keys().cloned().collect::<Vec<_>>();
        let fix = (current > budget || !unallowed_dependencies.is_empty()).then(|| {
            let mut rule = module.cloned().unwrap_or_else(|| target::ModuleRule {
                path: measured.boundary.path().to_path_buf(),
                allowed_dependencies: None,
                upward_dependencies: Some(Vec::new()),
                surface_budget: measured.esc,
                config_line: 0,
            });
            rule.surface_budget = measured.esc.max(rule.surface_budget);
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
            path: measured.boundary.path().to_path_buf(),
            symbol: None,
            current,
            budget,
            unallowed_dependencies,
            unallowed_dependency_sites: dependency_sites(unallowed),
            used_dependencies,
            config_line: module.map(|module| module.config_line),
            fix,
        });
    }
    for strangler in &target.strangler {
        let boundary = Boundary::new(&strangler.path);
        let scoped_sources = target::sources_for_path(&facts.sources, &boundary);
        parse_failure_paths.extend(
            facts
                .syntax
                .parse_failures
                .iter()
                .filter(|path| boundary.covers(path))
                .cloned(),
        );
        let current =
            target::count_in_sources(&scoped_sources, &facts.syntax.files, &strangler.symbol);
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

fn tighten(target: &mut Target, report: &Report, only: &BTreeSet<PathBuf>) {
    for module in &mut target.modules {
        if !only.is_empty() && !only.contains(&module.path) {
            continue;
        }
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
        if !only.is_empty() && !only.contains(&strangler.path) {
            continue;
        }
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
