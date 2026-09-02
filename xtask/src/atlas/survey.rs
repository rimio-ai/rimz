use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, json};

use super::conform::{self, Direction};
use super::detect::{self, GuardFamily};
use super::facts::{Facets, Facts};
use super::modules::{module_is_within, path_in_scope};
use super::output::{self, OutputArgs};
use super::rank::{self, Hotspot, RankBy, Row, Totals};
use super::shapes::{self, ShapeFamily};
use super::syntax::resolved_internal_import;
use super::target::{self, TARGET_FILE, Target, VerdictKind};
use super::{positive_usize, set_once, validate_scope, value};

const DEFAULT_PATH: &str = "crates/rimz/src";
const DEFAULT_TOP: usize = 20;

const USAGE: &str = "cargo xtask atlas survey [--path <prefix>] [--top N]
    [--by <code|esc|churn|pace|cx|tc>] [--all]

Emits a bounded Markdown survey of accretion, admitted upward dependencies, and duplicated knowledge.
Rank order defaults to accretion (code × churn).";

const SECTIONS: &[&str] = &["rank", "hot", "debt", "shapes", "guards", "footer"];

/// One target rule's upward dependencies, counted at their sites and split
/// by whether the target admits them.
#[derive(Clone, Debug, Serialize)]
struct DebtRow {
    path: PathBuf,
    upward_sites: usize,
    admitted: Vec<ProviderSites>,
    unadmitted: Vec<ProviderSites>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct ProviderSites {
    provider: String,
    sites: usize,
}

#[derive(Clone, Debug, Serialize)]
struct StranglerRow {
    path: PathBuf,
    symbol: String,
    current: usize,
    baseline: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
struct Debt {
    configured: bool,
    rules: Vec<DebtRow>,
    stranglers: Vec<StranglerRow>,
}

fn usage() -> String {
    format!("{USAGE}\n\n{}", output::USAGE)
}

#[derive(Debug, PartialEq, Eq)]
struct Args {
    path: PathBuf,
    top: usize,
    by: RankBy,
    all: bool,
    output: OutputArgs,
}

#[derive(Serialize)]
struct Report {
    path: PathBuf,
    rows: Vec<Row>,
    totals: Totals,
    hot: Vec<Hotspot>,
    debt: Debt,
    shapes: Vec<ShapeFamily>,
    guards: Vec<GuardFamily>,
    history_commits: usize,
    pace_window: usize,
    parse_failures: usize,
    shape_families_dropped: shapes::FamilyDrops,
    guard_families_dropped: detect::GuardDrops,
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
    let report = build_report(root, &facts, &args.path, args.by, args.all)?;
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
    let mut by = None;
    let mut all = false;
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
            "--by" => {
                let raw = value(args, index, "survey", "--by")?;
                let parsed = RankBy::parse(raw).ok_or_else(|| {
                    anyhow::anyhow!(
                        "atlas survey --by must be one of code, esc, churn, pace, cx, tc"
                    )
                })?;
                set_once(&mut by, parsed, "survey", "--by")?;
                index += 2;
            }
            "--all" => {
                if all {
                    bail!("atlas survey --all may only be passed once");
                }
                all = true;
                index += 1;
            }
            _ => bail!("unknown atlas survey argument `{arg}`"),
        }
    }
    output.validate_sections("survey", SECTIONS)?;
    Ok(Some(Args {
        path: path.unwrap_or_else(|| PathBuf::from(DEFAULT_PATH)),
        top: top.unwrap_or(DEFAULT_TOP),
        by: by.unwrap_or_default(),
        all,
        output,
    }))
}

