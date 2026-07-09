use super::*;

use crate::cli::render;
use rimz::config::{GlyphRole, ThemeConfig};

pub(super) fn list_agents(
    json: bool,
    all: bool,
    scope: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let mux = rimz::mux::auto_detect_backend(globals.mux).map_err(|_| {
        anyhow::anyhow!(crate::cli::agents_launch::live_session_guidance(
            &workspace.session_name
        ))
    })?;
    let backend = rimz::mux::backend_for(mux);
    crate::cli::agents_launch::ensure_live_session(&*backend, &workspace.session_name)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let state = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    state.ensure_dirs().context("preparing state directories")?;
    let snapshot = rimz::sidebar::consumer::read_published_snapshot(
        &mut rimz::sidebar::consumer::RollupCursor::new(),
        &state,
        &runtime,
        &workspace.session_name,
        None,
    )
    .context("reading the room snapshot")?;

    let channel = list_channel_filter(all, scope.as_deref(), &workspace);
    let in_room = in_room_agent_ids(&snapshot);
    let agents: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|agent| agent.parent_agent_id.is_none())
        .filter(|agent| in_room.contains(&agent.agent_id))
        .filter(|agent| {
            channel
                .as_deref()
                .is_none_or(|filter| rimz::harness::target::agent_in_worktree(agent, filter))
        })
        .collect();
    if json {
        supervised::output::print_json(&agents)?;
        return Ok(());
    }

    let machine_config = crate::cli::machine_config();
    let mut out = render::out();
    render_agents_table(
        &mut out,
        &snapshot,
        &agents,
        jiff::Timestamp::now(),
        render::terminal_columns(120),
        &machine_config.theme,
    )?;
    Ok(())
}

fn in_room_agent_ids(
    snapshot: &rimz::SidebarSnapshot,
) -> std::collections::HashSet<&AgentSessionId> {
    snapshot
        .agent_panes
        .iter()
        .filter_map(|pane_agent| pane_agent.agent_id.as_ref())
        .collect()
}

pub(crate) fn render_agents_table(
    w: &mut impl std::io::Write,
    snapshot: &rimz::SidebarSnapshot,
    agents: &[&AgentState],
    now: jiff::Timestamp,
    max_width: usize,
    theme: &ThemeConfig,
) -> std::io::Result<()> {
    let groups = rimz::store::snapshot::group_live_agents_by_worktree(agents, snapshot);
    let ordered_agents: Vec<&AgentState> = groups
        .iter()
        .flat_map(|group| group.agents.iter().copied())
        .collect();
    let glyph = rimz::sidebar_pane::render::theme_glyphs(theme);
    let mut table =
        render::Table::new(["AGENT", "STATUS", "MODEL", "CTX", "TOKENS", "AGE", "DESC"])
            .right(&[3, 4, 5])
            .clip_last(max_width);
    for group in groups {
        table.section_cells(group_header_cells(&group, snapshot, &glyph));
        for &agent in &group.agents {
            table.row(agent_row(agent, &ordered_agents, now));
        }
    }
    table.render(w)
}

fn group_header_cells(
    group: &rimz::store::snapshot::AgentWorktreeGroup<'_>,
    snapshot: &rimz::SidebarSnapshot,
    glyph: &impl Fn(GlyphRole) -> String,
) -> Vec<render::Cell> {
    if group.kind == rimz::SidebarWorktreeKind::External {
        return vec![render::cell("external").fg(render::palette::FAINT)];
    }

    let label = match group.kind {
        rimz::SidebarWorktreeKind::Worktree => {
            format!("{} {}", glyph(GlyphRole::WorktreeBranch), group.label)
        }
        rimz::SidebarWorktreeKind::Channel if channel_group_is_worktree_backed(group, snapshot) => {
            format!("{} {}", glyph(GlyphRole::WorktreeBranch), group.label)
        }
        rimz::SidebarWorktreeKind::Channel => {
            format!("{} {}", glyph(GlyphRole::ChannelHash), group.label)
        }
        rimz::SidebarWorktreeKind::Root => group.label.clone(),
        rimz::SidebarWorktreeKind::External => unreachable!("external returned above"),
    };
    let mut cells = vec![render::cell(label).fg(render::palette::ACCENT.bold())];
    if let Some(team) = shared_group_team(group)
        && !group.label.ends_with(&format!("/{team}"))
    {
        cells.push(render::cell(format!("· {team} team")).fg(render::palette::META));
    }
    cells
}

fn channel_group_is_worktree_backed(
    group: &rimz::store::snapshot::AgentWorktreeGroup<'_>,
    snapshot: &rimz::SidebarSnapshot,
) -> bool {
    let Some(project_root) = snapshot.project_root.as_deref() else {
        return false;
    };
    let Some(first) = group
        .agents
        .first()
        .and_then(|agent| agent.worktree_path.as_deref())
    else {
        return false;
    };
    Path::new(first) != project_root
        && group
            .agents
            .iter()
            .all(|agent| agent.worktree_path.as_deref() == Some(first))
}

fn shared_group_team(group: &rimz::store::snapshot::AgentWorktreeGroup<'_>) -> Option<String> {
    let first = group
        .agents
        .first()
        .and_then(|agent| agent.team.as_deref())?;
    if group
        .agents
        .iter()
        .all(|agent| agent.team.as_deref() == Some(first))
    {
        Some(first.to_owned())
    } else {
        None
    }
}

fn list_channel_filter(
    all: bool,
    worktree: Option<&str>,
    workspace: &rimz::ResolvedWorkspace,
) -> Option<String> {
    list_channel_filter_for_current(all, worktree, crate::cli::current_channel(workspace))
}

