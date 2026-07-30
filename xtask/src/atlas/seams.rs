use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::source_files;

use super::history::{self, CochangeEdge};
use super::modules::{crate_module_for_path, module_for_path};
use super::sources;
use super::syntax;
use super::{positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const DEFAULT_TOP: usize = 15;

const USAGE: &str = "cargo xtask atlas seams [--path <prefix>] [--top N] [--since <ref>] [--max-commit-files N] [--min-cochange N] [--json]

Imports come from Rust `use` items; inline fully-qualified paths are not counted.
Co-change omits commits broader than --max-commit-files.

  --path <path>          root-relative subtree (default crates/rimz/src)
  --top N                rows per section (default 15)
  --since <ref>          restrict co-change to <ref>..HEAD
  --max-commit-files N   omit broad commits (default 10)
  --min-cochange N       divergence threshold (default 3)
  --json                 versioned JSON agent contract (v1)";

#[derive(Debug)]
struct Args {
    path: PathBuf,
    top: usize,
    since: Option<String>,
    max_commit_files: usize,
    min_cochange: usize,
    json: bool,
}

#[derive(Clone, Debug, Serialize)]
struct ImportEdge {
    from: String,
    to: String,
    items: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ExternalSurface {
    module: String,
    outside: String,
    items: usize,
}

#[derive(Clone, Debug, Serialize)]
struct Divergence {
    kind: &'static str,
    left: String,
    right: String,
    imports: usize,
    cochanges: usize,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    path: PathBuf,
    import_edges: Vec<ImportEdge>,
    external_surface: Vec<ExternalSurface>,
    cochange_edges: Vec<CochangeEdge>,
    divergence: Vec<Divergence>,
    parse_failures: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas seams output is a command stdout contract"
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
            serde_json::to_string_pretty(&report).context("rendering atlas seams JSON")?
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
    let mut max_commit_files = None;
    let mut min_cochange = None;
    let mut json = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--path" => {
                let parsed = validate_scope(value(args, index, "seams", "--path")?, "--path")?;
                set_once(&mut path, parsed, "seams", "--path")?;
                index += 2;
            }
            "--top" => {
                let parsed =
                    positive_usize(value(args, index, "seams", "--top")?, "seams", "--top")?;
                set_once(&mut top, parsed, "seams", "--top")?;
                index += 2;
            }
            "--since" => {
                let reference = value(args, index, "seams", "--since")?.to_owned();
                set_once(&mut since, reference, "seams", "--since")?;
                index += 2;
            }
            "--max-commit-files" => {
                let parsed = positive_usize(
                    value(args, index, "seams", "--max-commit-files")?,
                    "seams",
                    "--max-commit-files",
                )?;
                set_once(&mut max_commit_files, parsed, "seams", "--max-commit-files")?;
                index += 2;
            }
            "--min-cochange" => {
                let parsed = positive_usize(
                    value(args, index, "seams", "--min-cochange")?,
                    "seams",
                    "--min-cochange",
                )?;
                set_once(&mut min_cochange, parsed, "seams", "--min-cochange")?;
                index += 2;
            }
            "--json" if !json => {
                json = true;
                index += 1;
            }
            "--json" => bail!("atlas seams --json may only be passed once"),
            _ => bail!("unknown atlas seams argument `{arg}`"),
        }
    }
    Ok(Some(Args {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        top: top.unwrap_or(DEFAULT_TOP),
        since,
        max_commit_files: max_commit_files.unwrap_or(10),
        min_cochange: min_cochange.unwrap_or(3),
        json,
    }))
}

