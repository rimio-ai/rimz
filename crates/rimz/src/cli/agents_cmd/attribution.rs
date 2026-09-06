//! `rimz agents attribution` audit collection and presentation.

use std::io::Write;

use super::*;
use crate::cli::render;
use rimz::agents::attribution::{
    Attribution, AttributionGroup, AttributionMember, AttributionRequest, AttributionScope,
    EffortTotals, LaneLifetimes, ModelStat, TokenSplit,
};

const REPO_URL: &str = "https://github.com/rimio-ai/rimz";

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
    let snapshot =
        ctx.fold_agent_context(rimz::store::snapshot::SidebarSnapshot::build_with_agents(
            ctx.workspace.workspace_id.clone(),
            projection.agents,
            jiff::Timestamp::now(),
        ));
    let peers = rimz::harness::target::addressable_agents(&snapshot);
    let channel = super::list::list_channel_filter(all, scope.as_deref(), &ctx.workspace);
    let default_worktree =
        (!all && channel.is_none()).then_some(ctx.workspace.worktree_root.as_path());
    let mut roots = Vec::new();
    let mut children = Vec::new();
    let mut subagents = Vec::new();
    for agent in &snapshot.agents {
        if agent.is_provider_subagent() {
            subagents.push(agent);
        } else if agent.is_launched_child() {
            children.push(agent);
        } else {
            roots.push(agent);
        }
    }
    let roots = roots
        .into_iter()
        .filter(|agent| {
            if let Some(filter) = channel.as_deref() {
                rimz::harness::target::agent_in_worktree(agent, filter)
            } else if let Some(worktree) = default_worktree {
                agent
                    .worktree_path
                    .as_deref()
                    .is_some_and(|path| std::path::Path::new(path) == worktree)
            } else {
                true
            }
        })
        .collect::<Vec<_>>();
    // Eligible children follow their durable parent link across lane selection.
    let agents = roots.iter().copied().chain(children).collect::<Vec<_>>();
    let lifetimes = rimz::worktree::lane_lifetimes(agents.iter().copied());
    render::warn_unreadable_lanes(&lifetimes);
    let report_scope = report_scope(scope, channel, default_worktree, &roots, &lifetimes);
    let transcript =
        rimz::transcript::read_all(ctx.store.paths()).context("reading conversation transcript")?;
    let me = super::report::SelfIdentity::from_env().resolve(&snapshot);
    let active_grace_secs = crate::cli::machine_config()
        .agents
        .attention
        .active_grace_secs
        .get();
    let now = jiff::Timestamp::now();
    let active_secs = rimz::store::active_time::read_for_keys(
        ctx.runtime(),
        agents
            .iter()
            .filter(|agent| !agent.is_launched_child())
            .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
    )
    .into_iter()
    .map(|record| {
        (
            (record.kind.clone(), record.agent_id.clone()),
            record.display_secs(now, active_grace_secs),
        )
    })
    .collect();
    let report = rimz::agents::attribution::build(AttributionRequest {
        agents: &agents,
        lifetimes: &lifetimes,
        peers: &peers,
        subagents: &subagents,
        transcript: &transcript,
        me: me.as_ref(),
        active_secs: &active_secs,
        pricing_cache_path: &ctx.runtime().shared_pricing_cache_path(),
        require_contribution: md,
        scope: report_scope,
        now,
    });

    if json {
        return render::json_pretty(&report);
    }
    let mut out = render::out();
    if md {
        return render::finish(render_markdown(&mut out, &report));
    }
    render::finish(render_panel(&mut out, &report))
}

fn report_scope(
    selector: Option<String>,
    filter: Option<String>,
    default_worktree: Option<&std::path::Path>,
    agents: &[&AgentState],
    lifetimes: &LaneLifetimes,
) -> AttributionScope {
    let channel = common_optional(agents.iter().map(|agent| agent.channel())).or(filter);
    AttributionScope {
        selector,
        channel,
        branch: common_optional(agents.iter().map(|agent| agent.worktree_branch.clone())),
        worktree: default_worktree
            .map(|path| path.display().to_string())
            .or_else(|| common_optional(agents.iter().map(|agent| agent.worktree_path.clone()))),
        since: lifetimes.common_since(agents),
    }
}

fn common_optional(values: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let mut values = values.into_iter();
    let first = values.next()??;
    values
        .all(|value| value.as_deref() == Some(first.as_str()))
        .then_some(first)
}

