use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::modules::{crate_module_for_path, crate_module_for_row, module_for_path};
use super::sources::{self, Source};
use super::syntax::{self, FileSyntax, PubItem};
use super::{REPORT_VERSION, positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const DEFAULT_TOP: usize = 20;

const USAGE: &str =
    "cargo xtask atlas api [--path <prefix>] [--top N] [--module <name>] [--since <ref>] [--json]

Reports effective visibility and whole-word identifier name matches outside each
defining file-module. Production and test evidence are reported separately.
Common identifiers can over-count, so name matches are not resolved callers.

  --path <path>    root-relative subtree (default crates/rimz/src)
  --top N          rows per section (default 20)
  --module <name>  drill into one module from the module table
  --since <ref>    add public-item delta against a git revision
  --json           versioned JSON agent contract (v2)";

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
    declared_visibility: String,
    effective_reach: String,
    implied_reach: String,
    escapes_module: bool,
    over_published: bool,
    test_only: bool,
    production_name_matches: usize,
    production_name_modules: Vec<String>,
    test_name_matches: usize,
    test_name_modules: Vec<String>,
    #[serde(skip)]
    unreferenced: bool,
}

#[derive(Debug, Serialize)]
struct ModuleApi {
    module: String,
    items: usize,
    escaping_items: usize,
    over_published_items: usize,
    test_only_items: usize,
    unreferenced_items: usize,
    name_match_median: f64,
    params_median: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delta_pub: Option<isize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct SingleNameCallerModule {
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
    total_single_name_caller_items: usize,
    single_name_caller_modules: Vec<SingleNameCallerModule>,
    single_name_caller_items: Vec<ItemOccurrence>,
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
    let all_sources = sources::all_sources(root, None)?;
    let scoped_sources = sources::sources_in_scope(&all_sources, &args.path)?;
    let occurrence_corpus = OccurrenceCorpus::new(&all_sources);
    let all_syntax = syntax::analyze_sources(&all_sources);
    let mod_index = syntax::ModIndex::new(&all_syntax.files);
    let syntax = syntax::analyze_sources(&scoped_sources);
    let previous = args
        .since
        .as_deref()
        .map(|reference| {
            let all_sources = sources::all_sources(root, Some(reference))?;
            sources::sources_in_scope(&all_sources, &args.path)
        })
        .transpose()?
        .map(|sources| syntax::analyze_sources(&sources));
    let previous_counts = previous
        .as_ref()
        .map(|report| public_counts(&report.files, &args.path));

    let mut module_items = BTreeMap::<String, Vec<ItemOccurrence>>::new();
    let mut module_params = BTreeMap::<String, Vec<usize>>::new();
    for file in &syntax.files {
        let module = module_for_path(&file.path, &args.path);
        let mut occurrences = file
            .pub_items
            .iter()
            .map(|item| count_occurrences(item, file, &occurrence_corpus, &mod_index))
            .collect::<Vec<_>>();
        propagate_signature_reach(&file.pub_items, &mut occurrences);
        for item in &file.pub_items {
            if let Some(params) = item.params {
                module_params
                    .entry(module.clone())
                    .or_default()
                    .push(params);
            }
        }
        module_items.entry(module).or_default().extend(occurrences);
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
    let mut scoped_single_name_caller_items = Vec::new();
    let mut modules = module_items
        .into_iter()
        .map(|(module, items)| {
            let target_module = crate_module_for_row(&args.path, &module);
            scoped_single_name_caller_items.extend(
                items
                    .iter()
                    .filter(|item| item.production_name_modules.len() == 1)
                    .cloned()
                    .map(|item| (module.clone(), item)),
            );
            let item_count = items.len();
            let name_match_median = median(
                items
                    .iter()
                    .map(|item| item.production_name_matches)
                    .collect::<Vec<_>>(),
            );
            let params_median = module_params
                .get(&module)
                .map(|values| median(values.clone()));
            let delta_pub = previous_counts.as_ref().map(|counts| {
                item_count as isize - counts.get(&module).copied().unwrap_or(0) as isize
            });
            ModuleApi {
                module,
                items: item_count,
                escaping_items: items
                    .iter()
                    .filter(|item| !is_within(&item.effective_reach, &target_module))
                    .count(),
                over_published_items: items.iter().filter(|item| item.over_published).count(),
                test_only_items: items.iter().filter(|item| item.test_only).count(),
                unreferenced_items: items.iter().filter(|item| item.unreferenced).count(),
                name_match_median,
                params_median,
                delta_pub,
            }
        })
        .collect::<Vec<_>>();
    modules.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| left.name_match_median.total_cmp(&right.name_match_median))
            .then_with(|| left.module.cmp(&right.module))
    });
    validate_requested_module(&modules, args.module.as_deref())?;
    scoped_single_name_caller_items.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.path.cmp(&right.1.path))
            .then_with(|| left.1.line.cmp(&right.1.line))
    });
    let single_name_caller_modules =
        single_name_caller_module_counts(&scoped_single_name_caller_items);
    let total_modules = modules.len();
    let total_single_name_caller_items = scoped_single_name_caller_items.len();
    let single_name_caller_items = select_single_name_caller_items(
        scoped_single_name_caller_items,
        args.module.as_deref(),
        args.top,
        args.json,
    );
    modules.truncate(args.top);
    Ok(Report {
        version: REPORT_VERSION,
        verb: "api",
        path: args.path.clone(),
        requested_module: args.module.clone(),
        total_modules,
        modules,
        module_items: requested_module_items,
        total_single_name_caller_items,
        single_name_caller_modules,
        single_name_caller_items,
        parse_failures: syntax.parse_failures.len(),
    })
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

