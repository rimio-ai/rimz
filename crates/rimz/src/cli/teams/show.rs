use std::io::Write;

use anyhow::{Context, Result, bail};

use super::super::{GlobalFlags, render};
use super::list::{LiveInstance, LiveMember, RoleReport, TeamReport, ci_style, stage_label};
use rimz::store::snapshot::{WorktreePrCi, WorktreePrState};

pub(super) fn run(name: &str, lane: Option<&str>, json: bool, globals: &GlobalFlags) -> Result<()> {
    let machine = rimz::config::MachineConfig::load().context("loading machine config")?;
    let reports = super::list::load_catalog(globals, lane, &machine)?;
    let Some(report) = reports
        .iter()
        .find(|report| report.name == name && report.defined)
        .cloned()
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
    if json {
        return render::json_pretty(&report);
    }
    write_report(&mut render::out(), &report, lane, jiff::Timestamp::now())
}

fn write_report(
    w: &mut impl Write,
    report: &TeamReport,
    lane: Option<&str>,
    now: jiff::Timestamp,
) -> Result<()> {
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
    definition.push(
        "leader",
        render::cell(report.leader.as_deref().unwrap_or("-")).dash(),
    );
    definition.push(
        "validation",
        render::cell(
            report
                .error
                .as_deref()
                .map_or_else(|| "ready".to_owned(), |error| format!("broken: {error}")),
        )
        .fg(if report.valid {
            render::palette::good()
        } else {
            render::palette::alarm()
        }),
    );
    definition.render(w)?;
    writeln!(w)?;

    let mut roles = render::Table::new(["ROLE", "PROFILE", "KIND", "MODEL", "EFFORT", "MODE"])
        .indent(2)
        .max_width(render::terminal_columns(120));
    for role in &report.roles {
        roles.row(role_cells(role));
    }
    roles.render(w)?;
    if report.roles.iter().any(|role| {
        role.system_prompt_file.is_some() || !role.append_system_prompt_files.is_empty()
    }) {
        writeln!(
            w,
            "  {}",
            render::paint(
                render::palette::muted(),
                &format!("(prompt stack: rimz teams show {} --json)", report.name)
            )
        )?;
    }

    if report.roles.iter().any(|role| !role.signals.is_empty()) {
        writeln!(w)?;
        writeln!(
            w,
            "{}",
            render::paint(render::palette::header(), "Declared signals")
        )?;
        let mut signals = render::Table::new(["ROLE", "SIGNAL", "MATCH", "PROMPT"])
            .indent(2)
            .max_width(render::terminal_columns(120));
        for role in &report.roles {
            for signal in &role.signals {
                signals.row([
                    render::cell(&role.role).fg(render::palette::accent()),
                    render::cell(&signal.signal),
                    render::cell(signal_matches(&signal.matches)).dash(),
                    render::cell(
                        signal
                            .prompt
                            .as_deref()
                            .map(render::one_line)
                            .unwrap_or_else(|| "-".to_owned()),
                    )
                    .dash(),
                ]);
            }
        }
        signals.render(w)?;
    }

    if report.instances.is_empty() && lane.is_some() {
        writeln!(w)?;
        writeln!(
            w,
            "{}",
            render::paint(
                render::palette::muted(),
                &format!("no live instance in #{}", lane.unwrap_or_default())
            )
        )?;
    }
    for instance in &report.instances {
        write_instance(w, instance, now)?;
    }

    if report
        .instances
        .iter()
        .flat_map(|instance| &instance.members)
        .any(|member| !member.signals.is_empty())
    {
        writeln!(w)?;
        writeln!(
            w,
            "{}",
            render::paint(render::palette::header(), "Live signals")
        )?;
        let mut signals = render::Table::new(["LANE", "MEMBER", "NAME", "SIGNAL", "MATCH"])
            .indent(2)
            .max_width(render::terminal_columns(120));
        for instance in &report.instances {
            for member in &instance.members {
                for signal in &member.signals {
                    signals.row([
                        render::cell(format!("#{}", instance.channel)).fg(render::palette::meta()),
                        render::cell(&member.handle).fg(render::palette::identity(&member.kind)),
                        render::cell(&signal.name),
                        render::cell(&signal.selector),
                        render::cell(signal_matches(&signal.matches)).dash(),
                    ]);
                }
            }
        }
        signals.render(w)?;
    }

    if report.instances.is_empty() {
        writeln!(w)?;
        writeln!(w, "Launch: rimz teams {} -w <worktree>", report.name)?;
        writeln!(w, "Resume: rimz teams resume {}", report.name)?;
        return Ok(());
    }
    if let Some((handle, channel)) = live_target(report) {
        writeln!(w)?;
        writeln!(
            w,
            "Reach: rimz message @{}#{} '<text>'",
            handle.trim_start_matches('@'),
            channel
        )?;
        writeln!(w, "Focus: rimz teams focus {}#{}", report.name, channel)?;
    }
    Ok(())
}

