use super::*;
use crate::config::{DaemonConfig, RemoteControlConfig};
use crate::ids::MuxName;
use std::path::PathBuf;

fn pane(command: Option<&str>, view_name: Option<&str>) -> PaneRef {
    pane_with_id("%1", command, view_name)
}

fn pane_with_id(raw: &str, command: Option<&str>, view_name: Option<&str>) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, raw),
        session_name: "rimz-demo".to_owned(),
        view_id: Some("@1".to_owned()),
        view_kind: None,
        view_name: view_name.map(ToOwned::to_owned),
        title: None,
        is_focused: false,
        is_floating: false,
        command: command.map(ToOwned::to_owned),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: None,
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn spawned_pane_with_id(raw: &str, spawn_command: &str, view_name: Option<&str>) -> PaneRef {
    PaneRef {
        spawn_command: Some(spawn_command.to_owned()),
        ..pane_with_id(raw, Some("rimz"), view_name)
    }
}

fn spawned_pane(spawn_command: &str, view_name: Option<&str>) -> PaneRef {
    spawned_pane_with_id("%1", spawn_command, view_name)
}

fn host(argv: &[&str]) -> HostPane {
    HostPane {
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: PathBuf::from("/repo"),
    }
}

fn daemon_view() -> DaemonView {
    DaemonView {
        name: VIEW_NAME.to_owned(),
        content: vec![host(&["rimz", "daemon", "content", "--slot", "0"])],
        hosts: vec![
            host(&["rimz", "codex", "app-server", "serve"]),
            host(&["claude", "remote-control", "--spawn", "worktree"]),
        ],
        loop_panel: host(&["rimz", "loop", "watch", "--hold"]),
    }
}

#[test]
fn daemon_view_spec_orders_the_ungated_broker_then_claude() {
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("valid id");
    let rimz_bin = Path::new("/usr/bin/rimz");
    let project_root = Path::new("/proj");
    let worktree_root = Path::new("/proj/wt");
    let spec = |remote_control: &RemoteControlConfig, claude_present, codex_present| {
        daemon_view_spec(DaemonViewSpecParams {
            remote_control,
            daemon: &DaemonConfig::default(),
            rimz_bin,
            workspace_id: &workspace_id,
            session_name: "rimz-demo",
            project_root,
            worktree_root,
            claude_present,
            codex_present,
        })
    };

    assert!(
        spec(&RemoteControlConfig::default(), true, false)
            .hosts
            .is_empty()
    );
    let codex = spec(&RemoteControlConfig::default(), false, true);
    assert_eq!(codex.hosts.len(), 1);
    assert_eq!(codex.hosts[0].argv[0], "/usr/bin/rimz");
    assert!(codex.hosts[0].argv.iter().any(|arg| arg == "app-server"));
    assert_eq!(codex.hosts[0].cwd, worktree_root);

    let claude_only = RemoteControlConfig {
        claude: true,
        codex: false,
    };
    assert!(spec(&claude_only, false, false).hosts.is_empty());
    let claude = spec(&claude_only, true, false);
    assert_eq!(claude.hosts.len(), 1);
    assert_eq!(
        claude.hosts[0].argv,
        crate::remote_control::claude_host_argv()
    );
    assert_eq!(claude.hosts[0].cwd, project_root);

    let both = RemoteControlConfig {
        claude: true,
        codex: true,
    };
    let pair = spec(&both, true, true);
    assert_eq!(pair.hosts.len(), 2);
    assert!(pair.hosts[0].argv.iter().any(|arg| arg == "app-server"));
    assert_eq!(pair.hosts[1].argv[0], "claude");
}

#[test]
fn daemon_view_spec_keeps_content_and_loop_panel_without_hosts() {
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("valid id");
    let view = daemon_view_spec(DaemonViewSpecParams {
        remote_control: &RemoteControlConfig::default(),
        daemon: &DaemonConfig::default(),
        rimz_bin: Path::new("/usr/bin/rimz"),
        workspace_id: &workspace_id,
        session_name: "rimz-demo",
        project_root: Path::new("/proj"),
        worktree_root: Path::new("/proj/wt"),
        claude_present: false,
        codex_present: false,
    });

    assert_eq!(view.name, VIEW_NAME);
    assert!(view.hosts.is_empty());
    assert_eq!(
        view.content,
        vec![content_supervisor_pane(
            0,
            Path::new("/usr/bin/rimz"),
            Path::new("/proj/wt")
        )]
    );
    assert_eq!(
        view.loop_panel,
        loop_panel(Path::new("/usr/bin/rimz"), Path::new("/proj/wt"))
    );
}