fn list_channel_filter_for_current(
    all: bool,
    scope: Option<&str>,
    current_channel: Option<String>,
) -> Option<String> {
    match (scope, all) {
        (Some(scope), _) => Some(scope.trim_start_matches('#').to_owned()),
        (None, true) => None,
        (None, false) => current_channel,
    }
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
    // Fold the rich statusline context so the shown card — and the `--json`
    // payload — carries the real token window, not the carried-forward
    // `context_pct`.
    let snapshot = crate::cli::alive_snapshot(&store, &runtime, &workspace.session_name)?
        .with_agent_context(rimz::store::agent_context::read_all(&runtime));
    let agent_result = crate::cli::resolve_agent_one(
        &snapshot,
        &reference,
        None,
        crate::cli::current_channel(&workspace).as_deref(),
    );
    let mut agent = agent_result.as_ref().ok().map(|agent| (*agent).clone());
    let mut stale = false;
    let mut audit_error = None;
    if agent.is_none() {
        match resolve_audit_agent(&store, &workspace, &runtime, &reference) {
            Ok(Some(audit_agent)) => {
                agent = Some(audit_agent);
                stale = true;
            }
            Ok(None) => {}
            Err(err) => audit_error = Some(err),
        }
    }
    let run = newest_run_by_ref(&store, &reference, agent.as_ref())?;
    let ask = match agent.as_ref() {
        Some(agent) => crate::cli::transcript::latest_ask_view(&workspace, agent)?,
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
        .and_then(|agent| session_cost(&runtime, agent));
    let messages = match agent.as_ref() {
        Some(agent) => show_messages(&store, agent)?,
        None => Vec::new(),
    };
    let recent_transcript = match agent.as_ref() {
        Some(agent) => recent_agent_transcript(&workspace, agent).ok(),
        None => None,
    }
    .filter(|view| !view.entries.is_empty());
    if json {
        #[derive(serde::Serialize)]
        struct Show<'a> {
            #[serde(skip_serializing_if = "Option::is_none")]
            agent: Option<&'a AgentState>,
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
        }
        supervised::output::print_json(&Show {
            agent: agent.as_ref(),
            stale,
            run,
            ask,
            cost,
            messages,
            capture: pane_capture,
        })?;
        return Ok(());
    }
    let Some(agent) = agent.as_ref() else {
        if let Some(run) = run {
            print_run_line(&run)?;
            return Ok(());
        }
        if let Some(err) = audit_error {
            return Err(err);
        }
        return match agent_result {
            Err(err) => Err(err),
            Ok(_) => unreachable!("agent is present above"),
        };
    };
    let peers: Vec<&AgentState> = snapshot
        .agents
        .iter()
        .filter(|candidate| candidate.parent_agent_id.is_none())
        .collect();
    let now = jiff::Timestamp::now();
    let mut out = render::out();
    render_agent_section(&mut out, agent, &peers)?;
    render_activity_section(&mut out, agent, ask.as_ref(), stale, now)?;
    render_context_section(&mut out, agent, cost.as_ref())?;
    render_placement_section(&mut out, agent)?;
    if let Some(run) = run.or_else(|| newest_run_for_agent(&store, agent).ok().flatten()) {
        render_run_section(&mut out, &run, now)?;
    }
    if !messages.is_empty() {
        render_messages_section(&mut out, &messages)?;
    }
    if let Some(view) = recent_transcript.as_ref() {
        section(&mut out, "Recent transcript")?;
        let tz = crate::cli::machine_config().time_zone();
        crate::cli::transcript::render_lines_to(&mut out, view, &tz)?;
    }
    if let Some(capture) = pane_capture {
        use std::io::Write;

        writeln!(
            out,
            "{} {}",
            render::paint(render::palette::MUTED, "capture:"),
            capture.pane_id,
        )?;
        write!(out, "{}", capture.raw_text)?;
    }
    Ok(())
}

fn resolve_audit_agent(
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
    status: String,
    from: String,
    age: String,
    text: String,
}

fn section(w: &mut impl Write, title: &str) -> std::io::Result<()> {
    writeln!(
        w,
        "{}",
        render::paint(render::palette::ACCENT.bold(), title)
    )
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
            .fg(render::palette::ACCENT),
    );
    kv.push(
        "kind",
        render::cell(agent.kind.to_string()).fg(render::palette::META),
    );
    if let Some(profile) = agent.profile.as_deref() {
        kv.push("profile", render::cell(profile).fg(render::palette::META));
    }
    if let Some(role) = agent.role.as_deref() {
        kv.push("role", render::cell(role).fg(render::palette::META));
    }
    if let Some(team) = agent.team.as_deref() {
        kv.push("team", render::cell(team).fg(render::palette::META));
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
        render::cell(agent.activity_description().unwrap_or("-")).dash(),
    );
    kv.push(
        "status",
        render::cell(agent_status_label(agent)).fg(agent_status_style(agent)),
    );
    let (_, phase) = agent_status_projection(agent);
    if phase != rimz::agents::TurnPhase::Idle {
        kv.push("phase", render::cell(phase_label(phase)));
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
            render::cell(label.unwrap_or("provider API error")).fg(render::palette::ALARM),
        );
    }
    if let Some(ask) = ask {
        kv.push(
            "ask",
            render::cell(crate::cli::transcript::ask_summary(ask)).fg(render::palette::WARN),
        );
    }
    if stale {
        kv.push("stale", render::cell("yes").fg(render::palette::FAINT));
    }
    kv.render(w)?;
    writeln!(w)
}

fn render_context_section(
    w: &mut impl Write,
    agent: &AgentState,
    cost: Option<&rimz::agents::AgentCost>,
) -> std::io::Result<()> {
    section(w, "Context")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push("model", render::cell(model_label(agent)).dash());
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
        .dash(),
    );
    kv.render(w)?;
    writeln!(w)
}

