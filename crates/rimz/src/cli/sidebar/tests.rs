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
    let focus = sidebar_fixture_snapshot(SidebarFixtureState::Focus).unwrap();
    let cards = agent_cards(&focus);
    let lead = cards
        .iter()
        .find(|card| card.handle.as_deref() == Some("coder"))
        .expect("coder card");
    assert_eq!(lead.sub_agents.len(), 5);
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
            .filter(|child| child.name == "general-purpose")
            .count(),
        2,
    );
    let handles = cards
        .iter()
        .filter_map(|card| card.handle.as_deref())
        .collect::<Vec<_>>();
    assert!(handles.contains(&"planner"));
    assert!(handles.contains(&"coder"));
    assert!(handles.contains(&"reviewer"));

    let economy = sidebar_fixture_snapshot(SidebarFixtureState::Economy).unwrap();
    assert_eq!(economy.providers.len(), 4);
    assert!(economy.theme.pets.enabled);
    assert_eq!(
        economy.theme.display.provider_tabs,
        rimz::config::ProviderTabsMode::Always,
    );

    let reach = sidebar_fixture_snapshot(SidebarFixtureState::Reach).unwrap();
    assert_eq!(reach.presence, Some(rimz::SidebarPresence::Detached));
    assert!(reach.link.is_some());
    assert_eq!(reach.theme.style, None);

    let cockpit = sidebar_fixture_snapshot(SidebarFixtureState::Cockpit).unwrap();
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
    assert!(statuses.contains(&rimz::agents::AgentStatus::Failed));
    assert!(statuses.contains(&rimz::agents::AgentStatus::Paused));
}

#[test]
fn gallery_fixture_frames_render_decisive_markers() {
    assert_fixture_frame_contains(
        SidebarFixtureState::Cockpit,
        &["opencode", "+182", "mux-merge"],
    );
    assert_fixture_frame_contains(
        SidebarFixtureState::Focus,
        &["coder", "Explore", "reviewer"],
    );
    assert_fixture_frame_contains(
        SidebarFixtureState::Economy,
        &["Claude", "Codex", "Opencode"],
    );
    assert_fixture_frame_contains(SidebarFixtureState::Reach, &["away", "48ms", "remote-link"]);
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
