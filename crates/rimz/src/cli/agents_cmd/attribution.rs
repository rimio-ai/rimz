//! `rimz agents attribution` audit collection and presentation.

use std::io::Write;

use super::*;
use crate::cli::render;
use rimz::agents::attribution::{
    Attribution, AttributionGroup, AttributionMember, AttributionRequest, AttributionScope,
    EffortTotals,
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
    let snapshot = ctx.fold_agent_context(rimz::SidebarSnapshot::build_with_agents(
        ctx.workspace.workspace_id.clone(),
        projection.agents,
        jiff::Timestamp::now(),
    ));
    let peers = rimz::harness::target::addressable_agents(&snapshot);
    let channel = super::list::list_channel_filter(all, scope.as_deref(), &ctx.workspace);
    let default_worktree =
        (!all && channel.is_none()).then_some(ctx.workspace.worktree_root.as_path());
    let (subagents, roots) = snapshot
        .agents
        .iter()
        .partition::<Vec<_>, _>(|agent| agent.is_provider_subagent());
    let agents = roots
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
    let transcript =
        rimz::transcript::read_all(ctx.store.paths()).context("reading conversation transcript")?;
    let me = super::report::SelfIdentity::from_env().resolve(&snapshot);
    let report = rimz::agents::attribution::build(AttributionRequest {
        agents: &agents,
        peers: &peers,
        subagents: &subagents,
        transcript: &transcript,
        me: me.as_ref(),
        runtime: ctx.runtime(),
        active_grace_secs: crate::cli::machine_config()
            .agents
            .attention
            .active_grace_secs
            .get(),
        require_contribution: md,
        scope: report_scope(scope, channel, default_worktree, &agents),
        now: jiff::Timestamp::now(),
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
        worktree: default_worktree
            .map(|path| path.display().to_string())
            .or_else(|| common_optional(agents.iter().map(|agent| agent.worktree_path.clone()))),
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
            if let Some(calls) = calls_label(member) {
                details.push("calls", render::cell(calls));
            }
            if let Some(messages) = messages_label(member) {
                details.push("messages", render::cell(messages));
            }
            if let Some(tokens) = token_label(member) {
                details.push("tokens", render::cell(tokens));
            }
            if let Some(subagents) = subagents_label(member) {
                details.push("subagents", render::cell(subagents));
            }
            details.render(w)?;
        }
    }
    writeln!(w, "\nTotal · {}", totals_label(&report.totals))
}