fn render_placement_section(w: &mut impl Write, agent: &AgentState) -> std::io::Result<()> {
    section(w, "Placement")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push("channel", render::cell(worktree_label(agent)).dash());
    kv.push(
        "worktree",
        render::cell(agent.worktree_path.as_deref().unwrap_or("-")).dash(),
    );
    push_pane_anchor(&mut kv, agent);
    kv.render(w)?;
    writeln!(w)
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
            render::cell(preview(tail)).fg(render::palette::ALARM),
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
            render::cell(message.id.as_str()).fg(render::palette::ACCENT),
            render::cell(message.status.as_str()),
            render::cell(message.from.as_str()).fg(render::palette::META),
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
        format!("${value:.2}")
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
            status: message.status.as_str().to_owned(),
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
            || !rimz::message::card_matches(
                &payload.kind,
                &payload.agent_id,
                payload.agent_name.as_deref(),
                &agent.kind,
                &agent.agent_id,
                agent.name.as_deref(),
            )
        {
            continue;
        }
        delivered.push(ShowMessage {
            id: payload.message_id.to_string(),
            status: payload.status.as_str().to_owned(),
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
    backend
        .focus_pane(&pane.pane_id, Some(&workspace.session_name))
        .map_err(Into::into)
}

pub(super) fn wait_agent(
    reference: String,
    timeout: Option<Duration>,
    stream_output: bool,
    from_start: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let current_channel = crate::cli::current_channel(&workspace);
    let live_agent_result =
        crate::cli::resolve_agent_one(&snapshot, &reference, None, current_channel.as_deref());
    let live_agent = live_agent_result.as_ref().ok().copied();
    if let Some(run) = newest_run_by_ref(&store, &reference, live_agent)?
        && (!run.status.is_terminal() || live_agent.is_none() || run.run_id.as_str() == reference)
    {
        return wait_run_record(&store, &run, timeout, stream_output, from_start, json);
    }
    if live_agent.is_none() {
        live_agent_result?;
    }
    if stream_output {
        return wait_interactive_agent_stream(
            &store,
            &reference,
            current_channel.as_deref(),
            timeout,
            from_start,
            json,
        );
    }
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
        let agent =
            crate::cli::resolve_agent_one(&snapshot, &reference, None, current_channel.as_deref())?;
        if gate_open(DeliveryGate::Done, agent.status) {
            if json {
                supervised::output::print_json(agent)?;
            }
            std::process::exit(0);
        }
        if agent.status == rimz::agents::AgentStatus::Failed {
            if json {
                supervised::output::print_json(agent)?;
            }
            std::process::exit(RunStatus::Failed.exit_code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn wait_interactive_agent_stream(
    store: &rimz::Store,
    reference: &str,
    current_channel: Option<&str>,
    timeout: Option<Duration>,
    from_start: bool,
    json: bool,
) -> Result<()> {
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let agent = crate::cli::resolve_agent_one(&snapshot, reference, None, current_channel)?;
    let adapter = rimz::agents::find_adapter(agent.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent.kind))?;
    let mut cursor = supervised::stream::TranscriptCursor::new(from_start);
    let mut stdout = render::out();
    let mut stderr = render::err();
    let mut json_stdout = std::io::stdout().lock();
    let mut sink = if json {
        supervised::output::StreamSink::ndjson(&mut json_stdout)
    } else {
        supervised::output::StreamSink::text(&mut stdout, &mut stderr)
    };
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
        let agent = crate::cli::resolve_agent_one(&snapshot, reference, None, current_channel)?;
        for text in cursor.messages(agent.transcript_path.as_deref(), adapter) {
            sink.message(text)?;
        }
        sink.status(interactive_live_status(agent))?;
        if gate_open(DeliveryGate::Done, agent.status) {
            sink.end_status(RunStatus::Completed, None)?;
            std::process::exit(0);
        }
        if agent.status == rimz::agents::AgentStatus::Failed {
            sink.end_status(RunStatus::Failed, None)?;
            std::process::exit(RunStatus::Failed.exit_code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if sink.is_text() {
                sink.timeout()?;
            }
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn interactive_live_status(agent: &AgentState) -> rimz::harness::run::RunLiveStatus {
    rimz::harness::run::RunLiveStatus {
        agent_status: agent.status,
        phase: agent.phase,
        pane_id: agent.pane.as_ref().map(|pane| pane.pane_id.clone()),
        context_pct: agent
            .context_fill_pct()
            .map(|pct| pct.round().clamp(0.0, 100.0) as u8),
    }
}

pub(super) fn logs_agent(
    reference: String,
    tail: Option<usize>,
    follow: bool,
    all: bool,
    json: bool,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let target = agent_logs_target(&reference);
    if follow {
        return follow_agent_logs(&workspace, &target, tail, all, json);
    }
    let view = crate::cli::transcript::chat_view(&workspace, Some(&target), None, tail, all)?;
    if json {
        let entries: Vec<_> = view
            .entries
            .iter()
            .map(|entry| entry.chat.clone())
            .collect();
        render::finish(write_json_pretty(
            &serde_json::json!({ "entries": entries }),
        ))?;
    } else if view.entries.is_empty() {
        let mut out = render::err();
        writeln!(
            out,
            "{}",
            render::paint(
                render::palette::FAINT,
                view.empty_message
                    .as_deref()
                    .unwrap_or("No conversation recorded yet.")
            )
        )?;
    } else {
        let tz = crate::cli::machine_config().time_zone();
        let mut out = render::out();
        finish_transcript_render(crate::cli::transcript::render_lines_to(
            &mut out, &view, &tz,
        ))?;
    }
    Ok(())
}

fn agent_logs_target(reference: &str) -> String {
    if reference.starts_with('@') || reference.starts_with('#') {
        reference.to_owned()
    } else {
        format!("@{reference}")
    }
}

fn follow_agent_logs(
    workspace: &rimz::ResolvedWorkspace,
    target: &str,
    tail: Option<usize>,
    all: bool,
    json: bool,
) -> Result<()> {
    let initial = crate::cli::transcript::chat_view(workspace, Some(target), None, tail, all)?;
    let baseline = if tail.is_some() {
        crate::cli::transcript::chat_view(workspace, Some(target), None, None, all)?
            .entries
            .len()
    } else {
        initial.entries.len()
    };
    if json {
        for entry in &initial.entries {
            render::finish(write_json_line(&entry.chat))?;
        }
    } else if !initial.entries.is_empty() {
        let tz = crate::cli::machine_config().time_zone();
        let mut out = render::out();
        finish_transcript_render(crate::cli::transcript::render_lines_to(
            &mut out, &initial, &tz,
        ))?;
    }

    let tz = crate::cli::machine_config().time_zone();
    let mut seen = baseline;
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let view = crate::cli::transcript::chat_view(workspace, Some(target), None, None, all)?;
        if view.entries.len() <= seen {
            continue;
        }
        let new_entries = view.entries[seen..].to_vec();
        seen = view.entries.len();
        if json {
            for entry in new_entries {
                render::finish(write_json_line(&entry.chat))?;
            }
        } else {
            let mut out = render::out();
            let delta = crate::cli::transcript::RenderedChat {
                channel: view.channel.clone(),
                focus: view.focus.clone(),
                entries: new_entries,
                archive_prefix: 0,
                archived_hidden: 0,
                newest_archived_at: None,
                empty_message: None,
            };
            finish_transcript_render(crate::cli::transcript::render_lines_to(
                &mut out, &delta, &tz,
            ))?;
        }
    }
}

fn finish_transcript_render(write: Result<()>) -> Result<()> {
    render::finish(write.map_err(|err| match err.downcast::<std::io::Error>() {
        Ok(err) => err,
        Err(err) => std::io::Error::other(err),
    }))
}

fn write_json_line(value: &impl serde::Serialize) -> std::io::Result<()> {
    let line = serde_json::to_string(value).map_err(std::io::Error::other)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{line}")
}

fn write_json_pretty(value: &impl serde::Serialize) -> std::io::Result<()> {
    let pretty = serde_json::to_string_pretty(value).map_err(std::io::Error::other)?;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{pretty}")
}

pub(super) fn stop_agent(reference: String, all: bool, globals: &GlobalFlags) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::open_store(&workspace)?;
    let snapshot = store.snapshot_cached().context("reading agent snapshot")?;
    let current_channel = crate::cli::current_channel(&workspace);
    if all {
        let agents = rimz::harness::target::resolve_many(
            &snapshot,
            &reference,
            None,
            current_channel.as_deref(),
        )?;
        let peers: Vec<&AgentState> = snapshot.root_agents().collect();
        let mut failed = false;
        let mut out = render::out();
        for agent in agents {
            let label = rimz::harness::target::agent_handle(agent, &peers, true);
            match stop_live_agent(&workspace, &store, globals, agent) {
                Ok(()) => writeln!(out, "stopped {label}")?,
                Err(err) => {
                    failed = true;
                    writeln!(out, "error {label}: {err:#}")?;
                }
            }
        }
        if failed {
            std::process::exit(1);
        }
        return Ok(());
    }
    let live_agent_result =
        crate::cli::resolve_agent_one(&snapshot, &reference, None, current_channel.as_deref());
    let live_agent = live_agent_result.as_ref().ok().copied();
    if let Some(run) = newest_run_by_ref(&store, &reference, live_agent)? {
        stop_run(&workspace, &store, globals, &run)?;
        return Ok(());
    }
    let live_agent = live_agent_result.map_err(|err| stop_resolve_error(err, &reference))?;
    close_agent_pane(&workspace, live_agent)
}

fn stop_resolve_error(err: anyhow::Error, reference: &str) -> anyhow::Error {
    let Some(target_err) = err.downcast_ref::<rimz::TargetErr>() else {
        return err;
    };
    if matches!(target_err, rimz::TargetErr::Ambiguous { .. }) {
        anyhow::anyhow!(
            "{target_err}; re-run `rimz agents stop {reference} --all` to stop every match"
        )
    } else {
        err
    }
}

fn stop_live_agent(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
    agent: &AgentState,
) -> Result<()> {
    if let Some(run) = newest_run_for_agent(store, agent)? {
        stop_run(workspace, store, globals, &run)
    } else {
        close_agent_pane(workspace, agent)
    }
}

fn stop_run(
    workspace: &rimz::ResolvedWorkspace,
    store: &rimz::Store,
    globals: &GlobalFlags,
    run: &RunRecord,
) -> Result<()> {
    if run_stop_should_cancel(run) {
        let (record, wrote) = rimz::harness::run::cancel(store.paths(), &run.run_id)?;
        if wrote {
            rimz::store::wakeup::wake_run(store.runtime_paths(), &record)
                .context("waking run waiter")?;
        }
    }
    if let Ok(backend) = supervised::pane::backend_for_workspace_session(workspace, globals) {
        supervised::pane::close_stopped_run_pane_after_grace(
            backend.as_ref(),
            store,
            &workspace.session_name,
            run,
            supervised::pane::STOP_BACKSTOP_GRACE,
        );
    }
    Ok(())
}

/// Whether `stop` must cancel a run's supervision before reclaiming its pane.
/// Live runs are canceled so a blocking `-p` waiter wakes. Terminal runs, such
/// as completed `--keep` agents, keep their record and only lose the pane.
pub(super) fn run_stop_should_cancel(run: &RunRecord) -> bool {
    !run.status.is_terminal()
}

fn close_agent_pane(workspace: &rimz::ResolvedWorkspace, agent: &AgentState) -> Result<()> {
    let pane = agent
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    backend
        .close_pane(&workspace.session_name, &pane.pane_id)
        .map_err(Into::into)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RunPlacement {
    Split,
    Tab,
}

/// A supervised `-p` run hosts its agent pane in a split of the current tab so
/// focus stays with the caller; it opens a new tab only when forced or when
/// there is no ambient pane to split.
pub(super) fn run_placement(force_new_tab: bool, has_ambient_pane: bool) -> RunPlacement {
    if force_new_tab || !has_ambient_pane {
        RunPlacement::Tab
    } else {
        RunPlacement::Split
    }
}

pub(super) fn run_print(args: AgentsArgs, globals: &GlobalFlags) -> Result<Option<RunRecord>> {
    let output_format = args.output_format.unwrap_or_default();
    let record = run_supervised(args, globals)?;
    let Some(record_ref) = record.as_ref() else {
        return Ok(record);
    };
    match output_format {
        OutputFormat::Text => {
            let mut stdout = render::out();
            let mut stderr = render::err();
            supervised::output::print_run_output(record_ref, &mut stdout, &mut stderr)?
        }
        OutputFormat::Json => supervised::output::print_json(record_ref)?,
        // stream-json already emitted its events as the run progressed.
        OutputFormat::StreamJson => {}
    }
    Ok(record)
}

pub(super) fn run_supervised(args: AgentsArgs, globals: &GlobalFlags) -> Result<Option<RunRecord>> {
    if args.json {
        bail!("on `-p`, choose output with `--output-format json` (`--json` is for `list`)");
    }
    let output_format = args.output_format.unwrap_or_default();
    let input_format = args.input_format.unwrap_or_default();
    if args.bg && output_format == OutputFormat::StreamJson {
        bail!("--output-format stream-json cannot be combined with --bg");
    }
    let prompt = resolve_print_prompt(&args, input_format)?;
    let workspace = supervised::resolve_run_workspace(globals)?;
    let machine_config = crate::cli::machine_config();
    let mode = supervised_permission_mode_from_flags(args.ask, args.yolo)?;
    let PreparedLaunch {
        profiles: _profiles,
        teams: _teams,
        mut layout,
        team_name: _team_name,
    } = prepare_launch_layout(&args, &workspace, &machine_config, Some(mode), None)?;
    if let Some(limit) = args.max_turns {
        apply_supervised_turn_limit(&mut layout, limit)?;
    }
    let agent_cells = agent_cells(&layout);
    if agent_cells.len() != 1 {
        bail!("--print requires a layout with exactly one agent cell");
    }
    if layout_cell_count(&layout) != 1 {
        bail!("--print requires a single-cell agent layout");
    }
    let agent_cell = agent_cells[0];
    let adapter = rimz::agents::find_adapter(agent_cell.kind)
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", agent_cell.kind))?;
    let launch_invocation = rimz::harness::launch::ExecInvocation {
        kind: agent_cell.kind,
        action: rimz::harness::launch::ExecAction::Launch {
            prompt: Some(&prompt),
            extra_args: &[],
        },
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        identity: rimz::harness::launch::ExecIdentity {
            profile: agent_cell.profile,
            role: agent_cell.role,
            channel: args.channel.as_deref(),
            model: agent_cell.model,
            effort: agent_cell.effort,
            ..rimz::harness::launch::ExecIdentity::default()
        },
    };
    let launch_env = full_agent_launch_env(
        &workspace.project_root,
        adapter,
        machine_config.harness.rtk,
        &launch_invocation,
    )?;
    supervised::preflight_agent(adapter)?;
    supervised::preflight_program(adapter, agent_cell.args, &prompt, &launch_env)?;

    let launch = crate::cli::agents_launch::resolve_cwd(
        &workspace,
        &machine_config.agents.worktree,
        args.worktree.as_deref(),
        args.from_pr.as_ref(),
    )?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let backend = rimz::mux::backend_for(mux);
    let mux_config = rimz::config::MultiplexerConfig::from(machine_config.as_ref());
    let width = rimz::mux::SidebarWidth::from_config(&machine_config.theme.display);
    let detected_size = rimz::mux::detect_terminal_size();
    let was_live = backend.list_sessions()?.contains(&workspace.session_name);
    let store = crate::cli::open_store(&workspace)?;
    if let Some(channel) = args.channel.as_deref() {
        crate::cli::channel::ensure_named_channel_available(&workspace, channel)?;
        rimz::channel::register(store.paths(), channel)?;
    }
    let room_channel = rimz::harness::target::resolve_room_channel(
        &workspace.project_root,
        &launch.cwd,
        None,
        args.channel.as_deref(),
    );
    if !was_live {
        crate::cli::room::purge_rebirth_heartbeats_for_workspace(&workspace.workspace_id);
    }
    backend.ensure_session(&rimz::mux::SessionOptions {
        session_name: workspace.session_name.clone(),
        workspace_id: workspace.workspace_id.clone(),
        project_root: workspace.project_root.clone(),
        cwd: launch.cwd.clone(),
        config: mux_config.clone(),
        detected_size,
        truecolor: rimz::tui::truecolor(),
    })?;
    if !was_live {
        crate::cli::room::record_rebirth_boundary(&workspace.workspace_id, &workspace.session_name);
    }
    // A supervised run can birth the room, so the focus chord registers here
    // too (tmux binds it; Zellij routes it through the presence plugin below) —
    // the key reaches the sidebar from any pane regardless of how the room came
    // to be.
    crate::cli::room::register_focus_key(backend.as_ref(), &machine_config);
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &launch.cwd,
        mux_config: &mux_config,
        width,
        detected_size: if was_live { None } else { detected_size },
        refresh_ms: None,
    };
    crate::cli::room::launch_sidebar_for_workspace(backend.as_ref(), &room, None, !was_live, &[]);
    crate::cli::room::gate_room_before_attach(backend.as_ref(), &room, None, &[])?;
    crate::cli::room::ensure_presence_plugin(
        backend.as_ref(),
        &workspace.session_name,
        &workspace.workspace_id,
        &mux_config.zellij,
        machine_config.web.enabled,
        machine_config.sidebar.focus_key_label(),
    );

    let permission_mode = agent_cell.mode.unwrap_or(mode);
    let mut record = RunRecord::new(
        workspace.workspace_id.clone(),
        AgentKind::new_unchecked(adapter.descriptor().kind),
        permission_mode,
        prompt.clone(),
        launch.cwd.clone(),
    );
    let run_id = record.run_id.clone();
    let launch_requests = launch_identity_requests(
        &layout,
        args.name.as_deref(),
        generated_worktree_name(&launch),
        None,
        None,
        room_channel.as_deref(),
    )?;
    let launch_requests = launch_requests
        .into_iter()
        .map(|mut request| {
            request.run_id = Some(run_id.clone());
            request
        })
        .collect::<Vec<_>>();
    let mut launch_identities = store.append_agent_launches_allocating(
        &launch_requests,
        &AgentLaunchAppend {
            workspace_id: workspace.workspace_id.clone(),
            session_name: workspace.session_name.clone(),
            cwd: launch.cwd.clone(),
            worktree_name: launch.worktree_name.clone(),
            channel: room_channel.clone(),
            prompt: Some(prompt.clone()),
            description: args.description.clone(),
            state: rimz::store::event::AgentLaunchState::Starting,
            pane_id: None,
        },
    )?;
    let launch_identity = launch_identities
        .pop()
        .ok_or_else(|| anyhow::anyhow!("--print requires one agent cell"))?;
    record.agent_name = Some(launch_identity.name.clone());
    let pane = supervised::run_pane_cmd(supervised::RunPaneCmdArgs {
        adapter,
        run_id: &run_id,
        agent_name: Some(&launch_identity.name),
        agent_name_explicit: launch_identity.name_explicit,
        agent_profile: agent_cell.profile,
        agent_role: agent_cell.role,
        agent_channel: room_channel.as_deref(),
        agent_model: agent_cell.model,
        agent_effort: agent_cell.effort,
        launch_id: Some(&launch_identity.agent_id),
        cwd: &launch.cwd,
        prompt: &prompt,
        cleanup_worktree: args.worktree.is_some() || args.from_pr.is_some(),
        permission_args: agent_cell.args,
        self_cleanup_on_completion: args.bg && !args.keep,
    })?;
    let bound = if args.bg {
        None
    } else {
        Some(run_wake::bind_run(store.runtime_paths(), &run_id).context("binding run socket")?)
    };
    let interrupt = if args.bg {
        None
    } else {
        Some(supervised::install_run_interrupt_flag()?)
    };
    let socket_guard = bound
        .as_ref()
        .map(|(_sock, sock_path)| SocketGuard::new(sock_path.clone()));
    rimz::harness::run::create(store.paths(), &record).context("recording run")?;
    let target = own_pane_id(mux);
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let open_result = match run_placement(args.new_tab, target.is_some()) {
        RunPlacement::Split => backend.split_pane(SplitPaneOptions {
            target_pane_id: target,
            cwd: Some(launch.cwd.to_string_lossy().into_owned()),
            command: Some(pane.argv.clone()),
            env: crate::cli::agents_launch::launch_identity_env(
                &workspace,
                room_channel.as_deref(),
                args.worktree.is_none() && args.from_pr.is_none(),
            ),
            direction,
            focus: false,
        }),
        RunPlacement::Tab => backend.open_tab(&TabOptions {
            session_name: workspace.session_name.clone(),
            title: format!("run {}", adapter.descriptor().kind),
            cwd: launch.cwd.clone(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![pane],
                    stacked: false,
                }],
            },
            focus: false,
            dock_sidebar: true,
            sidebar: crate::cli::room::build_sidebar_opts(&room, Vec::new())?,
        }),
    };
    if let Err(err) = open_result {
        let _ = rimz::harness::run::fail(store.paths(), &run_id);
        let _ = append_launch_event(
            &store,
            &workspace,
            &launch_identity,
            LaunchEventParams {
                cwd: &launch.cwd,
                worktree_name: launch.worktree_name.as_deref(),
                channel: room_channel.as_deref(),
                prompt: Some(&prompt),
                state: rimz::store::event::AgentLaunchState::Failed,
                pane_id: None,
            },
        );
        return Err(err).context("opening run pane");
    }
    if args.bg {
        #[expect(clippy::print_stdout, reason = "command result is the agent name")]
        {
            println!("{}", launch_identity.name);
        }
        return Ok(None);
    }
    let Some((sock, _sock_path)) = bound else {
        bail!("blocking run did not bind its completion socket");
    };
    let expected = ExpectedRunFrame {
        workspace_id: workspace.workspace_id.clone(),
        run_id: run_id.clone(),
    };
    let mut record = if output_format == OutputFormat::StreamJson {
        supervised::stream::stream_blocking_run(
            sock,
            expected,
            &store,
            &run_id,
            adapter,
            args.timeout,
            interrupt
                .as_deref()
                .expect("blocking run has interrupt flag"),
        )?
    } else {
        let outcome = supervised::wait_for_run(
            sock,
            expected,
            args.timeout,
            interrupt
                .as_deref()
                .expect("blocking run has interrupt flag"),
        )?;
        supervised::terminal_record_after_wait(store.paths(), &run_id, outcome)?
    };
    record = record_failure_tail_before_cleanup(
        backend.as_ref(),
        &store,
        &workspace.session_name,
        record,
    );
    if !args.keep {
        if record.status == RunStatus::Canceled {
            supervised::pane::close_stopped_run_pane_after_grace(
                backend.as_ref(),
                &store,
                &workspace.session_name,
                &record,
                supervised::pane::STOP_BACKSTOP_GRACE,
            );
        } else {
            supervised::pane::close_run_pane(
                backend.as_ref(),
                &store,
                &workspace.session_name,
                &record,
            );
        }
    }
    drop(socket_guard);
    Ok(Some(record))
}

fn record_failure_tail_before_cleanup(
    backend: &dyn rimz::mux::MuxBackend,
    store: &rimz::Store,
    session_name: &str,
    record: RunRecord,
) -> RunRecord {
    if record.status == RunStatus::Completed || record.failure_tail.is_some() {
        return record;
    }
    let Some(pane) = supervised::pane::resolve_run_pane(store, session_name, &record) else {
        return record;
    };
    let Some(tail) = supervised::pane::capture_failure_tail(backend, &pane.pane_id) else {
        return record;
    };
    match rimz::harness::run::record_failure_tail(store.paths(), &record.run_id, &tail) {
        Ok(record) => record,
        Err(err) => {
            tracing::debug!(
                run_id = %record.run_id,
                pane = %pane.pane_id,
                error = %err,
                "could not record supervised run failure pane tail",
            );
            record
        }
    }
}

/// Resolve the supervised prompt from text input or, for `--input-format
/// stream-json`, from stream-json user messages on stdin.
fn resolve_print_prompt(args: &AgentsArgs, input_format: InputFormat) -> Result<String> {
    match input_format {
        InputFormat::Text => {
            let piped = crate::cli::send::read_piped_text_prompt()?;
            crate::cli::send::combine_text_prompt(args.prompt.as_deref(), piped.as_deref())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "expected a prompt for `rimz agents <spec> -p` (positional PROMPT or piped stdin)"
                    )
                })
        }
        InputFormat::StreamJson => {
            if args.prompt.as_deref().is_some_and(|p| !p.trim().is_empty()) {
                bail!(
                    "--input-format stream-json reads the prompt from stdin; drop the positional PROMPT"
                );
            }
            let prompt = supervised::read_stream_json_prompt(std::io::stdin().lock())
                .context("reading stream-json prompt from stdin")?;
            if prompt.trim().is_empty() {
                bail!("--input-format stream-json received no user message text on stdin");
            }
            Ok(prompt)
        }
    }
}

