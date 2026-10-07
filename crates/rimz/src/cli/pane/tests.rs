#[test]
fn capture_lines_rejects_zero_and_accepts_positive_bounds() {
    use clap::Parser;
    let err =
        crate::cli::Cli::try_parse_from(["rimz", "pane", "capture", "tmux:%1", "--lines", "0"])
            .expect_err("zero is not a useful capture bound");
    assert_eq!(err.exit_code(), 2);
    for n in ["1", "500", "65535"] {
        assert!(
            crate::cli::Cli::try_parse_from(["rimz", "pane", "capture", "tmux:%1", "--lines", n,])
                .is_ok()
        );
    }
}

use super::*;
use clap::Parser;
use jiff::Timestamp;
use rimz::agents::AgentStatus;
use rimz::ids::{AgentSessionId, MuxName};

#[derive(Debug, Parser)]
struct Harness {
    #[command(flatten)]
    args: PaneArgs,
}

#[test]
fn pane_help_orders_the_user_actions() {
    let help = crate::cli::Cli::try_parse_from(["rimz", "pane", "--help"])
        .unwrap_err()
        .to_string();
    let mut previous = 0;
    for verb in [
        "list",
        "capture",
        "send",
        "focus",
        "zoom",
        "split",
        "detach",
        "bandwidth",
    ] {
        let position = help.find(&format!("  {verb} ")).expect("verb in help");
        assert!(position > previous, "{help}");
        previous = position;
    }
    assert!(
        help.contains("rimz message") && help.contains("rimz transcript"),
        "{help}"
    );
}

#[test]
fn focus_hides_but_accepts_the_process_guard() {
    let help = Harness::try_parse_from(["rimz", "focus", "--help"])
        .unwrap_err()
        .to_string();
    assert!(!help.contains("pane-process-start"), "{help}");
    assert!(
        Harness::try_parse_from(["rimz", "focus", "tmux:%7", "--pane-process-start", "123"])
            .is_ok()
    );
}

#[test]
fn send_help_explains_order_and_keys() {
    let help = Harness::try_parse_from(["rimz", "send", "--help"])
        .unwrap_err()
        .to_string();
    let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        help.contains("Sends in a fixed order: TEXT, then each --key, then Enter."),
        "{help}"
    );
    assert!(
        help.contains("rimz message") && help.contains("rimz answer"),
        "{help}"
    );
    for key in "enter escape tab shift-tab backspace up down left right ctrl-c ctrl-d ctrl-u space delete home end page-up page-down ctrl-a ctrl-e ctrl-l".split_whitespace() {
        assert!(help.contains(key), "{key}: {help}");
    }
    for key in NamedKey::NAMES {
        assert!(help.contains(key), "{key}: {help}");
    }
}

#[test]
fn bare_tmux_id_uses_the_selected_backend() {
    for mux in ["tmux", "zellij"] {
        let globals = crate::cli::Cli::try_parse_from(["rimz", "--mux", mux])
            .unwrap()
            .global;
        let resolved = resolve_pane_target("%7", &globals);
        if mux == "tmux" {
            assert_eq!(
                resolved.expect("bare tmux id").pane,
                PaneId::from_parts(MuxName::Tmux, "%7")
            );
        } else {
            assert!(
                resolved
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("invalid pane target")
            );
        }
    }
}

#[test]
fn channel_target_explains_how_to_choose_a_pane() {
    let error = classify_pane_target("#lane").unwrap_err().to_string();
    assert!(error.contains("channel holds several panes"), "{error}");
    assert!(
        error.contains("rimz pane list") && error.contains("@handle#channel"),
        "{error}"
    );
}

#[test]
fn send_accepts_hyphen_text_and_flags() {
    for value in ["--dry-run first", "-x", "-"] {
        for argv in [
            ["rimz", "send", "@coder", "--enter", value],
            ["rimz", "send", "@coder", value, "--enter"],
        ] {
            let PaneSubcmd::Send {
                target,
                text,
                enter,
                ..
            } = Harness::try_parse_from(argv)
                .expect("parse send")
                .args
                .command
            else {
                panic!("send verb");
            };
            assert_eq!(target, "@coder");
            assert_eq!(text.as_deref(), Some(value));
            assert!(enter);
        }
    }
    for value in ["--enter", "-h", "--help"] {
        let PaneSubcmd::Send { text, .. } =
            Harness::try_parse_from(["rimz", "send", "@coder", "--", value])
                .unwrap()
                .args
                .command
        else {
            panic!("send verb");
        };
        assert_eq!(text.as_deref(), Some(value));
    }
}

