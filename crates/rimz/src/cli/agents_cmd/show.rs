use super::*;

use super::list::agent_pr;
use super::report::{
    AgentReportEntry, PrInfo, ReportOverrides, SelfIdentity, build_entry, child_for_agent,
    row_for_agent, status_style,
};
use super::runs_lookup::{agent_name, newest_run_by_ref, newest_run_for_agent, print_run_line};
use crate::cli::render;

#[derive(serde::Serialize, Default)]
struct ShowReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    tmp_dir: Option<std::path::PathBuf>,
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
    #[serde(skip)]
    ask_next_step: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    messages: Vec<ShowMessage>,
    #[serde(skip)]
    system_messages_hidden: usize,
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
    let agent_result = resolve_live_or_audit(
        store,
        workspace,
        runtime,
        snapshot,
        reference,
        &crate::cli::current_channel(workspace, Some(store)).address_context(),
    );
    let (agent, stale, deferred_error) = match agent_result {
        Ok((agent, live)) => (Some(agent), !live, None),
        // A live ambiguity or bad selector is the answer; a miss, its launch hint,
        // or an audit read error still lets a run match the reference.
        Err(err)
            if err.downcast_ref::<render::AmbiguousAgents>().is_some()
                || (err.downcast_ref::<rimz::address::TargetErr>().is_some()
                    && !crate::cli::resolution_missed(&err)) =>
        {
            return Err(err);
        }
        Err(err) => (None, false, Some(err)),
    };
    let run = newest_run_by_ref(store, reference, agent.as_ref())?;
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
                .capture_pane(&pane.pane_id, &workspace.session_name, None, ansi)
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
    let (messages, system_messages_hidden) = match agent.as_ref() {
        Some(agent) => show_messages(store, agent)?,
        None => (Vec::new(), 0),
    };
    let recent_transcript = match agent.as_ref() {
        Some(agent) => recent_agent_transcript(workspace, agent).ok(),
        None => None,
    }
    .filter(|view| !view.entries.is_empty());
    let reader = crate::cli::current_channel(workspace, Some(store)).address_context();
    let me = SelfIdentity::from_env().resolve(snapshot);
    let now = jiff::Timestamp::now();
    let demotion = rimz::harness::park_demotion(
        Some(store),
        store.paths(),
        runtime,
        &crate::cli::machine_config(),
        &snapshot.agents,
        snapshot.resume_outcomes.as_deref().unwrap_or_default(),
        now,
    );
    let report_agent = agent.as_ref().map(|agent| {
        build_entry(
            &demotion,
            agent,
            row_for_agent(snapshot, agent),
            agent_pr(snapshot, agent),
            rimz::address::paste_address(agent, snapshot, &reader),
            me.as_ref(),
            now,
            ReportOverrides {
                child: child_for_agent(snapshot, agent),
                runtime: Some(runtime),
                effort: effort.map(|(effort, _)| effort),
                active_secs: effort.and_then(|(_, active_secs)| active_secs),
                budget_cost_usd,
            },
        )
    });
    let (ask, ask_next_step) = match agent.as_ref().zip(report_agent.as_ref()) {
        Some((agent, entry)) => (
            crate::cli::transcript::latest_ask_view(workspace, agent)?,
            show_ask_next_step(store.paths(), agent, &entry.handle)?,
        ),
        None => (None, None),
    };
    Ok((
        ShowReport {
            tmp_dir: agent
                .as_ref()
                .map(|agent| {
                    rimz::harness::launch_plan::agent_temp_unit(
                        agent,
                        &snapshot.agents,
                        store.paths(),
                    )
                })
                .filter(|dir| dir.exists()),
            agent: report_agent,
            agent_state: agent,
            stale,
            run,
            ask,
            ask_next_step,
            messages,
            system_messages_hidden,
            capture: pane_capture,
            recent_transcript,
        },
        deferred_error,
    ))
}

