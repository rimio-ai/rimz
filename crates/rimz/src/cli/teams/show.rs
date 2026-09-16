use std::io::Write;

use anyhow::{Context, Result, bail};
use unicode_width::UnicodeWidthStr;

use super::super::{GlobalFlags, render};
use super::list::{
    CohortState, LiveInstance, LiveMember, PrReport, SignalFire, TeamReport, ci_style, stage_label,
};
use rimz::config::Isolation;
use rimz::harness::schedule::run_log::LoopRunResult;
use rimz::store::snapshot::{WorktreePrCi, WorktreePrState};

pub(super) fn run(
    name: Option<&str>,
    lane: Option<&str>,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let machine = rimz::config::MachineConfig::load().context("loading machine config")?;
    let reports = super::list::load_catalog(globals, lane, &machine)?;
    let selected = select_reports(&reports, name)?;
    if json {
        return if name.is_some() {
            render::json_pretty(&selected[0])
        } else {
            render::json_pretty(&selected)
        };
    }
    let mut out = render::out();
    if selected.is_empty() {
        writeln!(
            out,
            "{}",
            render::paint(
                render::palette::muted(),
                &format!("no live team in #{}", lane.unwrap_or_default())
            )
        )?;
    }
    let now = jiff::Timestamp::now();
    for (index, report) in selected.iter().enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        write_report(&mut out, report, lane, now)?;
    }
    Ok(())
}

fn select_reports<'a>(
    reports: &'a [TeamReport],
    name: Option<&str>,
) -> Result<Vec<&'a TeamReport>> {
    let Some(name) = name else {
        return Ok(reports
            .iter()
            .filter(|report| !report.instances.is_empty())
            .collect());
    };
    let Some(report) = reports
        .iter()
        .find(|report| report.name == name && report.defined)
    else {
        let valid = reports
            .iter()
            .filter(|report| report.defined)
            .map(|report| report.name.as_str())
            .collect::<Vec<_>>();
        if valid.is_empty() {
            bail!(
                "unknown team `{name}`; no teams are configured (install one with `rimz teams install forge`)"
            );
        }
        bail!(
            "unknown team `{name}`; configured teams: {}",
            valid.join(", ")
        );
    };
    Ok(vec![report])
}

fn write_report(
    w: &mut impl Write,
    report: &TeamReport,
    lane: Option<&str>,
    now: jiff::Timestamp,
) -> Result<()> {
    let Some(lane) = lane else {
        write_spec(w, report)?;
        return write_cohort_table(w, report, now);
    };
    if report.instances.is_empty() {
        writeln!(
            w,
            "{}",
            render::paint(
                render::palette::muted(),
                &format!("no live instance in #{lane}")
            )
        )?;
        writeln!(w)?;
    }
    for instance in &report.instances {
        write_lane(w, &report.name, instance, now)?;
        writeln!(w)?;
    }
    write_spec(w, report)
}