fn build_report(root: &Path, args: &Args) -> Result<Report> {
    let sources = sources::scope_sources(root, &args.path, None)?;
    let syntax = syntax::analyze_sources(&sources);
    let known_modules = source_files::tracked_rust_files(root)?
        .into_iter()
        .filter_map(|path| path.strip_prefix(root).ok().map(crate_module_for_path))
        .collect::<BTreeSet<_>>();
    let scope_module = crate_module_for_path(&args.path.join("mod.rs"));
    let mut imports = BTreeMap::<(String, String), BTreeSet<String>>::new();
    for file in &syntax.files {
        let from = module_for_path(&file.path, &args.path);
        for imported in &file.imports {
            if !imported.internal {
                continue;
            }
            let imported_module = syntax::resolved_import_module(imported, &known_modules);
            let to = endpoint(&imported_module, &scope_module);
            if from != to {
                imports
                    .entry((from.clone(), to))
                    .or_default()
                    .insert(imported.item.clone());
            }
        }
    }
    let mut import_edges = imports
        .iter()
        .map(|((from, to), items)| ImportEdge {
            from: from.clone(),
            to: to.clone(),
            items: items.len(),
        })
        .collect::<Vec<_>>();
    import_edges.sort_by(|left, right| {
        right
            .items
            .cmp(&left.items)
            .then_with(|| left.from.cmp(&right.from))
            .then_with(|| left.to.cmp(&right.to))
    });
    let mut external_surface = import_edges
        .iter()
        .map(|edge| ExternalSurface {
            module: edge.from.clone(),
            outside: edge.to.clone(),
            items: edge.items,
        })
        .collect::<Vec<_>>();
    external_surface.sort_by(|left, right| {
        left.module
            .cmp(&right.module)
            .then_with(|| right.items.cmp(&left.items))
            .then_with(|| left.outside.cmp(&right.outside))
    });

    let cochange_edges = history::cochange(
        root,
        &args.path,
        args.since.as_deref(),
        args.max_commit_files,
    )?;
    let import_lookup = import_edges
        .iter()
        .map(|edge| ((edge.from.clone(), edge.to.clone()), edge.items))
        .collect::<BTreeMap<_, _>>();
    let cochange_lookup = cochange_edges
        .iter()
        .map(|edge| (ordered_pair(&edge.left, &edge.right), edge.commits))
        .collect::<BTreeMap<_, _>>();
    let mut divergence = Vec::new();
    for edge in &cochange_edges {
        let imports = import_lookup
            .get(&(edge.left.clone(), edge.right.clone()))
            .copied()
            .unwrap_or(0)
            + import_lookup
                .get(&(edge.right.clone(), edge.left.clone()))
                .copied()
                .unwrap_or(0);
        if edge.commits >= args.min_cochange && imports == 0 {
            divergence.push(Divergence {
                kind: "cochange-without-import",
                left: edge.left.clone(),
                right: edge.right.clone(),
                imports: 0,
                cochanges: edge.commits,
            });
        }
    }
    for edge in &import_edges {
        let cochanges = cochange_lookup
            .get(&ordered_pair(&edge.from, &edge.to))
            .copied()
            .unwrap_or(0);
        if cochanges == 0 {
            divergence.push(Divergence {
                kind: "import-without-cochange",
                left: edge.from.clone(),
                right: edge.to.clone(),
                imports: edge.items,
                cochanges: 0,
            });
        }
    }
    divergence.sort_by(|left, right| {
        right
            .cochanges
            .cmp(&left.cochanges)
            .then_with(|| right.imports.cmp(&left.imports))
            .then_with(|| left.left.cmp(&right.left))
            .then_with(|| left.right.cmp(&right.right))
    });
    Ok(Report {
        version: 1,
        verb: "seams",
        path: args.path.clone(),
        import_edges,
        external_surface,
        cochange_edges,
        divergence,
        parse_failures: syntax.parse_failures.len(),
    })
}

fn endpoint(module_path: &str, scope_module: &str) -> String {
    if module_path == scope_module {
        return "(root)".to_owned();
    }
    if let Some(relative) = module_path.strip_prefix(&format!("{scope_module}::")) {
        return relative.split("::").next().unwrap_or("(root)").to_owned();
    }
    module_path
        .split("::")
        .next()
        .unwrap_or("(root)")
        .to_owned()
}

fn ordered_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas seams report is the command's stdout contract"
)]
fn print_report(report: &Report, top: usize) {
    println!("Atlas seams — {}", report.path.display());
    println!();
    println!("Import edges (distinct imported items)");
    for edge in report.import_edges.iter().take(top) {
        println!("{} -> {}: {}", edge.from, edge.to, edge.items);
    }
    bounded_tail(report.import_edges.len(), top, "import edges");
    println!();
    println!("External surface");
    for surface in report.external_surface.iter().take(top) {
        println!(
            "{} -> {}: {} items",
            surface.module, surface.outside, surface.items
        );
    }
    bounded_tail(report.external_surface.len(), top, "surface rows");
    println!();
    println!("Co-change edges");
    for edge in report.cochange_edges.iter().take(top) {
        println!("{} <> {}: {} commits", edge.left, edge.right, edge.commits);
    }
    bounded_tail(report.cochange_edges.len(), top, "co-change edges");
    println!();
    println!("Divergence");
    for row in report.divergence.iter().take(top) {
        println!(
            "{} {} <> {}: imports {}, cochanges {}",
            row.kind, row.left, row.right, row.imports, row.cochanges
        );
    }
    bounded_tail(report.divergence.len(), top, "divergence rows");
    println!(
        "total: {} import edges, {} co-change edges, {} divergence rows, {} parse failures",
        report.import_edges.len(),
        report.cochange_edges.len(),
        report.divergence.len(),
        report.parse_failures
    );
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas bounded report is the command's stdout contract"
)]
fn bounded_tail(total: usize, top: usize, label: &str) {
    if total > top {
        println!("… and {} more {label}", total - top);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_follow_scope_granularity() {
        assert_eq!(endpoint("cli::agents_cmd::show", "cli"), "agents_cmd");
        assert_eq!(endpoint("store::event", "cli"), "store");
    }
}
