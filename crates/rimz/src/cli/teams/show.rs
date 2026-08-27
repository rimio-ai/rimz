use std::io::Write;

use anyhow::{Result, bail};

use super::super::{GlobalFlags, render};
use super::list::{LiveMember, RoleReport, TeamReport};

pub(super) fn run(name: &str, lane: Option<&str>, json: bool, globals: &GlobalFlags) -> Result<()> {
    let reports = super::list::load_catalog(globals, lane)?;
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
    write_report(&mut render::out(), &report, lane)
}

fn write_report(w: &mut impl Write, report: &TeamReport, lane: Option<&str>) -> Result<()> {
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
    } else if !report.instances.is_empty() {
        writeln!(w)?;
        writeln!(
            w,
            "{}",
            render::paint(render::palette::header(), "Live instances")
        )?;
        let mut live = render::Table::new(["LANE", "MEMBER", "STATUS", "CTX", "COST"])
            .indent(2)
            .right(&[3, 4]);
        for instance in &report.instances {
            for member in &instance.members {
                live.row(member_cells(&instance.channel, member));
            }
        }
        live.render(w)?;
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

fn member_cells(channel: &str, member: &LiveMember) -> [render::Cell; 5] {
    [
        render::cell(format!("#{channel}")).fg(render::palette::meta()),
        render::cell(&member.handle).fg(render::palette::identity(&member.kind)),
        render::cell(member.status.as_str()).fg(render::status::agent(member.status, member.phase)),
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
            members: vec![LiveMember {
                handle: "@planner".to_owned(),
                kind: "claude".to_owned(),
                status: AgentStatus::Running,
                phase: TurnPhase::Reasoning,
                context_fill_pct: Some(42.0),
                cost_usd: Some(0.25),
            }],
        }
    }

    fn rendered(report: &TeamReport, lane: Option<&str>) -> String {
        let mut output = anstream::StripStream::new(Vec::new());
        write_report(&mut output, report, lane).unwrap();
        String::from_utf8(output.into_inner()).unwrap()
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