fn render_show_report(
    mut report: ShowReport,
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
    if report.run.is_none() {
        report.run = newest_run_for_agent(store, state).ok().flatten();
    }
    render_show_sections(&mut out, &report, agent, now)?;
    Ok(())
}

fn render_show_sections(
    w: &mut impl Write,
    report: &ShowReport,
    agent: &AgentReportEntry,
    now: jiff::Timestamp,
) -> Result<()> {
    render_agent_section(w, agent)?;
    render_activity_section(
        w,
        agent,
        report.ask.as_ref(),
        report.ask_next_step.as_deref(),
        report.stale,
        now,
    )?;
    if let Some(run) = report.run.as_ref() {
        render_run_section(w, run, now)?;
    }
    if !report.messages.is_empty() || report.system_messages_hidden > 0 {
        render_messages_section(
            w,
            &report.messages,
            report.system_messages_hidden,
            &agent.id,
        )?;
    }
    if let Some(view) = report.recent_transcript.as_ref() {
        section(w, "Recent transcript")?;
        let tz = crate::cli::machine_config().time_zone();
        crate::cli::transcript::render_lines_to(w, view, &tz, render::prose::Prose::for_stdout())?;
        writeln!(w)?;
    }
    render_context_section(w, agent, now)?;
    render_identity_section(w, agent, report.tmp_dir.as_deref())?;
    writeln!(w)?;
    if let Some(capture) = report.capture.as_ref() {
        render_capture_section(w, capture)?;
    }
    Ok(())
}

fn show_ask_next_step(
    paths: &rimz::StatePaths,
    state: &AgentState,
    handle: &str,
) -> Result<Option<String>> {
    Ok(
        rimz::agents::read_open_ask(paths, state, None)?.map(|detail| {
            crate::cli::ask_commands::AskCommands {
                detail: &detail,
                pane: handle,
                target: handle,
            }
            .next_step()
        }),
    )
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
    let snapshot = ctx.published_snapshot()?;
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
        // A miss is an error in either format: an empty report is not an answer.
        if report.agent.is_none() && report.run.is_none() {
            return Err(
                deferred_error.unwrap_or_else(|| anyhow::anyhow!("agent resolution failed"))
            );
        }
        return render::json_pretty(&report);
    }
    render_show_report(report, &ctx.store, deferred_error)
}

pub(super) fn resolve_audit_agent(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    runtime: &rimz::RuntimePaths,
    reference: &str,
    context: &rimz::address::AddressContext,
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
    let matches = match rimz::address::resolve_many(&snapshot, reference, None, context) {
        Ok(matches) if matches.len() > 1 => {
            return Err(render::AmbiguousAgents::new(
                reference, &matches, &snapshot, context, true,
            )
            .into());
        }
        Ok(matches) => matches,
        Err(
            rimz::address::TargetErr::NoMatch { .. }
            | rimz::address::TargetErr::NoMatchInChannel { .. },
        ) => {
            if context.channel.is_none() {
                return Ok(None);
            }
            let unscoped = crate::cli::CurrentChannel::Unscoped.address_context();
            match rimz::address::resolve_many(&snapshot, reference, None, &unscoped) {
                Ok(matches) if matches.len() == 1 => matches,
                _ => return Ok(None),
            }
        }
        Err(err) => return Err(err.into()),
    };
    matches
        .first()
        .map(|agent| {
            store
                .load_full_agent(agent)
                .context("loading audit agent text")
        })
        .transpose()
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, serde::Serialize)]
struct ShowMessage {
    id: String,
    status: rimz::store::message::MessageStatus,
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
    let handle = agent
        .handle
        .split_once('#')
        .map_or(agent.handle.as_str(), |(handle, _)| handle);
    let identity = render::paint(render::palette::identity(agent.kind.as_str()), handle);
    let mut facts = vec![
        agent
            .login
            .as_ref()
            .map_or_else(|| agent.kind.to_string(), ToString::to_string),
    ];
    if let Some(profile) = &agent.profile {
        facts.push(profile.clone());
    }
    if let Some(model) = &agent.model.label {
        let mut model = model.clone();
        if agent.model.fast {
            let config = crate::cli::machine_config();
            let glyph = rimz::theme::theme_glyphs(&config.theme);
            model.push_str(&format!(
                " {}",
                render::paint(
                    render::palette::fast_mode(),
                    &glyph(rimz::config::GlyphRole::CardFast)
                )
            ));
        }
        if let Some(tier) = &agent.model.tier {
            let passes: String = tier.passes().map(|pass| format!("; {pass:#}")).collect();
            model.push_str(&format!(" ({}{passes})", tier.tier));
        }
        facts.push(model);
    }
    let address = if handle == agent.handle {
        String::new()
    } else {
        format!("  ({})", agent.handle)
    };
    writeln!(w, "{identity}  {}{address}", facts.join(" · "))
}

