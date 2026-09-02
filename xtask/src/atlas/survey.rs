use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::detect::{self, GuardFamily};
use super::facts::{Facets, Facts};
use super::rank::{self, Row, Totals};
use super::shapes::{self, ShapeFamily};
use super::target::{self, TARGET_FILE, VerdictKind};
use super::{positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const DEFAULT_TOP: usize = 20;

const USAGE: &str = "cargo xtask atlas survey [--path <prefix>] [--top N]

Emits a bounded Markdown survey of accretion and duplicated knowledge.";

#[derive(Debug)]
struct Args {
    path: PathBuf,
    top: usize,
}

struct Report {
    path: PathBuf,
    rows: Vec<Row>,
    totals: Totals,
    shapes: Vec<ShapeFamily>,
    guards: Vec<GuardFamily>,
    history_commits: usize,
    pace_window: usize,
    parse_failures: usize,
    suppressed: usize,
    stale: Vec<String>,
}

#[expect(
    clippy::print_stdout,
    reason = "xtask atlas survey output is a command stdout contract"
)]
pub(super) fn run(root: &Path, raw: &[String]) -> Result<()> {
    let Some(args) = parse_args(raw)? else {
        println!("{USAGE}");
        return Ok(());
    };
    let facts = Facts::load(
        root,
        &args.path,
        Facets {
            history: true,
            metrics: true,
            references: false,
        },
    )?;
    let report = build_report(root, &facts, &args.path)?;
    print!("{}", render_markdown(&report, args.top));
    Ok(())
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut path = None;
    let mut top = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--path" => {
                let parsed = validate_scope(value(args, index, "survey", "--path")?, "--path")?;
                set_once(&mut path, parsed, "survey", "--path")?;
                index += 2;
            }
            "--top" => {
                let parsed =
                    positive_usize(value(args, index, "survey", "--top")?, "survey", "--top")?;
                set_once(&mut top, parsed, "survey", "--top")?;
                index += 2;
            }
            _ => bail!("unknown atlas survey argument `{arg}`"),
        }
    }
    Ok(Some(Args {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        top: top.unwrap_or(DEFAULT_TOP),
    }))
}

