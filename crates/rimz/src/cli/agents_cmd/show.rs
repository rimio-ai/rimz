use super::*;

use super::list::agent_pr;
use super::report::{
    AgentReportEntry, PrInfo, ReportOverrides, SelfIdentity, build_entry, context_cell,
    row_for_agent, status_style,
};
use super::runs_lookup::{agent_name, newest_run_by_ref, newest_run_for_agent, print_run_line};
use crate::cli::render;

#[derive(serde::Serialize)]
struct ShowReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<AgentReportEntry>,
    #[serde(skip)]
    agent_state: Option<AgentState>,
    #[serde(skip_serializing_if = "is_false")]
    stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<RunRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ask: Option<crate::cli::transcript::AskView>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    messages: Vec<ShowMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capture: Option<rimz::mux::PaneCapture>,
    #[serde(skip)]
    recent_transcript: Option<crate::cli::transcript::RenderedChat>,
}

fn collect_show_report(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    runtime: &rimz::RuntimePaths,
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    reference: &str,
    capture: bool,
    ansi: bool,
) -> Result<(ShowReport, Option<anyhow::Error>)> {
    let agent_result = crate::cli::resolve_agent_one(
        snapshot,
        reference,
        None,
        crate::cli::current_channel(workspace).as_deref(),
    );
    let (mut agent, mut deferred_error) = match agent_result {
        Ok(agent) => (Some(agent.clone()), None),
        Err(err) => (None, Some(err)),
    };
    let mut stale = false;
    if agent.is_none() {
        match resolve_audit_agent(store, workspace, runtime, reference) {
            Ok(Some(audit_agent)) => {
                agent = Some(audit_agent);
                stale = true;
            }
            Ok(None) => {}
            Err(err) => deferred_error = Some(err),
        }
    }
    let run = newest_run_by_ref(store, reference, agent.as_ref())?;
    let ask = match agent.as_ref() {
        Some(agent) => crate::cli::transcript::latest_ask_view(workspace, agent)?,
        None => None,
    };
    let pane_capture = if capture {
        let agent = agent.as_ref().ok_or_else(|| {
            anyhow::anyhow!("no live agent matches `{reference}`; nothing to capture")
        })?;
        let pane = agent
            .pane
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
        let backend = rimz::mux::backend_for(pane.pane_id.mux());
        Some(
            backend
                // rimz-invariant: explicit-agent-show-capture
                .capture_pane(&pane.pane_id, None, ansi)
                .context("capturing agent pane")?,
        )
    } else {
        None
    };
    let effort = agent
        .as_ref()
        .map(|agent| slot_lifetime_effort(store, runtime, agent))
        .transpose()?;
    let budget_cost_usd = agent
        .as_ref()
        .and_then(|agent| session_cost(runtime, agent))
        .and_then(|cost| cost.total_cost_usd);
    let messages = match agent.as_ref() {
        Some(agent) => show_messages(store, agent)?,
        None => Vec::new(),
    };
    let recent_transcript = match agent.as_ref() {
        Some(agent) => recent_agent_transcript(workspace, agent).ok(),
        None => None,
    }
    .filter(|view| !view.entries.is_empty());
    let peers = rimz::harness::target::addressable_agents(snapshot);
    let me = SelfIdentity::from_env().resolve(snapshot);
    let report_agent = agent.as_ref().map(|agent| {
        build_entry(
            agent,
            row_for_agent(snapshot, agent),
            agent_pr(snapshot, agent),
            &peers,
            me.as_ref(),
            jiff::Timestamp::now(),
            ReportOverrides {
                runtime: Some(runtime),
                effort: effort.map(|(effort, _)| effort),
                active_secs: effort.and_then(|(_, active_secs)| active_secs),
                budget_cost_usd,
            },
        )
    });
    Ok((
        ShowReport {
            agent: report_agent,
            agent_state: agent,
            stale,
            run,
            ask,
            messages,
            capture: pane_capture,
            recent_transcript,
        },
        deferred_error,
    ))
}

