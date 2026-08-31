use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::facts::{Facets, Facts};
use super::modules::{
    crate_module_for_row, module_for_path, module_is_within, path_in_scope, reference_module_label,
};
use super::syntax::FileSyntax;
use super::{REPORT_VERSION, positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const DEFAULT_TOP: usize = 20;

const USAGE: &str =
    "cargo xtask atlas api [--path <prefix>] [--top N] [--module <name>] [--since <ref>] [--no-index] [--json]

Reports declared and effective visibility for boundary-visible Rust items.

  --path <path>    root-relative subtree (default crates/rimz/src)
  --top N          rows per section (default 20)
  --module <name>  drill into one module from the module table
  --since <ref>    add public-item delta against a git revision
  --no-index       omit exact-reference fields
  --json           versioned JSON agent contract (v3)";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    path: PathBuf,
    top: usize,
    module: Option<String>,
    since: Option<String>,
    no_index: bool,
    json: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ItemOccurrence {
    module: String,
    name: String,
    kind: String,
    path: PathBuf,
    line: usize,
    declared_visibility: String,
    effective_reach: String,
    escapes_module: bool,
    params: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    production_ref_modules: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    production_refs: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_ref_modules: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_refs: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    single_caller: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ModuleApi {
    module: String,
    items: usize,
    escaping_items: usize,
    params_median: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refs: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unref: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_only: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    single: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unresolved: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_pub: Option<isize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct SingleCallerModule {
    module: String,
    items: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_module: Option<String>,
    total_modules: usize,
    modules: Vec<ModuleApi>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    module_items: Vec<ItemOccurrence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_single_caller_items: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    single_caller_modules: Option<Vec<SingleCallerModule>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    single_caller_items: Option<Vec<ItemOccurrence>>,
    parse_failures: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas api output is a command stdout contract"
)]
pub(super) fn run(root: &Path, args: &[String]) -> Result<()> {
    let Some(args) = parse_args(args)? else {
        println!("{USAGE}");
        return Ok(());
    };
    if args.no_index {
        super::note_no_index();
    }
    let report = build_report(root, &args)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("rendering atlas api JSON")?
        );
    } else {
        print_report(
            &report,
            args.top,
            args.module.as_deref(),
            args.since.is_some(),
        );
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut path = None;
    let mut top = None;
    let mut module = None;
    let mut since = None;
    let mut no_index = false;
    let mut json = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--path" => {
                let parsed = validate_scope(value(args, index, "api", "--path")?, "--path")?;
                set_once(&mut path, parsed, "api", "--path")?;
                index += 2;
            }
            "--top" => {
                let parsed = positive_usize(value(args, index, "api", "--top")?, "api", "--top")?;
                set_once(&mut top, parsed, "api", "--top")?;
                index += 2;
            }
            "--module" => {
                let parsed = value(args, index, "api", "--module")?.to_owned();
                set_once(&mut module, parsed, "api", "--module")?;
                index += 2;
            }
            "--since" => {
                let reference = value(args, index, "api", "--since")?.to_owned();
                set_once(&mut since, reference, "api", "--since")?;
                index += 2;
            }
            "--no-index" if !no_index => {
                no_index = true;
                index += 1;
            }
            "--no-index" => bail!("atlas api --no-index may only be passed once"),
            "--json" if !json => {
                json = true;
                index += 1;
            }
            "--json" => bail!("atlas api --json may only be passed once"),
            _ => bail!("unknown atlas api argument `{arg}`"),
        }
    }
    Ok(Some(Args {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        top: top.unwrap_or(DEFAULT_TOP),
        module,
        since,
        no_index,
        json,
    }))
}

