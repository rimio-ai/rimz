use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rimz::address::{AddressContext, ChannelOrigin, resolve_many};
use rimz::agents::{AgentState, AgentStatus};
use rimz::config::ThemeConfig;
use rimz::ids::{MuxClientId, PaneId};
use rimz::mux::ClientView;
use rimz::pane::ClientPaneView;
use rimz::store::snapshot::{
    AgentCard, ProcessCard, RowCard, SidebarRow, SidebarSnapshot, SidebarSubAgent,
    SidebarWorktreeGroup,
};

use super::model::*;
use super::*;

fn pane(id: u32) -> PaneId {
    PaneId::from_parts(rimz::MuxName::Tmux, format!("%{id}"))
}

fn row(id: u32, handle: Option<&str>) -> SidebarRow {
    SidebarRow {
        id: id.to_string(),
        name: "codex".to_owned(),
        pane: Some(
            serde_json::from_value(serde_json::json!({
                "pane_id": pane(id), "session_name": "room"
            }))
            .unwrap(),
        ),
        worktree_path: Some("/project/feature".to_owned()),
        checkout: None,
        worktree_branch: Some("feature/keyboard".to_owned()),
        channel: Some("feature".to_owned()),
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        attention_at_ms: None,
        last_activity: jiff::Timestamp::UNIX_EPOCH,
        card: match handle {
            Some(handle) => RowCard::Agent(
                AgentCard {
                    handle: Some(handle.to_owned()),
                    description: Some("ship the popup".to_owned()),
                    status: AgentStatus::Running,
                    ..Default::default()
                }
                .into(),
            ),
            None => RowCard::Process(ProcessCard::default()),
        },
    }
}

fn group(rows: Vec<SidebarRow>) -> SidebarWorktreeGroup {
    serde_json::from_value(serde_json::json!({
        "key": "feature", "label": "feature", "kind": "worktree",
        "rows": rows, "status_counts": [], "pr_number": 42
    }))
    .unwrap()
}

fn agent(id: u32, profile: Option<&str>, lane: Option<&str>) -> AgentState {
    let mut agent =
        rimz::testkit::agent_state("codex", &id.to_string(), jiff::Timestamp::UNIX_EPOCH);
    agent.pane = row(id, None).pane;
    agent.profile = profile.map(str::to_owned);
    agent.name = Some(format!("pet-{id}"));
    agent.channel = lane.map(str::to_owned);
    agent.root_lane = lane.is_none();
    agent
}

fn child(id: u32) -> SidebarSubAgent {
    serde_json::from_value(serde_json::json!({
        "id": id.to_string(), "kind": "codex", "pane": pane(id),
        "name": "review", "status": AgentStatus::Running,
        "last_activity": jiff::Timestamp::UNIX_EPOCH
    }))
    .unwrap()
}

fn snapshot(agents: Vec<AgentState>, rows: Vec<SidebarRow>) -> SidebarSnapshot {
    let mut snapshot = SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
        agents,
        jiff::Timestamp::UNIX_EPOCH,
    );
    snapshot.worktree_groups = vec![group(rows)].into();
    snapshot
}

/// Whatever lane the child inherits, its target must select this agent and no other.
fn assert_selects(snapshot: &SidebarSnapshot, card: &Candidate) {
    for channel in [None, Some("feature"), Some("other")] {
        let reader = AddressContext {
            channel: channel.map(str::to_owned),
            origin: ChannelOrigin::Stamped,
        };
        let matches = resolve_many(snapshot, &card.target, None, &reader).unwrap();
        assert_eq!(
            matches
                .iter()
                .map(|a| a.agent_id.as_str())
                .collect::<Vec<_>>(),
            [card.id.as_str()],
            "{} from {channel:?}",
            card.target
        );
    }
}

fn cards() -> Vec<Candidate> {
    [(1, "coder"), (2, "reviewer")]
        .into_iter()
        .map(|(id, name)| Candidate {
            id: pane(id).to_string(),
            pane: pane(id),
            target: format!("@{name}#feature"),
            agent: true,
            compact: true,
            label: format!("@{name}#feature"),
            name: String::new(),
            kind: "codex".to_owned(),
            status: "running".to_owned(),
            description: "ship the popup".to_owned(),
            worktree: "feature".to_owned(),
            branch: "feature/keyboard".to_owned(),
            pr: Some(42),
            group_key: "feature".to_owned(),
        })
        .collect()
}

