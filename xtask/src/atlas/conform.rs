use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::facts::{Facets, Facts};
use super::modules::{crate_module_for_path, module_is_within, path_in_scope, scope_for_matching};
use super::sources::Source;
use super::syntax;
use super::target::{self, Layer, LayerRanks, ModuleRule, TARGET_FILE, Target};
use super::{REPORT_VERSION, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";

const USAGE: &str = "cargo xtask atlas conform [--ratchet|--tighten|--status|--init] [--file <path>] [--path <prefix>] [--layers <a+b,c,...>] [--verbose] [--json]

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
  --tighten      lower budgets/baselines and remove unused imports/debt
  --status       show distance to goals and admitted debt still in use
  --init         seed module budgets, layers, and upward imports from the current tree
  --file <path>  target file (default root refactor-target.toml);
                 absolute as-is, relative from the repository root
  --path <path>  root-relative init subtree (default crates/rimz/src)
  --layers <...> comma-separated low-to-high layers; `+` joins peers
  --verbose      show every rule instead of folding rules exactly at budget
  --json         versioned JSON agent contract (v4)

Schema:
  version = 4
  layers = [\"store\", [\"agents\", \"harness\"], \"cli\"]
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
    Status,
    Init,
}

impl Mode {
    fn report_label(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Tighten => "tighten",
            Self::Report | Self::Ratchet => "report",
            Self::Init => unreachable!("init returns before evaluation"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    mode: Mode,
    file: Option<PathBuf>,
    path: Option<PathBuf>,
    layers: Option<Vec<Layer>>,
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
struct DebtEntry {
    prefix: String,
    sites: usize,
    open: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    goal: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remaining: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    debt: Vec<DebtEntry>,
    unallowed_imports: Vec<String>,
    unallowed_import_sites: Vec<ImportSite>,
    #[serde(skip)]
    used_upward_imports: BTreeMap<String, BTreeSet<(PathBuf, usize)>>,
    config_line: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    mode: &'static str,
    target: PathBuf,
    #[serde(skip)]
    default_target: bool,
    layers: Vec<Layer>,
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
                        "mode": args.mode.report_label(),
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
    let report = evaluate(root, &target, &target_path, default_target, args.mode)?;
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
        Mode::Status => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .context("rendering atlas conform status JSON")?
                );
            } else {
                print_status(&report, args.verbose);
            }
            Ok(())
        }
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
    enforce(&evaluate(root, &target, &target_path, true, Mode::Ratchet)?)
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
            "--ratchet" | "--tighten" | "--status" | "--init" => {
                if mode != Mode::Report {
                    bail!(
                        "atlas conform --ratchet, --tighten, --status, and --init are mutually exclusive"
                    );
                }
                mode = match arg.as_str() {
                    "--ratchet" => Mode::Ratchet,
                    "--tighten" => Mode::Tighten,
                    "--status" => Mode::Status,
                    "--init" => Mode::Init,
                    _ => unreachable!(),
                };
                index += 1;
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
                let parsed = parse_layers(value(args, index, "conform", "--layers")?)?;
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
    if !matches!(mode, Mode::Report | Mode::Status) && verbose {
        bail!("atlas conform --verbose is only valid for report or --status");
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

fn parse_layers(raw: &str) -> Result<Vec<Layer>> {
    let layers = raw
        .split(',')
        .map(|layer| {
            let modules = layer
                .split('+')
                .map(str::trim)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if modules.iter().any(String::is_empty) {
                bail!(
                    "atlas conform --layers requires comma-separated layers; join peer modules with `+`"
                );
            }
            Ok(if let [module] = modules.as_slice() {
                Layer::Module(module.clone())
            } else {
                Layer::Group(modules)
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if layers.is_empty() {
        bail!("atlas conform --layers requires comma-separated layers; join peer modules with `+`");
    }
    Ok(layers)
}

fn initialize(root: &Path, scope: &Path, requested_layers: Option<&[Layer]>) -> Result<Target> {
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
        || {
            greedy_layers(files_by_rule.values().flatten().copied(), &facts)
                .into_iter()
                .map(Layer::Module)
                .collect()
        },
        <[Layer]>::to_vec,
    );
    let layer_ranks = LayerRanks::new(&layers);
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
                .flat_map(|file| file.imports.iter().map(move |import| (*file, import)))
                .filter_map(|(file, import)| {
                    syntax::resolved_internal_import(
                        import,
                        &facts.known_modules,
                        &facts.crate_names,
                    )
                    .map(|resolved| (file, resolved))
                })
                .filter(|(file, import)| {
                    layer_direction(&layer_ranks, &file.module_path, import)
                        == Some(Direction::Upward)
                })
                .map(|(_, import)| import)
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
                surface_goal: None,
                upward_debt: None,
                config_line: 0,
            }
        })
        .collect();
    Ok(Target {
        version: 4,
        layers,
        modules,
        strangler: Vec::new(),
    })
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

fn greedy_layers<'a>(
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
    mode: Mode,
) -> Result<Report> {
    let facts = Facts::load(root, Path::new("."), Facets::default())?;
    let layer_ranks = target.layer_ranks();
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
        let mut used_upward_imports = BTreeMap::<String, BTreeSet<(PathBuf, usize)>>::new();
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
                    match layer_direction(&layer_ranks, &file.module_path, &resolved) {
                        Some(Direction::Upward) => {
                            let mut matched = false;
                            for prefix in module
                                .upward_imports
                                .as_deref()
                                .unwrap_or_default()
                                .iter()
                                .filter(|allowed| module_is_within(&resolved, allowed))
                            {
                                used_upward_imports
                                    .entry(prefix.clone())
                                    .or_default()
                                    .insert((file.path.clone(), import.line));
                                matched = true;
                            }
                            matched
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
        let debt = module
            .upward_debt
            .iter()
            .flatten()
            .map(|prefix| {
                let sites = used_upward_imports.get(prefix).map_or(0, BTreeSet::len);
                DebtEntry {
                    prefix: prefix.clone(),
                    sites,
                    open: sites > 0,
                }
            })
            .collect();
        rules.push(RuleResult {
            kind: if module.allowed_imports.is_some() {
                "module"
            } else {
                "upward-import"
            },
            path: module.path.clone(),
            symbol: None,
            status: if regression {
                "regression"
            } else if module.surface_goal.is_some_and(|goal| current <= goal) {
                "met"
            } else {
                "ok"
            },
            current,
            budget: module.surface_budget,
            delta: current as isize - module.surface_budget as isize,
            goal: module.surface_goal,
            remaining: module.surface_goal.map(|goal| current.saturating_sub(goal)),
            debt,
            unallowed_imports,
            unallowed_import_sites,
            used_upward_imports,
            config_line: module.config_line,
        });
    }
    for file in facts.syntax.files.iter().filter(|file| {
        !target.modules.iter().any(|module| {
            let absolute = root.join(&module.path);
            if absolute.is_file() {
                file.path == module.path
            } else {
                path_in_scope(&file.path, &module.path)
            }
        })
    }) {
        if layer_ranks.get(top_module(&file.module_path)).is_none() {
            continue;
        }
        let mut unallowed = BTreeMap::<String, BTreeSet<(PathBuf, usize)>>::new();
        for import in &file.imports {
            let Some(resolved) =
                syntax::resolved_internal_import(import, &facts.known_modules, &facts.crate_names)
            else {
                continue;
            };
            if layer_direction(&layer_ranks, &file.module_path, &resolved)
                != Some(Direction::Upward)
            {
                continue;
            }
            unallowed
                .entry(resolved)
                .or_default()
                .insert((file.path.clone(), import.line));
        }
        if unallowed.is_empty() {
            continue;
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
        rules.push(RuleResult {
            kind: "upward-import",
            path: file.path.clone(),
            symbol: None,
            status: "regression",
            current: 0,
            budget: 0,
            delta: 0,
            goal: None,
            remaining: None,
            debt: Vec::new(),
            unallowed_imports,
            unallowed_import_sites,
            used_upward_imports: BTreeMap::new(),
            config_line: 1,
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
            goal: Some(0),
            remaining: Some(current),
            debt: Vec::new(),
            unallowed_imports: Vec::new(),
            unallowed_import_sites: Vec::new(),
            used_upward_imports: BTreeMap::new(),
            config_line: strangler.config_line,
        });
    }
    Ok(Report {
        version: REPORT_VERSION,
        verb: "conform",
        mode: mode.report_label(),
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
        .flat_map(|file| file.pub_items.iter().map(move |item| (file, item)))
        .filter(|(file, item)| super::modules::item_escapes(file, item, target_module, mod_index))
        .count()
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
    for module in &mut target.modules {
        if let Some(result) = report
            .rules
            .iter()
            .find(|result| result.symbol.is_none() && result.path == module.path)
        {
            module.surface_budget = module.surface_budget.min(result.current);
            if module
                .surface_goal
                .is_some_and(|goal| result.current <= goal)
            {
                module.surface_goal = None;
            }
            if let Some(upward_imports) = &mut module.upward_imports {
                upward_imports.retain(|import| result.used_upward_imports.contains_key(import));
                if upward_imports.is_empty() {
                    module.upward_imports = None;
                }
            }
            if let Some(upward_debt) = &mut module.upward_debt {
                upward_debt.retain(|import| result.used_upward_imports.contains_key(import));
                if upward_debt.is_empty() {
                    module.upward_debt = None;
                }
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
fn print_report(report: &Report, verbose: bool) {
    println!("Atlas conform — {}", report.target.display());
    if !report.layers.is_empty() {
        println!("layers: {}", render_layers(&report.layers));
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

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas conform status is the command's stdout contract"
)]
fn print_status(report: &Report, verbose: bool) {
    println!("Atlas conform status — {}", report.target.display());
    if !report.layers.is_empty() {
        println!("layers: {}", render_layers(&report.layers));
    }
    println!("status      kind        budget  current     goal  remaining  rule");
    for rule in status_rules(report, verbose) {
        let label = rule.symbol.as_ref().map_or_else(
            || rule.path.display().to_string(),
            |symbol| format!("{symbol} ({})", rule.path.display()),
        );
        let goal = rule
            .goal
            .map_or_else(|| "-".to_owned(), |goal| goal.to_string());
        let remaining = rule
            .remaining
            .map_or_else(|| "-".to_owned(), |remaining| remaining.to_string());
        println!(
            "{:<11} {:<10} {:>7} {:>8} {:>8} {:>10}  {}",
            rule.status, rule.kind, rule.budget, rule.current, goal, remaining, label
        );
        for debt in &rule.debt {
            println!(
                "  debt: {}  {} sites  {}",
                debt.prefix,
                debt.sites,
                if debt.open { "open" } else { "closed" }
            );
        }
    }
    println!("{}", status_summary(report));
}

fn status_rules(report: &Report, verbose: bool) -> Vec<&RuleResult> {
    report
        .rules
        .iter()
        .filter(|rule| {
            verbose || rule.status == "regression" || rule.goal.is_some() || !rule.debt.is_empty()
        })
        .collect()
}

fn render_layers(layers: &[Layer]) -> String {
    layers
        .iter()
        .map(|layer| layer.modules().join(" + "))
        .collect::<Vec<_>>()
        .join(" < ")
}

fn status_summary(report: &Report) -> String {
    let module_goals = report
        .rules
        .iter()
        .filter(|rule| rule.kind != "strangler" && rule.goal.is_some())
        .count();
    let module_debt = report
        .rules
        .iter()
        .filter(|rule| rule.kind != "strangler" && !rule.debt.is_empty())
        .count();
    let stranglers = report
        .rules
        .iter()
        .filter(|rule| rule.kind == "strangler")
        .count();
    let remaining_surface = report
        .rules
        .iter()
        .filter(|rule| rule.kind != "strangler")
        .filter_map(|rule| rule.remaining)
        .sum::<usize>();
    let strangler_occurrences = report
        .rules
        .iter()
        .filter(|rule| rule.kind == "strangler")
        .map(|rule| rule.current)
        .sum::<usize>();
    let total_debt = report
        .rules
        .iter()
        .map(|rule| rule.debt.len())
        .sum::<usize>();
    let open_debt = report
        .rules
        .iter()
        .flat_map(|rule| &rule.debt)
        .filter(|debt| debt.open)
        .count();
    let open_debt_sites = report
        .rules
        .iter()
        .flat_map(|rule| &rule.debt)
        .filter(|debt| debt.open)
        .map(|debt| debt.sites)
        .sum::<usize>();
    format!(
        "summary: {module_goals} module rules with goals, {module_debt} with debt, {stranglers} stranglers; remaining surface {remaining_surface}; strangler occurrences {strangler_occurrences}; open debt {open_debt}/{total_debt} ({open_debt_sites} sites); {} regressions; {} parse failures",
        report.regressions, report.parse_failures
    )
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
mod tests;