fn wait_run_record(
    store: &rimz::Store,
    run: &RunRecord,
    timeout: Option<Duration>,
    stream_output: bool,
    from_start: bool,
    json: bool,
) -> Result<()> {
    let adapter = rimz::agents::find_adapter(run.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", run.kind))?;
    if stream_output {
        let mut stdout = render::out();
        let mut stderr = render::err();
        let mut json_stdout = std::io::stdout().lock();
        let mut sink = if json {
            supervised::output::StreamSink::ndjson(&mut json_stdout)
        } else {
            supervised::output::StreamSink::text(&mut stdout, &mut stderr)
        };
        match supervised::stream::stream_attached_run(
            store,
            &run.run_id,
            adapter,
            from_start,
            timeout,
            &mut sink,
        )? {
            Some(record) => std::process::exit(record.status.exit_code()),
            None => std::process::exit(RunStatus::TimedOut.exit_code()),
        }
    }
    let deadline = timeout.map(|duration| Instant::now() + duration);
    loop {
        let current = rimz::harness::run::load(store.paths(), &run.run_id)?;
        if current.status.is_terminal() {
            if json {
                supervised::output::print_json(&current)?;
            } else {
                let mut stdout = render::out();
                let mut stderr = render::err();
                supervised::output::print_run_output(&current, &mut stdout, &mut stderr)?;
            }
            std::process::exit(current.status.exit_code());
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            std::process::exit(RunStatus::TimedOut.exit_code());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn newest_run_for_agent(store: &rimz::Store, agent: &AgentState) -> Result<Option<RunRecord>> {
    newest_run_by_ref(store, agent.name.as_deref().unwrap_or(""), Some(agent))
}

fn newest_run_by_ref(
    store: &rimz::Store,
    reference: &str,
    agent: Option<&AgentState>,
) -> Result<Option<RunRecord>> {
    let mut records = rimz::harness::run::list(store.paths())?;
    records.retain(|record| {
        if record.run_id.as_str() == reference || record.agent_name.as_deref() == Some(reference) {
            return true;
        }
        if let Some(agent) = agent {
            return record.kind == agent.kind
                && (record.agent_id.as_ref() == Some(&agent.agent_id)
                    || record.agent_name.as_deref() == agent.name.as_deref());
        }
        false
    });
    records.sort_by_key(|record| std::cmp::Reverse(record.started_at));
    Ok(records.into_iter().next())
}

fn print_run_line(run: &RunRecord) -> std::io::Result<()> {
    use std::io::Write;
    let status = supervised::output::status_label(run.status);
    writeln!(
        render::out(),
        "{} {} {} {}",
        render::paint(render::palette::MUTED, "run:"),
        run.run_id,
        render::paint(render::status::run(run.status), status),
        run.prompt,
    )
}

/// The columns shared by `agents list` in display order. The lead `AGENT` cell
/// omits `#channel` because its section header carries that scope.
fn agent_row(agent: &AgentState, peers: &[&AgentState], now: jiff::Timestamp) -> Vec<render::Cell> {
    vec![
        render::cell(rimz::harness::target::agent_handle(agent, peers, false))
            .fg(render::palette::ACCENT),
        render::cell(agent_status_label(agent)).fg(agent_status_style(agent)),
        model_cell(agent),
        context_cell(agent),
        render::cell(tokens_label(agent)).dash(),
        render::cell(render::age_short(agent.last_seen, now)),
        render::cell(agent.activity_description().unwrap_or("-")).dash(),
    ]
}

/// Context fill warms as it climbs: gold past 75%, rose past 90%.
fn context_cell(agent: &AgentState) -> render::Cell {
    let pct = agent.context_fill_pct();
    let text = pct
        .map(|pct| format!("{}%", pct.round() as u8))
        .unwrap_or_else(|| "-".to_owned());
    let c = render::cell(text);
    match pct {
        Some(pct) if pct >= 90.0 => c.fg(render::palette::ALARM),
        Some(pct) if pct >= 75.0 => c.fg(render::palette::WARN),
        Some(_) => c,
        None => c.dash(),
    }
}

/// A launchable agent cell from a resolved layout.
#[derive(Clone, Copy)]
struct AgentCell<'a> {
    kind: &'a str,
    args: &'a [String],
    mode: Option<PermissionMode>,
    profile: Option<&'a str>,
    role: Option<&'a str>,
    model: Option<&'a str>,
    effort: Option<&'a str>,
}

fn agent_cells(layout: &LayoutSpec) -> Vec<AgentCell<'_>> {
    layout
        .agent_cells()
        .filter_map(|cell| match cell {
            Cell::Agent {
                kind,
                args,
                mode,
                profile,
                role,
                model,
                effort,
                ..
            } => Some(AgentCell {
                kind: kind.as_str(),
                args: args.as_slice(),
                mode: *mode,
                profile: profile.as_deref(),
                role: role.as_deref(),
                model: model.as_deref(),
                effort: effort.as_deref(),
            }),
            Cell::Command { .. } => None,
        })
        .collect()
}

fn layout_cell_count(layout: &LayoutSpec) -> usize {
    layout.columns.iter().map(|column| column.rows.len()).sum()
}

fn agent_name(agent: &AgentState) -> &str {
    agent.name.as_deref().unwrap_or(agent.agent_id.as_str())
}

fn agent_status_label(agent: &AgentState) -> &'static str {
    agent_status_projection(agent).0.as_str()
}

fn agent_status_style(agent: &AgentState) -> anstyle::Style {
    let (status, phase) = agent_status_projection(agent);
    render::status::agent(status, phase)
}

fn agent_status_projection(
    agent: &AgentState,
) -> (rimz::agents::AgentStatus, rimz::agents::TurnPhase) {
    match agent.displayed_turn_error().map(|(class, _)| class) {
        Some(rimz::agents::TurnErrorClass::PausedRateLimit)
        | Some(rimz::agents::TurnErrorClass::PausedSpendLimit)
        | Some(rimz::agents::TurnErrorClass::PausedOverloaded) => (
            rimz::agents::AgentStatus::Paused,
            rimz::agents::TurnPhase::Idle,
        ),
        Some(rimz::agents::TurnErrorClass::Unknown)
        | Some(rimz::agents::TurnErrorClass::Failed) => (
            rimz::agents::AgentStatus::Failed,
            rimz::agents::TurnPhase::Idle,
        ),
        None => (agent.status, agent.phase),
    }
}

fn phase_label(phase: rimz::agents::TurnPhase) -> &'static str {
    match phase {
        rimz::agents::TurnPhase::Idle => "idle",
        rimz::agents::TurnPhase::Reasoning => "reasoning",
        rimz::agents::TurnPhase::Acting => "acting",
        rimz::agents::TurnPhase::Parked => "parked",
    }
}