fn build_report(root: &Path, args: &Args) -> Result<Report> {
    let facts = Facts::load(
        root,
        &args.path,
        Facets {
            references: Some(if args.no_index {
                super::index::IndexPolicy::Skip
            } else {
                super::index::IndexPolicy::Required
            }),
            ..Facets::default()
        },
    )?;
    let scoped_files = facts
        .syntax
        .files
        .iter()
        .filter(|file| path_in_scope(&file.path, &args.path))
        .cloned()
        .collect::<Vec<_>>();
    let previous = args
        .since
        .as_deref()
        .map(|reference| Facts::load_at(root, &args.path, reference))
        .transpose()?;
    let previous_counts = previous.as_ref().map(|facts| {
        let files = facts
            .syntax
            .files
            .iter()
            .filter(|file| path_in_scope(&file.path, &args.path))
            .cloned()
            .collect::<Vec<_>>();
        public_counts(&files, &args.path)
    });

    let mut module_items = BTreeMap::<String, Vec<ItemOccurrence>>::new();
    let mut module_params = BTreeMap::<String, Vec<usize>>::new();
    let scope_module = crate_module_for_row(&args.path, "(root)");
    for file in &scoped_files {
        let module = module_for_path(&file.path, &args.path);
        for item in &file.pub_items {
            if let Some(params) = item.params {
                module_params
                    .entry(module.clone())
                    .or_default()
                    .push(params);
            }
        }
        module_items.entry(module).or_default().extend(
            file.pub_items
                .iter()
                .map(|item| item_occurrence(&facts, file, item, &scope_module)),
        );
    }

    let mut requested_module_items = args
        .module
        .as_ref()
        .and_then(|module| module_items.get(module))
        .cloned()
        .unwrap_or_default();
    requested_module_items.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
    });
    let references_loaded = facts.references.is_some();
    let mut single_caller_items = Vec::<(String, ItemOccurrence)>::new();
    let mut modules = module_items
        .into_iter()
        .map(|(module, items)| {
            let target_module = crate_module_for_row(&args.path, &module);
            let item_count = items.len();
            let params_median = module_params
                .get(&module)
                .map(|values| median(values.clone()));
            let delta_pub = previous_counts.as_ref().map(|counts| {
                item_count as isize - counts.get(&module).copied().unwrap_or(0) as isize
            });
            let escaping = items
                .iter()
                .filter(|item| !module_is_within(&item.effective_reach, &target_module))
                .collect::<Vec<_>>();
            single_caller_items.extend(
                escaping
                    .iter()
                    .filter(|item| item.single_caller == Some(true))
                    .map(|item| (module.clone(), (*item).clone())),
            );
            let resolved = escaping
                .iter()
                .filter(|item| item.resolved == Some(true))
                .copied()
                .collect::<Vec<_>>();
            ModuleApi {
                module,
                items: item_count,
                escaping_items: escaping.len(),
                params_median,
                refs: references_loaded
                    .then(|| {
                        (!resolved.is_empty()).then(|| {
                            median(
                                resolved
                                    .iter()
                                    .map(|item| {
                                        item.production_ref_modules.as_ref().map_or(0, Vec::len)
                                    })
                                    .collect(),
                            )
                        })
                    })
                    .flatten(),
                unref: references_loaded.then(|| {
                    resolved
                        .iter()
                        .filter(|item| {
                            item.production_ref_modules
                                .as_ref()
                                .is_some_and(Vec::is_empty)
                        })
                        .count()
                }),
                test_only: references_loaded.then(|| {
                    resolved
                        .iter()
                        .filter(|item| {
                            item.production_ref_modules
                                .as_ref()
                                .is_some_and(Vec::is_empty)
                                && item
                                    .test_ref_modules
                                    .as_ref()
                                    .is_some_and(|modules| !modules.is_empty())
                        })
                        .count()
                }),
                single: references_loaded.then(|| {
                    resolved
                        .iter()
                        .filter(|item| item.single_caller == Some(true))
                        .count()
                }),
                unresolved: references_loaded.then(|| {
                    escaping
                        .iter()
                        .filter(|item| item.resolved == Some(false))
                        .count()
                }),
                delta_pub,
            }
        })
        .collect::<Vec<_>>();
    modules.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| left.module.cmp(&right.module))
    });
    validate_requested_module(&modules, args.module.as_deref())?;
    let total_modules = modules.len();
    single_caller_items.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.path.cmp(&right.1.path))
            .then_with(|| left.1.line.cmp(&right.1.line))
    });
    let single_caller_modules = references_loaded.then(|| {
        let mut counts = BTreeMap::<String, usize>::new();
        for (module, _) in &single_caller_items {
            *counts.entry(module.clone()).or_default() += 1;
        }
        let mut rows = counts
            .into_iter()
            .map(|(module, items)| SingleCallerModule { module, items })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            right
                .items
                .cmp(&left.items)
                .then_with(|| left.module.cmp(&right.module))
        });
        rows
    });
    let total_single_caller_items = references_loaded.then_some(single_caller_items.len());
    let single_caller_items = references_loaded.then(|| {
        single_caller_items
            .into_iter()
            .filter(|(module, _)| {
                args.module
                    .as_ref()
                    .is_none_or(|requested| requested == module)
            })
            .map(|(_, item)| item)
            .collect()
    });
    modules.truncate(args.top);
    Ok(Report {
        version: REPORT_VERSION,
        verb: "api",
        path: args.path.clone(),
        requested_module: args.module.clone(),
        total_modules,
        modules,
        module_items: requested_module_items,
        total_single_caller_items,
        single_caller_modules,
        single_caller_items,
        parse_failures: facts
            .syntax
            .parse_failures
            .iter()
            .filter(|path| path_in_scope(path, &args.path))
            .count(),
    })
}

