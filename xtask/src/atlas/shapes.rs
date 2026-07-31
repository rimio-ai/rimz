use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use super::modules::module_for_path;
use super::sources;
use super::syntax::{self, FnBody};
use super::{finite_nonnegative, positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const MIN_SHARED_CALLEES: usize = 3;

const USAGE: &str =
    "cargo xtask atlas shapes [--path <prefix>] [--top N] [--min-sloc N] [--similarity S] [--json]

Clusters large functions by Jaccard similarity over the functions and methods
they call. Generic iterator, conversion, and error-context methods are omitted,
and a pair must share at least three callees, so the report highlights shared
domain choreography rather than incidental loop shape.
Functions with parallel control flow but different callees do not cluster;
use `rank --verbose` to find those large entry points.
Clusters spanning more modules and files rank before member-count × mean-sloc.

  --path <path>   root-relative subtree (default crates/rimz/src)
  --top N         clusters to report (default 10)
  --min-sloc N    minimum function source lines (default 40)
  --similarity S  Jaccard threshold from 0 through 1 (default 0.35)
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
    distinct_files: usize,
    distinct_modules: usize,
    shared_callees: Vec<String>,
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
        top: top.unwrap_or(10),
        min_sloc: min_sloc.unwrap_or(40),
        similarity: similarity.unwrap_or(0.35),
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
    let callees = functions
        .iter()
        .map(|function| callee_set(&function.callees))
        .collect::<Vec<_>>();
    let mut similarities = vec![vec![0.0; functions.len()]; functions.len()];
    for left in 0..functions.len() {
        for right in left + 1..functions.len() {
            let similarity = callee_similarity(&callees[left], &callees[right]);
            similarities[left][right] = similarity;
            similarities[right][left] = similarity;
        }
    }
    let mut clusters = complete_linkage_groups(&similarities, args.similarity)
        .into_iter()
        .filter(|members| members.len() > 1)
        .map(|members| cluster(&functions, &callees, members, &args.path))
        .collect::<Vec<_>>();
    sort_clusters(&mut clusters);
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

fn sort_clusters(clusters: &mut [Cluster]) {
    clusters.sort_by(|left, right| {
        right
            .distinct_modules
            .cmp(&left.distinct_modules)
            .then_with(|| right.distinct_files.cmp(&left.distinct_files))
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.members[0].path.cmp(&right.members[0].path))
            .then_with(|| left.members[0].line.cmp(&right.members[0].line))
    });
}

fn cluster(
    functions: &[FnBody],
    callees: &[BTreeSet<String>],
    indexes: Vec<usize>,
    scope: &Path,
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
                .map(move |right| callee_similarity(&callees[*left], &callees[*right]))
        })
        .fold(1.0_f64, f64::min);
    let shared_callees = indexes
        .iter()
        .skip(1)
        .fold(callees[indexes[0]].clone(), |shared, index| {
            shared.intersection(&callees[*index]).cloned().collect()
        })
        .into_iter()
        .collect();
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
    let distinct_files = members
        .iter()
        .map(|member| &member.path)
        .collect::<BTreeSet<_>>()
        .len();
    let distinct_modules = members
        .iter()
        .map(|member| module_for_path(&member.path, scope))
        .collect::<BTreeSet<_>>()
        .len();
    Cluster {
        similarity_floor,
        mean_sloc,
        score: members.len() as f64 * mean_sloc,
        distinct_files,
        distinct_modules,
        shared_callees,
        members,
    }
}

fn callee_set(callees: &[String]) -> BTreeSet<String> {
    callees
        .iter()
        .filter(|callee| !is_generic_callee(callee))
        .cloned()
        .collect()
}

fn is_generic_callee(callee: &str) -> bool {
    const GENERIC_METHODS: &[&str] = &[
        ".all",
        ".and_then",
        ".any",
        ".as_deref",
        ".as_mut",
        ".as_ref",
        ".as_slice",
        ".as_str",
        ".clone",
        ".cloned",
        ".cmp",
        ".collect",
        ".context",
        ".copied",
        ".dedup",
        ".display",
        ".enumerate",
        ".expect",
        ".extend",
        ".filter",
        ".filter_map",
        ".find",
        ".file_name",
        ".first",
        ".flat_map",
        ".flatten",
        ".get",
        ".get_mut",
        ".insert",
        ".into_owned",
        ".into_iter",
        ".is_empty",
        ".is_err",
        ".is_none",
        ".is_none_or",
        ".is_ok",
        ".is_ok_and",
        ".is_some",
        ".is_some_and",
        ".iter",
        ".iter_mut",
        ".join",
        ".last",
        ".len",
        ".lines",
        ".lock",
        ".map",
        ".map_err",
        ".map_or",
        ".map_or_else",
        ".next",
        ".ok",
        ".ok_or_else",
        ".or",
        ".or_else",
        ".or_default",
        ".push",
        ".sort",
        ".sort_by",
        ".sort_by_key",
        ".split",
        ".starts_with",
        ".take",
        ".then",
        ".then_some",
        ".to_owned",
        ".to_path_buf",
        ".to_string",
        ".to_string_lossy",
        ".transpose",
        ".trim",
        ".unwrap",
        ".unwrap_or",
        ".unwrap_or_default",
        ".unwrap_or_else",
        ".with_context",
    ];
    matches!(
        callee,
        "Ok" | "Err" | "Some" | "None" | "drop" | "render::out" | "render::paint"
    ) || callee.starts_with("render::palette::")
        || GENERIC_METHODS.contains(&callee)
}