#[test]
fn classify_pane_target_accepts_ids_and_agent_addresses() {
    assert_eq!(
        classify_pane_target("zellij:terminal_3").expect("zellij pane id"),
        PaneTarget::Id(PaneId::from_parts(MuxName::Zellij, "terminal_3"))
    );
    assert_eq!(
        classify_pane_target("tmux:%1").expect("tmux pane id"),
        PaneTarget::Id(PaneId::from_parts(MuxName::Tmux, "%1"))
    );
    assert_eq!(
        classify_pane_target("@coder#lane").expect("agent address"),
        PaneTarget::Address("@coder#lane".to_owned())
    );
    assert_eq!(
        classify_pane_target("sidebar").expect("sidebar target"),
        PaneTarget::Sidebar
    );
}

#[test]
fn classify_pane_target_error_points_at_addresses_and_pane_list() {
    let err = classify_pane_target("garbage").expect_err("invalid target");
    let message = err.to_string();
    assert!(message.contains("agent address (`@coder`, `@coder#lane`)"));
    assert!(message.contains("`sidebar`"));
    assert!(message.contains("rimz pane list"));
}

fn pane(raw: &str, view: &str, name: &str, command: &str, cwd: &str) -> PaneRef {
    PaneRef {
        view_id: Some(view.to_owned()),
        view_name: Some(name.to_owned()),
        is_floating: false,
        command: Some(command.to_owned()),
        cwd: Some(cwd.to_owned()),
        ..PaneRef::from_id(PaneId::from_parts(MuxName::Zellij, raw))
    }
}

fn agent_on(pane_raw: &str, kind: &str, branch: &str) -> AgentState {
    let now = Timestamp::now();
    AgentState {
        kind_ordinal: Some(1),
        status: AgentStatus::Running,
        phase: rimz::agents::TurnPhase::Reasoning,
        pane: Some(PaneRef::from_id(PaneId::from_parts(
            MuxName::Zellij,
            pane_raw,
        ))),
        worktree_path: Some(format!("/repo/{branch}")),
        worktree_branch: Some(branch.to_owned()),
        ..rimz::testkit::agent_state(kind, "sess-1", now)
    }
}

#[test]
fn group_by_tab_buckets_panes_in_first_seen_order() {
    let panes = vec![
        pane("terminal_1", "tab_0", "#auth", "claude", "/repo/auth"),
        pane("terminal_2", "tab_1", "shell", "zsh", "/repo"),
        pane("terminal_3", "tab_0", "#auth", "zsh", "/repo/auth"),
    ];
    let tabs = group_by_tab(&panes);
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[0].label(), "#auth");
    assert_eq!(tabs[0].panes.len(), 2, "both auth panes land under one tab");
    assert_eq!(tabs[1].label(), "shell");
    assert_eq!(tabs[1].panes.len(), 1);
}

#[test]
fn pane_list_scope_and_all_parse() {
    use clap::Parser;
    for args in [
        vec!["rimz", "pane", "list", "#auth", "--all"],
        vec!["rimz", "pane", "list", "-w", "auth"],
    ] {
        assert!(crate::cli::Cli::try_parse_from(args).is_ok());
    }
    assert!(
        crate::cli::Cli::try_parse_from(["rimz", "pane", "list", "#auth", "-w", "auth"]).is_err()
    );
}

#[test]
fn pane_table_strips_and_numbers_repeated_headings() {
    let panes = vec![
        pane("terminal_1", "tab_0", "brainstormer ?", "zsh", "/repo"),
        pane("terminal_2", "tab_1", "brainstormer ?", "zsh", "/repo"),
    ];
    let (table, hidden) = pane_table(&panes, None, None, false);
    let mut out = Vec::new();
    table.render(&mut out).unwrap();
    let raw = String::from_utf8(out).unwrap();
    let text = anstream::adapter::strip_str(&raw).to_string();
    assert!(text.lines().any(|line| line == "brainstormer"), "{text}");
    assert!(
        text.lines().any(|line| line == "brainstormer (2)"),
        "{text}"
    );
    assert_eq!(hidden, 0);
    let tabs = group_by_tab(&panes);
    for tab in &tabs {
        let json = serde_json::to_value(TabJson {
            view_id: tab.view_id.as_deref(),
            name: tab.name.as_deref(),
            panes: Vec::new(),
        })
        .unwrap();
        assert_eq!(json["name"], "brainstormer");
    }
}

