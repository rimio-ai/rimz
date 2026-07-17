use super::*;

#[test]
fn render_selected_card_keeps_finished_metadata_without_a_live_clock() {
    // A selected parent expands its `⧉ subagents` list. A finished child
    // keeps its exact token/model/effort row, while the elapsed clock is dropped
    // because a done child needs no live work span. A still-running child keeps both lines: the live thinking head,
    // type, and description, then the token spend `◇` and model left with the
    // live sub-minute `<1m` clock-fill elapsed pinned right.
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.context = Some(claude_context(fixed_now()));

    let mut child = agent(
        "child-1",
        "claude",
        AgentStatus::Success,
        None,
        None,
        Some("Explore"),
    );
    child.parent_agent_id = Some("claude-1".into());
    child.subagent_description = Some("locate the render seam".to_owned());
    child.subagent_started_at = Some(fixed_now() - Duration::from_secs(90));
    child.last_activity = fixed_now() - Duration::from_secs(30);
    child.last_seen = fixed_now() - Duration::from_secs(30);
    child.total_tokens = Some(12_400);
    // A bare model id — the renderer prettifies it through `model_label`.
    child.model = Some("claude-opus-4-8".to_owned());
    // Claude reports the child's effort on its `SubagentStop`.
    child.effort = Some("high".to_owned());

    let mut fresh = agent(
        "child-2",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("review"),
    );
    fresh.parent_agent_id = Some("claude-1".into());
    // Mid-reasoning: the child's leading cell is the thinking orbit (frame 0
    // of the animation at the test's fixed phase), not the static `⢿`.
    fresh.phase = crate::agents::TurnPhase::Reasoning;
    fresh.subagent_description = Some("audit the trust hash".to_owned());
    fresh.subagent_started_at = Some(fixed_now() - Duration::from_secs(30));
    fresh.total_tokens = Some(3_100);
    // A sibling on a different model — the per-child label tells them apart.
    fresh.model = Some("claude-haiku-4-5".to_owned());

    let snapshot = snapshot_with(vec![parent, child, fresh]);
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

    assert!(
        rendered.contains("⧉ subagents (2)"),
        "the expanded card lists its children:\n{rendered}"
    );
    // The finished child collapses to one line: its type + the description of
    // what the parent asked it to do.
    assert!(
        rendered.contains("Explore — locate the render seam"),
        "the finished child keeps its type line:\n{rendered}"
    );
    // The running child's leading cell is the thinking orbit (frame 0 at the
    // test's fixed animation phase), the agent-row head vocabulary verbatim.
    assert!(
        rendered.contains("⠁ review — audit the trust hash"),
        "a reasoning child wears the thinking head:\n{rendered}"
    );
    // The running child keeps its metadata row — token spend and model left
    // (`3k` sized to the one rendered row, no sibling to pad to), the live
    // sub-minute `<1m` clock pinned right.
    assert!(
        rendered.contains("◇  3k · Haiku 4.5"),
        "the running child carries its token spend and model:\n{rendered}"
    );
    assert!(
        rendered.contains("◔ <1m"),
        "the running child reads the live sub-minute clock:\n{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|line| line.contains("◇ 12k · Opus 4.8") && line.contains("· high")),
        "the finished child retains exact metadata:\n{rendered}"
    );
    let finished_line = rendered
        .lines()
        .find(|line| line.contains("◇ 12k"))
        .expect("finished child metadata line");
    assert!(
        !finished_line.contains('◔'),
        "the finished child has no elapsed clock:\n{rendered}"
    );
    // Both metadata-bearing children render a second line.
    let subagent_metadata_rows = rendered
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            line.contains("◇")
                && line.contains("▌")
                && !trimmed.starts_with("W:")
                && !trimmed.starts_with("M:")
        })
        .count();
    assert_eq!(
        subagent_metadata_rows, 2,
        "both metadata-bearing children carry a `◇` row:\n{rendered}"
    );
    assert_snapshot("subagent_two_line_entry", rendered);
}