fn model_label(agent: &AgentState) -> String {
    match (agent.model.as_deref(), agent.effort.as_deref()) {
        (Some(model), Some(effort)) => format!("{model}@{effort}"),
        (Some(model), None) => model.to_owned(),
        (None, Some(effort)) => format!("auto@{effort}"),
        (None, None) => "-".to_owned(),
    }
}

fn model_cell(agent: &AgentState) -> render::Cell {
    let label = model_label(agent);
    if label == "-" {
        return render::cell(label).dash();
    }
    match brand_style(agent.kind.as_str()) {
        Some(style) => render::cell(label).fg(style),
        None => render::cell(label),
    }
}

/// The agent kind's brand tone for truecolor output, or `None` for an unknown kind.
fn brand_style(kind: &str) -> Option<anstyle::Style> {
    let (r, g, b) = rimz::agents::descriptor_by_kind(kind)?.brand.color_rgb;
    Some(render::palette::rgb((r, g, b)))
}

fn tokens_label(agent: &AgentState) -> String {
    agent
        .total_tokens
        .map(compact_count)
        .unwrap_or_else(|| "-".to_owned())
}

fn compact_count(value: u64) -> String {
    if value < 1_000 {
        value.to_string()
    } else if value < 1_000_000 {
        format!("{}k", value / 1_000)
    } else {
        format!("{}m", value / 1_000_000)
    }
}