#[test]
fn planner_chains_an_empty_runtime_column_in_spec_order() {
    let view = daemon_view();
    let mut panes = vec![
        pane_with_id(
            "%1",
            Some(crate::pane::SIDEBAR_CHROME_TITLE),
            Some(VIEW_NAME),
        ),
        spawned_pane_with_id(
            "%2",
            "rimz daemon content --slot 0 --worktree-root /repo",
            Some(VIEW_NAME),
        ),
    ];

    let RepairStep::Spawn {
        pane: broker,
        anchor_pane_id,
        direction,
    } = next_repair_step(&panes, &view)
    else {
        panic!("broker spawn")
    };
    assert_eq!(
        host_marker(&broker),
        Some(ManagedPaneMarker::CodexAppServer)
    );
    assert_eq!(anchor_pane_id.raw(), "%2");
    assert_eq!(direction, SplitDirection::Right);
    panes.push(spawned_pane_with_id(
        "%3",
        &broker.argv.join(" "),
        Some(VIEW_NAME),
    ));

    let RepairStep::Spawn {
        pane: claude,
        anchor_pane_id,
        direction,
    } = next_repair_step(&panes, &view)
    else {
        panic!("Claude spawn")
    };
    assert_eq!(
        host_marker(&claude),
        Some(ManagedPaneMarker::ClaudeRemoteControl)
    );
    assert_eq!(anchor_pane_id.raw(), "%3");
    assert_eq!(direction, SplitDirection::Down);
    panes.push(spawned_pane_with_id(
        "%4",
        &claude.argv.join(" "),
        Some(VIEW_NAME),
    ));

    let RepairStep::Spawn {
        pane: panel,
        anchor_pane_id,
        direction,
    } = next_repair_step(&panes, &view)
    else {
        panic!("loop-panel spawn")
    };
    assert_eq!(host_marker(&panel), Some(ManagedPaneMarker::LoopPanel));
    assert_eq!(anchor_pane_id.raw(), "%4");
    assert_eq!(direction, SplitDirection::Down);
}

#[test]
fn planner_places_missing_claude_after_the_preceding_broker() {
    let view = daemon_view();
    let panes = [
        spawned_pane_with_id("%2", "rimz daemon content --slot 0", Some(VIEW_NAME)),
        spawned_pane_with_id("%3", "rimz codex app-server serve", Some(VIEW_NAME)),
        spawned_pane_with_id("%5", "rimz loop watch --hold", Some(VIEW_NAME)),
    ];

    let RepairStep::Spawn {
        pane,
        anchor_pane_id,
        direction,
    } = next_repair_step(&panes, &view)
    else {
        panic!("Claude spawn")
    };
    assert_eq!(
        host_marker(&pane),
        Some(ManagedPaneMarker::ClaudeRemoteControl)
    );
    assert_eq!(anchor_pane_id.raw(), "%3");
    assert_eq!(direction, SplitDirection::Down);
}

#[test]
fn planner_creates_the_content_column_from_the_sidebar() {
    let view = daemon_view();
    let sidebar = pane_with_id(
        "%1",
        Some(crate::pane::SIDEBAR_CHROME_TITLE),
        Some(VIEW_NAME),
    );
    let RepairStep::Spawn {
        pane,
        anchor_pane_id,
        direction,
    } = next_repair_step(std::slice::from_ref(&sidebar), &view)
    else {
        panic!("content spawn")
    };

    assert_eq!(host_marker(&pane), Some(ManagedPaneMarker::ContentSlot(0)));
    assert_eq!(anchor_pane_id, sidebar.pane_id);
    assert_eq!(direction, SplitDirection::Right);
}

#[test]
fn planner_treats_a_closed_view_as_deliberate() {
    assert_eq!(next_repair_step(&[], &daemon_view()), RepairStep::Done);
    let outside = pane(Some("zsh"), Some("work"));
    assert_eq!(
        next_repair_step(std::slice::from_ref(&outside), &daemon_view()),
        RepairStep::Done
    );
}

#[test]
fn managed_pane_reconciliation_diffs_the_daemon_view_spec() {
    let present = [
        spawned_pane(
            "rimz daemon content --slot 0 --worktree-root /repo",
            Some(VIEW_NAME),
        ),
        spawned_pane("rimz codex app-server serve", Some(VIEW_NAME)),
        spawned_pane("rimz loop watch --hold", Some(VIEW_NAME)),
        pane(Some("user shell"), Some(VIEW_NAME)),
        spawned_pane("claude remote-control --spawn worktree", Some("work")),
    ];

    let missing = managed_pane_reconciliation(&daemon_view(), &present)
        .spawn
        .into_iter()
        .map(|host| host.argv.join(" "))
        .collect::<Vec<_>>();
    assert_eq!(missing, vec!["claude remote-control --spawn worktree"]);
}