fn item_occurrence(
    facts: &Facts,
    file: &FileSyntax,
    item: &super::syntax::PubItem,
    scope_module: &str,
) -> ItemOccurrence {
    let effective_reach = facts.mod_index.effective_reach(file, item);
    let item_refs = facts
        .references
        .as_ref()
        .and_then(|references| references.get(file, item));
    let outside = |modules: &std::collections::BTreeSet<String>| {
        modules
            .iter()
            .filter(|module| !module_is_within(module, &item.module))
            .map(|module| reference_module_label(module, scope_module))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    };
    let production_ref_modules = facts
        .references
        .as_ref()
        .map(|_| item_refs.map_or_else(Vec::new, |references| outside(&references.production)));
    let test_ref_modules = facts
        .references
        .as_ref()
        .map(|_| item_refs.map_or_else(Vec::new, |references| outside(&references.tests)));
    ItemOccurrence {
        module: reference_module_label(&item.module, scope_module),
        name: item.name.clone(),
        kind: item.kind.clone(),
        path: file.path.clone(),
        line: item.line,
        declared_visibility: item.declared.clone(),
        escapes_module: effective_reach != item.module,
        effective_reach,
        params: item.params,
        resolved: facts.references.as_ref().map(|_| item_refs.is_some()),
        production_refs: facts
            .references
            .as_ref()
            .map(|_| item_refs.map_or(0, |references| references.production_count)),
        test_refs: facts
            .references
            .as_ref()
            .map(|_| item_refs.map_or(0, |references| references.test_count)),
        single_caller: production_ref_modules
            .as_ref()
            .map(|modules| modules.len() == 1),
        production_ref_modules,
        test_ref_modules,
    }
}

fn validate_requested_module(modules: &[ModuleApi], requested: Option<&str>) -> Result<()> {
    let Some(requested) = requested else {
        return Ok(());
    };
    if modules.iter().any(|module| module.module == requested) {
        return Ok(());
    }
    bail!(
        "atlas api --module `{requested}` is not in the module table; choose a module from `cargo xtask atlas api`"
    )
}

fn public_counts(files: &[FileSyntax], scope: &Path) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for file in files {
        *counts
            .entry(module_for_path(&file.path, scope))
            .or_default() += file.pub_items.len();
    }
    counts
}

pub(super) fn median(mut values: Vec<usize>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) as f64 / 2.0
    } else {
        values[middle] as f64
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas api report is the command's stdout contract"
)]
fn print_report(report: &Report, top: usize, requested_module: Option<&str>, show_delta: bool) {
    println!("Atlas api — {}", report.path.display());
    let show_references = report
        .modules
        .iter()
        .any(|module| module.unresolved.is_some());
    println!("{}", module_header(show_references, show_delta));
    for module in report.modules.iter().take(top) {
        let params = module
            .params_median
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}"));
        let refs = module
            .refs
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}"));
        if show_references && show_delta {
            println!(
                "{:<25} {:>5} {:>5} {:>9} {:>5} {:>5} {:>5} {:>6} {:>10} {:+4}",
                module.module,
                module.items,
                module.escaping_items,
                params,
                refs,
                module.unref.unwrap_or(0),
                module.test_only.unwrap_or(0),
                module.single.unwrap_or(0),
                module.unresolved.unwrap_or(0),
                module.delta_pub.unwrap_or(0)
            );
        } else if show_references {
            println!(
                "{:<25} {:>5} {:>5} {:>9} {:>5} {:>5} {:>5} {:>6} {:>10}",
                module.module,
                module.items,
                module.escaping_items,
                params,
                refs,
                module.unref.unwrap_or(0),
                module.test_only.unwrap_or(0),
                module.single.unwrap_or(0),
                module.unresolved.unwrap_or(0)
            );
        } else if show_delta {
            println!(
                "{:<25} {:>5} {:>5} {:>9} {:+4}",
                module.module,
                module.items,
                module.escaping_items,
                params,
                module.delta_pub.unwrap_or(0)
            );
        } else {
            println!(
                "{:<25} {:>5} {:>5} {:>9}",
                module.module, module.items, module.escaping_items, params
            );
        }
    }
    if report.total_modules > report.modules.len() {
        println!(
            "… and {} more modules",
            report.total_modules - report.modules.len()
        );
    }
    println!();
    if let Some(module) = requested_module {
        println!("Public items in {module}");
        for item in report.module_items.iter().take(top) {
            let reference_evidence = reference_evidence(item);
            println!(
                "{}::{} ({}) {}:{} declared {} reach {} params {} [{}]{}",
                item.module,
                item.name,
                item.kind,
                item.path.display(),
                item.line,
                item.declared_visibility,
                display_reach(&item.effective_reach),
                item.params
                    .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                item_tags(item),
                reference_evidence,
            );
        }
        if report.module_items.len() > top {
            println!(
                "… and {} more public items (use --json for the complete list)",
                report.module_items.len() - top
            );
        }
    }
    if let Some(modules) = &report.single_caller_modules {
        println!();
        println!("Single-caller escaping items by defining module");
        for module in modules.iter().take(top) {
            println!("{:<32} {:>5}", module.module, module.items);
        }
    }
    if let Some(items) = &report.single_caller_items {
        println!();
        println!("Exact single-caller escaping items");
        for item in items.iter().take(top) {
            let caller = item
                .production_ref_modules
                .as_deref()
                .and_then(|modules| modules.first())
                .map_or("—", String::as_str);
            println!(
                "{}:{} {}::{} -> {}",
                item.path.display(),
                item.line,
                item.module,
                item.name,
                caller
            );
        }
    }
    println!(
        "{}",
        totals_line(
            report.total_modules,
            report.total_single_caller_items,
            report.parse_failures
        )
    );
}