#[test]
fn query_prefix_alone_controls_mode_and_erasing_it_returns_to_cards() {
    for (query, expected) in [
        ("", Mode::Cards),
        ("coder", Mode::Cards),
        (">restart", Mode::Commands),
        ("#feature", Mode::Worktrees),
        ("+coder", Mode::New),
        ("a>b", Mode::Cards),
    ] {
        assert_eq!(Mode::from_query(query), expected, "{query}");
    }
    let mut state = PaletteState::new(Mode::Commands, None, "room");
    assert_eq!(state.query, ">");
    state.set_query(String::new());
    assert_eq!(state.mode(), Mode::Cards);
}

#[test]
fn fuzzy_subsequence_weights_handle_before_description_and_keeps_ties() {
    assert!(fuzzy_score("cpe", "card popup engine").is_some());
    assert!(fuzzy_score("cxz", "card popup engine").is_none());
    let mut source = cards();
    source[0].description = "reviewer".to_owned();
    let mut state = PaletteState::new(Mode::Cards, None, "room");
    state.refresh(source.clone(), true);
    state.set_query("rvw".to_owned());
    assert_eq!(
        state
            .entries()
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        [source[1].pane.as_str(), source[0].pane.as_str()]
    );
    state.set_query("codex".to_owned());
    assert_eq!(
        state
            .entries()
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        [source[0].pane.as_str(), source[1].pane.as_str()]
    );
}

#[test]
fn every_card_search_field_matches() {
    let source = cards().into_iter().take(1).collect();
    let mut state = PaletteState::new(Mode::Cards, None, "room");
    state.refresh(source, true);
    for query in ["cdr", "cdx", "ftr", "ftr/kyb", "42", "shppp"] {
        state.set_query(query.to_owned());
        assert_eq!(state.entries().len(), 1, "{query}");
    }
}

#[test]
fn candidates_admit_processes_and_pane_children_but_not_paneless_rows() {
    let mut parent = row(1, Some("coder"));
    let mut native = child(5);
    native.pane = None;
    parent.as_agent_mut().unwrap().sub_agents = vec![child(3), native];
    let mut closed = row(4, Some("closed"));
    closed.pane = None;
    let snapshot = snapshot(
        vec![
            agent(1, Some("coder"), Some("feature")),
            agent(3, None, Some("feature")),
        ],
        vec![parent, row(2, None), closed],
    );
    let result = candidates(&snapshot);
    assert_eq!(result.len(), 3);
    assert_eq!(
        result.iter().map(|c| c.agent).collect::<Vec<_>>(),
        [true, true, false]
    );
    assert_eq!(result[2].target, "tmux:%2");
    assert_selects(&snapshot, &result[0]);
    assert_selects(&snapshot, &result[1]);
}

#[test]
fn agents_sharing_a_handle_in_one_lane_get_targets_that_select_each() {
    let snapshot = snapshot(
        vec![
            agent(1, Some("coder"), Some("feature")),
            agent(2, Some("coder"), Some("feature")),
        ],
        vec![row(1, Some("coder")), row(2, Some("coder"))],
    );
    let source = candidates(&snapshot);
    assert_ne!(source[0].target, source[1].target);
    for card in &source {
        assert_selects(&snapshot, card);
    }
}

#[test]
fn a_root_lane_agent_is_named_by_lane_beside_its_namesake() {
    let mut root = row(1, Some("coder"));
    root.channel = None;
    let snapshot = snapshot(
        vec![
            agent(1, Some("coder"), None),
            agent(2, Some("coder"), Some("feature")),
        ],
        vec![root, row(2, Some("coder"))],
    );
    let source = candidates(&snapshot);
    assert_eq!(source[0].target, "@coder#main");
    assert_eq!(source[1].target, "@coder#feature");
    for card in &source {
        assert_selects(&snapshot, card);
    }
}

#[test]
fn a_wired_pane_before_its_session_is_a_pane_only_card() {
    let mut wired = row(1, Some("coder"));
    wired.id = pane(1).to_string();
    wired.as_agent_mut().unwrap().handle = None;
    let source = candidates(&snapshot(Vec::new(), vec![wired]));
    assert_eq!(source[0].target, "tmux:%1");
    assert_eq!(source[0].label, "codex");
    assert!(!source[0].agent);
    assert_eq!(command_entries(Some(&source[0]), "room").len(), 21);
}