fn render_show_report(
    report: ShowReport,
    store: &rimz::Store,
    deferred_error: Option<anyhow::Error>,
) -> Result<()> {
    let Some(agent) = report.agent.as_ref() else {
        if let Some(run) = report.run.as_ref() {
            print_run_line(run)?;
            return Ok(());
        }
        return Err(deferred_error.unwrap_or_else(|| anyhow::anyhow!("agent resolution failed")));
    };
    let Some(state) = report.agent_state.as_ref() else {
        return Err(anyhow::anyhow!("agent report lost its source state"));
    };
    let now = jiff::Timestamp::now();
    let mut out = render::out();
    render_agent_section(&mut out, agent)?;
    render_activity_section(&mut out, agent, report.ask.as_ref(), report.stale, now)?;
    render_context_section(&mut out, agent, now)?;
    render_placement_section(&mut out, agent)?;
    let fallback_run = if report.run.is_none() {
        newest_run_for_agent(store, state).ok().flatten()
    } else {
        None
    };
    if let Some(run) = report.run.as_ref().or(fallback_run.as_ref()) {
        render_run_section(&mut out, run, now)?;
    }
    if !report.messages.is_empty() {
        render_messages_section(&mut out, &report.messages)?;
    }
    if let Some(view) = report.recent_transcript.as_ref() {
        section(&mut out, "Recent transcript")?;
        let tz = crate::cli::machine_config().time_zone();
        crate::cli::transcript::render_lines_to(
            &mut out,
            view,
            &tz,
            render::prose::Prose::for_stdout(),
        )?;
    }
    if let Some(capture) = report.capture.as_ref() {
        if report.recent_transcript.is_some() {
            writeln!(out)?;
        }
        render_capture_section(&mut out, capture)?;
    }
    Ok(())
}

pub(super) fn show_agent(
    reference: String,
    json: bool,
    capture: bool,
    ansi: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let runtime = ctx.runtime();
    let snapshot = rimz::sidebar::consumer::PublishedSnapshotReader::new(
        runtime.clone(),
        ctx.workspace.session_name.clone(),
        None,
    )
    .read(ctx.store.paths())
    .context("reading the room snapshot")?;
    let (report, deferred_error) = collect_show_report(
        &ctx.store,
        &ctx.workspace,
        runtime,
        &snapshot,
        &reference,
        capture,
        ansi,
    )?;
    if json {
        return render::json_pretty(&report);
    }
    render_show_report(report, &ctx.store, deferred_error)
}

pub(super) fn resolve_audit_agent(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    runtime: &rimz::RuntimePaths,
    reference: &str,
) -> Result<Option<AgentState>> {
    let audit = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?;
    if audit.agents.is_empty() {
        return Ok(None);
    }
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        workspace.workspace_id.clone(),
        audit.agents,
        jiff::Timestamp::now(),
    )
    .with_agent_context(rimz::store::agent_context::read_all(runtime));
    match crate::cli::resolve_agent_one(&snapshot, reference, None, None) {
        Ok(agent) => Ok(Some(agent.clone())),
        Err(err) => Err(err),
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, serde::Serialize)]
struct ShowMessage {
    id: String,
    status: rimz::message::MessageStatus,
    from: String,
    age: String,
    text: String,
}

fn section(w: &mut impl Write, title: &str) -> std::io::Result<()> {
    writeln!(w, "{}", render::paint(render::palette::header(), title))
}

fn render_capture_section(
    w: &mut impl Write,
    capture: &rimz::mux::PaneCapture,
) -> std::io::Result<()> {
    section(w, "Capture")?;
    render::pane_frame(w, &capture.pane_id.to_string(), &capture.raw_text)
}

fn render_agent_section(w: &mut impl Write, agent: &AgentReportEntry) -> std::io::Result<()> {
    section(w, "Agent")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push(
        "handle",
        render::cell(agent.handle.as_str()).fg(render::palette::identity(agent.kind.as_str())),
    );
    kv.push(
        "kind",
        render::cell(agent.kind.to_string()).fg(render::palette::meta()),
    );
    if let Some(profile) = agent.profile.as_deref() {
        kv.push("profile", render::cell(profile).fg(render::palette::meta()));
    }
    if let Some(role) = agent.role.as_deref() {
        kv.push("role", render::cell(role).fg(render::palette::meta()));
    }
    if let Some(team) = agent.team.as_deref() {
        kv.push("team", render::cell(team).fg(render::palette::meta()));
    }
    if let Some(name) = agent.name.as_deref() {
        kv.push("name", render::cell(name));
    }
    kv.push("session", render::cell(agent.id.to_string()));
    if let Some(registered_at) = agent.timeline.registered_at {
        kv.push("registered_at", render::cell(registered_at.to_string()));
    }
    kv.render(w)?;
    writeln!(w)
}