fn write_spec(w: &mut impl Write, report: &TeamReport) -> Result<()> {
    writeln!(
        w,
        "{}",
        render::paint(render::palette::header().bold(), &report.name)
    )?;
    let mut definition = render::KeyVals::new().indent(2);
    definition.push(
        "source",
        render::cell(
            report
                .source
                .as_deref()
                .map(render::home_relative)
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash(),
    );
    definition.push(
        "layout",
        render::cell(report.layout.as_deref().unwrap_or("-")).dash(),
    );
    if let Some(error) = &report.error {
        definition.push(
            "error",
            render::cell(render::one_line(error)).fg(render::palette::alarm()),
        );
    }
    definition.render(w)?;
    if !report.roles.is_empty() {
        writeln!(w)?;
    }

    render::Roster::new(
        report
            .roles
            .iter()
            .map(|role| render::RosterRow {
                handle: role.role.clone(),
                kind: role.kind.clone().unwrap_or_else(|| "-".to_owned()),
                model: role.model.clone(),
                leader: report.leader.as_deref() == Some(role.role.as_str()),
            })
            .collect(),
    )
    .signal_width(render::terminal_columns(120))
    .signals(
        report
            .roles
            .iter()
            .flat_map(|role| {
                role.signals.iter().map(|signal| render::RosterSignal {
                    signal: signal.signal.clone(),
                    matches: signal.matches.clone(),
                    role: role.role.clone(),
                })
            })
            .collect(),
    )
    .indent(2)
    .render(w)?;
    if report.consensus.is_some()
        || report.roles.iter().any(|role| {
            role.system_prompt_file.is_some() || !role.append_system_prompt_files.is_empty()
        })
    {
        writeln!(
            w,
            "  {}",
            render::paint(
                render::palette::muted(),
                &format!("(prompt stack: rimz teams show {} --json)", report.name)
            )
        )?;
    }
    Ok(())
}

fn write_cohort_table(w: &mut impl Write, report: &TeamReport, now: jiff::Timestamp) -> Result<()> {
    if report.instances.is_empty() {
        return Ok(());
    }
    writeln!(w)?;
    let mut table = render::Table::new(["LANE", "STAGE", "PR", "STATUS"])
        .indent(2)
        .max_width(render::terminal_columns(120));
    for instance in &report.instances {
        let pr = instance.pr.as_ref();
        table.row([
            render::cell(format!("#{}", instance.channel)).fg(render::palette::meta()),
            render::cell(stage_with_age(instance, now).unwrap_or_else(|| "-".to_owned())).dash(),
            render::cell(pr.map_or_else(
                || "-".to_owned(),
                |pr| {
                    pr_facts(pr)
                        .into_iter()
                        .map(|(fact, _)| fact)
                        .collect::<Vec<_>>()
                        .join(" · ")
                },
            ))
            .fg(pr
                .and_then(|pr| pr.ci)
                .map_or_else(render::palette::accent, ci_style))
            .dash(),
            render::cell(state_with_age(instance, now)).fg(state_style(instance.state)),
        ]);
    }
    table.render(w)?;
    Ok(())
}

fn stage_with_age(instance: &LiveInstance, now: jiff::Timestamp) -> Option<String> {
    let stage = instance.stage.as_ref()?;
    let label = stage_label(stage);
    Some(match stage.since {
        Some(since) => format!("{label} for {}", render::age_short(since, now)),
        None => label,
    })
}

fn state_with_age(instance: &LiveInstance, now: jiff::Timestamp) -> String {
    match instance.last_activity_at {
        Some(at) if instance.state != CohortState::Working => {
            format!("{} {}", instance.state.as_str(), render::age_short(at, now))
        }
        _ => instance.state.as_str().to_owned(),
    }
}

fn state_style(state: CohortState) -> anstyle::Style {
    match state {
        CohortState::Working => render::palette::accent(),
        CohortState::Blocked => render::palette::alarm(),
        CohortState::Paused => render::palette::warn(),
        CohortState::Sleeping | CohortState::Idle => render::palette::muted(),
    }
}

/// `#n`, PR state, and CI verdict as far as cached, each with its tone; the URL stays with the lane view.
fn pr_facts(pr: &PrReport) -> Vec<(String, anstyle::Style)> {
    let mut facts = Vec::new();
    if let Some(number) = pr.number {
        facts.push((format!("#{number}"), anstyle::Style::new()));
    }
    if let Some(state) = pr.state {
        facts.push(match state {
            WorktreePrState::Open => ("open".to_owned(), render::palette::accent()),
            WorktreePrState::Merged => ("merged".to_owned(), render::palette::good()),
            WorktreePrState::Closed => ("closed".to_owned(), render::palette::muted()),
        });
    }
    if let Some(ci) = pr.ci {
        let label = match ci {
            WorktreePrCi::Passing => "passing",
            WorktreePrCi::Pending => "pending",
            WorktreePrCi::Failing => "failing",
        };
        facts.push((format!("ci {label}"), ci_style(ci)));
    }
    facts
}

/// `  signals: `, the indent and key the `signals` block renders before each line.
const SIGNALS_PREFIX_WIDTH: usize = 11;

fn write_lane(
    w: &mut impl Write,
    team: &str,
    instance: &LiveInstance,
    now: jiff::Timestamp,
) -> Result<()> {
    write!(
        w,
        "{}",
        render::paint(
            render::palette::header().bold(),
            &format!("{team}#{}", instance.channel)
        )
    )?;
    writeln!(
        w,
        " · {}",
        render::paint(state_style(instance.state), &state_with_age(instance, now))
    )?;

    let mut progress = render::KeyVals::new().indent(2);
    progress.push(
        "stage",
        match stage_with_age(instance, now) {
            Some(stage) => render::cell(stage),
            None => render::cell("none").fg(render::palette::muted()),
        },
    );
    if !instance.stages.is_empty() {
        let current = instance.stage.as_ref().map(|stage| stage.name.as_str());
        progress.push(
            "pipeline",
            render::cell(rimz::harness::team_stage::stage_strip(
                &instance.stages,
                current,
            )),
        );
    }
    let pr = instance
        .pr
        .iter()
        .flat_map(|report| {
            pr_facts(report)
                .into_iter()
                .map(|(fact, style)| render::paint(style, &fact))
                .chain(report.url.clone())
        })
        .collect::<Vec<_>>();
    progress.push(
        "pr",
        render::cell(if pr.is_empty() {
            "none".to_owned()
        } else {
            pr.join(" · ")
        }),
    );
    progress.render(w)?;

    writeln!(w)?;
    let mut live = render::Table::new(["MEMBER", "STATUS", "AGE", "ACTIVITY", "CTX", "COST"])
        .indent(2)
        .right(&[2, 4, 5])
        .max_width(render::terminal_columns(120));
    for member in &instance.members {
        live.row(member_cells(member, now));
    }
    live.render(w)?;

    let signals = instance
        .members
        .iter()
        .flat_map(|member| member.signals.iter().map(move |signal| (member, signal)))
        .collect::<Vec<_>>();
    if !signals.is_empty() {
        writeln!(w)?;
        let width = render::terminal_columns(120);
        let mut armed = render::KeyVals::new().indent(2);
        armed.push_lines(
            "signals",
            signals.into_iter().map(|(member, signal)| {
                let (fire, style) = signal_fire(signal.fired.as_ref(), now);
                let lead = format!("{} → {} · ", signal.selector, member.handle);
                let room =
                    width.saturating_sub(SIGNALS_PREFIX_WIDTH + lead.width() + fire.width() + 3);
                vec![
                    render::cell(lead),
                    render::cell(fire).fg(style),
                    render::cell(" · "),
                    render::cell(render::clip_to_width(&signal.name, room))
                        .fg(render::palette::muted()),
                ]
            }),
        );
        armed.render(w)?;
    }

    writeln!(w)?;
    let mut facts = render::KeyVals::new().indent(2);
    let mut checkout = instance
        .worktree
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "-".to_owned());
    if let Some(branch) = &instance.branch
        && instance.worktree.as_ref().and_then(|path| path.file_name())
            != Some(std::ffi::OsStr::new(branch))
    {
        checkout.push_str(&format!(" · branch {branch}"));
    }
    facts.push("worktree", render::cell(checkout).dash());
    let tmp = instance.tmp_dir.display().to_string();
    let tmp = match instance.isolation {
        Isolation::Sandbox => format!("{} (as /tmp)", render::home_relative(&tmp)),
        Isolation::Host => tmp,
    };
    facts.push(
        "isolation",
        render::cell(format!("{} · tmp {tmp}", instance.isolation)),
    );
    if !instance.memory.is_empty() {
        let names = instance
            .memory
            .iter()
            .map(|file| {
                instance
                    .worktree
                    .as_ref()
                    .and_then(|worktree| file.path.strip_prefix(worktree).ok())
                    .unwrap_or(&file.path)
                    .display()
                    .to_string()
            })
            .collect::<Vec<_>>();
        let name_width = names.iter().map(|name| name.width()).max().unwrap_or(0);
        let lines_width = instance
            .memory
            .iter()
            .map(|file| file.lines.to_string().len())
            .max()
            .unwrap_or(0);
        facts.push_lines(
            "memory",
            instance.memory.iter().zip(&names).map(|(file, name)| {
                let age = file
                    .modified_at
                    .map(|time| render::rel_age(time, now))
                    .unwrap_or_else(|| "-".to_owned());
                vec![render::cell(format!(
                    "{name}{:pad$}  {:>lines_width$} lines · {age}",
                    "",
                    file.lines,
                    pad = name_width - name.width(),
                ))]
            }),
        );
    }
    facts.render(w)?;
    Ok(())
}

