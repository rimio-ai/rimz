use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::api::OccurrenceCorpus;
use super::modules::{
    crate_module_for_path, path_in_scope, scope_for_matching, workspace_crate_names,
};
use super::sources::{self, Source};
use super::syntax;
use super::target::{self, ModuleRule, TARGET_FILE, Target};
use super::{set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";

const USAGE: &str = "cargo xtask atlas conform [--ratchet|--tighten|--init] [--file <path>] [--path <prefix>] [--json]

Compares the working tree with a refactor target (root refactor-target.toml by
default). `--ratchet` fails only when current values exceed budgets/baselines or
an import is outside its allow list. `--tighten` atomically lowers budgets and
baselines to current values; it never raises them. A strangler counts whole-word
occurrences of its symbol in non-test Rust under its path (a file or directory).
A missing default target passes; a missing explicit --file is an error. `--init`
creates a clean current-tree baseline and never overwrites an existing target.
Import allow-lists cover resolved internal `use` declarations only.
Split Rust modules (`foo.rs` plus `foo/`) remain separate filesystem rules.

  --ratchet      fail on regressions (the checks/gate mode)
  --tighten      lower budgets and baselines to current values
  --init         seed module budgets and import allow-lists from the current tree
  --file <path>  target file (default root refactor-target.toml);
                 absolute as-is, relative from the repository root
  --path <path>  root-relative init subtree (default crates/rimz/src)
  --json         versioned JSON agent contract (v1)

Schema:
  version = 1
  [[module]]
  path = \"crates/rimz/src/cli\"
  allowed-imports = [\"agents\"]
  pub-budget = 10
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
    #[serde(skip)]
    default_target: bool,
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
        let target = initialize(root, scope)?;
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
                        "version": 1,
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
                print_report(&report);
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
    Ok(Some(Args {
        mode,
        file,
        path,
        json,
    }))
}

fn initialize(root: &Path, scope: &Path) -> Result<Target> {
    let all_sources = sources::working_tree_rust_sources(root)?;
    let scoped_sources = all_sources
        .iter()
        .filter(|source| path_in_scope(&source.path, scope))
        .cloned()
        .collect::<Vec<_>>();
    if scoped_sources.is_empty() {
        bail!("no Rust files under `{}`", scope.display());
    }
    let syntax = syntax::analyze_sources(&scoped_sources);
    let known_modules = all_sources
        .iter()
        .map(|source| crate_module_for_path(&source.path))
        .collect::<BTreeSet<_>>();
    let workspace_crates = workspace_crate_names(root)?;
    let mut files_by_rule = BTreeMap::new();
    for file in &syntax.files {
        files_by_rule
            .entry(direct_rule_path(&file.path, scope))
            .or_insert_with(Vec::new)
            .push(file);
    }
    let modules = files_by_rule
        .into_iter()
        .map(|(path, files)| {
            let module_entry = if root.join(&path).is_dir() {
                path.join("mod.rs")
            } else {
                path.clone()
            };
            let target_module = crate_module_for_path(&module_entry);
            let imports = files
                .iter()
                .flat_map(|file| &file.imports)
                .filter_map(|import| {
                    syntax::resolved_internal_import(import, &known_modules, &workspace_crates)
                })
                .filter(|import| !is_within(import, &target_module))
                .collect::<BTreeSet<_>>();
            let mut allowed_imports = Vec::<String>::new();
            for import in imports {
                if !allowed_imports
                    .iter()
                    .any(|allowed| is_within(&import, allowed))
                {
                    allowed_imports.push(import);
                }
            }
            ModuleRule {
                path,
                allowed_imports,
                pub_budget: files.iter().map(|file| file.pub_items.len()).sum(),
                config_line: 0,
            }
        })
        .collect();
    Ok(Target {
        version: 1,
        modules,
        strangler: Vec::new(),
    })
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
        default_target,
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
                path: None,
                json: false,
            })
        );
        assert_eq!(
            parse_args(&["--ratchet".into()]).unwrap(),
            Some(Args {
                mode: Mode::Ratchet,
                file: None,
                path: None,
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
        assert!(is_within("cli::render::table", "cli::render"));
        assert!(!is_within("cli::renderer", "cli::render"));
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
        let forbidden_report = evaluate(root.path(), &forbidden, &target_path, true).unwrap();
        assert_eq!(forbidden_report.regressions, 1);
        assert_eq!(forbidden_report.rules[0].unallowed_imports, ["other"]);
        assert!(enforce(&forbidden_report).is_err());

        let report = evaluate(root.path(), &configured, &target_path, true).unwrap();
        tighten(&mut configured, &report);
        target::write(&target_path, &configured).unwrap();
        let tightened = target::load(&target_path).unwrap().unwrap();
        assert_eq!(tightened.modules[0].pub_budget, 1);
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
        assert_eq!(initialized.modules[0].allowed_imports, ["other"]);
        assert_eq!(initialized.modules[1].path, Path::new("src/nested"));
        assert_eq!(initialized.modules[1].allowed_imports, ["other"]);
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
