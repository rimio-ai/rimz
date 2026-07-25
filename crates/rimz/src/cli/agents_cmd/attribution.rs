//! `rimz agents attribution` audit collection and presentation.

use std::io::Write;

use super::*;
use crate::cli::render;
use rimz::agents::attribution::{
    Attribution, AttributionGroup, AttributionMember, AttributionRequest, AttributionScope,
    EffortTotals,
};

pub(super) fn attribution(
    scope: Option<String>,
    all: bool,
    json: bool,
    md: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let projection = ctx
        .store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?;
    let snapshot = rimz::SidebarSnapshot::build_with_agents(
        ctx.workspace.workspace_id.clone(),
        projection.agents,
        jiff::Timestamp::now(),
    )
    .with_agent_context(rimz::store::agent_context::read_all(ctx.runtime()));
    let peers = snapshot.root_agents().collect::<Vec<_>>();
    let channel = super::list::list_channel_filter(all, scope.as_deref(), &ctx.workspace);
    let agents = peers
        .iter()
        .copied()
        .filter(|agent| {
            channel
                .as_deref()
                .is_none_or(|filter| rimz::harness::target::agent_in_worktree(agent, filter))
        })
        .collect::<Vec<_>>();
    let me = super::report::SelfIdentity::from_env().resolve(&snapshot);
    let report = rimz::agents::attribution::build(AttributionRequest {
        agents: &agents,
        peers: &peers,
        me: me.as_ref(),
        runtime: ctx.runtime(),
        active_grace_secs: crate::cli::machine_config()
            .agents
            .attention
            .active_grace_secs
            .get(),
        scope: report_scope(scope, channel, &agents),
        now: jiff::Timestamp::now(),
    });

    if json {
        return render::json_pretty(&report);
    }
    let mut out = render::out();
    if md {
        return render::finish(render_markdown(&mut out, &report));
    }
    render::finish(render_panel(
        &mut out,
        &report,
        render::terminal_columns(120),
    ))
}

fn report_scope(
    selector: Option<String>,
    filter: Option<String>,
    agents: &[&AgentState],
) -> AttributionScope {
    let channel = common_optional(
        agents
            .iter()
            .map(|agent| rimz::harness::target::agent_channel(agent)),
    )
    .or(filter);
    AttributionScope {
        selector,
        channel,
        branch: common_optional(agents.iter().map(|agent| agent.worktree_branch.clone())),
        worktree: common_optional(agents.iter().map(|agent| agent.worktree_path.clone())),
    }
}

fn common_optional(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let mut values = values.into_iter();
    let first = values.next()??;
    values
        .all(|value| value.as_deref() == Some(first.as_str()))
        .then_some(first)
}

pub(super) fn render_panel(
    w: &mut impl Write,
    report: &Attribution,
    max_width: usize,
) -> std::io::Result<()> {
    if report.groups.is_empty() {
        return writeln!(
            w,
            "{}",
            render::paint(
                render::palette::muted(),
                "No agent attribution records in this scope."
            )
        );
    }

    let mut table = render::Table::new(["AGENT", "PROVIDER", "MODEL", "ACTIVE", "TOOLS", "COST"])
        .right(&[3, 4, 5])
        .max_width(max_width);
    for group in &report.groups {
        table.section_cells(vec![
            render::cell(group_name(group)).fg(render::palette::header()),
            render::cell(format!("· {}", totals_label(&group.totals))).fg(render::palette::meta()),
        ]);
        for member in &group.members {
            let active = member
                .active_secs
                .map(duration_label)
                .unwrap_or_else(|| "-".to_owned());
            let cost = member
                .cost_usd
                .map(rimz::theme::fmt::dollars2)
                .unwrap_or_else(|| "-".to_owned());
            table.card(
                [
                    render::cell(member.handle.as_str())
                        .fg(render::palette::identity(member.kind.as_str())),
                    render::cell(member.provider.as_str()),
                    render::cell(model_label(member)).fg(render::palette::muted()),
                    render::cell(active).dash(),
                    render::cell(member.tool_calls.to_string()),
                    render::cell(cost).dash(),
                ],
                Some(render::cell(member_detail(member)).fg(render::palette::muted())),
            );
        }
    }
    table.render(w)?;
    writeln!(w, "\nTotal · {}", totals_label(&report.totals))
}