fn build_report(
    root: &Path,
    facts: &Facts,
    scope: &Path,
    by: RankBy,
    include_all: bool,
) -> Result<Report> {
    let rows = rank::rows_by(facts, scope, by)?;
    let totals = rank::totals(&rows);
    let (all_shapes, shape_families_dropped) = if include_all {
        shapes::families_all_with_dropped(facts, scope)
    } else {
        shapes::families_with_dropped(facts, scope)
    };
    let (all_guards, guard_families_dropped) = if include_all {
        detect::guard_families_all_with_dropped(facts, scope)
    } else {
        detect::guard_families_with_dropped(facts, scope)
    };
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
    let metrics = facts
        .metrics
        .as_ref()
        .context("survey metric facts missing")?;
    let hot = rank::hotspots(metrics, &log.file_shares(&facts.root, scope));
    let debt = configured.as_ref().map_or_else(Debt::default, |target| {
        recorded_debt(root, target, facts, scope)
    });

    Ok(Report {
        path: facts.scope.clone(),
        rows,
        totals,
        hot,
        debt,
        shapes,
        guards,
        history_commits: log.len(),
        pace_window: log.window_len(25),
        parse_failures: facts
            .syntax
            .parse_failures
            .iter()
            .filter(|path| path_in_scope(path, scope))
            .count(),
        shape_families_dropped,
        guard_families_dropped,
        suppressed,
        stale,
    })
}

/// Counts every upward dependency site under each target rule that touches
/// the scope, grouped by the admission it matches, and every strangler's
/// current count against its baseline. Syntax-only, so it needs no index.
fn recorded_debt(root: &Path, target: &Target, facts: &Facts, scope: &Path) -> Debt {
    let ranks = target.layer_ranks();
    let touches_scope = |path: &Path| path_in_scope(path, scope) || path_in_scope(scope, path);
    let mut rules = Vec::new();
    for rule in target
        .modules
        .iter()
        .filter(|rule| touches_scope(&rule.path))
    {
        let admissions = rule
            .allowed_dependencies
            .as_deref()
            .or(rule.upward_dependencies.as_deref())
            .unwrap_or_default();
        let mut admitted = BTreeMap::<String, usize>::new();
        let mut unadmitted = BTreeMap::<String, usize>::new();
        let files = facts
            .syntax
            .files
            .iter()
            .filter(|file| conform::rule_covers_path(root, &rule.path, &file.path));
        let target_module =
            super::modules::crate_module_for_path(&if root.join(&rule.path).is_dir() {
                rule.path.join("mod.rs")
            } else {
                rule.path.clone()
            });
        for file in files {
            for dependency in &file.dependencies {
                let Some(resolved) =
                    resolved_internal_import(dependency, &facts.known_modules, &facts.crate_names)
                else {
                    continue;
                };
                if module_is_within(&resolved, &target_module)
                    || conform::layer_direction(&ranks, &file.module_path, &resolved)
                        != Some(Direction::Upward)
                {
                    continue;
                }
                match admissions
                    .iter()
                    .find(|prefix| module_is_within(&resolved, prefix))
                {
                    Some(prefix) => *admitted.entry(prefix.clone()).or_default() += 1,
                    None => *unadmitted.entry(resolved).or_default() += 1,
                }
            }
        }
        let upward_sites = admitted.values().sum::<usize>() + unadmitted.values().sum::<usize>();
        if upward_sites == 0 {
            continue;
        }
        rules.push(DebtRow {
            path: rule.path.clone(),
            upward_sites,
            admitted: provider_sites(admitted),
            unadmitted: provider_sites(unadmitted),
        });
    }
    rules.sort_by(|left, right| {
        right
            .upward_sites
            .cmp(&left.upward_sites)
            .then_with(|| left.path.cmp(&right.path))
    });
    let stranglers = target
        .strangler
        .iter()
        .filter(|strangler| touches_scope(&strangler.path))
        .map(|strangler| {
            let is_file = root.join(&strangler.path).is_file();
            let sources = conform::sources_for_path(&facts.sources, &strangler.path, is_file);
            StranglerRow {
                path: strangler.path.clone(),
                symbol: strangler.symbol.clone(),
                current: conform::count_in_sources(
                    &sources,
                    &facts.syntax.files,
                    &strangler.symbol,
                ),
                baseline: strangler.baseline,
            }
        })
        .collect();
    Debt {
        configured: true,
        rules,
        stranglers,
    }
}