pub(super) fn render_panel(w: &mut impl Write, report: &Attribution) -> std::io::Result<()> {
    if let Some(since) = report.scope.since {
        writeln!(
            w,
            "{}",
            render::paint(render::palette::muted(), &since_label(since))
        )?;
    }
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

    let show_captions = report.groups.len() > 1 || report.groups[0].team.as_ref().is_some();
    for (group_index, group) in report.groups.iter().enumerate() {
        if group_index > 0 {
            writeln!(w)?;
        }
        if show_captions {
            writeln!(
                w,
                "{}",
                render::paint(
                    render::palette::header(),
                    &format!("{} · {}", group_name(group), totals_label(&group.totals))
                )
            )?;
            writeln!(w)?;
        }
        for (member_index, member) in group.members.iter().enumerate() {
            if member_index > 0 {
                writeln!(w)?;
            }
            writeln!(
                w,
                "  {} · {} · {}",
                render::paint(
                    render::palette::identity(member.kind.as_str()),
                    &identity_label(member)
                ),
                member.provider,
                render::paint(render::palette::muted(), &model_label(member))
            )?;
            let mut details = render::KeyVals::new().indent(6);
            if let Some(effort) = effort_label(member) {
                details.push("effort", render::cell(effort));
            }
            if let Some(subagents) = subagents_label(member) {
                details.push("subagents", render::cell(subagents));
            }
            if let Some(activity) = activity_label(member) {
                details.push("activity", render::cell(activity));
            }
            if let Some(messages) = messages_label(member) {
                details.push("messages", render::cell(messages));
            }
            if let Some(tokens) = token_split_label(&member.tokens) {
                details.push("tokens", render::cell(tokens));
            }
            details.render(w)?;
        }
    }
    if !report.models.is_empty() {
        writeln!(w)?;
        writeln!(w, "{}", render::paint(render::palette::header(), "Models"))?;
        let mut models = render::KeyVals::new().indent(2);
        for stat in &report.models {
            models.push(model_name(stat), render::cell(model_row_label(stat)));
        }
        models.render(w)?;
    }
    if show_captions && report.groups.len() == 1 {
        return Ok(());
    }
    writeln!(w, "\nTotal · {}", totals_label(&report.totals))
}

pub(super) fn render_markdown(w: &mut impl Write, report: &Attribution) -> std::io::Result<()> {
    if report.groups.is_empty() {
        return Ok(());
    }
    writeln!(w, "<details>")?;
    write!(
        w,
        "<summary>{} · {}",
        markdown_summary_subject(report),
        totals_label(&report.totals)
    )?;
    if let Some(since) = report.scope.since {
        write!(w, " · {}", since_label(since))?;
    }
    writeln!(w, "</summary>")?;
    writeln!(w, "\n<br/>\n\n**Agents**\n")?;
    let show_captions = report.groups.len() > 1;
    for (index, group) in report.groups.iter().enumerate() {
        if index > 0 {
            writeln!(w)?;
        }
        if show_captions {
            writeln!(w, "**{}**", markdown_escape(&group_name(group)))?;
            writeln!(w)?;
        }
        for member in &group.members {
            let role = member.role.as_deref().unwrap_or(member.handle.as_str());
            writeln!(
                w,
                "- **{}** — {} {}",
                markdown_escape(role),
                markdown_escape(&member.provider),
                markdown_code(&model_label(member)),
            )?;
            if let Some(effort) = effort_label(member) {
                writeln!(w, "  - effort: {}", markdown_escape(&effort))?;
            }
            if let Some(subagents) = subagents_label(member) {
                writeln!(w, "  - subagents: {}", markdown_escape(&subagents))?;
            }
            if let Some(activity) = activity_label(member) {
                writeln!(w, "  - activity: {}", markdown_escape(&activity))?;
            }
            if let Some(messages) = messages_label(member) {
                writeln!(w, "  - messages: {}", markdown_escape(&messages))?;
            }
            if let Some(tokens) = token_split_label(&member.tokens) {
                writeln!(w, "  - tokens: {}", markdown_escape(&tokens))?;
            }
        }
    }
    if !report.models.is_empty() {
        writeln!(w, "\n**Models**\n")?;
        for stat in &report.models {
            let name = stat
                .model
                .as_deref()
                .map_or_else(|| "unknown".to_owned(), markdown_code);
            writeln!(w, "- {name} — {}", markdown_escape(&model_row_label(stat)))?;
        }
    }
    writeln!(w)?;
    writeln!(w, "</details>")
}