fn reference_evidence(item: &ItemOccurrence) -> String {
    match item.resolved {
        Some(true) => format!(
            " resolved true prod {} [{}] test {} [{}]",
            item.production_refs.unwrap_or(0),
            item.production_ref_modules
                .as_deref()
                .unwrap_or_default()
                .join(","),
            item.test_refs.unwrap_or(0),
            item.test_ref_modules
                .as_deref()
                .unwrap_or_default()
                .join(",")
        ),
        Some(false) => " resolved false".to_owned(),
        None => String::new(),
    }
}

fn totals_line(
    total_modules: usize,
    single_callers: Option<usize>,
    parse_failures: usize,
) -> String {
    let references = single_callers.map_or_else(String::new, |items| {
        format!(", {items} exact single-caller items")
    });
    format!(
        "total: {} modules{}, {} parse failures",
        total_modules, references, parse_failures
    )
}

fn module_header(show_references: bool, show_delta: bool) -> &'static str {
    if show_references && show_delta {
        "module                    items   esc params/fn  refs unref  test single unresolved Δpub"
    } else if show_references {
        "module                    items   esc params/fn  refs unref  test single unresolved"
    } else if show_delta {
        "module                    items   esc params/fn Δpub"
    } else {
        "module                    items   esc params/fn"
    }
}

fn display_reach(reach: &str) -> &str {
    if reach.is_empty() { "crate" } else { reach }
}

fn item_tags(item: &ItemOccurrence) -> String {
    let mut tags = Vec::new();
    if item.effective_reach == super::modules::EXTERNAL_REACH {
        tags.push("external");
    }
    if item.escapes_module {
        tags.push("escapes-module");
    }
    if tags.is_empty() {
        "—".to_owned()
    } else {
        tags.join(",")
    }
}

#[cfg(test)]
mod v3_tests {
    use super::*;
    use crate::atlas::modules;

    #[test]
    fn api_header_only_advertises_requested_delta() {
        assert!(!module_header(false, false).contains("Δpub"));
        assert!(module_header(false, true).ends_with("Δpub"));
        assert!(module_header(true, false).contains("unresolved"));
    }

    #[test]
    fn api_module_drilldown_rejects_names_outside_the_table() {
        let modules = vec![ModuleApi {
            module: "agents_cmd".to_owned(),
            items: 1,
            escaping_items: 0,
            params_median: Some(0.0),
            refs: None,
            unref: None,
            test_only: None,
            single: None,
            unresolved: None,
            delta_pub: None,
        }];

        assert!(validate_requested_module(&modules, Some("agents_cmd")).is_ok());
        assert!(validate_requested_module(&modules, Some("missing")).is_err());
    }

    #[test]
    fn api_no_index_total_omits_reference_counts() {
        assert_eq!(
            totals_line(17, None, 0),
            "total: 17 modules, 0 parse failures"
        );
        assert_eq!(
            totals_line(17, Some(4), 0),
            "total: 17 modules, 4 exact single-caller items, 0 parse failures"
        );
    }

    #[test]
    fn api_unresolved_item_omits_reference_counts() {
        let mut item = ItemOccurrence {
            module: "agents".to_owned(),
            name: "Agent".to_owned(),
            kind: "struct".to_owned(),
            path: PathBuf::from("src/agents.rs"),
            line: 1,
            declared_visibility: "pub".to_owned(),
            effective_reach: modules::EXTERNAL_REACH.to_owned(),
            escapes_module: true,
            params: None,
            resolved: Some(false),
            production_ref_modules: Some(Vec::new()),
            production_refs: Some(0),
            test_ref_modules: Some(Vec::new()),
            test_refs: Some(0),
            single_caller: Some(false),
        };

        assert_eq!(reference_evidence(&item), " resolved false");
        item.resolved = Some(true);
        assert_eq!(
            reference_evidence(&item),
            " resolved true prod 0 [] test 0 []"
        );
    }
}
