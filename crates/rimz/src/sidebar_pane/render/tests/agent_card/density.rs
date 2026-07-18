use super::*;

use crate::config::CardDensityMode;

#[test]
fn compact_density_trims_resting_cards_by_status() {
    let mut selected = density_agent(
        "selected-1",
        "selector",
        AgentStatus::Idle,
        Some("selected other"),
        0,
    );
    selected.worktree_path = Some("/repo/other".to_owned());
    selected.worktree_branch = Some("other".to_owned());

    let mut snapshot = snapshot_with(vec![
        density_agent("idle-1", "idlebot", AgentStatus::Idle, Some("idle task"), 0),
        density_agent(
            "run-1",
            "runner",
            AgentStatus::Running,
            Some("running task"),
            38,
        ),
        density_agent(
            "wait-1",
            "waiter",
            AgentStatus::Waiting,
            Some("waiting task"),
            42,
        ),
        density_agent(
            "paused-1",
            "paused",
            AgentStatus::Paused,
            Some("paused task"),
            44,
        ),
        density_agent(
            "done-1",
            "done",
            AgentStatus::Success,
            Some("done task"),
            46,
        ),
        density_agent(
            "fail-1",
            "failed",
            AgentStatus::Failed,
            Some("failed task"),
            48,
        ),
        selected,
    ]);
    snapshot.theme.display.card_density = CardDensityMode::Compact;

    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 6,
            ..Default::default()
        },
        54,
        31,
    );

    assert!(
        !rendered.contains("idle task"),
        "idle resting cards collapse to identity only:\n{rendered}"
    );
    for task in [
        "running task",
        "waiting task",
        "paused task",
        "done task",
        "failed task",
    ] {
        assert!(rendered.contains(task), "{task} stays visible:\n{rendered}");
    }
    assert_eq!(
        rendered.matches('▣').count(),
        2,
        "only running and waiting resting cards keep the context bar:\n{rendered}"
    );
    assert!(
        !rendered.contains('▤'),
        "compact resting cards drop token-stat rows:\n{rendered}"
    );
    assert_snapshot("card_density_compact_resting_statuses", rendered);
}

#[test]
fn compact_density_running_waiting_render_placeholder_meter() {
    let mut running = density_agent(
        "run-1",
        "runner",
        AgentStatus::Running,
        Some("running task"),
        0,
    );
    running.usage.context_pct = None;

    let mut waiting = density_agent(
        "wait-1",
        "waiter",
        AgentStatus::Waiting,
        Some("waiting task"),
        0,
    );
    waiting.usage.context_pct = None;

    let mut selected = density_agent(
        "selected-1",
        "selector",
        AgentStatus::Idle,
        Some("selected other"),
        1,
    );
    selected.worktree_path = Some("/repo/other".to_owned());
    selected.worktree_branch = Some("other".to_owned());

    let mut snapshot = snapshot_with(vec![running, waiting, selected]);
    snapshot.theme.display.card_density = CardDensityMode::Compact;

    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 2,
            ..Default::default()
        },
        54,
        25,
    );

    assert!(
        rendered.contains("running task"),
        "running task visible:\n{rendered}"
    );
    assert!(
        rendered.contains("waiting task"),
        "waiting task visible:\n{rendered}"
    );
    assert_eq!(
        rendered.matches('▢').count(),
        2,
        "running and waiting cards hold their meter line before context arrives:\n{rendered}"
    );
    assert_eq!(
        rendered.matches('▤').count(),
        1,
        "compact resting cards still drop token-stat rows while the selection opens fully:\n{rendered}"
    );
}

#[test]
fn compact_density_selected_card_opens_to_full_form() {
    let mut parent = density_agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("db migrate"),
        38,
    );
    parent.context = Some(claude_context(fixed_now()));

    let mut child = density_agent(
        "child-1",
        "claude",
        AgentStatus::Running,
        Some("Explore"),
        12,
    );
    child.parent_agent_id = Some("claude-1".into());
    child.subagent_description = Some("trace compaction".to_owned());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(120));

    let mut snapshot = snapshot_with(vec![parent, child]);
    snapshot.theme.display.card_density = CardDensityMode::Compact;

    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            ..Default::default()
        },
        54,
        21,
    );

    assert!(
        rendered.contains("▤ 76k · ◌ 68k ◍ 6k ↘ 1k ↗ 2k"),
        "the selected compact card restores the token line:\n{rendered}"
    );
    assert!(
        rendered.contains("⧉ subagents (1)"),
        "the selected compact card restores subagents:\n{rendered}"
    );
    assert_snapshot("card_density_compact_selected_full", rendered);
}

#[test]
fn expanded_density_shows_subagents_on_non_selected_cards() {
    let selected = density_agent(
        "question-1",
        "asker",
        AgentStatus::Waiting,
        Some("needs review"),
        30,
    );
    let mut parent = density_agent(
        "claude-1",
        "claude",
        AgentStatus::Idle,
        Some("delegated sweep"),
        0,
    );
    parent.prompt = Some("delegated sweep".to_owned());
    let mut child = density_agent(
        "child-1",
        "claude",
        AgentStatus::Success,
        Some("Explore"),
        10,
    );
    child.parent_agent_id = Some("claude-1".into());
    child.subagent_description = Some("map the render path".to_owned());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(180));
    child.last_activity = fixed_now() - Duration::from_secs(60);

    let mut snapshot = snapshot_with(vec![parent, selected, child]);
    snapshot.theme.display.card_density = CardDensityMode::Expanded;

    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            ..Default::default()
        },
        54,
        23,
    );

    let subagent_line = rendered
        .lines()
        .find(|line| line.contains("⧉ subagents (1)"))
        .expect("expanded mode should render the parent's subagent section");
    assert!(
        subagent_line.contains("▎  ⧉ subagents (1)"),
        "expanded mode opens the non-selected parent's subagents:\n{rendered}"
    );
    assert_snapshot("card_density_expanded_non_selected_subagents", rendered);
}

fn density_agent(
    id: &str,
    kind: &str,
    status: AgentStatus,
    task: Option<&str>,
    context_pct: u8,
) -> crate::agents::AgentState {
    let mut agent = agent(id, kind, status, Some("/repo/main"), Some("main"), task);
    agent.model = Some("opus".to_owned());
    agent.effort = Some("high".to_owned());
    agent.usage.context_pct = Some(context_pct);
    agent.usage.context_window = Some(200_000);
    if context_pct > 0 {
        agent.usage.cache_read_input_tokens = Some(24_000);
        agent.usage.fresh_input_tokens = Some(6_000);
        agent.usage.output_tokens = Some(1_000);
        agent.usage.total_tokens = Some(31_000);
    }
    agent
}