fn build_report(root: &Path, facts: &Facts, scope: &Path) -> Result<Report> {
    let rows = rank::rows(facts, scope)?;
    let totals = rank::totals(&rows);
    let all_shapes = shapes::families(facts, scope);
    let all_guards = detect::guard_families(facts, scope);
    let shape_keys = all_shapes
        .iter()
        .map(|family| family.name.clone())
        .collect::<BTreeSet<_>>();
    let guard_keys = all_guards
        .iter()
        .map(|family| family.key.clone())
        .collect::<BTreeSet<_>>();
    let configured = target::load(&root.join(TARGET_FILE))?;
    let suppressed_shapes = configured.as_ref().map_or_else(BTreeSet::new, |target| {
        target
            .verdicts
            .iter()
            .filter(|verdict| verdict.kind == VerdictKind::Shape)
            .map(|verdict| verdict.key.clone())
            .collect()
    });
    let suppressed_guards = configured.as_ref().map_or_else(BTreeSet::new, |target| {
        target
            .verdicts
            .iter()
            .filter(|verdict| verdict.kind == VerdictKind::Guard)
            .map(|verdict| verdict.key.clone())
            .collect()
    });
    let mut stale = configured
        .iter()
        .flat_map(|target| &target.verdicts)
        .filter_map(|verdict| match verdict.kind {
            VerdictKind::Shape if !shape_keys.contains(verdict.key.as_str()) => {
                Some(format!("shape:{}", verdict.key))
            }
            VerdictKind::Guard if !guard_keys.contains(verdict.key.as_str()) => {
                Some(format!("guard:{}", verdict.key))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    stale.sort();
    let shapes = all_shapes
        .into_iter()
        .filter(|family| !suppressed_shapes.contains(family.name.as_str()))
        .collect::<Vec<_>>();
    let guards = all_guards
        .into_iter()
        .filter(|family| !suppressed_guards.contains(family.key.as_str()))
        .collect::<Vec<_>>();
    let suppressed = shape_keys.intersection(&suppressed_shapes).count()
        + guard_keys.intersection(&suppressed_guards).count();
    let log = facts
        .history
        .as_ref()
        .context("survey history facts missing")?;

    Ok(Report {
        path: facts.scope.clone(),
        rows,
        totals,
        shapes,
        guards,
        history_commits: log.len(),
        pace_window: log.window_len(25),
        parse_failures: facts
            .syntax
            .parse_failures
            .iter()
            .filter(|path| super::modules::path_in_scope(path, scope))
            .count(),
        suppressed,
        stale,
    })
}

fn render_markdown(report: &Report, top: usize) -> String {
    let mut output = String::new();
    writeln!(output, "# Atlas survey — {}", report.path.display()).unwrap();
    writeln!(output, "\n## Accretion rank").unwrap();
    writeln!(
        output,
        "| module | code | tests | esc | churn% | pace | cx | t/c | flags |"
    )
    .unwrap();
    writeln!(
        output,
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |"
    )
    .unwrap();
    for row in report.rows.iter().take(top) {
        let pace = row
            .pace
            .map_or_else(|| "—".to_owned(), |pace| format!("{pace:.2}"));
        let ratio = if row.code > 0 {
            format!("{:.2}", row.tests as f64 / row.code as f64)
        } else {
            "—".to_owned()
        };
        let flags = if row.flags.is_empty() {
            "—".to_owned()
        } else {
            row.flags.join(",")
        };
        writeln!(
            output,
            "| {} | {} | {} | {} | {:.1} | {} | {:.1} | {} | {} |",
            row.module, row.code, row.tests, row.esc, row.churn, pace, row.cx, ratio, flags
        )
        .unwrap();
    }
    writeln!(
        output,
        "\noverall: code {}, tests {}, esc {}, cx {:.1}",
        report.totals.code, report.totals.tests, report.totals.esc, report.totals.cx
    )
    .unwrap();

    writeln!(output, "\n## Duplicated knowledge").unwrap();
    writeln!(output, "### Shape families").unwrap();
    for family in report.shapes.iter().take(top) {
        let locations = family
            .members
            .iter()
            .take(5)
            .map(|member| format!("{}:{}", member.path.display(), member.line))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            output,
            "- shape key: `{}`; {} members / {} files; mean {:.1} sloc; {}",
            family.name,
            family.members.len(),
            family.files,
            family.mean_sloc,
            locations
        )
        .unwrap();
    }
    writeln!(output, "### Guard families").unwrap();
    for family in report.guards.iter().take(top) {
        let locations = family
            .locations
            .iter()
            .take(5)
            .map(|site| format!("{}:{}", site.path.display(), site.line))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            output,
            "- guard key: `{}`; {} sites / {} files; {}",
            family.key, family.sites, family.files, locations
        )
        .unwrap();
    }

    writeln!(output, "\n## Footer").unwrap();
    writeln!(
        output,
        "history: {} scoped commits (pace window {})",
        report.history_commits, report.pace_window
    )
    .unwrap();
    writeln!(output, "parse failures: {}", report.parse_failures).unwrap();
    writeln!(output, "suppressed families: {}", report.suppressed).unwrap();
    let stale = report
        .stale
        .iter()
        .take(top)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if report.stale.len() > top {
        writeln!(
            output,
            "stale verdict keys: {} (and {} more)",
            stale,
            report.stale.len() - top
        )
        .unwrap();
    } else if stale.is_empty() {
        writeln!(output, "stale verdict keys: none").unwrap();
    } else {
        writeln!(output, "stale verdict keys: {stale}").unwrap();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::super::detect::GuardSite;
    use super::super::shapes::Member;
    use super::*;

    #[test]
    fn survey_output_is_bounded_by_top() {
        let rows = (0..30)
            .map(|index| Row {
                module: format!("module-{index}"),
                code: index,
                ..Row::default()
            })
            .collect::<Vec<_>>();
        let shapes = (0..30)
            .map(|index| ShapeFamily {
                name: format!("shape-{index}"),
                members: vec![Member {
                    path: PathBuf::from(format!("src/shape-{index}.rs")),
                    line: 1,
                    name: "work".to_owned(),
                    sloc: 40,
                }],
                files: 1,
                mean_sloc: 40.0,
                score: 40.0,
            })
            .collect();
        let guards = (0..30)
            .map(|index| GuardFamily {
                key: format!("guard-{index}"),
                files: 3,
                sites: 3,
                locations: vec![GuardSite {
                    path: PathBuf::from(format!("src/guard-{index}.rs")),
                    line: 1,
                    kind: "if".to_owned(),
                }],
            })
            .collect();
        let report = Report {
            path: PathBuf::from("src"),
            totals: rank::totals(&rows),
            rows,
            shapes,
            guards,
            history_commits: 100,
            pace_window: 25,
            parse_failures: 0,
            suppressed: 0,
            stale: (0..30)
                .map(|index| format!("shape:stale-{index}"))
                .collect(),
        };

        let output = render_markdown(&report, 20);

        assert!(output.lines().count() <= 80);
        assert!(output.contains("module-19"));
        assert!(!output.contains("module-20"));
        assert!(output.contains("and 10 more"));
    }
}
