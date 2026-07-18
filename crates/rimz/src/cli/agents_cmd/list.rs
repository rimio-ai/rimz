use super::*;

use crate::cli::render;
use rimz::config::{GlyphRole, ThemeConfig};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) struct PrInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    pub state: rimz::WorktreePrState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<rimz::WorktreePrCi>,
}

#[derive(serde::Serialize)]
struct AgentListEntry<'a> {
    #[serde(flatten)]
    agent: &'a AgentState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pr: Option<PrInfo>,
}

pub(super) fn list_agents(
    json: bool,
    all: bool,
    scope: Option<String>,
    globals: &GlobalFlags,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())?;
    let _mux = rimz::room::require_live_mux(globals.mux, &workspace)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let state = rimz::StatePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing state paths")?;
    let snapshot = rimz::sidebar::consumer::PublishedSnapshotReader::new(
        runtime,
        workspace.session_name.clone(),
        None,
    )
    .read(&state)
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
        return render::json_pretty(&agent_list_entries(&snapshot, &agents));
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

fn agent_list_entries<'a>(
    snapshot: &rimz::SidebarSnapshot,
    agents: &[&'a AgentState],
) -> Vec<AgentListEntry<'a>> {
    rimz::store::snapshot::group_live_agents_by_worktree(agents, snapshot)
        .into_iter()
        .flat_map(|group| {
            let pr = group_pr(snapshot, &group.key).and_then(pr_info);
            group
                .agents
                .into_iter()
                .map(move |agent| AgentListEntry { agent, pr })
        })
        .collect()
}

pub(super) fn agent_pr(snapshot: &rimz::SidebarSnapshot, agent: &AgentState) -> Option<PrInfo> {
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
    snapshot: &'a rimz::SidebarSnapshot,
    key: &str,
) -> Option<&'a rimz::SidebarWorktreeGroup> {
    snapshot
        .worktree_groups
        .iter()
        .find(|group| group.key == key)
}

