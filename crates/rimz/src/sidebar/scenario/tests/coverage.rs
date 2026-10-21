use super::super::Scenario;
use std::time::Duration;

fn scenario_snapshot(
    name: &str,
) -> Result<crate::store::snapshot::SidebarSnapshot, super::super::ScenarioError> {
    let source = match name {
        "empty" => include_str!("../../../../scenarios/empty.scn"),
        "fleet" => include_str!("../../../../scenarios/fleet.scn"),
        "provider" => include_str!("../../../../scenarios/provider.scn"),
        "cockpit" => include_str!("../../../../scenarios/cockpit.scn"),
        "focus" => include_str!("../../../../scenarios/focus.scn"),
        "economy" => include_str!("../../../../scenarios/economy.scn"),
        "reach" => include_str!("../../../../scenarios/reach.scn"),
        "clocks" => include_str!("../../../../scenarios/clocks.scn"),
        "clocks-done" => include_str!("../../../../scenarios/clocks-done.scn"),
        "clocks-nerd-font" => include_str!("../../../../scenarios/clocks-nerd-font.scn"),
        "clocks-done-nerd-font" => include_str!("../../../../scenarios/clocks-done-nerd-font.scn"),
        _ => panic!("unknown checked-in scene {name}"),
    };
    Ok(Scenario::parse(name, source)?.at(Duration::ZERO))
}

