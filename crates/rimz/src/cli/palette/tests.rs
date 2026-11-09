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

use super::launch::*;
use super::model::*;
use super::*;
use std::path::PathBuf;

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
            checkout: None,
        })
        .collect()
}

fn setup() -> LaunchSetup {
    LaunchSetup {
        profiles: ["coder", "reviewer"]
            .map(|name| Profile {
                name: name.to_owned(),
                model: "gpt-5".to_owned(),
                description: format!("{name} profile"),
            })
            .into(),
        root: PathBuf::from("/project"),
        trees: Some(
            ["feature", "docs"]
                .map(|name| Tree {
                    name: name.to_owned(),
                    path: PathBuf::from(format!("/project-worktrees/{name}")),
                    branch: format!("{name}/branch"),
                })
                .into(),
        ),
        unavailable: None,
        new_worktree: false,
    }
}

fn launch_state(setup: LaunchSetup, checkout: Option<&str>) -> PaletteState {
    let mut state = PaletteState::new(Mode::New, Some(pane(1)), "room").with_launch(setup);
    let mut source = cards();
    source[0].checkout = checkout.map(PathBuf::from);
    state.refresh(source, true);
    state
}

fn type_text(state: &mut PaletteState, text: &str) {
    for character in text.chars() {
        state.type_char(character);
    }
}

fn labels(state: &PaletteState) -> Vec<String> {
    state
        .entries()
        .into_iter()
        .map(|entry| entry.label)
        .collect()
}

/// What a printed launch parses to: profile, `-w` value, `--from-trunk`, `--cwd`, prompt.
type Parsed = (
    String,
    Option<String>,
    bool,
    Option<PathBuf>,
    Option<String>,
);

fn parse_launch(args: &[String]) -> Parsed {
    let matches = <crate::cli::Cli as clap::CommandFactory>::command()
        .try_get_matches_from(std::iter::once("rimz").chain(args.iter().map(String::as_str)))
        .unwrap_or_else(|error| panic!("{args:?}: {error}"));
    let launch = matches
        .subcommand_matches("agents")
        .and_then(|agents| agents.subcommand_matches("launch"))
        .unwrap_or_else(|| panic!("{args:?} is not an agents launch"));
    (
        launch
            .get_one::<String>("spec")
            .cloned()
            .unwrap_or_default(),
        launch.get_one::<String>("worktree").cloned(),
        launch.get_flag("from_trunk"),
        launch.get_one::<PathBuf>("cwd").cloned(),
        launch.get_one::<String>("prompt").cloned(),
    )
}

struct Walk {
    checkout: Option<&'static str>,
    new_worktree: bool,
    place: usize,
    name: &'static str,
    prompt: &'static str,
    parsed: Parsed,
}

fn walks() -> Vec<Walk> {
    let parsed = |worktree: Option<&str>, from_trunk, cwd: Option<&str>, prompt: Option<&str>| {
        (
            "coder".to_owned(),
            worktree.map(str::to_owned),
            from_trunk,
            cwd.map(PathBuf::from),
            prompt.map(str::to_owned),
        )
    };
    vec![
        Walk {
            checkout: Some("/project"),
            new_worktree: false,
            place: 0,
            name: "",
            prompt: "",
            parsed: parsed(None, false, None, None),
        },
        Walk {
            checkout: Some("/project-worktrees/feature"),
            new_worktree: false,
            place: 0,
            name: "",
            prompt: "fix the login",
            parsed: parsed(Some("feature"), false, None, Some("fix the login")),
        },
        Walk {
            checkout: Some("/elsewhere"),
            new_worktree: false,
            place: 0,
            name: "",
            prompt: "it's \"quoted\" -w x",
            parsed: parsed(
                None,
                false,
                Some("/elsewhere"),
                Some("it's \"quoted\" -w x"),
            ),
        },
        Walk {
            checkout: None,
            new_worktree: false,
            place: 2,
            name: "",
            prompt: "",
            parsed: parsed(Some("docs"), false, None, None),
        },
        Walk {
            checkout: None,
            new_worktree: true,
            place: 3,
            name: "feat/x",
            prompt: "ship it",
            parsed: parsed(Some("feat/x"), true, None, Some("ship it")),
        },
        Walk {
            checkout: None,
            new_worktree: true,
            place: 3,
            name: "",
            prompt: "two words",
            parsed: parsed(Some(""), true, None, Some("two words")),
        },
    ]
}