#[test]
fn bare_kind_agents_are_searchable_by_pet_name_and_keep_agent_commands() {
    let mut bare = row(1, Some("coder"));
    bare.as_agent_mut().unwrap().handle = None;
    let snapshot = snapshot(vec![agent(1, None, Some("feature"))], vec![bare]);
    let source = candidates(&snapshot);
    assert_selects(&snapshot, &source[0]);
    assert!(source[0].agent);
    assert_eq!(source[0].name, "pet-1");
    let mut state = PaletteState::new(Mode::Cards, None, "room");
    state.refresh(source, true);
    state.set_query("pet-1".to_owned());
    assert_eq!(state.entries().len(), 1);
    assert!(state.entries()[0].detail.starts_with("pet-1  "));
}

#[test]
fn a_card_with_no_description_searches_its_task_then_first_prompt() {
    for (task, prompt, expected) in [
        (
            Some("port the store"),
            Some("migrate database"),
            "port the store",
        ),
        (None, Some("migrate database"), "migrate database"),
    ] {
        let mut card = row(1, Some("coder"));
        let fields = card.as_agent_mut().unwrap();
        fields.description = None;
        fields.task = task.map(str::to_owned);
        fields.first_prompt = prompt.map(str::to_owned);
        let source = candidates(&snapshot(
            vec![agent(1, Some("coder"), Some("feature"))],
            vec![card],
        ));
        assert_eq!(source[0].description, expected);
    }
}

#[test]
fn a_child_in_another_lane_shows_its_own_checkout_not_its_parents() {
    let mut parent = row(1, Some("coder"));
    parent.as_agent_mut().unwrap().sub_agents = vec![child(3), child(4)];
    let mut away = agent(3, None, Some("review"));
    away.worktree_branch = Some("review/branch".to_owned());
    let mut home = agent(4, None, Some("feature"));
    home.worktree_branch = Some("feature/child".to_owned());
    let snapshot = snapshot(
        vec![agent(1, Some("coder"), Some("feature")), away, home],
        vec![parent],
    );
    let source = candidates(&snapshot);
    assert_selects(&snapshot, &source[1]);
    assert_eq!(
        (source[1].worktree.as_str(), source[1].branch.as_str()),
        ("review", "review/branch")
    );
    assert_eq!(source[1].pr, None);
    assert_ne!(source[1].group_key, source[0].group_key);
    assert_eq!(
        (source[2].worktree.as_str(), source[2].branch.as_str()),
        ("feature", "feature/child")
    );
    assert_eq!(source[2].pr, Some(42));
    assert_eq!(source[2].group_key, source[0].group_key);

    let mut with_home = snapshot.clone();
    let mut namesake = group(Vec::new());
    namesake.key = "/other/checkout".to_owned();
    namesake.label = "review".to_owned();
    namesake.pr_number = Some(8);
    let mut review = group(Vec::new());
    review.key = "channel:review".to_owned();
    review.label = "review".to_owned();
    review.pr_number = Some(7);
    std::sync::Arc::make_mut(&mut with_home.worktree_groups).extend([namesake, review]);
    let source = candidates(&with_home);
    assert_eq!(source[1].worktree, "review");
    assert_eq!(source[1].pr, Some(7));
    assert_eq!(source[1].group_key, "channel:review");
}

#[test]
fn a_child_at_the_root_joins_the_root_group_under_its_branch_label() {
    let mut parent = row(1, Some("coder"));
    parent.as_agent_mut().unwrap().sub_agents = vec![child(3)];
    let mut snapshot = snapshot(
        vec![
            agent(1, Some("coder"), Some("feature")),
            agent(3, None, None),
            agent(5, Some("lead"), None),
        ],
        vec![parent],
    );
    let mut root = group(vec![row(5, Some("lead"))]);
    root.key = "/project".to_owned();
    root.label = "master".to_owned();
    root.root_lane = true;
    root.pr_number = Some(7);
    std::sync::Arc::make_mut(&mut snapshot.worktree_groups).push(root);

    let source = candidates(&snapshot);
    let at_root = source.iter().find(|card| card.id == "3").unwrap();
    assert_eq!(
        (at_root.worktree.as_str(), at_root.group_key.as_str()),
        ("master", "/project")
    );
    assert_eq!(at_root.pr, Some(7));

    let mut state = PaletteState::new(Mode::Worktrees, None, "room");
    state.refresh(source, true);
    assert_eq!(
        state.entries().len(),
        2,
        "feature and master, no second root"
    );
}