pub(super) fn render_markdown(w: &mut impl Write, report: &Attribution) -> std::io::Result<()> {
    if report.groups.is_empty() {
        return Ok(());
    }
    writeln!(w, "<details>")?;
    writeln!(
        w,
        "<summary>{} — {}</summary>",
        markdown_summary_subject(report),
        totals_label(&report.totals)
    )?;
    writeln!(w)?;
    let show_captions = report.groups.len() > 1 || report.groups[0].team.as_ref().is_some();
    for (index, group) in report.groups.iter().enumerate() {
        if index > 0 {
            writeln!(w)?;
        }
        if show_captions {
            writeln!(w, "**{}**", markdown_escape(&group_name(group)))?;
            writeln!(w)?;
        }
        writeln!(
            w,
            "| Role | Agent | Model | Active | Tools | Tokens | Cost |"
        )?;
        writeln!(w, "|---|---|---|--:|--:|---|--:|")?;
        for member in &group.members {
            let role = member.role.as_deref().unwrap_or(member.handle.as_str());
            let active = member
                .active_secs
                .map(duration_label)
                .unwrap_or_else(|| "-".to_owned());
            let cost = member
                .cost_usd
                .map(rimz::theme::fmt::dollars2)
                .unwrap_or_else(|| "-".to_owned());
            writeln!(
                w,
                "| {} | {} | {} | {} | {} | {} | {} |",
                markdown_escape(role),
                markdown_escape(&member.provider),
                markdown_escape(&model_label(member)),
                active,
                member.tool_calls,
                markdown_escape(&token_label(member)),
                cost,
            )?;
        }
    }
    writeln!(w)?;
    writeln!(w, "</details>")
}

fn markdown_summary_subject(report: &Attribution) -> String {
    match report.groups.as_slice() {
        [
            AttributionGroup {
                team: Some(team), ..
            },
        ] => {
            format!(
                "Implemented by the RimZ <code>{}</code> team",
                html_escape(&team.name)
            )
        }
        [group] if group.members.len() == 1 => {
            let member = &group.members[0];
            format!("Implemented with RimZ by {}", html_escape(&member.provider))
        }
        _ => "Implemented by RimZ agents".to_owned(),
    }
}

fn group_name(group: &AttributionGroup) -> String {
    group.team.as_ref().map_or_else(
        || "Other agents".to_owned(),
        |team| format!("{} team", team.name),
    )
}

fn model_label(member: &AttributionMember) -> String {
    match (member.model.as_deref(), member.effort.as_deref()) {
        (Some(model), Some(effort)) => format!("{model}@{effort}"),
        (Some(model), None) => model.to_owned(),
        (None, Some(effort)) => format!("@{effort}"),
        (None, None) => "-".to_owned(),
    }
}

fn member_detail(member: &AttributionMember) -> String {
    let sessions = if member.sessions == 1 {
        "1 session".to_owned()
    } else {
        format!("{} sessions", member.sessions)
    };
    let compactions = if member.compactions == 1 {
        "1 compaction".to_owned()
    } else {
        format!("{} compactions", member.compactions)
    };
    format!("{} · {sessions} · {compactions}", token_label(member))
}

fn token_label(member: &AttributionMember) -> String {
    let cache = member
        .tokens
        .cache_write
        .saturating_add(member.tokens.cache_read);
    format!(
        "{} in · {} out · {} cache",
        render::compact_count(member.tokens.input),
        render::compact_count(member.tokens.output),
        render::compact_count(cache)
    )
}

fn totals_label(totals: &EffortTotals) -> String {
    let mut parts = vec![format!(
        "{} {}",
        totals.agents,
        if totals.agents == 1 {
            "agent"
        } else {
            "agents"
        }
    )];
    parts.push(
        totals
            .active_secs
            .map(|seconds| format!("{} active", duration_label(seconds)))
            .unwrap_or_else(|| "active unknown".to_owned()),
    );
    parts.push(
        totals
            .cost_usd
            .map(rimz::theme::fmt::dollars2)
            .unwrap_or_else(|| "cost unknown".to_owned()),
    );
    parts.join(" · ")
}

fn duration_label(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    if seconds < 3_600 {
        return format!("{}m", seconds / 60);
    }
    if seconds < 86_400 {
        return format!("{}h{:02}m", seconds / 3_600, seconds % 3_600 / 60);
    }
    format!("{}d{:02}h", seconds / 86_400, seconds % 86_400 / 3_600)
}