fn markdown_summary_subject(report: &Attribution) -> String {
    let rimz = format!(r#"<a href="{REPO_URL}">RimZ</a>"#);
    match report.groups.as_slice() {
        [
            AttributionGroup {
                team: Some(team), ..
            },
        ] => {
            format!(
                "Implemented by the {rimz} <code>{}</code> team",
                html_escape(&team.name),
            )
        }
        [group] if group.members.len() == 1 => {
            let member = &group.members[0];
            format!(
                "Implemented with {rimz} by {}",
                html_escape(&member.provider)
            )
        }
        _ => format!("Implemented by {rimz} agents"),
    }
}

fn group_name(group: &AttributionGroup) -> String {
    group.team.as_ref().map_or_else(
        || "Other agents".to_owned(),
        |team| format!("{} team", team.name),
    )
}

fn since_label(since: jiff::Timestamp) -> String {
    format!(
        "since {}",
        since
            .to_zoned(crate::cli::machine_config().time_zone())
            .strftime("%Y-%m-%d %H:%M")
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

fn identity_label(member: &AttributionMember) -> String {
    let handle_role = member
        .handle
        .trim_start_matches('@')
        .split_once('#')
        .map_or(member.handle.trim_start_matches('@'), |(handle, _)| handle);
    match member.role.as_deref() {
        Some(role) if role != handle_role => format!("{} ({role})", member.handle),
        _ => member.handle.clone(),
    }
}

/// A code span renders its contents verbatim, so the span branch only flattens
/// newlines; the backtick fallback is plain Markdown text and takes the full escape.
fn markdown_code(label: &str) -> String {
    if label.contains('`') {
        markdown_escape(label)
    } else {
        format!("`{}`", label.replace(['\r', '\n'], " "))
    }
}

fn effort_label(member: &AttributionMember) -> Option<String> {
    let mut parts = Vec::with_capacity(2);
    if let Some(active) = member
        .active_secs
        .map(|seconds| format!("{} active", duration_label(seconds)))
    {
        parts.push(active);
    }
    if let Some(cost) = member.cost_usd.map(rimz::theme::fmt::dollars2) {
        parts.push(cost);
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn activity_label(member: &AttributionMember) -> Option<String> {
    let mut parts = Vec::with_capacity(3);
    match member.asks {
        0 => {}
        1 => parts.push("1 ask".to_owned()),
        count => parts.push(format!("{count} asks")),
    }
    match member.tool_calls {
        0 => {}
        1 => parts.push("1 tool call".to_owned()),
        count => parts.push(format!("{count} tool calls")),
    }
    match member.compactions {
        0 => {}
        1 => parts.push("1 compaction".to_owned()),
        count => parts.push(format!("{count} compactions")),
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn messages_label(member: &AttributionMember) -> Option<String> {
    [
        (member.messages.from_user, "from you"),
        (member.messages.from_teammates, "from teammates"),
        (member.messages.to_teammates, "to teammates"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, name)| format!("{count} {name}"))
    .reduce(|mut label, component| {
        label.push_str(" · ");
        label.push_str(&component);
        label
    })
}

fn model_name(stat: &ModelStat) -> &str {
    stat.model.as_deref().unwrap_or("unknown")
}

fn model_row_label(stat: &ModelStat) -> String {
    [
        stat.cost_usd.map(rimz::theme::fmt::dollars2),
        token_split_label(&stat.tokens),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn token_split_label(tokens: &TokenSplit) -> Option<String> {
    [
        (tokens.input, "input"),
        (tokens.output, "output"),
        (tokens.cache_write, "cache write"),
        (tokens.cache_read, "cache read"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, name)| format!("{} {name}", token_count(count)))
    .reduce(|mut label, component| {
        label.push_str(", ");
        label.push_str(&component);
        label
    })
}

fn subagents_label(member: &AttributionMember) -> Option<String> {
    if member.subagents.is_empty() {
        return None;
    }
    let mut cost_usd = None;
    let mut segments = Vec::with_capacity(member.subagents.len());
    for stat in &member.subagents {
        let task = stat.task.as_deref().unwrap_or("other");
        segments.push(format!("{} × {task}", stat.count));
        cost_usd = rimz::agents::spending::sum_optional_cost(cost_usd, stat.cost_usd);
    }
    let mut label = segments.join(", ");
    if let Some(cost) = cost_usd {
        label.push_str(" · ");
        label.push_str(&rimz::theme::fmt::dollars2(cost));
    }
    Some(label)
}

fn token_count(value: u64) -> String {
    if value < 1_000 {
        return value.to_string();
    }
    let units = [
        (1_000_u64, "k"),
        (1_000_000, "m"),
        (1_000_000_000, "b"),
        (1_000_000_000_000, "t"),
        (1_000_000_000_000_000, "q"),
        (1_000_000_000_000_000_000, "e"),
    ];
    let mut unit = 0;
    while unit + 1 < units.len() && rounded_tenths(value, units[unit].0) >= 10_000 {
        unit += 1;
    }
    let tenths = rounded_tenths(value, units[unit].0);
    if tenths.is_multiple_of(10) {
        format!("{}{}", tenths / 10, units[unit].1)
    } else {
        format!("{}.{}{}", tenths / 10, tenths % 10, units[unit].1)
    }
}

fn rounded_tenths(value: u64, divisor: u64) -> u64 {
    u64::try_from((u128::from(value) * 10 + u128::from(divisor / 2)) / u128::from(divisor))
        .unwrap_or(u64::MAX)
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
    if let Some(active) = totals
        .active_secs
        .map(|seconds| format!("{} active", duration_label(seconds)))
    {
        parts.push(active);
    }
    if let Some(cost) = totals.cost_usd.map(rimz::theme::fmt::dollars2) {
        parts.push(cost);
    }
    let messages = totals
        .messages
        .from_user
        .saturating_add(totals.messages.from_teammates);
    if messages > 0 {
        let from_you = if totals.messages.from_user > 0 {
            format!(" ({} from you)", totals.messages.from_user)
        } else {
            String::new()
        };
        parts.push(format!("{messages} messages{from_you}"));
    }
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
    html_escape(value)
        .replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('[', "\\[")
        .replace(']', "\\]")
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
mod tests;