pub(super) fn render_activity_section(
    w: &mut impl Write,
    agent: &AgentReportEntry,
    ask: Option<&crate::cli::transcript::AskView>,
    stale: bool,
    now: jiff::Timestamp,
) -> std::io::Result<()> {
    section(w, "Activity")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push(
        "description",
        render::cell(agent.description.as_deref().unwrap_or("-")).dash(),
    );
    kv.push(
        "status",
        render::cell(agent.status.as_str()).fg(status_style(agent)),
    );
    if agent.phase != rimz::agents::TurnPhase::Idle {
        kv.push("phase", render::cell(agent.phase.as_str()));
    }
    if let Some(started) = agent.timeline.turn_started_at {
        kv.push("turn_started", render::cell(started.to_string()));
        kv.push("turn_elapsed", render::cell(render::rel_age(started, now)));
    }
    kv.push(
        "last_activity",
        render::cell(render::rel_age(agent.timeline.last_activity, now)),
    );
    if let Some(error) = agent.turn_error.as_ref() {
        kv.push(
            "turn_error",
            render::cell(error.label.as_deref().unwrap_or("provider API error"))
                .fg(render::palette::alarm()),
        );
    }
    if let Some(ask) = ask {
        kv.push(
            "ask",
            render::cell(crate::cli::transcript::ask_summary(ask)).fg(render::palette::warn()),
        );
    }
    if stale {
        kv.push("stale", render::cell("yes").fg(render::palette::faint()));
    }
    kv.render(w)?;
    writeln!(w)
}