pub(super) fn render_markdown(w: &mut impl Write, report: &Attribution) -> std::io::Result<()> {
    if report.groups.is_empty() {
        return Ok(());
    }
    writeln!(w, "<details>")?;
    writeln!(
        w,
        "<summary>{} · {}</summary>",
        markdown_summary_subject(report),
        totals_label(&report.totals)
    )?;
    writeln!(w)?;
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
                markdown_model_label(member),
            )?;
            if let Some(effort) = effort_label(member) {
                writeln!(w, "  - effort: {}", markdown_escape(&effort))?;
            }
            if let Some(calls) = calls_label(member) {
                writeln!(w, "  - calls: {}", markdown_escape(&calls))?;
            }
            if let Some(messages) = messages_label(member) {
                writeln!(w, "  - messages: {}", markdown_escape(&messages))?;
            }
            if let Some(tokens) = token_label(member) {
                writeln!(w, "  - tokens: {}", markdown_escape(&tokens))?;
            }
            if let Some(subagents) = subagents_label(member) {
                writeln!(w, "  - subagents: {}", markdown_escape(&subagents))?;
            }
        }
    }
    writeln!(w)?;
    writeln!(
        w,
        "<sub>Generated by [`rimz agents attribution`]({})</sub>",
        format!("{REPO_URL}/blob/main/docs/reference/cli/agents.md#attribution")
    )?;
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
fn markdown_model_label(member: &AttributionMember) -> String {
    let label = model_label(member);
    if label.contains('`') {
        markdown_escape(&label)
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
        parts.push(if member.subagents.is_empty() {
            cost
        } else {
            format!("{cost} incl. subagents")
        });
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn calls_label(member: &AttributionMember) -> Option<String> {
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

fn token_label(member: &AttributionMember) -> Option<String> {
    [
        (member.tokens.input, "input"),
        (member.tokens.output, "output"),
        (member.tokens.cache_write, "cache write"),
        (member.tokens.cache_read, "cache read"),
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
    member
        .subagents
        .iter()
        .map(|stat| {
            let mut segment = format!("{} ×", stat.count);
            if let Some(task) = stat.task.as_deref() {
                segment.push(' ');
                segment.push_str(&task.to_lowercase());
            } else {
                segment.push(' ');
                segment.push_str("other");
            }
            if let Some(cost) = stat.cost_usd {
                segment.push_str(" (");
                segment.push_str(&rimz::theme::fmt::dollars2(cost));
                segment.push(')');
            }
            segment
        })
        .reduce(|mut label, segment| {
            label.push_str(" · ");
            label.push_str(&segment);
            label
        })
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
mod tests {
    use super::*;
    use rimz::agents::attribution::{MessageCounts, Presence, SubagentStat, TeamRef, TokenSplit};
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
            asks: 2,
            asks_answered: 1,
            tool_calls: 7,
            compactions: 1,
            messages: MessageCounts {
                from_user: 2,
                from_teammates: 5,
                to_teammates: 4,
            },
            tokens: TokenSplit {
                input: 1_200,
                output: 800,
                cache_write: 2_000,
                cache_read: 3_000,
            },
            cost_usd: Some(1.25),
            subagents: vec![
                SubagentStat {
                    task: Some("Explorer".to_owned()),
                    count: 3,
                    cost_usd: Some(0.55),
                },
                SubagentStat {
                    task: None,
                    count: 1,
                    cost_usd: None,
                },
            ],
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
            asks: 2 * members.len() as u64,
            asks_answered: members.len() as u64,
            tool_calls: 7 * members.len() as u64,
            compactions: members.len() as u32,
            messages: MessageCounts {
                from_user: 2 * members.len() as u64,
                from_teammates: 5 * members.len() as u64,
                to_teammates: 4 * members.len() as u64,
            },
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
            schema: 3,
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
        render_panel(&mut output, &report()).expect("render panel");
        insta::assert_snapshot!(String::from_utf8(output.into_inner()).expect("utf8"), @r"
        forge team · 1 agent · 1h05m active · $1.25 · 7 messages (2 from you)

          @planner (plan|ner) · Claude · fable`2@high
              effort:    1h05m active · $1.25 incl. subagents
              calls:     2 asks · 7 tool calls · 1 compaction
              messages:  2 from you · 5 from teammates · 4 to teammates
              tokens:    1.2k input, 800 output, 2k cache write, 3k cache read
              subagents: 3 × explorer ($0.55) · 1 × other

        Other agents · 1 agent · 1h05m active · $1.25 · 7 messages (2 from you)

          @codex · Codex · gpt-5.5@high
              effort:    1h05m active · $1.25 incl. subagents
              calls:     2 asks · 7 tool calls · 1 compaction
              messages:  2 from you · 5 from teammates · 4 to teammates
              tokens:    1.2k input, 800 output, 2k cache write, 3k cache read
              subagents: 3 × explorer ($0.55) · 1 × other

        Total · 2 agents · 2h10m active · $2.50 · 14 messages (4 from you)
        ");
    }

    #[test]
    fn panel_omits_redundant_caption_for_single_teamless_group() {
        let mut report = report();
        let teamless = report.groups.pop().expect("teamless fixture group");
        report.totals = teamless.totals.clone();
        report.groups = vec![teamless];

        let mut output = anstream::StripStream::new(Vec::new());
        render_panel(&mut output, &report).expect("render panel");
        insta::assert_snapshot!(String::from_utf8(output.into_inner()).expect("utf8"), @r"
          @codex · Codex · gpt-5.5@high
              effort:    1h05m active · $1.25 incl. subagents
              calls:     2 asks · 7 tool calls · 1 compaction
              messages:  2 from you · 5 from teammates · 4 to teammates
              tokens:    1.2k input, 800 output, 2k cache write, 3k cache read
              subagents: 3 × explorer ($0.55) · 1 × other

        Total · 1 agent · 1h05m active · $1.25 · 7 messages (2 from you)
        ");
    }

    #[test]
    fn markdown_escapes_values_and_renders_grouped_bullets() {
        let mut output = Vec::new();
        render_markdown(&mut output, &report()).expect("render markdown");
        insta::assert_snapshot!(String::from_utf8(output).expect("utf8"), @r###"
        <details>
        <summary>Implemented by <a href="https://github.com/rimio-ai/rimz">RimZ</a> agents · 2 agents · 2h10m active · $2.50 · 14 messages (4 from you)</summary>

        **forge team**

        - **plan|ner** — Claude fable&#96;2@high
          - effort: 1h05m active · $1.25 incl. subagents
          - calls: 2 asks · 7 tool calls · 1 compaction
          - messages: 2 from you · 5 from teammates · 4 to teammates
          - tokens: 1.2k input, 800 output, 2k cache write, 3k cache read
          - subagents: 3 × explorer ($0.55) · 1 × other

        **Other agents**

        - **@codex** — Codex `gpt-5.5@high`
          - effort: 1h05m active · $1.25 incl. subagents
          - calls: 2 asks · 7 tool calls · 1 compaction
          - messages: 2 from you · 5 from teammates · 4 to teammates
          - tokens: 1.2k input, 800 output, 2k cache write, 3k cache read
          - subagents: 3 × explorer ($0.55) · 1 × other

        <sub>Generated by [`rimz agents attribution`](https://github.com/rimio-ai/rimz/blob/main/docs/reference/cli/agents.md#attribution)</sub>

        </details>
        "###);
    }

    #[test]
    fn markdown_single_team_keeps_the_group_name_in_the_summary_only() {
        let mut report = report();
        report.groups.pop();
        report.totals = report.groups[0].totals.clone();
        let mut output = Vec::new();

        render_markdown(&mut output, &report).expect("render markdown");
        let output = String::from_utf8(output).expect("utf8");

        assert!(output.contains("<code>forge</code> team"));
        assert!(!output.contains("**forge team**"));
    }

    #[test]
    fn markdown_escapes_emphasis_and_link_punctuation() {
        let mut report = report();
        report.groups[0].members[0].role = Some(r"plan*ner_[x]\tail".to_owned());
        let mut output = Vec::new();

        render_markdown(&mut output, &report).expect("render markdown");
        let output = String::from_utf8(output).expect("utf8");

        assert!(output.contains(r"- **plan\*ner\_\[x\]\\tail**"));
    }

    #[test]
    fn markdown_model_code_span_keeps_punctuation_verbatim() {
        let mut spanned = member("@coder", Some("coder"), "Qwen", "qwen2_5-coder");
        assert_eq!(markdown_model_label(&spanned), "`qwen2_5-coder@high`");

        spanned.model = Some("llama3*8b".to_owned());
        assert_eq!(markdown_model_label(&spanned), "`llama3*8b@high`");

        spanned.model = Some("a[1]".to_owned());
        assert_eq!(markdown_model_label(&spanned), "`a[1]@high`");

        spanned.model = Some("fable`2".to_owned());
        assert_eq!(markdown_model_label(&spanned), "fable&#96;2@high");
    }

    #[test]
    fn identity_omits_a_role_already_carried_by_the_handle() {
        let matching = member("@planner#auth", Some("planner"), "Claude", "fable-2");
        let displaced = member("@quiet-fox", Some("planner"), "Claude", "fable-2");

        assert_eq!(identity_label(&matching), "@planner#auth");
        assert_eq!(identity_label(&displaced), "@quiet-fox (planner)");
    }

    #[test]
    fn call_labels_name_only_recorded_components() {
        let mut sample = member("@coder", Some("coder"), "Codex", "gpt-5.5");
        sample.asks = 0;
        sample.tool_calls = 0;
        sample.compactions = 0;
        assert_eq!(calls_label(&sample), None);

        sample.asks = 1;
        assert_eq!(calls_label(&sample).as_deref(), Some("1 ask"));

        sample.asks = 0;
        sample.tool_calls = 1;
        assert_eq!(calls_label(&sample).as_deref(), Some("1 tool call"));

        sample.tool_calls = 0;
        sample.compactions = 1;
        assert_eq!(calls_label(&sample).as_deref(), Some("1 compaction"));

        sample.tool_calls = 2;
        sample.compactions = 3;
        sample.asks = 4;
        assert_eq!(
            calls_label(&sample).as_deref(),
            Some("4 asks · 2 tool calls · 3 compactions")
        );
    }

    #[test]
    fn renderers_omit_calls_when_none_are_recorded() {
        let mut report = report();
        for group in &mut report.groups {
            for member in &mut group.members {
                member.asks = 0;
                member.tool_calls = 0;
                member.compactions = 0;
            }
        }

        let mut panel = anstream::StripStream::new(Vec::new());
        render_panel(&mut panel, &report).expect("render panel");
        assert!(
            !String::from_utf8(panel.into_inner())
                .expect("utf8")
                .contains("calls:")
        );

        let mut markdown = Vec::new();
        render_markdown(&mut markdown, &report).expect("render markdown");
        assert!(
            !String::from_utf8(markdown)
                .expect("utf8")
                .contains("  - calls:")
        );
    }

    #[test]
    fn token_labels_name_only_recorded_components() {
        let mut sample = member("@coder", Some("coder"), "Codex", "gpt-5.5");
        sample.tokens.cache_write = 0;
        assert_eq!(
            token_label(&sample).as_deref(),
            Some("1.2k input, 800 output, 3k cache read")
        );

        sample.tokens = TokenSplit::default();
        assert_eq!(token_label(&sample), None);
    }

    #[test]
    fn absent_details_are_omitted_without_placeholders() {
        let mut report = report();
        report.groups.truncate(1);
        report.groups[0].members[0].active_secs = None;
        report.groups[0].members[0].cost_usd = None;
        report.groups[0].members[0].asks = 0;
        report.groups[0].members[0].tool_calls = 0;
        report.groups[0].members[0].compactions = 0;
        report.groups[0].members[0].messages = MessageCounts::default();
        report.groups[0].members[0].tokens = TokenSplit::default();
        report.groups[0].members[0].subagents.clear();
        report.groups[0].totals.active_secs = None;
        report.groups[0].totals.cost_usd = None;
        report.groups[0].totals.messages = MessageCounts::default();
        report.totals = report.groups[0].totals.clone();

        let mut panel = anstream::StripStream::new(Vec::new());
        render_panel(&mut panel, &report).expect("render panel");
        let panel = String::from_utf8(panel.into_inner()).expect("utf8");
        assert!(!panel.contains("unknown"));
        assert!(!panel.contains("none recorded"));
        assert!(!panel.contains("effort:"));
        assert!(!panel.contains("calls:"));
        assert!(!panel.contains("messages:"));
        assert!(!panel.contains("tokens:"));
        assert!(!panel.contains("subagents:"));

        let mut markdown = Vec::new();
        render_markdown(&mut markdown, &report).expect("render markdown");
        let markdown = String::from_utf8(markdown).expect("utf8");
        assert!(!markdown.contains("unknown"));
        assert!(!markdown.contains("none recorded"));
    }

    #[test]
    fn token_counts_change_units_at_decimal_boundaries() {
        assert_eq!(token_count(999), "999");
        assert_eq!(token_count(1_000), "1k");
        assert_eq!(token_count(1_100), "1.1k");
        assert_eq!(token_count(999_949), "999.9k");
        assert_eq!(token_count(999_950), "1m");
        assert_eq!(token_count(1_000_000), "1m");
        assert_eq!(token_count(999_949_999), "999.9m");
        assert_eq!(token_count(999_950_000), "1b");
    }

    #[test]
    fn empty_scope_is_muted_for_people_and_silent_for_markdown() {
        let mut report = report();
        report.groups.clear();
        report.totals = EffortTotals::default();
        let mut panel = anstream::StripStream::new(Vec::new());
        render_panel(&mut panel, &report).expect("render panel");
        assert_eq!(
            String::from_utf8(panel.into_inner()).expect("utf8"),
            "No agent attribution records in this scope.\n"
        );
        let mut markdown = Vec::new();
        render_markdown(&mut markdown, &report).expect("render markdown");
        assert!(markdown.is_empty());
    }
}
