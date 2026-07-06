use super::SidebarFixtureState;
use super::fixture::sidebar_fixture_snapshot;

#[test]
fn plugin_focus_argv_parses_without_workspace_id() {
    use clap::Parser;

    crate::cli::Cli::try_parse_from([
        "rimz",
        "sidebar",
        "focus",
        "--toggle",
        "--session-name",
        "s",
        "--mux",
        "zellij",
    ])
    .expect("plugin focus argv must parse");

    assert!(
        crate::cli::Cli::try_parse_from([
            "rimz",
            "sidebar",
            "focus",
            "--toggle",
            "--workspace-id",
            "ws_0123456789abcdef01234567",
            "--session-name",
            "s",
            "--mux",
            "zellij",
        ])
        .is_err(),
        "sidebar focus intentionally accepts no workspace id",
    );
}

#[test]
fn gallery_argv_parses_pets_flag() {
    use clap::Parser;

    crate::cli::Cli::try_parse_from(["rimz", "sidebar", "gallery", "--pets"])
        .expect("sidebar gallery --pets must parse");
    crate::cli::Cli::try_parse_from(["rimz", "sidebar", "gallery-render", "--pets"])
        .expect("sidebar gallery-render --pets must parse");
}

fn strip_sgr(ansi: &[u8]) -> String {
    let text = String::from_utf8_lossy(ansi);
    let mut stripped = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            for ch in chars.by_ref() {
                if ch == 'm' {
                    break;
                }
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
    let snapshot = sidebar_fixture_snapshot(SidebarFixtureState::Provider).unwrap();

    let mut ansi = Vec::new();
    rimz::sidebar_pane::render::render_fixed_line_ansi(&mut ansi, &snapshot, None, 54, 34).unwrap();

    insta::assert_snapshot!("provider_fixture_frame", strip_sgr(&ansi));
}

#[test]
fn gallery_columns_follow_requested_order_and_selection() {
    let columns = super::gallery_fixture_columns();
    assert_eq!(
        columns.map(|(state, _)| state),
        [
            SidebarFixtureState::Focus,
            SidebarFixtureState::Cockpit,
            SidebarFixtureState::Reach,
            SidebarFixtureState::Economy,
        ],
    );

    for ((state, selector), (expected_id, expected_kind)) in columns.into_iter().zip([
        ("agent:claude:planner", "claude"),
        ("agent:codex:pricing", "codex"),
        ("agent:pi:reach", "pi"),
        ("agent:opencode:credits", "opencode"),
    ]) {
        let snapshot = sidebar_fixture_snapshot(state).unwrap();
        let selected_index = super::gallery_selected_index(&snapshot, selector);
        let selected = snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .nth(selected_index)
            .expect("selected row");
        if !matches!(
            state,
            SidebarFixtureState::Focus | SidebarFixtureState::Cockpit
        ) {
            assert!(selected_index > 0, "{state:?} selected top row");
        }
        assert_eq!(selected.id, expected_id);
        assert_eq!(selected.name, expected_kind);
        assert!(selected.as_agent().is_some());
        assert!(!selected.unread);
    }
}

#[test]
fn gallery_render_columns_apply_pets_override() {
    let columns =
        super::gallery_render_columns(true, &rimz::config::ThemeConfig::default()).unwrap();
    assert_eq!(
        columns
            .iter()
            .map(|(snapshot, _)| (
                snapshot.theme.pets.enabled,
                snapshot.theme.pets.pet.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            (true, "rocky"),
            (true, "seedy"),
            (true, "fireball"),
            (true, "bsod")
        ],
    );

    let disabled =
        super::gallery_render_columns(false, &rimz::config::ThemeConfig::default()).unwrap();
    assert!(
        disabled
            .iter()
            .all(|(snapshot, _)| !snapshot.theme.pets.enabled)
    );
}

#[test]
fn gallery_fixtures_build_with_coherent_context() {
    let states = [
        SidebarFixtureState::Cockpit,
        SidebarFixtureState::Focus,
        SidebarFixtureState::Economy,
        SidebarFixtureState::Reach,
    ]
    .map(|state| sidebar_fixture_snapshot(state).unwrap());

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

    let focus = sidebar_fixture_snapshot(SidebarFixtureState::Focus).unwrap();
    let cards = agent_cards(&focus);
    let planner = cards
        .iter()
        .find(|card| card.handle.as_deref() == Some("planner"))
        .expect("planner card");
    assert_eq!(planner.status, rimz::agents::AgentStatus::Success);
    assert_eq!(planner.phase, rimz::agents::TurnPhase::Idle);
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
        child.name != "Explore" || child.status == rimz::agents::AgentStatus::Success
    }));
    assert!(planner.sub_agents.iter().all(|child| {
        child.name != "Plan" || child.status == rimz::agents::AgentStatus::Success
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
            .all(|child| child.status != rimz::agents::AgentStatus::Waiting)
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
            && group.trunk_sync == Some(rimz::WorktreeTrunkSync::Merged)
            && group.pr_state == Some(rimz::WorktreePrState::Merged)
    }));
    let reach = &states[3];
    assert_eq!(reach.presence, None);
    assert!(reach.link.is_none());
    let statuses = cockpit
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter_map(|row| row.status())
        .collect::<Vec<_>>();
    assert!(statuses.contains(&rimz::agents::AgentStatus::Running));
    assert!(statuses.contains(&rimz::agents::AgentStatus::Idle));
    assert!(statuses.contains(&rimz::agents::AgentStatus::Success));
    assert!(statuses.contains(&rimz::agents::AgentStatus::Waiting));
    assert!(statuses.contains(&rimz::agents::AgentStatus::Failed));
    assert!(statuses.contains(&rimz::agents::AgentStatus::Paused));
    assert!(agent_cards(cockpit).iter().any(|card| {
        card.status == rimz::agents::AgentStatus::Running
            && card.phase == rimz::agents::TurnPhase::Acting
    }));
    assert!(agent_cards(cockpit).iter().any(|card| {
        card.status == rimz::agents::AgentStatus::Running
            && card.phase == rimz::agents::TurnPhase::Reasoning
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
        .flat_map(|snapshot| &snapshot.worktree_groups)
        .flat_map(|group| &group.rows)
        .filter_map(|row| row.status())
        .collect::<Vec<_>>();
    for status in [
        rimz::agents::AgentStatus::Waiting,
        rimz::agents::AgentStatus::Failed,
        rimz::agents::AgentStatus::Paused,
        rimz::agents::AgentStatus::Success,
        rimz::agents::AgentStatus::Running,
        rimz::agents::AgentStatus::Idle,
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
        for group in &snapshot.worktree_groups {
            let rendered_idle = group
                .rows
                .iter()
                .filter(|row| row.status() == Some(rimz::agents::AgentStatus::Idle))
                .count();
            let counted_idle = group
                .status_counts
                .iter()
                .find(|count| count.status == rimz::agents::AgentStatus::Idle)
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
        .flat_map(|snapshot| &snapshot.worktree_groups)
        .flat_map(|group| &group.rows)
    {
        let Some(card) = row.as_agent() else { continue };
        if row.status() == Some(rimz::agents::AgentStatus::Idle) {
            assert_eq!(card.task, None, "{}", row.id);
            assert_eq!(card.context_pct, None, "{}", row.id);
            assert_eq!(card.context_window, None, "{}", row.id);
            assert_eq!(card.total_tokens, None, "{}", row.id);
            assert!(card.context.is_none(), "{}", row.id);
            continue;
        }
        let expected_window = if row.name == "claude" {
            1_000_000
        } else {
            272_000
        };
        assert_eq!(card.context_window, Some(expected_window), "{}", row.id);
        assert!(
            card.total_tokens
                .is_some_and(|total| total < expected_window),
            "{}",
            row.id,
        );
        let split = card.cache_read_input_tokens.unwrap_or(0)
            + card.cache_write_input_tokens.unwrap_or(0)
            + card.fresh_input_tokens.unwrap_or(0);
        assert_eq!(card.total_tokens, Some(split), "{}", row.id);
    }
}

#[test]
fn gallery_shimmer_leads_are_waiting_asks() {
    let cases: [(
        SidebarFixtureState,
        Option<(&str, rimz::agents::AgentStatus)>,
    ); 4] = [
        (
            SidebarFixtureState::Focus,
            Some((
                "agent:claude:rollout-reviewer",
                rimz::agents::AgentStatus::Waiting,
            )),
        ),
        (
            SidebarFixtureState::Cockpit,
            Some(("agent:opencode:theme", rimz::agents::AgentStatus::Waiting)),
        ),
        (
            SidebarFixtureState::Reach,
            Some(("agent:claude:netcheck", rimz::agents::AgentStatus::Waiting)),
        ),
        (SidebarFixtureState::Economy, None),
    ];

    for (state, expected) in cases {
        let snapshot = sidebar_fixture_snapshot(state).unwrap();
        let lead = rimz::lead_unread_row(&snapshot.worktree_groups);
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
        (
            SidebarFixtureState::Focus,
            "agent:claude:ci-paused",
            "API Error: Overloaded",
        ),
        (
            SidebarFixtureState::Cockpit,
            "agent:claude:mux-merge-paused",
            "API Error: Overloaded",
        ),
        (
            SidebarFixtureState::Cockpit,
            "agent:claude:observer",
            "API Error: Overloaded",
        ),
        (
            SidebarFixtureState::Reach,
            "agent:claude:netcheck-paused",
            "API Error: rate limit exceeded",
        ),
        (
            SidebarFixtureState::Economy,
            "agent:claude:limit-paused",
            "API Error: rate limit exceeded",
        ),
    ] {
        let snapshot = sidebar_fixture_snapshot(state).unwrap();
        assert_eq!(
            agent_card_by_id(&snapshot, id).turn_error_label.as_deref(),
            Some(label),
            "{state:?} {id}",
        );
    }
}

#[test]
fn gallery_fixture_frames_render_decisive_markers() {
    assert_fixture_frame_contains(
        SidebarFixtureState::Cockpit,
        &["stabilize render diff", "cargo nextest"],
    );
    assert_fixture_frame_lacks(SidebarFixtureState::Cockpit, &["away", "pnpm serve"]);
    assert_fixture_frame_contains(
        SidebarFixtureState::Focus,
        &["planner", "coder", "reviewer", "48ms"],
    );
    assert_fixture_frame_contains(
        SidebarFixtureState::Economy,
        &["OpenAI OAuth", "provider-ledger", "GPT 5.5", "pnpm serve"],
    );
    assert_fixture_frame_contains(
        SidebarFixtureState::Reach,
        &["remote-link", "edge-cache", "Claude Max", "network-check"],
    );
}

fn agent_card_by_id<'a>(snapshot: &'a rimz::SidebarSnapshot, id: &str) -> &'a rimz::AgentCard {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .find_map(|row| match &row.card {
            rimz::RowCard::Agent(card) if row.id == id => Some(card.as_ref()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("agent card {id}"))
}

fn agent_cards(snapshot: &rimz::SidebarSnapshot) -> Vec<&rimz::AgentCard> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter_map(|row| row.as_agent())
        .collect()
}

fn assert_fixture_frame_contains(state: SidebarFixtureState, markers: &[&str]) {
    let snapshot = sidebar_fixture_snapshot(state).unwrap();
    let mut ansi = Vec::new();
    rimz::sidebar_pane::render::render_fixed_line_ansi(&mut ansi, &snapshot, None, 80, 60).unwrap();
    let frame = strip_sgr(&ansi);
    for marker in markers {
        assert!(
            frame.contains(marker),
            "fixture {state:?} missing marker {marker:?}:\n{frame}",
        );
    }
}

fn assert_fixture_frame_lacks(state: SidebarFixtureState, markers: &[&str]) {
    let snapshot = sidebar_fixture_snapshot(state).unwrap();
    let mut ansi = Vec::new();
    rimz::sidebar_pane::render::render_fixed_line_ansi(&mut ansi, &snapshot, None, 80, 60).unwrap();
    let frame = strip_sgr(&ansi);
    for marker in markers {
        assert!(
            !frame.contains(marker),
            "fixture {state:?} unexpectedly contains marker {marker:?}:\n{frame}",
        );
    }
}