#[test]
fn pane_table_omits_sidebar_only_tabs_and_reports_hidden_count() {
    let panes = vec![pane(
        "terminal_1",
        "tab_0",
        "sidebar-only",
        rimz::pane::SIDEBAR_CHROME_TITLE,
        "/repo",
    )];
    let (table, hidden) = pane_table(&panes, None, None, false);
    let mut out = Vec::new();
    table.render(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(!text.contains("sidebar-only"), "{text}");
    assert_eq!(hidden, 1);
    assert_eq!(sidebar_hint(hidden), "+1 sidebar · --all shows it");
    assert_eq!(sidebar_hint(34), "+34 sidebars · --all shows them");
    let (table, hidden) = pane_table(&panes, None, None, true);
    let mut out = Vec::new();
    table.render(&mut out).unwrap();
    assert!(String::from_utf8(out).unwrap().contains("sidebar-only"));
    assert_eq!(hidden, 0);
}

#[test]
fn pane_scope_keeps_nonagent_tabmates_but_filters_each_agent() {
    let panes = vec![
        PaneRef {
            hosted_agent_kind: Some(rimz::ids::AgentKind::new_unchecked("codex")),
            ..pane("terminal_1", "tab_0", "#auth", "rimz", "/repo/auth")
        },
        pane("terminal_2", "tab_0", "#auth", "zsh", "/elsewhere"),
        pane(
            "terminal_3",
            "tab_0",
            "#auth",
            rimz::pane::SIDEBAR_CHROME_TITLE,
            "/repo",
        ),
        pane("terminal_4", "tab_1", "#auth", "zsh", "/repo/auth"),
        pane("terminal_5", "tab_0", "#auth", "rimz", "/repo/other"),
    ];
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-pane-test")),
        vec![
            agent_on("terminal_1", "codex", "auth"),
            agent_on("terminal_5", "claude", "other"),
        ],
        Timestamp::now(),
    )
    .with_live_panes(panes.clone(), None);
    for scope in ["auth", "#auth"] {
        let selected = filter_panes_by_scope(panes.clone(), &snapshot, scope);
        assert_eq!(
            selected
                .iter()
                .map(|pane| pane.pane_id.raw())
                .collect::<Vec<_>>(),
            ["terminal_1", "terminal_2", "terminal_3"]
        );
    }
    assert!(filter_panes_by_scope(panes.clone(), &snapshot, "missing").is_empty());
    let (table, _) = pane_table(&panes, Some(&snapshot), None, false);
    let mut out = Vec::new();
    table.render(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("COMMAND") && text.contains("codex") && !text.contains("  rimz  "),
        "{text}"
    );
}

#[test]
fn pane_json_annotates_the_bound_agent_with_its_handle() {
    let pane = pane("terminal_1", "tab_0", "#main", "claude", "/repo/main");
    let agent = agent_on("terminal_1", "claude", "main");
    let peers: Vec<&AgentState> = vec![&agent];
    let json = pane_json(&pane, Some(&agent), &peers, false);
    assert_eq!(json.kind, "agent");
    let bound = json.agent.as_ref().expect("agent bound");
    assert_eq!(bound.handle, "@claude#main");
    assert_eq!(bound.kind, "claude");
    assert_eq!(bound.worktree.as_deref(), Some("main"));
    let serialized = serde_json::to_value(&json).expect("pane JSON");
    assert!(serialized.get("focused").is_none());
    assert!(serialized.get("self").is_none());
    assert_eq!(json.pane_id, "zellij:terminal_1");
}

#[test]
fn pane_list_role_handle_ignores_historical_roots() {
    let mut live = agent_on("terminal_1", "codex", "main");
    live.role = Some("coder".to_owned());
    let mut historical = rimz::testkit::agent_state("codex", "sess-historical", Timestamp::now());
    historical.role = Some("coder".to_owned());
    let pane = pane("terminal_1", "tab_0", "#main", "codex", "/repo/main");
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-pane-test")),
        vec![live, historical],
        Timestamp::now(),
    )
    .with_live_panes(vec![pane.clone()], None);

    let peers: Vec<&AgentState> = snapshot.pane_bound_roots().collect();
    let json = pane_json(&pane, Some(peers[0]), &peers, false);
    assert_eq!(json.agent.expect("bound agent").handle, "@coder#main");
}