fn strip_sgr(ansi: &[u8]) -> String {
    let text = String::from_utf8_lossy(ansi);
    let mut stripped = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ch == 'm' {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    let mut escaped = false;
                    for ch in chars.by_ref() {
                        if ch == '\x07' || (escaped && ch == '\\') {
                            break;
                        }
                        escaped = ch == '\x1b';
                    }
                }
                _ => stripped.push(ch),
            }
        } else {
            stripped.push(ch);
        }
    }
    stripped
        .lines()
        .map(|line| {
            // The text fixture strips SGR, so color-chip tabs and plain cap
            // tabs share one stable shape.
            let normalized = line
                .chars()
                .map(|ch| match ch {
                    '┤' | '├' => '─',
                    _ => ch,
                })
                .collect::<String>();
            normalized.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn provider_fixture_frame_is_deterministic() {
    assert_fixture_frame_snapshot("provider", "provider_fixture_frame");
}

#[test]
fn expanded_fixture_frame_is_deterministic() {
    let snapshot = scenario_snapshot("cockpit").unwrap();
    let mut ansi = Vec::new();
    crate::sidebar_pane::render::render_expanded_line_ansi(&mut ansi, &snapshot, 54).unwrap();

    insta::assert_snapshot!("expanded_fixture_frame", strip_sgr(&ansi));
}

#[test]
fn fixture_frames_are_deterministic() {
    for (state, snapshot_name) in [
        ("fleet", "fleet_fixture_frame"),
        ("cockpit", "cockpit_fixture_frame"),
        ("focus", "focus_fixture_frame"),
        ("economy", "economy_fixture_frame"),
        ("reach", "reach_fixture_frame"),
        ("clocks", "clocks_fixture_frame"),
        ("clocks-done", "clocks_done_fixture_frame"),
        ("clocks-nerd-font", "clocks_nerd_font_fixture_frame"),
        (
            "clocks-done-nerd-font",
            "clocks_done_nerd_font_fixture_frame",
        ),
    ] {
        assert_fixture_frame_snapshot(state, snapshot_name);
    }
}

#[test]
fn clock_fixtures_separate_runtime_quiet_and_finished_durations() {
    for (state, markers) in [
        (
            "clocks",
            vec![
                "Implement 4h12m",
                "4h51m🮇",
                "   2h▐",
                "◔  8m▐",
                "silent 30m",
                "◉ 30m▐",
                "   9m▐",
                "◉  4h▐",
            ],
        ),
        ("clocks-done", vec!["Done", "4h51m🮇", "   9m▐"]),
        (
            "clocks-nerd-font",
            vec![
                "Implement 4h12m",
                "4h51m🮇",
                "   2h▐",
                "silent 30m",
                "   9m▐",
            ],
        ),
        ("clocks-done-nerd-font", vec!["Done", "4h51m🮇", "   9m▐"]),
    ] {
        let snapshot = scenario_snapshot(state).unwrap();
        let mut ansi = Vec::new();
        crate::sidebar_pane::render::render_fixed_line_ansi(&mut ansi, &snapshot, 54, 34).unwrap();
        let frame = strip_sgr(&ansi);
        for marker in markers {
            assert!(frame.contains(marker), "{marker} absent:\n{frame}");
        }
        if matches!(state, "clocks-done" | "clocks-done-nerd-font") {
            assert!(!frame.contains("4h12m"), "{frame}");
        }
    }
}

fn assert_fixture_frame_snapshot(state: &str, snapshot_name: &str) {
    let snapshot = scenario_snapshot(state).unwrap();

    let mut ansi = Vec::new();
    crate::sidebar_pane::render::render_fixed_line_ansi(&mut ansi, &snapshot, 54, 34).unwrap();

    let rendered = strip_sgr(&ansi);
    if matches!(state, "reach" | "provider") {
        assert!(
            rendered.lines().any(|line| line.starts_with("▌      ▤")),
            "{rendered}"
        );
    }
    insta::assert_snapshot!(snapshot_name, rendered);
}

#[test]
fn scenarios_build_with_coherent_context() {
    let states =
        ["cockpit", "focus", "economy", "reach"].map(|state| scenario_snapshot(state).unwrap());

    for snapshot in &states {
        assert!(!snapshot.providers.is_empty(), "{}", snapshot.display_name);
        assert!(
            snapshot
                .worktree_groups
                .iter()
                .any(|group| !group.rows.is_empty()),
            "{}",
            snapshot.display_name,
        );
    }

    let focus = scenario_snapshot("focus").unwrap();
    let cards = agent_cards(&focus);
    let planner = cards
        .iter()
        .find(|card| card.handle.as_deref() == Some("planner"))
        .expect("planner card");
    assert_eq!(planner.status, crate::agents::AgentStatus::Success);
    assert_eq!(planner.phase, crate::agents::TurnPhase::Idle);
    assert_eq!(planner.sub_agents.len(), 5);
    assert_eq!(
        planner
            .sub_agents
            .iter()
            .filter(|child| child.name == "Explore")
            .count(),
        3,
    );
    assert_eq!(
        planner
            .sub_agents
            .iter()
            .filter(|child| child.name == "Plan")
            .count(),
        2,
    );
    assert!(planner.sub_agents.iter().all(|child| {
        child.name != "Explore" || child.status == crate::agents::AgentStatus::Success
    }));
    assert!(planner.sub_agents.iter().all(|child| {
        child.name != "Plan" || child.status == crate::agents::AgentStatus::Success
    }));
    let coder = agent_card_by_id(&focus, "agent:codex:coder");
    assert_eq!(coder.sub_agents.len(), 2);
    assert_eq!(
        coder
            .sub_agents
            .iter()
            .filter(|child| child.name == "general-purpose")
            .count(),
        2,
    );
    let allowed = ["Explore", "Plan", "general-purpose"]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let sub_agent_names = states
        .iter()
        .flat_map(agent_cards)
        .flat_map(|card| &card.sub_agents)
        .map(|child| child.name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        sub_agent_names.is_subset(&allowed),
        "unexpected sub-agent names: {sub_agent_names:?}",
    );
    assert!(states.iter().flat_map(agent_cards).all(|card| {
        card.sub_agents
            .iter()
            .all(|child| child.status != crate::agents::AgentStatus::Waiting)
    }));
    let handles = cards
        .iter()
        .filter_map(|card| card.handle.as_deref())
        .collect::<Vec<_>>();
    assert!(handles.contains(&"planner"));
    assert!(handles.contains(&"coder"));
    assert!(handles.contains(&"reviewer"));
    assert!(handles.contains(&"architect"));
    assert!(handles.contains(&"developer"));
    assert!(handles.contains(&"sre"));

    let cockpit = &states[0];
    assert_eq!(cockpit.presence, None);
    assert!(cockpit.link.is_none());
    assert!(states[1].link.is_some());
    assert!(cockpit.worktree_groups.iter().any(|group| {
        group.landed == Some(true)
            && group.trunk_sync == Some(crate::store::snapshot::WorktreeTrunkSync::Merged)
            && group.pr_state == Some(crate::store::snapshot::WorktreePrState::Merged)
    }));
    let reach = &states[3];
    assert_eq!(reach.presence, None);
    assert!(reach.link.is_none());
    let statuses = cockpit
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter_map(|row| row.status())
        .collect::<Vec<_>>();
    assert!(statuses.contains(&crate::agents::AgentStatus::Running));
    assert!(statuses.contains(&crate::agents::AgentStatus::Idle));
    assert!(statuses.contains(&crate::agents::AgentStatus::Success));
    assert!(statuses.contains(&crate::agents::AgentStatus::Waiting));
    assert!(statuses.contains(&crate::agents::AgentStatus::Failed));
    assert!(statuses.contains(&crate::agents::AgentStatus::Paused));
    assert!(agent_cards(cockpit).iter().any(|card| {
        card.status == crate::agents::AgentStatus::Running
            && card.phase == crate::agents::TurnPhase::Acting
    }));
    assert!(agent_cards(cockpit).iter().any(|card| {
        card.status == crate::agents::AgentStatus::Running
            && card.phase == crate::agents::TurnPhase::Reasoning
    }));
    assert!(
        agent_cards(cockpit)
            .iter()
            .any(|card| card.turn_error_label.as_deref() == Some("API error"))
    );
    assert!(
        agent_cards(cockpit)
            .iter()
            .any(|card| card.compacting || card.compaction_count > 0)
    );

    let all_statuses = states
        .iter()
        .flat_map(|snapshot| snapshot.worktree_groups.iter())
        .flat_map(|group| group.rows.iter())
        .filter_map(|row| row.status())
        .collect::<Vec<_>>();
    for status in [
        crate::agents::AgentStatus::Waiting,
        crate::agents::AgentStatus::Failed,
        crate::agents::AgentStatus::Paused,
        crate::agents::AgentStatus::Success,
        crate::agents::AgentStatus::Running,
        crate::agents::AgentStatus::Idle,
    ] {
        assert!(all_statuses.contains(&status), "missing {status:?}");
    }

    for snapshot in &states {
        let live = snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.status_counts)
            .map(|count| count.count)
            .sum::<usize>();
        assert!((10..=42).contains(&live), "live count {live}");
        for group in snapshot.worktree_groups.iter() {
            let rendered_idle = group
                .rows
                .iter()
                .filter(|row| row.status() == Some(crate::agents::AgentStatus::Idle))
                .count();
            let counted_idle = group
                .status_counts
                .iter()
                .find(|count| count.status == crate::agents::AgentStatus::Idle)
                .map(|count| count.count)
                .unwrap_or(0);
            assert_eq!(counted_idle, rendered_idle);
        }
        let sessions = snapshot.value_tally.as_ref().unwrap().headline.sessions;
        assert!((60..=120).contains(&sessions), "sessions {sessions}");
    }
    assert!(states.iter().flat_map(agent_cards).any(|card| {
        card.context
            .as_ref()
            .and_then(|context| context.cost.as_ref())
            .and_then(|cost| cost.total_cost_usd)
            .is_some()
    }));
    for row in states
        .iter()
        .flat_map(|snapshot| snapshot.worktree_groups.iter())
        .flat_map(|group| group.rows.iter())
    {
        let Some(card) = row.as_agent() else { continue };
        if row.status() == Some(crate::agents::AgentStatus::Idle) {
            assert_eq!(card.task, None, "{}", row.id);
            assert_eq!(card.usage.context_pct, None, "{}", row.id);
            assert_eq!(card.usage.context_window, None, "{}", row.id);
            assert_eq!(card.usage.total_tokens, None, "{}", row.id);
            assert!(card.context.is_none(), "{}", row.id);
            continue;
        }
        let expected_window = if row.name == "claude" {
            1_000_000
        } else {
            272_000
        };
        assert_eq!(
            card.usage.context_window,
            Some(expected_window),
            "{}",
            row.id
        );
        assert!(
            card.usage
                .total_tokens
                .is_some_and(|total| total < expected_window),
            "{}",
            row.id,
        );
        let split = card.usage.cache_read_input_tokens.unwrap_or(0)
            + card.usage.cache_write_input_tokens.unwrap_or(0)
            + card.usage.fresh_input_tokens.unwrap_or(0);
        assert_eq!(card.usage.total_tokens, Some(split), "{}", row.id);
    }
}

#[test]
fn scenario_shimmer_leads_are_waiting_asks() {
    let cases: [(&str, Option<(&str, crate::agents::AgentStatus)>); 4] = [
        (
            "focus",
            Some((
                "agent:claude:rollout-reviewer",
                crate::agents::AgentStatus::Waiting,
            )),
        ),
        (
            "cockpit",
            Some(("agent:opencode:theme", crate::agents::AgentStatus::Waiting)),
        ),
        (
            "reach",
            Some(("agent:claude:netcheck", crate::agents::AgentStatus::Waiting)),
        ),
        ("economy", None),
    ];

    for (state, expected) in cases {
        let snapshot = scenario_snapshot(state).unwrap();
        let lead = crate::store::snapshot::lead_unread_row(&snapshot.worktree_groups);
        match (lead, expected) {
            (Some(row), Some((expected_id, expected_status))) => {
                assert_eq!(row.id, expected_id, "{state:?}");
                assert_eq!(row.status(), Some(expected_status), "{state:?}");
            }
            (None, None) => {}
            (Some(row), None) => panic!("{state:?} unexpected lead {}", row.id),
            (None, Some((expected_id, _))) => panic!("{state:?} missing lead {expected_id}"),
        }
    }

    for (state, id, label) in [
        ("focus", "agent:claude:ci-paused", "API Error: Overloaded"),
        (
            "cockpit",
            "agent:claude:mux-merge-paused",
            "API Error: Overloaded",
        ),
        ("cockpit", "agent:claude:observer", "API Error: Overloaded"),
        (
            "reach",
            "agent:claude:netcheck-paused",
            "API Error: rate limit exceeded",
        ),
        (
            "economy",
            "agent:claude:limit-paused",
            "API Error: rate limit exceeded",
        ),
    ] {
        let snapshot = scenario_snapshot(state).unwrap();
        assert_eq!(
            agent_card_by_id(&snapshot, id).turn_error_label.as_deref(),
            Some(label),
            "{state:?} {id}",
        );
    }
}

#[test]
fn scenario_frames_render_decisive_markers() {
    assert_fixture_frame_contains(
        "cockpit",
        &["#91", "stabilize render diff", "cargo nextest"],
    );
    assert_fixture_frame_lacks("cockpit", &["away", "pnpm serve"]);
    assert_fixture_frame_contains("focus", &["planner", "coder", "reviewer", "48ms"]);
    assert_fixture_frame_contains(
        "economy",
        &["OpenAI OAuth", "provider-store", "GPT 5.5", "pnpm serve"],
    );
    assert_fixture_frame_contains(
        "reach",
        &[
            "remote-link",
            "browser-reach",
            "Claude Max",
            "network-check",
        ],
    );
    assert_fixture_frame_lacks("reach", &["edge-cache"]);
}

fn agent_card_by_id<'a>(
    snapshot: &'a crate::store::snapshot::SidebarSnapshot,
    id: &str,
) -> &'a crate::store::snapshot::AgentCard {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .find_map(|row| match &row.card {
            crate::store::snapshot::RowCard::Agent(card) if row.id == id => Some(card.as_ref()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("agent card {id}"))
}

fn agent_cards(
    snapshot: &crate::store::snapshot::SidebarSnapshot,
) -> Vec<&crate::store::snapshot::AgentCard> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .filter_map(|row| row.as_agent())
        .collect()
}

fn assert_fixture_frame_contains(state: &str, markers: &[&str]) {
    let snapshot = scenario_snapshot(state).unwrap();
    let mut ansi = Vec::new();
    crate::sidebar_pane::render::render_fixed_line_ansi(&mut ansi, &snapshot, 80, 60).unwrap();
    let frame = strip_sgr(&ansi);
    for marker in markers {
        assert!(
            frame.contains(marker),
            "fixture {state:?} missing marker {marker:?}:\n{frame}",
        );
    }
}

fn assert_fixture_frame_lacks(state: &str, markers: &[&str]) {
    let snapshot = scenario_snapshot(state).unwrap();
    let mut ansi = Vec::new();
    crate::sidebar_pane::render::render_fixed_line_ansi(&mut ansi, &snapshot, 80, 60).unwrap();
    let frame = strip_sgr(&ansi);
    for marker in markers {
        assert!(
            !frame.contains(marker),
            "fixture {state:?} unexpectedly contains marker {marker:?}:\n{frame}",
        );
    }
}
