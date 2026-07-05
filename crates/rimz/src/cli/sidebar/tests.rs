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
        .map(str::trim_end)
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
            SidebarFixtureState::Cockpit,
            SidebarFixtureState::Focus,
            SidebarFixtureState::Reach,
            SidebarFixtureState::Economy,
        ],
    );

    for ((state, selector), (expected_id, expected_kind)) in columns.into_iter().zip([
        ("agent:claude:compacting", "claude"),
        ("agent:codex:coder", "codex"),
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
        assert!(selected_index > 0, "{state:?} selected top row");
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
fn gallery_fixture_states_carry_feature_flags() {
    let states = [
        SidebarFixtureState::Cockpit,
        SidebarFixtureState::Focus,
        SidebarFixtureState::Economy,
        SidebarFixtureState::Reach,
    ]
    .into_iter()
    .map(|state| sidebar_fixture_snapshot(state).unwrap())
    .collect::<Vec<_>>();
    assert!(states.iter().all(|snapshot| !snapshot.providers.is_empty()));

    for snapshot in &states {
        assert_eq!(
            snapshot.theme.display.provider_tabs,
            rimz::config::ProviderTabsMode::Always,
        );
    }
    assert!(states.iter().all(|snapshot| !snapshot.theme.pets.enabled));
    assert!(states.iter().any(|snapshot| {
        snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .any(|row| row.unread)
    }));
    let lead_kinds = states.iter().map(top_agent_kind).collect::<Vec<_>>();
    assert_eq!(
        lead_kinds,
        vec![Some("claude"), Some("pi"), Some("opencode"), Some("claude")]
    );

    let focus = sidebar_fixture_snapshot(SidebarFixtureState::Focus).unwrap();
    let cards = agent_cards(&focus);
    let planner = cards
        .iter()
        .find(|card| card.handle.as_deref() == Some("planner"))
        .expect("planner card");
    assert_eq!(planner.status, rimz::agents::AgentStatus::Running);
    assert_eq!(planner.phase, rimz::agents::TurnPhase::Reasoning);
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
    assert!(planner.sub_agents.iter().any(|child| {
        child.name == "Plan" && child.status == rimz::agents::AgentStatus::Running
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
    assert!(cockpit.link.is_some());
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

    for snapshot in &states {
        let live = snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.status_counts)
            .map(|count| count.count)
            .sum::<usize>();
        assert!((12..=42).contains(&live), "live count {live}");
        for group in snapshot
            .worktree_groups
            .iter()
            .filter(|group| group.hidden_count > 0)
        {
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
            assert_eq!(counted_idle, rendered_idle + group.hidden_count);
        }
        let statuses = snapshot
            .worktree_groups
            .iter()
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
            assert!(statuses.contains(&status), "missing {status:?}");
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
        .filter(|row| matches!(row.name.as_str(), "pi" | "opencode"))
    {
        let rimz::RowCard::Agent(card) = &row.card else {
            continue;
        };
        assert_eq!(card.model.as_deref(), Some("GPT-5.5"));
    }
}

#[test]
fn gallery_fixture_frames_render_decisive_markers() {
    assert_fixture_frame_contains(
        SidebarFixtureState::Cockpit,
        &["stabilize render diff", "48ms", "pnpm serve"],
    );
    assert_fixture_frame_lacks(SidebarFixtureState::Cockpit, &["away"]);
    assert_fixture_frame_contains(
        SidebarFixtureState::Focus,
        &["planner", "coder", "reviewer"],
    );
    assert_fixture_frame_contains(
        SidebarFixtureState::Economy,
        &["OpenAI OAuth", "provider-ledger", "GPT 5.5"],
    );
    assert_fixture_frame_contains(
        SidebarFixtureState::Reach,
        &["remote-link", "edge-cache", "Claude Max"],
    );
}

fn agent_cards(snapshot: &rimz::SidebarSnapshot) -> Vec<&rimz::AgentCard> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter_map(|row| match &row.card {
            rimz::RowCard::Agent(card) => Some(card.as_ref()),
            rimz::RowCard::Process(_) => None,
        })
        .collect()
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

fn top_agent_kind(snapshot: &rimz::SidebarSnapshot) -> Option<&str> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .find_map(|row| match &row.card {
            rimz::RowCard::Agent(_) => Some(row.name.as_str()),
            rimz::RowCard::Process(_) => None,
        })
}

fn assert_fixture_frame_contains(state: SidebarFixtureState, markers: &[&str]) {
    let snapshot = sidebar_fixture_snapshot(state).unwrap();
    let mut ansi = Vec::new();
    rimz::sidebar_pane::render::render_fixed_line_ansi(&mut ansi, &snapshot, None, 80, 42).unwrap();
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
    rimz::sidebar_pane::render::render_fixed_line_ansi(&mut ansi, &snapshot, None, 80, 42).unwrap();
    let frame = strip_sgr(&ansi);
    for marker in markers {
        assert!(
            !frame.contains(marker),
            "fixture {state:?} unexpectedly contains marker {marker:?}:\n{frame}",
        );
    }
}