fn render_context_section(
    w: &mut impl Write,
    agent: &AgentReportEntry,
    now: jiff::Timestamp,
) -> std::io::Result<()> {
    section(w, "Context")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push(
        "model",
        render::cell(agent.model.label.as_deref().unwrap_or("-"))
            .dash()
            .fg(render::palette::muted()),
    );
    kv.push("fill", context_cell(agent.context.fill_pct));
    kv.push(
        "window",
        render::cell(
            agent
                .context
                .window
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash(),
    );
    kv.push(
        "total_tokens",
        render::cell(opt_count(agent.stats.total_tokens)).dash(),
    );
    kv.push(
        "fresh_input_tokens",
        render::cell(opt_count(agent.stats.fresh_input_tokens)).dash(),
    );
    kv.push(
        "cache_read_tokens",
        render::cell(opt_count(agent.stats.cache_read_tokens)).dash(),
    );
    kv.push(
        "cache_write_tokens",
        render::cell(opt_count(agent.stats.cache_write_tokens)).dash(),
    );
    kv.push(
        "output_tokens",
        render::cell(opt_count(agent.stats.output_tokens)).dash(),
    );
    kv.push(
        "compactions",
        render::cell(agent.context.compactions.to_string()),
    );
    if !agent.stats.tool_calls.is_empty() {
        kv.push(
            "tools",
            render::cell(format_tool_calls(&agent.stats.tool_calls)),
        );
    }
    if let Some(repeat) = agent.stats.tool_repeat.as_ref().filter(|repeat| {
        repeat.count
            >= crate::cli::machine_config()
                .agents
                .attention
                .tool_repeat_warn_after
                .get()
    }) {
        let age_secs = now.duration_since(repeat.since).as_secs().max(0) as u64;
        kv.push(
            "repeat",
            render::cell(format!(
                "{} ×{}, {}",
                repeat.tool,
                repeat.count,
                render::age_label(age_secs)
            )),
        );
    }
    if let Some(active_secs) = agent.stats.active_secs {
        kv.push("active", render::cell(render::age_label(active_secs)));
    }
    kv.push(
        "cost",
        render::cell(
            agent
                .stats
                .cost_usd
                .map(fmt_cost)
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash()
        .fg(render::palette::money()),
    );
    kv.push(
        "budget",
        render::cell(budget_summary(&agent.budget))
            .dash()
            .fg(render::palette::money()),
    );
    kv.render(w)?;
    writeln!(w)
}

fn budget_summary(budget: &super::report::BudgetReport) -> String {
    match (budget.spent_usd, budget.cap.as_deref()) {
        (Some(spent_usd), Some(cap)) => format!("${spent_usd:.2} of {cap}"),
        _ => budget
            .park
            .clone()
            .or_else(|| budget.cap.clone())
            .unwrap_or_else(|| "-".to_owned()),
    }
}

fn format_tool_calls(tool_calls: &std::collections::BTreeMap<String, u32>) -> String {
    let total = tool_calls
        .values()
        .copied()
        .map(u64::from)
        .fold(0_u64, u64::saturating_add);
    let mut ranked: Vec<_> = tool_calls.iter().collect();
    ranked.sort_unstable_by(|(name_a, count_a), (name_b, count_b)| {
        count_b.cmp(count_a).then_with(|| name_a.cmp(name_b))
    });
    let visible = ranked
        .iter()
        .take(3)
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    if ranked.len() > 3 {
        format!("{total} · {visible}, …")
    } else {
        format!("{total} · {visible}")
    }
}

pub(super) fn render_placement_section(
    w: &mut impl Write,
    agent: &AgentReportEntry,
) -> std::io::Result<()> {
    section(w, "Placement")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push(
        "channel",
        render::cell(agent.placement.channel.as_deref().unwrap_or("-")).dash(),
    );
    kv.push(
        "worktree",
        render::cell(agent.placement.worktree.as_deref().unwrap_or("-")).dash(),
    );
    kv.push(
        "pr",
        render::cell(
            agent
                .placement
                .pr
                .map(format_pr_info)
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash(),
    );
    kv.push(
        "pane",
        render::cell(agent.placement.pane.as_deref().unwrap_or("-")).dash(),
    );
    kv.render(w)?;
    writeln!(w)
}

pub(super) fn format_pr_info(pr: PrInfo) -> String {
    let state = match pr.state {
        rimz::store::snapshot::WorktreePrState::Open => "open",
        rimz::store::snapshot::WorktreePrState::Closed => "closed",
        rimz::store::snapshot::WorktreePrState::Merged => "merged",
    };
    let number = pr
        .number
        .map(|number| format!("#{number} "))
        .unwrap_or_default();
    let ci = pr.ci.map(|ci| match ci {
        rimz::store::snapshot::WorktreePrCi::Pending => " · ci pending",
        rimz::store::snapshot::WorktreePrCi::Passing => " · ci passing",
        rimz::store::snapshot::WorktreePrCi::Failing => " · ci failing",
    });
    format!("{number}{state}{}", ci.unwrap_or_default())
}

fn render_run_section(
    w: &mut impl Write,
    run: &RunRecord,
    now: jiff::Timestamp,
) -> std::io::Result<()> {
    section(w, "Run")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push("id", render::cell(run.run_id.to_string()));
    kv.push(
        "status",
        render::cell(supervised::output::status_label(run.status))
            .fg(render::status::run(run.status)),
    );
    kv.push("prompt", render::cell(preview(&run.prompt)));
    kv.push(
        "started",
        render::cell(render::rel_age(run.started_at, now)),
    );
    let end = run.completed_at.unwrap_or(run.updated_at);
    kv.push("updated", render::cell(render::rel_age(end, now)));
    kv.push(
        "duration",
        render::cell(duration_label(run.started_at, end)),
    );
    kv.push(
        "exit_code",
        render::cell(run.status.exit_code().to_string()),
    );
    if let Some(tail) = run.failure_tail.as_deref() {
        kv.push(
            "failure",
            render::cell(preview(tail)).fg(render::palette::alarm()),
        );
    }
    kv.render(w)?;
    writeln!(w)
}

fn render_messages_section(w: &mut impl Write, messages: &[ShowMessage]) -> std::io::Result<()> {
    section(w, "Messages")?;
    let mut table = render::Table::new(["ID", "STATUS", "FROM", "AGE", "TEXT"]);
    for message in messages {
        table.row([
            render::cell(message.id.as_str()),
            render::cell(message.status.as_str()).fg(render::status::message(message.status)),
            render::cell(message.from.as_str()).fg(render::palette::meta()),
            render::cell(message.age.as_str()),
            render::cell(message.text.as_str()).dash(),
        ]);
    }
    table.render(w)?;
    writeln!(w)
}

fn opt_count(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_owned())
}

fn fmt_cost(value: f64) -> String {
    if value >= 1.0 {
        rimz::theme::fmt::dollars2(value)
    } else {
        format!("${value:.4}")
    }
}

fn duration_label(start: jiff::Timestamp, end: jiff::Timestamp) -> String {
    render::age_label(end.duration_since(start).as_secs().max(0) as u64)
}

fn preview(text: &str) -> String {
    const MAX: usize = 80;
    let preview = text.replace(['\r', '\n', '\t'], " ");
    let mut chars = preview.chars();
    let short: String = chars.by_ref().take(MAX).collect();
    if chars.next().is_some() {
        let mut shortened = preview.chars().take(MAX - 3).collect::<String>();
        shortened.push_str("...");
        shortened
    } else {
        short
    }
}

fn slot_lifetime_effort(
    store: &rimz::Store,
    runtime: &rimz::RuntimePaths,
    agent: &AgentState,
) -> Result<(rimz::agents::spending::SlotEffort, Option<u64>)> {
    let audit = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?;
    let refs = audit.agents.iter().collect::<Vec<_>>();
    let records = rimz::agents::attribution::slot_groups(&refs)
        .into_iter()
        .find(|records| {
            records
                .iter()
                .any(|record| record.agent_id == agent.agent_id)
        })
        .unwrap_or_else(|| vec![agent]);
    let prices = rimz::agents::pricing::cached_book(&runtime.shared_pricing_cache_path());
    let effort = rimz::agents::spending::slot_effort(
        &records
            .iter()
            .map(|record| rimz::agents::spending::EffortSessionRef::from_state(record))
            .collect::<Vec<_>>(),
        &prices,
    );
    let now = jiff::Timestamp::now();
    let active_grace_secs = crate::cli::machine_config()
        .agents
        .attention
        .active_grace_secs
        .get();
    let active_secs = rimz::store::active_time::read_for_keys(
        runtime,
        records
            .iter()
            .map(|record| (record.kind.as_str(), record.agent_id.as_str())),
    )
    .into_iter()
    .map(|record| record.display_secs(now, active_grace_secs))
    .reduce(u64::saturating_add);
    Ok((effort, active_secs))
}

fn session_cost(
    runtime: &rimz::RuntimePaths,
    agent: &AgentState,
) -> Option<rimz::agents::AgentCost> {
    let adapter = rimz::agents::find_definition(agent.kind.as_str())?;
    let transcript = Path::new(agent.transcript_path.as_deref()?);
    let prices = rimz::agents::pricing::cached_book(&runtime.shared_pricing_cache_path());
    rimz::agents::spending::session_cost_usd(adapter, agent.agent_id.as_str(), transcript, &prices)
}

fn show_messages(store: &rimz::Store, agent: &AgentState) -> Result<Vec<ShowMessage>> {
    let now = jiff::Timestamp::now();
    let mut rows: Vec<ShowMessage> = store
        .list_messages()?
        .into_iter()
        .filter(|message| message.same_agent_card(agent))
        .map(|message| ShowMessage {
            id: message.message_id.to_string(),
            status: message.status,
            from: message.sender.render(),
            age: render::rel_age(message.enqueued_at, now),
            text: preview(&message.text),
        })
        .collect();
    let mut delivered = Vec::new();
    for event in store.read_events()?.into_iter().rev() {
        let rimz::store::event::EventKind::Message { payload, .. } = event.kind() else {
            continue;
        };
        if payload.status != rimz::message::MessageStatus::Delivered
            || !rimz::agents::AgentCardRef::new(
                &payload.kind,
                &payload.agent_id,
                payload.agent_name.as_deref(),
            )
            .matches(agent.card_ref())
        {
            continue;
        }
        delivered.push(ShowMessage {
            id: payload.message_id.to_string(),
            status: payload.status,
            from: payload.sender.unwrap_or_default().render(),
            age: render::rel_age(payload.enqueued_at.unwrap_or(event.timestamp), now),
            text: "-".to_owned(),
        });
        if delivered.len() >= 3 {
            break;
        }
    }
    delivered.reverse();
    rows.extend(delivered);
    Ok(rows)
}

fn recent_agent_transcript(
    workspace: &rimz::ResolvedWorkspace,
    agent: &AgentState,
) -> Result<crate::cli::transcript::RenderedChat> {
    crate::cli::transcript::chat_view(
        workspace,
        Some(&format!("@{}", agent.agent_id.as_str())),
        None,
        Some(6),
        false,
    )
}

pub(super) fn focus_agent(reference: String, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let snapshot = ctx.cached_snapshot()?;
    let agent = crate::cli::resolve_agent_one(&snapshot, &reference, None, ctx.channel())?;
    focus_resolved(&ctx, agent)
}

pub(in crate::cli) fn focus_resolved(ctx: &Ctx, agent: &AgentState) -> Result<()> {
    let pane = agent
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    rimz::sidebar::focus_anchor::execute_action(
        backend.as_ref(),
        ctx.runtime(),
        &ctx.workspace.session_name,
        pane.pane_id.clone(),
        rimz::sidebar::focus_anchor::FocusOrigin::User,
        None,
        Default::default(),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(
        render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> std::io::Result<()>,
    ) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_one(&mut stream).expect("render to in-memory buffer");
        String::from_utf8(stream.into_inner()).expect("utf-8")
    }

    #[test]
    fn capture_section_has_a_heading_and_pane_frame() {
        let capture = rimz::mux::PaneCapture {
            pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_3"),
            raw_text: "working".to_owned(),
            lines: vec!["working".to_owned()],
        };

        let rendered = strip(|w| render_capture_section(w, &capture));

        assert!(
            rendered.starts_with("Capture\n╭─ zellij:terminal_3 "),
            "{rendered}"
        );
        assert!(rendered.contains("\n│ working"), "{rendered}");
        assert!(rendered.ends_with("╯\n"), "{rendered}");
    }

    #[test]
    fn show_message_status_serializes_as_its_existing_string() {
        let message = ShowMessage {
            id: "msg_1".to_owned(),
            status: rimz::message::MessageStatus::TimedOut,
            from: "@planner".to_owned(),
            age: "2m ago".to_owned(),
            text: "check".to_owned(),
        };

        assert_eq!(
            serde_json::to_string(&message).expect("serialize show message"),
            r#"{"id":"msg_1","status":"timed_out","from":"@planner","age":"2m ago","text":"check"}"#
        );
    }

    #[test]
    fn show_json_wraps_the_projected_agent() {
        let state = rimz::testkit::agent_state("codex", "show", jiff::Timestamp::UNIX_EPOCH);
        let peers = [&state];
        let report = ShowReport {
            agent: Some(build_entry(
                &state,
                None,
                None,
                &peers,
                None,
                jiff::Timestamp::UNIX_EPOCH,
                ReportOverrides {
                    effort: Some(rimz::agents::spending::SlotEffort {
                        cost_usd: Some(0.42),
                        ..rimz::agents::spending::SlotEffort::default()
                    }),
                    ..ReportOverrides::default()
                },
            )),
            agent_state: Some(state),
            stale: true,
            run: None,
            ask: None,
            messages: Vec::new(),
            capture: None,
            recent_transcript: None,
        };

        insta::assert_json_snapshot!("show_agent_report", report);
    }

    #[test]
    fn tool_calls_render_total_then_top_three() {
        let calls = std::collections::BTreeMap::from([
            ("Edit".to_owned(), 9),
            ("Read".to_owned(), 7),
            ("Bash".to_owned(), 18),
            ("Write".to_owned(), 2),
        ]);

        assert_eq!(format_tool_calls(&calls), "36 · Bash 18, Edit 9, Read 7, …");
    }
}
