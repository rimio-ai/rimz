use super::*;

#[test]
fn render_selected_card_shows_subagent_description_tokens_and_elapsed() {
    // A selected parent expands its `⧉ subagents` list. Each child reads two
    // lines: the live head (the thinking sparkle while it reasons, `✓` once
    // it lands), the type, and its `subagentStatusLine` description, then the
    // token spend `◇`, model, and effort left with the clock-fill elapsed
    // work pinned right. The first child is finished, so its elapsed is
    // frozen at `last_activity − started` (exactly 60s here, independent of
    // wall-clock) — a deterministic ` 1m` in the fixed three-cell slot — and
    // it carries the effort its `SubagentStop` reported; the second is still
    // running ~30s in, so it reads the sub-minute `<1m` (never seconds), and
    // the two right-pinned clusters stack into one column.
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
    // Mid-reasoning: the child's leading cell is the thinking sparkle (frame 0
    // of the animation at the test's fixed phase), not the static `⢿`.
    fresh.phase = crate::agents::TurnPhase::Reasoning;
    fresh.subagent_description = Some("audit the trust hash".to_owned());
    fresh.subagent_started_at = Some(fixed_now() - Duration::from_secs(30));
    fresh.total_tokens = Some(3_100);
    // A sibling on a different model — the per-child label tells them apart.
    fresh.model = Some("claude-haiku-4-5".to_owned());

    let snapshot = snapshot_with(Vec::new(), vec![parent, child, fresh]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            ..Default::default()
        },
        54,
        20,
    );

    assert!(
        rendered.contains("⧉ subagents (2)"),
        "the expanded card lists its children:\n{rendered}"
    );
    // Line 1: type + the description of what the parent asked it to do.
    assert!(
        rendered.contains("Explore — locate the render seam"),
        "line 1 carries the description:\n{rendered}"
    );
    // The running child's leading cell is the thinking sparkle (frame 0 at the
    // test's fixed animation phase), the agent-row head vocabulary verbatim.
    assert!(
        rendered.contains("· review — audit the trust hash"),
        "a reasoning child wears the thinking head:\n{rendered}"
    );
    // Line 2: token spend, model, and effort left, elapsed work right-pinned.
    // 12_400 → the whole-unit `12k`, the bare id prettified to `Opus 4.8` and
    // padded to the widest sibling model (`Haiku 4.5`) so the effort column
    // stacks, the stop-reported effort after it, 60s frozen → ` 1m`
    // right-aligned in the fixed slot.
    assert!(
        rendered.contains("◇ 12k · Opus 4.8  · high"),
        "line 2 carries the token spend, model, and effort:\n{rendered}"
    );
    // The narrower `3k` right-aligns under the sibling's `12k`, so the `·`
    // seams and models stack into one column.
    assert!(
        rendered.contains("◇  3k · Haiku 4.5"),
        "the sibling carries its own model, column-aligned:\n{rendered}"
    );
    assert!(
        rendered.contains("◔  1m"),
        "line 2 carries the frozen elapsed in the fixed slot:\n{rendered}"
    );
    // The running child reads the sub-minute `<1m`, never a seconds figure.
    assert!(
        rendered.contains("◔ <1m"),
        "a sub-minute child reads `<1m`:\n{rendered}"
    );
    assert_snapshot("subagent_two_line_entry", rendered);
}
#[test]
fn subagent_metadata_blank_fills_the_per_card_grid() {
    // A child missing a field a sibling carries blank-fills that slot, so the
    // card's metadata lines stay one column grid: the token-less child's model
    // starts exactly under its sibling's, with no bare `◇` and no orphan `·`
    // seam leading the line.
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
        AgentStatus::Success,
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

    let snapshot = snapshot_with(Vec::new(), vec![parent, spender, quiet]);
    let rendered = snapshot_to_screen_with_alert_and_ui(
        &snapshot,
        None,
        &UiState {
            selected_index: 0,
            ..Default::default()
        },
        54,
        20,
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
