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

#[test]
fn line_two_control_characters_collapse_before_framing() {
    let mut codex = agent(
        "codex-1",
        "codex",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    let mut context = codex_context(fixed_now());
    context.session_preview = Some("ship\nwide\tlabel\rnow\u{0007}".to_owned());
    codex.context = Some(context);
    let rendered = snapshot_to_screen(&snapshot_with(Vec::new(), vec![codex]), 44, 12);
    let line = rendered
        .lines()
        .find(|line| line.contains("ship wide label now"))
        .unwrap_or_else(|| panic!("single-line description rendered:\n{rendered}"));

    assert_eq!(
        line.chars().nth(43),
        Some('▐'),
        "the selected card's right rail stays in the final column:\n{rendered}"
    );
    for leaked in ['\n', '\r', '\t', '\u{0007}'] {
        assert!(
            !line.contains(leaked),
            "description line contains no control character {leaked:?}: {line:?}"
        );
    }
}

fn rendered_group_lines_with(
    snapshot: &SidebarSnapshot,
    theme: &Theme,
    phase: u64,
) -> Vec<Line<'static>> {
    let mut row_index = 0;
    let mut lines = Vec::new();
    let mut map = Vec::new();
    worktree_group_lines(
        theme,
        &snapshot.worktree_groups[0],
        &snapshot.providers,
        snapshot.now,
        54,
        &snapshot.sidebar.context,
        snapshot.sidebar.card_density,
        None,
        &mut row_index,
        0,
        phase,
        &CostRolls::default(),
        &mut lines,
        &mut map,
    );
    lines
}

fn rendered_group_lines_at(snapshot: &SidebarSnapshot, phase: u64) -> Vec<Line<'static>> {
    rendered_group_lines_with(snapshot, &Theme::fixed(true), phase)
}

fn span_for<'a>(lines: &'a [Line<'static>], text: &str) -> &'a Span<'static> {
    lines
        .iter()
        .flat_map(|line| line.spans.iter())
        .find(|span| span.content.as_ref() == text)
        .unwrap_or_else(|| panic!("span {text:?} present"))
}

#[test]
fn unread_descriptor_grows_bold_without_dimming() {
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
    // Under NO_COLOR the unread descriptor shares the lead glyph/name blink
    // through a grow-only weight toggle: plain at the trough, bold near the
    // crest, never dim.
    let unread_mods: Vec<_> = (0..32)
        .map(|phase| {
            span_for(&rendered_group_lines_at(&unread, phase), "done")
                .style
                .add_modifier
        })
        .collect();
    assert!(unread_mods.iter().any(|m| m.contains(Modifier::BOLD)));
    assert!(unread_mods.iter().any(|m| m.is_empty()));
    assert!(unread_mods.iter().all(|m| !m.contains(Modifier::DIM)));

    // A read descriptor never blinks — its weight is the same at every phase.
    let read = snapshot_with(Vec::new(), vec![agent]);
    for phase in 0..32 {
        let modifier = span_for(&rendered_group_lines_at(&read, phase), "done")
            .style
            .add_modifier;
        assert!(!modifier.contains(Modifier::BOLD));
    }
}

#[test]
fn unread_descriptor_holds_bold_while_colored_pulse_brightens() {
    let mut sidebar = crate::config::SidebarConfig::default();
    sidebar.theme.mode = crate::config::ThemeMode::Truecolor;
    let theme = Theme::fixed_for_sidebar(false, &sidebar);
    let agent = agent(
        "claude-1",
        "claude",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("done"),
    );
    let mut snapshot = snapshot_with(Vec::new(), vec![agent]);
    snapshot.worktree_groups[0].rows[0].unread = true;

    let styles: Vec<_> = (0..32)
        .map(|phase| span_for(&rendered_group_lines_with(&snapshot, &theme, phase), "done").style)
        .collect();
    assert!(
        styles
            .iter()
            .all(|style| style.add_modifier == Modifier::BOLD),
        "colored unread descriptors hold bold through the whole pulse"
    );
    assert!(
        styles.iter().any(|style| style.fg != styles[0].fg),
        "the colored pulse changes lightness phase to phase"
    );
    assert!(
        styles
            .iter()
            .all(|style| !style.add_modifier.contains(Modifier::DIM)),
        "the grow-only colored pulse never dims"
    );
}

#[test]
fn unread_turn_error_label_pulses_and_stays_italic() {
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

    let mods: Vec<_> = (0..32)
        .map(|phase| {
            span_for(&rendered_group_lines_at(&snapshot, phase), "api error")
                .style
                .add_modifier
        })
        .collect();
    assert!(
        mods.iter().any(|m| m.contains(Modifier::BOLD)),
        "the unread error label blinks bold throughout the colored pulse"
    );
    assert!(
        mods.iter().all(|m| m.contains(Modifier::ITALIC)),
        "the error-label branch keeps the soft italic style throughout"
    );
}
