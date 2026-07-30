use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::sources;
use super::syntax::{self, FnBody};
use super::{finite_nonnegative, positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";

const USAGE: &str =
    "cargo xtask atlas shapes [--path <prefix>] [--top N] [--min-sloc N] [--similarity S] [--json]

Clusters large functions by Jaccard similarity over normalized control-flow
4-grams. Identifiers and literals do not enter the skeleton.

  --path <path>   root-relative subtree (default crates/rimz/src)
  --top N         clusters to report (default 15)
  --min-sloc N    minimum function source lines (default 40)
  --similarity S  Jaccard threshold from 0 through 1 (default 0.75)
  --json          versioned JSON agent contract (v1)";

#[derive(Debug)]
struct Args {
    path: PathBuf,
    top: usize,
    min_sloc: usize,
    similarity: f64,
    json: bool,
}

#[derive(Clone, Debug, Serialize)]
struct Member {
    path: PathBuf,
    line: usize,
    name: String,
    sloc: usize,
}

#[derive(Clone, Debug, Serialize)]
struct Cluster {
    similarity_floor: f64,
    mean_sloc: f64,
    score: f64,
    members: Vec<Member>,
}

#[derive(Debug, Serialize)]
struct Report {
    version: u8,
    verb: &'static str,
    path: PathBuf,
    eligible_functions: usize,
    total_clusters: usize,
    clusters: Vec<Cluster>,
    parse_failures: usize,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas shapes output is a command stdout contract"
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
            serde_json::to_string_pretty(&report).context("rendering atlas shapes JSON")?
        );
    } else {
        print_report(&report);
    }
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut path = None;
    let mut top = None;
    let mut min_sloc = None;
    let mut similarity = None;
    let mut json = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--path" => {
                let parsed = validate_scope(value(args, index, "shapes", "--path")?, "--path")?;
                set_once(&mut path, parsed, "shapes", "--path")?;
                index += 2;
            }
            "--top" => {
                let parsed =
                    positive_usize(value(args, index, "shapes", "--top")?, "shapes", "--top")?;
                set_once(&mut top, parsed, "shapes", "--top")?;
                index += 2;
            }
            "--min-sloc" => {
                let parsed = positive_usize(
                    value(args, index, "shapes", "--min-sloc")?,
                    "shapes",
                    "--min-sloc",
                )?;
                set_once(&mut min_sloc, parsed, "shapes", "--min-sloc")?;
                index += 2;
            }
            "--similarity" => {
                let parsed = finite_nonnegative(
                    value(args, index, "shapes", "--similarity")?,
                    "shapes",
                    "--similarity",
                )?;
                if parsed > 1.0 {
                    bail!("atlas shapes --similarity must not exceed 1");
                }
                set_once(&mut similarity, parsed, "shapes", "--similarity")?;
                index += 2;
            }
            "--json" if !json => {
                json = true;
                index += 1;
            }
            "--json" => bail!("atlas shapes --json may only be passed once"),
            _ => bail!("unknown atlas shapes argument `{arg}`"),
        }
    }
    Ok(Some(Args {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        top: top.unwrap_or(15),
        min_sloc: min_sloc.unwrap_or(40),
        similarity: similarity.unwrap_or(0.75),
        json,
    }))
}

fn build_report(root: &Path, args: &Args) -> Result<Report> {
    let sources = sources::scope_sources(root, &args.path, None)?;
    let syntax = syntax::analyze_sources(&sources);
    let functions = syntax
        .files
        .iter()
        .flat_map(|file| &file.fns)
        .filter(|function| function.sloc >= args.min_sloc)
        .cloned()
        .collect::<Vec<_>>();
    let shingles = functions
        .iter()
        .map(|function| shingle_set(&function.skeleton))
        .collect::<Vec<_>>();
    let mut union = UnionFind::new(functions.len());
    for left in 0..functions.len() {
        for right in left + 1..functions.len() {
            let similarity = jaccard(&shingles[left], &shingles[right]);
            if similarity >= args.similarity {
                union.join(left, right);
            }
        }
    }
    let mut groups = BTreeMap::<usize, Vec<usize>>::new();
    for index in 0..functions.len() {
        groups.entry(union.find(index)).or_default().push(index);
    }
    let mut clusters = groups
        .into_values()
        .filter(|members| members.len() > 1)
        .map(|members| cluster(&functions, &shingles, members))
        .collect::<Vec<_>>();
    clusters.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.members[0].path.cmp(&right.members[0].path))
            .then_with(|| left.members[0].line.cmp(&right.members[0].line))
    });
    let total_clusters = clusters.len();
    clusters.truncate(args.top);
    Ok(Report {
        version: 1,
        verb: "shapes",
        path: args.path.clone(),
        eligible_functions: functions.len(),
        total_clusters,
        clusters,
        parse_failures: syntax.parse_failures.len(),
    })
}