#[test]
fn metadata_free_finished_subagent_stays_one_line() {
    let parent = agent(
        "copilot-root",
        "copilot",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("delegate"),
    );
    let mut child = agent(
        "copilot-child",
        "copilot",
        AgentStatus::Success,
        None,
        None,
        Some("cleanup"),
    );
    child.parent_agent_id = Some("copilot-root".into());

    let snapshot = snapshot_with(vec![parent, child]);
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
    let lines = rendered.lines().collect::<Vec<_>>();
    let child_line = lines
        .iter()
        .position(|line| line.contains("✓ cleanup"))
        .unwrap_or_else(|| panic!("finished child missing:\n{rendered}"));
    assert!(
        !lines
            .get(child_line + 1)
            .is_some_and(|line| line.starts_with("▌      ")),
        "metadata-free completion stays one line:\n{rendered}"
    );
}
#[test]
fn subagent_metadata_blank_fills_the_per_card_grid() {
    // A child missing a field a sibling carries blank-fills that slot, so the
    // card's metadata lines stay one column grid: the token-less child's model
    // starts exactly under its sibling's, with no bare `◇` and no orphan `·`
    // seam leading the line. Both children are running so both render a metadata
    // row — a finished child collapses to one line instead (covered separately).
    let mut parent = agent(
        "claude-1",
        "claude",
        AgentStatus::Running,
        Some("/repo/main"),
        Some("main"),
        Some("db migrate"),
    );
    parent.context = Some(claude_context(fixed_now()));

    let mut spender = agent(
        "child-1",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("Explore"),
    );
    spender.parent_agent_id = Some("claude-1".into());
    spender.subagent_started_at = Some(fixed_now() - Duration::from_secs(90));
    spender.total_tokens = Some(12_400);
    spender.model = Some("claude-opus-4-8".to_owned());
    spender.effort = Some("high".to_owned());

    // A sibling before its first `subagentStatusLine` report: no tokens yet,
    // its model already known from its own lifecycle events.
    let mut quiet = agent(
        "child-2",
        "claude",
        AgentStatus::Running,
        None,
        None,
        Some("review"),
    );
    quiet.parent_agent_id = Some("claude-1".into());
    quiet.model = Some("claude-haiku-4-5".to_owned());

    let snapshot = snapshot_with(vec![parent, spender, quiet]);
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

    // Anchor each lookup to the child's own metadata line — the parent's
    // identity line also reads `Opus 4.8`.
    let char_col = |line_needle: &str, col_needle: &str| {
        let line = rendered
            .lines()
            .find(|line| line.contains(line_needle))
            .unwrap_or_else(|| panic!("{line_needle:?} missing:\n{rendered}"));
        line[..line.find(col_needle).unwrap()].chars().count()
    };
    assert_eq!(
        char_col("◇ 12k", "Opus 4.8"),
        char_col("Haiku 4.5", "Haiku 4.5"),
        "the token-less child's model starts under its sibling's:\n{rendered}"
    );
    assert!(
        !rendered.contains("· Haiku 4.5"),
        "a blank-filled token slot carries no orphan seam:\n{rendered}"
    );
    let quiet_line = rendered
        .lines()
        .find(|line| line.contains("Haiku 4.5"))
        .expect("the token-less child still renders its metadata line");
    assert!(
        !quiet_line.contains('◇'),
        "no bare `◇` over a blank figure:\n{rendered}"
    );
}

#[test]
fn codex_subagent_renders_nickname_nested_path_and_current_context() {
    let parent = agent(
        "codex-root",
        "codex",
        AgentStatus::Success,
        Some("/repo/main"),
        Some("main"),
        Some("ship hooks"),
    );
    let mut child = agent(
        "codex-child",
        "codex",
        AgentStatus::Running,
        None,
        None,
        Some("research/explore_hooks"),
    );
    child.parent_agent_id = Some("codex-root".into());
    child.name = Some("Atlas".to_owned());
    child.name_explicit = true;
    child.total_tokens = Some(32_100);
    child.model = Some("gpt-5.5-codex".to_owned());
    child.effort = Some("xhigh".to_owned());

    let snapshot = snapshot_with(vec![parent, child]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            ..Default::default()
        },
        60,
        20,
    );

    assert!(
        rendered.contains("Atlas — research/explore_hooks"),
        "nickname and flat nested path stay distinct:\n{rendered}"
    );
    assert!(
        rendered.contains("◇ 32k"),
        "Codex's current context reading is rendered:\n{rendered}"
    );
}
