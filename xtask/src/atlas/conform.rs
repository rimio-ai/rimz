use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::api::OccurrenceCorpus;
use super::modules::{crate_module_for_path, path_in_scope, workspace_crate_names};
use super::sources::{self, Source};
use super::syntax;
use super::target::{self, TARGET_FILE, Target};

const USAGE: &str = "cargo xtask atlas conform [--ratchet|--tighten] [--json]

Compares the working tree with root refactor-target.toml. `--ratchet` fails only
when current values exceed budgets/baselines or an import is outside its allow
list. `--tighten` atomically lowers budgets/baselines to current values; it
never raises them. A strangler counts whole-word occurrences of its symbol in
non-test Rust under its path (a file or directory). A missing target file passes.

  --ratchet  fail on regressions (the checks/gate mode)
  --tighten  lower budgets and baselines to current values
  --json     versioned JSON agent contract (v1)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Report,
    Ratchet,
    Tighten,
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
    config_line: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    target: &'static str,
    rules: Vec<RuleResult>,
    regressions: usize,
    parse_failures: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas conform output is a command stdout contract"
)]
pub(super) fn run(root: &Path, args: &[String]) -> Result<()> {
    let Some((mode, json)) = parse_args(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let Some(mut target) = target::load(root)? else {
        if mode != Mode::Ratchet {
            if json {
                println!(
                    "{{\n  \"version\": 1,\n  \"verb\": \"conform\",\n  \"target\": \"{TARGET_FILE}\",\n  \"configured\": false\n}}"
                );
            } else {
                println!("Atlas conform — no {TARGET_FILE}; nothing to check");
            }
        }
        return Ok(());
    };
    let report = evaluate(root, &target)?;
    match mode {
        Mode::Report => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .context("rendering atlas conform JSON")?
                );
            } else {
                print_report(&report);
            }
            Ok(())
        }
        Mode::Ratchet => enforce(&report),
        Mode::Tighten => {
            tighten(&mut target, &report);
            target::write(root, &target)?;
            if !json {
                println!("tightened {TARGET_FILE}");
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .context("rendering atlas conform JSON")?
                );
            }
            Ok(())
        }
    }
}

pub(super) fn ratchet(root: &Path) -> Result<()> {
    let Some(target) = target::load(root)? else {
        return Ok(());
    };
    enforce(&evaluate(root, &target)?)
}

fn parse_args(args: &[String]) -> Result<Option<(Mode, bool)>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut mode = Mode::Report;
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--ratchet" if mode == Mode::Report => mode = Mode::Ratchet,
            "--tighten" if mode == Mode::Report => mode = Mode::Tighten,
            "--ratchet" | "--tighten" => {
                bail!("atlas conform --ratchet and --tighten are mutually exclusive")
            }
            "--json" if !json => json = true,
            "--json" => bail!("atlas conform --json may only be passed once"),
            _ => bail!("unknown atlas conform argument `{arg}`"),
        }
    }
    if mode == Mode::Ratchet && json {
        bail!("atlas conform --ratchet does not combine with --json");
    }
    Ok(Some((mode, json)))
}

fn evaluate(root: &Path, target: &Target) -> Result<Report> {
    let all_sources = sources::working_tree_rust_sources(root)?;
    let known_modules = all_sources
        .iter()
        .map(|source| crate_module_for_path(&source.path))
        .collect::<BTreeSet<_>>();
    let workspace_crates = workspace_crate_names(root)?;
    let occurrence_corpus = OccurrenceCorpus::new(&all_sources);
    let mut rules = Vec::new();
    let mut parse_failures = 0;
    for module in &target.modules {
        let absolute = root.join(&module.path);
        if !absolute.exists() {
            bail!(
                "{TARGET_FILE}:{}: configured module path `{}` does not exist",
                module.config_line,
                module.path.display()
            );
        }
        let module_sources = sources_for_path(&all_sources, &module.path, absolute.is_file());
        let parsed = syntax::analyze_sources(&module_sources);
        parse_failures += parsed.parse_failures.len();
        let current = parsed
            .files
            .iter()
            .map(|file| file.pub_items.len())
            .sum::<usize>();
        let module_entry = if absolute.is_dir() {
            module.path.join("mod.rs")
        } else {
            module.path.clone()
        };
        let target_module = crate_module_for_path(&module_entry);
        let mut unallowed_imports = parsed
            .files
            .iter()
            .flat_map(|file| &file.imports)
            .filter_map(|import| {
                syntax::resolved_internal_import(import, &known_modules, &workspace_crates)
            })
            .filter(|import| !is_within(import, &target_module))
            .filter(|import| {
                !module
                    .allowed_imports
                    .iter()
                    .any(|allowed| is_within(import, allowed))
            })
            .collect::<Vec<_>>();
        unallowed_imports.sort();
        unallowed_imports.dedup();
        let regression = current > module.pub_budget || !unallowed_imports.is_empty();
        rules.push(RuleResult {
            kind: "module",
            path: module.path.clone(),
            symbol: None,
            status: if regression { "regression" } else { "ok" },
            current,
            budget: module.pub_budget,
            delta: current as isize - module.pub_budget as isize,
            unallowed_imports,
            config_line: module.config_line,
        });
    }
    for strangler in &target.strangler {
        let absolute = root.join(&strangler.path);
        if !absolute.exists() {
            bail!(
                "{TARGET_FILE}:{}: configured strangler path `{}` does not exist",
                strangler.config_line,
                strangler.path.display()
            );
        }
        let scope_entry = if absolute.is_dir() {
            strangler.path.join("mod.rs")
        } else {
            strangler.path.clone()
        };
        let scope_module = crate_module_for_path(&scope_entry);
        let current = if absolute.is_dir() {
            occurrence_corpus.count_under(&scope_module, &strangler.symbol)
        } else {
            occurrence_corpus.count_in_module(&scope_module, &strangler.symbol)
        };
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
            config_line: strangler.config_line,
        });
    }
    Ok(Report {
        version: 1,
        verb: "conform",
        target: TARGET_FILE,
        regressions: rules
            .iter()
            .filter(|rule| rule.status == "regression")
            .count(),
        rules,
        parse_failures,
    })
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