#[test]
fn title_identity_prevents_respawn_after_foreground_command_churn() {
    let mut daemon_host = pane_with_id("%9", Some("claude-sdk"), Some(VIEW_NAME));
    daemon_host.title = Some("claude remote-control --spawn worktree".to_owned());
    let reconciliation = managed_pane_reconciliation(&daemon_view(), &[daemon_host]);

    assert!(
        !reconciliation
            .spawn
            .iter()
            .any(|host| { host_marker(host) == Some(ManagedPaneMarker::ClaudeRemoteControl) })
    );
}

#[test]
fn title_identity_matches_content_supervisor_after_child_churn() {
    let mut content = pane_with_id("%8", Some("rimz stats --refresh --hold"), Some(VIEW_NAME));
    content.title = Some("rimz daemon content --slot 0".to_owned());
    let reconciliation = managed_pane_reconciliation(&daemon_view(), &[content]);

    assert!(
        !reconciliation
            .spawn
            .iter()
            .any(|host| { host_marker(host) == Some(ManagedPaneMarker::ContentSlot(0)) })
    );
}

#[test]
fn reconciliation_closes_surplus_managed_panes_but_keeps_the_oldest() {
    let panes = [
        spawned_pane_with_id(
            "%9",
            "claude remote-control --spawn worktree",
            Some(VIEW_NAME),
        ),
        spawned_pane_with_id(
            "%7",
            "claude remote-control --spawn worktree",
            Some(VIEW_NAME),
        ),
        spawned_pane_with_id(
            "%3",
            "claude remote-control --spawn worktree",
            Some(VIEW_NAME),
        ),
    ];

    assert_eq!(
        managed_pane_reconciliation(&daemon_view(), &panes).close,
        vec![
            PaneId::from_parts(MuxName::Tmux, "%7"),
            PaneId::from_parts(MuxName::Tmux, "%9"),
        ]
    );
}

#[test]
fn disabled_claude_host_selection_uses_title_and_stays_in_the_daemon_view() {
    let mut daemon_host = pane_with_id("%4", Some("claude-sdk"), Some(VIEW_NAME));
    daemon_host.title = Some("claude remote-control --spawn worktree".to_owned());
    let daemon_host_id = daemon_host.pane_id.clone();
    let mut working_host = pane_with_id("%5", Some("claude-sdk"), Some("work"));
    working_host.title = Some("claude remote-control --spawn worktree".to_owned());
    let user_pane = spawned_pane("nvim remote-control.md", Some(VIEW_NAME));
    let panes = [daemon_host, working_host, user_pane];

    let mut disabled = daemon_view();
    disabled
        .hosts
        .retain(|host| host_marker(host) != Some(ManagedPaneMarker::ClaudeRemoteControl));
    assert_eq!(
        managed_pane_reconciliation(&disabled, &panes).close,
        vec![daemon_host_id]
    );
    assert!(
        managed_pane_reconciliation(&daemon_view(), &panes)
            .close
            .is_empty()
    );
}

#[test]
fn detects_hosts_and_loop_panel_across_mux_identity_fields() {
    assert!(pane_is_host(&pane(
        Some("claude remote-control --spawn worktree"),
        None,
    )));
    assert!(pane_is_host(&pane(
        Some("rimz codex app-server serve --workspace-id w"),
        None,
    )));
    assert!(pane_is_host(&spawned_pane(
        "claude remote-control --spawn worktree",
        None,
    )));
    assert!(pane_is_host(&pane(Some("rimz"), Some(VIEW_NAME))));
    assert!(!pane_is_host(&pane(Some("claude"), Some("work"))));

    let older = spawned_pane_with_id("%2", "rimz loop watch --hold", Some(VIEW_NAME));
    let newer = spawned_pane_with_id("%8", "rimz loop watch --hold", Some(VIEW_NAME));
    assert_eq!(find_loop_panel(&[newer, older.clone()]), Some(&older));
}

#[test]
fn claude_host_presence_requires_the_managed_pane_marker() {
    let managed = spawned_pane("claude remote-control --spawn worktree", Some(VIEW_NAME));
    let user_session = spawned_pane("claude", Some("work"));
    let mut managed_after_churn = pane(Some("claude-sdk"), Some(VIEW_NAME));
    managed_after_churn.title = Some("claude remote-control --spawn worktree".to_owned());

    assert!(claude_host_present(&[managed, user_session.clone()]));
    assert!(claude_host_present(&[managed_after_churn]));
    assert!(!claude_host_present(&[user_session]));
}