pub(super) fn render_activity_section(
    w: &mut impl Write,
    agent: &AgentReportEntry,
    ask_view: Option<&crate::cli::transcript::AskView>,
    next_step: Option<&str>,
    stale: bool,
    now: jiff::Timestamp,
) -> std::io::Result<()> {
    let mut state = render::paint(status_style(agent), agent.status.as_str());
    if agent.phase != rimz::agents::TurnPhase::Idle {
        state.push_str(&format!(" ({})", agent.phase.as_str()));
    }
    if agent.status == rimz::agents::AgentStatus::Running
        && let Some(started) = agent.timeline.turn_started_at
    {
        state.push_str(&format!(" · turn {}", duration_label(started, now)));
    }
    state.push_str(&format!(
        " · last seen {}",
        render::rel_age(agent.timeline.last_seen, now)
    ));
    if stale {
        state.push_str(" · stale");
    }
    writeln!(w, "{state}")?;
    if agent.status == rimz::agents::AgentStatus::Waiting {
        let summary = ask_view
            .map(crate::cli::transcript::ask_summary)
            .unwrap_or_else(|| {
                render::one_line(agent.ask.as_ref().map_or_else(
                    || agent.description.as_deref().unwrap_or("in the pane"),
                    |ask| {
                        ask.detail
                            .as_deref()
                            .unwrap_or_else(|| ask.kind.short_label())
                    },
                ))
            });
        let pane_step = agent
            .ask
            .is_none()
            .then(|| format!("rimz agents focus {}", agent.handle));
        let step = next_step
            .or(pane_step.as_deref())
            .map(|step| format!(" → {step}"))
            .unwrap_or_default();
        writeln!(w, "  waiting: {summary}{step}")?;
    }
    if matches!(
        agent.status,
        rimz::agents::AgentStatus::Paused | rimz::agents::AgentStatus::Failed
    ) && let Some(description) = &agent.description
    {
        writeln!(w, "  {}: {description}", agent.status.as_str())?;
    } else if !(agent.status == rimz::agents::AgentStatus::Waiting && agent.ask.is_none())
        && let Some(description) = &agent.description
    {
        writeln!(w, "{description}")?;
    }
    let mut kv = render::KeyVals::new().indent(2);
    if let Some(pending) = agent.idle_stop.as_ref() {
        let requester = pending
            .stop
            .requested_by
            .as_deref()
            .map(|by| format!(" ({by})"))
            .unwrap_or_default();
        kv.push(
            "idle_stop",
            render::cell(format!("{}{requester}", pending.label(now))),
        );
    }
    if let Some(hibernation) = &agent.hibernation {
        let final_step = crate::cli::machine_config()
            .agents
            .hibernate
            .schedule()
            .map(|schedule| schedule.final_step().to_string())
            .unwrap_or_else(|_| "?".into());
        let mut detail = format!(
            "hibernating since {}, step {} of {}, {} reclaimed",
            hibernation.frozen_at,
            hibernation.step,
            final_step,
            render::fmt_bytes(hibernation.reclaimed_bytes)
        );
        if let Some(error) = &hibernation.wake_error {
            detail.push_str(", wake failed: ");
            detail.push_str(error);
        }
        kv.push("hibernation", render::cell(detail));
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
    let fill = agent
        .context
        .fill_pct
        .map(|pct| format!("{pct}%"))
        .unwrap_or_else(|| "-".into());
    let cost = agent
        .stats
        .cost_usd
        .map(fmt_cost)
        .unwrap_or_else(|| "-".into());
    let active = agent
        .stats
        .active_secs
        .map(render::age_label)
        .unwrap_or_else(|| "-".into());
    writeln!(
        w,
        "  {fill} of {} · {} in window · {} spent ({} cache read) · {} · {active} active",
        opt_compact_count(agent.context.window),
        opt_compact_count(agent.context.used_tokens),
        opt_compact_count(agent.stats.total_tokens),
        opt_compact_count(agent.stats.cache_read_tokens),
        render::paint(render::palette::money(), &cost)
    )?;
    let mut kv = render::KeyVals::new().indent(2);
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

pub(super) fn render_identity_section(
    w: &mut impl Write,
    agent: &AgentReportEntry,
    tmp_dir: Option<&Path>,
) -> std::io::Result<()> {
    section(w, "Identity")?;
    let mut kv = render::KeyVals::new().indent(2);
    kv.push("session", render::cell(agent.id.to_string()));
    if let Some(registered) = agent.timeline.registered_at {
        kv.push("registered", render::cell(registered.to_string()));
    }
    if let Some(login) = &agent.login {
        kv.push(
            "account",
            render::cell(login.to_string()).fg(render::palette::meta()),
        );
    }
    for (label, value) in [
        ("role", &agent.role),
        ("team", &agent.team),
        ("name", &agent.name),
    ] {
        if let Some(value) = value {
            kv.push(label, render::cell(value));
        }
    }
    kv.push(
        "channel",
        render::cell(agent.placement.lane_label.as_deref().unwrap_or("-")).dash(),
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
    if let Some(tmp_dir) = tmp_dir {
        kv.push(
            "tmp",
            render::cell(format!("{} (/tmp in a scoped pane)", tmp_dir.display())),
        );
    }
    if !agent.launch_warnings.is_empty() {
        kv.push_lines(
            "launch_warnings",
            agent
                .launch_warnings
                .iter()
                .map(|warning| vec![render::cell(warning).fg(render::palette::warn())]),
        );
    }
    kv.render(w)
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
        rimz::store::snapshot::WorktreeCi::Pending => " · ci pending",
        rimz::store::snapshot::WorktreeCi::Passing => " · ci passing",
        rimz::store::snapshot::WorktreeCi::Failing => " · ci failing",
    });
    format!("{number}{state}{}", ci.unwrap_or_default())
}

fn render_run_section(
    w: &mut impl Write,
    run: &RunRecord,
    now: jiff::Timestamp,
) -> std::io::Result<()> {
    section(w, "Run")?;
    let status = render::paint(render::status::run(run.status), run.status.label());
    let summary = if run.status.is_terminal() {
        format!(
            "{status} · exit {} · {}",
            run.status.exit_code(),
            duration_label(run.started_at, run.completed_at.unwrap_or(run.updated_at))
        )
    } else {
        format!("{status} {}", duration_label(run.started_at, now))
    };
    let reader = run
        .reader
        .as_deref()
        .map(|reader| format!(" · for @{}", reader.trim_start_matches('@')))
        .unwrap_or_default();
    writeln!(w, "  {} · {summary}{reader}", run.run_id)?;
    if let Some(parked_at) = run.parked_at {
        writeln!(w, "  parked: {}", render::rel_age(parked_at, now))?;
    }
    writeln!(
        w,
        "  prompt: {}",
        preview(run.prompt.lines().next().unwrap_or_default())
    )?;
    if let Some(tail) = run.failure_tail.as_deref() {
        writeln!(
            w,
            "  failure: {}",
            render::paint(render::palette::alarm(), &preview(tail))
        )?;
    }
    writeln!(w)
}

fn render_messages_section(
    w: &mut impl Write,
    messages: &[ShowMessage],
    system_messages_hidden: usize,
    agent_id: &rimz::ids::AgentSessionId,
) -> std::io::Result<()> {
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
    if !messages.is_empty() {
        table.render(w)?;
    }
    if system_messages_hidden > 0 {
        writeln!(
            w,
            "{}",
            render::paint(
                render::palette::faint(),
                &format!(
                    "{system_messages_hidden} system messages hidden — rimz message list --all --system @{agent_id}"
                ),
            )
        )?;
    }
    writeln!(w)
}

fn opt_compact_count(value: Option<u64>) -> String {
    value
        .map(render::compact_count)
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
    let records = slot_lifetime_records(&refs, agent);
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
    let active = rimz::store::active_time::display_secs_for_keys(
        runtime,
        records
            .iter()
            .map(|record| (record.kind.as_str(), record.agent_id.as_str())),
        now,
        active_grace_secs,
    );
    let active_secs = rimz::agents::attribution::seat_active_secs(records, &active);
    Ok((effort, active_secs))
}

fn slot_lifetime_records<'a>(
    records: &[&'a AgentState],
    agent: &'a AgentState,
) -> Vec<&'a AgentState> {
    let launched_children = agent.is_launched_child().then(|| {
        records
            .iter()
            .copied()
            .filter(|record| record.is_launched_child())
            .collect::<Vec<_>>()
    });
    let candidates = launched_children.as_deref().unwrap_or(records);
    let lifetimes = rimz::worktree::lane_lifetimes(candidates.iter().copied());
    render::warn_unreadable_lanes(&lifetimes);
    super::report::slot_records_for_agent(
        &rimz::agents::attribution::slot_groups(candidates, &lifetimes),
        agent,
    )
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

fn show_messages(store: &rimz::Store, agent: &AgentState) -> Result<(Vec<ShowMessage>, usize)> {
    let now = jiff::Timestamp::now();
    let mut system_messages_hidden = 0;
    let mut rows: Vec<ShowMessage> = store
        .list_messages()?
        .into_iter()
        .filter(|message| message.same_agent_card(agent))
        .filter(|message| {
            if message.sender.is_conversation() {
                return true;
            }
            system_messages_hidden += 1;
            false
        })
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
        if payload.status != rimz::store::message::MessageStatus::Delivered
            || !rimz::agents::AgentCardRef::new(
                &payload.kind,
                &payload.agent_id,
                payload.agent_name.as_deref(),
            )
            .matches(agent.card_ref())
        {
            continue;
        }
        let sender = payload.sender.unwrap_or_default();
        if !sender.is_conversation() {
            system_messages_hidden += 1;
            continue;
        }
        delivered.push(ShowMessage {
            id: payload.message_id.to_string(),
            status: payload.status,
            from: sender.render(),
            age: render::rel_age(payload.enqueued_at.unwrap_or(event.timestamp), now),
            text: "-".to_owned(),
        });
        if delivered.len() >= 3 {
            break;
        }
    }
    delivered.reverse();
    rows.extend(delivered);
    Ok((rows, system_messages_hidden))
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
    let agent = crate::cli::resolve_agent_one(
        &ctx.store,
        &snapshot,
        &reference,
        None,
        &ctx.address_context(),
    )?;
    focus_resolved(&ctx, agent)?;
    writeln!(
        render::out(),
        "focused {}",
        rimz::address::paste_address(agent, &snapshot, &ctx.address_context())
    )?;
    Ok(())
}

pub(in crate::cli) fn focus_resolved(ctx: &Ctx, agent: &AgentState) -> Result<()> {
    let pane = agent
        .pane
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("agent {} has no bound pane", agent_name(agent)))?;
    let backend = rimz::mux::backend_for(pane.pane_id.mux());
    rimz::harness::hibernate::wake_pane(&ctx.store, &pane.pane_id)?;
    rimz::mux::focus_anchor::execute_action(
        backend.as_ref(),
        ctx.runtime(),
        &ctx.workspace.session_name,
        pane.pane_id.clone(),
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "show/tests.rs"]
mod tests;