fn pr_info(group: &rimz::SidebarWorktreeGroup) -> Option<PrInfo> {
    let state = group.pr_state?;
    Some(PrInfo {
        number: group.pr_number,
        state,
        ci: (state == rimz::WorktreePrState::Open)
            .then_some(group.pr_ci)
            .flatten(),
    })
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
    let mut table = render::Table::new(["AGENT", "STATUS", "MODEL", "CTX", "TOKENS", "AGE"])
        .right(&[3, 4, 5])
        .max_width(max_width);
    for group in groups {
        table.section_cells(group_header_cells(&group, snapshot, &glyph));
        for &agent in &group.agents {
            let detail = agent
                .activity_line()
                .map(|line| render::cell(line).fg(render::palette::muted()));
            table.card(agent_row(agent, &ordered_agents, now), detail);
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
        return vec![render::cell("external").fg(render::palette::faint())];
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
    let mut cells = vec![render::cell(label).fg(render::palette::header())];
    if let Some(team) = shared_group_team(group)
        && !group.label.ends_with(&format!("/{team}"))
    {
        cells.push(render::cell(format!("· {team} team")).fg(render::palette::meta()));
    }
    if let Some(pr) = group_pr(snapshot, &group.key)
        && let Some(number) = pr.pr_number
    {
        cells.push(render::cell(format!("#{number}")).fg(render::palette::accent()));
        if pr.pr_state == Some(rimz::WorktreePrState::Open)
            && let Some(ci) = pr.pr_ci
        {
            let (role, style) = match ci {
                rimz::WorktreePrCi::Passing => {
                    (GlyphRole::WorktreeCiPassing, render::palette::good())
                }
                rimz::WorktreePrCi::Pending => {
                    (GlyphRole::WorktreeCiPending, render::palette::warn())
                }
                rimz::WorktreePrCi::Failing => {
                    (GlyphRole::WorktreeCiFailing, render::palette::alarm())
                }
            };
            cells.push(render::cell(glyph(role)).fg(style));
        }
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

fn agent_row(agent: &AgentState, peers: &[&AgentState], now: jiff::Timestamp) -> Vec<render::Cell> {
    vec![
        render::cell(rimz::harness::target::agent_handle(agent, peers, false))
            .fg(render::palette::identity(agent.kind.as_str())),
        render::cell(agent_status_label(agent)).fg(agent_status_style(agent)),
        model_cell(agent),
        context_cell(agent),
        render::cell(tokens_label(agent)).dash(),
        render::cell(render::age_short(agent.last_seen, now)),
    ]
}

/// Context fill warms as it climbs: gold past 75%, rose past 90%.
pub(super) fn context_cell(agent: &AgentState) -> render::Cell {
    let pct = agent.context_fill_pct();
    let text = pct
        .map(|pct| format!("{}%", pct.round() as u8))
        .unwrap_or_else(|| "-".to_owned());
    let c = render::cell(text);
    match pct {
        Some(pct) if pct >= 90.0 => c.fg(render::palette::alarm()),
        Some(pct) if pct >= 75.0 => c.fg(render::palette::warn()),
        Some(_) => c,
        None => c.dash(),
    }
}

pub(super) fn agent_status_label(agent: &AgentState) -> &'static str {
    agent_status_projection(agent).0.as_str()
}

pub(super) fn agent_status_style(agent: &AgentState) -> anstyle::Style {
    let (status, phase) = agent_status_projection(agent);
    render::status::agent(status, phase)
}

pub(super) fn agent_status_projection(
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
        None => {
            let status = agent.effective_status();
            let phase = if status == rimz::agents::AgentStatus::Running {
                agent.phase
            } else {
                rimz::agents::TurnPhase::Idle
            };
            (status, phase)
        }
    }
}

pub(super) fn model_label(agent: &AgentState) -> String {
    let context = agent.context.as_ref();
    let model = context
        .and_then(|context| context.model_display_name.as_deref())
        .or_else(|| context.and_then(|context| context.model_id.as_deref()))
        .or(agent.model.as_deref());
    let effort = context
        .and_then(|context| context.effort.as_deref())
        .or(agent.effort.as_deref());
    match (model, effort) {
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
    render::cell(label).fg(render::palette::muted())
}

fn tokens_label(agent: &AgentState) -> String {
    agent
        .total_tokens
        .map(render::compact_count)
        .unwrap_or_else(|| "-".to_owned())
}

/// The agent's channel for display, dashed when it runs outside any worktree.
/// The channel itself comes from [`rimz::harness::target::agent_channel`], the single
/// source of truth; this only chooses the `-` placeholder over the resolver's
/// prose label.
pub(super) fn worktree_label(agent: &AgentState) -> String {
    rimz::harness::target::agent_channel(agent).unwrap_or_else(|| "-".to_owned())
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
    fn model_label_prefers_live_context_then_durable_launch_fields() {
        let mut agent = test_agent("model-precedence");
        agent.model = Some("launch-model".to_owned());
        agent.effort = Some("launch-effort".to_owned());
        let mut context = rimz::agents::AgentContext::new("copilot", jiff::Timestamp::UNIX_EPOCH);
        context.model_id = Some("context-id".to_owned());
        context.model_display_name = Some("Context Display".to_owned());
        context.effort = Some("high".to_owned());
        agent.context = Some(context);
        assert_eq!(model_label(&agent), "Context Display@high");

        agent.context.as_mut().unwrap().model_display_name = None;
        assert_eq!(model_label(&agent), "context-id@high");

        agent.context.as_mut().unwrap().model_id = None;
        assert_eq!(model_label(&agent), "launch-model@high");

        agent.context.as_mut().unwrap().effort = None;
        assert_eq!(model_label(&agent), "launch-model@launch-effort");

        agent.context = None;
        assert_eq!(model_label(&agent), "launch-model@launch-effort");
    }

    #[test]
    fn json_entries_add_pr_only_to_agents_in_the_linked_group() {
        let now = jiff::Timestamp::UNIX_EPOCH;
        let mut linked = test_agent("linked");
        linked.channel = Some("feature".to_owned());
        linked.worktree_path = Some("/repo/worktrees/feature".to_owned());
        linked.worktree_branch = Some("feature".to_owned());
        let mut plain = test_agent("plain");
        plain.channel = Some("docs".to_owned());
        plain.worktree_path = Some("/repo/main".to_owned());
        plain.worktree_branch = Some("main".to_owned());
        let mut snapshot = rimz::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::from_project_root(Path::new("/repo/main")),
            vec![linked, plain],
            now,
        )
        .with_project_root(Some(PathBuf::from("/repo/main")));
        let refs = snapshot.agents.iter().collect::<Vec<_>>();
        let (linked_key, linked_label, linked_kind) = {
            let group = rimz::store::snapshot::group_live_agents_by_worktree(&refs, &snapshot)
                .into_iter()
                .find(|group| group.label == "feature")
                .unwrap();
            (group.key, group.label, group.kind)
        };
        snapshot.worktree_groups.push(
            serde_json::from_value(serde_json::json!({
                "key": linked_key,
                "label": linked_label,
                "kind": linked_kind,
                "status_counts": [],
                "rows": [],
                "pr_number": 91,
                "pr_state": "open",
                "pr_ci": "passing"
            }))
            .unwrap(),
        );

        let refs = snapshot.agents.iter().collect::<Vec<_>>();
        let entries = agent_list_entries(&snapshot, &refs);
        let linked = entries
            .iter()
            .find(|entry| entry.agent.agent_id.as_str() == "linked")
            .unwrap();
        let plain = entries
            .iter()
            .find(|entry| entry.agent.agent_id.as_str() == "plain")
            .unwrap();

        assert_eq!(
            serde_json::to_value(linked).unwrap()["pr"],
            serde_json::json!({"number": 91, "state": "open", "ci": "passing"})
        );
        assert_eq!(
            serde_json::to_value(plain).unwrap(),
            serde_json::to_value(plain.agent).unwrap()
        );
    }

    #[test]
    fn agent_pr_uses_projected_group_membership() {
        let mut linked = test_agent("linked");
        linked.worktree_path = Some("/repo/worktree".to_owned());
        linked.worktree_branch = Some("feature".to_owned());
        let mut stale = test_agent("stale");
        stale.worktree_path = Some("/repo/worktree".to_owned());
        stale.worktree_branch = Some("other".to_owned());
        let mut snapshot = rimz::SidebarSnapshot::build_with_agents(
            rimz::WorkspaceId::from_project_root(Path::new("/repo/main")),
            vec![linked, stale],
            jiff::Timestamp::UNIX_EPOCH,
        );
        let mut group: rimz::SidebarWorktreeGroup = serde_json::from_value(serde_json::json!({
            "key": "/repo/worktree",
            "label": "feature",
            "kind": "worktree",
            "status_counts": [],
            "rows": [],
            "pr_number": 91,
            "pr_state": "open",
            "pr_ci": "passing"
        }))
        .unwrap();
        group.rows.push(rimz::SidebarRow {
            id: "linked".to_owned(),
            name: "codex".to_owned(),
            pane: None,
            worktree_path: Some("/repo/worktree".to_owned()),
            worktree_branch: Some("feature".to_owned()),
            channel: None,
            unread: false,
            inactive: false,
            archived: false,
            attention_score: 0,
            last_activity: jiff::Timestamp::UNIX_EPOCH,
            card: rimz::RowCard::Agent(Box::new(rimz::AgentCard {
                status: rimz::agents::AgentStatus::Idle,
                ..rimz::AgentCard::default()
            })),
        });
        snapshot.worktree_groups.push(group);

        assert_eq!(
            agent_pr(&snapshot, &snapshot.agents[0]),
            Some(PrInfo {
                number: Some(91),
                state: rimz::WorktreePrState::Open,
                ci: Some(rimz::WorktreePrCi::Passing),
            })
        );
        assert_eq!(agent_pr(&snapshot, &snapshot.agents[1]), None);
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
