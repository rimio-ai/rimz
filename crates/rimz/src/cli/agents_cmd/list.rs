use super::*;

use super::report::{
    AgentListJson, AgentReportEntry, PrInfo, build_list_report, context_cell,
    slot_records_for_agent, status_style,
};
#[cfg(test)]
use super::report::{ReportOverrides, build_entry};
use crate::cli::render;
use crate::cli::select::FILTER_DEFAULTS;
#[cfg(test)]
use rimz::address::selector::Field;
use rimz::address::selector::{self, AgentRow, LaneScope, Query};
use rimz::config::{GlyphRole, ThemeConfig};
#[cfg(test)]
use rimz::store::snapshot::{AgentCard, PaneAgent, RowCard, SidebarRow};
use rimz::store::snapshot::{
    AgentWorktreeGroup, SidebarSnapshot, SidebarWorktreeGroup, SidebarWorktreeKind, WorktreeCi,
    WorktreePrState, group_live_agents_by_worktree,
};
use rimz::theme::theme_glyphs;

pub(super) fn list_agents(
    json: bool,
    quiet: bool,
    all: bool,
    scope: Option<String>,
    filters: &[String],
    globals: &GlobalFlags,
) -> Result<()> {
    if crate::cli::select::is_filter_help(filters) {
        return Ok(render::filter_help(&mut render::out(), FILTER_DEFAULTS)?);
    }
    let query = Query::parse(filters).map_err(|err| UsageError::new(err.to_string()))?;
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let store = crate::cli::require_existing_store(&workspace)?;
    let _mux = rimz::room::require_live_mux(globals.mux, &workspace)?;
    let runtime = rimz::RuntimePaths::for_project_root(&workspace.project_root)
        .context("preparing runtime paths")?;
    let state = rimz::StatePaths::for_project_root(&workspace.project_root)
        .context("preparing state paths")?;
    let snapshot = rimz::sidebar::consumer::PublishedSnapshotReader::new(
        runtime.clone(),
        workspace.session_name.clone(),
        None,
    )
    .read(&state)
    .context("reading the room snapshot")?;

    let channel = list_channel_filter(all, scope.as_deref(), &workspace);
    let lane_scope = match channel {
        None => LaneScope::Room,
        Some(lane) if scope.is_some() => LaneScope::Named(lane),
        Some(lane) => LaneScope::Current(lane),
    };
    let channel = lane_scope.clone().lane(&query);
    let reader = crate::cli::current_channel(&workspace, Some(&store)).address_context();
    let now = jiff::Timestamp::now();
    let machine_config = crate::cli::machine_config();
    let demotion = crate::cli::select::park_demotion(&store, &state, &runtime, &snapshot, now);
    let audit = if json || query.reads_closed() {
        store
            .runtime_projection(rimz::RuntimeScope::Audit)
            .context("reading audit agent rollup")?
            .agents
    } else {
        Vec::new()
    };
    let audit_snapshot = query
        .reads_closed()
        .then(|| crate::cli::select::closed_snapshot(&workspace, &runtime, audit.clone(), now));
    let lane = lane_scope.map(|lane| rimz::address::LaneSelector::parse(Some(&lane)));
    let rows = selector::rows_in_scope(
        &snapshot,
        audit_snapshot.as_ref(),
        lane.as_ref(),
        &query,
        &demotion,
        &reader,
        now,
    );
    let agents: Vec<&AgentState> = rows.iter().map(AgentRow::agent).collect();
    let row_refs: Vec<&AgentRow<'_>> = rows.iter().collect();
    if agents.is_empty() {
        if let Some(scope) = scope.as_deref() {
            refuse_unknown_lane(&store, &workspace, scope)?;
        }
        if quiet {
            return Ok(());
        }
        if !json {
            let line = empty_list_line(channel.as_deref(), filters);
            writeln!(render::out(), "{line}")?;
            return Ok(());
        }
    }
    if quiet {
        let mut out = render::out();
        for agent in group_live_agents_by_worktree(&agents, &snapshot)
            .into_iter()
            .flat_map(|group| group.agents)
        {
            if let Some(row) = rows.iter().find(|row| std::ptr::eq(row.agent(), agent)) {
                writeln!(out, "{}", row.handle())?;
            }
        }
        return Ok(());
    }
    if json {
        let refs = audit.iter().collect::<Vec<_>>();
        let lifetimes = rimz::worktree::lane_lifetimes(refs.iter().copied());
        render::warn_unreadable_lanes(&lifetimes);
        let slots = rimz::agents::attribution::slot_groups(&refs, &lifetimes);
        let active_secs = rimz::store::active_time::display_secs_for_keys(
            &runtime,
            agents
                .iter()
                .flat_map(|agent| slot_records_for_agent(&slots, agent))
                .map(|agent| (agent.kind.as_str(), agent.agent_id.as_str())),
            now,
            crate::cli::machine_config()
                .agents
                .attention
                .active_grace_secs
                .get(),
        );
        let report = build_list_report(
            &demotion,
            &snapshot,
            &row_refs,
            now,
            Some(&runtime),
            &slots,
            &active_secs,
        );
        return render::json_pretty(&AgentListJson::new(&report)?);
    }

    let mut out = render::out();
    let report = build_list_report(
        &demotion,
        &snapshot,
        &row_refs,
        now,
        None,
        &[],
        &Default::default(),
    );
    let scope = channel
        .as_ref()
        .map_or_else(|| "whole room".to_owned(), |lane| format!("#{lane}"));
    let live = report
        .agents
        .iter()
        .zip(report.rows.iter().map(|row| row.live()))
        .collect::<Vec<_>>();
    render_list_summary(
        &mut out,
        &live,
        group_live_agents_by_worktree(&agents, &snapshot).len(),
        &scope,
        now,
    )?;
    render_projected_agents_table(
        &mut out,
        &snapshot,
        &agents,
        &report.agents,
        now,
        render::terminal_columns(120),
        &machine_config.theme,
    )?;
    Ok(())
}