fn single_name_caller_module_counts(
    items: &[(String, ItemOccurrence)],
) -> Vec<SingleNameCallerModule> {
    let mut counts = BTreeMap::<String, usize>::new();
    for (module, _) in items {
        *counts.entry(module.clone()).or_default() += 1;
    }
    let mut counts = counts
        .into_iter()
        .map(|(module, items)| SingleNameCallerModule { module, items })
        .collect::<Vec<_>>();
    counts.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| left.module.cmp(&right.module))
    });
    counts
}

fn select_single_name_caller_items(
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
    mod_index: &syntax::ModIndex,
) -> ItemOccurrence {
    let production = corpus.production_from_module(&defining_file.module_path, &item.name);
    let test = corpus.test_matches(&item.name);
    let implied_reach = common_reach(
        std::iter::once(item.module.as_str()).chain(production.modules.iter().map(String::as_str)),
    );
    let effective_reach = mod_index.effective_reach(&item.module, &item.reach);
    let over_published = is_strictly_broader(&effective_reach, &implied_reach);
    ItemOccurrence {
        module: item.module.clone(),
        name: item.name.clone(),
        kind: item.kind.clone(),
        path: defining_file.path.clone(),
        line: item.line,
        declared_visibility: item.declared.clone(),
        escapes_module: effective_reach != item.module,
        effective_reach,
        implied_reach,
        over_published,
        test_only: production.matches == 0 && test.matches > 0,
        production_name_matches: production.matches,
        production_name_modules: production.modules,
        test_name_matches: test.matches,
        test_name_modules: test.modules,
        unreferenced: production.matches == 0 && test.matches == 0,
    }
}