fn markdown_escape(value: &str) -> String {
    html_escape(value).replace('|', "\\|")
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('`', "&#96;")
        .replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::attribution::{Presence, TeamRef, TokenSplit};
    use rimz::ids::AgentKind;

    fn member(handle: &str, role: Option<&str>, provider: &str, model: &str) -> AttributionMember {
        AttributionMember {
            handle: handle.to_owned(),
            role: role.map(ToOwned::to_owned),
            name: None,
            kind: AgentKind::new_unchecked(provider.to_ascii_lowercase()),
            provider: provider.to_owned(),
            model: Some(model.to_owned()),
            effort: Some("high".to_owned()),
            presence: Presence::Exited,
            me: false,
            launch_ordinal: Some(0),
            sessions: 2,
            registered_at: Some(jiff::Timestamp::UNIX_EPOCH),
            last_activity: jiff::Timestamp::UNIX_EPOCH,
            active_secs: Some(3_900),
            tool_calls: 7,
            compactions: 1,
            tokens: TokenSplit {
                input: 1_200,
                output: 800,
                cache_write: 2_000,
                cache_read: 3_000,
            },
            cost_usd: Some(1.25),
        }
    }

    fn report() -> Attribution {
        let team_member = member("@planner", Some("plan|ner"), "Claude", "fable`2");
        let stray = member("@codex", None, "Codex", "gpt-5.5");
        let group_totals = |members: &[AttributionMember]| EffortTotals {
            agents: u32::try_from(members.len()).expect("small fixture"),
            active_secs: Some(3_900 * members.len() as u64),
            wall_clock_secs: 4_000,
            cost_usd: Some(1.25 * members.len() as f64),
            tool_calls: 7 * members.len() as u64,
            compactions: members.len() as u32,
            tokens: TokenSplit {
                input: 1_200 * members.len() as u64,
                output: 800 * members.len() as u64,
                cache_write: 2_000 * members.len() as u64,
                cache_read: 3_000 * members.len() as u64,
            },
        };
        let team_members = vec![team_member];
        let other_members = vec![stray];
        Attribution {
            schema: 1,
            generated_at: jiff::Timestamp::UNIX_EPOCH,
            rimz_version: "test".to_owned(),
            scope: AttributionScope::default(),
            totals: group_totals(&[team_members[0].clone(), other_members[0].clone()]),
            groups: vec![
                AttributionGroup {
                    team: Some(TeamRef {
                        name: "forge".to_owned(),
                        roles: vec!["planner".to_owned()],
                    }),
                    totals: group_totals(&team_members),
                    members: team_members,
                },
                AttributionGroup {
                    team: None,
                    totals: group_totals(&other_members),
                    members: other_members,
                },
            ],
        }
    }

    #[test]
    fn panel_groups_team_and_stray_members() {
        let mut output = anstream::StripStream::new(Vec::new());
        render_panel(&mut output, &report(), 120).expect("render panel");
        insta::assert_snapshot!(String::from_utf8(output.into_inner()).expect("utf8"), @r"
        AGENT     PROVIDER  MODEL         ACTIVE  TOOLS   COST

        forge team · 1 agent · 1h05m active · $1.25
        @planner  Claude    fable`2@high   1h05m      7  $1.25
          1k in · 800 out · 5k cache · 2 sessions · 1 compaction

        Other agents · 1 agent · 1h05m active · $1.25
        @codex    Codex     gpt-5.5@high   1h05m      7  $1.25
          1k in · 800 out · 5k cache · 2 sessions · 1 compaction

        Total · 2 agents · 2h10m active · $2.50
        ");
    }

    #[test]
    fn markdown_escapes_cells_and_wraps_native_tables() {
        let mut output = Vec::new();
        render_markdown(&mut output, &report()).expect("render markdown");
        insta::assert_snapshot!(String::from_utf8(output).expect("utf8"), @r"
        <details>
        <summary>Implemented by RimZ agents — 2 agents · 2h10m active · $2.50</summary>

        **forge team**

        | Role | Agent | Model | Active | Tools | Tokens | Cost |
        |---|---|---|--:|--:|---|--:|
        | plan\|ner | Claude | fable&#96;2@high | 1h05m | 7 | 1k in · 800 out · 5k cache | $1.25 |

        **Other agents**

        | Role | Agent | Model | Active | Tools | Tokens | Cost |
        |---|---|---|--:|--:|---|--:|
        | @codex | Codex | gpt-5.5@high | 1h05m | 7 | 1k in · 800 out · 5k cache | $1.25 |

        </details>
        ");
    }

    #[test]
    fn empty_scope_is_muted_for_people_and_silent_for_markdown() {
        let mut report = report();
        report.groups.clear();
        report.totals = EffortTotals::default();
        let mut panel = anstream::StripStream::new(Vec::new());
        render_panel(&mut panel, &report, 120).expect("render panel");
        assert_eq!(
            String::from_utf8(panel.into_inner()).expect("utf8"),
            "No agent attribution records in this scope.\n"
        );
        let mut markdown = Vec::new();
        render_markdown(&mut markdown, &report).expect("render markdown");
        assert!(markdown.is_empty());
    }
}