fn jaccard(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    if left.is_empty() || right.is_empty() {
        return 0.0;
    }
    let intersection = left.intersection(right).count();
    let union = left.len() + right.len() - intersection;
    intersection as f64 / union as f64
}

fn callee_similarity(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    if left.intersection(right).count() < MIN_SHARED_CALLEES {
        0.0
    } else {
        jaccard(left, right)
    }
}

fn complete_linkage_groups(similarities: &[Vec<f64>], threshold: f64) -> Vec<Vec<usize>> {
    let mut pairs = (0..similarities.len())
        .flat_map(|left| {
            (left + 1..similarities.len())
                .map(move |right| (similarities[left][right], left, right))
        })
        .filter(|(similarity, _, _)| *similarity >= threshold)
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| {
        right
            .0
            .total_cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });

    let mut groups = (0..similarities.len())
        .map(|index| vec![index])
        .collect::<Vec<_>>();
    for (_, left, right) in pairs {
        let left_group = groups
            .iter()
            .position(|group| group.contains(&left))
            .expect("each function remains in exactly one complete-linkage group");
        let right_group = groups
            .iter()
            .position(|group| group.contains(&right))
            .expect("each function remains in exactly one complete-linkage group");
        if left_group == right_group
            || !groups[left_group].iter().all(|left| {
                groups[right_group]
                    .iter()
                    .all(|right| similarities[*left][*right] >= threshold)
            })
        {
            continue;
        }
        let (keep, remove) = if left_group < right_group {
            (left_group, right_group)
        } else {
            (right_group, left_group)
        };
        let removed = groups.remove(remove);
        groups[keep].extend(removed);
        groups[keep].sort_unstable();
    }
    groups
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas shapes report is the command's stdout contract"
)]
fn print_report(report: &Report) {
    println!("Atlas shapes — {}", report.path.display());
    for (index, cluster) in report.clusters.iter().enumerate() {
        println!(
            "{}. {} members across {} files / {} modules, mean {:.1} sloc, similarity floor {:.2}",
            index + 1,
            cluster.members.len(),
            cluster.distinct_files,
            cluster.distinct_modules,
            cluster.mean_sloc,
            cluster.similarity_floor
        );
        let mut calls = cluster
            .shared_callees
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if cluster.shared_callees.len() > 8 {
            calls.push_str(&format!(
                ", … +{}",
                cluster.shared_callees.len().saturating_sub(8)
            ));
        }
        println!("   calls: {calls}");
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

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn jaccard_scores_shape_similarity() {
        assert_eq!(jaccard(&set(&["open"]), &set(&["open"])), 1.0);
        assert_eq!(jaccard(&set(&["open", "load"]), &set(&["open"])), 0.5);
    }

    #[test]
    fn callee_sets_drop_generic_method_noise() {
        let callees = [
            "domain::load",
            ".map",
            ".context",
            "Some",
            ".render",
            "render::out",
            "render::paint",
            "render::palette::accent",
        ]
        .map(str::to_owned);

        assert_eq!(callee_set(&callees), set(&["domain::load", ".render"]));
    }

    #[test]
    fn choreography_requires_three_shared_callees() {
        assert_eq!(
            callee_similarity(&set(&["load", "resolve"]), &set(&["load", "resolve"])),
            0.0
        );
        assert_eq!(
            callee_similarity(
                &set(&["load", "resolve", "launch"]),
                &set(&["load", "resolve", "launch"])
            ),
            1.0
        );
    }

    #[test]
    fn complete_linkage_does_not_chain_dissimilar_members() {
        let similarities = vec![
            vec![1.0, 0.8, 0.6],
            vec![0.8, 1.0, 0.8],
            vec![0.6, 0.8, 1.0],
        ];

        let groups = complete_linkage_groups(&similarities, 0.7);

        assert_eq!(groups, [vec![0, 1], vec![2]]);
        assert!(groups.iter().all(|group| {
            group.iter().enumerate().all(|(position, left)| {
                group[position + 1..]
                    .iter()
                    .all(|right| similarities[*left][*right] >= 0.7)
            })
        }));
    }

    #[test]
    fn cross_module_clusters_rank_before_larger_single_file_clusters() {
        let function = |path: &str, line, sloc| FnBody {
            name: format!("function_{line}"),
            path: PathBuf::from(path),
            line,
            sloc,
            callees: ["prepare", "resolve", "launch"].map(str::to_owned).to_vec(),
        };
        let functions = vec![
            function("src/fixture.rs", 1, 100),
            function("src/fixture.rs", 2, 100),
            function("src/fixture.rs", 3, 100),
            function("src/fixture.rs", 4, 100),
            function("src/fixture.rs", 5, 100),
            function("src/a/one.rs", 1, 50),
            function("src/b/two.rs", 1, 50),
            function("src/c/three.rs", 1, 50),
        ];
        let callees = functions
            .iter()
            .map(|function| callee_set(&function.callees))
            .collect::<Vec<_>>();
        let mut clusters = vec![
            cluster(&functions, &callees, vec![0, 1, 2, 3, 4], Path::new("src")),
            cluster(&functions, &callees, vec![5, 6, 7], Path::new("src")),
        ];

        sort_clusters(&mut clusters);

        assert_eq!(clusters[0].distinct_modules, 3);
        assert_eq!(clusters[0].distinct_files, 3);
        assert_eq!(clusters[1].score, 500.0);
    }
}
