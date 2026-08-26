use std::io::Write;

use anyhow::{Result, bail};

use super::super::{GlobalFlags, render};
use super::list::{LiveMember, RoleReport, TeamReport};

pub(super) fn run(name: &str, lane: Option<&str>, json: bool, globals: &GlobalFlags) -> Result<()> {
    let reports = super::list::load_catalog(globals)?;
    let Some(mut report) = reports
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
    if let Some(lane) = lane {
        report.instances.retain(|instance| instance.channel == lane);
    }
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
        render::cell(report.source.as_deref().unwrap_or("-")).dash(),
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

    let mut roles = render::Table::new([
        "ROLE", "PROFILE", "KIND", "MODEL", "EFFORT", "MODE", "PROMPT",
    ])
    .indent(2)
    .max_width(render::terminal_columns(120));
    for role in &report.roles {
        roles.row(role_cells(role));
    }
    roles.render(w)?;

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

    writeln!(w)?;
    writeln!(w, "Launch: rimz teams launch {} -w <worktree>", report.name)?;
    writeln!(w, "Resume: rimz teams resume {}", report.name)?;
    Ok(())
}

fn role_cells(role: &RoleReport) -> [render::Cell; 7] {
    let prompt = role
        .system_prompt_file
        .as_deref()
        .map(|path| path.display().to_string())
        .into_iter()
        .collect::<Vec<_>>();
    let prompt = if prompt.is_empty() {
        "-".to_owned()
    } else {
        prompt.join(" ")
    };
    [
        render::cell(&role.role).fg(render::palette::accent()),
        render::cell(&role.profile),
        render::cell(role.kind.as_deref().unwrap_or("-")).dash(),
        render::cell(role.model.as_deref().unwrap_or("-")).dash(),
        render::cell(role.effort.as_deref().unwrap_or("-")).dash(),
        render::cell(role.mode.as_deref().unwrap_or("-")).dash(),
        render::cell(prompt).dash(),
    ]
}

fn member_cells(channel: &str, member: &LiveMember) -> [render::Cell; 5] {
    [
        render::cell(format!("#{channel}")).fg(render::palette::meta()),
        render::cell(&member.handle).fg(render::palette::identity(&member.kind)),
        render::cell(&member.status),
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
    use std::collections::BTreeMap;

    #[test]
    fn human_show_includes_definition_live_members_and_hints() {
        let report = TeamReport {
            name: "forge".to_owned(),
            defined: true,
            source: Some("/home/me/.agents/teams/forge/team.toml".to_owned()),
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
                append_system_prompt_files: Vec::new(),
            }],
            valid: true,
            error: None,
            instances: vec![LiveInstance {
                channel: "feat-x".to_owned(),
                state: "running".to_owned(),
                status_counts: BTreeMap::from([("running".to_owned(), 1)]),
                members: vec![LiveMember {
                    handle: "planner".to_owned(),
                    kind: "claude".to_owned(),
                    status: "running".to_owned(),
                    context_fill_pct: Some(42.0),
                    cost_usd: Some(0.25),
                }],
            }],
        };
        let mut output = Vec::new();
        write_report(&mut output, &report, None).unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("planner,coder+reviewer"));
        assert!(output.contains("#feat-x"));
        assert!(output.contains("42%"));
        assert!(output.contains("$0.25"));
        assert!(output.contains("rimz teams launch forge -w <worktree>"));
        assert!(output.contains("rimz teams resume forge"));
    }

    #[test]
    fn human_show_answers_when_a_selected_lane_is_not_live() {
        let report = TeamReport {
            name: "forge".to_owned(),
            defined: true,
            source: None,
            layout: Some("planner".to_owned()),
            leader: Some("planner".to_owned()),
            roles: Vec::new(),
            valid: true,
            error: None,
            instances: Vec::new(),
        };
        let mut output = Vec::new();

        write_report(&mut output, &report, Some("ended-lane")).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("no live instance in #ended-lane"));
    }
}
