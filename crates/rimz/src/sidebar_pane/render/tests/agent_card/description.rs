use super::*;
use ratatui::text::Span;

#[test]
fn line_one_prefers_session_name_over_task() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    claude.context = Some(claude_context(fixed_now()));
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
    let rendered = snapshot_to_screen(&snapshot, 44, 12);

    assert!(rendered.contains("ledger refactor"));
    assert!(!rendered.contains("db migrate"));
}
/// An unnamed session whose turn has ended (the activity-bound `task` cleared)
/// keeps its latest prompt on line two instead of falling to an em dash, until
/// a real session name exists.
#[test]
fn line_two_falls_back_to_the_latest_prompt_when_unnamed() {
    let mut claude = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        None, // idle cleared the task; no session name (no context)
    );
    claude.prompt = Some("wire the bridge".to_owned());
    let snapshot = snapshot_with(Vec::new(), vec![claude]);
    let rendered = snapshot_to_screen(&snapshot, 44, 12);

    assert!(rendered.contains("wire the bridge"));
    assert!(
        !rendered.contains('—'),
        "the prompt stands in for the em dash"
    );
}

fn rendered_group_lines(snapshot: &SidebarSnapshot) -> Vec<Line<'static>> {
    let theme = Theme::fixed(true);
    let mut row_index = 0;
    let mut lines = Vec::new();
    let mut map = Vec::new();
    worktree_group_lines(
        &theme,
        &snapshot.worktree_groups[0],
        &snapshot.providers,
        snapshot.now,
        54,
        &snapshot.sidebar.context,
        snapshot.sidebar.card_density,
        None,
        &mut row_index,
        0,
        0,
        &CostRolls::default(),
        &mut lines,
        &mut map,
    );
    lines
}

fn span_for<'a>(lines: &'a [Line<'static>], text: &str) -> &'a Span<'static> {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == text)
        .unwrap_or_else(|| panic!("span {text:?} present"))
}

#[test]
fn unread_descriptor_renders_bold() {
    let agent = agent(
        "claude-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    );
    let mut unread = snapshot_with(Vec::new(), vec![agent.clone()]);
    unread.worktree_groups[0].rows[0].unread = true;
    let unread_lines = rendered_group_lines(&unread);
    assert!(
        span_for(&unread_lines, "done")
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );

    let read = snapshot_with(Vec::new(), vec![agent]);
    let read_lines = rendered_group_lines(&read);
    assert!(
        !span_for(&read_lines, "done")
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn unread_turn_error_label_stays_soft_and_bold() {
    let agent = agent(
        "claude-1",
        "claude",
        AgentStatus::Failed,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    );
    let mut snapshot = snapshot_with(Vec::new(), vec![agent]);
    let row = &mut snapshot.worktree_groups[0].rows[0];
    row.unread = true;
    row.as_agent_mut().unwrap().turn_error_label = Some("api error".to_owned());

    let lines = rendered_group_lines(&snapshot);
    let span = span_for(&lines, "api error");
    assert!(
        span.style.add_modifier.contains(Modifier::BOLD),
        "unread error labels keep the unread weight"
    );
    assert!(
        span.style.add_modifier.contains(Modifier::ITALIC),
        "the error-label branch keeps the soft italic style"
    );
}
