use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::detect::{self, PassThrough, RepeatedGuard, VestigialItem};
use super::facts::{Facets, Facts};
use super::history;
use super::index::IndexPolicy;
use super::modules::{crate_module_for_path, module_for_path, module_is_within, path_in_scope};
use super::{REPORT_VERSION, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const USAGE: &str = "cargo xtask atlas brief (--module <path>|--all --out-dir <dir>) [--path <scope>] [--md|--json] [--no-index]

Builds a module dossier from one shared Atlas facts model.";

#[derive(Debug)]
struct Args {
    module: Option<PathBuf>,
    all: bool,
    out_dir: Option<PathBuf>,
    path: PathBuf,
    json: bool,
    no_index: bool,
}

#[derive(Clone, Debug, Serialize)]
struct InterfaceItem {
    name: String,
    kind: String,
    path: PathBuf,
    line: usize,
    declared: String,
    effective_reach: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    production_ref_modules: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize)]
struct Caller {
    module: String,
    items: usize,
    item_names: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct Provider {
    module: String,
    items: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    module: PathBuf,
    code: u64,
    tests: u64,
    test_files: usize,
    inline_test_regions: usize,
    pub_items: usize,
    escaping_items: usize,
    interface: Vec<InterfaceItem>,
    callers: Vec<Caller>,
    providers: Vec<Provider>,
    cochange: Vec<super::history::CochangeEdge>,
    divergence: Vec<super::survey::Divergence>,
    shapes: serde_json::Value,
    passthroughs: Vec<PassThrough>,
    vestigial_items: Vec<VestigialItem>,
    repeated_guards: Vec<RepeatedGuard>,
    conform_rules: Vec<PathBuf>,
    parse_failures: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas brief output is a command stdout contract"
)]
pub(super) fn run(root: &Path, raw: &[String]) -> Result<()> {
    let Some(args) = parse_args(raw)? else {
        println!("{USAGE}");
        return Ok(());
    };
    if args.no_index {
        super::note_no_index();
    }
    let facts = Facts::load(
        root,
        &args.path,
        Facets {
            history: true,
            metrics: true,
            references: Some(if args.no_index {
                IndexPolicy::Skip
            } else {
                IndexPolicy::Required
            }),
            blame: true,
        },
    )?;
    if args.all {
        let out_dir = args
            .out_dir
            .as_ref()
            .context("brief --all output directory missing")?;
        fs::create_dir_all(out_dir)
            .with_context(|| format!("creating brief output directory {}", out_dir.display()))?;
        let modules = facts
            .sizes
            .keys()
            .filter(|path| path_in_scope(path, &args.path))
            .map(|path| module_for_path(path, &args.path))
            .filter(|module| module != "(root)")
            .collect::<BTreeSet<_>>();
        for module in modules {
            let path = args.path.join(&module);
            let report = build_report(&facts, &path, &args)?;
            fs::write(
                out_dir.join(format!("{}.md", module.replace('/', "-"))),
                markdown(&report),
            )
            .with_context(|| format!("writing brief for {module}"))?;
        }
        println!("wrote briefs to {}", out_dir.display());
        return Ok(());
    }
    let module = args.module.as_ref().context("brief module missing")?;
    let report = build_report(&facts, module, &args)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("rendering atlas brief JSON")?
        );
    } else {
        print!("{}", markdown(&report));
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut module = None;
    let mut out_dir = None;
    let mut path = None;
    let mut all = false;
    let mut md = false;
    let mut json = false;
    let mut no_index = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--module" => {
                let parsed = validate_scope(value(args, index, "brief", "--module")?, "--module")?;
                set_once(&mut module, parsed, "brief", "--module")?;
                index += 2;
            }
            "--path" => {
                let parsed = validate_scope(value(args, index, "brief", "--path")?, "--path")?;
                set_once(&mut path, parsed, "brief", "--path")?;
                index += 2;
            }
            "--out-dir" => {
                let parsed = PathBuf::from(value(args, index, "brief", "--out-dir")?);
                set_once(&mut out_dir, parsed, "brief", "--out-dir")?;
                index += 2;
            }
            "--all" if !all => {
                all = true;
                index += 1;
            }
            "--md" if !md => {
                md = true;
                index += 1;
            }
            "--json" if !json => {
                json = true;
                index += 1;
            }
            "--no-index" if !no_index => {
                no_index = true;
                index += 1;
            }
            "--all" | "--md" | "--json" | "--no-index" => {
                bail!("atlas brief flag `{arg}` may only be passed once")
            }
            _ => bail!("unknown atlas brief argument `{arg}`"),
        }
    }
    if all == module.is_some() {
        bail!("atlas brief requires exactly one of --module or --all");
    }
    if all != out_dir.is_some() {
        bail!("atlas brief --all requires --out-dir, which is invalid without --all");
    }
    if md && json {
        bail!("atlas brief --md and --json are mutually exclusive");
    }
    Ok(Some(Args {
        module,
        all,
        out_dir,
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        json,
        no_index,
    }))
}