#[test]
fn pane_list_live_overlay_keeps_ordinal_disambiguation() {
    let mut first = agent_on("terminal_1", "codex", "main");
    first.agent_id = AgentSessionId::from("sess-1");
    first.kind_ordinal = Some(1);
    let mut second = agent_on("terminal_2", "codex", "main");
    second.agent_id = AgentSessionId::from("sess-2");
    second.kind_ordinal = Some(2);
    let first_pane = pane("terminal_1", "tab_0", "#main", "codex", "/repo/main");
    let second_pane = pane("terminal_2", "tab_0", "#main", "codex", "/repo/main");
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-pane-test")),
        vec![first, second],
        Timestamp::now(),
    )
    .with_live_panes(vec![first_pane.clone(), second_pane.clone()], None);
    let peers: Vec<&AgentState> = snapshot.pane_bound_roots().collect();

    let first = snapshot
        .agent_bound_to_pane(&first_pane)
        .expect("first pane bound");
    let second = snapshot
        .agent_bound_to_pane(&second_pane)
        .expect("second pane bound");
    assert_eq!(
        pane_json(&first_pane, Some(first), &peers, false)
            .agent
            .expect("first agent")
            .handle,
        "@codex-1#main"
    );
    assert_eq!(
        pane_json(&second_pane, Some(second), &peers, false)
            .agent
            .expect("second agent")
            .handle,
        "@codex-2#main"
    );
}

#[test]
fn pane_json_leaves_a_plain_pane_unannotated() {
    let pane = pane("terminal_2", "tab_1", "shell", "zsh", "/home/x");
    let json = pane_json(&pane, None, &[], false);
    assert!(json.agent.is_none(), "a bare shell carries no agent");
    assert_eq!(json.kind, "process");
    assert_eq!(json.command, Some("zsh"), "command is retained in json");
    assert_eq!(json.pane_id, "zellij:terminal_2");
}

#[test]
fn pane_json_labels_the_sidebar_pane() {
    let pane = pane(
        "terminal_3",
        "tab_1",
        "shell",
        rimz::pane::SIDEBAR_CHROME_TITLE,
        "/home/x",
    );
    let json = pane_json(&pane, None, &[], false);

    assert_eq!(json.kind, "sidebar");
    assert!(json.agent.is_none());
    let serialized = serde_json::to_value(&json).expect("pane JSON");
    assert!(serialized.get("self").is_none());
}

#[test]
fn pane_json_marks_the_calling_pane() {
    let pane = pane("terminal_2", "tab_1", "shell", "zsh", "/home/x");
    let json = pane_json(&pane, None, &[], true);

    let serialized = serde_json::to_value(&json).expect("pane JSON");
    assert_eq!(serialized.get("self"), Some(&serde_json::json!(true)));
}

#[test]
fn pane_row_labels_a_calling_sidebar_before_any_agent_overlay() {
    let pane = pane(
        "terminal_3",
        "tab_1",
        "shell",
        rimz::pane::SIDEBAR_CHROME_TITLE,
        "/home/x",
    );
    let agent = agent_on("terminal_3", "codex", "main");
    let peers = vec![&agent];
    let mut table = render::Table::new(["AGENT", "STATUS", "COMMAND", "CWD", "PANE"]);
    table.row(pane_row(&pane, Some(&agent), &peers, true));
    let mut raw = Vec::new();

    table.render(&mut raw).expect("pane row");
    let raw = String::from_utf8(raw).expect("utf-8");
    let plain = anstream::adapter::strip_str(&raw).to_string();

    assert!(plain.contains("sidebar"));
    assert!(plain.contains("zellij:terminal_3 (self)"));
    assert!(!plain.contains("@codex"));
}

#[test]
fn overlay_refuses_an_agent_whose_pane_was_reused() {
    // The overlay binds through the snapshot's stamped-pane guard, so a pane
    // the multiplexer has handed to a shell since the agent left never
    // inherits that agent — the same rule the sidebar card binds by.
    let t1: Timestamp = "2026-06-01T00:00:00Z".parse().unwrap();
    let t2: Timestamp = "2026-06-01T01:00:00Z".parse().unwrap();
    let mut agent = agent_on("terminal_1", "codex", "main");
    agent.last_activity = t1;
    let snapshot = rimz::store::snapshot::SidebarSnapshot::build_with_agents(
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/rimz-pane-test")),
        vec![agent],
        t2,
    );

    // terminal_1 is now a shell whose process started after the agent's last
    // activity: the pane was reused, so nothing binds.
    let reused = PaneRef {
        command: Some("zsh".to_owned()),
        pane_process_start: Some(t2),
        ..pane("terminal_1", "tab_0", "shell", "zsh", "/repo")
    };
    assert!(
        snapshot.agent_bound_to_pane(&reused).is_none(),
        "a reused pane carries no agent"
    );

    // The same pane still running codex binds as before.
    let live = pane("terminal_1", "tab_0", "#main", "codex", "/repo/main");
    let bound = snapshot
        .agent_bound_to_pane(&live)
        .expect("the live codex pane still binds");
    assert_eq!(bound.kind.as_str(), "codex");
}