fn is_within(path: &str, allowed: &str) -> bool {
    path == allowed || path.starts_with(&format!("{allowed}::"))
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
                "{TARGET_FILE}:{}: {} `{}` is {} above {}",
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
            violations.push(format!(
                "{TARGET_FILE}:{}: module `{}` imports outside its allow list: {}",
                rule.config_line,
                rule.path.display(),
                rule.unallowed_imports.join(", ")
            ));
        }
    }
    bail!(
        "atlas conform ratchet regressed:\n{}\n\nReduce the current values, run `cargo xtask atlas conform --tighten` after improvement, or deliberately edit {TARGET_FILE} to reopen a budget.",
        violations.join("\n")
    )
}

fn tighten(target: &mut Target, report: &Report) {
    let mut results = report.rules.iter();
    for module in &mut target.modules {
        if let Some(result) = results.next() {
            module.pub_budget = module.pub_budget.min(result.current);
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
fn print_report(report: &Report) {
    println!("Atlas conform — {}", report.target);
    println!("status      kind        current  budget      Δ  rule");
    for rule in &report.rules {
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
        }
    }
    println!(
        "summary: {} rules, {} regressions, {} parse failures",
        report.rules.len(),
        report.regressions,
        report.parse_failures
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn conform_args_separate_report_ratchet_and_tighten() {
        assert_eq!(parse_args(&[]).unwrap(), Some((Mode::Report, false)));
        assert_eq!(
            parse_args(&["--ratchet".into()]).unwrap(),
            Some((Mode::Ratchet, false))
        );
        assert!(parse_args(&["--ratchet".into(), "--tighten".into()]).is_err());
    }

    #[test]
    fn allow_list_matches_descendants_not_prefix_collisions() {
        assert!(is_within("cli::render::table", "cli::render"));
        assert!(!is_within("cli::renderer", "cli::render"));
    }

    #[test]
    fn configured_target_ratchets_and_tightens_without_subprocesses() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
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
            root.path().join("src/tests.rs"),
            "fn characterization() { crate::run(); crate::run(); }\n",
        )
        .unwrap();
        fs::write(
            root.path().join(TARGET_FILE),
            r#"
version = 1
[[module]]
path = "src/lib.rs"
allowed-imports = ["other"]
pub-budget = 5
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
        let mut configured = target::load(root.path()).unwrap().unwrap();
        let mut forbidden = configured.clone();
        forbidden.modules[0].allowed_imports.clear();
        let forbidden_report = evaluate(root.path(), &forbidden).unwrap();
        assert_eq!(forbidden_report.regressions, 1);
        assert_eq!(forbidden_report.rules[0].unallowed_imports, ["other"]);
        assert!(enforce(&forbidden_report).is_err());

        let report = evaluate(root.path(), &configured).unwrap();
        tighten(&mut configured, &report);
        target::write(root.path(), &configured).unwrap();
        let tightened = target::load(root.path()).unwrap().unwrap();
        assert_eq!(tightened.modules[0].pub_budget, 1);
        assert_eq!(tightened.strangler[0].baseline, 1);
        assert_eq!(tightened.strangler[1].baseline, 2);
    }
}
