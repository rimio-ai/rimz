use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, json};

use super::detect::{self, GuardFamily};
use super::facts::{Facets, Facts};
use super::output::{self, OutputArgs};
use super::rank::{self, Row, Totals};
use super::shapes::{self, ShapeFamily};
use super::target::{self, TARGET_FILE, VerdictKind};
use super::{positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const DEFAULT_TOP: usize = 20;

const USAGE: &str = "cargo xtask atlas survey [--path <prefix>] [--top N]

Emits a bounded Markdown survey of accretion and duplicated knowledge.";

const SECTIONS: &[&str] = &["rank", "shapes", "guards", "footer"];

fn usage() -> String {
    format!("{USAGE}\n\n{}", output::USAGE)
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    path: PathBuf,
    top: usize,
    output: OutputArgs,
}

#[derive(Serialize)]
struct Report {
    path: PathBuf,
    rows: Vec<Row>,
    totals: Totals,
    shapes: Vec<ShapeFamily>,
    guards: Vec<GuardFamily>,
    history_commits: usize,
    pace_window: usize,
    parse_failures: usize,
    guard_families_dropped: usize,
    suppressed: usize,
    stale: Vec<String>,
}

pub(super) fn run(root: &Path, raw: &[String]) -> Result<()> {
    let Some(args) = parse_args(raw)? else {
        return OutputArgs::default().emit(&format!("{}\n", usage()));
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
    let rendered = if args.output.json {
        render_json(&report, &args.output)?
    } else {
        render_markdown(&report, args.top, &args.output)
    };
    args.output.emit(&rendered)
}

fn parse_args(args: &[String]) -> Result<Option<Args>> {
    if args.iter().any(|arg| crate::is_help_flag(arg)) {
        return Ok(None);
    }
    let mut path = None;
    let mut top = None;
    let mut output = OutputArgs::default();
    let mut index = 0;
    while index < args.len() {
        if let Some(eaten) = output.parse_flag(args, index, "survey")? {
            index += eaten;
            continue;
        }
        let arg = &args[index];
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
    output.validate_sections("survey", SECTIONS)?;
    Ok(Some(Args {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        top: top.unwrap_or(DEFAULT_TOP),
        output,
    }))
}

fn build_report(root: &Path, facts: &Facts, scope: &Path) -> Result<Report> {
    let rows = rank::rows(facts, scope)?;
    let totals = rank::totals(&rows);
    let all_shapes = shapes::families(facts, scope);
    let (all_guards, guard_families_dropped) = detect::guard_families_with_dropped(facts, scope);
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
        guard_families_dropped,
        suppressed,
        stale,
    })
}

fn render_markdown(report: &Report, top: usize, output_args: &OutputArgs) -> String {
    let mut output = String::new();
    writeln!(output, "# Atlas survey — {}", report.path.display()).unwrap();
    if output_args.wants("rank") {
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
    }

    if output_args.wants("shapes") || output_args.wants("guards") {
        writeln!(output, "\n## Duplicated knowledge").unwrap();
    }
    if output_args.wants("shapes") {
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
    }
    if output_args.wants("guards") {
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
    }

    if output_args.wants("footer") {
        writeln!(output, "\n## Footer").unwrap();
        writeln!(
            output,
            "history: {} scoped commits (pace window {})",
            report.history_commits, report.pace_window
        )
        .unwrap();
        writeln!(output, "parse failures: {}", report.parse_failures).unwrap();
        writeln!(
            output,
            "guard families dropped as std idiom: {}",
            report.guard_families_dropped
        )
        .unwrap();
        writeln!(output, "suppressed families: {}", report.suppressed).unwrap();
        writeln!(
            output,
            "cx: severity-weighted over-threshold excess summed per function; 0 = every function under its warn thresholds"
        )
        .unwrap();
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
    }
    output
}

fn render_json(report: &Report, output_args: &OutputArgs) -> Result<String> {
    let mut sections = Map::new();
    sections.insert("path".to_owned(), serde_json::to_value(&report.path)?);
    if output_args.wants("rank") {
        sections.insert(
            "rank".to_owned(),
            json!({
                "rows": &report.rows,
                "totals": &report.totals,
            }),
        );
    }
    if output_args.wants("shapes") {
        sections.insert("shapes".to_owned(), serde_json::to_value(&report.shapes)?);
    }
    if output_args.wants("guards") {
        sections.insert("guards".to_owned(), serde_json::to_value(&report.guards)?);
    }
    if output_args.wants("footer") {
        sections.insert(
            "footer".to_owned(),
            json!({
                "history_commits": report.history_commits,
                "pace_window": report.pace_window,
                "parse_failures": report.parse_failures,
                "guard_families_dropped_as_std_idiom": report.guard_families_dropped,
                "suppressed_families": report.suppressed,
                "stale_verdict_keys": &report.stale,
            }),
        );
    }
    Ok(serde_json::to_string_pretty(&sections)?)
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
            guard_families_dropped: 4,
            suppressed: 0,
            stale: (0..30)
                .map(|index| format!("shape:stale-{index}"))
                .collect(),
        };

        let output = render_markdown(&report, 20, &OutputArgs::default());

        assert!(output.lines().count() <= 80);
        assert!(output.contains("module-19"));
        assert!(!output.contains("module-20"));
        assert!(output.contains("and 10 more"));
        assert!(output.contains("guard families dropped as std idiom: 4"));
        assert!(output.contains("cx: severity-weighted over-threshold excess"));
    }

    #[test]
    fn survey_parses_json_out_and_sections() {
        let args = [
            "--json",
            "--out",
            "/tmp/atlas-survey.json",
            "--section",
            "rank,guards",
        ]
        .map(str::to_owned)
        .to_vec();

        let parsed = parse_args(&args).unwrap().unwrap();

        assert!(parsed.output.json);
        assert_eq!(
            parsed.output.out.as_deref(),
            Some(Path::new("/tmp/atlas-survey.json"))
        );
        assert!(parsed.output.wants("rank"));
        assert!(parsed.output.wants("guards"));
        assert!(!parsed.output.wants("shapes"));
    }

    #[test]
    fn survey_rejects_unknown_sections() {
        let args = ["--section", "rank,unknown"].map(str::to_owned).to_vec();

        let error = parse_args(&args).unwrap_err().to_string();

        assert!(error.contains("unknown section(s) unknown"));
    }
}