#[test]
fn compact_is_offered_only_to_kinds_it_can_compact() {
    let mut source = cards();
    source[0].compact = false;
    assert!(
        !command_entries(Some(&source[0]), "room")
            .iter()
            .any(|entry| entry.label.contains("compact"))
    );
    let result = candidates(&snapshot(
        vec![agent(1, Some("coder"), Some("feature"))],
        vec![row(1, Some("coder"))],
    ));
    let spec = rimz::agents::spec_by_kind("codex").unwrap();
    assert_eq!(result[0].compact, spec.launch.compact_command.is_some());
}

#[test]
fn commands_parse_through_real_clap_and_respect_target_class() {
    let source = cards();
    assert!(!source.is_empty(), "pane-backed agents must be candidates");
    let agent = command_entries(Some(&source[0]), "room");
    let mut process_card = source[0].clone();
    process_card.agent = false;
    process_card.target = process_card.pane.to_string();
    let process = command_entries(Some(&process_card), "room");
    let room = command_entries(None, "room");
    assert_eq!((agent.len(), process.len(), room.len()), (26, 21, 19));
    for entry in agent.iter().chain(&process).chain(&room) {
        let action = entry.action.as_ref().unwrap();
        let parsed = crate::cli::Cli::try_parse_from(
            std::iter::once("rimz").chain(action.args.iter().map(String::as_str)),
        );
        assert!(parsed.is_ok(), "{}: {parsed:?}", action.command_line());
    }
    assert!(!process.iter().any(|e| e.label.contains("restart")));
    assert!(
        !room
            .iter()
            .any(|e| e.label.contains("capture") || e.label.contains("focus"))
    );
    assert_eq!(
        agent
            .iter()
            .filter(|e| e.action.as_ref().unwrap().effect == Effect::Act)
            .count(),
        17
    );
    assert_eq!(
        process
            .iter()
            .filter(|e| e.action.as_ref().unwrap().effect == Effect::Act)
            .count(),
        14
    );
}