/// The line an empty list prints: a filter's miss, or a lane with no live agent.
fn empty_list_line(lane: Option<&str>, filters: &[String]) -> String {
    match lane {
        // A filter that matches nothing is no sign the lane is closed.
        lane if !filters.is_empty() => crate::cli::select::no_match_line(lane),
        Some(lane) => {
            let lane = format!("#{lane}");
            format!(
                "no live agents in '{lane}'; reopen closed ones with {}",
                shell_command(["rimz", "agents", "resume", &lane])
            )
        }
        None => "no live agents in this room".to_owned(),
    }
}

pub(super) fn agent_pr(snapshot: &SidebarSnapshot, agent: &AgentState) -> Option<PrInfo> {
    snapshot
        .worktree_groups
        .iter()
        .find(|group| {
            group
                .rows
                .iter()
                .any(|row| row.is_agent() && row.id == agent.agent_id.as_str())
        })
        .and_then(pr_info)
}

pub(super) fn group_pr<'a>(
    snapshot: &'a SidebarSnapshot,
    key: &str,
) -> Option<&'a SidebarWorktreeGroup> {
    snapshot
        .worktree_groups
        .iter()
        .find(|group| group.key == key)
}

pub(super) fn pr_info(group: &SidebarWorktreeGroup) -> Option<PrInfo> {
    let state = group.pr_state?;
    Some(PrInfo {
        number: group.pr_number,
        state,
        ci: (state == WorktreePrState::Open)
            .then_some(group.ci)
            .flatten(),
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the reader's address scope is independent of the listed rows"
)]
pub(crate) fn render_agents_table(
    demotion: &rimz::agents::ParkDemotion,
    w: &mut impl std::io::Write,
    snapshot: &SidebarSnapshot,
    agents: &[&AgentState],
    now: jiff::Timestamp,
    max_width: usize,
    theme: &ThemeConfig,
    reader: &rimz::address::AddressContext,
) -> std::io::Result<()> {
    let rows = agents
        .iter()
        .map(|agent| AgentRow::of(agent, snapshot, demotion, reader, now))
        .collect::<Vec<_>>();
    let rows = rows.iter().collect::<Vec<_>>();
    let report = build_list_report(
        demotion,
        snapshot,
        &rows,
        now,
        None,
        &[],
        &Default::default(),
    );
    render_projected_agents_table(w, snapshot, agents, &report.agents, now, max_width, theme)
}

fn render_projected_agents_table(
    w: &mut impl std::io::Write,
    snapshot: &SidebarSnapshot,
    agents: &[&AgentState],
    grouped_entries: &[AgentReportEntry],
    now: jiff::Timestamp,
    max_width: usize,
    theme: &ThemeConfig,
) -> std::io::Result<()> {
    let groups = group_live_agents_by_worktree(agents, snapshot);
    let mut entries = grouped_entries.iter();
    let glyph = theme_glyphs(theme);
    let mut table = render::Table::new([
        "HANDLE", "PROFILE", "AGENT", "STATUS", "MODEL", "CTX", "TOKENS", "AGE",
    ])
    .right(&[5, 6, 7])
    .max_width(max_width);
    for group in groups {
        table.section_cells(group_header_cells(&group, snapshot, &glyph));
        for report in entries.by_ref().take(group.agents.len()) {
            let detail = Some(
                render::cell(report.description.as_deref().unwrap_or("-"))
                    .fg(render::palette::muted()),
            );
            table.card(agent_row(report, now, &glyph), detail);
        }
    }
    table.render(w)
}

fn group_header_cells(
    group: &AgentWorktreeGroup<'_>,
    snapshot: &SidebarSnapshot,
    glyph: &impl Fn(GlyphRole) -> String,
) -> Vec<render::Cell> {
    if group.kind == SidebarWorktreeKind::External {
        return vec![render::cell("external").fg(render::palette::faint())];
    }

    let label = match group.kind {
        SidebarWorktreeKind::Worktree => {
            format!("{} {}", glyph(GlyphRole::WorktreeBranch), group.label)
        }
        SidebarWorktreeKind::Channel if group.place.is_some() => {
            format!("{} {}", glyph(GlyphRole::WorktreeBranch), group.label)
        }
        SidebarWorktreeKind::Channel => {
            format!("{} {}", glyph(GlyphRole::ChannelHash), group.label)
        }
        SidebarWorktreeKind::Root => group.label.clone(),
        SidebarWorktreeKind::External => {
            unreachable!("external returned above")
        }
    };
    let mut cells = vec![render::cell(label).fg(render::palette::header())];
    if let Some(branch) = group.header_branch() {
        cells.push(render::cell(format!("· {branch}")).fg(render::palette::meta()));
    }
    // The published qualifier deepens colliding labels; a snapshot that skipped
    // enrichment (the message-miss table) still names a foreign repo's folder,
    // by the rule enrichment applies.
    let qualifier = group_pr(snapshot, &group.key)
        .and_then(|published| published.label_qualifier.clone())
        .or_else(|| group.place.as_ref()?.foreign_qualifier(&group.label));
    if let Some(qualifier) = qualifier {
        cells.push(render::cell(format!("· {qualifier}")).fg(render::palette::meta()));
    }
    let group_lane = rimz::address::LaneSelector::parse(Some(&group.label));
    if !group
        .agents
        .iter()
        .all(|agent| rimz::address::agent_in_lane(agent, &group_lane))
        && let Some(lane) = group
            .agents
            .first()
            .and_then(|agent| rimz::address::lane_label(agent))
    {
        cells.push(render::cell(format!("· #{lane}")).fg(render::palette::meta()));
    }
    // An in-place team's lane is named after the team, so its header names it once.
    if let Some(team) = group.team()
        && group.label != rimz::harness::lanes::team_lane(team).as_str()
    {
        cells.push(render::cell(format!("· {team} team")).fg(render::palette::meta()));
    }
    if let Some(pr) = group_pr(snapshot, &group.key) {
        if let Some(number) = pr.pr_number {
            cells.push(render::cell(format!("#{number}")).fg(render::palette::accent()));
        }
        // The projection decides which lanes carry CI at all, so draw what it gives.
        if let Some(ci) = pr.ci {
            let (role, style) = match ci {
                WorktreeCi::Passing => (GlyphRole::WorktreeCiPassing, render::palette::good()),
                WorktreeCi::Pending => (GlyphRole::WorktreeCiPending, render::palette::warn()),
                WorktreeCi::Failing => (GlyphRole::WorktreeCiFailing, render::palette::alarm()),
            };
            cells.push(render::cell(glyph(role)).fg(style));
        }
    }
    cells
}

fn render_list_summary(
    w: &mut impl std::io::Write,
    entries: &[(&AgentReportEntry, bool)],
    lanes: usize,
    scope: &str,
    now: jiff::Timestamp,
) -> std::io::Result<()> {
    let attention = entries
        .iter()
        .filter(|(entry, _)| entry.needs_you.is_some())
        .count();
    let awake = |status| {
        entries
            .iter()
            .filter(|(entry, live)| *live && entry.needs_you.is_none() && entry.status == status)
            .count()
    };
    let running = awake(rimz::agents::AgentStatus::Running);
    let sleeping = awake(rimz::agents::AgentStatus::Sleeping);
    let at_rest = entries.len() - attention - running - sleeping;
    let mut summary = vec![
        format!(
            "{} agent{}",
            entries.len(),
            if entries.len() == 1 { "" } else { "s" }
        ),
        format!("{lanes} lane{}", if lanes == 1 { "" } else { "s" }),
    ];
    for (count, label) in [
        (attention, "needs you"),
        (running, "running"),
        (sleeping, "sleeping"),
        (at_rest, "at rest"),
    ] {
        if count > 0 {
            summary.push(format!("{count} {label}"));
        }
    }
    summary.push(scope.to_owned());
    writeln!(w, "{}", summary.join(" · "))?;
    let rows = entries
        .iter()
        .filter_map(|(entry, _)| {
            let attention = entry.needs_you.as_ref()?;
            Some(render::NeedsYouRow {
                handle: entry.handle.clone(),
                kind: Some(entry.kind.to_string()),
                status: Some(attention.status),
                since: attention.since,
                reason: attention.reason.clone(),
            })
        })
        .collect::<Vec<_>>();
    render::needs_you(w, &rows, now)
}

/// A named scope that selects no live agent: fail when it is no lane at all,
/// by the rule `resume` resolves lanes with, naming the lanes there are.
pub(super) fn refuse_unknown_lane(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    scope: &str,
) -> Result<()> {
    let audit = store
        .runtime_projection(rimz::RuntimeScope::Audit)
        .context("reading audit agent rollup")?
        .agents;
    let worktrees = super::resume::local_worktrees(workspace)?;
    let Err(lanes) = rimz::harness::resume::check_lane_scope(scope, &audit, &worktrees) else {
        return Ok(());
    };
    let lane = format!("#{}", scope.trim_start_matches('#'));
    bail!(
        "no lane '{lane}' in this workspace{}",
        known_lanes_suffix(&lanes)
    )
}

pub(super) fn known_lanes_suffix(lanes: &[String]) -> String {
    const SHOWN_LANES: usize = 8;
    if lanes.is_empty() {
        return String::new();
    }
    let mut shown = lanes.iter().take(SHOWN_LANES).cloned().collect::<Vec<_>>();
    if lanes.len() > SHOWN_LANES {
        shown.push("…".to_owned());
    }
    format!("; lanes: {}", shown.join(", "))
}

pub(super) fn list_channel_filter(
    all: bool,
    worktree: Option<&str>,
    workspace: &rimz::ResolvedWorkspace,
) -> Option<String> {
    let store = crate::cli::open_existing_store(workspace).ok().flatten();
    list_channel_filter_for_current(
        all,
        worktree,
        crate::cli::current_channel(workspace, store.as_ref())
            .into_name()
            .map(String::from),
    )
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

fn agent_row(
    agent: &AgentReportEntry,
    now: jiff::Timestamp,
    glyph: &impl Fn(GlyphRole) -> String,
) -> Vec<render::Cell> {
    let model = agent.model.label.as_deref().unwrap_or("-");
    let model = if model == "-" {
        render::cell(model).dash()
    } else if agent.model.fast {
        render::cell(model)
            .fg(render::palette::muted())
            .suffix(glyph(GlyphRole::CardFast), render::palette::fast_mode())
    } else {
        render::cell(model).fg(render::palette::muted())
    };
    let tokens = agent
        .context
        .used_tokens
        .map(render::compact_count)
        .unwrap_or_else(|| "-".to_owned());
    vec![
        render::cell(agent.handle.as_str()).fg(render::palette::identity(agent.kind.as_str())),
        render::cell(agent.profile.as_deref().unwrap_or("-")).dash(),
        render::cell(
            agent
                .login
                .as_ref()
                .map_or_else(|| agent.kind.to_string(), ToString::to_string),
        ),
        render::cell(agent.status.as_str()).fg(status_style(agent)),
        model,
        context_cell(agent.context.fill_pct),
        render::cell(tokens).dash(),
        render::cell(render::age_short(agent.timeline.idle_since, now)),
    ]
}

#[cfg(test)]
mod tests;