fn build_report(facts: &Facts, module: &Path, args: &Args) -> Result<Report> {
    if !module.starts_with(&args.path) || !facts.root.join(module).exists() {
        bail!(
            "atlas brief module `{}` is not under `{}`",
            module.display(),
            args.path.display()
        );
    }
    let module_entry = if facts.root.join(module).is_dir() {
        module.join("mod.rs")
    } else {
        module.to_path_buf()
    };
    let crate_module = crate_module_for_path(&module_entry);
    let files = facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, module))
        .collect::<Vec<_>>();
    let mut interface = Vec::new();
    for file in &files {
        for item in &file.pub_items {
            let reach = facts.mod_index.effective_reach(file, item);
            if module_is_within(&reach, &crate_module) {
                continue;
            }
            let refs = facts.references.as_ref().map(|references| {
                references
                    .get(file, item)
                    .map_or_else(Vec::new, |item_refs| {
                        item_refs
                            .production
                            .iter()
                            .filter(|caller| !module_is_within(caller, &item.module))
                            .cloned()
                            .collect()
                    })
            });
            interface.push(InterfaceItem {
                name: item.name.clone(),
                kind: item.kind.clone(),
                path: file.path.clone(),
                line: item.line,
                declared: item.declared.clone(),
                effective_reach: reach,
                production_ref_modules: refs,
            });
        }
    }
    let callers = callers(facts, &crate_module);
    let providers = providers(facts, module, &crate_module);
    let cochange = history::cochange(
        facts
            .history
            .as_ref()
            .context("brief history facts missing")?,
        &facts.root,
        module,
        None,
        25,
        10,
    )?;
    let passthroughs = detect::passthroughs(facts, module);
    let vestigial_items = detect::vestigial(facts, module, 25);
    let repeated_guards = detect::guards(facts, module, 3);
    let (code, tests) = facts
        .sizes
        .iter()
        .filter(|(path, _)| path_in_scope(path, module))
        .fold((0, 0), |(code, tests), (_, size)| {
            (code + size.code, tests + size.tests)
        });
    let test_files = facts
        .sources
        .iter()
        .filter(|source| path_in_scope(&source.path, module) && source.is_test())
        .count();
    let inline_test_regions = files.iter().map(|file| file.test_regions.len()).sum();
    let conform_rules = super::target::load(&facts.root.join(super::target::TARGET_FILE))?
        .map(|target| {
            target
                .modules
                .into_iter()
                .filter(|rule| rule.path.starts_with(module) || module.starts_with(&rule.path))
                .map(|rule| rule.path)
                .collect()
        })
        .unwrap_or_default();
    let divergence = super::survey::divergence(facts, module, &cochange.edges, 3);
    Ok(Report {
        version: REPORT_VERSION,
        verb: "brief",
        module: module.to_path_buf(),
        code,
        tests,
        test_files,
        inline_test_regions,
        pub_items: files.iter().map(|file| file.pub_items.len()).sum(),
        escaping_items: interface.len(),
        interface,
        callers,
        providers,
        cochange: cochange.edges,
        divergence,
        shapes: super::shapes::survey_value(facts, module)?,
        passthroughs,
        vestigial_items,
        repeated_guards,
        conform_rules,
        parse_failures: facts
            .syntax
            .parse_failures
            .iter()
            .filter(|path| path_in_scope(path, module))
            .count(),
    })
}

