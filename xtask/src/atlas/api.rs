use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::modules::{crate_module_for_path, module_for_path};
use super::sources::{self, Source};
use super::syntax::{self, FileSyntax, PubItem};
use super::{positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const DEFAULT_TOP: usize = 20;

const USAGE: &str =
    "cargo xtask atlas api [--path <prefix>] [--top N] [--module <name>] [--since <ref>] [--json]

Reports public boundary shape and whole-word identifier occurrences outside each
defining file-module. `occ` excludes tests; 0 may mean unreferenced or test-only.
Common identifiers can over-count, so `occ` is not a resolved caller count.

  --path <path>    root-relative subtree (default crates/rimz/src)
  --top N          rows per section (default 20)
  --module <name>  drill into one module from the module table
  --since <ref>    add public-item delta against a git revision
  --json           versioned JSON agent contract (v1)";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    path: PathBuf,
    top: usize,
    module: Option<String>,
    since: Option<String>,
    json: bool,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct ItemOccurrence {
    module: String,
    name: String,
    kind: String,
    path: PathBuf,
    line: usize,
    occurrences: usize,
    outside_modules: usize,
}

#[derive(Debug, Serialize)]
struct ModuleApi {
    module: String,
    pub_fns: usize,
    pub_types: usize,
    pub_items: usize,
    occurrence_median: f64,
    zero_occurrences: usize,
    params_median: Option<f64>,
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
    total_single_caller_items: usize,
    single_caller_modules: Vec<SingleCallerModule>,
    single_caller_items: Vec<ItemOccurrence>,
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
    let report = build_report(root, &args)?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("rendering atlas api JSON")?
        );
    } else {
        print_report(&report, args.top, args.module.as_deref());
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
        json,
    }))
}

fn build_report(root: &Path, args: &Args) -> Result<Report> {
    let scoped_sources = sources::scope_sources(root, &args.path, None)?;
    let all_sources = sources::scope_sources(root, Path::new("."), None)?;
    let occurrence_corpus = OccurrenceCorpus::new(&all_sources);
    let syntax = syntax::analyze_sources(&scoped_sources);
    let previous = args
        .since
        .as_deref()
        .map(|reference| sources::scope_sources(root, &args.path, Some(reference)))
        .transpose()?
        .map(|sources| syntax::analyze_sources(&sources));
    let previous_counts = previous
        .as_ref()
        .map(|report| public_counts(&report.files, &args.path));

    let mut module_items = BTreeMap::<String, Vec<ItemOccurrence>>::new();
    let mut module_params = BTreeMap::<String, Vec<usize>>::new();
    let mut module_kinds = BTreeMap::<String, (usize, usize)>::new();
    for file in &syntax.files {
        let module = module_for_path(&file.path, &args.path);
        for item in &file.pub_items {
            let occurrence = count_occurrences(item, file, &occurrence_corpus);
            module_items
                .entry(module.clone())
                .or_default()
                .push(occurrence);
            if let Some(params) = item.params {
                module_params
                    .entry(module.clone())
                    .or_default()
                    .push(params);
            }
            let kinds = module_kinds.entry(module.clone()).or_default();
            if item.kind == "fn" {
                kinds.0 += 1;
            } else {
                kinds.1 += 1;
            }
        }
    }

    let mut scoped_single_caller_items = Vec::new();
    let mut modules = module_items
        .into_iter()
        .map(|(module, items)| {
            scoped_single_caller_items.extend(
                items
                    .iter()
                    .filter(|item| item.outside_modules == 1)
                    .cloned()
                    .map(|item| (module.clone(), item)),
            );
            let (pub_fns, pub_types) = module_kinds.get(&module).copied().unwrap_or_default();
            let pub_items = items.len();
            let occurrence_median = median(
                items
                    .iter()
                    .map(|item| item.occurrences)
                    .collect::<Vec<_>>(),
            );
            let zero_occurrences = zero_occurrence_count(&items);
            let params_median = module_params
                .get(&module)
                .map(|values| median(values.clone()));
            let delta_pub = previous_counts.as_ref().map(|counts| {
                pub_items as isize - counts.get(&module).copied().unwrap_or(0) as isize
            });
            ModuleApi {
                module,
                pub_fns,
                pub_types,
                pub_items,
                occurrence_median,
                zero_occurrences,
                params_median,
                delta_pub,
            }
        })
        .collect::<Vec<_>>();
    modules.sort_by(|left, right| {
        right
            .pub_items
            .cmp(&left.pub_items)
            .then_with(|| left.occurrence_median.total_cmp(&right.occurrence_median))
            .then_with(|| left.module.cmp(&right.module))
    });
    validate_requested_module(&modules, args.module.as_deref())?;
    scoped_single_caller_items.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.path.cmp(&right.1.path))
            .then_with(|| left.1.line.cmp(&right.1.line))
    });
    let single_caller_modules = single_caller_module_counts(&scoped_single_caller_items);
    let total_modules = modules.len();
    let total_single_caller_items = scoped_single_caller_items.len();
    let single_caller_items = select_single_caller_items(
        scoped_single_caller_items,
        args.module.as_deref(),
        args.top,
        args.json,
    );
    modules.truncate(args.top);
    Ok(Report {
        version: 1,
        verb: "api",
        path: args.path.clone(),
        requested_module: args.module.clone(),
        total_modules,
        modules,
        total_single_caller_items,
        single_caller_modules,
        single_caller_items,
        parse_failures: syntax.parse_failures.len(),
    })
}