fn propagate_signature_reach(items: &[PubItem], occurrences: &mut [ItemOccurrence]) {
    let targets = items.iter().enumerate().fold(
        BTreeMap::<&str, Vec<usize>>::new(),
        |mut targets, (index, item)| {
            targets.entry(&item.name).or_default().push(index);
            targets
        },
    );
    loop {
        let previous = occurrences
            .iter()
            .map(|item| item.implied_reach.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for (source_index, item) in items.iter().enumerate() {
            for target_index in item
                .signature_names
                .iter()
                .filter_map(|name| targets.get(name.as_str()))
                .flatten()
                .copied()
            {
                let reach = common_reach(
                    [
                        occurrences[target_index].implied_reach.as_str(),
                        previous[source_index].as_str(),
                    ]
                    .into_iter(),
                );
                if reach != occurrences[target_index].implied_reach {
                    occurrences[target_index].implied_reach = reach;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    for item in occurrences {
        item.over_published = is_strictly_broader(&item.effective_reach, &item.implied_reach);
    }
}

fn common_reach<'a>(modules: impl Iterator<Item = &'a str>) -> String {
    let mut modules = modules;
    let Some(first) = modules.next() else {
        return String::new();
    };
    let mut common = first.split("::").collect::<Vec<_>>();
    for module in modules {
        let parts = module.split("::").collect::<Vec<_>>();
        common.truncate(
            common
                .iter()
                .zip(parts)
                .take_while(|(left, right)| **left == *right)
                .count(),
        );
    }
    common.join("::")
}

fn is_strictly_broader(reach: &str, required: &str) -> bool {
    reach != required
        && (reach.is_empty() || required == reach || required.starts_with(&format!("{reach}::")))
}

fn is_within(module: &str, ancestor: &str) -> bool {
    ancestor.is_empty() || module == ancestor || module.starts_with(&format!("{ancestor}::"))
}

#[derive(Debug)]
struct NameMatches {
    matches: usize,
    modules: Vec<String>,
}

pub(super) struct OccurrenceCorpus {
    production: BTreeMap<String, BTreeMap<String, usize>>,
    tests: BTreeMap<String, BTreeMap<String, usize>>,
}

impl OccurrenceCorpus {
    pub(super) fn new(sources: &[Source]) -> Self {
        let mut production = BTreeMap::<String, BTreeMap<String, usize>>::new();
        let mut tests = BTreeMap::<String, BTreeMap<String, usize>>::new();
        let syntax = syntax::analyze_sources(sources);
        for source in sources {
            let module = crate_module_for_path(&source.path);
            if !source.is_production() {
                add_identifiers(tests.entry(module).or_default(), &source.text);
                continue;
            }
            let test_regions = syntax
                .files
                .iter()
                .find(|file| file.path == source.path)
                .map_or(&[][..], |file| file.test_regions.as_slice());
            let mut production_text = String::new();
            let mut test_text = String::new();
            for (index, line) in source.text.split_inclusive('\n').enumerate() {
                let target = if test_regions
                    .iter()
                    .any(|region| region.contains(&(index + 1)))
                {
                    &mut test_text
                } else {
                    &mut production_text
                };
                target.push_str(line);
            }
            add_identifiers(
                production.entry(module.clone()).or_default(),
                &production_text,
            );
            add_identifiers(tests.entry(module).or_default(), &test_text);
        }
        Self { production, tests }
    }

    pub(super) fn count_in_sources(sources: &[Source], symbol: &str) -> usize {
        Self::new(sources)
            .production
            .values()
            .map(|counts| counts.get(symbol).copied().unwrap_or(0))
            .sum()
    }

    pub(super) fn count_from_module(&self, defining_module: &str, symbol: &str) -> (usize, usize) {
        let matches = self.production_from_module(defining_module, symbol);
        (matches.matches, matches.modules.len())
    }

    fn production_from_module(&self, defining_module: &str, symbol: &str) -> NameMatches {
        self.matches(&self.production, symbol, Some(defining_module))
    }

    fn test_matches(&self, symbol: &str) -> NameMatches {
        self.matches(&self.tests, symbol, None)
    }

    fn matches(
        &self,
        corpus: &BTreeMap<String, BTreeMap<String, usize>>,
        symbol: &str,
        excluded_module: Option<&str>,
    ) -> NameMatches {
        let mut total = 0;
        let mut modules = Vec::new();
        for (module, counts) in corpus {
            if excluded_module == Some(module.as_str()) {
                continue;
            }
            let count = counts.get(symbol).copied().unwrap_or(0);
            if count > 0 {
                total += count;
                modules.push(module.clone());
            }
        }
        NameMatches {
            matches: total,
            modules,
        }
    }
}

fn add_identifiers(counts: &mut BTreeMap<String, usize>, text: &str) {
    for identifier in text
        .split(|character: char| !is_identifier_character(character))
        .filter(|identifier| !identifier.is_empty())
    {
        *counts.entry(identifier.to_owned()).or_default() += 1;
    }
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
fn print_report(report: &Report, top: usize, requested_module: Option<&str>, show_delta: bool) {
    println!("Atlas api — {}", report.path.display());
    println!("{}", module_header(show_delta));
    for module in report.modules.iter().take(top) {
        let params = module
            .params_median
            .map_or_else(|| "—".to_owned(), |value| format!("{value:.1}"));
        if show_delta {
            println!(
                "{:<25} {:>5} {:>5} {:>5} {:>9} {:>5} {:>8.1} {:>9} {:+4}",
                module.module,
                module.items,
                module.escaping_items,
                module.over_published_items,
                module.test_only_items,
                module.unreferenced_items,
                module.name_match_median,
                params,
                module.delta_pub.unwrap_or(0)
            );
        } else {
            println!(
                "{:<25} {:>5} {:>5} {:>5} {:>9} {:>5} {:>8.1} {:>9}",
                module.module,
                module.items,
                module.escaping_items,
                module.over_published_items,
                module.test_only_items,
                module.unreferenced_items,
                module.name_match_median,
                params
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
        println!("Public items in {module} (name matches are heuristic evidence)");
        for item in report.module_items.iter().take(top) {
            println!(
                "{}::{} ({}) {}:{} declared {} reach {} implied {} prod {} test {} [{}]",
                item.module,
                item.name,
                item.kind,
                item.path.display(),
                item.line,
                item.declared_visibility,
                display_reach(&item.effective_reach),
                display_reach(&item.implied_reach),
                item.production_name_matches,
                item.test_name_matches,
                item_tags(item),
            );
        }
        if report.module_items.len() > top {
            println!(
                "… and {} more public items (use --json for the complete list)",
                report.module_items.len() - top
            );
        }
        println!();
        println!("Single name-caller module public items in {module}");
        for item in report.single_name_caller_items.iter().take(top) {
            println!(
                "{}::{} {}:{} production name matches {}",
                item.module,
                item.name,
                item.path.display(),
                item.line,
                item.production_name_matches
            );
        }
        let module_total = report
            .single_name_caller_modules
            .iter()
            .find(|row| row.module == module)
            .map_or(0, |row| row.items);
        if module_total > report.single_name_caller_items.len() {
            println!(
                "… and {} more single name-caller module items",
                module_total - report.single_name_caller_items.len()
            );
        }
    } else {
        println!("Single name-caller module public items by module");
        for module in report.single_name_caller_modules.iter().take(top) {
            println!("{}: {} items", module.module, module.items);
        }
        if report.single_name_caller_modules.len() > top {
            println!(
                "… and {} more modules",
                report.single_name_caller_modules.len() - top
            );
        }
    }
    println!(
        "total: {} modules, {} shortlist items, {} parse failures",
        report.total_modules, report.total_single_name_caller_items, report.parse_failures
    );
}

fn module_header(show_delta: bool) -> &'static str {
    if show_delta {
        "module                    items   esc  over test-only unref name-occ params/fn Δpub"
    } else {
        "module                    items   esc  over test-only unref name-occ params/fn"
    }
}

fn display_reach(reach: &str) -> &str {
    if reach.is_empty() { "crate" } else { reach }
}

fn item_tags(item: &ItemOccurrence) -> String {
    let mut tags = Vec::new();
    if item.escapes_module {
        tags.push("escapes-module");
    }
    if item.over_published {
        tags.push("over-published");
    }
    if item.test_only {
        tags.push("test-only");
    }
    if item.unreferenced {
        tags.push("unreferenced");
    }
    if tags.is_empty() {
        "—".to_owned()
    } else {
        tags.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, production_name_matches: usize) -> ItemOccurrence {
        ItemOccurrence {
            module: "crate_module".to_owned(),
            name: name.to_owned(),
            kind: "fn".to_owned(),
            path: PathBuf::from(format!("src/{name}.rs")),
            line: 1,
            declared_visibility: "pub(super)".to_owned(),
            effective_reach: "crate_module".to_owned(),
            implied_reach: "crate_module".to_owned(),
            escapes_module: false,
            over_published: false,
            test_only: false,
            production_name_matches,
            production_name_modules: vec!["caller".to_owned()],
            test_name_matches: 0,
            test_name_modules: Vec::new(),
            unreferenced: production_name_matches == 0,
        }
    }

    #[test]
    fn occurrence_scan_uses_identifier_boundaries() {
        let sources = vec![Source::new(
            "src/caller.rs",
            "run(); rerun(); run_again(); run",
        )];
        assert_eq!(
            OccurrenceCorpus::new(&sources).count_from_module("", "run"),
            (2, 1)
        );
    }

    #[test]
    fn api_header_only_advertises_requested_delta() {
        assert!(!module_header(false).contains("Δpub"));
        assert!(module_header(true).ends_with("Δpub"));
    }

    #[test]
    fn occurrence_corpus_splits_production_and_test_name_evidence() {
        let sources = vec![
            Source::new("src/lib.rs", "pub fn target() {}\n"),
            Source::new(
                "src/live.rs",
                "fn caller() { target(); }\n#[cfg(test)]\nmod tests { fn check() { target(); } }\n",
            ),
            Source::new("src/tests.rs", "fn check() { target(); }\n"),
        ];
        let corpus = OccurrenceCorpus::new(&sources);
        assert_eq!(corpus.count_from_module("", "target"), (1, 1));
        let tests = corpus.test_matches("target");
        assert_eq!(tests.matches, 2);
        assert_eq!(tests.modules.len(), 2);
    }

    #[test]
    fn occurrence_count_in_sources_sums_only_the_supplied_sources() {
        let sources = vec![
            Source::new("src/cli/render.rs", "target();"),
            Source::new("src/cli/render/table.rs", "target(); target();"),
            Source::new("src/cli/renderer.rs", "target(); target(); target();"),
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
            single_name_caller_module_counts(&items),
            [
                SingleNameCallerModule {
                    module: "stats".to_owned(),
                    items: 2,
                },
                SingleNameCallerModule {
                    module: "agents_cmd".to_owned(),
                    items: 1,
                },
            ]
        );
    }

    #[test]
    fn api_json_shortlist_is_untruncated() {
        let items = vec![item("zero", 0), item("used", 2), item("also-zero", 0)];
        let scoped = items
            .into_iter()
            .map(|item| ("stats".to_owned(), item))
            .collect();
        assert_eq!(
            select_single_name_caller_items(scoped, None, 1, true).len(),
            3
        );
    }

    #[test]
    fn api_module_drilldown_rejects_names_outside_the_table() {
        let modules = vec![ModuleApi {
            module: "agents_cmd".to_owned(),
            items: 1,
            escaping_items: 0,
            over_published_items: 0,
            test_only_items: 0,
            unreferenced_items: 0,
            name_match_median: 1.0,
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
            version: REPORT_VERSION,
            verb: "api",
            path: PathBuf::from("src"),
            requested_module: Some("room".to_owned()),
            total_modules: 1,
            modules: Vec::new(),
            module_items: vec![item("all-items", 0)],
            total_single_name_caller_items: 10,
            single_name_caller_modules: Vec::new(),
            single_name_caller_items: vec![item("filtered", 1)],
            parse_failures: 0,
        };

        let payload = serde_json::to_value(report).unwrap();
        assert_eq!(payload["requested_module"], "room");
        assert_eq!(payload["module_items"].as_array().unwrap().len(), 1);
        assert_eq!(payload["version"], 2);
        assert_eq!(
            payload["single_name_caller_items"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(payload["total_single_name_caller_items"], 10);
        let api_item = &payload["module_items"][0];
        assert!(api_item.get("effective_reach").is_some());
        assert!(api_item.get("production_name_matches").is_some());
        assert!(api_item.get("occurrences").is_none());
        assert!(api_item.get("outside_modules").is_none());
        assert!(api_item.get("zero_occurrences").is_none());
    }

    #[test]
    fn signature_types_inherit_the_functions_implied_reach() {
        let sources = vec![
            Source::new(
                "crates/rimz/src/cli/demo.rs",
                "pub(super) struct AgentListReport;\npub(super) fn build_list_report() -> AgentListReport { AgentListReport }\n",
            ),
            Source::new(
                "crates/rimz/src/cli/caller.rs",
                "fn call() { build_list_report(); }\n",
            ),
        ];
        let syntax = syntax::analyze_sources(&sources);
        let file = syntax
            .files
            .iter()
            .find(|file| file.module_path == "cli::demo")
            .unwrap();
        let corpus = OccurrenceCorpus::new(&sources);
        let index = syntax::ModIndex::new(&syntax.files);
        let mut occurrences = file
            .pub_items
            .iter()
            .map(|item| count_occurrences(item, file, &corpus, &index))
            .collect::<Vec<_>>();
        propagate_signature_reach(&file.pub_items, &mut occurrences);
        let report_type = occurrences
            .iter()
            .find(|item| item.name == "AgentListReport")
            .unwrap();
        assert_eq!(report_type.implied_reach, "cli");
    }

    #[test]
    fn unused_and_test_only_items_have_distinct_tags() {
        let sources = vec![
            Source::new(
                "crates/rimz/src/demo.rs",
                "pub(super) fn only_tested() {}\npub(super) fn unused() {}\n",
            ),
            Source::new(
                "crates/rimz/src/tests.rs",
                "fn check() { only_tested(); }\n",
            ),
        ];
        let syntax = syntax::analyze_sources(&sources);
        let file = &syntax.files[0];
        let corpus = OccurrenceCorpus::new(&sources);
        let index = syntax::ModIndex::new(&syntax.files);
        let occurrences = file
            .pub_items
            .iter()
            .map(|item| count_occurrences(item, file, &corpus, &index))
            .collect::<Vec<_>>();
        let only_tested = occurrences
            .iter()
            .find(|item| item.name == "only_tested")
            .unwrap();
        let unused = occurrences
            .iter()
            .find(|item| item.name == "unused")
            .unwrap();
        assert!(only_tested.test_only);
        assert!(!only_tested.unreferenced);
        assert!(!unused.test_only);
        assert!(unused.unreferenced);
    }

    #[test]
    fn reexports_and_common_names_are_conservative_name_evidence() {
        let sources = vec![
            Source::new("crates/rimz/src/lib.rs", "pub use inner::Thing;\n"),
            Source::new("crates/rimz/src/inner.rs", "pub struct Thing;\n"),
            Source::new(
                "crates/rimz/src/other.rs",
                "// Thing also appears in comments and strings\nconst TEXT: &str = \"Thing\";\n",
            ),
        ];
        let corpus = OccurrenceCorpus::new(&sources);
        let evidence = corpus.production_from_module("inner", "Thing");
        assert_eq!(evidence.matches, 3);
        assert_eq!(
            common_reach(
                std::iter::once("inner").chain(evidence.modules.iter().map(String::as_str))
            ),
            ""
        );
    }
}
