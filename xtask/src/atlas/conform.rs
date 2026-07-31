use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::api::OccurrenceCorpus;
use super::modules::{crate_module_for_path, path_in_scope, workspace_crate_names};
use super::sources::{self, Source};
use super::syntax;
use super::target::{self, TARGET_FILE, Target};
use super::{set_once, validate_scope, value};

const USAGE: &str = "cargo xtask atlas conform [--ratchet|--tighten] [--file <path>] [--json]

Compares the working tree with a refactor target (root refactor-target.toml by
default). `--ratchet` fails only when current values exceed budgets/baselines or
an import is outside its allow list. `--tighten` atomically lowers budgets and
baselines to current values; it never raises them. A strangler counts whole-word
occurrences of its symbol in non-test Rust under its path (a file or directory).
A missing default target passes; a missing explicit --file is an error.

  --ratchet      fail on regressions (the checks/gate mode)
  --tighten      lower budgets and baselines to current values
  --file <path>  root-relative target file (default refactor-target.toml)
  --json         versioned JSON agent contract (v1)";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Report,
    Ratchet,
    Tighten,
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
    file: Option<PathBuf>,
    json: bool,
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
    target: PathBuf,
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
    let target_file = args
        .file
        .as_deref()
        .unwrap_or_else(|| Path::new(TARGET_FILE));
    let target_path = root.join(target_file);
    let Some(mut target) = target::load(&target_path)? else {
        if args.file.is_some() {
            bail!(
                "atlas conform target file `{}` does not exist",
                target_file.display()
            );
        }
        if args.mode != Mode::Ratchet {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "version": 1,
                        "verb": "conform",
                        "target": target_file,
                        "configured": false,
                    }))
                    .context("rendering unconfigured atlas conform JSON")?
                );
            } else {
                println!("Atlas conform — no {TARGET_FILE}; nothing to check");
            }
        }
        return Ok(());
    };
    let report = evaluate(root, &target, target_file)?;
    match args.mode {
        Mode::Report => {
            if args.json {
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
            target::write(&target_path, &target)?;
            if !args.json {
                println!("tightened {}", target_file.display());
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
    let target_path = root.join(TARGET_FILE);
    let Some(target) = target::load(&target_path)? else {
        return Ok(());
    };
    enforce(&evaluate(root, &target, Path::new(TARGET_FILE))?)
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut mode = Mode::Report;
    let mut file = None;
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
            "--ratchet" | "--tighten" => {
                bail!("atlas conform --ratchet and --tighten are mutually exclusive")
            }
            "--file" => {
                let parsed = validate_scope(value(args, index, "conform", "--file")?, "--file")?;
                set_once(&mut file, parsed, "conform", "--file")?;
                index += 2;
            }
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
    Ok(Some(Args { mode, file, json }))
}

fn evaluate(root: &Path, target: &Target, target_path: &Path) -> Result<Report> {
    let all_sources = sources::working_tree_rust_sources(root)?;
    let known_modules = all_sources
        .iter()
        .map(|source| crate_module_for_path(&source.path))
        .collect::<BTreeSet<_>>();
    let workspace_crates = workspace_crate_names(root)?;
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
                "{}:{}: configured strangler path `{}` does not exist",
                target_path.display(),
                strangler.config_line,
                strangler.path.display()
            );
        }
        let scoped_sources = sources_for_path(&all_sources, &strangler.path, absolute.is_file());
        let current = OccurrenceCorpus::count_in_sources(&scoped_sources, &strangler.symbol);
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
        target: target_path.to_path_buf(),
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
            violations.push(format!(
                "{}:{}: module `{}` imports outside its allow list: {}",
                report.target.display(),
                rule.config_line,
                rule.path.display(),
                rule.unallowed_imports.join(", ")
            ));
        }
    }
    bail!(
        "atlas conform ratchet regressed:\n{}\n\nReduce the current values, run `cargo xtask atlas conform --tighten --file {}` after improvement, or deliberately edit {} to reopen a budget.",
        violations.join("\n"),
        report.target.display(),
        report.target.display()
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
    println!("Atlas conform — {}", report.target.display());
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
        assert_eq!(
            parse_args(&[]).unwrap(),
            Some(Args {
                mode: Mode::Report,
                file: None,
                json: false,
            })
        );
        assert_eq!(
            parse_args(&["--ratchet".into()]).unwrap(),
            Some(Args {
                mode: Mode::Ratchet,
                file: None,
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
        assert!(parse_args(&["--file".into(), "/tmp/target.toml".into()]).is_err());
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
        let target_path = root.path().join(TARGET_FILE);
        let mut configured = target::load(&target_path).unwrap().unwrap();
        let mut forbidden = configured.clone();
        forbidden.modules[0].allowed_imports.clear();
        let forbidden_report = evaluate(root.path(), &forbidden, &target_path).unwrap();
        assert_eq!(forbidden_report.regressions, 1);
        assert_eq!(forbidden_report.rules[0].unallowed_imports, ["other"]);
        assert!(enforce(&forbidden_report).is_err());

        let report = evaluate(root.path(), &configured, &target_path).unwrap();
        tighten(&mut configured, &report);
        target::write(&target_path, &configured).unwrap();
        let tightened = target::load(&target_path).unwrap().unwrap();
        assert_eq!(tightened.modules[0].pub_budget, 1);
        assert_eq!(tightened.strangler[0].baseline, 1);
        assert_eq!(tightened.strangler[1].baseline, 2);
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
            version: 1,
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
