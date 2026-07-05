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
