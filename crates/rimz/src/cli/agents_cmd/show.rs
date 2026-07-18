use super::*;

use super::list::{
    PrInfo, agent_pr, agent_status_label, agent_status_projection, agent_status_style,
    context_cell, model_label, worktree_label,
};
use super::runs_lookup::{agent_name, newest_run_by_ref, newest_run_for_agent, print_run_line};
use crate::cli::render;

#[derive(serde::Serialize)]
struct ShowReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<AgentState>,
    #[serde(skip_serializing_if = "is_false")]
    stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<RunRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ask: Option<crate::cli::transcript::AskView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<rimz::agents::AgentCost>,
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
    snapshot: &rimz::SidebarSnapshot,
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
    let cost = agent
        .as_ref()
        .and_then(|agent| session_cost(runtime, agent));
    let messages = match agent.as_ref() {
        Some(agent) => show_messages(store, agent)?,
        None => Vec::new(),
    };
    let recent_transcript = match agent.as_ref() {
        Some(agent) => recent_agent_transcript(workspace, agent).ok(),
        None => None,
    }
    .filter(|view| !view.entries.is_empty());
    Ok((
        ShowReport {
            agent,
            stale,
            run,
            ask,
            cost,
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
    snapshot: &rimz::SidebarSnapshot,
    runtime: &rimz::RuntimePaths,
    deferred_error: Option<anyhow::Error>,
) -> Result<()> {
    let Some(agent) = report.agent.as_ref() else {
        if let Some(run) = report.run.as_ref() {
            print_run_line(run)?;
            return Ok(());
        }
        return Err(deferred_error.unwrap_or_else(|| anyhow::anyhow!("agent resolution failed")));
    };
    let peers: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|candidate| candidate.parent_agent_id.is_none())
        .collect();
    let now = jiff::Timestamp::now();
    let mut out = render::out();
    render_agent_section(&mut out, agent, &peers)?;
    render_activity_section(&mut out, agent, report.ask.as_ref(), report.stale, now)?;
    render_context_section(&mut out, agent, report.cost.as_ref(), runtime)?;
    render_placement_section(&mut out, agent, agent_pr(snapshot, agent))?;
    let fallback_run = if report.run.is_none() {
        newest_run_for_agent(store, agent).ok().flatten()
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
        crate::cli::transcript::render_lines_to(&mut out, view, &tz)?;
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
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let snapshot = crate::cli::alive_snapshot(&store, &runtime, &workspace.session_name)?
        .with_agent_context(rimz::store::agent_context::read_all(&runtime));
    let (report, deferred_error) = collect_show_report(
        &store, &workspace, &runtime, &snapshot, &reference, capture, ansi,
    )?;
    if json {
        return render::json_pretty(&report);
    }
    render_show_report(report, &store, &snapshot, &runtime, deferred_error)
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
    let snapshot = rimz::SidebarSnapshot::build_with_agents(
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

fn render_agent_section(
    w: &mut impl Write,
    agent: &AgentState,
    peers: &[&AgentState],
) -> std::io::Result<()> {
    section(w, "Agent")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push(
        "handle",
        render::cell(rimz::harness::target::agent_handle(agent, peers, true))
            .fg(render::palette::identity(agent.kind.as_str())),
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
    kv.push("session", render::cell(agent.agent_id.to_string()));
    if let Some(registered_at) = agent.registered_at {
        kv.push("registered_at", render::cell(registered_at.to_string()));
    }
    kv.render(w)?;
    writeln!(w)
}

pub(super) fn render_activity_section(
    w: &mut impl Write,
    agent: &AgentState,
    ask: Option<&crate::cli::transcript::AskView>,
    stale: bool,
    now: jiff::Timestamp,
) -> std::io::Result<()> {
    section(w, "Activity")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push(
        "description",
        render::cell(agent.activity_line().unwrap_or_else(|| "-".to_owned())).dash(),
    );
    kv.push(
        "status",
        render::cell(agent_status_label(agent)).fg(agent_status_style(agent)),
    );
    let (_, phase) = agent_status_projection(agent);
    if phase != rimz::agents::TurnPhase::Idle {
        kv.push("phase", render::cell(phase.as_str()));
    }
    if let Some(started) = agent.turn_started_at {
        kv.push("turn_started", render::cell(started.to_string()));
        kv.push("turn_elapsed", render::cell(render::rel_age(started, now)));
    }
    kv.push(
        "last_activity",
        render::cell(render::rel_age(agent.last_activity, now)),
    );
    if let Some((_, label)) = agent.displayed_turn_error() {
        kv.push(
            "turn_error",
            render::cell(label.unwrap_or("provider API error")).fg(render::palette::alarm()),
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
    agent: &AgentState,
    cost: Option<&rimz::agents::AgentCost>,
    runtime: &rimz::RuntimePaths,
) -> std::io::Result<()> {
    section(w, "Context")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push(
        "model",
        render::cell(model_label(agent))
            .dash()
            .fg(render::palette::muted()),
    );
    kv.push("fill", context_cell(agent));
    kv.push(
        "window",
        render::cell(
            agent
                .resolved_context_window()
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash(),
    );
    kv.push(
        "total_tokens",
        render::cell(opt_count(agent.total_tokens)).dash(),
    );
    kv.push(
        "fresh_input_tokens",
        render::cell(opt_count(agent.fresh_input_tokens)).dash(),
    );
    kv.push(
        "cache_read_tokens",
        render::cell(opt_count(agent.cache_read_input_tokens)).dash(),
    );
    kv.push(
        "cache_write_tokens",
        render::cell(opt_count(agent.cache_write_input_tokens)).dash(),
    );
    kv.push(
        "output_tokens",
        render::cell(opt_count(agent.output_tokens)).dash(),
    );
    kv.push(
        "compactions",
        render::cell(agent.compaction_count.to_string()),
    );
    kv.push(
        "cost",
        render::cell(
            cost.and_then(|cost| cost.total_cost_usd)
                .map(fmt_cost)
                .unwrap_or_else(|| "-".to_owned()),
        )
        .dash()
        .fg(render::palette::money()),
    );
    let budget = rimz::harness::budget::spend_summary(
        runtime,
        agent,
        cost.and_then(|cost| cost.total_cost_usd),
    );
    kv.push(
        "budget",
        render::cell(budget.unwrap_or_else(|| "-".to_owned()))
            .dash()
            .fg(render::palette::money()),
    );
    kv.render(w)?;
    writeln!(w)
}

pub(super) fn render_placement_section(
    w: &mut impl Write,
    agent: &AgentState,
    pr: Option<PrInfo>,
) -> std::io::Result<()> {
    section(w, "Placement")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push("channel", render::cell(worktree_label(agent)).dash());
    kv.push(
        "worktree",
        render::cell(agent.worktree_path.as_deref().unwrap_or("-")).dash(),
    );
    kv.push(
        "pr",
        render::cell(pr.map(format_pr_info).unwrap_or_else(|| "-".to_owned())).dash(),
    );
    push_pane_anchor(&mut kv, agent);
    kv.render(w)?;
    writeln!(w)
}

pub(super) fn format_pr_info(pr: PrInfo) -> String {
    let state = match pr.state {
        rimz::WorktreePrState::Open => "open",
        rimz::WorktreePrState::Closed => "closed",
        rimz::WorktreePrState::Merged => "merged",
    };
    let number = pr
        .number
        .map(|number| format!("#{number} "))
        .unwrap_or_default();
    let ci = pr.ci.map(|ci| match ci {
        rimz::WorktreePrCi::Pending => " · ci pending",
        rimz::WorktreePrCi::Passing => " · ci passing",
        rimz::WorktreePrCi::Failing => " · ci failing",
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

fn session_cost(
    runtime: &rimz::RuntimePaths,
    agent: &AgentState,
) -> Option<rimz::agents::AgentCost> {
    let adapter = rimz::agents::find_adapter(agent.kind.as_str())?;
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
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let agent = crate::cli::resolve_agent_one(
        &snapshot,
        &reference,
        None,
        crate::cli::current_channel(&workspace).as_deref(),
    )?;
    let pane = agent
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())?;
    rimz::sidebar::focus_anchor::execute_action(
        backend.as_ref(),
        &runtime,
        &workspace.session_name,
        pane.pane_id.clone(),
        rimz::sidebar::focus_anchor::FocusOrigin::User,
        None,
    )?;
    Ok(())
}

fn push_pane_anchor(kv: &mut render::KeyVals, agent: &AgentState) {
    let Some(pane) = agent.pane.as_ref() else {
        kv.push("pane", render::cell("-").dash());
        return;
    };
    kv.push("pane", render::cell(pane.pane_id.to_string()));
    if let Some(view_id) = pane.view_id.as_deref() {
        kv.push("tab", render::cell(view_id));
    }
    if let Some(cwd) = pane.cwd.as_deref() {
        kv.push("pane_cwd", render::cell(cwd));
    }
    if let Some(pid) = pane.pane_pid {
        kv.push("pane_pid", render::cell(pid.to_string()));
    }
    if let Some(start) = pane.pane_process_start {
        kv.push("pane_process_start", render::cell(start.to_string()));
    }
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
}