fn cluster(
    functions: &[FnBody],
    shingles: &[BTreeSet<Vec<String>>],
    indexes: Vec<usize>,
) -> Cluster {
    let mean_sloc = indexes
        .iter()
        .map(|index| functions[*index].sloc)
        .sum::<usize>() as f64
        / indexes.len() as f64;
    let similarity_floor = indexes
        .iter()
        .enumerate()
        .flat_map(|(position, left)| {
            indexes[position + 1..]
                .iter()
                .map(move |right| jaccard(&shingles[*left], &shingles[*right]))
        })
        .fold(1.0_f64, f64::min);
    let mut members = indexes
        .into_iter()
        .map(|index| {
            let function = &functions[index];
            Member {
                path: function.path.clone(),
                line: function.line,
                name: function.name.clone(),
                sloc: function.sloc,
            }
        })
        .collect::<Vec<_>>();
    members.sort_by(|left, right| {
        right
            .sloc
            .cmp(&left.sloc)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    Cluster {
        similarity_floor,
        mean_sloc,
        score: members.len() as f64 * mean_sloc,
        members,
    }
}

fn shingle_set(tokens: &[String]) -> BTreeSet<Vec<String>> {
    tokens
        .windows(4)
        .map(|window| window.to_vec())
        .collect::<BTreeSet<_>>()
}

fn jaccard(left: &BTreeSet<Vec<String>>, right: &BTreeSet<Vec<String>>) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    let union = left.len() + right.len() - intersection;
    intersection as f64 / union as f64
}

struct UnionFind {
    parents: Vec<usize>,
}

impl UnionFind {
    fn new(len: usize) -> Self {
        Self {
            parents: (0..len).collect(),
        }
    }

    fn find(&mut self, index: usize) -> usize {
        if self.parents[index] != index {
            self.parents[index] = self.find(self.parents[index]);
        }
        self.parents[index]
    }

    fn join(&mut self, left: usize, right: usize) {
        let left = self.find(left);
        let right = self.find(right);
        if left != right {
            self.parents[right] = left;
        }
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas shapes report is the command's stdout contract"
)]
fn print_report(report: &Report) {
    println!("Atlas shapes — {}", report.path.display());
    for (index, cluster) in report.clusters.iter().enumerate() {
        println!(
            "{}. {} members, mean {:.1} sloc, similarity floor {:.2}",
            index + 1,
            cluster.members.len(),
            cluster.mean_sloc,
            cluster.similarity_floor
        );
        for member in cluster.members.iter().take(5) {
            println!(
                "   {}:{} {} {} sloc",
                member.path.display(),
                member.line,
                member.name,
                member.sloc
            );
        }
        if cluster.members.len() > 5 {
            println!("   … and {} more members", cluster.members.len() - 5);
        }
    }
    if report.total_clusters > report.clusters.len() {
        println!(
            "… and {} more clusters",
            report.total_clusters - report.clusters.len()
        );
    }
    println!(
        "total: {} eligible functions, {} clusters, {} parse failures",
        report.eligible_functions, report.total_clusters, report.parse_failures
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<Vec<String>> {
        items
            .iter()
            .map(|item| item.split_whitespace().map(str::to_owned).collect())
            .collect()
    }

    #[test]
    fn jaccard_and_union_find_cluster_shapes() {
        assert_eq!(jaccard(&set(&["A B C D"]), &set(&["A B C D"])), 1.0);
        assert_eq!(
            jaccard(&set(&["A B C D", "B C D E"]), &set(&["A B C D"])),
            0.5
        );
        let mut union = UnionFind::new(3);
        union.join(0, 1);
        assert_eq!(union.find(0), union.find(1));
        assert_ne!(union.find(0), union.find(2));
    }
}