fn provider_sites(counts: BTreeMap<String, usize>) -> Vec<ProviderSites> {
    let mut rows = counts
        .into_iter()
        .map(|(provider, sites)| ProviderSites { provider, sites })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .sites
            .cmp(&left.sites)
            .then_with(|| left.provider.cmp(&right.provider))
    });
    rows
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

    if output_args.wants("hot") {
        writeln!(output, "\n## Function hotspots").unwrap();
        writeln!(output, "| function | file:line | cx | churn% | hot |").unwrap();
        writeln!(output, "| --- | --- | ---: | ---: | ---: |").unwrap();
        for row in report.hot.iter().take(top) {
            writeln!(
                output,
                "| {} | {}:{} | {:.1} | {:.1} | {:.1} |",
                row.function,
                row.path.display(),
                row.line,
                row.cx,
                row.churn,
                row.hot
            )
            .unwrap();
        }
    }

    if output_args.wants("debt") {
        render_debt(&mut output, &report.debt, top);
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
            let siblings = family.role.as_ref().map_or_else(String::new, |role| {
                format!("; siblings {} ({role})", family.siblings)
            });
            let provider = family
                .provider
                .as_ref()
                .map_or_else(String::new, |provider| format!("; provider {provider}"));
            writeln!(
                output,
                "- shape key: `{}`; {} members / {} files{siblings}{provider}; mean {:.1} sloc; {:.1} sloc in play; {}",
                family.name,
                family.members.len(),
                family.files,
                family.mean_sloc,
                family.sloc_in_play,
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
            "shape families dropped as std vocabulary: {}; {} below the finding gate; {} as one module's API (`--all` shows them)",
            report.shape_families_dropped.vocabulary,
            report.shape_families_dropped.below_gate,
            report.shape_families_dropped.single_provider
        )
        .unwrap();
        writeln!(
            output,
            "guard families dropped as std idiom: {}; {} as predicate use (`--all` shows them)",
            report.guard_families_dropped.vocabulary, report.guard_families_dropped.predicate_use
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

fn render_debt(output: &mut String, debt: &Debt, top: usize) {
    writeln!(output, "\n## Admitted upward dependencies").unwrap();
    if !debt.configured {
        writeln!(output, "no {TARGET_FILE}; nothing recorded").unwrap();
        return;
    }
    if debt.rules.is_empty() {
        writeln!(output, "upward dependencies: none under scoped rules").unwrap();
    } else {
        writeln!(
            output,
            "| rule | upward sites | admitted (sites) | unadmitted (sites) |"
        )
        .unwrap();
        writeln!(output, "| --- | ---: | --- | --- |").unwrap();
        for row in debt.rules.iter().take(top) {
            writeln!(
                output,
                "| {} | {} | {} | {} |",
                row.path.display(),
                row.upward_sites,
                render_provider_sites(&row.admitted, top),
                render_provider_sites(&row.unadmitted, top)
            )
            .unwrap();
        }
        if debt.rules.len() > top {
            writeln!(output, "\n_{} more rules omitted._", debt.rules.len() - top).unwrap();
        }
    }
    if !debt.stranglers.is_empty() {
        let stranglers = debt
            .stranglers
            .iter()
            .take(top)
            .map(|row| {
                format!(
                    "`{}` {} {}/{}",
                    row.symbol,
                    row.path.display(),
                    row.current,
                    row.baseline
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "\nstranglers (current/baseline): {stranglers}").unwrap();
    }
}

fn render_provider_sites(rows: &[ProviderSites], top: usize) -> String {
    if rows.is_empty() {
        return "—".to_owned();
    }
    let mut rendered = rows
        .iter()
        .take(top)
        .map(|row| format!("{} {}", row.provider, row.sites))
        .collect::<Vec<_>>();
    if rows.len() > top {
        rendered.push(format!("… {} more", rows.len() - top));
    }
    rendered.join(", ")
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
    if output_args.wants("hot") {
        sections.insert("hot".to_owned(), serde_json::to_value(&report.hot)?);
    }
    if output_args.wants("debt") {
        sections.insert("debt".to_owned(), serde_json::to_value(&report.debt)?);
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
                "shape_families_dropped_as_std_vocabulary": report.shape_families_dropped.vocabulary,
                "shape_families_below_finding_gate": report.shape_families_dropped.below_gate,
                "shape_families_dropped_as_one_module_api": report.shape_families_dropped.single_provider,
                "guard_families_dropped_as_std_idiom": report.guard_families_dropped.vocabulary,
                "guard_families_dropped_as_predicate_use": report.guard_families_dropped.predicate_use,
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
                sloc_in_play: 40.0,
                score: 40.0,
                siblings: 0,
                role: None,
                provider: None,
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
        let hot = (0..30)
            .map(|index| Hotspot {
                function: format!("hot-{index}"),
                path: PathBuf::from(format!("src/hot-{index}.rs")),
                line: 1,
                cx: 1.0,
                churn: 1.0,
                hot: 1.0,
            })
            .collect();
        let report = Report {
            path: PathBuf::from("src"),
            totals: rank::totals(&rows),
            rows,
            hot,
            debt: Debt {
                configured: true,
                rules: (0..30)
                    .map(|index| DebtRow {
                        path: PathBuf::from(format!("src/rule-{index}")),
                        upward_sites: 30 - index,
                        admitted: vec![ProviderSites {
                            provider: "cli".to_owned(),
                            sites: 30 - index,
                        }],
                        unadmitted: Vec::new(),
                    })
                    .collect(),
                stranglers: vec![StranglerRow {
                    path: PathBuf::from("src/store"),
                    symbol: "legacy_open".to_owned(),
                    current: 2,
                    baseline: 3,
                }],
            },
            shapes,
            guards,
            history_commits: 100,
            pace_window: 25,
            parse_failures: 0,
            shape_families_dropped: shapes::FamilyDrops {
                vocabulary: 6,
                below_gate: 3,
                single_provider: 2,
            },
            guard_families_dropped: detect::GuardDrops {
                vocabulary: 4,
                predicate_use: 2,
            },
            suppressed: 0,
            stale: (0..30)
                .map(|index| format!("shape:stale-{index}"))
                .collect(),
        };

        let output = render_markdown(&report, 20, &OutputArgs::default());

        assert!(output.lines().count() <= 135);
        assert!(output.contains("module-19"));
        assert!(!output.contains("module-20"));
        assert!(output.contains("hot-19"));
        assert!(!output.contains("hot-20"));
        assert!(output.contains("and 10 more"));
        assert!(output.contains("| src/rule-19 | 11 | cli 11 | — |"));
        assert!(!output.contains("src/rule-20"));
        assert!(output.contains("_10 more rules omitted._"));
        assert!(output.contains("stranglers (current/baseline): `legacy_open` src/store 2/3"));
        assert!(output.contains("shape families dropped as std vocabulary: 6"));
        assert!(output.contains("3 below the finding gate"));
        assert!(output.contains("2 as one module's API"));
        assert!(output.contains("guard families dropped as std idiom: 4"));
        assert!(output.contains("2 as predicate use"));
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
            "--by",
            "tc",
            "--all",
        ]
        .map(str::to_owned)
        .to_vec();

        let parsed = parse_args(&args).unwrap().unwrap();

        assert!(parsed.output.json);
        assert_eq!(parsed.by, RankBy::TestCode);
        assert!(parsed.all);
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