fn signal_fire(fired: Option<&SignalFire>, now: jiff::Timestamp) -> (String, anstyle::Style) {
    let Some(fired) = fired else {
        return ("never fired".to_owned(), render::palette::muted());
    };
    let (label, style) = match fired.result {
        LoopRunResult::Delivered => ("fired", render::palette::good()),
        LoopRunResult::SignalSkipped => ("skipped", render::palette::muted()),
        LoopRunResult::TargetGone | LoopRunResult::Errored => {
            (fired.result.label(), render::palette::alarm())
        }
        other => (other.label(), render::palette::muted()),
    };
    let mut text = format!("{label} {}", render::rel_age(fired.at, now));
    if fired.runs > 1 {
        text.push_str(&format!(" ×{}", fired.runs));
    }
    (text, style)
}

fn member_cells(member: &LiveMember, now: jiff::Timestamp) -> [render::Cell; 6] {
    [
        render::cell(&member.handle).fg(render::palette::identity(&member.kind)),
        render::cell(member.status.as_str()).fg(render::status::agent(member.status, member.phase)),
        render::cell(render::age_short(member.last_activity_at, now)).fg(render::palette::muted()),
        render::cell(member.activity.as_deref().unwrap_or("-")).dash(),
        render::cell(
            member
                .context_fill_pct
                .map(|pct| format!("{pct:.0}%"))
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash(),
        render::cell(
            member
                .cost_usd
                .map(|cost| format!("${cost:.2}"))
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash()
        .fg(render::palette::money()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::teams::list::{LiveInstance, RoleReport, TeamReport};
    use rimz::agents::{AgentStatus, TurnPhase};
    use std::collections::BTreeMap;

    fn report(instances: Vec<LiveInstance>) -> TeamReport {
        TeamReport {
            name: "forge".to_owned(),
            defined: true,
            source: Some("/tmp/.agents/teams/forge/team.toml".to_owned()),
            layout: Some("planner,coder+reviewer".to_owned()),
            leader: Some("planner".to_owned()),
            roles: vec![RoleReport {
                signals: vec![super::super::list::DeclaredSignal {
                    signal: "ci.failed".to_owned(),
                    matches: BTreeMap::new(),
                    prompt: Some("Fix CI".to_owned()),
                }],
                role: "planner".to_owned(),
                profile: "claude".to_owned(),
                kind: Some("claude".to_owned()),
                model: Some("fable".to_owned()),
                effort: Some("high".to_owned()),
                mode: Some("auto".to_owned()),
                system_prompt_file: Some("planner.md".into()),
                append_system_prompt_files: Vec::new(),
            }],
            consensus: Some("builtin".to_owned()),
            append_system_prompt_files: vec!["pipeline.md".into()],
            valid: true,
            error: None,
            instances,
        }
    }

    fn live_instance() -> LiveInstance {
        LiveInstance {
            channel: "feat-x".to_owned(),
            state: CohortState::Working,
            status_counts: BTreeMap::from([("running".to_owned(), 1)]),
            last_activity_at: Some(jiff::Timestamp::UNIX_EPOCH),
            worktree: Some("/repo/worktrees/feat-x".into()),
            branch: Some("feat-x".to_owned()),
            isolation: Isolation::Host,
            tmp_dir: "/tmp".into(),
            stages: vec!["Explore".into(), "Plan".into(), "Implement".into()],
            stage: Some(super::super::list::StageReport {
                name: "Plan (delta)".into(),
                owner: Some("planner".into()),
                since: Some(jiff::Timestamp::UNIX_EPOCH),
            }),
            pr: None,
            memory: Vec::new(),
            members: vec![LiveMember {
                role: Some("planner".to_owned()),
                signals: vec![super::super::list::LiveSignal {
                    name: "team-forge-feat-x-planner-ci-failed".to_owned(),
                    selector: "ci.failed".to_owned(),
                    matches: BTreeMap::from([("path".to_owned(), "/repo/feat-x".to_owned())]),
                    fired: Some(super::super::list::SignalFire {
                        at: jiff::Timestamp::from_second(60).unwrap(),
                        result: LoopRunResult::Delivered,
                        runs: 2,
                    }),
                }],
                handle: "@planner".to_owned(),
                kind: "claude".to_owned(),
                status: AgentStatus::Running,
                phase: TurnPhase::Reasoning,
                activity: Some("reading show.rs".into()),
                last_activity_at: jiff::Timestamp::UNIX_EPOCH,
                context_fill_pct: Some(42.0),
                cost_usd: Some(0.25),
            }],
        }
    }

    fn rendered(report: &TeamReport, lane: Option<&str>) -> String {
        let mut output = anstream::StripStream::new(Vec::new());
        write_report(
            &mut output,
            report,
            lane,
            jiff::Timestamp::from_second(120).unwrap(),
        )
        .unwrap();
        String::from_utf8(output.into_inner()).unwrap()
    }

    fn stage_value(output: &str) -> &str {
        output
            .lines()
            .nth(1)
            .and_then(|line| line.trim_start().strip_prefix("stage:"))
            .map_or("", str::trim_start)
    }

    #[test]
    fn human_show_brackets_the_board_stage_and_uses_last_activity() {
        let mut instance = live_instance();
        instance.pr = Some(super::super::list::PrReport {
            number: Some(412),
            state: Some(WorktreePrState::Open),
            ci: Some(WorktreePrCi::Passing),
            url: Some("https://example.com/pull/412".into()),
        });
        instance.memory = vec![super::super::list::MemoryReport {
            path: "/repo/worktrees/feat-x/blackboard.md".into(),
            lines: 41,
            modified_at: Some(jiff::Timestamp::UNIX_EPOCH),
        }];
        let output = rendered(&report(vec![instance.clone()]), Some("feat-x"));
        assert!(output.starts_with("forge#feat-x · working\n"));
        assert_eq!(stage_value(&output), "Plan (delta) (@planner) for 2m");
        assert!(output.contains("Explore → Plan → Implement → Done"));
        assert!(output.contains(
            "ci.failed → @planner · fired 1m ago ×2 · team-forge-feat-x-planner-ci-failed"
        ));
        assert!(output.contains("ci passing"));
        assert!(output.contains("blackboard.md"));
        assert_eq!(output.matches("/repo/worktrees/feat-x").count(), 1);
        assert!(output.contains("isolation: host · tmp /tmp"));
        assert!(output.contains("41 lines · 2m ago"));
        assert!(output.lines().any(|line| {
            line.split_whitespace()
                .take(3)
                .eq(["@planner", "running", "2m"])
        }));
        instance.state = CohortState::Idle;
        let stageless = LiveInstance {
            stage: None,
            ..instance.clone()
        };
        let output = rendered(&report(vec![stageless]), Some("feat-x"));
        assert!(output.starts_with("forge#feat-x · idle 2m\n"));
        assert_eq!(stage_value(&output), "none");
        instance.stage.as_mut().unwrap().name = "Done".into();
        assert!(
            rendered(&report(vec![instance]), Some("feat-x"))
                .contains("Explore → Plan → Implement → [Done]")
        );
    }

    #[test]
    fn json_show_includes_declared_and_live_signals() {
        let json = serde_json::to_value(report(vec![live_instance()])).unwrap();
        assert_eq!(
            json["roles"][0]["signals"],
            serde_json::json!([{
                "signal": "ci.failed", "match": {}, "prompt": "Fix CI"
            }])
        );
        let member = &json["instances"][0]["members"][0];
        assert_eq!(json["instances"][0]["isolation"], "host");
        assert_eq!(json["instances"][0]["tmp_dir"], "/tmp");
        assert_eq!(json["instances"][0]["state"], "working");
        assert_eq!(
            json["instances"][0]["stage"]["since"],
            "1970-01-01T00:00:00Z"
        );
        assert_eq!(member["role"], "planner");
        assert_eq!(
            member["signals"],
            serde_json::json!([{
                "name": "team-forge-feat-x-planner-ci-failed",
                "selector": "ci.failed", "matches": {"path": "/repo/feat-x"},
                "fired": {"at": "1970-01-01T00:01:00Z", "result": "delivered", "runs": 2}
            }])
        );
    }

    #[test]
    fn human_show_lane_form_leads_with_status() {
        insta::assert_snapshot!(rendered(&report(vec![live_instance()]), Some("feat-x")));
    }

    #[test]
    fn human_show_team_form_lists_cohorts() {
        let mut idle = live_instance();
        idle.channel = "feat-y".to_owned();
        idle.state = CohortState::Idle;
        idle.stage = None;
        idle.pr = Some(super::super::list::PrReport {
            number: Some(412),
            state: Some(WorktreePrState::Open),
            ci: Some(WorktreePrCi::Failing),
            url: Some("https://example.com/pull/412".into()),
        });
        insta::assert_snapshot!(rendered(&report(vec![live_instance(), idle]), None));
    }

    #[test]
    fn human_show_without_live_instance() {
        insta::assert_snapshot!(rendered(&report(Vec::new()), Some("ended-lane")));
    }

    #[test]
    fn human_show_empty_roster_has_no_extra_separator() {
        for instances in [Vec::new(), vec![live_instance()]] {
            let mut report = report(instances);
            report.roles.clear();
            for lane in [None, Some("feat-x")] {
                let output = rendered(&report, lane);
                assert!(!output.contains("\n\n\n"));
                assert!(!output.ends_with("\n\n"));
            }
        }
    }

    #[test]
    fn human_show_answers_when_a_selected_lane_is_not_live() {
        let output = rendered(&report(Vec::new()), Some("ended-lane"));
        assert!(output.contains("no live instance in #ended-lane"));
    }

    #[test]
    fn human_show_omits_trailing_hints() {
        let mut second = live_instance();
        second.channel = "feat-y".to_owned();
        for instances in [vec![], vec![live_instance()], vec![live_instance(), second]] {
            for lane in [None, Some("feat-x")] {
                let output = rendered(&report(instances.clone()), lane);
                for hint in ["Reach:", "Focus:", "Launch:", "Resume:"] {
                    assert!(!output.contains(hint));
                }
                assert!(!output.ends_with("\n\n"));
            }
        }
    }

    #[test]
    fn human_show_elides_only_the_matching_branch() {
        let mut instance = live_instance();
        assert!(
            !rendered(&report(vec![instance.clone()]), Some("feat-x")).contains("branch feat-x")
        );
        instance.worktree = Some("/repo/worktrees/another".into());
        assert!(
            rendered(&report(vec![instance.clone()]), Some("feat-x")).contains("branch feat-x")
        );
        instance.worktree = None;
        assert!(rendered(&report(vec![instance]), Some("feat-x")).contains("branch feat-x"));
    }

    #[test]
    fn human_show_describes_sandbox_tmp_mount() {
        let mut instance = live_instance();
        instance.isolation = Isolation::Sandbox;
        instance.tmp_dir = "/state/room/tmp".into();
        let output = rendered(&report(vec![instance]), Some("feat-x"));
        assert!(output.contains("isolation: sandbox · tmp /state/room/tmp (as /tmp)"));
    }

    #[test]
    fn human_show_aligns_memory_columns_by_display_width() {
        let mut instance = live_instance();
        instance.memory = ["blackboard.md", "笔记.md", "é.md"]
            .into_iter()
            .map(|name| super::super::list::MemoryReport {
                path: instance.worktree.as_ref().unwrap().join(name),
                lines: 41,
                modified_at: None,
            })
            .collect();
        let output = rendered(&report(vec![instance]), Some("feat-x"));
        let columns = output
            .lines()
            .filter_map(|line| {
                line.split_once("41 lines")
                    .map(|(prefix, _)| prefix.width())
            })
            .collect::<Vec<_>>();
        assert_eq!(columns.len(), 3);
        assert!(columns.iter().all(|column| *column == columns[0]));
    }

    #[test]
    fn lane_selection_includes_every_live_team_even_without_a_definition() {
        let mut orphan = report(vec![live_instance()]);
        orphan.name = "orphan".into();
        orphan.defined = false;
        let reports = vec![report(vec![live_instance()]), report(vec![]), orphan];
        let selected = select_reports(&reports, None).unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|report| report.name.as_str())
                .collect::<Vec<_>>(),
            ["forge", "orphan"]
        );
        assert_eq!(select_reports(&reports, Some("forge")).unwrap().len(), 1);
        assert!(select_reports(&reports, Some("orphan")).is_err());
        assert!(select_reports(&reports, Some("missing")).is_err());
        assert!(select_reports(&[report(vec![])], None).unwrap().is_empty());
    }
}
