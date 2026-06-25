use super::SidebarFixtureState;
use super::fixture::sidebar_fixture_snapshot;

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

    assert_eq!(
        states[0].theme.display.provider_tabs,
        rimz::config::ProviderTabsMode::Never,
    );
    for snapshot in &states[1..] {
        assert_eq!(
            snapshot.theme.display.provider_tabs,
            rimz::config::ProviderTabsMode::Always,
        );
    }
    assert_eq!(states[2].theme.pets.pet, "seedy");
    assert!(states[2].theme.pets.enabled);
    assert_eq!(states[3].theme.pets.pet, "rocky");
    assert!(states[3].theme.pets.enabled);
    assert!(states.iter().any(|snapshot| {
        snapshot
            .worktree_groups
            .iter()
            .flat_map(|group| &group.rows)
            .any(|row| row.unread)
    }));

    let focus = sidebar_fixture_snapshot(SidebarFixtureState::Focus).unwrap();
    let cards = agent_cards(&focus);
    let lead = cards
        .iter()
        .find(|card| card.handle.as_deref() == Some("coder"))
        .expect("coder card");
    assert_eq!(lead.sub_agents.len(), 6);
    assert_eq!(
        lead.sub_agents
            .iter()
            .filter(|child| child.name == "Explore")
            .count(),
        3,
    );
    assert_eq!(
        lead.sub_agents
            .iter()
            .filter(|child| child.name == "Plan")
            .count(),
        1,
    );
    assert_eq!(
        lead.sub_agents
            .iter()
            .filter(|child| child.name == "general-purpose")
            .count(),
        2,
    );
    assert!(lead.sub_agents.iter().all(|child| {
        child.name != "Explore" || child.status == rimz::agents::AgentStatus::Success
    }));
    assert!(lead.sub_agents.iter().any(|child| {
        child.name == "Plan" && child.status == rimz::agents::AgentStatus::Running
    }));
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

    let reach = &states[3];
    assert_eq!(reach.presence, Some(rimz::SidebarPresence::Detached));
    assert!(reach.link.is_some());

    let cockpit = &states[0];
    assert!(cockpit.worktree_groups.iter().any(|group| {
        group.landed == Some(true)
            && group.trunk_sync == Some(rimz::WorktreeTrunkSync::Merged)
            && group.pr_state == Some(rimz::WorktreePrState::Merged)
    }));
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
        card.status == Some(rimz::agents::AgentStatus::Running)
            && card.phase == rimz::agents::TurnPhase::Acting
    }));
    assert!(agent_cards(cockpit).iter().any(|card| {
        card.status == Some(rimz::agents::AgentStatus::Running)
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
        assert!((12..=30).contains(&live), "live count {live}");
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
        &["pricing-refresh", "mux-merge", "$3,990.00"],
    );
    assert_fixture_frame_contains(
        SidebarFixtureState::Focus,
        &["coder", "reviewer", "rollout-guard"],
    );
    assert_fixture_frame_contains(
        SidebarFixtureState::Economy,
        &["OpenAI OAuth", "usage-alerts", "GPT 5.5"],
    );
    assert_fixture_frame_contains(SidebarFixtureState::Reach, &["away", "48ms", "vpn-check"]);
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