/// The palette at each step of `walk`, the last one ready to launch.
fn walk_states(walk: &Walk) -> Vec<PaletteState> {
    let mut setup = setup();
    setup.new_worktree = walk.new_worktree;
    let mut state = launch_state(setup, walk.checkout);
    let mut states = vec![state.clone()];
    assert!(!state.advance());
    state.selected = walk.place;
    states.push(state.clone());
    assert!(!state.advance());
    if state.launch_step() == Some(Step::Name) {
        type_text(&mut state, walk.name);
        states.push(state.clone());
        assert!(!state.advance());
    }
    assert_eq!(state.launch_step(), Some(Step::Prompt));
    type_text(&mut state, walk.prompt);
    states.push(state.clone());
    states
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
    for walk in walks() {
        for mut state in walk_states(&walk) {
            let footer = state.footer();
            assert!(footer.starts_with("$ rimz agents launch coder"), "{footer}");
            let mut ran = false;
            state.activate(|action| {
                assert_eq!(footer, format!("$ {}", action.command_line()));
                assert_eq!(
                    shlex::split(footer.trim_start_matches("$ ")).unwrap()[1..],
                    action.args
                );
                assert_eq!(action.effect, Effect::Launch);
                ran = true;
                ChildOutcome::Success
            });
            assert!(ran && state.closed, "{footer}");
        }
    }
}

#[test]
fn launch_commands_parse_to_the_selected_profile_place_and_prompt() {
    for walk in walks() {
        let states = walk_states(&walk);
        let action = states.last().unwrap().selected_action().unwrap();
        assert_eq!(
            parse_launch(&action.args),
            walk.parsed,
            "{}",
            action.command_line()
        );
    }
}

#[test]
fn where_step_lists_current_then_root_then_trees_then_new() {
    let new = "new worktree from trunk";
    for (checkout, new_worktree, expected, selected) in [
        (
            Some("/project-worktrees/feature"),
            false,
            vec!["feature", "project", "docs", new],
            0,
        ),
        (
            Some("/project"),
            false,
            vec!["project", "feature", "docs", new],
            0,
        ),
        (None, false, vec!["project", "feature", "docs", new], 0),
        (None, true, vec!["project", "feature", "docs", new], 3),
        (
            Some("/elsewhere"),
            false,
            vec!["/elsewhere", "project", "feature", "docs", new],
            0,
        ),
    ] {
        let mut setup = setup();
        setup.new_worktree = new_worktree;
        let mut state = launch_state(setup, checkout);
        assert!(!state.advance());
        assert_eq!(state.launch_step(), Some(Step::Where));
        assert_eq!(labels(&state), expected, "{checkout:?}");
        assert_eq!(state.selected, selected, "{checkout:?}");
    }
    let mut no_repo = setup();
    no_repo.trees = None;
    let mut state = launch_state(no_repo, None);
    assert_eq!(state.footer(), "$ rimz agents launch coder");
    assert!(!state.advance());
    assert_eq!(labels(&state), ["project"]);
}

/// A tree's marker keeps the configured `dir` unresolved (`<root>/../<root>-worktrees/NAME`)
/// while a card names its checkout normalized, as a live room records them.
#[test]
fn where_step_matches_marker_and_card_spellings_of_one_checkout() {
    let new = "new worktree from trunk";
    let mut marker_spelling = setup();
    for tree in marker_spelling.trees.as_mut().unwrap() {
        tree.path = PathBuf::from(format!("/project/../project-worktrees/{}", tree.name));
    }
    for (checkout, expected, footer) in [
        (
            "/project-worktrees/feature",
            vec!["feature", "project", "docs", new],
            "$ rimz agents launch coder -w feature",
        ),
        (
            "/project-worktrees/../project",
            vec!["project", "feature", "docs", new],
            "$ rimz agents launch coder",
        ),
    ] {
        let mut state = launch_state(marker_spelling.clone(), Some(checkout));
        assert_eq!(state.footer(), footer, "{checkout}");
        assert!(!state.advance());
        assert_eq!(labels(&state), expected, "{checkout}");
    }
}

#[test]
fn name_step_refuses_invalid_and_listed_names_and_leaves_empty_to_generate() {
    let mut setup = setup();
    setup.new_worktree = true;
    let mut state = launch_state(setup, None);
    assert_eq!(state.footer(), "$ rimz agents launch coder -w --from-trunk");
    state.advance();
    state.advance();
    assert_eq!(state.launch_step(), Some(Step::Name));
    for (name, reason) in [
        ("bad name", "invalid worktree name `bad name`"),
        (
            "docs",
            "worktree `docs` is listed; pick it in the where step",
        ),
    ] {
        type_text(&mut state, name);
        assert_eq!(state.footer(), "nothing to run", "{name}");
        assert!(
            labels(&state)[0].starts_with(reason),
            "{name}: {:?}",
            labels(&state)
        );
        assert!(!state.advance());
        assert_eq!(state.launch_step(), Some(Step::Name));
        for _ in name.chars() {
            state.backspace();
        }
    }
    assert_eq!(state.footer(), "$ rimz agents launch coder -w --from-trunk");
    state.advance();
    assert_eq!(state.launch_step(), Some(Step::Prompt));
}