fn write_instance(w: &mut impl Write, instance: &LiveInstance, now: jiff::Timestamp) -> Result<()> {
    writeln!(w)?;
    let stage = instance
        .stage
        .as_ref()
        .map(|stage| format!(" · {}", stage_label(stage)))
        .unwrap_or_default();
    writeln!(
        w,
        "{}",
        render::paint(
            render::palette::header(),
            &format!("#{}{stage}", instance.channel)
        )
    )?;
    let mut facts = render::KeyVals::new().indent(2);
    let mut checkout = instance
        .worktree
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "-".to_owned());
    if let Some(branch) = &instance.branch {
        checkout.push_str(&format!(" · branch {branch}"));
    }
    facts.push("worktree", render::cell(checkout).dash());
    if !instance.stages.is_empty() {
        let current = instance
            .stage
            .as_ref()
            .and_then(|stage| stage.name.split_whitespace().next());
        facts.push(
            "stages",
            render::cell(
                instance
                    .stages
                    .iter()
                    .map(|stage| {
                        if current == Some(stage.as_str()) {
                            format!("[{stage}]")
                        } else {
                            stage.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" → "),
            ),
        );
    }
    let mut pr = Vec::new();
    if let Some(report) = &instance.pr {
        if let Some(number) = report.number {
            pr.push(format!("#{number}"));
        }
        if let Some(state) = report.state {
            let (label, style) = match state {
                WorktreePrState::Open => ("open", render::palette::accent()),
                WorktreePrState::Merged => ("merged", render::palette::good()),
                WorktreePrState::Closed => ("closed", render::palette::muted()),
            };
            pr.push(render::paint(style, label));
        }
        if let Some(ci) = report.ci {
            let label = match ci {
                WorktreePrCi::Passing => "passing",
                WorktreePrCi::Pending => "pending",
                WorktreePrCi::Failing => "failing",
            };
            pr.push(render::paint(ci_style(ci), &format!("ci {label}")));
        }
        if let Some(url) = &report.url {
            pr.push(url.clone());
        }
    }
    facts.push(
        "pr",
        render::cell(if pr.is_empty() {
            "none".to_owned()
        } else {
            pr.join(" · ")
        }),
    );
    if !instance.memory.is_empty() {
        facts.push_lines(
            "memory",
            instance.memory.iter().map(|file| {
                let age = file
                    .modified_at
                    .map(|time| render::rel_age(time, now))
                    .unwrap_or_else(|| "-".to_owned());
                vec![render::cell(format!(
                    "{}  {} lines · {age}",
                    file.path.display(),
                    file.lines
                ))]
            }),
        );
    }
    facts.render(w)?;
    writeln!(w)?;
    let mut live = render::Table::new(["MEMBER", "STATUS", "ACTIVITY", "CTX", "COST", "AGE"])
        .indent(2)
        .right(&[3, 4, 5])
        .max_width(render::terminal_columns(120));
    for member in &instance.members {
        live.row(member_cells(member, now));
    }
    live.render(w)?;
    Ok(())
}

fn signal_matches(matches: &std::collections::BTreeMap<String, String>) -> String {
    if matches.is_empty() {
        return "-".to_owned();
    }
    matches
        .iter()
        .map(|(key, value)| render::one_line(&format!("{key}={value}")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn live_target(report: &TeamReport) -> Option<(&str, &str)> {
    if report.instances.len() != 1 {
        return None;
    }
    let instance = report.instances.first()?;
    let member = report
        .leader
        .as_deref()
        .and_then(|leader| {
            instance
                .members
                .iter()
                .find(|member| member.handle.trim_start_matches('@') == leader)
        })
        .or_else(|| instance.members.first())?;
    Some((&member.handle, &instance.channel))
}

fn role_cells(role: &RoleReport) -> [render::Cell; 6] {
    [
        render::cell(&role.role).fg(render::palette::accent()),
        render::cell(&role.profile),
        render::cell(role.kind.as_deref().unwrap_or("-")).dash(),
        render::cell(role.model.as_deref().unwrap_or("-")).dash(),
        render::cell(role.effort.as_deref().unwrap_or("-")).dash(),
        render::cell(role.mode.as_deref().unwrap_or("-")).dash(),
    ]
}

fn member_cells(member: &LiveMember, now: jiff::Timestamp) -> [render::Cell; 6] {
    [
        render::cell(&member.handle).fg(render::palette::identity(&member.kind)),
        render::cell(member.status.as_str()).fg(render::status::agent(member.status, member.phase)),
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
        render::cell(render::age_short(member.last_activity_at, now)).fg(render::palette::muted()),
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
                append_system_prompt_files: vec!["consensus.md".into()],
            }],
            valid: true,
            error: None,
            instances,
        }
    }

    fn live_instance() -> LiveInstance {
        LiveInstance {
            channel: "feat-x".to_owned(),
            state: "running".to_owned(),
            status_counts: BTreeMap::from([("running".to_owned(), 1)]),
            worktree: Some("/repo/worktrees/feat-x".into()),
            branch: Some("feat-x".to_owned()),
            stages: vec!["Explore".into(), "Plan".into(), "Implement".into()],
            stage: Some(super::super::list::StageReport {
                name: "Plan (delta)".into(),
                owner: Some("planner".into()),
            }),
            pr: None,
            memory: Vec::new(),
            members: vec![LiveMember {
                role: Some("planner".to_owned()),
                signals: vec![super::super::list::LiveSignal {
                    name: "team-forge-feat-x-planner-ci-failed".to_owned(),
                    selector: "ci.failed".to_owned(),
                    matches: BTreeMap::from([("path".to_owned(), "/repo/feat-x".to_owned())]),
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
        let output = rendered(&report(vec![instance.clone()]), None);
        assert!(output.contains("Explore → [Plan] → Implement"));
        assert!(output.contains("Plan (delta) (@planner)"));
        assert!(output.contains("ci passing"));
        assert!(output.contains("/repo/worktrees/feat-x/blackboard.md"));
        assert!(output.contains("41 lines · 2m ago"));
        assert!(
            output
                .lines()
                .any(|line| line.contains("@planner") && line.ends_with("2m"))
        );
        instance.stage.as_mut().unwrap().name = "Done".into();
        assert!(rendered(&report(vec![instance]), None).contains("Explore → Plan → Implement"));
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
        assert_eq!(member["role"], "planner");
        assert_eq!(
            member["signals"],
            serde_json::json!([{
                "name": "team-forge-feat-x-planner-ci-failed",
                "selector": "ci.failed", "matches": {"path": "/repo/feat-x"}
            }])
        );
    }

    #[test]
    fn human_show_with_live_instance() {
        insta::assert_snapshot!(rendered(&report(vec![live_instance()]), None));
    }

    #[test]
    fn human_show_without_live_instance() {
        insta::assert_snapshot!(rendered(&report(Vec::new()), Some("ended-lane")));
    }

    #[test]
    fn human_show_answers_when_a_selected_lane_is_not_live() {
        let output = rendered(&report(Vec::new()), Some("ended-lane"));
        assert!(output.contains("no live instance in #ended-lane"));
    }

    #[test]
    fn human_show_omits_ambiguous_reach_hint() {
        let mut second = live_instance();
        second.channel = "feat-y".to_owned();
        let report = report(vec![live_instance(), second]);

        for lane in [None, Some("feat-x")] {
            let output = rendered(&report, lane);
            assert!(!output.contains("Reach:"));
            assert!(!output.contains("Focus:"));
            assert!(!output.ends_with("\n\n"));
        }
    }
}