fn zero_occurrence_count(items: &[ItemOccurrence]) -> usize {
    items.iter().filter(|item| item.occurrences == 0).count()
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

fn single_caller_module_counts(items: &[(String, ItemOccurrence)]) -> Vec<SingleCallerModule> {
    let mut counts = BTreeMap::<String, usize>::new();
    for (module, _) in items {
        *counts.entry(module.clone()).or_default() += 1;
    }
    let mut counts = counts
        .into_iter()
        .map(|(module, items)| SingleCallerModule { module, items })
        .collect::<Vec<_>>();
    counts.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| left.module.cmp(&right.module))
    });
    counts
}

fn select_single_caller_items(
    items: Vec<(String, ItemOccurrence)>,
    requested_module: Option<&str>,
    top: usize,
    json: bool,
) -> Vec<ItemOccurrence> {
    let mut selected = items
        .into_iter()
        .filter(|(module, _)| requested_module.is_none_or(|requested| module == requested))
        .map(|(_, item)| item)
        .collect::<Vec<_>>();
    if !json {
        selected.truncate(top);
    }
    selected
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

fn count_occurrences(
    item: &PubItem,
    defining_file: &FileSyntax,
    corpus: &OccurrenceCorpus,
) -> ItemOccurrence {
    let (occurrences, outside_modules) =
        corpus.count_from_module(&defining_file.module_path, &item.name);
    ItemOccurrence {
        module: defining_file.module_path.clone(),
        name: item.name.clone(),
        kind: item.kind.clone(),
        path: defining_file.path.clone(),
        line: item.line,
        occurrences,
        outside_modules,
    }
}

pub(super) struct OccurrenceCorpus {
    modules: BTreeMap<String, BTreeMap<String, usize>>,
}

impl OccurrenceCorpus {
    pub(super) fn new(sources: &[Source]) -> Self {
        let mut modules = BTreeMap::<String, BTreeMap<String, usize>>::new();
        for source in sources
            .iter()
            .filter(|source| !crate::source_files::is_test_file(&source.path))
        {
            let counts = modules
                .entry(crate_module_for_path(&source.path))
                .or_default();
            for identifier in production_prefix(&source.text)
                .split(|character: char| !is_identifier_character(character))
                .filter(|identifier| !identifier.is_empty())
            {
                *counts.entry(identifier.to_owned()).or_default() += 1;
            }
        }
        Self { modules }
    }

    pub(super) fn count_in_sources(sources: &[Source], symbol: &str) -> usize {
        Self::new(sources)
            .modules
            .values()
            .map(|counts| counts.get(symbol).copied().unwrap_or(0))
            .sum()
    }

    pub(super) fn count_from_module(&self, defining_module: &str, symbol: &str) -> (usize, usize) {
        let mut total = 0;
        let mut outside_modules = 0;
        for (module, counts) in &self.modules {
            if module == defining_module {
                continue;
            }
            let count = counts.get(symbol).copied().unwrap_or(0);
            if count > 0 {
                total += count;
                outside_modules += 1;
            }
        }
        (total, outside_modules)
    }
}

fn production_prefix(source: &str) -> &str {
    let Some(marker) = crate::source_files::inline_test_marker_line(source) else {
        return source;
    };
    if marker == 1 {
        return "";
    }
    let end = source
        .match_indices('\n')
        .nth(marker as usize - 2)
        .map_or(source.len(), |(index, _)| index + 1);
    &source[..end]
}

fn is_identifier_character(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
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
fn print_report(report: &Report, top: usize, requested_module: Option<&str>) {
    println!("Atlas api — {}", report.path.display());
    println!("module                    pub fn  pub type  occ/item  occ0  params/fn  Δpub");
    for module in report.modules.iter().take(top) {
        let params = module
            .params_median
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}"));
        let delta = module
            .delta_pub
            .map_or_else(|| "—".to_owned(), |value| format!("{value:+}"));
        println!(
            "{:<25} {:>6}  {:>8}  {:>8.1}  {:>4}  {:>9}  {:>4}",
            module.module,
            module.pub_fns,
            module.pub_types,
            module.occurrence_median,
            module.zero_occurrences,
            params,
            delta
        );
    }
    if report.total_modules > report.modules.len() {
        println!(
            "… and {} more modules",
            report.total_modules - report.modules.len()
        );
    }
    println!();
    if let Some(module) = requested_module {
        println!(
            "Single-outside-module public items in {module} (`occ` is identifier occurrences)"
        );
        for item in report.single_caller_items.iter().take(top) {
            println!(
                "{}::{} {}:{} occ {}",
                item.module,
                item.name,
                item.path.display(),
                item.line,
                item.occurrences
            );
        }
        let module_total = report
            .single_caller_modules
            .iter()
            .find(|row| row.module == module)
            .map_or(0, |row| row.items);
        if module_total > report.single_caller_items.len() {
            println!(
                "… and {} more single-outside-module items",
                module_total - report.single_caller_items.len()
            );
        }
    } else {
        println!("Single-outside-module public items by module");
        for module in report.single_caller_modules.iter().take(top) {
            println!("{}: {} items", module.module, module.items);
        }
        if report.single_caller_modules.len() > top {
            println!(
                "… and {} more modules",
                report.single_caller_modules.len() - top
            );
        }
    }
    println!(
        "total: {} modules, {} shortlist items, {} parse failures",
        report.total_modules, report.total_single_caller_items, report.parse_failures
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, occurrences: usize) -> ItemOccurrence {
        ItemOccurrence {
            module: "crate_module".to_owned(),
            name: name.to_owned(),
            kind: "fn".to_owned(),
            path: PathBuf::from(format!("src/{name}.rs")),
            line: 1,
            occurrences,
            outside_modules: 1,
        }
    }

    #[test]
    fn occurrence_scan_uses_identifier_boundaries() {
        let sources = vec![Source {
            path: PathBuf::from("src/caller.rs"),
            text: "run(); rerun(); run_again(); run".to_owned(),
        }];
        assert_eq!(
            OccurrenceCorpus::new(&sources).count_from_module("", "run"),
            (2, 1)
        );
    }

    #[test]
    fn occurrence_corpus_excludes_test_files_and_inline_test_modules() {
        let sources = vec![
            Source {
                path: PathBuf::from("src/lib.rs"),
                text: "pub fn target() {}\n".to_owned(),
            },
            Source {
                path: PathBuf::from("src/live.rs"),
                text: "fn caller() { target(); }\n#[cfg(test)]\nmod tests { fn check() { target(); } }\n"
                    .to_owned(),
            },
            Source {
                path: PathBuf::from("src/tests.rs"),
                text: "fn check() { target(); }\n".to_owned(),
            },
        ];
        assert_eq!(
            OccurrenceCorpus::new(&sources).count_from_module("", "target"),
            (1, 1)
        );
    }

    #[test]
    fn occurrence_count_in_sources_sums_only_the_supplied_sources() {
        let sources = vec![
            Source {
                path: PathBuf::from("src/cli/render.rs"),
                text: "target();".to_owned(),
            },
            Source {
                path: PathBuf::from("src/cli/render/table.rs"),
                text: "target(); target();".to_owned(),
            },
            Source {
                path: PathBuf::from("src/cli/renderer.rs"),
                text: "target(); target(); target();".to_owned(),
            },
        ];
        assert_eq!(OccurrenceCorpus::count_in_sources(&sources, "target"), 6);
    }

    #[test]
    fn api_args_reject_duplicates() {
        assert!(parse_args(&["--json".into(), "--json".into()]).is_err());
        let args = parse_args(&[
            "--top".into(),
            "7".into(),
            "--module".into(),
            "agents_cmd".into(),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(args.top, 7);
        assert_eq!(args.module.as_deref(), Some("agents_cmd"));
        assert!(
            parse_args(&[
                "--module".into(),
                "agents_cmd".into(),
                "--module".into(),
                "stats".into(),
            ])
            .is_err()
        );
    }

    #[test]
    fn api_shortlist_aggregates_by_scope_module() {
        let items = vec![
            ("stats".to_owned(), item("one", 1)),
            ("agents_cmd".to_owned(), item("two", 2)),
            ("stats".to_owned(), item("three", 3)),
        ];

        assert_eq!(
            single_caller_module_counts(&items),
            [
                SingleCallerModule {
                    module: "stats".to_owned(),
                    items: 2,
                },
                SingleCallerModule {
                    module: "agents_cmd".to_owned(),
                    items: 1,
                },
            ]
        );
    }

    #[test]
    fn api_occurrence_zeros_and_json_shortlist_are_untruncated() {
        let items = vec![item("zero", 0), item("used", 2), item("also-zero", 0)];
        assert_eq!(zero_occurrence_count(&items), 2);

        let scoped = items
            .into_iter()
            .map(|item| ("stats".to_owned(), item))
            .collect();
        assert_eq!(select_single_caller_items(scoped, None, 1, true).len(), 3);
    }

    #[test]
    fn api_module_drilldown_rejects_names_outside_the_table() {
        let modules = vec![ModuleApi {
            module: "agents_cmd".to_owned(),
            pub_fns: 1,
            pub_types: 0,
            pub_items: 1,
            occurrence_median: 1.0,
            zero_occurrences: 0,
            params_median: Some(0.0),
            delta_pub: None,
        }];

        assert!(validate_requested_module(&modules, Some("agents_cmd")).is_ok());
        let error = validate_requested_module(&modules, Some("missing")).unwrap_err();
        assert!(error.to_string().contains("module table"));
    }

    #[test]
    fn api_json_identifies_a_filtered_module_drilldown() {
        let report = Report {
            version: 1,
            verb: "api",
            path: PathBuf::from("src"),
            requested_module: Some("room".to_owned()),
            total_modules: 1,
            modules: Vec::new(),
            total_single_caller_items: 10,
            single_caller_modules: Vec::new(),
            single_caller_items: vec![item("filtered", 1)],
            parse_failures: 0,
        };

        let payload = serde_json::to_value(report).unwrap();
        assert_eq!(payload["requested_module"], "room");
        assert_eq!(payload["single_caller_items"].as_array().unwrap().len(), 1);
        assert_eq!(payload["total_single_caller_items"], 10);
    }
}