fn callers(facts: &Facts, crate_module: &str) -> Vec<Caller> {
    let mut rows = BTreeMap::<String, BTreeSet<String>>::new();
    if let Some(references) = &facts.references {
        for edge in references.edges.iter().filter(|edge| {
            !edge.test
                && module_is_within(&edge.to, crate_module)
                && !module_is_within(&edge.from, crate_module)
        }) {
            rows.entry(edge.from.clone())
                .or_default()
                .insert(edge.item.clone());
        }
    }
    let mut rows = rows
        .into_iter()
        .map(|(module, items)| Caller {
            module,
            items: items.len(),
            item_names: items.into_iter().collect(),
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| left.module.cmp(&right.module))
    });
    rows
}

fn providers(facts: &Facts, module: &Path, crate_module: &str) -> Vec<Provider> {
    let mut rows = BTreeMap::<String, BTreeSet<String>>::new();
    for file in facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, module))
    {
        for import in &file.imports {
            let Some(resolved) = super::syntax::resolved_internal_import(
                import,
                &facts.known_modules,
                &facts.crate_names,
            ) else {
                continue;
            };
            if !module_is_within(&resolved, crate_module) {
                rows.entry(resolved)
                    .or_default()
                    .insert(import.item.clone());
            }
        }
    }
    rows.into_iter()
        .map(|(module, items)| Provider {
            module,
            items: items.into_iter().collect(),
        })
        .collect()
}

fn markdown(report: &Report) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(out, "# Atlas brief — {}", report.module.display());
    let _ = writeln!(
        out,
        "\ncode {} · tests {} · public {} · escaping {}",
        report.code, report.tests, report.pub_items, report.escaping_items
    );
    let _ = writeln!(out, "\n## Interface\n```");
    for item in &report.interface {
        let _ = writeln!(
            out,
            "{}:{} {:<12} {:<24} {}",
            item.path.display(),
            item.line,
            item.kind,
            item.name,
            item.effective_reach
        );
    }
    let _ = writeln!(out, "```\n\n## Callers by assembly\n```");
    for caller in &report.callers {
        let _ = writeln!(
            out,
            "{:<32} {:>4}  {}",
            caller.module,
            caller.items,
            caller.item_names.join(", ")
        );
    }
    let _ = writeln!(out, "```\n\n## Providers\n```");
    for provider in &report.providers {
        let _ = writeln!(out, "{:<32} {}", provider.module, provider.items.join(", "));
    }
    let _ = writeln!(out, "```\n\n## Co-change and divergence\n```");
    for edge in &report.cochange {
        let _ = writeln!(out, "{} <> {} ({})", edge.left, edge.right, edge.commits);
    }
    for row in &report.divergence {
        let _ = writeln!(
            out,
            "{} <> {} items {} cochanges {} [{}]",
            row.left, row.right, row.items, row.cochanges, row.kind
        );
    }
    let _ = writeln!(
        out,
        "```\n\n## Shapes\n```\n{}\n```",
        serde_json::to_string_pretty(&report.shapes).unwrap_or_default()
    );
    let _ = writeln!(out, "\n## Pass-throughs\n```");
    for row in &report.passthroughs {
        let _ = writeln!(
            out,
            "{}:{} {} -> {}",
            row.path.display(),
            row.line,
            row.name,
            row.callee
        );
    }
    let _ = writeln!(out, "```\n\n## Vestigial items\n```");
    for row in &report.vestigial_items {
        let _ = writeln!(
            out,
            "{}:{} {} ({}d)",
            row.path.display(),
            row.line,
            row.name,
            row.age_days
        );
    }
    let _ = writeln!(out, "```\n\n## Repeated guards\n```");
    for row in &report.repeated_guards {
        let _ = writeln!(out, "{} files  {}", row.files, row.predicate);
    }
    let _ = writeln!(
        out,
        "```\n\n## Test shape\n{} test SLOC · {} files · {} inline regions",
        report.tests, report.test_files, report.inline_test_regions
    );
    let _ = writeln!(
        out,
        "\n## Conform rules\n{}",
        report
            .conform_rules
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    out
}
