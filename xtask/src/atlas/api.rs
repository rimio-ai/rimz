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

const USAGE: &str = "cargo xtask atlas api [--path <prefix>] [--top N] [--since <ref>] [--json]

Reports public boundary shape and whole-word identifier occurrences outside each
defining module. `occ` is a ranking heuristic, not a resolved caller count.

  --path <path>  root-relative subtree (default crates/rimz/src)
  --top N        rows per shortlist (default 20)
  --since <ref>  add public-item delta against a git revision
  --json         versioned JSON agent contract (v1)";

#[derive(Debug, PartialEq, Eq)]
struct Args {
    path: PathBuf,
    top: usize,
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
    params_median: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_pub: Option<isize>,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    path: PathBuf,
    total_modules: usize,
    modules: Vec<ModuleApi>,
    total_single_caller_items: usize,
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
        print_report(&report, args.top);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut path = None;
    let mut top = None;
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

    let mut single_caller_items = Vec::new();
    let mut modules = module_items
        .into_iter()
        .map(|(module, items)| {
            single_caller_items.extend(
                items
                    .iter()
                    .filter(|item| item.outside_modules == 1)
                    .cloned(),
            );
            let (pub_fns, pub_types) = module_kinds.get(&module).copied().unwrap_or_default();
            let pub_items = items.len();
            let occurrence_median = median(
                items
                    .iter()
                    .map(|item| item.occurrences)
                    .collect::<Vec<_>>(),
            );
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
    single_caller_items.sort_by(|left, right| {
        left.module
            .cmp(&right.module)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    let total_modules = modules.len();
    let total_single_caller_items = single_caller_items.len();
    modules.truncate(args.top);
    single_caller_items.truncate(args.top);
    Ok(Report {
        version: 1,
        verb: "api",
        path: args.path.clone(),
        total_modules,
        modules,
        total_single_caller_items,
        single_caller_items,
        parse_failures: syntax.parse_failures.len(),
    })
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

    pub(super) fn count_in_module(&self, module: &str, symbol: &str) -> usize {
        self.modules
            .get(module)
            .and_then(|counts| counts.get(symbol))
            .copied()
            .unwrap_or(0)
    }

    pub(super) fn count_under(&self, module_prefix: &str, symbol: &str) -> usize {
        self.modules
            .iter()
            .filter(|(module, _)| {
                module_prefix.is_empty()
                    || *module == module_prefix
                    || module.starts_with(&format!("{module_prefix}::"))
            })
            .map(|(_, counts)| counts.get(symbol).copied().unwrap_or(0))
            .sum()
    }

    fn count_from_module(&self, defining_module: &str, symbol: &str) -> (usize, usize) {
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

fn median(mut values: Vec<usize>) -> f64 {
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
fn print_report(report: &Report, top: usize) {
    println!("Atlas api — {}", report.path.display());
    println!("module                    pub fn  pub type  occ/item  params/fn  Δpub");
    for module in report.modules.iter().take(top) {
        let params = module
            .params_median
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}"));
        let delta = module
            .delta_pub
            .map_or_else(|| "—".to_owned(), |value| format!("{value:+}"));
        println!(
            "{:<25} {:>6}  {:>8}  {:>8.1}  {:>9}  {:>4}",
            module.module,
            module.pub_fns,
            module.pub_types,
            module.occurrence_median,
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
    println!("Single-outside-module public items (`occ` is identifier occurrences)");
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
    if report.total_single_caller_items > report.single_caller_items.len() {
        println!(
            "… and {} more single-outside-module items",
            report.total_single_caller_items - report.single_caller_items.len()
        );
    }
    println!(
        "total: {} modules, {} shortlist items, {} parse failures",
        report.total_modules, report.total_single_caller_items, report.parse_failures
    );
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn occurrence_scope_distinguishes_files_directories_and_prefix_collisions() {
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
        let corpus = OccurrenceCorpus::new(&sources);

        assert_eq!(corpus.count_in_module("cli::render", "target"), 1);
        assert_eq!(corpus.count_under("cli::render", "target"), 3);
        assert_eq!(corpus.count_under("", "target"), 6);
    }

    #[test]
    fn api_args_reject_duplicates() {
        assert!(parse_args(&["--json".into(), "--json".into()]).is_err());
        assert_eq!(
            parse_args(&["--top".into(), "7".into()])
                .unwrap()
                .unwrap()
                .top,
            7
        );
    }
}