/// The agent's channel for display, dashed when it runs outside any worktree.
/// The channel itself comes from [`rimz::harness::target::agent_channel`], the single
/// source of truth; this only chooses the `-` placeholder over the resolver's
/// prose label.
fn worktree_label(agent: &AgentState) -> String {
    rimz::harness::target::agent_channel(agent).unwrap_or_else(|| "-".to_owned())
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

    #[test]
    fn list_channel_filter_resolves_explicit_all_and_current_channel() {
        assert_eq!(
            list_channel_filter_for_current(true, Some("manual"), Some("feature".to_owned()))
                .as_deref(),
            Some("manual")
        );
        assert_eq!(
            list_channel_filter_for_current(false, Some("#manual"), Some("feature".to_owned()))
                .as_deref(),
            Some("manual")
        );
        assert_eq!(
            list_channel_filter_for_current(true, None, Some("feature".to_owned())),
            None
        );
        assert_eq!(
            list_channel_filter_for_current(false, None, Some("feature".to_owned())).as_deref(),
            Some("feature")
        );
        assert_eq!(list_channel_filter_for_current(false, None, None), None);
    }

    #[test]
    fn in_room_agent_ids_keeps_only_pane_bound_sessions() {
        let mut snapshot = rimz::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::parse("ws_000000000000000000000000").expect("workspace id"),
            vec![
                test_agent("sess-one"),
                test_agent("sess-two"),
                test_agent("sess-paneless"),
            ],
            jiff::Timestamp::UNIX_EPOCH,
        );
        snapshot.agent_panes = vec![
            test_pane_agent("sess-one", "terminal_1"),
            test_pane_agent("sess-two", "terminal_2"),
            rimz::PaneAgent {
                kind: AgentKind::new_unchecked("codex"),
                kind_ordinal: None,
                name: None,
                name_explicit: false,
                profile: None,
                role: None,
                channel: None,
                agent_id: None,
                pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, "terminal_lazy"),
                pane_pid: None,
                worktree_path: None,
                worktree_branch: None,
            },
        ];

        let in_room = in_room_agent_ids(&snapshot);
        let kept: Vec<&str> = snapshot
            .agents
            .iter()
            .filter(|agent| agent.parent_agent_id.is_none())
            .filter(|agent| in_room.contains(&agent.agent_id))
            .map(|agent| agent.agent_id.as_str())
            .collect();

        assert_eq!(kept, ["sess-one", "sess-two"]);
    }

    #[test]
    fn brand_style_uses_registered_agent_brand_rgb() {
        for kind in ["claude", "codex"] {
            let expected = rimz::agents::descriptor_by_kind(kind)
                .expect("registered descriptor")
                .brand
                .color_rgb;
            assert_eq!(brand_style(kind), Some(render::palette::rgb(expected)));
        }

        assert_eq!(brand_style("unknown"), None);
    }

    fn test_agent(id: &str) -> AgentState {
        rimz::testkit::agent_state("codex", id, jiff::Timestamp::UNIX_EPOCH)
    }

    fn test_pane_agent(agent_id: &str, pane: &str) -> rimz::PaneAgent {
        rimz::PaneAgent {
            kind: AgentKind::new_unchecked("codex"),
            kind_ordinal: None,
            name: None,
            name_explicit: false,
            profile: None,
            role: None,
            channel: None,
            agent_id: Some(AgentSessionId::from(agent_id)),
            pane_id: rimz::PaneId::from_parts(rimz::MuxName::Zellij, pane),
            pane_pid: None,
            worktree_path: None,
            worktree_branch: None,
        }
    }
}