#[test]
fn launch_step_keys_step_back_on_empty_fields_and_keep_mode_characters_as_text() {
    let mut state = launch_state(setup(), None);
    type_text(&mut state, "rev");
    assert_eq!(labels(&state), ["reviewer"]);
    state.advance();
    state.advance();
    type_text(&mut state, "> #x");
    assert_eq!(state.mode(), Mode::New);
    assert_eq!(state.footer(), "$ rimz agents launch reviewer '> #x'");
    for _ in 0..4 {
        state.backspace();
    }
    assert_eq!(state.launch_step(), Some(Step::Prompt));
    state.backspace();
    assert_eq!(state.launch_step(), Some(Step::Where));
    state.backspace();
    assert_eq!(state.launch_step(), None);
    assert_eq!(state.query, "+rev");
    assert_eq!(state.footer(), "$ rimz agents launch reviewer");
    for _ in 0..4 {
        state.backspace();
    }
    assert_eq!(state.mode(), Mode::Cards);
}

#[test]
fn launches_are_offered_only_in_a_live_room() {
    let mut setup = setup();
    let refusal =
        "no live RimZ room `room`; run `rimz start` first or enter one with `rimz attach`";
    setup.unavailable = Some(refusal.to_owned());
    let mut state = launch_state(setup, None);
    assert_eq!(labels(&state), [refusal]);
    assert_eq!(state.footer(), "nothing to run");
    assert!(!state.advance());
    assert_eq!(state.launch_step(), None);
}

#[test]
fn launch_child_inherits_the_terminal_without_the_palettes_pane() {
    let workspace = ResolvedWorkspace {
        workspace_id: rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/project")),
        project_root: PathBuf::from("/project"),
        cwd_project_root: None,
        root_class: rimz::workspace::RootClass::Repo,
        worktree_root: PathBuf::from("/project"),
        worktree_branch: None,
        session_name: "room".to_owned(),
        mux_hint: None,
    };
    let args = ["agents".to_owned(), "launch".to_owned(), "coder".to_owned()];
    let command = launch_command(std::path::Path::new("/bin/rimz"), &workspace, &args);
    let envs = command
        .get_envs()
        .collect::<std::collections::BTreeMap<_, _>>();
    for key in ["TMUX_PANE", "ZELLIJ_PANE_ID"] {
        assert_eq!(envs.get(std::ffi::OsStr::new(key)), Some(&None), "{key}");
    }
    for (key, value) in rimz::workspace::pin_env(&workspace.workspace_id, &workspace.project_root) {
        assert_eq!(
            envs.get(std::ffi::OsStr::new(&key)),
            Some(&Some(std::ffi::OsStr::new(&value))),
            "{key}"
        );
    }
    assert_eq!(
        command.get_current_dir(),
        Some(std::path::Path::new("/project"))
    );
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        args.each_ref().map(std::ffi::OsStr::new)
    );
}

#[test]
fn printed_profiles_and_trees_resolve_to_the_entries_shown() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.email=rimz@example.test",
                "-c",
                "user.name=RimZ Test",
            ])
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(status.status.success(), "{args:?}: {status:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "init"]);
    let mut machine = rimz::config::MachineConfig::default();
    machine.agents.worktree.dir = home.path().join("trees").display().to_string();
    rimz::worktree::create(
        &repo,
        &machine.agents.worktree,
        Some("docs"),
        None,
        None,
        false,
        rimz::worktree::LaunchBase::Configured,
    )
    .unwrap();
    machine.agents.profiles.0.insert(
        "coder".to_owned(),
        toml::from_str("agent = 'codex'\ndescription = 'writes code'").unwrap(),
    );
    let setup = launch_setup(
        &machine,
        &rimz::config::AgentSpecSources::default(),
        &repo,
        None,
        false,
    );
    assert_eq!(
        setup
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>(),
        ["coder"]
    );
    let mut state = PaletteState::new(Mode::New, None, "room").with_launch(setup);
    state.refresh(Vec::new(), true);
    state.advance();
    state.selected = 1;
    let args = state.selected_action().unwrap().args;
    assert_eq!(labels(&state)[1], "docs");
    let layout = rimz::harness::spec::parse_layout_spec(
        &args[2],
        &machine.agents.profiles,
        &machine.agents.commands,
    )
    .unwrap();
    assert_eq!(layout.agent_kinds().collect::<Vec<_>>(), ["codex"]);
    let owned = rimz::worktree::discover_owned(&repo).unwrap();
    assert_eq!(args[3..], ["-w", "docs"]);
    assert_eq!(
        owned
            .iter()
            .map(|tree| tree.marker.name.as_str())
            .collect::<Vec<_>>(),
        [args[4].as_str()]
    );
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
fn command_target_renders_without_inventing_an_action() {
    {
        let state = PaletteState::new(Mode::Commands, None, "room");
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
        assert!(text.contains("no card"), "{text}");
    }
}