#[test]
fn sidebar_navigation_is_a_room_action_independent_of_the_palette_target() {
    let source = cards();
    let mut process = source[0].clone();
    process.agent = false;
    let mut expected = vec![
        "rimz sidebar walk next --session-name room".to_owned(),
        "rimz sidebar walk prev --session-name room".to_owned(),
    ];
    expected.extend((1..=9).map(|n| format!("rimz sidebar slot {n} --session-name room")));
    expected.push("rimz sidebar last --session-name room".to_owned());
    let expected = expected
        .into_iter()
        .map(|label| (label, Effect::Act))
        .collect::<Vec<_>>();
    let actual = [Some(&source[0]), Some(&process), None].map(|target| {
        command_entries(target, "room")
            .into_iter()
            .filter_map(|entry| {
                let action = entry.action.unwrap();
                (action.args[0] == "sidebar" && action.args[1] != "repair")
                    .then_some((entry.label, action.effect))
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(actual, [expected.clone(), expected.clone(), expected]);
}

#[test]
fn sidebar_navigation_runs_the_displayed_command_in_the_palettes_room() {
    let mut state = PaletteState::new(Mode::Cards, Some(pane(1)), "other room");
    state.refresh(cards(), true);
    state.move_selection(1);
    let mut actual = Vec::new();
    for query in ["walk next", "walk prev", "slot 9", "last"] {
        state.set_query(format!(">sidebar {query}"));
        let footer = state.footer();
        state.activate(|action| {
            assert_eq!(
                shlex::split(footer.trim_start_matches("$ ")).unwrap()[1..],
                action.args
            );
            assert_eq!(action.effect, Effect::Act);
            ChildOutcome::Success
        });
        actual.push((footer, state.closed));
    }
    assert_eq!(
        actual,
        ["walk next", "walk prev", "slot 9", "last"].map(|query| (
            format!("$ rimz sidebar {query} --session-name 'other room'"),
            true
        ))
    );
}

#[test]
fn command_target_is_moved_card_then_opener_else_none() {
    let source = cards();
    let mut state = PaletteState::new(Mode::Cards, Some(pane(1)), "room");
    state.refresh(source, true);
    state.set_query(">".to_owned());
    assert_eq!(
        state.target().map(|c| c.target.as_str()),
        Some("@coder#feature")
    );
    state.set_query(String::new());
    state.move_selection(1);
    state.set_query(">".to_owned());
    assert_eq!(
        state.target().map(|c| c.target.as_str()),
        Some("@reviewer#feature")
    );
    let mut departed = PaletteState::new(Mode::Cards, None, "room");
    departed.refresh(cards(), true);
    departed.move_selection(1);
    let moved = departed.target().unwrap().id.clone();
    let mut remaining = cards();
    remaining.retain(|card| card.id != moved);
    departed.refresh(remaining, true);
    assert!(
        departed.target().is_none(),
        "a departed target never passes to another card"
    );
    let mut absent = PaletteState::new(Mode::Commands, Some(pane(99)), "room");
    absent.refresh(cards(), true);
    assert!(absent.target().is_none());
    assert_eq!(absent.entries().len(), 19);
    assert!(
        PaletteState::new(Mode::Commands, None, "room")
            .target()
            .is_none()
    );
}

#[test]
fn opening_on_commands_selects_the_first_row_of_the_opener_card() {
    let mut state = PaletteState::new(Mode::Commands, Some(pane(1)), "room");
    state.refresh(cards(), true);
    assert_eq!(state.selected, 0);
    assert_eq!(state.footer(), "$ rimz pane focus @coder#feature");
}

#[test]
fn commands_opened_before_any_card_arrives_land_on_the_opener_card() {
    for published in [false, true] {
        let mut state = PaletteState::new(Mode::Commands, Some(pane(1)), "room");
        state.refresh(Vec::new(), published);
        state.refresh(cards(), true);
        assert_eq!(
            state.footer(),
            "$ rimz pane focus @coder#feature",
            "first read published: {published}"
        );
    }
}

#[test]
fn command_lines_quote_only_what_a_shell_would_change() {
    let line = |args: &[&str]| {
        Action {
            args: args.iter().map(|word| (*word).to_owned()).collect(),
            effect: Effect::Act,
        }
        .command_line()
    };
    assert_eq!(
        line(&["pane", "focus", "tmux:%1"]),
        "rimz pane focus tmux:%1"
    );
    assert_eq!(
        line(&["agents", "list", "#auth"]),
        "rimz agents list '#auth'"
    );
    assert_eq!(line(&["message", "two words"]), "rimz message 'two words'");
}

#[test]
fn selection_survives_refresh_by_identity_or_pane_and_falls_to_top() {
    let mut state = PaletteState::new(Mode::Cards, None, "room");
    let mut source = cards();
    state.refresh(source.clone(), true);
    assert_eq!(state.entries().len(), 2);
    state.move_selection(1);
    let selected = state.entries()[state.selected].id.clone();
    source.reverse();
    state.refresh(source.clone(), true);
    assert_eq!(state.entries()[state.selected].id, selected);
    source.remove(0);
    state.refresh(source, true);
    assert_eq!(state.selected, 0);
    state.refresh(cards(), true);
    state.move_selection(1);
    let mut replacement = cards();
    replacement[1].id = "new-session".to_owned();
    state.refresh(replacement.clone(), true);
    assert_eq!(state.entries()[state.selected].id, "new-session");
    assert_eq!(state.target().unwrap().id, "new-session");
    replacement[1].pane = pane(9);
    replacement[1].id = "elsewhere".to_owned();
    state.refresh(replacement, true);
    assert_eq!(state.selected, 0, "a card on another pane is another card");
    assert!(state.target().is_none());
}

#[test]
fn footer_is_exactly_the_action_passed_to_runner_in_each_mode() {
    for mode in [Mode::Cards, Mode::Commands, Mode::Worktrees] {
        let mut state = PaletteState::new(mode, Some(pane(1)), "room");
        state.refresh(cards(), true);
        let footer = state.footer();
        assert!(footer.starts_with("$ rimz "), "{mode:?}: {footer}");
        let mut ran = false;
        state.activate(|action| {
            assert_eq!(footer, format!("$ {}", action.command_line()));
            assert_eq!(
                shlex::split(footer.trim_start_matches("$ ")).unwrap()[1..],
                action.args
            );
            ran = true;
            ChildOutcome::Success
        });
        assert!(ran);
    }
    let mut state = PaletteState::new(Mode::New, None, "room");
    assert_eq!(state.entries().len(), 1);
    assert_eq!(state.footer(), "nothing to run");
    state.activate(|_| panic!("placeholder must not run"));
    assert!(!state.closed);
}

#[test]
fn action_failure_keeps_palette_open_and_success_closes_it() {
    let mut state = PaletteState::new(Mode::Cards, None, "room");
    state.refresh(cards(), true);
    state.activate(|_| ChildOutcome::Failure("exit 3: pane is gone\nmore detail".to_owned()));
    assert!(!state.closed);
    assert_eq!(state.notice.as_deref(), Some("exit 3: pane is gone"));
    state.activate(|_| ChildOutcome::Success);
    assert!(state.closed);
}

#[test]
fn missing_publication_has_no_candidates_and_names_repair() {
    let mut state = PaletteState::new(Mode::Cards, None, "room");
    state.refresh(Vec::new(), false);
    assert!(state.entries().is_empty());
    assert_eq!(
        state.notice_text(),
        Some("no sidebar has published this room yet; run `rimz sidebar repair`")
    );
}

#[test]
fn terminal_refusal_names_the_popup_escape() {
    assert_eq!(
        require_terminal(false, false).unwrap_err().to_string(),
        "rimz palette needs a terminal; run `rimz palette --popup`"
    );
    assert!(require_terminal(false, true).is_ok());
    assert!(require_terminal(true, false).is_ok());
}

fn clients(names: &[&str]) -> ClientView {
    ClientView {
        clients: names
            .iter()
            .map(|name| ClientPaneView {
                client_id: MuxClientId::Tmux((*name).to_owned()),
                pane_id: pane(1),
            })
            .collect(),
        ..Default::default()
    }
}

#[test]
fn tmux_client_refusals_describe_zero_and_multiple_attached_clients() {
    assert_eq!(
        popup_client("room", None, &clients(&[]))
            .unwrap_err()
            .to_string(),
        "no client is attached to session room; attach with `rimz attach`"
    );
    assert_eq!(
        popup_client("room", None, &clients(&["a", "b"]))
            .unwrap_err()
            .to_string(),
        "several clients are attached to session room; pass --client <name>"
    );
    assert_eq!(popup_client("room", None, &clients(&["a"])).unwrap(), "a");
    assert_eq!(popup_client("room", Some("b"), &clients(&[])).unwrap(), "b");
}

#[test]
fn launcher_preserves_mode_room_mux_and_opener_without_popup_flag() {
    let args = PaletteArgs {
        mode: Mode::Commands,
        session_name: Some("room".to_owned()),
        from_pane: Some(pane(1)),
        popup: true,
        client: Some("a".to_owned()),
    };
    assert_eq!(
        popup_args(&args, "room", rimz::MuxName::Tmux, Some(&pane(1))),
        [
            "palette",
            "--mode",
            "commands",
            "--session-name",
            "room",
            "--mux",
            "tmux",
            "--from-pane",
            "tmux:%1",
        ]
    );
}

#[test]
fn renders_complete_cards_selection_and_command_at_normal_and_minimum_size() {
    for (width, height) in [(80, 24), (32, 8)] {
        for no_color in [false, true] {
            let theme = PaletteTheme::resolve(&ThemeConfig::default(), true, no_color);
            let mut state = PaletteState::new(Mode::Cards, None, "room");
            let mut source = cards();
            if width == 32 {
                source[0].target = "@coder-with-a-long-handle#feature".to_owned();
            }
            state.refresh(source, true);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, &state, &theme))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let lines = (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>();
            let screen = lines.join("\n");
            assert!(screen.contains('▶'), "{screen}");
            assert!(lines.last().unwrap().contains("rimz"), "{screen}");
            if width == 32 {
                assert!(state.footer().len() > usize::from(width));
            }
            if width == 80 {
                assert!(screen.contains("@coder#feature"), "{screen}");
                assert!(screen.contains("running"), "{screen}");
                assert_eq!(screen.matches("#42").count(), 2, "{screen}");
                assert_eq!(screen.matches("feature/keyboard").count(), 2, "{screen}");
                assert_eq!(screen.matches("ship the popup").count(), 2, "{screen}");
            }
            if no_color {
                assert!(
                    buffer
                        .content
                        .iter()
                        .all(|cell| cell.fg == ratatui::style::Color::Reset
                            && cell.bg == ratatui::style::Color::Reset)
                );
            } else {
                assert!(
                    buffer
                        .content
                        .iter()
                        .any(|cell| cell.fg != ratatui::style::Color::Reset)
                );
            }
        }
    }
}

#[test]
fn command_target_and_new_placeholder_render_without_inventing_an_action() {
    for mode in [Mode::Commands, Mode::New] {
        let state = PaletteState::new(mode, None, "room");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let theme = PaletteTheme::resolve(&ThemeConfig::default(), false, true);
        terminal
            .draw(|frame| render(frame, &state, &theme))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            text.contains(if mode == Mode::Commands {
                "no card"
            } else {
                "later release"
            }),
            "{text}"
        );
    }
}
