use super::*;

use super::report::{
    AgentReportEntry, PrInfo, ReportOverrides, build_entry, build_list_report, context_cell,
    row_for_agent, status_style,
};
use crate::cli::render;
use rimz::config::{GlyphRole, ThemeConfig};
use rimz::theme::theme_glyphs;

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
        runtime.clone(),
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
    let now = jiff::Timestamp::now();
    if json {
        return render::json_pretty(&build_list_report(&snapshot, &agents, now, Some(&runtime)));
    }

    let machine_config = crate::cli::machine_config();
    let mut out = render::out();
    render_agents_table(
        &mut out,
        &snapshot,
        &agents,
        now,
        render::terminal_columns(120),
        &machine_config.theme,
    )?;
    Ok(())
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

pub(super) fn pr_info(group: &rimz::SidebarWorktreeGroup) -> Option<PrInfo> {
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
    let identity = super::report::SelfIdentity::from_env();
    let me = identity.resolve(snapshot);
    let glyph = theme_glyphs(theme);
    let mut table = render::Table::new(["AGENT", "STATUS", "MODEL", "CTX", "TOKENS", "AGE"])
        .right(&[3, 4, 5])
        .max_width(max_width);
    for group in groups {
        table.section_cells(group_header_cells(&group, snapshot, &glyph));
        let pr = group_pr(snapshot, &group.key).and_then(pr_info);
        for &agent in &group.agents {
            let report = build_entry(
                agent,
                row_for_agent(snapshot, agent),
                pr,
                &ordered_agents,
                me.as_ref(),
                now,
                ReportOverrides::default(),
            );
            let detail = report
                .description
                .as_deref()
                .map(|line| render::cell(line).fg(render::palette::muted()));
            table.card(agent_row(&report, now), detail);
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
    if let Some(team) = group.team()
        && !group.label.ends_with(&format!("/{team}"))
    {
        cells.push(render::cell(format!("· {team} team")).fg(render::palette::meta()));
    }
    if let Some(pr) = group_pr(snapshot, &group.key) {
        if let Some(number) = pr.pr_number {
            cells.push(render::cell(format!("#{number}")).fg(render::palette::accent()));
        }
        if (pr.pr_state.is_none()
            || matches!(
                pr.pr_state,
                Some(rimz::WorktreePrState::Open | rimz::WorktreePrState::Merged)
            ))
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

fn agent_row(agent: &AgentReportEntry, now: jiff::Timestamp) -> Vec<render::Cell> {
    let model = agent.model.label.as_deref().unwrap_or("-");
    let model = if model == "-" {
        render::cell(model).dash()
    } else {
        render::cell(model).fg(render::palette::muted())
    };
    let tokens = agent
        .stats
        .total_tokens
        .map(render::compact_count)
        .unwrap_or_else(|| "-".to_owned());
    vec![
        render::cell(agent.handle.as_str()).fg(render::palette::identity(agent.kind.as_str())),
        render::cell(agent.status.as_str()).fg(status_style(agent)),
        model,
        context_cell(agent.context.fill_pct),
        render::cell(tokens).dash(),
        render::cell(render::age_short(agent.timeline.last_seen, now)),
    ]
}

#[cfg(test)]
mod tests {
    use super::super::report::model_report;
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
        assert_eq!(
            model_report(&agent).label.as_deref(),
            Some("Context Display@high")
        );

        agent.context.as_mut().unwrap().model_display_name = None;
        assert_eq!(
            model_report(&agent).label.as_deref(),
            Some("context-id@high")
        );

        agent.context.as_mut().unwrap().model_id = None;
        assert_eq!(
            model_report(&agent).label.as_deref(),
            Some("launch-model@high")
        );

        agent.context.as_mut().unwrap().effort = None;
        assert_eq!(
            model_report(&agent).label.as_deref(),
            Some("launch-model@launch-effort")
        );

        agent.context = None;
        assert_eq!(
            model_report(&agent).label.as_deref(),
            Some("launch-model@launch-effort")
        );
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
        let entries = build_list_report(&snapshot, &refs, now, None);
        let linked = entries
            .agents
            .iter()
            .find(|entry| entry.id.as_str() == "linked")
            .unwrap();
        let plain = entries
            .agents
            .iter()
            .find(|entry| entry.id.as_str() == "plain")
            .unwrap();

        assert_eq!(
            serde_json::to_value(linked).unwrap()["placement"]["pr"],
            serde_json::json!({"number": 91, "state": "open", "ci": "passing"})
        );
        assert_eq!(
            serde_json::to_value(plain).unwrap()["placement"]["pr"],
            serde_json::Value::Null
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
